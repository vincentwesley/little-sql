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

pub mod alter;
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

pub use db::{Database, Params, ResultSet};
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

/// Split SQL text into statements at top-level semicolons (a CREATE
/// TRIGGER body stays whole), without parsing them. Shells use this to run
/// statements one at a time, so a syntax error in one does not stop the
/// statements before it, as in sqlite3.
pub fn split_statements(sql: &str) -> Result<Vec<String>> {
    use sql::lexer::Tok;
    let toks = sql::lexer::tokenize(sql)?;
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    let mut in_trigger = false;
    let mut first_words: Vec<String> = Vec::new();
    for (i, t) in toks.iter().enumerate() {
        match &t.tok {
            Tok::Eof => break,
            Tok::Op(";") => {
                let ends_trigger = i > 0
                    && matches!(&toks[i - 1].tok, Tok::Word(w) if w.eq_ignore_ascii_case("END"));
                if in_trigger && !ends_trigger {
                    continue;
                }
                if let Some(s) = start.take() {
                    out.push(sql[s..t.start].to_string());
                }
                in_trigger = false;
                first_words.clear();
            }
            other => {
                if start.is_none() {
                    start = Some(t.start);
                }
                if first_words.len() < 3 {
                    if let Tok::Word(w) = other {
                        first_words.push(w.to_ascii_uppercase());
                        let fw: Vec<&str> = first_words.iter().map(String::as_str).collect();
                        if matches!(
                            fw.as_slice(),
                            ["CREATE", "TRIGGER"] | ["CREATE", "TEMP" | "TEMPORARY", "TRIGGER"]
                        ) {
                            in_trigger = true;
                        }
                    } else {
                        first_words.push(String::new());
                    }
                }
            }
        }
    }
    if let Some(s) = start {
        out.push(sql[s..].to_string());
    }
    Ok(out)
}
