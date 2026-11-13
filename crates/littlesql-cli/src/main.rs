//! `littlesql`: an interactive shell for Little SQL, modelled on the
//! `sqlite3` command-line tool.
//!
//! ```text
//! littlesql [OPTIONS] [FILE] [SQL]
//! ```
//!
//! With SQL on the command line it runs it and exits; otherwise it reads
//! statements from standard input (with a prompt when that is a terminal).

use littlesql::plan::render_plan;
use littlesql::{Database, Limits, ResultSet, Value};
use std::io::{BufRead, IsTerminal, Write};
use std::time::Instant;

const HELP: &str = "\
Usage: littlesql [OPTIONS] [FILE] [SQL]

Open FILE (a SQLite database) and run SQL, or start an interactive shell.
Without FILE an empty in-memory database is used.

Options:
  -readonly          open the file read-only
  -header / -noheader  show or hide column names
  -list -csv -tabs -line -json -column -table -box -markdown
                     output mode (default: list)
  -separator SEP     field separator for list mode (default: |)
  -nullvalue TEXT    text shown for NULL (default: empty)
  -cmd COMMAND       run COMMAND before reading input
  -eqp               print the query plan before each result
  -bail              stop at the first error
  -version           print the version and exit
  -help              show this message
";

const DOT_HELP: &str = "\
.bail on|off          Stop after hitting an error
.databases            Show the open database file
.eqp on|off           Show the query plan (with row counts) after each query
.exit / .quit         Leave the shell
.headers on|off       Show or hide column names
.help                 Show this message
.indexes ?TABLE?      List indexes
.mode MODE            list, csv, tabs, line, json, column, table, box, markdown
.nullvalue TEXT       Text shown for NULL
.open FILE            Close this database and open FILE
.read FILE            Run the SQL in FILE
.schema ?PATTERN?     Show CREATE statements
.separator SEP        Field separator for list mode
.stats on|off         Show pages read and rows visited after each statement
.tables ?PATTERN?     List tables and views
.timer on|off         Show how long each statement took
.limit steps N        Stop statements after visiting N rows (0 = no limit)
.limit millis N       Stop statements after N milliseconds (0 = no limit)
";

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    List,
    Csv,
    Tabs,
    Line,
    Json,
    Column,
    Table,
    Box,
    Markdown,
}

impl Mode {
    fn parse(s: &str) -> Option<Mode> {
        Some(match s {
            "list" => Mode::List,
            "csv" => Mode::Csv,
            "tabs" => Mode::Tabs,
            "line" => Mode::Line,
            "json" => Mode::Json,
            "column" => Mode::Column,
            "table" => Mode::Table,
            "box" => Mode::Box,
            "markdown" => Mode::Markdown,
            _ => return None,
        })
    }
}

struct Shell {
    db: Database,
    path: String,
    readonly: bool,
    mode: Mode,
    headers: bool,
    separator: String,
    null: String,
    eqp: bool,
    stats: bool,
    timer: bool,
    bail: bool,
    limits: Limits,
    errors: usize,
    out: Box<dyn Write>,
}

/// What to do after a line of input.
enum Flow {
    Continue,
    Quit,
}

fn open(path: &str, readonly: bool) -> Result<Database, String> {
    if path.is_empty() || path == ":memory:" {
        return Ok(Database::memory());
    }
    let r = if readonly {
        Database::open_readonly(path)
    } else {
        Database::open(path)
    };
    r.map_err(|e| format!("cannot open {path}: {e}"))
}

impl Shell {
    fn print_error(&mut self, msg: &str) {
        self.errors += 1;
        let _ = self.out.flush();
        eprintln!("Error: {msg}");
    }

    /// Run one or more SQL statements, printing each result.
    fn run_sql(&mut self, sql: &str) -> bool {
        self.db.set_limits(self.limits);
        let started = Instant::now();
        // Statements run one at a time so earlier results print even if a
        // later one fails.
        let stmts = match littlesql::sql::parser::parse_statements(sql) {
            Ok(s) => s,
            Err(e) => {
                self.print_error(&e.to_string());
                return false;
            }
        };
        for (_, text) in stmts {
            let t0 = Instant::now();
            match self.db.query(&text) {
                Ok(rs) => {
                    self.print_result(&rs);
                    if self.stats {
                        let rows: u64 = rs.stats.iter().map(|s| s.rows).sum();
                        let _ = writeln!(
                            self.out,
                            "Pages read: {}   Rows visited: {}   Rows changed: {}",
                            rs.page_reads, rows, rs.changes
                        );
                    }
                    if self.timer {
                        let _ = writeln!(self.out, "Run Time: {:.3} s", t0.elapsed().as_secs_f64());
                    }
                }
                Err(e) => {
                    self.print_error(&e.to_string());
                    return false;
                }
            }
        }
        let _ = started;
        let _ = self.out.flush();
        true
    }

    fn print_result(&mut self, rs: &ResultSet) {
        if rs.is_explain {
            let with_stats = rs.stats.iter().any(|s| s.loops > 0);
            let text = render_plan(&rs.plan, with_stats.then_some(rs.stats.as_slice()));
            let _ = write!(self.out, "{text}");
            return;
        }
        if self.eqp && !rs.plan.is_empty() {
            let _ = write!(self.out, "{}", rs.plan_text());
        }
        if rs.columns.is_empty() {
            return;
        }
        let text = |v: &Value, null: &str| -> String {
            match v {
                Value::Null => null.to_string(),
                Value::Blob(b) => String::from_utf8_lossy(b).into_owned(),
                other => other.to_text(),
            }
        };
        let null = self.null.clone();
        let out = &mut self.out;
        match self.mode {
            Mode::List | Mode::Tabs => {
                let sep = if self.mode == Mode::Tabs {
                    "\t"
                } else {
                    self.separator.as_str()
                };
                if self.headers {
                    let _ = writeln!(out, "{}", rs.columns.join(sep));
                }
                for row in &rs.rows {
                    let cells: Vec<String> = row.iter().map(|v| text(v, &null)).collect();
                    let _ = writeln!(out, "{}", cells.join(sep));
                }
            }
            Mode::Csv => {
                let quote = |s: &str| {
                    if s.contains([',', '"', '\n', '\r']) {
                        format!("\"{}\"", s.replace('"', "\"\""))
                    } else {
                        s.to_string()
                    }
                };
                if self.headers {
                    let h: Vec<String> = rs.columns.iter().map(|c| quote(c)).collect();
                    let _ = writeln!(out, "{}", h.join(","));
                }
                for row in &rs.rows {
                    let cells: Vec<String> = row.iter().map(|v| quote(&text(v, &null))).collect();
                    let _ = writeln!(out, "{}", cells.join(","));
                }
            }
            Mode::Line => {
                let w = rs
                    .columns
                    .iter()
                    .map(|c| c.chars().count())
                    .max()
                    .unwrap_or(0);
                for (i, row) in rs.rows.iter().enumerate() {
                    if i > 0 {
                        let _ = writeln!(out);
                    }
                    for (c, v) in rs.columns.iter().zip(row) {
                        let _ = writeln!(out, "{c:>w$} = {}", text(v, &null));
                    }
                }
            }
            Mode::Json => {
                let _ = write!(out, "[");
                for (i, row) in rs.rows.iter().enumerate() {
                    let _ = write!(out, "{}{{", if i == 0 { "" } else { ",\n" });
                    for (j, (c, v)) in rs.columns.iter().zip(row).enumerate() {
                        let val = match v {
                            Value::Null => "null".to_string(),
                            Value::Int(_) | Value::Real(_) => v.to_text(),
                            Value::Text(t) => json_string(t),
                            Value::Blob(b) => json_string(
                                &b.iter().map(|x| format!("{x:02x}")).collect::<String>(),
                            ),
                        };
                        let _ = write!(
                            out,
                            "{}{}:{}",
                            if j == 0 { "" } else { "," },
                            json_string(c),
                            val
                        );
                    }
                    let _ = write!(out, "}}");
                }
                let _ = writeln!(out, "]");
            }
            Mode::Column | Mode::Table | Mode::Box | Mode::Markdown => {
                let cells: Vec<Vec<String>> = rs
                    .rows
                    .iter()
                    .map(|r| {
                        r.iter()
                            .map(|v| text(v, &null).replace('\n', " "))
                            .collect()
                    })
                    .collect();
                let numeric: Vec<bool> = (0..rs.columns.len())
                    .map(|i| {
                        rs.rows
                            .iter()
                            .all(|r| matches!(r[i], Value::Int(_) | Value::Real(_) | Value::Null))
                            && !rs.rows.is_empty()
                    })
                    .collect();
                let mut width: Vec<usize> = rs.columns.iter().map(|c| c.chars().count()).collect();
                for r in &cells {
                    for (i, c) in r.iter().enumerate() {
                        width[i] = width[i].max(c.chars().count());
                    }
                }
                let pad = |s: &str, i: usize| -> String {
                    let n = width[i].saturating_sub(s.chars().count());
                    if numeric[i] && self.mode != Mode::Column {
                        format!("{}{s}", " ".repeat(n))
                    } else {
                        format!("{s}{}", " ".repeat(n))
                    }
                };
                let rule = |l: &str, m: &str, r: &str, fill: &str| -> String {
                    let parts: Vec<String> = width.iter().map(|w| fill.repeat(w + 2)).collect();
                    format!("{l}{}{r}", parts.join(m))
                };
                match self.mode {
                    Mode::Column => {
                        let line = |v: Vec<String>| v.join("  ").trim_end().to_string();
                        if self.headers {
                            let h = rs
                                .columns
                                .iter()
                                .enumerate()
                                .map(|(i, c)| pad(c, i))
                                .collect();
                            let _ = writeln!(out, "{}", line(h));
                            let d = width.iter().map(|w| "-".repeat(*w)).collect();
                            let _ = writeln!(out, "{}", line(d));
                        }
                        for r in &cells {
                            let v = r.iter().enumerate().map(|(i, c)| pad(c, i)).collect();
                            let _ = writeln!(out, "{}", line(v));
                        }
                    }
                    Mode::Markdown => {
                        let row = |v: Vec<String>| format!("| {} |", v.join(" | "));
                        let h = rs
                            .columns
                            .iter()
                            .enumerate()
                            .map(|(i, c)| pad(c, i))
                            .collect();
                        let _ = writeln!(out, "{}", row(h));
                        let d = width
                            .iter()
                            .zip(&numeric)
                            .map(|(w, n)| {
                                if *n {
                                    format!("{}:", "-".repeat(w.saturating_sub(1)))
                                } else {
                                    "-".repeat(*w)
                                }
                            })
                            .collect();
                        let _ = writeln!(out, "{}", row(d));
                        for r in &cells {
                            let v = r.iter().enumerate().map(|(i, c)| pad(c, i)).collect();
                            let _ = writeln!(out, "{}", row(v));
                        }
                    }
                    _ => {
                        let boxy = self.mode == Mode::Box;
                        let (v, top, mid, bot, fill) = if boxy {
                            ("│", ["┌", "┬", "┐"], ["├", "┼", "┤"], ["└", "┴", "┘"], "─")
                        } else {
                            ("|", ["+", "+", "+"], ["+", "+", "+"], ["+", "+", "+"], "-")
                        };
                        let row = |cells: Vec<String>| {
                            format!("{v} {} {v}", cells.join(&format!(" {v} ")))
                        };
                        let _ = writeln!(out, "{}", rule(top[0], top[1], top[2], fill));
                        let h = rs
                            .columns
                            .iter()
                            .enumerate()
                            .map(|(i, c)| pad(c, i))
                            .collect();
                        let _ = writeln!(out, "{}", row(h));
                        let _ = writeln!(out, "{}", rule(mid[0], mid[1], mid[2], fill));
                        for r in &cells {
                            let c = r.iter().enumerate().map(|(i, c)| pad(c, i)).collect();
                            let _ = writeln!(out, "{}", row(c));
                        }
                        let _ = writeln!(out, "{}", rule(bot[0], bot[1], bot[2], fill));
                    }
                }
            }
        }
    }

    fn query_rows(&self, sql: &str) -> Result<Vec<Vec<Value>>, String> {
        self.db
            .query(sql)
            .map(|rs| rs.rows)
            .map_err(|e| e.to_string())
    }

    fn like_pattern(arg: Option<&str>) -> String {
        match arg {
            Some(p) => format!(" AND name LIKE '{}'", p.replace('\'', "''")),
            None => String::new(),
        }
    }

    fn dot(&mut self, line: &str) -> Flow {
        let args = split_args(line);
        let cmd = args.first().map(String::as_str).unwrap_or("");
        let arg1 = args.get(1).map(String::as_str);
        let on = |a: Option<&str>| matches!(a, Some("on" | "1" | "yes" | "true"));
        match cmd {
            ".quit" | ".exit" | ".q" => return Flow::Quit,
            ".help" => {
                let _ = write!(self.out, "{DOT_HELP}");
            }
            ".tables" | ".table" => {
                let sql = format!(
                    "SELECT name FROM sqlite_schema WHERE type IN ('table', 'view') \
                     AND name NOT LIKE 'sqlite_%'{} ORDER BY name",
                    Self::like_pattern(arg1)
                );
                match self.query_rows(&sql) {
                    Ok(rows) => {
                        let names: Vec<String> = rows.iter().map(|r| r[0].to_text()).collect();
                        print_columns(&mut self.out, &names);
                    }
                    Err(e) => self.print_error(&e),
                }
            }
            ".indexes" | ".indices" => {
                let filter = match arg1 {
                    Some(t) => format!(" AND tbl_name LIKE '{}'", t.replace('\'', "''")),
                    None => String::new(),
                };
                let sql = format!(
                    "SELECT name FROM sqlite_schema WHERE type = 'index'{filter} ORDER BY name"
                );
                match self.query_rows(&sql) {
                    Ok(rows) => {
                        let names: Vec<String> = rows.iter().map(|r| r[0].to_text()).collect();
                        print_columns(&mut self.out, &names);
                    }
                    Err(e) => self.print_error(&e),
                }
            }
            ".schema" => {
                let filter = match arg1 {
                    Some(p) => {
                        let p = p.replace('\'', "''");
                        format!(" AND (name LIKE '{p}' OR tbl_name LIKE '{p}')")
                    }
                    None => String::new(),
                };
                let sql = format!(
                    "SELECT sql FROM sqlite_schema WHERE sql IS NOT NULL \
                     AND name NOT LIKE 'sqlite_%'{filter} ORDER BY rowid"
                );
                match self.query_rows(&sql) {
                    Ok(rows) => {
                        for r in rows {
                            let _ = writeln!(self.out, "{};", r[0].to_text());
                        }
                    }
                    Err(e) => self.print_error(&e),
                }
            }
            ".databases" => {
                let _ = writeln!(
                    self.out,
                    "main: {} {}",
                    if self.path.is_empty() {
                        "\"\""
                    } else {
                        &self.path
                    },
                    if self.readonly { "r/o" } else { "r/w" }
                );
            }
            ".mode" => match arg1.and_then(Mode::parse) {
                Some(m) => {
                    self.mode = m;
                    if matches!(m, Mode::Column | Mode::Table | Mode::Box | Mode::Markdown) {
                        self.headers = true;
                    }
                }
                None => self.print_error(
                    "mode should be one of: list csv tabs line json column table box markdown",
                ),
            },
            ".headers" | ".header" => self.headers = on(arg1),
            ".eqp" => self.eqp = on(arg1),
            ".stats" => self.stats = on(arg1),
            ".timer" => self.timer = on(arg1),
            ".bail" => self.bail = on(arg1),
            ".nullvalue" => self.null = arg1.unwrap_or("").to_string(),
            ".separator" => self.separator = arg1.unwrap_or("|").to_string(),
            ".limit" => {
                let n: u64 = args.get(2).and_then(|n| n.parse().ok()).unwrap_or(0);
                match arg1 {
                    Some("steps") => self.limits.max_steps = n,
                    Some("millis") => self.limits.max_millis = n,
                    _ => {
                        let _ = writeln!(
                            self.out,
                            "steps {}\nmillis {}",
                            self.limits.max_steps, self.limits.max_millis
                        );
                    }
                }
            }
            ".open" => match arg1 {
                Some(p) => match open(p, self.readonly) {
                    Ok(db) => {
                        let _ = self.db.close();
                        self.db = db;
                        self.path = p.to_string();
                    }
                    Err(e) => self.print_error(&e),
                },
                None => self.print_error("usage: .open FILE"),
            },
            ".read" => match arg1 {
                Some(p) => match std::fs::read_to_string(p) {
                    Ok(text) => {
                        return self.run_script(&text);
                    }
                    Err(e) => self.print_error(&format!("cannot read {p}: {e}")),
                },
                None => self.print_error("usage: .read FILE"),
            },
            _ => self.print_error(&format!(
                "unknown command or invalid arguments: \"{}\". Enter \".help\" for help",
                cmd.trim_start_matches('.')
            )),
        }
        Flow::Continue
    }

    /// Run a whole script (as from `.read` or a pipe): dot commands on
    /// their own lines, SQL statements ending with `;`.
    fn run_script(&mut self, text: &str) -> Flow {
        let mut buf = String::new();
        for line in text.lines() {
            match self.feed(&mut buf, line) {
                Some(Flow::Quit) => return Flow::Quit,
                Some(Flow::Continue) | None => {}
            }
        }
        if !buf.trim().is_empty() {
            let sql = std::mem::take(&mut buf);
            if !self.run_sql(&sql) && self.bail {
                return Flow::Quit;
            }
        }
        Flow::Continue
    }

    /// Add one line of input. Returns `None` while a statement is still
    /// incomplete.
    fn feed(&mut self, buf: &mut String, line: &str) -> Option<Flow> {
        if buf.trim().is_empty() && line.trim_start().starts_with('.') {
            buf.clear();
            return Some(self.dot(line.trim()));
        }
        buf.push_str(line);
        buf.push('\n');
        if littlesql::sql_is_complete(buf) {
            let sql = std::mem::take(buf);
            let ok = self.run_sql(&sql);
            if !ok && self.bail {
                return Some(Flow::Quit);
            }
            return Some(Flow::Continue);
        }
        if buf.trim().is_empty() {
            buf.clear();
            return Some(Flow::Continue);
        }
        None
    }
}

fn json_string(s: &str) -> String {
    let mut out = String::from("\"");
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

/// Print names in columns, as sqlite3's `.tables` does.
fn print_columns(out: &mut Box<dyn Write>, names: &[String]) {
    if names.is_empty() {
        return;
    }
    let w = names.iter().map(|n| n.chars().count()).max().unwrap_or(0) + 2;
    let per_row = (80 / w).max(1);
    let rows = names.len().div_ceil(per_row);
    for r in 0..rows {
        let mut line = String::new();
        for c in 0..per_row {
            if let Some(n) = names.get(c * rows + r) {
                line.push_str(&format!("{n:<w$}"));
            }
        }
        let _ = writeln!(out, "{}", line.trim_end());
    }
}

/// Split a dot command into words, honouring single and double quotes.
fn split_args(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    for c in line.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                started = true;
            }
            None if c.is_whitespace() => {
                if started {
                    out.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            None => {
                cur.push(c);
                started = true;
            }
        }
    }
    if started {
        out.push(cur);
    }
    out
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut positional = Vec::new();
    let mut readonly = false;
    let mut mode = Mode::List;
    let mut headers: Option<bool> = None;
    let mut separator = "|".to_string();
    let mut null = String::new();
    let mut cmds = Vec::new();
    let mut eqp = false;
    let mut bail = false;
    let mut i = 0;
    while i < argv.len() {
        let a = argv[i].as_str();
        let opt = a.strip_prefix("--").or_else(|| a.strip_prefix('-'));
        let value = |i: &mut usize| -> String {
            *i += 1;
            match argv.get(*i) {
                Some(v) => v.clone(),
                None => {
                    eprintln!("Error: missing argument to {a}");
                    std::process::exit(1);
                }
            }
        };
        match opt {
            Some("help" | "h") => {
                print!("{HELP}");
                return;
            }
            Some("version") => {
                println!("littlesql {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            Some("readonly") => readonly = true,
            Some("header" | "headers") => headers = Some(true),
            Some("noheader" | "noheaders") => headers = Some(false),
            Some("eqp") => eqp = true,
            Some("bail") => bail = true,
            Some("separator") => separator = value(&mut i),
            Some("nullvalue") => null = value(&mut i),
            Some("cmd") => cmds.push(value(&mut i)),
            Some(m) if Mode::parse(m).is_some() => mode = Mode::parse(m).unwrap(),
            Some(o) if !o.is_empty() && !a.contains(' ') && positional.is_empty() => {
                eprintln!("Error: unknown option: {a}\nUse -help for a list of options.");
                std::process::exit(1);
            }
            _ => positional.push(a.to_string()),
        }
        i += 1;
    }
    let path = positional.first().cloned().unwrap_or_default();
    let db = match open(&path, readonly) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    };
    let table_like = matches!(
        mode,
        Mode::Column | Mode::Table | Mode::Box | Mode::Markdown
    );
    let mut shell = Shell {
        db,
        path,
        readonly,
        mode,
        headers: headers.unwrap_or(table_like),
        separator,
        null,
        eqp,
        stats: false,
        timer: false,
        bail,
        limits: Limits::default(),
        errors: 0,
        out: Box::new(std::io::BufWriter::new(std::io::stdout())),
    };
    for c in &cmds {
        if let Flow::Quit = shell.run_script(c) {
            return;
        }
    }
    let mut failed = false;
    if positional.len() > 1 {
        for sql in &positional[1..] {
            let ok = if sql.trim_start().starts_with('.') {
                matches!(shell.dot(sql.trim()), Flow::Continue)
            } else {
                shell.run_sql(sql)
            };
            if !ok {
                failed = true;
                if bail {
                    break;
                }
            }
        }
        let _ = shell.out.flush();
        let _ = shell.db.close();
        std::process::exit(if failed { 1 } else { 0 });
    }
    let interactive = std::io::stdin().is_terminal();
    if interactive {
        println!(
            "Little SQL {}\nEnter \".help\" for usage hints.",
            env!("CARGO_PKG_VERSION")
        );
        if shell.path.is_empty() {
            println!("Connected to a transient in-memory database.");
        }
    }
    let stdin = std::io::stdin();
    let mut buf = String::new();
    let mut lines = stdin.lock().lines();
    loop {
        if interactive {
            let prompt = if buf.trim().is_empty() {
                "littlesql> "
            } else {
                "      ...> "
            };
            print!("{prompt}");
            let _ = std::io::stdout().flush();
        }
        let Some(Ok(line)) = lines.next() else { break };
        if let Some(Flow::Quit) = shell.feed(&mut buf, &line) {
            break;
        }
    }
    if !buf.trim().is_empty() {
        shell.run_sql(&buf);
    }
    let _ = shell.out.flush();
    if let Err(e) = shell.db.close() {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
    if !interactive && shell.errors > 0 {
        std::process::exit(1);
    }
}
