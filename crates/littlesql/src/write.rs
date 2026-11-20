//! Statements that change the database: CREATE, DROP, INSERT, UPDATE,
//! DELETE, and transactions.
//!
//! Every write runs inside a transaction. Without BEGIN, each statement is
//! its own transaction and commits when it succeeds; a failing statement
//! leaves no trace. Inside BEGIN ... COMMIT a failing statement is undone
//! on its own (a savepoint) and the transaction carries on.
//!
//! Row selection for UPDATE and DELETE reuses the query planner: the
//! statement becomes a SELECT of the target rows, which is collected in
//! full before anything changes (so an update can never see its own
//! output).

use crate::btree::{Cursor, LEAF_TABLE};
use crate::btree_write::{KeyOrder, Writer};
use crate::db::{Database, ResultSet};
use crate::error::{Error, Result};
use crate::pager::HEADER_SIZE;
use crate::record::{decode_all, encode_record_enc};
use crate::schema::{Index, Table};
use crate::sql::ast::{
    self, CreateIndex, CreateTable, CreateView, DropKind, Expr as AExpr, FromItem, Indexed,
    InsertSource, JoinKind, OnConflict, ResultColumn, Select, SelectBody, SelectCore, Stmt,
    TableSource, TriggerEvent, TriggerTiming,
};
use crate::sql::lexer::{tokenize, Tok};
use crate::value::{Affinity, Value};
use std::cmp::Ordering;
use std::rc::Rc;

/// SQLite 3.45.1, the version whose file format this engine writes.
const SQLITE_VERSION_NUMBER: u32 = 3_045_001;

pub(crate) fn run_write(db: &Database, stmt: &Stmt, text: &str) -> Result<ResultSet> {
    match stmt {
        Stmt::Begin => {
            if db.explicit_txn.get() {
                return Err(Error::Sql(
                    "cannot start a transaction within a transaction".into(),
                ));
            }
            db.explicit_txn.set(true);
            return Ok(ResultSet::default());
        }
        Stmt::Commit => {
            if !db.explicit_txn.get() {
                return Err(Error::Sql(
                    "cannot commit - no transaction is active".into(),
                ));
            }
            commit(db)?;
            db.explicit_txn.set(false);
            return Ok(ResultSet::default());
        }
        Stmt::Rollback => {
            if !db.explicit_txn.get() {
                return Err(Error::Sql(
                    "cannot rollback - no transaction is active".into(),
                ));
            }
            db.pager.rollback();
            db.explicit_txn.set(false);
            db.reload_schema()?;
            return Ok(ResultSet::default());
        }
        _ => {}
    }
    prepare_file(db)?;
    let sp = db.pager.savepoint();
    let before = crate::func::counters();
    match execute(db, stmt, text) {
        Ok(rs) => {
            if !db.explicit_txn.get() {
                if let Err(e) = commit(db) {
                    db.pager.rollback();
                    db.reload_schema()?;
                    return Err(e);
                }
            }
            let mut c = crate::func::counters();
            c.changes = rs.changes as i64;
            c.total_changes = before.total_changes + rs.changes as i64;
            crate::func::set_counters(c);
            Ok(rs)
        }
        Err(e) => {
            if db.explicit_txn.get() {
                db.pager.restore(sp);
            } else {
                db.pager.rollback();
            }
            crate::func::set_counters(crate::func::ConnCounters {
                last_insert_rowid: before.last_insert_rowid,
                ..crate::func::counters()
            });
            db.reload_schema()?;
            Err(e)
        }
    }
}

/// Before the first change: give a new database its first page, and move a
/// file-backed database to WAL mode on disk (see DECISIONS D12).
fn prepare_file(db: &Database) -> Result<()> {
    let pager = &db.pager;
    if !pager.is_writable() {
        return Err(Error::Runtime(
            "attempt to write a readonly database".into(),
        ));
    }
    if pager.page_count() == 0 {
        let size = pager.page_size();
        let mut p1 = vec![0u8; size];
        p1[..16].copy_from_slice(b"SQLite format 3\0");
        let ps = if size == 65536 { 1u16 } else { size as u16 };
        p1[16..18].copy_from_slice(&ps.to_be_bytes());
        let mode = if pager.has_wal() { 2 } else { 1 };
        p1[18] = mode;
        p1[19] = mode;
        p1[20] = 0;
        p1[21] = 64;
        p1[22] = 32;
        p1[23] = 32;
        p1[24..28].copy_from_slice(&1u32.to_be_bytes()); // change counter
        p1[28..32].copy_from_slice(&1u32.to_be_bytes()); // size in pages
        p1[44..48].copy_from_slice(&4u32.to_be_bytes()); // schema format
        p1[56..60].copy_from_slice(&1u32.to_be_bytes()); // UTF-8
        p1[92..96].copy_from_slice(&1u32.to_be_bytes()); // version-valid-for
        p1[96..100].copy_from_slice(&SQLITE_VERSION_NUMBER.to_be_bytes());
        p1[HEADER_SIZE] = LEAF_TABLE;
        let end = pager.usable_size();
        let start = if end == 65536 { 0 } else { end as u16 };
        p1[HEADER_SIZE + 5..HEADER_SIZE + 7].copy_from_slice(&start.to_be_bytes());
        pager.init_direct(&p1)?;
        return Ok(());
    }
    if pager.has_wal() && !pager.in_transaction() {
        let p1 = pager.get(1)?;
        if p1[18] != 2 || p1[19] != 2 {
            if p1[18] > 2 || p1[19] > 2 {
                return Err(Error::Unsupported(format!(
                    "file format write version {}",
                    p1[18]
                )));
            }
            let mut p = p1.to_vec();
            p[18] = 2;
            p[19] = 2;
            pager.write_header_direct(&p)?;
        }
    }
    Ok(())
}

/// Commit the open transaction: update the file header, then make the
/// pages durable (and checkpoint now and then).
pub(crate) fn commit(db: &Database) -> Result<()> {
    let pager = &db.pager;
    if !pager.in_transaction() {
        return Ok(());
    }
    let mut p1 = pager.get(1)?.to_vec();
    let counter = u32::from_be_bytes(p1[24..28].try_into().unwrap()).wrapping_add(1);
    p1[24..28].copy_from_slice(&counter.to_be_bytes());
    p1[28..32].copy_from_slice(&pager.page_count().to_be_bytes());
    p1[92..96].copy_from_slice(&counter.to_be_bytes());
    p1[96..100].copy_from_slice(&SQLITE_VERSION_NUMBER.to_be_bytes());
    pager.write_page(1, p1)?;
    pager.commit()?;
    // Like SQLite's default, checkpoint once the WAL holds 1000 pages.
    if pager.wal_frames() >= 1000 {
        pager.checkpoint()?;
    }
    Ok(())
}

fn execute(db: &Database, stmt: &Stmt, text: &str) -> Result<ResultSet> {
    match stmt {
        Stmt::CreateTable(ct) => create_table(db, ct, text),
        Stmt::CreateIndex(ci) => create_index(db, ci, text),
        Stmt::CreateView(cv) => create_view(db, cv, text),
        Stmt::Drop {
            kind,
            if_exists,
            name,
        } => drop(db, *kind, *if_exists, name),
        Stmt::Insert(ins) => insert(db, ins),
        Stmt::Update(up) => update(db, up),
        Stmt::Delete(del) => delete(db, del),
        Stmt::CreateTrigger(ct) => create_trigger(db, ct, text),
        Stmt::Reindex(name) => reindex(db, name.as_deref()),
        other => Err(Error::Unsupported(format!("statement {other:?}"))),
    }
}

// ---------------------------------------------------------------- schema

/// The text SQLite stores for a CREATE statement: `CREATE <KIND> ` and then
/// the statement as written from the object's name on (so `IF NOT EXISTS`,
/// `TEMP` and keyword spacing are dropped).
fn stored_sql(text: &str, kind: &str) -> Result<String> {
    let toks = tokenize(text)?;
    let mut i = 0;
    let word =
        |i: usize, w: &str| matches!(&toks[i].tok, Tok::Word(x) if x.eq_ignore_ascii_case(w));
    while i < toks.len() && !word(i, kind.rsplit(' ').next().unwrap()) {
        i += 1;
    }
    i += 1;
    if i + 2 < toks.len() && word(i, "IF") && word(i + 1, "NOT") && word(i + 2, "EXISTS") {
        i += 3;
    }
    let start = toks.get(i).map_or(text.len(), |t| t.start);
    let body = text[start..].trim_end().trim_end_matches(';').trim_end();
    Ok(format!("CREATE {kind} {body}"))
}

fn name_taken(db: &Database, name: &str) -> Option<&'static str> {
    let schema = db.schema();
    if schema.table(name).is_some() {
        Some("table")
    } else if schema.view(name).is_some() {
        Some("view")
    } else if schema.index(name).is_some() {
        Some("index")
    } else {
        None
    }
}

fn bump_schema_cookie(w: &Writer, db: &Database) -> Result<()> {
    let p1 = db.pager.get(1)?;
    let cookie = u32::from_be_bytes(p1[40..44].try_into().unwrap()).wrapping_add(1);
    w.set_header_u32(40, cookie)
}

fn schema_insert(
    db: &Database,
    w: &Writer,
    kind: &str,
    name: &str,
    tbl: &str,
    root: u32,
    sql: Option<&str>,
) -> Result<()> {
    let rowid = max_rowid(db, 1)?.map_or(1, |r| r + 1);
    let rec = encode_record_enc(
        &[
            Value::text(kind),
            Value::text(name),
            Value::text(tbl),
            Value::Int(root as i64),
            sql.map_or(Value::Null, Value::text),
        ],
        db.pager.encoding(),
    );
    w.table_insert(1, rowid, &rec)
}

fn max_rowid(db: &Database, root: u32) -> Result<Option<i64>> {
    let mut c = Cursor::new(db.pager.clone(), root);
    c.last()?;
    if c.valid() {
        Ok(Some(c.rowid()?))
    } else {
        Ok(None)
    }
}

fn create_table(db: &Database, ct: &CreateTable, text: &str) -> Result<ResultSet> {
    if ct.temp {
        return Err(Error::Unsupported("TEMP tables".into()));
    }
    if let Some(kind) = name_taken(db, &ct.name) {
        if ct.if_not_exists && kind == "table" || ct.if_not_exists && kind == "view" {
            return Ok(ResultSet::default());
        }
        return Err(Error::Sql(format!("{kind} {} already exists", ct.name)));
    }
    if ct.name.to_ascii_lowercase().starts_with("sqlite_") {
        return Err(Error::Sql(format!(
            "object name reserved for internal use: {}",
            ct.name
        )));
    }
    let w = Writer::new(&db.pager);
    // CREATE TABLE ... AS SELECT: columns from the query.
    let (ct, sql, rows) = match &ct.as_select {
        Some(sel) => {
            let rs = db.run_select(sel, true)?;
            let plan = crate::bind::bind_select(&db.schema(), &db.pager, sel)?;
            let mut cols = Vec::new();
            let mut defs = Vec::new();
            for (name, c) in rs.columns.iter().zip(&plan.query.cols) {
                let ty = match c.aff {
                    Affinity::Integer => "INT",
                    Affinity::Real => "REAL",
                    Affinity::Text => "TEXT",
                    Affinity::Numeric => "NUM",
                    Affinity::Blob => "",
                };
                defs.push(if ty.is_empty() {
                    quote_ident(name)
                } else {
                    format!("{} {ty}", quote_ident(name))
                });
                cols.push(ast::ColumnDef {
                    name: name.clone(),
                    type_name: ty.to_string(),
                    primary_key: false,
                    pk_desc: false,
                    autoincrement: false,
                    not_null: false,
                    unique: false,
                    default: None,
                    collation: None,
                    generated: false,
                    checks: Vec::new(),
                });
            }
            let sql = format!("CREATE TABLE {}({})", quote_ident(&ct.name), defs.join(","));
            let new = CreateTable {
                name: ct.name.clone(),
                temp: false,
                if_not_exists: false,
                columns: cols,
                constraints: Vec::new(),
                without_rowid: false,
                as_select: None,
            };
            (new, sql, Some(rs.rows))
        }
        None => (ct.clone(), stored_sql(text, "TABLE")?, None),
    };
    if ct.columns.iter().any(|c| c.generated) {
        return Err(Error::Unsupported("generated columns".into()));
    }
    // Validate the definition the same way the schema loader will.
    let table = crate::schema::table_from_ast(&ct, &ct.name, 0, &sql)?;
    let root = w.create_tree(ct.without_rowid)?;
    schema_insert(db, &w, "table", &ct.name, &ct.name, root, Some(&sql))?;
    let keys = crate::schema::auto_index_keys(&ct, &table)?;
    for n in 1..=keys.len() {
        let ix_root = w.create_tree(true)?;
        let name = format!("sqlite_autoindex_{}_{n}", ct.name);
        schema_insert(db, &w, "index", &name, &ct.name, ix_root, None)?;
    }
    if table.autoincrement && db.schema().table("sqlite_sequence").is_none() {
        let seq_root = w.create_tree(false)?;
        schema_insert(
            db,
            &w,
            "table",
            "sqlite_sequence",
            "sqlite_sequence",
            seq_root,
            Some("CREATE TABLE sqlite_sequence(name,seq)"),
        )?;
    }
    bump_schema_cookie(&w, db)?;
    db.reload_schema()?;
    let mut changes = 0;
    if let Some(rows) = rows {
        let table = db.schema().table(&ct.name).cloned().unwrap();
        let mut ins = Inserter::new(db, &table, OnConflict::Abort)?;
        for row in rows {
            ins.insert_row(row, None)?;
            changes += 1;
        }
    }
    let _ = changes;
    Ok(ResultSet::default())
}

fn quote_ident(name: &str) -> String {
    let plain = !name.is_empty()
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !crate::sql::parser::is_reserved(name);
    if plain {
        name.to_string()
    } else {
        format!("\"{}\"", name.replace('"', "\"\""))
    }
}

fn create_index(db: &Database, ci: &CreateIndex, text: &str) -> Result<ResultSet> {
    if let Some(kind) = name_taken(db, &ci.name) {
        if ci.if_not_exists && kind == "index" {
            return Ok(ResultSet::default());
        }
        return Err(Error::Sql(format!("{kind} {} already exists", ci.name)));
    }
    let schema = db.schema();
    let table = match schema.table(&ci.table) {
        Some(t) => t.clone(),
        None if schema.view(&ci.table).is_some() => {
            return Err(Error::Sql("views may not be indexed".into()))
        }
        None => return Err(Error::Sql(format!("no such table: main.{}", ci.table))),
    };
    if table.name.eq_ignore_ascii_case("sqlite_schema") || table.name.starts_with("sqlite_") {
        return Err(Error::Sql(format!(
            "table {} may not be indexed",
            table.name
        )));
    }
    if ci.partial {
        return Err(Error::Unsupported("partial indexes".into()));
    }
    for c in &ci.columns {
        match &c.name {
            None => return Err(Error::Unsupported("indexes on expressions".into())),
            Some(n) if table.column_index(n).is_none() => {
                return Err(Error::Sql(format!("no such column: {n}")))
            }
            _ => {}
        }
    }
    let w = Writer::new(&db.pager);
    let root = w.create_tree(true)?;
    let sql = stored_sql(text, if ci.unique { "UNIQUE INDEX" } else { "INDEX" })?;
    schema_insert(db, &w, "index", &ci.name, &table.name, root, Some(&sql))?;
    bump_schema_cookie(&w, db)?;
    db.reload_schema()?;
    // Fill the new index from the table's rows.
    let table = db.schema().table(&ci.table).cloned().unwrap();
    let index = table
        .indexes
        .iter()
        .find(|i| i.name.eq_ignore_ascii_case(&ci.name))
        .cloned()
        .ok_or_else(|| Error::Sql(format!("index {} could not be read back", ci.name)))?;
    for (rowid, row) in all_rows(db, &table)? {
        let ins = Inserter::new(db, &table, OnConflict::Abort)?;
        ins.check_unique_one(&index, &row, rowid)?;
        ins.add_index_entry(&index, &row, rowid)?;
    }
    Ok(ResultSet::default())
}

fn create_view(db: &Database, cv: &CreateView, text: &str) -> Result<ResultSet> {
    if let Some(kind) = name_taken(db, &cv.name) {
        if cv.if_not_exists && kind == "view" {
            return Ok(ResultSet::default());
        }
        return Err(Error::Sql(format!("{kind} {} already exists", cv.name)));
    }
    // The query must make sense now (SQLite checks it the same way).
    let rs = db.run_select(&cv.select, false)?;
    if !cv.columns.is_empty() && cv.columns.len() != rs.columns.len() {
        return Err(Error::Sql(format!(
            "expected {} columns for '{}' but got {}",
            cv.columns.len(),
            cv.name,
            rs.columns.len()
        )));
    }
    let sql = stored_sql(text, "VIEW")?;
    if cv.temp {
        // A TEMP view belongs to this connection and is never stored.
        db.temp_views.borrow_mut().push(sql);
        db.reload_schema()?;
        return Ok(ResultSet::default());
    }
    let w = Writer::new(&db.pager);
    schema_insert(db, &w, "view", &cv.name, &cv.name, 0, Some(&sql))?;
    bump_schema_cookie(&w, db)?;
    db.reload_schema()?;
    Ok(ResultSet::default())
}

fn drop(db: &Database, kind: DropKind, if_exists: bool, name: &str) -> Result<ResultSet> {
    let schema = db.schema();
    let w = Writer::new(&db.pager);
    let lname = name.to_ascii_lowercase();
    let missing = |what: &str| {
        if if_exists {
            Ok(ResultSet::default())
        } else {
            Err(Error::Sql(format!("no such {what}: {name}")))
        }
    };
    // Schema rows to remove, and b-trees to free.
    let mut remove: Vec<i64> = Vec::new();
    let mut trees: Vec<u32> = Vec::new();
    match kind {
        DropKind::Table => {
            let Some(t) = schema.table(name).cloned() else {
                if schema.view(name).is_some() {
                    return Err(Error::Sql(format!("use DROP VIEW to delete view {name}")));
                }
                return missing("table");
            };
            if lname.starts_with("sqlite_") {
                return Err(Error::Sql(format!("table {name} may not be dropped")));
            }
            for r in &schema.rows {
                if r.tbl_name.eq_ignore_ascii_case(name)
                    && matches!(r.kind.as_str(), "table" | "index" | "trigger")
                {
                    remove.push(r.rowid);
                    if r.root > 0 {
                        trees.push(r.root);
                    }
                }
            }
            if t.autoincrement {
                if let Some(seq) = schema.table("sqlite_sequence").cloned() {
                    for (rowid, row) in all_rows(db, &seq)? {
                        if row[0].to_text().eq_ignore_ascii_case(name) {
                            w.table_delete(seq.root, rowid)?;
                        }
                    }
                }
            }
        }
        DropKind::Index => {
            let Some((_, ix)) = schema.index(name) else {
                return missing("index");
            };
            if ix.sql.is_none() {
                return Err(Error::Sql(
                    "index associated with UNIQUE or PRIMARY KEY constraint cannot be dropped"
                        .into(),
                ));
            }
            for r in &schema.rows {
                if r.kind == "index" && r.name.eq_ignore_ascii_case(name) {
                    remove.push(r.rowid);
                    trees.push(r.root);
                }
            }
        }
        DropKind::View => {
            let mut temps = db.temp_views.borrow_mut();
            let before = temps.len();
            temps.retain(|sql| {
                !matches!(crate::sql::parser::parse_one(sql),
                    Ok(Stmt::CreateView(v)) if v.name.eq_ignore_ascii_case(name))
            });
            if temps.len() != before {
                std::mem::drop(temps);
                db.reload_schema()?;
                return Ok(ResultSet::default());
            }
            std::mem::drop(temps);
            if schema.view(name).is_none() {
                if schema.table(name).is_some() {
                    return Err(Error::Sql(format!("use DROP TABLE to delete table {name}")));
                }
                return missing("view");
            }
            for r in &schema.rows {
                let own_trigger = r.kind == "trigger" && r.tbl_name.eq_ignore_ascii_case(name);
                if (r.kind == "view" && r.name.eq_ignore_ascii_case(name)) || own_trigger {
                    remove.push(r.rowid);
                }
            }
        }
        DropKind::Trigger => {
            let Some(row) = schema
                .rows
                .iter()
                .find(|r| r.kind == "trigger" && r.name.eq_ignore_ascii_case(name))
            else {
                return missing("trigger");
            };
            remove.push(row.rowid);
        }
    }
    for rowid in remove {
        w.table_delete(1, rowid)?;
    }
    for root in trees {
        w.drop_tree(root)?;
    }
    bump_schema_cookie(&w, db)?;
    db.reload_schema()?;
    Ok(ResultSet::default())
}

fn create_trigger(db: &Database, ct: &ast::CreateTrigger, text: &str) -> Result<ResultSet> {
    if ct.temp {
        return Err(Error::Unsupported("TEMP triggers".into()));
    }
    let schema = db.schema();
    if schema.trigger(&ct.name).is_some() {
        if ct.if_not_exists {
            return Ok(ResultSet::default());
        }
        return Err(Error::Sql(format!("trigger {} already exists", ct.name)));
    }
    let on_view = schema.view(&ct.table).is_some();
    match (schema.table(&ct.table), on_view, ct.timing) {
        (Some(t), _, TriggerTiming::InsteadOf) => {
            return Err(Error::Sql(format!(
                "cannot create INSTEAD OF trigger on table: {}",
                t.name
            )))
        }
        (Some(t), _, _) if t.name.to_ascii_lowercase().starts_with("sqlite_") => {
            return Err(Error::Sql("cannot create trigger on system table".into()))
        }
        (Some(_), _, _) => {}
        (None, true, TriggerTiming::InsteadOf) => {}
        (None, true, t) => {
            let when = if t == TriggerTiming::Before {
                "BEFORE"
            } else {
                "AFTER"
            };
            return Err(Error::Sql(format!(
                "cannot create {when} trigger on view: {}",
                ct.table
            )));
        }
        (None, false, _) => return Err(Error::Sql(format!("no such table: main.{}", ct.table))),
    }
    let w = Writer::new(&db.pager);
    let sql = stored_sql(text, "TRIGGER")?;
    let table_name = schema
        .table(&ct.table)
        .map(|t| t.name.clone())
        .or_else(|| schema.view(&ct.table).map(|v| v.name.clone()))
        .unwrap();
    schema_insert(db, &w, "trigger", &ct.name, &table_name, 0, Some(&sql))?;
    bump_schema_cookie(&w, db)?;
    db.reload_schema()?;
    Ok(ResultSet::default())
}

fn reindex(db: &Database, name: Option<&str>) -> Result<ResultSet> {
    // Indexes here are always up to date; only the name is checked.
    if let Some(n) = name {
        let schema = db.schema();
        let known = schema.table(n).is_some()
            || schema.index(n).is_some()
            || crate::value::Collation::from_name(n).is_some();
        if !known {
            return Err(Error::Sql(
                "unable to identify the object to be reindexed".into(),
            ));
        }
    }
    Ok(ResultSet::default())
}

#[derive(Clone, Copy, PartialEq)]
enum Event {
    Insert,
    Update,
    Delete,
}

thread_local! {
    /// Triggers currently running, innermost last. A trigger never fires
    /// itself again (SQLite's default, recursive_triggers = OFF).
    static FIRING: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The triggers on `table` for this timing and event.
fn triggers_for(
    db: &Database,
    table: &str,
    timing: TriggerTiming,
    event: Event,
    set_cols: &[String],
) -> Vec<Rc<crate::schema::Trigger>> {
    db.schema()
        .triggers
        .iter()
        .filter(|t| t.table.eq_ignore_ascii_case(table) && t.def.timing == timing)
        .filter(|t| match (&t.def.event, event) {
            (TriggerEvent::Insert, Event::Insert) | (TriggerEvent::Delete, Event::Delete) => true,
            (TriggerEvent::Update(cols), Event::Update) => {
                cols.is_empty()
                    || cols
                        .iter()
                        .any(|c| set_cols.iter().any(|s| s.eq_ignore_ascii_case(c)))
            }
            _ => false,
        })
        .cloned()
        .collect()
}

fn has_triggers(db: &Database, table: &str, event: Event) -> bool {
    db.schema().triggers.iter().any(|t| {
        t.table.eq_ignore_ascii_case(table)
            && matches!(
                (&t.def.event, event),
                (TriggerEvent::Insert, Event::Insert)
                    | (TriggerEvent::Delete, Event::Delete)
                    | (TriggerEvent::Update(_), Event::Update)
            )
    })
}

/// Run the matching triggers for one row.
fn fire(
    db: &Database,
    table: &str,
    timing: TriggerTiming,
    event: Event,
    set_cols: &[String],
    row: &crate::bind::TriggerRow,
) -> Result<()> {
    for t in triggers_for(db, table, timing, event, set_cols) {
        let running = FIRING.with(|f| f.borrow().iter().any(|n| n.eq_ignore_ascii_case(&t.name)));
        if running {
            continue;
        }
        let depth = FIRING.with(|f| f.borrow().len());
        if depth >= 1000 {
            return Err(Error::Runtime(
                "too many levels of trigger recursion".into(),
            ));
        }
        FIRING.with(|f| f.borrow_mut().push(t.name.clone()));
        let result = crate::bind::with_trigger_row(row.clone(), || -> Result<()> {
            if let Some(when) = &t.def.when {
                let rs = db.run_select(
                    &select_of(vec![result(when.clone())], Vec::new(), None),
                    true,
                )?;
                let go = rs
                    .rows
                    .first()
                    .and_then(|r| r.first())
                    .and_then(Value::truth);
                if go != Some(true) {
                    return Ok(());
                }
            }
            for stmt in &t.def.body {
                match stmt {
                    Stmt::Select(sel) => {
                        db.run_select(sel, true)?;
                    }
                    other => {
                        execute(db, other, "")?;
                    }
                }
            }
            Ok(())
        });
        FIRING.with(|f| f.borrow_mut().pop());
        result?;
    }
    Ok(())
}

fn trigger_row(
    table: &Table,
    new: Option<(i64, &[Value])>,
    old: Option<(i64, &[Value])>,
) -> crate::bind::TriggerRow {
    crate::bind::TriggerRow {
        columns: table.columns.iter().map(|c| c.name.clone()).collect(),
        new: new.map(|(r, v)| (r, v.to_vec())),
        old: old.map(|(r, v)| (r, v.to_vec())),
    }
}

// ---------------------------------------------------------------- rows

/// Every row of a table as (rowid, column values); rowid is 0 for
/// WITHOUT ROWID tables.
fn all_rows(db: &Database, table: &Table) -> Result<Vec<(i64, Vec<Value>)>> {
    let enc = db.pager.encoding();
    let mut out = Vec::new();
    let mut c = Cursor::new(db.pager.clone(), table.root);
    c.first()?;
    while c.valid() {
        let rowid = if table.without_rowid { 0 } else { c.rowid()? };
        let row = crate::exec::decode_table_row(table, c.payload()?.as_slice(), rowid, enc, None)?;
        out.push((rowid, row[..table.columns.len()].to_vec()));
        c.next()?;
    }
    Ok(out)
}

/// Inserts rows into one table, keeping its indexes and constraints.
struct Inserter<'d> {
    db: &'d Database,
    table: Rc<Table>,
    w: Writer<'d>,
    conflict: OnConflict,
    enc: crate::record::TextEncoding,
}

impl<'d> Inserter<'d> {
    fn new(db: &'d Database, table: &Rc<Table>, conflict: OnConflict) -> Result<Inserter<'d>> {
        if table.indexes.iter().any(|i| !i.usable) {
            return Err(Error::Unsupported(format!(
                "writing to table {}, which has an expression or partial index",
                table.name
            )));
        }
        Ok(Inserter {
            db,
            table: table.clone(),
            w: Writer::new(&db.pager),
            conflict,
            enc: db.pager.encoding(),
        })
    }

    fn order(&self, index: &Index) -> KeyOrder {
        let mut fields: Vec<_> = index.cols.iter().map(|c| (c.collation, c.desc)).collect();
        if let Some(pk) = &self.table.pk_index {
            if !index.primary {
                for e in &index.extra {
                    if let Some(p) = pk.cols.iter().find(|p| p.col == *e) {
                        fields.push((p.collation, p.desc));
                    }
                }
            }
        }
        KeyOrder { fields }
    }

    /// The index entry for a row: key columns, then the rowid (or the
    /// primary key columns not already in the key).
    fn entry(&self, index: &Index, row: &[Value], rowid: i64) -> Vec<Value> {
        let mut v: Vec<Value> = index.cols.iter().map(|c| row[c.col].clone()).collect();
        if self.table.without_rowid {
            v.extend(index.extra.iter().map(|c| row[*c].clone()));
        } else {
            v.push(Value::Int(rowid));
        }
        v
    }

    fn add_index_entry(&self, index: &Index, row: &[Value], rowid: i64) -> Result<()> {
        let entry = self.entry(index, row, rowid);
        let rec = encode_record_enc(&entry, self.enc);
        let n = entry.len();
        self.w
            .index_insert(index.root, &rec, &self.order(index), n, true)
    }

    fn remove_index_entry(&self, index: &Index, row: &[Value], rowid: i64) -> Result<()> {
        let entry = self.entry(index, row, rowid);
        let rec = encode_record_enc(&entry, self.enc);
        let n = entry.len();
        self.w
            .index_delete(index.root, &rec, &self.order(index), n)?;
        Ok(())
    }

    /// The row stored for `rowid`, if any.
    fn row_at(&self, rowid: i64) -> Result<Option<Vec<Value>>> {
        let mut c = Cursor::new(self.db.pager.clone(), self.table.root);
        c.seek_rowid(rowid)?;
        if c.valid() && c.rowid()? == rowid {
            let row = crate::exec::decode_table_row(
                &self.table,
                c.payload()?.as_slice(),
                rowid,
                self.enc,
                None,
            )?;
            return Ok(Some(row[..self.table.columns.len()].to_vec()));
        }
        Ok(None)
    }

    /// Find an entry of `index` equal to `key` on its key columns; returns
    /// the whole decoded entry.
    fn find_entry(&self, index: &Index, key: &[Value]) -> Result<Option<Vec<Value>>> {
        let order = self.order(index);
        let n = key.len();
        let enc = self.enc;
        let mut c = Cursor::new(self.db.pager.clone(), index.root);
        let mut cmp = |payload: &[u8]| -> Result<Ordering> {
            let vals = decode_all(payload, enc)?;
            Ok(order_cmp(&order, &vals, key, n))
        };
        c.seek(&mut cmp)?;
        if !c.valid() {
            return Ok(None);
        }
        let vals = decode_all(c.payload()?.as_slice(), enc)?;
        if order_cmp(&order, &vals, key, n) == Ordering::Equal {
            Ok(Some(vals))
        } else {
            Ok(None)
        }
    }

    /// The row (rowid, values) that a UNIQUE index says `row` collides
    /// with, other than `rowid` itself.
    fn unique_conflict(
        &self,
        index: &Index,
        row: &[Value],
        rowid: i64,
    ) -> Result<Option<(i64, Vec<Value>)>> {
        let key: Vec<Value> = index.cols.iter().map(|c| row[c.col].clone()).collect();
        if key.iter().any(Value::is_null) {
            return Ok(None);
        }
        let Some(entry) = self.find_entry(index, &key)? else {
            return Ok(None);
        };
        if self.table.without_rowid {
            let pk = self.table.pk_index.clone().unwrap();
            let mut full = vec![Value::Null; self.table.columns.len()];
            for (i, c) in index.cols.iter().enumerate() {
                full[c.col] = entry[i].clone();
            }
            for (j, c) in index.extra.iter().enumerate() {
                full[*c] = entry[index.cols.len() + j].clone();
            }
            let same = pk
                .cols
                .iter()
                .all(|c| full[c.col].compare(&row[c.col], c.collation) == Ordering::Equal);
            if same && !index.primary {
                return Ok(None);
            }
            let pkey: Vec<Value> = pk.cols.iter().map(|c| full[c.col].clone()).collect();
            let existing = self.find_entry(&pk, &pkey)?;
            return Ok(existing.map(|e| (0, self.wr_row(&e))));
        }
        let Some(Value::Int(other)) = entry.last().cloned() else {
            return Ok(None);
        };
        if other == rowid {
            return Ok(None);
        }
        Ok(self.row_at(other)?.map(|r| (other, r)))
    }

    /// A WITHOUT ROWID row from its stored record (storage order).
    fn wr_row(&self, stored: &[Value]) -> Vec<Value> {
        let mut row = vec![Value::Null; self.table.columns.len()];
        for (i, c) in self.table.storage_order.iter().enumerate() {
            row[*c] = stored.get(i).cloned().unwrap_or(Value::Null);
        }
        row
    }

    fn unique_error(&self, index: &Index) -> Error {
        let cols: Vec<String> = index
            .cols
            .iter()
            .map(|c| format!("{}.{}", self.table.name, self.table.columns[c.col].name))
            .collect();
        Error::Constraint(format!("UNIQUE constraint failed: {}", cols.join(", ")))
    }

    fn check_unique_one(&self, index: &Index, row: &[Value], rowid: i64) -> Result<()> {
        if index.unique && self.unique_conflict(index, row, rowid)?.is_some() {
            return Err(self.unique_error(index));
        }
        Ok(())
    }

    /// Apply column affinities and defaults' NOT NULL rules to a full row.
    /// Returns None when the row is to be skipped (OR IGNORE).
    fn prepare(&self, mut row: Vec<Value>) -> Result<Option<Vec<Value>>> {
        for (i, c) in self.table.columns.iter().enumerate() {
            let v = std::mem::replace(&mut row[i], Value::Null);
            row[i] = v.apply_affinity(c.affinity);
        }
        for (i, c) in self.table.columns.iter().enumerate() {
            let pk_not_null = self.table.without_rowid && self.table.pk.iter().any(|k| k.col == i);
            if (c.not_null || pk_not_null) && row[i].is_null() && self.table.ipk != Some(i) {
                match self.conflict {
                    OnConflict::Ignore => return Ok(None),
                    OnConflict::Replace if c.default.is_some() => {
                        row[i] = default_value(self.db, c)?.apply_affinity(c.affinity);
                    }
                    _ => {
                        return Err(Error::Constraint(format!(
                            "NOT NULL constraint failed: {}.{}",
                            self.table.name, c.name
                        )))
                    }
                }
            }
        }
        if !self.passes_checks(&row)? {
            return Ok(None);
        }
        Ok(Some(row))
    }

    /// Evaluate the table's CHECK constraints against a row. A false result
    /// is an error (or a skip under OR IGNORE); NULL passes, as in SQLite.
    fn passes_checks(&self, row: &[Value]) -> Result<bool> {
        if self.table.checks.is_empty() {
            return Ok(true);
        }
        // SELECT <check>, ... FROM (SELECT <value> AS <column>, ...)
        let cols: Vec<ResultColumn> = self
            .table
            .columns
            .iter()
            .zip(row)
            .map(|(c, v)| ResultColumn::Expr {
                expr: AExpr::Literal(v.clone()),
                alias: Some(c.name.clone()),
                text: String::new(),
            })
            .collect();
        let inner = select_of(cols, Vec::new(), None);
        let from = vec![FromItem {
            join: JoinKind::Inner,
            natural: false,
            source: TableSource::Subquery(Box::new(inner)),
            alias: Some(self.table.name.clone()),
            on: None,
            using: Vec::new(),
            indexed: Indexed::Any,
        }];
        let tests = self
            .table
            .checks
            .iter()
            .map(|(_, e)| result(e.clone()))
            .collect();
        let rs = self.db.run_select(&select_of(tests, from, None), true)?;
        let Some(vals) = rs.rows.first() else {
            return Ok(true);
        };
        for (v, (label, _)) in vals.iter().zip(&self.table.checks) {
            if v.truth() == Some(false) {
                if self.conflict == OnConflict::Ignore {
                    return Ok(false);
                }
                return Err(Error::Constraint(format!(
                    "CHECK constraint failed: {label}"
                )));
            }
        }
        Ok(true)
    }

    /// Choose the rowid for a new row of a rowid table.
    fn new_rowid(&self, row: &[Value], given: Option<i64>) -> Result<i64> {
        if let Some(ipk) = self.table.ipk {
            match &row[ipk] {
                Value::Null => {}
                Value::Int(i) => return Ok(*i),
                _ => return Err(Error::Constraint("datatype mismatch".into())),
            }
        }
        if let Some(r) = given {
            return Ok(r);
        }
        let max = max_rowid(self.db, self.table.root)?;
        let mut next = match max {
            None => 1,
            Some(i64::MAX) => {
                return Err(Error::Runtime("database or disk is full".into()));
            }
            Some(m) => m + 1,
        };
        if self.table.autoincrement {
            if let Some(seq) = self.sequence()? {
                next = next.max(seq + 1);
            }
        }
        Ok(next)
    }

    fn sequence(&self) -> Result<Option<i64>> {
        let Some(seq) = self.db.schema().table("sqlite_sequence").cloned() else {
            return Ok(None);
        };
        for (_, row) in all_rows(self.db, &seq)? {
            if row[0].to_text().eq_ignore_ascii_case(&self.table.name) {
                return Ok(Some(row[1].to_int()));
            }
        }
        Ok(None)
    }

    fn bump_sequence(&self, rowid: i64) -> Result<()> {
        if !self.table.autoincrement {
            return Ok(());
        }
        let Some(seq) = self.db.schema().table("sqlite_sequence").cloned() else {
            return Ok(());
        };
        let mut found = None;
        for (rid, row) in all_rows(self.db, &seq)? {
            if row[0].to_text().eq_ignore_ascii_case(&self.table.name) {
                found = Some((rid, row[1].to_int()));
            }
        }
        let rec =
            |v: i64| encode_record_enc(&[Value::text(&self.table.name), Value::Int(v)], self.enc);
        match found {
            Some((_, cur)) if cur >= rowid => Ok(()),
            Some((rid, _)) => self.w.table_insert(seq.root, rid, &rec(rowid)),
            None => {
                let rid = max_rowid(self.db, seq.root)?.map_or(1, |r| r + 1);
                self.w.table_insert(seq.root, rid, &rec(rowid))
            }
        }
    }

    /// Remove a row and its index entries.
    fn delete_row(&self, rowid: i64, row: &[Value]) -> Result<()> {
        for ix in &self.table.indexes {
            self.remove_index_entry(ix, row, rowid)?;
        }
        if self.table.without_rowid {
            let pk = self.table.pk_index.clone().unwrap();
            let key: Vec<Value> = self
                .table
                .storage_order
                .iter()
                .map(|c| row[*c].clone())
                .collect();
            let rec = encode_record_enc(&key, self.enc);
            self.w
                .index_delete(self.table.root, &rec, &self.order(&pk), pk.cols.len())?;
        } else {
            self.w.table_delete(self.table.root, rowid)?;
        }
        Ok(())
    }

    /// Resolve every uniqueness conflict for `row` (about to be stored with
    /// `rowid`): an error, a skip (Ok(false)), or deleting the old rows.
    fn resolve_conflicts(&self, row: &[Value], rowid: i64) -> Result<bool> {
        // The rowid itself.
        if !self.table.without_rowid {
            if let Some(old) = self.row_at(rowid)? {
                match self.conflict {
                    OnConflict::Ignore => return Ok(false),
                    OnConflict::Replace => self.delete_row(rowid, &old)?,
                    OnConflict::Abort => {
                        let col = match self.table.ipk {
                            Some(i) => {
                                format!("{}.{}", self.table.name, self.table.columns[i].name)
                            }
                            None => format!("{}.rowid", self.table.name),
                        };
                        return Err(Error::Constraint(format!(
                            "UNIQUE constraint failed: {col}"
                        )));
                    }
                }
            }
        }
        // A WITHOUT ROWID primary key.
        if self.table.without_rowid {
            let pk = self.table.pk_index.clone().unwrap();
            let key: Vec<Value> = pk.cols.iter().map(|c| row[c.col].clone()).collect();
            if let Some(found) = self.find_entry(&pk, &key)? {
                let old = self.wr_row(&found);
                {
                    match self.conflict {
                        OnConflict::Ignore => return Ok(false),
                        OnConflict::Replace => self.delete_row(0, &old)?,
                        OnConflict::Abort => {
                            let cols: Vec<String> = pk
                                .cols
                                .iter()
                                .map(|c| {
                                    format!(
                                        "{}.{}",
                                        self.table.name, self.table.columns[c.col].name
                                    )
                                })
                                .collect();
                            return Err(Error::Constraint(format!(
                                "UNIQUE constraint failed: {}",
                                cols.join(", ")
                            )));
                        }
                    }
                }
            }
        }
        // UNIQUE indexes.
        for ix in self.table.indexes.iter().filter(|i| i.unique) {
            let me = if self.table.without_rowid { 0 } else { rowid };
            if let Some((other, old)) = self.unique_conflict(ix, row, me)? {
                match self.conflict {
                    OnConflict::Ignore => return Ok(false),
                    OnConflict::Replace => self.delete_row(other, &old)?,
                    OnConflict::Abort => return Err(self.unique_error(ix)),
                }
            }
        }
        Ok(true)
    }

    /// Store a new row (values in column order, before affinity).
    /// Returns the rowid, or None if the row was skipped.
    fn insert_row(&mut self, row: Vec<Value>, given_rowid: Option<i64>) -> Result<Option<i64>> {
        Ok(self.insert_row_full(row, given_rowid)?.map(|(r, _)| r))
    }

    /// Like `insert_row`, also returning the row as stored.
    fn insert_row_full(
        &mut self,
        row: Vec<Value>,
        given_rowid: Option<i64>,
    ) -> Result<Option<(i64, Vec<Value>)>> {
        let Some(mut row) = self.prepare(row)? else {
            return Ok(None);
        };
        let rowid = if self.table.without_rowid {
            0
        } else {
            self.new_rowid(&row, given_rowid)?
        };
        if !self.resolve_conflicts(&row, rowid)? {
            return Ok(None);
        }
        if let Some(ipk) = self.table.ipk {
            row[ipk] = Value::Int(rowid);
        }
        self.store(&row, rowid)?;
        if !self.table.without_rowid {
            self.bump_sequence(rowid)?;
        }
        Ok(Some((rowid, row)))
    }

    /// Write the table record and index entries of a row.
    fn store(&self, row: &[Value], rowid: i64) -> Result<()> {
        if self.table.without_rowid {
            let pk = self.table.pk_index.clone().unwrap();
            let stored: Vec<Value> = self
                .table
                .storage_order
                .iter()
                .map(|c| row[*c].clone())
                .collect();
            let rec = encode_record_enc(&stored, self.enc);
            self.w
                .index_insert(self.table.root, &rec, &self.order(&pk), pk.cols.len(), true)?;
        } else {
            let mut vals = row.to_vec();
            if let Some(ipk) = self.table.ipk {
                vals[ipk] = Value::Null;
            }
            // Trailing NULLs are still stored: SQLite keeps every column.
            let rec = encode_record_enc(&vals, self.enc);
            self.w.table_insert(self.table.root, rowid, &rec)?;
        }
        for ix in &self.table.indexes {
            self.add_index_entry(ix, row, rowid)?;
        }
        Ok(())
    }
}

fn order_cmp(order: &KeyOrder, a: &[Value], b: &[Value], n: usize) -> Ordering {
    for i in 0..n {
        let (Some(x), Some(y)) = (a.get(i), b.get(i)) else {
            return a.len().min(n).cmp(&b.len().min(n));
        };
        let (coll, desc) = order
            .fields
            .get(i)
            .copied()
            .unwrap_or((crate::value::Collation::Binary, false));
        let o = x.compare(y, coll);
        if o != Ordering::Equal {
            return if desc { o.reverse() } else { o };
        }
    }
    Ordering::Equal
}

/// A column's DEFAULT value (NULL without one).
fn default_value(db: &Database, c: &crate::schema::Column) -> Result<Value> {
    match &c.default {
        None => Ok(Value::Null),
        Some(e) => {
            let sel = select_of(vec![result(e.clone())], Vec::new(), None);
            let rs = db.run_select(&sel, true)?;
            Ok(rs
                .rows
                .into_iter()
                .next()
                .and_then(|r| r.into_iter().next())
                .unwrap_or(Value::Null))
        }
    }
}

fn result(expr: AExpr) -> ResultColumn {
    ResultColumn::Expr {
        expr,
        alias: None,
        text: String::new(),
    }
}

fn select_of(columns: Vec<ResultColumn>, from: Vec<FromItem>, where_: Option<AExpr>) -> Select {
    Select {
        with: Vec::new(),
        body: SelectBody::Core(Box::new(SelectCore {
            distinct: false,
            columns,
            from,
            where_,
            group_by: Vec::new(),
            having: None,
        })),
        order_by: Vec::new(),
        limit: None,
        offset: None,
    }
}

fn from_table(name: &str) -> Vec<FromItem> {
    vec![FromItem {
        join: JoinKind::Inner,
        natural: false,
        source: TableSource::Table {
            schema: None,
            name: name.to_string(),
        },
        alias: None,
        on: None,
        using: Vec::new(),
        indexed: Indexed::Any,
    }]
}

fn col(name: &str) -> AExpr {
    AExpr::Name {
        table: None,
        name: name.to_string(),
        dq: false,
    }
}

fn target_table(db: &Database, name: &str) -> Result<Rc<Table>> {
    let schema = db.schema();
    match schema.table(name) {
        Some(t) if t.name.eq_ignore_ascii_case("sqlite_schema") => {
            Err(Error::Sql(format!("table {name} may not be modified")))
        }
        Some(t) => Ok(t.clone()),
        None if schema.view(name).is_some() => Err(Error::Sql(format!(
            "cannot modify {name} because it is a view"
        ))),
        None => Err(Error::Sql(format!("no such table: {name}"))),
    }
}

/// A name for the rowid that no column of the table hides.
fn rowid_name(table: &Table) -> Option<&'static str> {
    ["rowid", "_rowid_", "oid"]
        .into_iter()
        .find(|n| table.column_index(n).is_none())
}

// ---------------------------------------------------------------- DML

fn insert(db: &Database, ins: &ast::Insert) -> Result<ResultSet> {
    if let Some(rs) = instead_of(
        db,
        &ins.table,
        Event::Insert,
        || {
            let rs = match &ins.source {
                InsertSource::Select(sel) => db.run_select(sel, true)?,
                InsertSource::DefaultValues => ResultSet {
                    rows: vec![Vec::new()],
                    ..Default::default()
                },
            };
            Ok(rs.rows.into_iter().map(|r| (None, Some(r))).collect())
        },
        &ins.columns,
    )? {
        return Ok(rs);
    }
    let table = target_table(db, &ins.table)?;
    let triggered = has_triggers(db, &table.name, Event::Insert);
    let ncols = table.columns.len();
    // Which table column each supplied value goes to (None: the rowid).
    let targets: Vec<Option<usize>> = if ins.columns.is_empty() {
        (0..ncols).map(Some).collect()
    } else {
        let mut t = Vec::new();
        for c in &ins.columns {
            match table.column_index(c) {
                Some(i) => t.push(Some(i)),
                None if !table.without_rowid
                    && ["rowid", "_rowid_", "oid"]
                        .iter()
                        .any(|r| r.eq_ignore_ascii_case(c)) =>
                {
                    t.push(None)
                }
                None => {
                    return Err(Error::Sql(format!(
                        "table {} has no column named {c}",
                        table.name
                    )))
                }
            }
        }
        t
    };
    let rows: Vec<Vec<Value>> = match &ins.source {
        InsertSource::DefaultValues => vec![Vec::new()],
        InsertSource::Select(sel) => {
            if let SelectBody::Values(vrows) = &sel.body {
                if let Some(bad) = vrows.iter().find(|r| r.len() != targets.len()) {
                    return Err(Error::Sql(if ins.columns.is_empty() {
                        format!(
                            "table {} has {} columns but {} values were supplied",
                            table.name,
                            ncols,
                            bad.len()
                        )
                    } else {
                        format!("{} values for {} columns", bad.len(), targets.len())
                    }));
                }
            }
            let rs = db.run_select(sel, true)?;
            if rs.columns.len() != targets.len() {
                return Err(Error::Sql(if ins.columns.is_empty() {
                    format!(
                        "table {} has {} columns but {} values were supplied",
                        table.name,
                        ncols,
                        rs.columns.len()
                    )
                } else {
                    format!("{} values for {} columns", rs.columns.len(), targets.len())
                }));
            }
            rs.rows
        }
    };
    // Defaults for the columns not supplied.
    let mut defaults = Vec::with_capacity(ncols);
    for (i, c) in table.columns.iter().enumerate() {
        let supplied = targets.contains(&Some(i));
        defaults.push(if supplied {
            Value::Null
        } else {
            default_value(db, c)?
        });
    }
    let mut ins_ = Inserter::new(db, &table, ins.on_conflict)?;
    let mut changes = 0;
    let mut last = None;
    for vals in rows {
        let mut row = defaults.clone();
        let mut given_rowid = None;
        for (v, t) in vals.into_iter().zip(&targets) {
            match t {
                Some(i) => row[*i] = v,
                None => match v.apply_affinity(Affinity::Integer) {
                    Value::Int(i) => given_rowid = Some(i),
                    Value::Null => {}
                    _ => return Err(Error::Constraint("datatype mismatch".into())),
                },
            }
        }
        if let (Some(r), Some(ipk)) = (given_rowid, table.ipk) {
            if row[ipk].is_null() {
                row[ipk] = Value::Int(r);
            }
        }
        if triggered {
            let r = given_rowid.or_else(|| {
                table.ipk.and_then(|i| match &row[i] {
                    Value::Int(v) => Some(*v),
                    _ => None,
                })
            });
            let before = trigger_row(&table, Some((r.unwrap_or(-1), &row)), None);
            fire(
                db,
                &table.name,
                TriggerTiming::Before,
                Event::Insert,
                &[],
                &before,
            )?;
        }
        if let Some((rowid, stored)) = ins_.insert_row_full(row, given_rowid)? {
            changes += 1;
            last = Some(rowid);
            if triggered {
                let after = trigger_row(&table, Some((rowid, &stored)), None);
                fire(
                    db,
                    &table.name,
                    TriggerTiming::After,
                    Event::Insert,
                    &[],
                    &after,
                )?;
            }
        }
    }
    if let Some(r) = last {
        if !table.without_rowid {
            let mut c = crate::func::counters();
            c.last_insert_rowid = r;
            crate::func::set_counters(c);
        }
    }
    Ok(ResultSet {
        changes,
        ..Default::default()
    })
}

/// A target row of an UPDATE or DELETE: rowid, old row, extra values.
type TargetRow = (i64, Vec<Value>, Vec<Value>);

fn target_rows(
    db: &Database,
    table: &Table,
    where_: &Option<AExpr>,
    extra: Vec<AExpr>,
) -> Result<Vec<TargetRow>> {
    let mut cols = Vec::new();
    let rowid = if table.without_rowid {
        None
    } else {
        let name = rowid_name(table).ok_or_else(|| {
            Error::Unsupported(format!(
                "table {} has columns named rowid, _rowid_ and oid",
                table.name
            ))
        })?;
        cols.push(result(col(name)));
        Some(())
    };
    for c in &table.columns {
        cols.push(result(AExpr::Name {
            table: Some(table.name.clone()),
            name: c.name.clone(),
            dq: false,
        }));
    }
    let n_extra = extra.len();
    cols.extend(extra.into_iter().map(result));
    let sel = select_of(cols, from_table(&table.name), where_.clone());
    let rs = db.run_select(&sel, true)?;
    let n = table.columns.len();
    Ok(rs
        .rows
        .into_iter()
        .map(|mut r| {
            let id = if rowid.is_some() {
                r.remove(0).to_int()
            } else {
                0
            };
            let ex = r.split_off(n);
            debug_assert_eq!(ex.len(), n_extra);
            (id, r, ex)
        })
        .collect())
}

fn delete(db: &Database, del: &ast::Delete) -> Result<ResultSet> {
    if let Some(rs) = instead_of(
        db,
        &del.table,
        Event::Delete,
        || {
            let rows = view_rows(db, &del.table, &del.where_, Vec::new())?;
            Ok(rows.into_iter().map(|(old, _)| (Some(old), None)).collect())
        },
        &[],
    )? {
        return Ok(rs);
    }
    let table = target_table(db, &del.table)?;
    let ins = Inserter::new(db, &table, OnConflict::Abort)?;
    let w = Writer::new(&db.pager);
    let triggered = has_triggers(db, &table.name, Event::Delete);
    if del.where_.is_none() && !triggered {
        // DELETE without WHERE empties the b-trees directly.
        let n = crate::btree::count_entries(&db.pager, table.root)?;
        w.clear_tree(table.root)?;
        for ix in &table.indexes {
            w.clear_tree(ix.root)?;
        }
        return Ok(ResultSet {
            changes: n,
            ..Default::default()
        });
    }
    let rows = target_rows(db, &table, &del.where_, Vec::new())?;
    for (rowid, row, _) in &rows {
        if triggered {
            let tr = trigger_row(&table, None, Some((*rowid, row)));
            fire(
                db,
                &table.name,
                TriggerTiming::Before,
                Event::Delete,
                &[],
                &tr,
            )?;
            // A BEFORE trigger may already have removed or changed the row.
            if !table.without_rowid {
                match ins.row_at(*rowid)? {
                    Some(now) => {
                        ins.delete_row(*rowid, &now)?;
                    }
                    None => continue,
                }
            } else {
                ins.delete_row(*rowid, row)?;
            }
            fire(
                db,
                &table.name,
                TriggerTiming::After,
                Event::Delete,
                &[],
                &tr,
            )?;
        } else {
            ins.delete_row(*rowid, row)?;
        }
    }
    Ok(ResultSet {
        changes: rows.len() as u64,
        ..Default::default()
    })
}

fn update(db: &Database, up: &ast::Update) -> Result<ResultSet> {
    let set_names: Vec<String> = up.sets.iter().map(|(n, _)| n.clone()).collect();
    if let Some(rs) = instead_of(
        db,
        &up.table,
        Event::Update,
        || {
            let exprs: Vec<AExpr> = up.sets.iter().map(|(_, e)| e.clone()).collect();
            let rows = view_rows(db, &up.table, &up.where_, exprs)?;
            let cols = view_columns(db, &up.table)?;
            let mut out = Vec::new();
            for (old, newvals) in rows {
                let mut new = old.clone();
                for ((name, _), v) in up.sets.iter().zip(newvals) {
                    match cols.iter().position(|c| c.eq_ignore_ascii_case(name)) {
                        Some(i) => new[i] = v,
                        None => return Err(Error::Sql(format!("no such column: {name}"))),
                    }
                }
                out.push((Some(old), Some(new)));
            }
            Ok(out)
        },
        &set_names,
    )? {
        return Ok(rs);
    }
    let table = target_table(db, &up.table)?;
    let triggered = has_triggers(db, &table.name, Event::Update);
    let mut sets: Vec<(Option<usize>, AExpr)> = Vec::new();
    for (name, e) in &up.sets {
        match table.column_index(name) {
            Some(i) => sets.push((Some(i), e.clone())),
            None if !table.without_rowid
                && ["rowid", "_rowid_", "oid"]
                    .iter()
                    .any(|r| r.eq_ignore_ascii_case(name)) =>
            {
                sets.push((None, e.clone()))
            }
            None => return Err(Error::Sql(format!("no such column: {name}"))),
        }
    }
    let rows = target_rows(
        db,
        &table,
        &up.where_,
        sets.iter().map(|(_, e)| e.clone()).collect(),
    )?;
    let ins = Inserter::new(db, &table, up.on_conflict)?;
    let mut changes = 0;
    for (rowid, old, newvals) in rows {
        let mut row = old.clone();
        let mut new_rowid = rowid;
        for ((target, _), v) in sets.iter().zip(newvals) {
            match target {
                Some(i) => row[*i] = v,
                None => match v.apply_affinity(Affinity::Integer) {
                    Value::Int(r) => new_rowid = r,
                    _ => return Err(Error::Constraint("datatype mismatch".into())),
                },
            }
        }
        if let Some(ipk) = table.ipk {
            if sets.iter().any(|(t, _)| *t == Some(ipk)) {
                match row[ipk].clone().apply_affinity(Affinity::Integer) {
                    Value::Int(r) => new_rowid = r,
                    Value::Null => new_rowid = rowid,
                    _ => return Err(Error::Constraint("datatype mismatch".into())),
                }
            } else if new_rowid != rowid {
                row[ipk] = Value::Int(new_rowid);
            }
        }
        let Some(mut row) = ins.prepare(row)? else {
            continue;
        };
        if let Some(ipk) = table.ipk {
            row[ipk] = Value::Int(new_rowid);
        }
        // The row may have been removed by an earlier REPLACE.
        let still_there = if table.without_rowid {
            true
        } else {
            ins.row_at(rowid)?.is_some()
        };
        if !still_there {
            continue;
        }
        if triggered {
            let tr = trigger_row(&table, Some((new_rowid, &row)), Some((rowid, &old)));
            fire(
                db,
                &table.name,
                TriggerTiming::Before,
                Event::Update,
                &set_names,
                &tr,
            )?;
        }
        // Take the old row out first so it never conflicts with itself.
        ins.delete_row(rowid, &old)?;
        if !ins.resolve_conflicts(&row, new_rowid)? {
            // OR IGNORE: put the old row back unchanged.
            ins.store(&old_with_rowid(&table, old, rowid), rowid)?;
            continue;
        }
        ins.store(&row, new_rowid)?;
        changes += 1;
        if triggered {
            let tr = trigger_row(&table, Some((new_rowid, &row)), Some((rowid, &old)));
            fire(
                db,
                &table.name,
                TriggerTiming::After,
                Event::Update,
                &set_names,
                &tr,
            )?;
        }
    }
    Ok(ResultSet {
        changes,
        ..Default::default()
    })
}

fn old_with_rowid(table: &Table, mut row: Vec<Value>, rowid: i64) -> Vec<Value> {
    if let Some(ipk) = table.ipk {
        row[ipk] = Value::Int(rowid);
    }
    row
}

// ---------------------------------------------------------------- views

/// The OLD and NEW rows of one change to a view.
type OldNew = (Option<Vec<Value>>, Option<Vec<Value>>);

fn view_columns(db: &Database, view: &str) -> Result<Vec<String>> {
    let sel = select_of(vec![ResultColumn::Star], from_table(view), None);
    Ok(db.run_select(&sel, false)?.columns)
}

/// Rows of a view matching WHERE, with extra expressions evaluated per row.
fn view_rows(
    db: &Database,
    view: &str,
    where_: &Option<AExpr>,
    extra: Vec<AExpr>,
) -> Result<Vec<(Vec<Value>, Vec<Value>)>> {
    let n = view_columns(db, view)?.len();
    let mut cols = vec![ResultColumn::Star];
    cols.extend(extra.into_iter().map(result));
    let sel = select_of(cols, from_table(view), where_.clone());
    Ok(db
        .run_select(&sel, true)?
        .rows
        .into_iter()
        .map(|mut r| {
            let ex = r.split_off(n);
            (r, ex)
        })
        .collect())
}

/// A write to a view runs its INSTEAD OF triggers, once per row; `rows`
/// produces (OLD, NEW) for each. Returns None when `name` is not a view.
fn instead_of(
    db: &Database,
    name: &str,
    event: Event,
    rows: impl FnOnce() -> Result<Vec<OldNew>>,
    columns: &[String],
) -> Result<Option<ResultSet>> {
    let schema = db.schema();
    if schema.table(name).is_some() || schema.view(name).is_none() {
        return Ok(None);
    }
    let set_cols: Vec<String> = if event == Event::Update {
        columns.to_vec()
    } else {
        Vec::new()
    };
    if triggers_for(db, name, TriggerTiming::InsteadOf, event, &set_cols).is_empty() {
        return Err(Error::Sql(format!(
            "cannot modify {name} because it is a view"
        )));
    }
    let cols = view_columns(db, name)?;
    let mut n = 0;
    for (old, new) in rows()? {
        // INSERT with a column list fills the other columns with NULL.
        let new = match (event, new) {
            (Event::Insert, Some(vals)) if !columns.is_empty() => {
                let mut full = vec![Value::Null; cols.len()];
                for (c, v) in columns.iter().zip(vals) {
                    match cols.iter().position(|x| x.eq_ignore_ascii_case(c)) {
                        Some(i) => full[i] = v,
                        None => {
                            return Err(Error::Sql(format!("table {name} has no column named {c}")))
                        }
                    }
                }
                Some(full)
            }
            (_, v) => v,
        };
        let row = crate::bind::TriggerRow {
            columns: cols.clone(),
            new: new.map(|v| (0, v)),
            old: old.map(|v| (0, v)),
        };
        fire(db, name, TriggerTiming::InsteadOf, event, &set_cols, &row)?;
        n += 1;
    }
    Ok(Some(ResultSet {
        changes: n,
        ..Default::default()
    }))
}
