//! Differential tests: run each query through the `sqlite3` tool and
//! through Little SQL on the same database file and compare the rows.
//!
//! A test file is a list of SQL statements, each ending with `;` at the end
//! of a line. Comment lines directly above a statement may carry
//! directives:
//!
//! * `-- fixture: NAME` (once, at the top): build the database from
//!   `fixtures/NAME.sql` with sqlite3.
//! * `-- known-failure: REASON`: the statement is expected to differ. It
//!   still counts as a failure in the published pass rate, but does not
//!   fail the build.

use crate::oracle::Oracle;
use crate::run::{Failure, FileResult, Tally};
use littlesql::{Database, Limits};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Instant;

pub struct Case {
    pub line: usize,
    pub sql: String,
    pub known_failure: Option<String>,
}

pub struct TestFile {
    pub fixture: Option<String>,
    pub cases: Vec<Case>,
}

pub fn parse(text: &str) -> TestFile {
    let mut file = TestFile {
        fixture: None,
        cases: Vec::new(),
    };
    let mut sql: Vec<&str> = Vec::new();
    let mut start = 0;
    let mut known = None;
    for (i, line) in text.lines().enumerate() {
        let t = line.trim();
        if sql.is_empty() {
            if let Some(rest) = t.strip_prefix("--") {
                let rest = rest.trim();
                if let Some(f) = rest.strip_prefix("fixture:") {
                    file.fixture = Some(f.trim().to_string());
                } else if let Some(k) = rest.strip_prefix("known-failure:") {
                    known = Some(k.trim().to_string());
                }
                continue;
            }
            if t.is_empty() {
                continue;
            }
            start = i + 1;
        }
        sql.push(line);
        if t.ends_with(';') {
            let joined = sql.join("\n");
            file.cases.push(Case {
                line: start,
                sql: joined.trim().trim_end_matches(';').to_string(),
                known_failure: known.take(),
            });
            sql.clear();
        }
    }
    file
}

fn parse_ascii(raw: &str) -> Vec<Vec<String>> {
    raw.split('\x1e')
        .filter(|r| !r.is_empty())
        .map(|r| {
            r.trim_start_matches('\n')
                .split('\x1f')
                .map(String::from)
                .collect()
        })
        .collect()
}

/// Run one differential test file.
pub fn run_file(path: &Path, scratch: &Path) -> FileResult {
    let started = Instant::now();
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut res = FileResult {
        file: name.clone(),
        category: path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        tally: Tally::default(),
        failures: Vec::new(),
        millis: 0,
    };
    let mut broken = |res: &mut FileResult, why: String, n: u64| {
        res.tally.fail += n.max(1);
        res.failures.push(Failure {
            file: name.clone(),
            line: 0,
            reason: why,
            sql: String::new(),
        });
    };
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            broken(&mut res, format!("cannot read file: {e}"), 1);
            return res;
        }
    };
    let file = parse(&text);
    let n = file.cases.len() as u64;
    let db_path = scratch.join(format!("diff-{}-{}.db", std::process::id(), res.category));
    let _ = std::fs::remove_file(&db_path);
    if let Some(fx) = &file.fixture {
        let fx_path = path
            .parent()
            .unwrap_or(Path::new("."))
            .join("fixtures")
            .join(format!("{fx}.sql"));
        let built = std::fs::File::open(&fx_path)
            .map_err(|e| e.to_string())
            .and_then(|f| {
                Command::new("sqlite3")
                    .arg(&db_path)
                    .stdin(Stdio::from(f))
                    .output()
                    .map_err(|e| e.to_string())
                    .and_then(|o| {
                        if o.status.success() && o.stderr.is_empty() {
                            Ok(())
                        } else {
                            Err(String::from_utf8_lossy(&o.stderr).into_owned())
                        }
                    })
            });
        if let Err(e) = built {
            broken(&mut res, format!("cannot build fixture {fx}: {e}"), n);
            return res;
        }
    }
    let mut oracle = match Oracle::spawn(&db_path, true) {
        Ok(o) => o,
        Err(e) => {
            broken(&mut res, e, n);
            return res;
        }
    };
    let db = match Database::open_readonly(&db_path) {
        Ok(d) => d,
        Err(e) => {
            broken(
                &mut res,
                format!("cannot open the fixture database: {e}"),
                n,
            );
            return res;
        }
    };
    db.set_limits(Limits {
        max_steps: 0,
        max_millis: 60_000,
    });
    for case in &file.cases {
        let reference = match oracle.exec(&case.sql) {
            Ok(r) => r,
            Err(e) => {
                broken(&mut res, e, 1);
                break;
            }
        };
        let ours = match catch_unwind(AssertUnwindSafe(|| db.query(&case.sql))) {
            Ok(r) => r.map_err(|e| e.to_string()),
            Err(_) => Err("engine panicked".to_string()),
        };
        let ordered = case.sql.to_ascii_uppercase().contains("ORDER BY");
        let verdict: Result<(), String> = match (reference.ok, ours) {
            (false, Err(_)) => Ok(()),
            (false, Ok(_)) => Err(format!(
                "sqlite3 reports an error ({}), Little SQL does not",
                reference.lines.join(" ")
            )),
            (true, Err(e)) => Err(format!("error: {e}")),
            (true, Ok(rs)) => {
                let mut want = parse_ascii(&reference.lines.join("\n"));
                let mut got: Vec<Vec<String>> = rs
                    .rows
                    .iter()
                    .map(|r| {
                        r.iter()
                            .map(|v| {
                                if v.is_null() {
                                    "NULL".to_string()
                                } else {
                                    v.to_text()
                                }
                            })
                            .collect()
                    })
                    .collect();
                if !ordered {
                    want.sort();
                    got.sort();
                }
                if want == got {
                    Ok(())
                } else if want.len() != got.len() {
                    Err(format!("expected {} rows, got {}", want.len(), got.len()))
                } else {
                    let i = want.iter().zip(&got).position(|(a, b)| a != b).unwrap_or(0);
                    Err(format!(
                        "row {} differs: expected {:?}, got {:?}",
                        i + 1,
                        want[i],
                        got[i]
                    ))
                }
            }
        };
        match (verdict, &case.known_failure) {
            (Ok(()), None) => res.tally.pass += 1,
            (Ok(()), Some(_)) => {
                res.tally.pass += 1;
                res.failures.push(Failure {
                    file: name.clone(),
                    line: case.line,
                    reason: "stale: marked known-failure but passes".into(),
                    sql: case.sql.clone(),
                });
            }
            (Err(why), known) => {
                res.tally.fail += 1;
                let reason = match known {
                    Some(k) => format!("known: {k} ({why})"),
                    None => why,
                };
                res.failures.push(Failure {
                    file: name.clone(),
                    line: case.line,
                    reason,
                    sql: case.sql.clone(),
                });
            }
        }
    }
    drop(db);
    drop(oracle);
    let _ = std::fs::remove_file(&db_path);
    res.millis = started.elapsed().as_millis();
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cases_and_directives() {
        let f = parse("-- fixture: basic\n-- a comment\nSELECT 1;\n\n-- known-failure: later\nSELECT a,\n  b FROM t;\nSELECT 2;\n");
        assert_eq!(f.fixture.as_deref(), Some("basic"));
        assert_eq!(f.cases.len(), 3);
        assert_eq!(f.cases[1].sql, "SELECT a,\n  b FROM t");
        assert_eq!(f.cases[1].known_failure.as_deref(), Some("later"));
        assert!(f.cases[2].known_failure.is_none());
        assert_eq!(f.cases[1].line, 6);
    }

    #[test]
    fn ascii_rows() {
        assert_eq!(
            parse_ascii("1\x1fa\x1e2\x1fNULL\x1e"),
            vec![vec!["1", "a"], vec!["2", "NULL"]]
        );
    }
}
