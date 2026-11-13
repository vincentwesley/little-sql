//! The schema: what tables, indexes and views a database contains.
//!
//! SQLite stores its schema as rows of the `sqlite_schema` table (rooted at
//! page 1). Each row carries the original `CREATE` statement, so recovering
//! the column layout means parsing that SQL again.

use crate::btree::{estimate_rows, Cursor};
use crate::error::{Error, Result};
use crate::pager::Pager;
use crate::record::decode_all;
use crate::sql::ast::{self, CreateTable, Stmt, TableConstraint};
use crate::sql::parser::parse_one;
use crate::value::{Affinity, Collation, Value};
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Debug, Clone)]
pub struct Column {
    pub name: String,
    pub decl_type: String,
    pub affinity: Affinity,
    pub collation: Collation,
    pub not_null: bool,
    /// The DEFAULT expression, if any.
    pub default: Option<ast::Expr>,
    /// The DEFAULT as a constant, used for rows written before the column
    /// was added by `ALTER TABLE ADD COLUMN`.
    pub default_value: Value,
}

/// One key column of an index (or of a WITHOUT ROWID primary key).
#[derive(Debug, Clone)]
pub struct IndexCol {
    /// Position in the table's column list.
    pub col: usize,
    pub collation: Collation,
    pub desc: bool,
}

#[derive(Debug)]
pub struct Index {
    pub name: String,
    pub table: String,
    pub root: u32,
    pub cols: Vec<IndexCol>,
    pub unique: bool,
    /// False for partial and expression indexes, which the planner ignores
    /// (and which make their table read-only for this engine).
    pub usable: bool,
    pub sql: Option<String>,
}

#[derive(Debug)]
pub struct Table {
    pub name: String,
    pub root: u32,
    pub columns: Vec<Column>,
    /// The column that aliases the rowid (`INTEGER PRIMARY KEY`).
    pub ipk: Option<usize>,
    pub without_rowid: bool,
    /// For WITHOUT ROWID tables: the order in which columns are stored in
    /// the b-tree record (primary key columns first).
    pub storage_order: Vec<usize>,
    /// Primary key columns of a WITHOUT ROWID table.
    pub pk: Vec<IndexCol>,
    pub indexes: Vec<Rc<Index>>,
    pub autoincrement: bool,
    pub sql: String,
    row_estimate: Cell<Option<u64>>,
}

impl Table {
    pub fn column_index(&self, name: &str) -> Option<usize> {
        self.columns
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(name))
    }

    /// Estimated number of rows, computed on first use.
    pub fn row_estimate(&self, pager: &Pager) -> u64 {
        if let Some(n) = self.row_estimate.get() {
            return n;
        }
        let n = estimate_rows(pager, self.root);
        self.row_estimate.set(Some(n));
        n
    }
}

#[derive(Debug)]
pub struct View {
    pub name: String,
    pub columns: Vec<String>,
    pub select: ast::Select,
    pub sql: String,
}

/// One row of `sqlite_schema`.
#[derive(Debug, Clone)]
pub struct SchemaRow {
    pub rowid: i64,
    pub kind: String,
    pub name: String,
    pub tbl_name: String,
    pub root: u32,
    pub sql: Option<String>,
}

#[derive(Default)]
pub struct Schema {
    pub tables: HashMap<String, Rc<Table>>,
    pub views: HashMap<String, Rc<View>>,
    /// Index name (lower case) to the lower-case name of its table.
    pub index_tables: HashMap<String, String>,
    /// Objects whose SQL this engine cannot handle, with the reason.
    pub broken: HashMap<String, String>,
    pub rows: Vec<SchemaRow>,
}

fn lower(s: &str) -> String {
    s.to_ascii_lowercase()
}

fn collation_of(name: &Option<String>) -> Result<Collation> {
    match name {
        None => Ok(Collation::Binary),
        Some(n) => {
            Collation::from_name(n).ok_or_else(|| Error::Unsupported(format!("collation {n}")))
        }
    }
}

fn const_default(e: &ast::Expr) -> Value {
    match e {
        ast::Expr::Literal(v) => v.clone(),
        ast::Expr::Unary(ast::UnaryOp::Pos, inner) => const_default(inner),
        ast::Expr::Name { name, .. } => Value::text(name),
        _ => Value::Null,
    }
}

/// The key columns of every UNIQUE / PRIMARY KEY constraint that SQLite
/// backs with an automatic index, in the order SQLite numbers them.
fn auto_index_keys(ct: &CreateTable, table: &Table) -> Result<Vec<Vec<IndexCol>>> {
    let mut keys: Vec<Vec<IndexCol>> = Vec::new();
    let add = |cols: Vec<IndexCol>, keys: &mut Vec<Vec<IndexCol>>| {
        let dup = keys.iter().any(|k| {
            k.len() == cols.len()
                && k.iter()
                    .zip(&cols)
                    .all(|(a, b)| a.col == b.col && a.collation == b.collation)
        });
        if !dup {
            keys.push(cols);
        }
    };
    let resolve = |cols: &[ast::IndexedColumn]| -> Result<Vec<IndexCol>> {
        cols.iter()
            .map(|c| {
                let name = c
                    .name
                    .as_ref()
                    .ok_or_else(|| Error::Unsupported("expression in key".into()))?;
                let col = table
                    .column_index(name)
                    .ok_or_else(|| Error::Sql(format!("no such column: {name}")))?;
                let collation = match &c.collation {
                    Some(n) => collation_of(&Some(n.clone()))?,
                    None => table.columns[col].collation,
                };
                Ok(IndexCol {
                    col,
                    collation,
                    desc: c.desc,
                })
            })
            .collect()
    };
    for (i, c) in ct.columns.iter().enumerate() {
        let single = || {
            vec![IndexCol {
                col: i,
                collation: table.columns[i].collation,
                desc: false,
            }]
        };
        if c.primary_key && table.ipk != Some(i) && !ct.without_rowid {
            add(single(), &mut keys);
        }
        if c.unique {
            add(single(), &mut keys);
        }
    }
    for tc in &ct.constraints {
        match tc {
            TableConstraint::PrimaryKey(cols) => {
                let k = resolve(cols)?;
                let is_ipk = k.len() == 1 && table.ipk == Some(k[0].col);
                if !is_ipk && !ct.without_rowid {
                    add(k, &mut keys);
                }
            }
            TableConstraint::Unique(cols) => add(resolve(cols)?, &mut keys),
            TableConstraint::Other => {}
        }
    }
    Ok(keys)
}

/// Build a table definition from its parsed `CREATE TABLE`.
pub fn table_from_ast(ct: &CreateTable, name: &str, root: u32, sql: &str) -> Result<Table> {
    let mut columns = Vec::new();
    for c in &ct.columns {
        if c.generated {
            return Err(Error::Unsupported("generated columns".into()));
        }
        columns.push(Column {
            name: c.name.clone(),
            decl_type: c.type_name.clone(),
            affinity: Affinity::from_decl_type(&c.type_name),
            collation: collation_of(&c.collation)?,
            not_null: c.not_null,
            default_value: c.default.as_ref().map(const_default).unwrap_or(Value::Null),
            default: c.default.clone(),
        });
    }
    let mut table = Table {
        name: name.to_string(),
        root,
        columns,
        ipk: None,
        without_rowid: ct.without_rowid,
        storage_order: Vec::new(),
        pk: Vec::new(),
        indexes: Vec::new(),
        autoincrement: ct.columns.iter().any(|c| c.autoincrement),
        sql: sql.to_string(),
        row_estimate: Cell::new(None),
    };
    // Primary key columns, from a column constraint or a table constraint.
    let mut pk_cols: Vec<IndexCol> = Vec::new();
    let mut pk_desc = false;
    for (i, c) in ct.columns.iter().enumerate() {
        if c.primary_key {
            pk_cols.push(IndexCol {
                col: i,
                collation: table.columns[i].collation,
                desc: c.pk_desc,
            });
            pk_desc = c.pk_desc;
        }
    }
    for tc in &ct.constraints {
        if let TableConstraint::PrimaryKey(cols) = tc {
            for c in cols {
                let name = c
                    .name
                    .as_ref()
                    .ok_or_else(|| Error::Unsupported("expression in primary key".into()))?;
                let col = table
                    .column_index(name)
                    .ok_or_else(|| Error::Sql(format!("no such column: {name}")))?;
                let collation = match &c.collation {
                    Some(n) => collation_of(&Some(n.clone()))?,
                    None => table.columns[col].collation,
                };
                pk_cols.push(IndexCol {
                    col,
                    collation,
                    desc: c.desc,
                });
            }
        }
    }
    if ct.without_rowid {
        if pk_cols.is_empty() {
            return Err(Error::Sql(format!("PRIMARY KEY missing on table {name}")));
        }
        let mut order: Vec<usize> = Vec::new();
        let mut pk: Vec<IndexCol> = Vec::new();
        for k in pk_cols {
            if !order.contains(&k.col) {
                order.push(k.col);
                pk.push(k);
            }
        }
        for i in 0..table.columns.len() {
            if !order.contains(&i) {
                order.push(i);
            }
        }
        table.storage_order = order;
        table.pk = pk;
    } else if pk_cols.len() == 1 {
        let c = pk_cols[0].col;
        // The rowid alias rule: declared type exactly INTEGER. (A column
        // constraint written "INTEGER PRIMARY KEY DESC" is a historical
        // exception and does not alias the rowid.)
        if table.columns[c].decl_type.eq_ignore_ascii_case("INTEGER") && !pk_desc {
            table.ipk = Some(c);
        }
    }
    Ok(table)
}

impl Schema {
    /// Read and parse `sqlite_schema`.
    pub fn load(pager: &Rc<Pager>) -> Result<Schema> {
        let mut schema = Schema::default();
        if pager.page_count() == 0 {
            schema.add_schema_table();
            return Ok(schema);
        }
        let mut cur = Cursor::new(pager.clone(), 1);
        cur.first()?;
        while cur.valid() {
            let payload = cur.payload()?;
            let vals = decode_all(payload.as_slice(), pager.encoding())?;
            let text = |i: usize| match vals.get(i) {
                Some(Value::Text(s)) => Some(s.to_string()),
                _ => None,
            };
            schema.rows.push(SchemaRow {
                rowid: cur.rowid()?,
                kind: text(0).unwrap_or_default(),
                name: text(1).unwrap_or_default(),
                tbl_name: text(2).unwrap_or_default(),
                root: match vals.get(3) {
                    Some(Value::Int(i)) => *i as u32,
                    _ => 0,
                },
                sql: text(4),
            });
            cur.next()?;
        }
        schema.build();
        Ok(schema)
    }

    fn add_schema_table(&mut self) {
        let sql = "CREATE TABLE sqlite_schema(type text, name text, tbl_name text, rootpage integer, sql text)";
        if let Ok(Stmt::CreateTable(ct)) = parse_one(sql) {
            if let Ok(t) = table_from_ast(&ct, "sqlite_schema", 1, sql) {
                let t = Rc::new(t);
                self.tables.insert("sqlite_schema".into(), t.clone());
                self.tables.insert("sqlite_master".into(), t);
            }
        }
    }

    /// Turn the raw rows into table, index and view definitions.
    fn build(&mut self) {
        self.add_schema_table();
        let rows = self.rows.clone();
        let mut tables: HashMap<String, (Table, Option<CreateTable>)> = HashMap::new();
        for r in rows.iter().filter(|r| r.kind == "table") {
            let Some(sql) = &r.sql else { continue };
            let parsed = parse_one(sql).and_then(|s| match s {
                Stmt::CreateTable(ct) => {
                    table_from_ast(&ct, &r.name, r.root, sql).map(|t| (t, Some(ct)))
                }
                _ => Err(Error::Unsupported("virtual tables".into())),
            });
            match parsed {
                Ok(t) => {
                    tables.insert(lower(&r.name), t);
                }
                Err(e) => {
                    self.broken.insert(lower(&r.name), e.to_string());
                }
            }
        }
        // Explicit indexes carry their own SQL; automatic ones are matched
        // to the table's constraints by their sequence number.
        let mut auto: HashMap<String, Vec<(u32, &SchemaRow)>> = HashMap::new();
        for r in rows.iter().filter(|r| r.kind == "index") {
            let tkey = lower(&r.tbl_name);
            let Some((table, _)) = tables.get_mut(&tkey) else {
                continue;
            };
            self.index_tables.insert(lower(&r.name), tkey.clone());
            match &r.sql {
                None => {
                    let n = r
                        .name
                        .rsplit('_')
                        .next()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);
                    auto.entry(tkey).or_default().push((n, r));
                }
                Some(sql) => {
                    let idx = match parse_one(sql) {
                        Ok(Stmt::CreateIndex(ci)) => {
                            let mut cols = Vec::new();
                            let mut usable = !ci.partial && !table.without_rowid;
                            for c in &ci.columns {
                                let col = c.name.as_ref().and_then(|n| table.column_index(n));
                                let coll = c.collation.as_ref().map(|n| Collation::from_name(n));
                                match (col, coll) {
                                    (Some(col), None) => cols.push(IndexCol {
                                        col,
                                        collation: table.columns[col].collation,
                                        desc: c.desc,
                                    }),
                                    (Some(col), Some(Some(collation))) => cols.push(IndexCol {
                                        col,
                                        collation,
                                        desc: c.desc,
                                    }),
                                    _ => usable = false,
                                }
                            }
                            Index {
                                name: r.name.clone(),
                                table: table.name.clone(),
                                root: r.root,
                                cols,
                                unique: ci.unique,
                                usable,
                                sql: Some(sql.clone()),
                            }
                        }
                        _ => Index {
                            name: r.name.clone(),
                            table: table.name.clone(),
                            root: r.root,
                            cols: Vec::new(),
                            unique: false,
                            usable: false,
                            sql: Some(sql.clone()),
                        },
                    };
                    table.indexes.push(Rc::new(idx));
                }
            }
        }
        for (tkey, mut list) in auto {
            list.sort_by_key(|(n, _)| *n);
            let (table, ct) = tables.get_mut(&tkey).unwrap();
            let keys = ct
                .as_ref()
                .and_then(|ct| auto_index_keys(ct, table).ok())
                .unwrap_or_default();
            let matched = keys.len() == list.len();
            for (i, (_, r)) in list.iter().enumerate() {
                table.indexes.push(Rc::new(Index {
                    name: r.name.clone(),
                    table: table.name.clone(),
                    root: r.root,
                    cols: if matched { keys[i].clone() } else { Vec::new() },
                    unique: true,
                    usable: matched && !table.without_rowid,
                    sql: None,
                }));
            }
        }
        for (k, (t, _)) in tables {
            self.tables.insert(k, Rc::new(t));
        }
        for r in rows.iter().filter(|r| r.kind == "view") {
            let Some(sql) = &r.sql else { continue };
            match parse_one(sql) {
                Ok(Stmt::CreateView(cv)) => {
                    self.views.insert(
                        lower(&r.name),
                        Rc::new(View {
                            name: r.name.clone(),
                            columns: cv.columns,
                            select: cv.select,
                            sql: sql.clone(),
                        }),
                    );
                }
                Ok(_) => {
                    self.broken
                        .insert(lower(&r.name), "not a view definition".into());
                }
                Err(e) => {
                    self.broken.insert(lower(&r.name), e.to_string());
                }
            }
        }
    }

    pub fn table(&self, name: &str) -> Option<&Rc<Table>> {
        self.tables.get(&lower(name))
    }

    pub fn view(&self, name: &str) -> Option<&Rc<View>> {
        self.views.get(&lower(name))
    }

    /// Find an index by name, with its table.
    pub fn index(&self, name: &str) -> Option<(Rc<Table>, Rc<Index>)> {
        let t = self.tables.get(self.index_tables.get(&lower(name))?)?;
        let i = t
            .indexes
            .iter()
            .find(|i| i.name.eq_ignore_ascii_case(name))?;
        Some((t.clone(), i.clone()))
    }

    /// User table names in schema order.
    pub fn table_names(&self) -> Vec<String> {
        self.rows
            .iter()
            .filter(|r| (r.kind == "table" || r.kind == "view") && !r.name.starts_with("sqlite_"))
            .map(|r| r.name.clone())
            .collect()
    }
}
