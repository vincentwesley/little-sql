//! `ATTACH` and `DETACH`: more database files on one connection.
//!
//! Each attached file gets its own pager; its schema joins the merged
//! schema the connection binds against (see `Schema::merge`).

use crate::db::{Database, DbFile, ResultSet};
use crate::error::{Error, Result};
use crate::pager::Pager;
use crate::sql::ast::Expr;
use crate::storage::{FileStorage, MemStorage};
use crate::value::Value;
use std::path::Path;
use std::rc::Rc;

/// SQLite's default limit on attached files.
const MAX_ATTACHED: usize = 10;

pub(crate) fn attach(db: &Database, file: &Expr, name: &str) -> Result<ResultSet> {
    let rs = db.run_select(
        &crate::write::select_of(vec![crate::write::result(file.clone())], Vec::new(), None),
        true,
    )?;
    let path = match rs.rows.first().and_then(|r| r.first()) {
        Some(Value::Null) | None => String::new(),
        Some(v) => v.to_text(),
    };
    if db
        .files
        .borrow()
        .iter()
        .any(|f| f.name.eq_ignore_ascii_case(name))
    {
        return Err(Error::Runtime(format!("database {name} is already in use")));
    }
    if db.files.borrow().len() - 2 >= MAX_ATTACHED {
        return Err(Error::Runtime(format!(
            "too many attached databases - max {MAX_ATTACHED}"
        )));
    }
    let (pager, shown, wal_path) = if path.is_empty() || path == ":memory:" {
        let p = Pager::open(Box::new(MemStorage::new(Vec::new())), None, true)?;
        (p, String::new(), None)
    } else {
        let p = Path::new(&path);
        let opened = match FileStorage::open_readwrite(p) {
            Ok(st) => {
                let wal = {
                    let mut s = p.as_os_str().to_owned();
                    s.push("-wal");
                    std::path::PathBuf::from(s)
                };
                Pager::open(Box::new(st), Some(Box::new(FileStorage::lazy(&wal))), true)
                    .map(|pg| (pg, Some(wal)))
            }
            Err(_) => match FileStorage::open_readonly(p) {
                Ok(st) => Pager::open(Box::new(st), None, false).map(|pg| (pg, None)),
                Err(_) => return Err(Error::Runtime(format!("unable to open database: {path}"))),
            },
        };
        let (pg, wal) = opened.map_err(|e| match e {
            Error::Corrupt(_) => Error::Runtime("file is not a database".into()),
            other => other,
        })?;
        let shown = std::fs::canonicalize(p)
            .unwrap_or_else(|_| p.to_path_buf())
            .display()
            .to_string();
        (pg, shown, wal)
    };
    if pager.page_count() > 0 && pager.encoding() != db.pager.encoding() {
        return Err(Error::Sql(
            "attached databases must use the same text encoding as main database".into(),
        ));
    }
    db.files.borrow_mut().push(DbFile {
        name: name.to_string(),
        pager: Rc::new(pager),
        path: shown,
        wal_path,
    });
    if let Err(e) = db.reload_schema() {
        db.files.borrow_mut().pop();
        db.reload_schema()?;
        return Err(e);
    }
    Ok(ResultSet::default())
}

pub(crate) fn detach(db: &Database, name: &str) -> Result<ResultSet> {
    let pos = db
        .files
        .borrow()
        .iter()
        .position(|f| f.name.eq_ignore_ascii_case(name));
    let Some(pos) = pos else {
        return Err(Error::Runtime(format!("no such database: {name}")));
    };
    if pos < 2 {
        return Err(Error::Runtime(format!("cannot detach database {name}")));
    }
    if db.files.borrow()[pos].pager.in_transaction() {
        return Err(Error::Runtime(format!("database {name} is locked")));
    }
    let f = db.files.borrow_mut().remove(pos);
    close_file(&f)?;
    db.reload_schema()?;
    Ok(ResultSet::default())
}

/// Checkpoint a file's WAL and remove the WAL file if it is empty.
pub(crate) fn close_file(f: &DbFile) -> Result<()> {
    let pager = &f.pager;
    if !pager.is_writable() || !pager.has_wal() {
        return Ok(());
    }
    if pager.in_transaction() {
        pager.rollback();
    }
    if pager.wal_frames() > 0 {
        pager.checkpoint()?;
    }
    if let Some(p) = &f.wal_path {
        if p.metadata().map(|m| m.len() == 0).unwrap_or(false) {
            let _ = std::fs::remove_file(p);
        }
    }
    Ok(())
}
