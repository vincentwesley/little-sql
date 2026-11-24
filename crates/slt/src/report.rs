//! Aggregation and output of test results (text table and JSON).

use crate::run::{FileResult, Tally};
use std::collections::BTreeMap;
use std::fmt::Write;

pub struct Summary {
    pub suite: String,
    pub mode: String,
    pub sqlite_version: String,
    pub files: usize,
    pub total: Tally,
    pub categories: BTreeMap<String, (usize, Tally)>,
    pub seconds: f64,
}

pub fn summarize(
    suite: &str,
    mode: &str,
    sqlite_version: &str,
    results: &[FileResult],
    seconds: f64,
) -> Summary {
    let mut categories: BTreeMap<String, (usize, Tally)> = BTreeMap::new();
    let mut total = Tally::default();
    for r in results {
        let e = categories.entry(r.category.clone()).or_default();
        e.0 += 1;
        e.1.add(&r.tally);
        total.add(&r.tally);
    }
    Summary {
        suite: suite.to_string(),
        mode: mode.to_string(),
        sqlite_version: sqlite_version.to_string(),
        files: results.len(),
        total,
        categories,
        seconds,
    }
}

pub fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn tally_json(t: &Tally) -> String {
    format!(
        "\"pass\": {}, \"fail\": {}, \"scored\": {}, \"pass_rate\": {:.2}, \"not_applicable\": {}, \"setup\": {}",
        t.pass,
        t.fail,
        t.scored(),
        t.rate_floor(),
        t.not_applicable,
        t.setup
    )
}

impl Summary {
    pub fn to_json(&self) -> String {
        let mut s = String::from("{\n");
        let _ = writeln!(s, "  \"suite\": {},", json_string(&self.suite));
        let _ = writeln!(s, "  \"mode\": {},", json_string(&self.mode));
        let _ = writeln!(
            s,
            "  \"engine\": {},",
            json_string(concat!("littlesql ", env!("CARGO_PKG_VERSION")))
        );
        let _ = writeln!(
            s,
            "  \"reference\": {},",
            json_string(&format!("sqlite3 {}", self.sqlite_version))
        );
        let _ = writeln!(s, "  \"files\": {},", self.files);
        let _ = writeln!(s, "  \"total\": {{ {} }},", tally_json(&self.total));
        s.push_str("  \"categories\": [\n");
        let n = self.categories.len();
        for (i, (name, (files, t))) in self.categories.iter().enumerate() {
            let _ = writeln!(
                s,
                "    {{ \"name\": {}, \"files\": {}, {} }}{}",
                json_string(name),
                files,
                tally_json(t),
                if i + 1 < n { "," } else { "" }
            );
        }
        s.push_str("  ]\n}\n");
        s
    }

    pub fn to_text(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(
            s,
            "{} — {} mode, reference sqlite3 {}",
            self.suite, self.mode, self.sqlite_version
        );
        let _ = writeln!(
            s,
            "{:<24} {:>6} {:>10} {:>10} {:>9} {:>8} {:>8}",
            "category", "files", "pass", "fail", "rate", "n/a", "setup"
        );
        for (name, (files, t)) in &self.categories {
            let _ = writeln!(
                s,
                "{:<24} {:>6} {:>10} {:>10} {:>8.2}% {:>8} {:>8}",
                name,
                files,
                t.pass,
                t.fail,
                t.rate_floor(),
                t.not_applicable,
                t.setup
            );
        }
        let t = &self.total;
        let _ = writeln!(
            s,
            "{:<24} {:>6} {:>10} {:>10} {:>8.2}% {:>8} {:>8}",
            "TOTAL",
            self.files,
            t.pass,
            t.fail,
            t.rate_floor(),
            t.not_applicable,
            t.setup
        );
        let _ = writeln!(s, "{:.1} s", self.seconds);
        s
    }
}
