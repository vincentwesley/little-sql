//! The pragmas that only describe the schema (`table_info`, `index_list`,
//! ...). They work from the merged schema alone, so the same code answers
//! `PRAGMA x(arg)` and the table-valued `pragma_x(arg)` in a query.

use crate::error::Result;
use crate::schema::{Index, Schema, Table};
use crate::sql::ast::{self, Stmt, TableConstraint};
use crate::value::Value;
use std::rc::Rc;

/// Result columns of each schema pragma, and whether it takes an argument.
pub const PRAGMAS: &[(&str, &[&str], bool)] = &[
    (
        "table_info",
        &["cid", "name", "type", "notnull", "dflt_value", "pk"],
        true,
    ),
    (
        "table_xinfo",
        &[
            "cid",
            "name",
            "type",
            "notnull",
            "dflt_value",
            "pk",
            "hidden",
        ],
        true,
    ),
    (
        "index_list",
        &["seq", "name", "unique", "origin", "partial"],
        true,
    ),
    ("index_info", &["seqno", "cid", "name"], true),
    (
        "index_xinfo",
        &["seqno", "cid", "name", "desc", "coll", "key"],
        true,
    ),
    ("database_list", &["seq", "name", "file"], false),
];

/// The columns of schema pragma `name`, if it is one.
pub fn columns(name: &str) -> Option<(&'static [&'static str], bool)> {
    PRAGMAS
        .iter()
        .find(|(n, _, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, c, a)| (*c, *a))
}

/// A column's type as `PRAGMA table_info` shows it: SQLite spells the six
/// STRICT type names in upper case and leaves every other type as written.
fn info_type(t: &str) -> String {
    let u = t.to_ascii_uppercase();
    match u.as_str() {
        "ANY" | "BLOB" | "INT" | "INTEGER" | "REAL" | "TEXT" => u,
        _ => t.to_string(),
    }
}

/// The rows of schema pragma `name` for `arg` in database `db` (None: the
/// usual search order). None when `name` is not a schema pragma.
pub fn rows(
    schema: &Schema,
    db: Option<&str>,
    name: &str,
    arg: Option<&str>,
) -> Option<Result<Vec<Vec<Value>>>> {
    let name = name.to_ascii_lowercase();
    columns(&name)?;
    Some(run(schema, db, &name, arg))
}

fn run(
    schema: &Schema,
    db: Option<&str>,
    name: &str,
    arg: Option<&str>,
) -> Result<Vec<Vec<Value>>> {
    let mut rows = Vec::new();
    match name {
        "database_list" => {
            for (i, (n, s)) in schema.dbs.iter().enumerate() {
                // temp is listed once it holds something.
                let pager = s.pager.as_ref();
                if n == "temp" && pager.is_none_or(|p| p.page_count() == 0) {
                    continue;
                }
                let path = schema.paths.get(i).cloned().unwrap_or_default();
                rows.push(vec![
                    Value::Int(i as i64),
                    Value::text(n),
                    Value::text(path),
                ]);
            }
        }
        "table_info" | "table_xinfo" => {
            let Some(t) = arg.map(|a| schema.table_in(db, a)).transpose()?.flatten() else {
                return Ok(rows);
            };
            let x = name == "table_xinfo";
            let mut hidden_cols = 0;
            for (i, c) in t.columns.iter().enumerate() {
                let hidden = match &c.generated {
                    None => 0,
                    Some(g) if g.stored => 3,
                    Some(_) => 2,
                };
                if hidden != 0 && !x {
                    hidden_cols += 1;
                    continue;
                }
                let pk = if t.ipk == Some(i) {
                    1
                } else {
                    pk_position(&t, i)
                };
                let mut row = vec![
                    Value::Int((i - hidden_cols) as i64),
                    Value::text(&c.name),
                    Value::text(info_type(&c.decl_type)),
                    Value::Int(c.not_null as i64),
                    c.default_text.as_ref().map_or(Value::Null, Value::text),
                    Value::Int(pk),
                ];
                if x {
                    row.push(Value::Int(hidden));
                }
                rows.push(row);
            }
        }
        "index_list" => {
            let Some(t) = arg.map(|a| schema.table_in(db, a)).transpose()?.flatten() else {
                return Ok(rows);
            };
            let pk = pk_columns(&t);
            // SQLite lists the newest index first.
            let mut list: Vec<Rc<Index>> = t.indexes.clone();
            if let Some(p) = &t.pk_index {
                list.push(p.clone());
            }
            let order = |ix: &Index| {
                file_schema(schema, &t)
                    .and_then(|s| {
                        s.rows
                            .iter()
                            .position(|r| r.kind == "index" && r.root == ix.root)
                    })
                    .unwrap_or(0)
            };
            list.sort_by_key(|ix| std::cmp::Reverse(order(ix)));
            for (i, ix) in list.iter().enumerate() {
                let origin = if ix.sql.is_some() {
                    "c"
                } else if ix.primary || same_columns(ix, &pk) {
                    "pk"
                } else {
                    "u"
                };
                rows.push(vec![
                    Value::Int(i as i64),
                    Value::text(index_name(schema, &t, ix)),
                    Value::Int(ix.unique as i64),
                    Value::text(origin),
                    Value::Int(ix.where_.is_some() as i64),
                ]);
            }
        }
        "index_info" | "index_xinfo" => {
            let Some(a) = arg else { return Ok(rows) };
            let found = match db {
                Some(d) => schema.db(d).and_then(|s| s.index(a)),
                None => schema.index(a),
            };
            let (t, ix) = match found {
                Some(f) => f,
                // A WITHOUT ROWID table, or its primary key's autoindex
                // name, stands for its primary key.
                None => match pk_of(schema, db, a)? {
                    Some(f) => f,
                    None => return Ok(rows),
                },
            };
            let x = name == "index_xinfo";
            let colls = written_collations(&ix);
            let col_name = |c: usize| match t.columns.get(c) {
                Some(col) => Value::text(&col.name),
                None => Value::Null,
            };
            for (k, c) in ix.cols.iter().enumerate() {
                let is_expr = ix.exprs.get(k).is_some_and(Option::is_some);
                let cid = if is_expr { -2 } else { c.col as i64 };
                let name = if is_expr {
                    Value::Null
                } else {
                    col_name(c.col)
                };
                let mut row = vec![Value::Int(k as i64), Value::Int(cid), name];
                if x {
                    let coll = colls
                        .get(k)
                        .cloned()
                        .flatten()
                        .unwrap_or_else(|| c.collation.name().to_string());
                    row.extend([Value::Int(c.desc as i64), Value::text(coll), Value::Int(1)]);
                }
                rows.push(row);
            }
            if x {
                let n = ix.cols.len();
                if t.without_rowid {
                    for (j, c) in ix.extra.iter().enumerate() {
                        rows.push(vec![
                            Value::Int((n + j) as i64),
                            Value::Int(*c as i64),
                            col_name(*c),
                            Value::Int(0),
                            Value::text(t.columns[*c].collation.name()),
                            Value::Int(0),
                        ]);
                    }
                } else {
                    rows.push(vec![
                        Value::Int(n as i64),
                        Value::Int(-1),
                        Value::Null,
                        Value::Int(0),
                        Value::text("BINARY"),
                        Value::Int(0),
                    ]);
                }
            }
        }
        _ => {}
    }
    Ok(rows)
}

/// The primary key of a WITHOUT ROWID table named `a`, or whose
/// automatic index is named `a`.
fn pk_of(schema: &Schema, db: Option<&str>, a: &str) -> Result<Option<(Rc<Table>, Rc<Index>)>> {
    if let Some(t) = schema.table_in(db, a)?.filter(|t| t.without_rowid) {
        let p = t.pk_index.clone().unwrap();
        return Ok(Some((t, p)));
    }
    let files: Vec<&Rc<Schema>> = match db {
        Some(d) => schema.db(d).into_iter().collect(),
        None => schema.dbs.iter().map(|(_, s)| s).collect(),
    };
    for f in files {
        for t in f.tables.values().filter(|t| t.without_rowid) {
            if index_name(schema, t, t.pk_index.as_ref().unwrap()).eq_ignore_ascii_case(a) {
                return Ok(Some((t.clone(), t.pk_index.clone().unwrap())));
            }
        }
    }
    Ok(None)
}

/// The schema of the file `t` lives in.
fn file_schema<'a>(schema: &'a Schema, t: &Table) -> Option<&'a Rc<Schema>> {
    schema
        .dbs
        .iter()
        .map(|(_, s)| s)
        .find(|s| match (&s.pager, &t.pager) {
            (Some(a), Some(b)) => Rc::ptr_eq(a, b),
            _ => false,
        })
}

/// The name an index has in sqlite_schema (the primary key of a WITHOUT
/// ROWID table has no `Index::name` of its own).
fn index_name(schema: &Schema, t: &Table, ix: &Index) -> String {
    if !ix.primary {
        return ix.name.clone();
    }
    file_schema(schema, t)
        .and_then(|s| {
            s.rows
                .iter()
                .find(|r| r.kind == "index" && r.root == ix.root)
                .map(|r| r.name.clone())
        })
        .unwrap_or_else(|| format!("sqlite_autoindex_{}_1", t.name))
}

/// The declared PRIMARY KEY columns of a table, from its CREATE TABLE.
fn pk_columns(t: &Table) -> Vec<usize> {
    if !t.pk.is_empty() {
        return t.pk.iter().map(|k| k.col).collect();
    }
    let Ok(Stmt::CreateTable(ct)) = crate::sql::parser::parse_one(&t.sql) else {
        return Vec::new();
    };
    let mut out: Vec<usize> = ct
        .columns
        .iter()
        .enumerate()
        .filter(|(_, c)| c.primary_key)
        .map(|(i, _)| i)
        .collect();
    for tc in &ct.constraints {
        if let TableConstraint::PrimaryKey(cols) = tc {
            out.extend(
                cols.iter()
                    .filter_map(|c| c.name.as_ref().and_then(|n| t.column_index(n))),
            );
        }
    }
    out
}

/// 1-based position of column `i` in the declared primary key, or 0.
fn pk_position(t: &Table, i: usize) -> i64 {
    pk_columns(t)
        .iter()
        .position(|c| *c == i)
        .map_or(0, |p| p as i64 + 1)
}

fn same_columns(ix: &Index, cols: &[usize]) -> bool {
    !cols.is_empty()
        && ix.cols.len() == cols.len()
        && ix.cols.iter().zip(cols).all(|(a, b)| a.col == *b)
}

/// Collation names as the CREATE INDEX spells them, per key column.
fn written_collations(ix: &Index) -> Vec<Option<String>> {
    let Some(sql) = &ix.sql else {
        return Vec::new();
    };
    match crate::sql::parser::parse_one(sql) {
        Ok(Stmt::CreateIndex(ci)) => ci
            .columns
            .iter()
            .map(|c: &ast::IndexedColumn| c.collation.clone())
            .collect(),
        _ => Vec::new(),
    }
}

thread_local! {
    static CURRENT: std::cell::RefCell<Vec<Rc<Schema>>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Run `f` with `schema` as the one `pragma_*` table functions describe.
pub fn with_schema<T>(schema: Rc<Schema>, f: impl FnOnce() -> T) -> T {
    CURRENT.with(|c| c.borrow_mut().push(schema));
    let out = f();
    CURRENT.with(|c| c.borrow_mut().pop());
    out
}

/// The rows of `pragma_<name>(arg, schema)` in a query: the pragma's own
/// columns, then the hidden `arg` and `schema` for pragmas that take one.
pub fn table_rows(name: &str, args: &[Value]) -> Result<Vec<Vec<Value>>> {
    let Some(schema) = CURRENT.with(|c| c.borrow().last().cloned()) else {
        return Ok(Vec::new());
    };
    let (_, takes_arg) = columns(name).expect("a known pragma");
    let arg = args.first().filter(|v| !v.is_null()).map(Value::to_text);
    let db = args.get(1).filter(|v| !v.is_null()).map(Value::to_text);
    if let Some(d) = &db {
        if schema.db(d).is_none() {
            return Err(crate::error::Error::Runtime(format!(
                "unknown database '{d}'"
            )));
        }
    }
    if takes_arg && arg.is_none() {
        return Ok(Vec::new());
    }
    let mut rows = run(&schema, db.as_deref(), name, arg.as_deref())?;
    if takes_arg {
        for r in rows.iter_mut() {
            r.push(args.first().cloned().unwrap_or(Value::Null));
            r.push(args.get(1).cloned().unwrap_or(Value::Null));
        }
    }
    Ok(rows)
}
