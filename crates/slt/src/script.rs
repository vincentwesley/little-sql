//! Parser for sqllogictest script files.
//!
//! The format is documented at
//! <https://www.sqlite.org/sqllogictest/doc/trunk/about.wiki>: records are
//! separated by blank lines; each is a `statement`, a `query`, or a control
//! line, optionally preceded by `skipif` / `onlyif` conditions.

#[derive(Debug, Clone, PartialEq)]
pub struct Cond {
    /// `skipif` when true, `onlyif` when false.
    pub skip: bool,
    pub db: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Sort {
    None,
    Row,
    Value,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expected {
    Values(Vec<String>),
    Hash { count: usize, md5: String },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Statement {
        expect_ok: bool,
    },
    Query {
        types: String,
        sort: Sort,
        expected: Expected,
    },
    HashThreshold(usize),
    Halt,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    /// 1-based line number of the record's first line.
    pub line: usize,
    pub conds: Vec<Cond>,
    pub kind: Kind,
    pub sql: String,
}

impl Record {
    /// Whether the record applies to an engine that identifies as SQLite.
    pub fn applies(&self) -> bool {
        self.conds.iter().all(|c| (c.db == "sqlite") != c.skip)
    }
}

pub fn parse(text: &str) -> Vec<Record> {
    let lines: Vec<&str> = text.lines().map(|l| l.trim_end()).collect();
    let mut out = Vec::new();
    let mut conds = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if line.is_empty() || line.starts_with('#') {
            i += 1;
            continue;
        }
        let start = i + 1;
        let mut words = line.split_whitespace();
        match words.next() {
            Some(w @ ("skipif" | "onlyif")) => {
                conds.push(Cond {
                    skip: w == "skipif",
                    db: words.next().unwrap_or("").to_string(),
                });
                i += 1;
            }
            Some("hash-threshold") => {
                let n = words.next().and_then(|w| w.parse().ok()).unwrap_or(0);
                out.push(Record {
                    line: start,
                    conds: std::mem::take(&mut conds),
                    kind: Kind::HashThreshold(n),
                    sql: String::new(),
                });
                i += 1;
            }
            Some("halt") => {
                out.push(Record {
                    line: start,
                    conds: std::mem::take(&mut conds),
                    kind: Kind::Halt,
                    sql: String::new(),
                });
                i += 1;
            }
            Some("statement") => {
                let expect_ok = words.next() != Some("error");
                i += 1;
                let mut sql = Vec::new();
                while i < lines.len() && !lines[i].is_empty() {
                    sql.push(lines[i]);
                    i += 1;
                }
                out.push(Record {
                    line: start,
                    conds: std::mem::take(&mut conds),
                    kind: Kind::Statement { expect_ok },
                    sql: sql.join("\n"),
                });
            }
            Some("query") => {
                let types = words.next().unwrap_or("").to_string();
                let sort = match words.next() {
                    Some("rowsort") => Sort::Row,
                    Some("valuesort") => Sort::Value,
                    _ => Sort::None,
                };
                i += 1;
                let mut sql = Vec::new();
                while i < lines.len() && !lines[i].is_empty() && lines[i] != "----" {
                    sql.push(lines[i]);
                    i += 1;
                }
                let mut values = Vec::new();
                if i < lines.len() && lines[i] == "----" {
                    i += 1;
                    while i < lines.len() && !lines[i].is_empty() {
                        values.push(lines[i].to_string());
                        i += 1;
                    }
                }
                let expected = match values.as_slice() {
                    [one] if one.contains(" values hashing to ") => {
                        let mut p = one.split(' ');
                        let count = p.next().and_then(|n| n.parse().ok()).unwrap_or(0);
                        Expected::Hash {
                            count,
                            md5: p.last().unwrap_or("").to_string(),
                        }
                    }
                    _ => Expected::Values(values),
                };
                out.push(Record {
                    line: start,
                    conds: std::mem::take(&mut conds),
                    kind: Kind::Query {
                        types,
                        sort,
                        expected,
                    },
                    sql: sql.join("\n"),
                });
            }
            _ => i += 1,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_record_kinds() {
        let text = "# comment\nhash-threshold 8\n\nstatement ok\nCREATE TABLE t(a)\n\nskipif sqlite\nstatement error\nBAD\n\n\
                    onlyif mysql\nquery I nosort\nSELECT 1\n----\n1\n\nquery IT rowsort label-1\nSELECT a,\n b FROM t\n----\n30 values hashing to abc\n\nhalt\n";
        let r = parse(text);
        assert_eq!(r.len(), 6);
        assert_eq!(r[0].kind, Kind::HashThreshold(8));
        assert_eq!(r[1].sql, "CREATE TABLE t(a)");
        assert!(r[1].applies() && !r[2].applies() && !r[3].applies());
        match &r[4].kind {
            Kind::Query {
                types,
                sort,
                expected,
            } => {
                assert_eq!(types, "IT");
                assert_eq!(*sort, Sort::Row);
                assert_eq!(
                    *expected,
                    Expected::Hash {
                        count: 30,
                        md5: "abc".into()
                    }
                );
            }
            k => panic!("{k:?}"),
        }
        assert_eq!(r[4].sql, "SELECT a,\n b FROM t");
        assert_eq!(r[4].line, 16);
        assert_eq!(r[5].kind, Kind::Halt);
    }
}
