//! The public database handle.

use crate::bind::bind_select;
use crate::error::{Error, Result};
use crate::exec::{Exec, Limits};
use crate::pager::Pager;
use crate::plan::{explain_query, render_plan, CursorStats, PlanLine};
use crate::schema::Schema;
use crate::sql::ast::{self, Stmt};
use crate::sql::parser::parse_statements;
use crate::storage::{FileStorage, MemStorage, Storage};
use crate::value::Value;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// The outcome of one statement.
#[derive(Debug, Default, Clone)]
pub struct ResultSet {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
    /// How the statement was executed (empty for statements without a plan).
    pub plan: Vec<PlanLine>,
    /// Per-cursor run statistics, indexed by `PlanLine::cursor`.
    pub stats: Vec<CursorStats>,
    /// Pages fetched from storage while the statement ran.
    pub page_reads: u64,
    /// Rows inserted, updated or deleted.
    pub changes: u64,
}

impl ResultSet {
    /// The plan as an indented tree with run statistics.
    pub fn plan_text(&self) -> String {
        render_plan(&self.plan, Some(&self.stats))
    }
}

pub struct Database {
    pub(crate) pager: Rc<Pager>,
    pub(crate) schema: RefCell<Rc<Schema>>,
    /// True between BEGIN and COMMIT/ROLLBACK.
    pub(crate) explicit_txn: RefCell<bool>,
    pub(crate) limits: std::cell::Cell<Limits>,
    wal_path: Option<PathBuf>,
    mem: Option<MemStorage>,
}

fn wal_path_for(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push("-wal");
    PathBuf::from(s)
}

impl Database {
    fn from_pager(
        pager: Pager,
        wal_path: Option<PathBuf>,
        mem: Option<MemStorage>,
    ) -> Result<Database> {
        let pager = Rc::new(pager);
        let schema = Rc::new(Schema::load(&pager)?);
        Ok(Database {
            pager,
            schema: RefCell::new(schema),
            explicit_txn: RefCell::new(false),
            limits: Default::default(),
            wal_path,
            mem,
        })
    }

    /// Open a database file for reading and writing, creating it if it does
    /// not exist. Falls back to read-only when the file cannot be written.
    pub fn open(path: impl AsRef<Path>) -> Result<Database> {
        let path = path.as_ref();
        let journal = {
            let mut s = path.as_os_str().to_owned();
            s.push("-journal");
            PathBuf::from(s)
        };
        if journal.metadata().map(|m| m.len() > 0).unwrap_or(false) {
            return Err(Error::Unsupported(format!(
                "{} has a hot rollback journal; open it once with sqlite3 to recover it",
                path.display()
            )));
        }
        let db = match FileStorage::open_readwrite(path) {
            Ok(s) => s,
            Err(_) => return Database::open_readonly(path),
        };
        let wal_path = wal_path_for(path);
        let wal = FileStorage::lazy(&wal_path);
        Database::from_pager(
            Pager::open(Box::new(db), Some(Box::new(wal)), true)?,
            Some(wal_path),
            None,
        )
    }

    /// Open an existing database file without ever writing to it.
    pub fn open_readonly(path: impl AsRef<Path>) -> Result<Database> {
        let path = path.as_ref();
        let db = FileStorage::open_readonly(path)?;
        let wal_path = wal_path_for(path);
        let wal: Option<Box<dyn Storage>> = match FileStorage::open_readonly(&wal_path) {
            Ok(w) => Some(Box::new(w)),
            Err(_) => None,
        };
        Database::from_pager(Pager::open(Box::new(db), wal, false)?, None, None)
    }

    /// A new, empty in-memory database.
    pub fn memory() -> Database {
        Database::from_bytes(Vec::new()).expect("an empty database always opens")
    }

    /// Open a database image held in memory (for example an uploaded file).
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Database> {
        let mem = MemStorage::new(bytes);
        let pager = Pager::open(Box::new(mem.clone()), None, true)?;
        Database::from_pager(pager, None, Some(mem))
    }

    /// The current image of an in-memory database.
    pub fn to_bytes(&self) -> Option<Vec<u8>> {
        self.mem.as_ref().map(|m| m.bytes())
    }

    /// Limit how long a single statement may run (see [`Limits`]).
    pub fn set_limits(&self, limits: Limits) {
        self.limits.set(limits);
    }

    pub fn schema(&self) -> Rc<Schema> {
        self.schema.borrow().clone()
    }

    pub fn pager(&self) -> &Rc<Pager> {
        &self.pager
    }

    pub(crate) fn reload_schema(&self) -> Result<()> {
        *self.schema.borrow_mut() = Rc::new(Schema::load(&self.pager)?);
        Ok(())
    }

    /// Run every statement in `sql`, returning one result per statement.
    /// Execution stops at the first error.
    pub fn execute(&self, sql: &str) -> Result<Vec<ResultSet>> {
        let mut out = Vec::new();
        for (stmt, text) in parse_statements(sql)? {
            out.push(self.run(&stmt, &text)?);
        }
        Ok(out)
    }

    /// Run `sql` and return the result of its last statement.
    pub fn query(&self, sql: &str) -> Result<ResultSet> {
        self.execute(sql)?
            .pop()
            .ok_or_else(|| Error::Parse("empty statement".into()))
    }

    /// The plan for a SELECT, as text, without running it.
    pub fn explain(&self, sql: &str) -> Result<String> {
        let rs = self.query(&format!("EXPLAIN QUERY PLAN {sql}"))?;
        Ok(render_plan(&rs.plan, None))
    }

    pub(crate) fn run_select(&self, sel: &ast::Select, execute: bool) -> Result<ResultSet> {
        let schema = self.schema();
        let plan = bind_select(&schema, &self.pager, sel)?;
        let mut rs = ResultSet {
            columns: plan.query.cols.iter().map(|c| c.name.clone()).collect(),
            ..Default::default()
        };
        explain_query(&plan.query, 0, &mut rs.plan);
        if execute {
            let before = self.pager.stats.page_reads.get();
            let mut ex = Exec::new(self.pager.clone(), &plan);
            ex.set_limits(self.limits.get());
            rs.rows = ex.collect(&plan.query)?;
            rs.stats = ex.stats;
            rs.page_reads = self.pager.stats.page_reads.get() - before;
        }
        Ok(rs)
    }

    fn run(&self, stmt: &Stmt, text: &str) -> Result<ResultSet> {
        match stmt {
            Stmt::Select(sel) => self.run_select(sel, true),
            Stmt::Explain { query_plan, stmt } => {
                let Stmt::Select(sel) = stmt.as_ref() else {
                    return Err(Error::Unsupported(
                        "EXPLAIN for statements other than SELECT".into(),
                    ));
                };
                // EXPLAIN QUERY PLAN shows the plan; plain EXPLAIN also runs
                // the query so the plan carries row counts.
                let inner = self.run_select(sel, !*query_plan)?;
                let mut parents: Vec<usize> = Vec::new();
                let mut rows = Vec::new();
                for (i, line) in inner.plan.iter().enumerate() {
                    parents.truncate(line.depth);
                    let parent = parents.last().copied().unwrap_or(0);
                    let mut detail = line.text.clone();
                    if let (false, Some(c)) = (*query_plan, line.cursor) {
                        if let Some(s) = inner.stats.get(c) {
                            detail.push_str(&format!(" (loops={} rows={})", s.loops, s.rows));
                        }
                    }
                    rows.push(vec![
                        Value::Int(i as i64 + 1),
                        Value::Int(parent as i64),
                        Value::Int(0),
                        Value::text(detail),
                    ]);
                    parents.push(i + 1);
                }
                Ok(ResultSet {
                    columns: ["id", "parent", "notused", "detail"]
                        .map(String::from)
                        .to_vec(),
                    rows,
                    plan: inner.plan,
                    stats: inner.stats,
                    page_reads: inner.page_reads,
                    changes: 0,
                })
            }
            Stmt::Pragma { name, value } => self.pragma(name, value.as_deref()),
            Stmt::Noop(_) => Ok(ResultSet::default()),
            other => crate::write::run_write(self, other, text),
        }
    }

    fn pragma(&self, name: &str, value: Option<&str>) -> Result<ResultSet> {
        let one = |col: &str, v: Value| ResultSet {
            columns: vec![col.to_string()],
            rows: vec![vec![v]],
            ..Default::default()
        };
        let schema = self.schema();
        match name.to_ascii_lowercase().as_str() {
            "page_size" => Ok(one("page_size", Value::Int(self.pager.page_size() as i64))),
            "page_count" => Ok(one(
                "page_count",
                Value::Int(self.pager.page_count() as i64),
            )),
            "encoding" => Ok(one("encoding", Value::text("UTF-8"))),
            "journal_mode" => Ok(one(
                "journal_mode",
                Value::text(if self.pager.has_wal() {
                    "wal"
                } else {
                    "memory"
                }),
            )),
            "wal_checkpoint" => {
                let n = self.pager.checkpoint()?;
                Ok(ResultSet {
                    columns: ["busy", "log", "checkpointed"].map(String::from).to_vec(),
                    rows: vec![vec![
                        Value::Int(0),
                        Value::Int(n as i64),
                        Value::Int(n as i64),
                    ]],
                    ..Default::default()
                })
            }
            "integrity_check" | "quick_check" => {
                let problems = crate::check::integrity_check(self)?;
                let rows = if problems.is_empty() {
                    vec![vec![Value::text("ok")]]
                } else {
                    problems.into_iter().map(|p| vec![Value::text(p)]).collect()
                };
                Ok(ResultSet {
                    columns: vec!["integrity_check".into()],
                    rows,
                    ..Default::default()
                })
            }
            "table_info" => {
                let t = value.and_then(|v| schema.table(v).cloned());
                let mut rows = Vec::new();
                if let Some(t) = t {
                    for (i, c) in t.columns.iter().enumerate() {
                        let pk = if t.ipk == Some(i) {
                            1
                        } else {
                            t.pk.iter()
                                .position(|k| k.col == i)
                                .map_or(0, |p| p as i64 + 1)
                        };
                        rows.push(vec![
                            Value::Int(i as i64),
                            Value::text(&c.name),
                            Value::text(&c.decl_type),
                            Value::Int(c.not_null as i64),
                            c.default
                                .as_ref()
                                .map_or(Value::Null, |_| Value::text(c.default_value.to_text())),
                            Value::Int(pk),
                        ]);
                    }
                }
                Ok(ResultSet {
                    columns: ["cid", "name", "type", "notnull", "dflt_value", "pk"]
                        .map(String::from)
                        .to_vec(),
                    rows,
                    ..Default::default()
                })
            }
            "index_list" => {
                let mut rows = Vec::new();
                if let Some(t) = value.and_then(|v| schema.table(v).cloned()) {
                    for (i, ix) in t.indexes.iter().enumerate() {
                        rows.push(vec![
                            Value::Int(i as i64),
                            Value::text(&ix.name),
                            Value::Int(ix.unique as i64),
                            Value::text(if ix.sql.is_some() { "c" } else { "u" }),
                            Value::Int(0),
                        ]);
                    }
                }
                Ok(ResultSet {
                    columns: ["seq", "name", "unique", "origin", "partial"]
                        .map(String::from)
                        .to_vec(),
                    rows,
                    ..Default::default()
                })
            }
            // Settings that have no effect here are accepted silently so
            // that scripts written for sqlite3 keep working.
            _ => Ok(ResultSet::default()),
        }
    }

    /// Checkpoint and tidy up. Called automatically when the handle drops.
    pub fn close(&self) -> Result<()> {
        if !self.pager.is_writable() || !self.pager.has_wal() {
            return Ok(());
        }
        if self.pager.in_transaction() {
            self.pager.rollback();
        }
        if self.pager.wal_frames() > 0 {
            self.pager.checkpoint()?;
        }
        if let Some(p) = &self.wal_path {
            if p.metadata().map(|m| m.len() == 0).unwrap_or(false) {
                let _ = std::fs::remove_file(p);
            }
        }
        Ok(())
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        let _ = self.close();
    }
}
