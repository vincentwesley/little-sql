//! Little SQL: a small SQL engine, written from scratch, that reads and
//! writes SQLite database files.
//!
//! ```
//! use littlesql::Database;
//!
//! let db = Database::memory();
//! let rs = db.query("SELECT 1 + 1 AS two, upper('ok')").unwrap();
//! assert_eq!(rs.columns, ["two", "upper('ok')"]);
//! assert_eq!(rs.rows[0][0].to_text(), "2");
//! ```
//!
//! The layers, bottom up: [`storage`] (bytes), [`pager`] and [`wal`] (pages
//! and durability), [`btree`] and [`record`] (the file format), [`schema`],
//! [`sql`] (parser), [`bind`] (name resolution and planning), [`plan`] and
//! [`exec`] (the plan and its interpreter), and [`db`] (the public handle).

pub mod bind;
pub mod btree;
pub mod btree_write;
pub mod datetime;
pub mod db;
pub mod error;
pub mod exec;
pub mod func;
pub mod json;
pub mod pager;
pub mod plan;
pub mod record;
pub mod schema;
pub mod sql;
pub mod storage;
pub mod value;
pub mod wal;

pub use db::{Database, ResultSet};
pub use error::{Error, Result};
pub use exec::Limits;
pub use value::Value;
pub mod check;
pub mod write;

/// True when `sql` ends with a complete statement: its last token is a `;`
/// outside any string, identifier or comment. Shells use this to decide
/// whether to read another line.
pub fn sql_is_complete(sql: &str) -> bool {
    match sql::lexer::tokenize(sql) {
        Ok(toks) => {
            use sql::lexer::Tok;
            let words: Vec<&Tok> = toks
                .iter()
                .map(|t| &t.tok)
                .filter(|t| **t != Tok::Eof)
                .collect();
            if words.last() != Some(&&Tok::Op(";")) {
                return false;
            }
            // A CREATE TRIGGER body contains semicolons; it ends at END;.
            let is_word = |t: &Tok, w: &str| matches!(t, Tok::Word(x) if x.eq_ignore_ascii_case(w));
            let mut in_trigger = false;
            let mut i = 0;
            let mut at_stmt_start = true;
            while i < words.len() {
                let t = words[i];
                if at_stmt_start && is_word(t, "CREATE") {
                    let mut j = i + 1;
                    if j < words.len()
                        && (is_word(words[j], "TEMP") || is_word(words[j], "TEMPORARY"))
                    {
                        j += 1;
                    }
                    if j < words.len() && is_word(words[j], "TRIGGER") {
                        in_trigger = true;
                    }
                }
                at_stmt_start = false;
                if *t == Tok::Op(";") && (!in_trigger || (i > 0 && is_word(words[i - 1], "END"))) {
                    in_trigger = false;
                    at_stmt_start = true;
                }
                i += 1;
            }
            !in_trigger
        }
        // An unterminated string needs more input; any other lexing error
        // is complete enough to report.
        Err(e) => !e.to_string().contains("unterminated"),
    }
}
