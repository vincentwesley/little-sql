//! `ALTER TABLE`: RENAME TO, RENAME COLUMN, ADD COLUMN, DROP COLUMN.
//!
//! Like SQLite, renames rewrite the stored SQL of everything that refers to
//! the table or column: the table itself, its indexes and triggers, and
//! views and triggers that use it. The rewriting works on tokens, with
//! rules for where a name can denote a table (after FROM, JOIN, INTO,
//! UPDATE, TABLE, `ON` of an index or trigger, in a FROM list, or as the
//! qualifier before a `.`) or a column. Afterwards the schema is reloaded
//! and every view is bound again; if anything no longer makes sense the
//! statement fails and is rolled back, so a rewrite can be refused but
//! never leaves a broken schema behind.

use crate::btree_write::Writer;
use crate::db::{Database, ResultSet};
use crate::error::{Error, Result};
use crate::record::encode_record_enc;
use crate::schema::SchemaRow;
use crate::sql::ast::AlterAction;
use crate::sql::lexer::{tokenize, Tok, Token};
use crate::value::Value;

pub(crate) fn alter_table(db: &Database, table: &str, action: &AlterAction) -> Result<ResultSet> {
    let schema = db.schema();
    let Some(t) = schema.table(table).cloned() else {
        if schema.view(table).is_some() {
            return Err(Error::Sql(format!("cannot alter {table}: it is a view")));
        }
        return Err(Error::Sql(format!("no such table: {table}")));
    };
    if t.name.to_ascii_lowercase().starts_with("sqlite_") {
        return Err(Error::Sql(format!("table {} may not be altered", t.name)));
    }
    let w = Writer::new(&db.pager);
    match action {
        AlterAction::RenameTable(new) => rename_table(db, &w, &t.name, new)?,
        AlterAction::RenameColumn { from, to } => rename_column(db, &w, &t, from, to)?,
        AlterAction::AddColumn { def, text } => add_column(db, &w, &t, def, text)?,
        AlterAction::DropColumn(col) => drop_column(db, &w, &t, col)?,
    }
    crate::write::bump_schema_cookie(&w, db)?;
    db.reload_schema()?;
    // Every view and trigger must still make sense.
    let schema = db.schema();
    for r in &schema.rows {
        if let Some(why) = schema.broken.get(&r.name.to_ascii_lowercase()) {
            return Err(Error::Sql(format!("error in {} {}: {why}", r.kind, r.name)));
        }
        if r.kind == "view" {
            let v = schema.view(&r.name).cloned().unwrap();
            if let Err(e) = db.run_select(&v.select, false) {
                return Err(Error::Sql(format!("error in view {}: {e}", r.name)));
            }
        }
    }
    Ok(ResultSet::default())
}

fn quoted(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn is_ident(t: &Tok, name: &str) -> bool {
    match t {
        Tok::Word(w) => w.eq_ignore_ascii_case(name),
        Tok::Quoted { name: n, .. } => n.eq_ignore_ascii_case(name),
        Tok::Str(s) => s.eq_ignore_ascii_case(name),
        _ => false,
    }
}

fn is_kw(t: Option<&Token>, kws: &[&str]) -> bool {
    matches!(t.map(|t| &t.tok), Some(Tok::Word(w)) if kws.iter().any(|k| w.eq_ignore_ascii_case(k)))
}

fn is_op(t: Option<&Token>, op: &str) -> bool {
    matches!(t.map(|t| &t.tok), Some(Tok::Op(o)) if *o == op)
}

fn apply(sql: &str, mut edits: Vec<(usize, usize, String)>) -> String {
    edits.sort_by_key(|e| e.0);
    edits.dedup_by_key(|e| e.0);
    let mut out = String::with_capacity(sql.len() + 16);
    let mut at = 0;
    for (s, e, text) in edits {
        out.push_str(&sql[at..s]);
        out.push_str(&text);
        at = e;
    }
    out.push_str(&sql[at..]);
    out
}

/// Positions of tokens in `sql` that name table `old`.
fn table_refs(toks: &[Token], old: &str) -> Vec<usize> {
    let mut out = Vec::new();
    // in_from[depth]: inside a FROM clause at this nesting depth.
    let mut in_from = vec![false];
    for i in 0..toks.len() {
        let t = &toks[i].tok;
        match t {
            Tok::Op("(") => in_from.push(false),
            Tok::Op(")") => {
                in_from.pop();
                if in_from.is_empty() {
                    in_from.push(false);
                }
            }
            Tok::Word(w) => {
                let w = w.to_ascii_uppercase();
                let top = in_from.last_mut().unwrap();
                if w == "FROM" {
                    *top = true;
                } else if [
                    "WHERE",
                    "GROUP",
                    "HAVING",
                    "ORDER",
                    "LIMIT",
                    "WINDOW",
                    "UNION",
                    "EXCEPT",
                    "INTERSECT",
                    "SELECT",
                    "SET",
                    "VALUES",
                    "BEGIN",
                    "END",
                ]
                .contains(&w.as_str())
                {
                    *top = false;
                }
            }
            Tok::Op(";") => *in_from.last_mut().unwrap() = false,
            _ => {}
        }
        if !is_ident(t, old) || matches!(t, Tok::Str(_)) {
            continue;
        }
        let prev = i.checked_sub(1).map(|p| &toks[p]);
        let next = toks.get(i + 1);
        let refers = is_op(next, ".")
            || is_kw(
                prev,
                &[
                    "TABLE",
                    "FROM",
                    "JOIN",
                    "INTO",
                    "UPDATE",
                    "REFERENCES",
                    "EXISTS",
                ],
            )
            || (is_kw(prev, &["ON"])
                && (is_op(next, "(") || is_kw(next, &["BEGIN", "FOR", "WHEN"])))
            || (is_op(prev, ",") && *in_from.last().unwrap());
        if refers && !is_kw(prev, &["AS"]) {
            out.push(i);
        }
    }
    out
}

/// The schema row as it would be with new values.
fn update_row(
    db: &Database,
    w: &Writer,
    r: &SchemaRow,
    name: &str,
    tbl: &str,
    sql: Option<&str>,
) -> Result<()> {
    let rec = encode_record_enc(
        &[
            Value::text(&r.kind),
            Value::text(name),
            Value::text(tbl),
            Value::Int(r.root as i64),
            sql.map_or(Value::Null, Value::text),
        ],
        db.pager.encoding(),
    );
    w.table_insert(1, r.rowid, &rec)
}

fn rename_table(db: &Database, w: &Writer, old: &str, new: &str) -> Result<()> {
    let schema = db.schema();
    if schema.table(new).is_some() || schema.view(new).is_some() || schema.index(new).is_some() {
        return Err(Error::Sql(format!(
            "there is already another table or index with this name: {new}"
        )));
    }
    if new.to_ascii_lowercase().starts_with("sqlite_") {
        return Err(Error::Sql(format!(
            "object name reserved for internal use: {new}"
        )));
    }
    let auto_prefix = format!("sqlite_autoindex_{old}_").to_ascii_lowercase();
    for r in &schema.rows {
        let mut name = r.name.clone();
        let mut tbl = r.tbl_name.clone();
        if r.tbl_name.eq_ignore_ascii_case(old) {
            tbl = new.to_string();
        }
        if r.kind == "table" && r.name.eq_ignore_ascii_case(old) {
            name = new.to_string();
        }
        if r.kind == "index" && r.name.to_ascii_lowercase().starts_with(&auto_prefix) {
            name = format!("sqlite_autoindex_{new}_{}", &r.name[auto_prefix.len()..]);
        }
        let sql = match &r.sql {
            Some(sql) => {
                let toks = tokenize(sql)?;
                let edits: Vec<_> = table_refs(&toks, old)
                    .into_iter()
                    .map(|i| (toks[i].start, toks[i].end, quoted(new)))
                    .collect();
                Some(apply(sql, edits))
            }
            None => None,
        };
        if name != r.name || tbl != r.tbl_name || sql != r.sql {
            update_row(db, w, r, &name, &tbl, sql.as_deref())?;
        }
    }
    // AUTOINCREMENT's counter follows the table.
    if let Some(seq) = schema.table("sqlite_sequence").cloned() {
        let mut c = crate::btree::Cursor::new(db.pager.clone(), seq.root);
        c.first()?;
        let mut renames = Vec::new();
        while c.valid() {
            let vals = crate::record::decode_all(c.payload()?.as_slice(), db.pager.encoding())?;
            if vals
                .first()
                .is_some_and(|v| v.to_text().eq_ignore_ascii_case(old))
            {
                renames.push((c.rowid()?, vals));
            }
            c.next()?;
        }
        for (rowid, mut vals) in renames {
            vals[0] = Value::text(new);
            w.table_insert(
                seq.root,
                rowid,
                &encode_record_enc(&vals, db.pager.encoding()),
            )?;
        }
    }
    Ok(())
}

/// Token positions naming column `col` of table `table` in a statement.
fn column_refs(
    db: &Database,
    toks: &[Token],
    row: &SchemaRow,
    table: &str,
    col: &str,
) -> Vec<usize> {
    let schema = db.schema();
    let own_table = row.kind == "table" && row.name.eq_ignore_ascii_case(table);
    let own_index = row.kind == "index" && row.tbl_name.eq_ignore_ascii_case(table);
    let own_trigger = row.kind == "trigger" && row.tbl_name.eq_ignore_ascii_case(table);
    // Tables (and their aliases) this statement reads.
    let mut tables: Vec<String> = Vec::new();
    let mut aliases_of_target: Vec<String> = vec![table.to_ascii_lowercase()];
    if own_trigger {
        aliases_of_target.push("new".into());
        aliases_of_target.push("old".into());
    }
    for i in table_refs(toks, table) {
        // `FROM t AS x` or `FROM t x`: x is t too.
        let mut j = i + 1;
        if is_kw(toks.get(j), &["AS"]) {
            j += 1;
        }
        if let Some(Tok::Word(a)) | Some(Tok::Quoted { name: a, .. }) = toks.get(j).map(|t| &t.tok)
        {
            let up = a.to_ascii_uppercase();
            if ![
                "WHERE",
                "JOIN",
                "ON",
                "LEFT",
                "INNER",
                "CROSS",
                "GROUP",
                "ORDER",
                "LIMIT",
                "SET",
                "BEGIN",
                "FOR",
                "WHEN",
                "USING",
                "NATURAL",
                "UNION",
                "EXCEPT",
                "INTERSECT",
                "HAVING",
                "WINDOW",
                "VALUES",
                "DEFAULT",
                "SELECT",
            ]
            .contains(&up.as_str())
            {
                aliases_of_target.push(a.to_ascii_lowercase());
            }
        }
    }
    for t in toks {
        if let Tok::Word(w) | Tok::Quoted { name: w, .. } = &t.tok {
            if let Some(tb) = schema.table(w) {
                if !tables.iter().any(|x| x.eq_ignore_ascii_case(&tb.name)) {
                    tables.push(tb.name.clone());
                }
            }
        }
    }
    // An unqualified name is this column only if no other table the
    // statement mentions has a column of that name.
    let unambiguous = tables
        .iter()
        .filter(|tn| !tn.eq_ignore_ascii_case(table))
        .all(|tn| {
            schema
                .table(tn)
                .is_none_or(|t| t.column_index(col).is_none())
        });
    let mentions_target =
        own_table || own_index || own_trigger || !table_refs(toks, table).is_empty();
    if !mentions_target {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut skip_depth: Option<usize> = None;
    // Inside a trigger's `UPDATE OF a, b ON t`: names of t's columns.
    let mut in_of_list = false;
    for i in 0..toks.len() {
        let t = &toks[i].tok;
        if own_trigger && depth == 0 {
            if is_kw(Some(&toks[i]), &["OF"]) {
                in_of_list = true;
                continue;
            }
            if is_kw(Some(&toks[i]), &["ON"]) {
                in_of_list = false;
            }
        }
        let prev = i.checked_sub(1).map(|p| &toks[p]);
        let next = toks.get(i + 1);
        match t {
            Tok::Op("(") => {
                depth += 1;
                // A REFERENCES other(col) list names another table's columns.
                if own_table
                    && i >= 2
                    && is_kw(toks.get(i - 2), &["REFERENCES"])
                    && skip_depth.is_none()
                {
                    skip_depth = Some(depth);
                }
                continue;
            }
            Tok::Op(")") => {
                if skip_depth == Some(depth) {
                    skip_depth = None;
                }
                depth = depth.saturating_sub(1);
                continue;
            }
            _ => {}
        }
        if skip_depth.is_some() || !is_ident(t, col) || matches!(t, Tok::Str(_)) {
            continue;
        }
        if is_op(next, "(") || is_kw(prev, &["AS"]) || is_op(next, ".") {
            continue;
        }
        let qualified_by = if is_op(prev, ".") && i >= 2 {
            match &toks[i - 2].tok {
                Tok::Word(q) | Tok::Quoted { name: q, .. } => Some(q.to_ascii_lowercase()),
                _ => None,
            }
        } else {
            None
        };
        let hit = match &qualified_by {
            _ if in_of_list => true,
            Some(q) => aliases_of_target.contains(q),
            None if own_table => {
                // A column definition's name, or a name inside a constraint.
                (depth == 1 && (is_op(prev, "(") || is_op(prev, ","))) || depth >= 2
            }
            None if own_index => depth >= 1,
            None => unambiguous,
        };
        if hit {
            out.push(i);
        }
    }
    out
}

fn rename_column(
    db: &Database,
    w: &Writer,
    t: &crate::schema::Table,
    from: &str,
    to: &str,
) -> Result<()> {
    if t.column_index(from).is_none() {
        return Err(Error::Sql(format!("no such column: \"{from}\"")));
    }
    if t.column_index(to).is_some() {
        return Err(Error::Sql(format!("duplicate column name: {to}")));
    }
    let schema = db.schema();
    for r in &schema.rows {
        let Some(sql) = &r.sql else { continue };
        let toks = tokenize(sql)?;
        let edits: Vec<_> = column_refs(db, &toks, r, &t.name, from)
            .into_iter()
            .map(|i| {
                (
                    toks[i].start,
                    toks[i].end,
                    quote_if_needed(&toks[i].tok, to),
                )
            })
            .collect();
        if !edits.is_empty() {
            let new_sql = apply(sql, edits);
            update_row(db, w, r, &r.name, &r.tbl_name, Some(&new_sql))?;
        }
    }
    Ok(())
}

/// The new name, quoted if the old token was quoted or the name needs it.
fn quote_if_needed(old: &Tok, new: &str) -> String {
    let plain = new
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && new.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !crate::sql::parser::is_reserved(new);
    if matches!(old, Tok::Quoted { .. }) || !plain {
        quoted(new)
    } else {
        new.to_string()
    }
}

fn add_column(
    db: &Database,
    w: &Writer,
    t: &crate::schema::Table,
    def: &crate::sql::ast::ColumnDef,
    text: &str,
) -> Result<()> {
    if t.column_index(&def.name).is_some() {
        return Err(Error::Sql(format!("duplicate column name: {}", def.name)));
    }
    if def.primary_key {
        return Err(Error::Sql("Cannot add a PRIMARY KEY column".into()));
    }
    if def.unique {
        return Err(Error::Sql("Cannot add a UNIQUE column".into()));
    }
    if def.generated {
        return Err(Error::Unsupported("generated columns".into()));
    }
    if let Some(d) = &def.default {
        let constant = matches!(
            d,
            crate::sql::ast::Expr::Literal(_) | crate::sql::ast::Expr::Unary(_, _)
        );
        if !constant {
            return Err(Error::Sql(
                "Cannot add a column with non-constant default".into(),
            ));
        }
    }
    let rows = crate::btree::count_entries(&db.pager, t.root)?;
    let null_default = matches!(
        &def.default,
        None | Some(crate::sql::ast::Expr::Literal(Value::Null))
    );
    if def.not_null && null_default && rows > 0 {
        return Err(Error::Sql(
            "Cannot add a NOT NULL column with default value NULL".into(),
        ));
    }
    // Splice the definition in before the table's closing parenthesis.
    let schema = db.schema();
    let r = schema
        .rows
        .iter()
        .find(|r| r.kind == "table" && r.name.eq_ignore_ascii_case(&t.name))
        .unwrap();
    let sql = r.sql.clone().unwrap_or_default();
    let toks = tokenize(&sql)?;
    let mut depth = 0i32;
    let mut close = None;
    for tk in &toks {
        match tk.tok {
            Tok::Op("(") => depth += 1,
            Tok::Op(")") => {
                depth -= 1;
                if depth == 0 {
                    close = Some(tk.start);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close.ok_or_else(|| {
        Error::Unsupported("ALTER TABLE on a table created AS SELECT without columns".into())
    })?;
    let new_sql = format!(
        "{}, {}{}",
        sql[..close].trim_end(),
        text.trim(),
        &sql[close..]
    );
    update_row(db, w, r, &r.name, &r.tbl_name, Some(&new_sql))?;
    Ok(())
}

fn drop_column(db: &Database, w: &Writer, t: &crate::schema::Table, col: &str) -> Result<()> {
    let Some(ci) = t.column_index(col) else {
        return Err(Error::Sql(format!("no such column: \"{col}\"")));
    };
    let fail = |why: &str| Err(Error::Sql(format!("cannot drop column \"{col}\": {why}")));
    // The same refusals, in the same words, as SQLite.
    if t.ipk == Some(ci) || t.pk.iter().any(|k| k.col == ci) {
        return Err(Error::Sql(format!(
            "cannot drop PRIMARY KEY column: \"{col}\""
        )));
    }
    for ix in &t.indexes {
        if ix.cols.iter().any(|c| c.col == ci) {
            return Err(Error::Sql(match &ix.sql {
                None => format!("cannot drop UNIQUE column: \"{col}\""),
                Some(_) => format!(
                    "error in index {} after drop column: no such column: {col}",
                    ix.name
                ),
            }));
        }
    }
    if t.columns.len() == 1 {
        return fail("no other columns exist");
    }
    let schema = db.schema();
    for r in &schema.rows {
        let Some(sql) = &r.sql else { continue };
        if r.kind == "table" && r.name.eq_ignore_ascii_case(&t.name) {
            continue;
        }
        let toks = tokenize(sql)?;
        if !column_refs(db, &toks, r, &t.name, col).is_empty() {
            return fail(&format!("used by {} {}", r.kind, r.name));
        }
    }
    if t.checks.iter().any(|(_, e)| ast_mentions(e, col)) {
        return fail("used by a CHECK constraint");
    }
    // Remove the definition from the stored SQL: from its name up to the
    // next top-level comma (or the preceding one, for the last column).
    let r = schema
        .rows
        .iter()
        .find(|r| r.kind == "table" && r.name.eq_ignore_ascii_case(&t.name))
        .unwrap();
    let sql = r.sql.clone().unwrap_or_default();
    let toks = tokenize(&sql)?;
    let mut depth = 0;
    let mut starts: Vec<usize> = Vec::new(); // token index starting each definition
    let mut seps: Vec<usize> = Vec::new(); // token index of each top-level comma
    let mut close = toks.len();
    for (i, tk) in toks.iter().enumerate() {
        match tk.tok {
            Tok::Op("(") => {
                depth += 1;
                if depth == 1 {
                    starts.push(i + 1);
                }
            }
            Tok::Op(")") => {
                depth -= 1;
                if depth == 0 {
                    close = i;
                    break;
                }
            }
            Tok::Op(",") if depth == 1 => {
                seps.push(i);
                starts.push(i + 1);
            }
            _ => {}
        }
    }
    let k = starts
        .iter()
        .position(|&s| toks.get(s).is_some_and(|tk| is_ident(&tk.tok, col)))
        .ok_or_else(|| Error::Unsupported("could not find the column in the table's SQL".into()))?;
    let (from, to) = if k + 1 < starts.len() {
        // Remove "name ... ," (through the comma that follows it).
        (toks[starts[k]].start, toks[starts[k + 1]].start)
    } else {
        // The last item: remove ", name ...".
        (toks[seps[k - 1]].start, toks[close].start)
    };
    let new_sql = format!("{}{}", &sql[..from], &sql[to..]);
    update_row(db, w, r, &r.name, &r.tbl_name, Some(&new_sql))?;
    // Rewrite every row without the column.
    let enc = db.pager.encoding();
    let mut c = crate::btree::Cursor::new(db.pager.clone(), t.root);
    c.first()?;
    let mut rows = Vec::new();
    while c.valid() {
        let rowid = if t.without_rowid { 0 } else { c.rowid()? };
        let row = crate::exec::decode_table_row(t, c.payload()?.as_slice(), rowid, enc, None)?;
        rows.push((rowid, row[..t.columns.len()].to_vec()));
        c.next()?;
    }
    for (rowid, mut row) in rows {
        if let Some(ipk) = t.ipk {
            row[ipk] = Value::Null;
        }
        if t.without_rowid {
            // Stored in storage order, without the dropped column.
            let stored: Vec<Value> = t
                .storage_order
                .iter()
                .filter(|c| **c != ci)
                .map(|c| row[*c].clone())
                .collect();
            let pk = t.pk_index.clone().unwrap();
            let order = crate::btree_write::KeyOrder {
                fields: pk.cols.iter().map(|c| (c.collation, c.desc)).collect(),
            };
            w.index_insert(
                t.root,
                &encode_record_enc(&stored, enc),
                &order,
                pk.cols.len(),
                true,
            )?;
        } else {
            row.remove(ci);
            w.table_insert(t.root, rowid, &encode_record_enc(&row, enc))?;
        }
    }
    Ok(())
}

/// Does an expression mention column `col` (by name)?
fn ast_mentions(e: &crate::sql::ast::Expr, col: &str) -> bool {
    let text = format!("{e:?}");
    text.contains(&format!("name: {:?}", col))
        || text
            .to_ascii_lowercase()
            .contains(&format!("name: {:?}", col.to_ascii_lowercase()))
}
