//! Run sqllogictest files against Little SQL and score the outcome.

use crate::md5::Md5;
use crate::oracle::Oracle;
use crate::script::{parse, Expected, Kind, Record, Sort};
use littlesql::{Database, Limits, ResultSet, Value};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    /// sqlite3 runs the data-changing statements into a real database file;
    /// Little SQL opens that file and answers the queries.
    Read,
    /// Little SQL runs everything itself.
    Native,
}

#[derive(Debug, Clone)]
pub struct Failure {
    pub file: String,
    pub line: usize,
    pub reason: String,
    pub sql: String,
}

#[derive(Debug, Default, Clone)]
pub struct Tally {
    /// Records the engine answered exactly as expected.
    pub pass: u64,
    /// Records the engine got wrong, could not run, or never reached.
    pub fail: u64,
    /// Records that do not apply to SQLite (`skipif sqlite`, `onlyif <other>`,
    /// or after `halt`); excluded from the pass rate.
    pub not_applicable: u64,
    /// Read mode only: statements executed by sqlite3 to build the database.
    /// Never counted as passes.
    pub setup: u64,
}

impl Tally {
    pub fn add(&mut self, o: &Tally) {
        self.pass += o.pass;
        self.fail += o.fail;
        self.not_applicable += o.not_applicable;
        self.setup += o.setup;
    }

    pub fn scored(&self) -> u64 {
        self.pass + self.fail
    }

    pub fn rate(&self) -> f64 {
        if self.scored() == 0 {
            0.0
        } else {
            self.pass as f64 * 100.0 / self.scored() as f64
        }
    }

    /// The rate rounded down to two decimals, so a run with any failure
    /// never shows as 100.00 %.
    pub fn rate_floor(&self) -> f64 {
        (self.rate() * 100.0).floor() / 100.0
    }
}

#[derive(Debug, Clone)]
pub struct FileResult {
    pub file: String,
    pub category: String,
    pub tally: Tally,
    pub failures: Vec<Failure>,
    pub millis: u128,
}

/// Category of a test file: its place in the corpus tree, e.g.
/// `index/between`, `random/expr`, `evidence` or `select1`.
pub fn category(rel: &Path) -> String {
    let parts: Vec<String> = rel
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    match parts.len() {
        0 | 1 => rel
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        2 => parts[0].clone(),
        _ => format!("{}/{}", parts[0], parts[1]),
    }
}

fn is_query_sql(sql: &str) -> bool {
    let first = sql
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    matches!(first.as_str(), "SELECT" | "VALUES" | "WITH" | "EXPLAIN")
}

/// Render a value the way the reference harness (`slt_sqlite.c`) does.
/// `%.3f` as SQLite's own printf renders it: at most 16 significant
/// digits, with zeros after that (so -2^63 prints as
/// -9223372036854776000.000, not -9223372036854775808.000).
pub fn sqlite_fixed3(x: f64) -> String {
    if !x.is_finite() || x == 0.0 {
        return format!("{x:.3}");
    }
    let sci = format!("{:.15e}", x.abs());
    let (mant, exp) = sci.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    if exp + 1 + 3 <= 16 {
        return format!("{x:.3}");
    }
    let digits: Vec<u8> = mant.bytes().filter(|b| b.is_ascii_digit()).collect();
    let int_len = (exp + 1) as usize;
    let digit = |i: usize| digits.get(i).map_or('0', |d| *d as char);
    let mut out = String::new();
    if x < 0.0 {
        out.push('-');
    }
    out.extend((0..int_len).map(digit));
    out.push('.');
    out.extend((int_len..int_len + 3).map(digit));
    out
}

pub fn format_value(v: &Value, ty: char) -> String {
    if v.is_null() {
        return "NULL".into();
    }
    match ty {
        'I' => {
            let i = match v {
                Value::Real(r) => littlesql::value::real_to_int(*r),
                other => other.to_int(),
            };
            // sqlite3_column_int truncates to 32 bits.
            (i as i32).to_string()
        }
        'R' => sqlite_fixed3(v.to_real()),
        _ => {
            let bytes: Vec<u8> = match v {
                Value::Blob(b) => b.to_vec(),
                other => other.to_text().into_bytes(),
            };
            if bytes.is_empty() {
                return "(empty)".into();
            }
            // C strings stop at the first NUL.
            let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
            if end == 0 {
                return "(empty)".into();
            }
            bytes[..end]
                .iter()
                .map(|b| {
                    if (b' '..=b'~').contains(b) {
                        *b as char
                    } else {
                        '@'
                    }
                })
                .collect()
        }
    }
}

/// Compare a result set with the expectation of a `query` record.
pub fn check(
    rs: &ResultSet,
    types: &str,
    sort: Sort,
    expected: &Expected,
    threshold: usize,
) -> Result<(), String> {
    let ncols = types.chars().count();
    if rs.columns.len() != ncols {
        return Err(format!(
            "expected {ncols} result columns, got {}",
            rs.columns.len()
        ));
    }
    let tys: Vec<char> = types.chars().collect();
    let mut rows: Vec<Vec<String>> = rs
        .rows
        .iter()
        .map(|r| {
            r.iter()
                .zip(&tys)
                .map(|(v, t)| format_value(v, *t))
                .collect()
        })
        .collect();
    let mut values: Vec<String> = match sort {
        Sort::Row => {
            rows.sort();
            rows.into_iter().flatten().collect()
        }
        _ => rows.into_iter().flatten().collect(),
    };
    if sort == Sort::Value {
        values.sort();
    }
    let hash = |values: &[String]| {
        let mut m = Md5::default();
        for v in values {
            m.update(v.as_bytes());
            m.update(b"\n");
        }
        m.hex()
    };
    match expected {
        Expected::Hash { count, md5 } => {
            if values.len() != *count {
                return Err(format!("expected {count} values, got {}", values.len()));
            }
            if hash(&values) != *md5 {
                return Err("result hash differs".into());
            }
            Ok(())
        }
        Expected::Values(exp) => {
            if values == *exp {
                return Ok(());
            }
            // Tolerate a script whose threshold would have hashed.
            let _ = threshold;
            if values.len() != exp.len() {
                return Err(format!(
                    "expected {} values, got {}",
                    exp.len(),
                    values.len()
                ));
            }
            let i = values
                .iter()
                .zip(exp)
                .position(|(a, b)| a != b)
                .unwrap_or(0);
            Err(format!(
                "value {} differs: expected {:?}, got {:?}",
                i + 1,
                exp[i],
                values[i]
            ))
        }
    }
}

fn run_engine(db: &Database, sql: &str) -> Result<ResultSet, String> {
    match catch_unwind(AssertUnwindSafe(|| db.query(sql))) {
        Ok(Ok(rs)) => Ok(rs),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("engine panicked".into()),
    }
}

pub struct Options {
    pub mode: Mode,
    pub scratch: PathBuf,
    /// Per-statement time limit for the engine.
    pub statement_millis: u64,
    pub max_failures_per_file: usize,
}

/// Run one script file.
pub fn run_file(path: &Path, rel: &Path, opts: &Options, worker: usize) -> FileResult {
    let started = Instant::now();
    let file = rel.to_string_lossy().into_owned();
    let mut res = FileResult {
        file: file.clone(),
        category: category(rel),
        tally: Tally::default(),
        failures: Vec::new(),
        millis: 0,
    };
    let text = match std::fs::read(path) {
        Ok(b) => String::from_utf8_lossy(&b).into_owned(),
        Err(e) => {
            res.tally.fail += 1;
            res.failures.push(Failure {
                file,
                line: 0,
                reason: format!("cannot read file: {e}"),
                sql: String::new(),
            });
            return res;
        }
    };
    let records = parse(&text);
    let limits = Limits {
        max_steps: 0,
        max_millis: opts.statement_millis,
    };
    let db_path = opts
        .scratch
        .join(format!("slt-{}-{worker}.db", std::process::id()));
    let cleanup = |p: &Path| {
        let _ = std::fs::remove_file(p);
        for ext in ["-journal", "-wal", "-shm"] {
            let mut s = p.as_os_str().to_owned();
            s.push(ext);
            let _ = std::fs::remove_file(s);
        }
    };
    cleanup(&db_path);

    let mut oracle = None;
    let mut db: Option<Database> = None;
    let mut stale = true;
    let mut abort: Option<String> = None;
    if opts.mode == Mode::Read {
        match Oracle::spawn(&db_path, false) {
            Ok(o) => oracle = Some(o),
            Err(e) => abort = Some(e),
        }
    } else {
        let d = Database::memory();
        d.set_limits(limits);
        db = Some(d);
        stale = false;
    }

    let mut threshold = 0usize;
    let mut halted = false;
    let fail = |res: &mut FileResult, rec: &Record, reason: String| {
        res.tally.fail += 1;
        if res.failures.len() < opts.max_failures_per_file {
            res.failures.push(Failure {
                file: res.file.clone(),
                line: rec.line,
                reason,
                sql: rec.sql.clone(),
            });
        }
    };
    for rec in &records {
        match &rec.kind {
            Kind::HashThreshold(n) => {
                threshold = *n;
                continue;
            }
            Kind::Halt => {
                if rec.applies() {
                    halted = true;
                }
                continue;
            }
            _ => {}
        }
        if halted || !rec.applies() {
            res.tally.not_applicable += 1;
            continue;
        }
        if let Some(why) = &abort {
            fail(&mut res, rec, format!("not reached: {why}"));
            continue;
        }
        let engine_runs = opts.mode == Mode::Native || is_query_sql(&rec.sql);
        if !engine_runs {
            // Read mode: sqlite3 prepares the database.
            match oracle.as_mut().unwrap().exec(&rec.sql) {
                Ok(_) => {
                    res.tally.setup += 1;
                    stale = true;
                }
                Err(e) => abort = Some(e),
            }
            continue;
        }
        if stale {
            db = match Database::open_readonly(&db_path) {
                Ok(d) => {
                    d.set_limits(limits);
                    Some(d)
                }
                Err(e) => {
                    fail(&mut res, rec, format!("cannot open the database: {e}"));
                    continue;
                }
            };
            stale = false;
        }
        let outcome = run_engine(db.as_ref().unwrap(), &rec.sql);
        if matches!(&outcome, Err(e) if e == "engine panicked") {
            if opts.mode == Mode::Read {
                stale = true;
            } else {
                abort = Some("engine panicked earlier in this file".into());
            }
        }
        match (&rec.kind, outcome) {
            (Kind::Statement { expect_ok: true }, Ok(_))
            | (Kind::Statement { expect_ok: false }, Err(_)) => res.tally.pass += 1,
            (Kind::Statement { expect_ok: true }, Err(e)) => {
                fail(&mut res, rec, format!("error: {e}"))
            }
            (Kind::Statement { expect_ok: false }, Ok(_)) => fail(
                &mut res,
                rec,
                "statement succeeded but an error was expected".into(),
            ),
            (
                Kind::Query {
                    types,
                    sort,
                    expected,
                },
                Ok(rs),
            ) => match check(&rs, types, *sort, expected, threshold) {
                Ok(()) => res.tally.pass += 1,
                Err(why) => fail(&mut res, rec, format!("wrong result: {why}")),
            },
            // The reference runner records a query that SQLite aborts at
            // run time (for example sum() overflow) as returning no rows.
            // Only a run-time error counts here; parse errors and
            // unsupported features never do.
            (
                Kind::Query {
                    expected: Expected::Values(v),
                    ..
                },
                Err(e),
            ) if v.is_empty() && e == "integer overflow" => res.tally.pass += 1,
            (Kind::Query { .. }, Err(e)) => fail(&mut res, rec, format!("error: {e}")),
            _ => {}
        }
    }
    drop(db);
    drop(oracle);
    cleanup(&db_path);
    res.millis = started.elapsed().as_millis();
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed3_matches_sqlite_printf() {
        assert_eq!(sqlite_fixed3(1.5), "1.500");
        assert_eq!(sqlite_fixed3(-0.0004), "-0.000");
        assert_eq!(
            sqlite_fixed3(-9223372036854775808.0),
            "-9223372036854776000.000"
        );
        assert_eq!(
            sqlite_fixed3(-18446744073709551616.0),
            "-18446744073709550000.000"
        );
        assert_eq!(sqlite_fixed3(123456789012.3456), "123456789012.346");
        assert_eq!(sqlite_fixed3(1234567890123.4567), "1234567890123.457");
    }

    #[test]
    fn formats_like_the_reference_harness() {
        assert_eq!(format_value(&Value::Null, 'I'), "NULL");
        assert_eq!(format_value(&Value::Real(2.5), 'I'), "2");
        assert_eq!(format_value(&Value::Int(1 << 32), 'I'), "0");
        assert_eq!(format_value(&Value::text("12abc"), 'I'), "12");
        assert_eq!(format_value(&Value::Int(3), 'R'), "3.000");
        assert_eq!(format_value(&Value::Real(1.0005), 'R'), "1.000");
        assert_eq!(format_value(&Value::text(""), 'T'), "(empty)");
        assert_eq!(format_value(&Value::text("a\tb\u{e9}"), 'T'), "a@b@@");
        assert_eq!(format_value(&Value::Real(1.5), 'T'), "1.5");
    }

    #[test]
    fn categories() {
        assert_eq!(category(Path::new("select1.test")), "select1");
        assert_eq!(category(Path::new("evidence/in1.test")), "evidence");
        assert_eq!(
            category(Path::new("index/between/10/slt_good_0.test")),
            "index/between"
        );
        assert_eq!(
            category(Path::new("random/expr/slt_good_3.test")),
            "random/expr"
        );
    }

    #[test]
    fn checks_values_and_hashes() {
        let rs = ResultSet {
            columns: vec!["a".into(), "b".into()],
            rows: vec![
                vec![Value::Int(2), Value::text("x")],
                vec![Value::Int(1), Value::Null],
            ],
            ..Default::default()
        };
        let exp = Expected::Values(["1", "NULL", "2", "x"].map(String::from).to_vec());
        assert!(check(&rs, "IT", Sort::Row, &exp, 0).is_ok());
        assert!(check(&rs, "IT", Sort::None, &exp, 0).is_err());
        assert!(check(&rs, "I", Sort::Row, &exp, 0).is_err());
        let mut m = Md5::default();
        m.update(b"1\nNULL\n2\nx\n");
        let h = Expected::Hash {
            count: 4,
            md5: m.hex(),
        };
        assert!(check(&rs, "IT", Sort::Row, &h, 2).is_ok());
    }
}
