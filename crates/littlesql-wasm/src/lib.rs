//! Little SQL for the browser: a handful of plain C-ABI functions over one
//! database held in memory. JavaScript copies bytes in and out of the
//! module's linear memory; results come back as JSON text.
//!
//! The protocol (see `playground/engine.js`):
//!
//! 1. `ls_alloc(n)` returns a pointer; the host writes `n` bytes there.
//! 2. `ls_open(ptr, n)` takes those bytes as a database file (the buffer is
//!    consumed). `ls_open(0, 0)` opens an empty database.
//! 3. `ls_exec(ptr, n, max_rows, max_steps)` runs the UTF-8 SQL at `ptr`
//!    (consumed) and leaves a JSON report in the output buffer, read with
//!    `ls_out_ptr()` / `ls_out_len()`.
//! 4. `ls_export()` puts the current database image in the output buffer.

use littlesql::plan::{CursorStats, PlanLine};
use littlesql::{Database, Limits, ResultSet, Value};
use std::cell::RefCell;

thread_local! {
    static DB: RefCell<Option<Database>> = const { RefCell::new(None) };
    static OUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

fn set_out(bytes: Vec<u8>) {
    OUT.with(|o| *o.borrow_mut() = bytes);
}

/// Allocate `len` bytes for the host to fill.
#[no_mangle]
pub extern "C" fn ls_alloc(len: usize) -> *mut u8 {
    let mut v = Vec::<u8>::with_capacity(len.max(1));
    let p = v.as_mut_ptr();
    std::mem::forget(v);
    p
}

/// Free a buffer from `ls_alloc` that was not handed to another call.
///
/// # Safety
/// `ptr` and `len` must come from one `ls_alloc` call.
#[no_mangle]
pub unsafe extern "C" fn ls_free(ptr: *mut u8, len: usize) {
    if !ptr.is_null() {
        drop(Vec::from_raw_parts(ptr, 0, len.max(1)));
    }
}

/// Take ownership of an `ls_alloc` buffer holding `len` bytes.
unsafe fn take(ptr: *mut u8, len: usize) -> Vec<u8> {
    if ptr.is_null() {
        return Vec::new();
    }
    Vec::from_raw_parts(ptr, len, len.max(1))
}

#[no_mangle]
pub extern "C" fn ls_out_ptr() -> *const u8 {
    OUT.with(|o| o.borrow().as_ptr())
}

#[no_mangle]
pub extern "C" fn ls_out_len() -> usize {
    OUT.with(|o| o.borrow().len())
}

/// Open a database from bytes (or an empty one). Returns 1 on success; on
/// failure 0, with `{"error": ...}` in the output buffer.
///
/// # Safety
/// `ptr`/`len` must come from `ls_alloc` (or be 0/0).
#[no_mangle]
pub unsafe extern "C" fn ls_open(ptr: *mut u8, len: usize) -> u32 {
    let bytes = take(ptr, len);
    let db = if bytes.is_empty() {
        Ok(Database::memory())
    } else {
        Database::from_bytes(bytes)
    };
    match db.and_then(|db| db.query("SELECT count(*) FROM sqlite_schema").map(|_| db)) {
        Ok(db) => {
            DB.with(|d| *d.borrow_mut() = Some(db));
            set_out(b"{\"error\":null}".to_vec());
            1
        }
        Err(e) => {
            set_out(format!("{{\"error\":{}}}", json_str(&e.to_string())).into_bytes());
            0
        }
    }
}

/// Set the clock used by `'now'` (milliseconds since 1970).
#[no_mangle]
pub extern "C" fn ls_set_now(ms: f64) {
    littlesql::datetime::set_now_unix_ms(Some(ms as i64));
}

/// Run SQL. At most `max_rows` rows per statement are returned (the total
/// is still counted); `max_steps` row visits stop a runaway statement
/// (0 = no limit). Returns 1 if every statement succeeded.
///
/// # Safety
/// `ptr`/`len` must come from `ls_alloc`.
#[no_mangle]
pub unsafe extern "C" fn ls_exec(ptr: *mut u8, len: usize, max_rows: u32, max_steps: u32) -> u32 {
    let sql = String::from_utf8_lossy(&take(ptr, len)).into_owned();
    let (json, ok) = DB.with(|d| {
        let d = d.borrow();
        match d.as_ref() {
            Some(db) => exec(db, &sql, max_rows as usize, max_steps as u64),
            None => (
                "{\"results\":[],\"error\":\"no database is open\"}".to_string(),
                false,
            ),
        }
    });
    set_out(json.into_bytes());
    ok as u32
}

/// Put the current database file image in the output buffer.
#[no_mangle]
pub extern "C" fn ls_export() -> u32 {
    let bytes = DB.with(|d| d.borrow().as_ref().and_then(|db| db.to_bytes()));
    match bytes {
        Some(b) => {
            set_out(b);
            1
        }
        None => 0,
    }
}

/// Run every statement in `sql` and describe the results as JSON.
pub fn exec(db: &Database, sql: &str, max_rows: usize, max_steps: u64) -> (String, bool) {
    db.set_limits(Limits {
        max_steps,
        max_millis: 0,
    });
    let mut out = String::from("{\"results\":[");
    let stmts = match littlesql::sql::parser::parse_statements(sql) {
        Ok(s) => s,
        Err(e) => {
            out.push_str(&format!("],\"error\":{}}}", json_str(&e.to_string())));
            return (out, false);
        }
    };
    let mut error = None;
    for (i, (_, text)) in stmts.iter().enumerate() {
        match db.query(text) {
            Ok(rs) => {
                if i > 0 {
                    out.push(',');
                }
                result_json(&mut out, text, &rs, max_rows);
            }
            Err(e) => {
                error = Some(format!("{e}"));
                break;
            }
        }
    }
    out.push_str("],\"error\":");
    match &error {
        Some(e) => out.push_str(&json_str(e)),
        None => out.push_str("null"),
    }
    out.push('}');
    (out, error.is_none())
}

fn result_json(out: &mut String, sql: &str, rs: &ResultSet, max_rows: usize) {
    out.push_str("{\"sql\":");
    out.push_str(&json_str(sql.trim()));
    out.push_str(",\"columns\":[");
    for (i, c) in rs.columns.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&json_str(c));
    }
    out.push_str("],\"rows\":[");
    for (i, row) in rs.rows.iter().take(max_rows).enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('[');
        for (j, v) in row.iter().enumerate() {
            if j > 0 {
                out.push(',');
            }
            cell_json(out, v);
        }
        out.push(']');
    }
    out.push_str(&format!(
        "],\"rowCount\":{},\"changes\":{},\"pageReads\":{},\"explain\":{},\"plan\":",
        rs.rows.len(),
        rs.changes,
        rs.page_reads,
        rs.is_explain
    ));
    plan_json(out, &rs.plan, &rs.stats);
    out.push('}');
}

/// A cell as `[type, text]`: the text is exactly what sqlite3 would print.
fn cell_json(out: &mut String, v: &Value) {
    let (t, text) = match v {
        Value::Null => ("n", "NULL".to_string()),
        Value::Int(_) => ("i", v.to_text()),
        Value::Real(_) => ("r", v.to_text()),
        Value::Text(s) => ("t", s.to_string()),
        Value::Blob(b) => {
            let shown: String = b.iter().take(64).map(|x| format!("{x:02X}")).collect();
            let more = if b.len() > 64 { "…" } else { "" };
            ("b", format!("x'{shown}{more}'"))
        }
    };
    out.push_str(&format!("[\"{t}\",{}]", json_str(&text)));
}

fn plan_json(out: &mut String, plan: &[PlanLine], stats: &[CursorStats]) {
    out.push('[');
    for (i, l) in plan.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"depth\":{},\"text\":{}",
            l.depth,
            json_str(&l.text)
        ));
        if let Some(s) = l.cursor.and_then(|c| stats.get(c)) {
            out.push_str(&format!(",\"loops\":{},\"rows\":{}", s.loops, s.rows));
        }
        out.push('}');
    }
    out.push(']');
}

pub fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_reports_rows_plan_and_errors() {
        let db = Database::memory();
        let (json, ok) = exec(
            &db,
            "SELECT 1 AS one, 'a\"b', NULL, 2.5; SELECT x'00ff'",
            10,
            0,
        );
        assert!(ok, "{json}");
        assert!(
            json.contains(r#""columns":["one","'a\"b'","NULL","2.5"]"#),
            "{json}"
        );
        assert!(
            json.contains(r#"[["i","1"],["t","a\"b"],["n","NULL"],["r","2.5"]]"#),
            "{json}"
        );
        assert!(json.contains(r#"["b","x'00FF'"]"#), "{json}");
        let (json, ok) = exec(&db, "SELECT 1; SELECT nope", 10, 0);
        assert!(!ok);
        assert!(json.contains("no such column: nope"), "{json}");
        let (json, _) = exec(&db, "SELECT 1; SELECT 2", 1, 0);
        assert!(json.contains(r#""rowCount":1"#), "{json}");
    }
}
