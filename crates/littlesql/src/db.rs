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

/// The absolute form of a path, as `PRAGMA database_list` reports it.
fn absolute(path: &Path) -> String {
    std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}

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
    /// True for EXPLAIN / EXPLAIN QUERY PLAN results, whose `plan` a shell
    /// should draw as a tree.
    pub is_explain: bool,
}

impl ResultSet {
    /// The plan as an indented tree with run statistics.
    pub fn plan_text(&self) -> String {
        render_plan(&self.plan, Some(&self.stats))
    }
}

/// Values for a statement's parameters: `?` and `?N` by number (1-based,
/// so `positional[0]` is `?1`), `:name`, `@name` and `$name` by name (with
/// the prefix, e.g. `":id"`), or by the number the name was given.
#[derive(Clone, Debug, Default)]
pub struct Params {
    pub positional: Vec<Value>,
    pub named: Vec<(String, Value)>,
    /// Bind named parameters only by name, never by their number (what
    /// the sqlite3 shell's `.parameter` does).
    pub names_only: bool,
}

pub struct Database {
    pub(crate) pager: Rc<Pager>,
    pub(crate) schema: RefCell<Rc<Schema>>,
    /// True between BEGIN and COMMIT/ROLLBACK.
    pub(crate) explicit_txn: std::cell::Cell<bool>,
    /// Open savepoints, outermost first, each with the state to return to.
    pub(crate) savepoints: RefCell<Vec<(String, Savepoints)>>,
    /// The transaction was started by SAVEPOINT (not BEGIN), so releasing
    /// the outermost savepoint commits it.
    pub(crate) savepoint_txn: std::cell::Cell<bool>,
    /// Every file of the connection in `PRAGMA database_list` order: main,
    /// temp (an in-memory file only this connection sees), then attached
    /// files.
    pub(crate) files: RefCell<Vec<DbFile>>,
    /// The file the statement now running changes (see DECISIONS D37).
    pub(crate) target: RefCell<Option<Rc<Pager>>>,
    pub(crate) limits: std::cell::Cell<Limits>,
    mem: Option<MemStorage>,
}

/// A savepoint of every file of the connection.
pub(crate) type Savepoints = Vec<(Rc<Pager>, crate::pager::Savepoint)>;

/// One file of a connection.
pub(crate) struct DbFile {
    pub name: String,
    pub pager: Rc<Pager>,
    /// The file on disk ("" for memory), as `PRAGMA database_list` shows.
    pub path: String,
    pub wal_path: Option<PathBuf>,
}

fn temp_pager() -> Rc<Pager> {
    Rc::new(
        Pager::open(Box::new(MemStorage::new(Vec::new())), None, true)
            .expect("an empty database always opens"),
    )
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
        path: String,
    ) -> Result<Database> {
        let pager = Rc::new(pager);
        let files = vec![
            DbFile {
                name: "main".into(),
                pager: pager.clone(),
                path,
                wal_path: wal_path.clone(),
            },
            DbFile {
                name: "temp".into(),
                pager: temp_pager(),
                path: String::new(),
                wal_path: None,
            },
        ];
        let db = Database {
            pager,
            schema: RefCell::new(Rc::new(Schema::default())),
            explicit_txn: std::cell::Cell::new(false),
            savepoints: RefCell::new(Vec::new()),
            savepoint_txn: std::cell::Cell::new(false),
            files: RefCell::new(files),
            target: RefCell::new(None),
            limits: Default::default(),
            mem,
        };
        db.reload_schema()?;
        Ok(db)
    }

    /// The file the running statement changes (main when none was chosen).
    pub(crate) fn target(&self) -> Rc<Pager> {
        self.target
            .borrow()
            .clone()
            .unwrap_or_else(|| self.pager.clone())
    }

    /// Run `f` with `pager` as the file being changed.
    pub(crate) fn with_target<T>(&self, pager: Rc<Pager>, f: impl FnOnce() -> T) -> T {
        let saved = self.target.replace(Some(pager));
        let r = f();
        *self.target.borrow_mut() = saved;
        r
    }

    /// The schema of the target file alone.
    pub(crate) fn target_schema(&self) -> Rc<Schema> {
        let t = self.target();
        let schema = self.schema();
        schema
            .dbs
            .iter()
            .find(|(_, s)| s.pager.as_ref().is_some_and(|p| Rc::ptr_eq(p, &t)))
            .map(|(_, s)| s.clone())
            .unwrap_or(schema)
    }

    /// The name of the file a pager belongs to.
    pub(crate) fn file_name(&self, pager: &Rc<Pager>) -> String {
        self.files
            .borrow()
            .iter()
            .find(|f| Rc::ptr_eq(&f.pager, pager))
            .map_or_else(|| "main".into(), |f| f.name.clone())
    }

    /// The pager of the file called `name` (an error if there is none).
    pub(crate) fn file_pager(&self, name: &str) -> Result<Rc<Pager>> {
        self.files
            .borrow()
            .iter()
            .find(|f| f.name.eq_ignore_ascii_case(name))
            .map(|f| f.pager.clone())
            .ok_or_else(|| Error::Sql(format!("unknown database {name}")))
    }

    /// Every file's pager, in `database_list` order.
    pub(crate) fn pagers(&self) -> Vec<Rc<Pager>> {
        self.files
            .borrow()
            .iter()
            .map(|f| f.pager.clone())
            .collect()
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
            absolute(path),
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
        Database::from_pager(
            Pager::open(Box::new(db), wal, false)?,
            None,
            None,
            absolute(path),
        )
    }

    /// A new, empty in-memory database.
    pub fn memory() -> Database {
        Database::from_bytes(Vec::new()).expect("an empty database always opens")
    }

    /// Open a database image held in memory (for example an uploaded file).
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Database> {
        let mem = MemStorage::new(bytes);
        let pager = Pager::open(Box::new(mem.clone()), None, true)?;
        Database::from_pager(pager, None, Some(mem), String::new())
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

    /// Re-read every file's schema and rebuild the merged one.
    pub(crate) fn reload_schema(&self) -> Result<()> {
        let mut parts = Vec::new();
        for f in self.files.borrow().iter() {
            parts.push((f.name.clone(), Rc::new(Schema::load(&f.pager)?)));
        }
        let mut merged = Schema::merge(&parts);
        merged.paths = self.files.borrow().iter().map(|f| f.path.clone()).collect();
        *self.schema.borrow_mut() = Rc::new(merged);
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

    /// [`Database::execute`] with parameter values.
    ///
    /// ```
    /// use littlesql::{Database, Params, Value};
    /// let db = Database::memory();
    /// let p = Params {
    ///     positional: vec![Value::Int(2)],
    ///     named: vec![(":x".into(), Value::Int(40))],
    ///     ..Default::default()
    /// };
    /// let rs = db.query_with("SELECT ?1 + :x", &p).unwrap();
    /// assert_eq!(rs.rows[0][0], Value::Int(42));
    /// ```
    pub fn execute_with(&self, sql: &str, params: &Params) -> Result<Vec<ResultSet>> {
        crate::bind::with_params(params, || self.execute(sql))
    }

    /// [`Database::query`] with parameter values.
    pub fn query_with(&self, sql: &str, params: &Params) -> Result<ResultSet> {
        crate::bind::with_params(params, || self.query(sql))
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
            rs.rows = crate::pragma::with_schema(schema.clone(), || ex.collect(&plan.query))?;
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
                    is_explain: true,
                })
            }
            Stmt::Pragma { db, name, value } => self.pragma(db.as_deref(), name, value.as_deref()),
            Stmt::Noop(_) => Ok(ResultSet::default()),
            other => crate::write::run_write(self, other, text),
        }
    }

    fn pragma(&self, db: Option<&str>, name: &str, value: Option<&str>) -> Result<ResultSet> {
        let pager = match db {
            Some(d) => self.file_pager(d)?,
            None => self.pager.clone(),
        };
        let one = |col: &str, v: Value| ResultSet {
            columns: vec![col.to_string()],
            rows: vec![vec![v]],
            ..Default::default()
        };
        let schema = self.schema();
        match name.to_ascii_lowercase().as_str() {
            "page_size" => Ok(one("page_size", Value::Int(pager.page_size() as i64))),
            "page_count" => Ok(one("page_count", Value::Int(pager.page_count() as i64))),
            "encoding" => Ok(one(
                "encoding",
                Value::text(match pager.encoding() {
                    crate::record::TextEncoding::Utf8 => "UTF-8",
                    crate::record::TextEncoding::Utf16Le => "UTF-16le",
                    crate::record::TextEncoding::Utf16Be => "UTF-16be",
                }),
            )),
            "journal_mode" => {
                let mode = if self.mem.is_some() && Rc::ptr_eq(&pager, &self.pager)
                    || !pager.has_wal() && !Rc::ptr_eq(&pager, &self.pager)
                {
                    "memory"
                } else if pager.page_count() > 0 && pager.get(1)?[18] == 2 {
                    "wal"
                } else {
                    "delete"
                };
                Ok(one("journal_mode", Value::text(mode)))
            }
            "wal_checkpoint" => {
                let n = pager.checkpoint()?;
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
                let own = self.schema();
                let file_schema = own
                    .dbs
                    .iter()
                    .find(|(_, s)| s.pager.as_ref().is_some_and(|p| Rc::ptr_eq(p, &pager)))
                    .map(|(_, s)| s.clone())
                    .unwrap_or_else(|| own.clone());
                let problems = crate::check::integrity_check(&pager, &file_schema)?;
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
            n if crate::pragma::columns(n).is_some() => {
                let (cols, _) = crate::pragma::columns(n).unwrap();
                Ok(ResultSet {
                    columns: cols.iter().map(|c| c.to_string()).collect(),
                    rows: crate::pragma::rows(&schema, db, n, value).unwrap()?,
                    ..Default::default()
                })
            }
            // Settings that have no effect here are accepted silently so
            // that scripts written for sqlite3 keep working.
            _ => Ok(ResultSet::default()),
        }
    }

    /// Discard every change not yet committed, in every file, and end any
    /// transaction (for a caller recovering from an internal error).
    pub fn rollback_uncommitted(&self) {
        for p in self.pagers() {
            p.rollback();
        }
        self.explicit_txn.set(false);
        self.savepoints.borrow_mut().clear();
        self.savepoint_txn.set(false);
        let _ = self.reload_schema();
    }

    /// Checkpoint and tidy up. Called automatically when the handle drops.
    pub fn close(&self) -> Result<()> {
        for f in self.files.borrow().iter() {
            crate::attach::close_file(f)?;
        }
        Ok(())
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        let _ = self.close();
    }
}
