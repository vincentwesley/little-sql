//! Statements that change the database.

use crate::db::{Database, ResultSet};
use crate::error::{Error, Result};
use crate::sql::ast::Stmt;

pub(crate) fn run_write(_db: &Database, _stmt: &Stmt, _text: &str) -> Result<ResultSet> {
    Err(Error::Unsupported(
        "statements that modify the database".into(),
    ))
}
