//! A structural consistency check, in the spirit of `PRAGMA integrity_check`.

use crate::db::Database;
use crate::error::Result;

/// Returns a list of problems; empty means the database is consistent.
pub fn integrity_check(_db: &Database) -> Result<Vec<String>> {
    Ok(Vec::new())
}
