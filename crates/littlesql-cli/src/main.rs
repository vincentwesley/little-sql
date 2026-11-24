//! `littlesql`: an interactive shell for Little SQL, modelled on the
//! `sqlite3` command-line tool.
//!
//! ```text
//! littlesql [OPTIONS] [FILE] [SQL]
//! ```
//!
//! With SQL on the command line it runs it and exits; otherwise it reads
//! statements from standard input (with a prompt when that is a terminal).

mod dump;

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
.databases            List the database files (main, temp, attached)
.dump ?PATTERN?       Write the database (or matching tables) as SQL text
.eqp on|off           Show the query plan (with row counts) after each query
.exit / .quit         Leave the shell
.headers on|off       Show or hide column names
.help                 Show this message
.indexes ?TABLE?      List indexes
.mode MODE            list, csv, tabs, line, json, column, table, box, markdown
.nullvalue TEXT       Text shown for NULL
.parameter set NAME VALUE  Bind ?N, :name, @name or $name (also unset/list/clear)
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
    /// `.parameter set` values: `?N`, `:name`, `@name` or `$name`.
    param_list: Vec<(String, Value)>,
    errors: usize,
    out: Box<dyn Write>,
    /// How SQL errors are reported, as sqlite3 does for each input kind.
    style: ErrStyle,
    /// Lines read so far from the current script, and the line on which
    /// the pending statement buffer starts.
    lineno: usize,
    buf_line: usize,
}

/// Where SQL came from, which decides how sqlite3 words its errors.
#[derive(Clone, Copy, PartialEq)]
enum ErrStyle {
    /// A command-line argument or `-cmd`: `Error: in prepare, ...`.
    Arg,
    /// Typed at the prompt: `Parse error: ...`.
    Interactive,
    /// Piped in or `.read`: `Parse error near line N: ...`.
    Script,
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

    /// Report an error from running SQL, worded as sqlite3 words it:
    /// errors found while preparing a statement are parse errors, the rest
    /// runtime errors. `line` is where the statement starts.
    fn print_sql_error(&mut self, e: &littlesql::Error, line: usize) {
        use littlesql::Error as E;
        self.errors += 1;
        let _ = self.out.flush();
        let prepare = matches!(e, E::Parse(_) | E::Sql(_) | E::Unsupported(_));
        // sqlite3 adds the result code when it is not plain SQLITE_ERROR.
        let code = match e {
            E::Constraint(_) | E::Raise(..) => " (19)",
            E::Corrupt(_) => " (11)",
            E::Io(_) => " (10)",
            _ => "",
        };
        match (self.style, prepare) {
            (ErrStyle::Arg, true) => eprintln!("Error: in prepare, {e}"),
            (ErrStyle::Arg, false) => eprintln!("Error: stepping, {e}{code}"),
            (ErrStyle::Interactive, true) => eprintln!("Parse error: {e}"),
            (ErrStyle::Interactive, false) => eprintln!("Runtime error: {e}{code}"),
            (ErrStyle::Script, true) => eprintln!("Parse error near line {line}: {e}"),
            (ErrStyle::Script, false) => {
                eprintln!("Runtime error near line {line}: {e}{code}")
            }
        }
    }

    /// Run one or more SQL statements, printing each result.
    fn run_sql(&mut self, sql: &str) -> bool {
        self.db.set_limits(self.limits);
        let started = Instant::now();
        // Statements run one at a time so earlier results print even if a
        // later one fails.
        let first_line = self.buf_line.max(1);
        let stmts = match littlesql::split_statements(sql) {
            Ok(s) => s,
            Err(e) => {
                self.print_sql_error(&e, first_line);
                return false;
            }
        };
        let mut pos = 0;
        for text in stmts {
            // The line the statement starts on, counted from the start of
            // the buffer.
            let start = sql[pos..].find(text.trim()).map_or(pos, |i| pos + i);
            pos = start + text.trim().len();
            let line = first_line + sql[..start].matches('\n').count();
            let t0 = Instant::now();
            // An engine bug must not take the shell down: report it as an
            // error and carry on (the statement's transaction rolls back).
            let params = self.params();
            let db = &self.db;
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                db.query_with(&text, &params)
            }))
            .unwrap_or_else(|p| {
                let msg = p
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_default();
                self.db.rollback_uncommitted();
                Err(littlesql::Error::Runtime(format!(
                    "internal error (please report it): {msg}"
                )))
            });
            match outcome {
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
                    self.print_sql_error(&e, line);
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
                    let mut line: Vec<u8> = Vec::new();
                    for (i, v) in row.iter().enumerate() {
                        if i > 0 {
                            line.extend_from_slice(sep.as_bytes());
                        }
                        line.extend_from_slice(&cell_bytes(v, &null));
                    }
                    line.push(b'\n');
                    let _ = out.write_all(&line);
                }
            }
            Mode::Csv => {
                let quote = |s: &[u8]| -> Vec<u8> {
                    if s.iter().any(|b| matches!(b, b',' | b'"' | b'\n' | b'\r')) {
                        let mut q = vec![b'"'];
                        for b in s {
                            if *b == b'"' {
                                q.push(b'"');
                            }
                            q.push(*b);
                        }
                        q.push(b'"');
                        q
                    } else {
                        s.to_vec()
                    }
                };
                if self.headers {
                    let h: Vec<Vec<u8>> = rs.columns.iter().map(|c| quote(c.as_bytes())).collect();
                    let _ = out.write_all(&h.join(&b","[..]));
                    let _ = out.write_all(b"\n");
                }
                for row in &rs.rows {
                    let cells: Vec<Vec<u8>> =
                        row.iter().map(|v| quote(&cell_bytes(v, &null))).collect();
                    let _ = out.write_all(&cells.join(&b","[..]));
                    let _ = out.write_all(b"\n");
                }
            }
            Mode::Line => {
                let w = rs
                    .columns
                    .iter()
                    .map(|c| c.chars().count())
                    .max()
                    .unwrap_or(0)
                    .max(5);
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
                let mut width: Vec<usize> = rs.columns.iter().map(|c| c.chars().count()).collect();
                for r in &cells {
                    for (i, c) in r.iter().enumerate() {
                        width[i] = width[i].max(c.chars().count());
                    }
                }
                // Like sqlite3: cells left-aligned, headers centred in the
                // boxed modes.
                let pad = |s: &str, i: usize| -> String {
                    let n = width[i].saturating_sub(s.chars().count());
                    format!("{s}{}", " ".repeat(n))
                };
                let centre = |s: &str, i: usize| -> String {
                    let n = width[i].saturating_sub(s.chars().count());
                    format!("{}{s}{}", " ".repeat(n / 2), " ".repeat(n - n / 2))
                };
                let rule = |l: &str, m: &str, r: &str, fill: &str| -> String {
                    let parts: Vec<String> = width.iter().map(|w| fill.repeat(w + 2)).collect();
                    format!("{l}{}{r}", parts.join(m))
                };
                let header: Vec<String> = rs
                    .columns
                    .iter()
                    .enumerate()
                    .map(|(i, c)| centre(c, i))
                    .collect();
                match self.mode {
                    Mode::Column => {
                        if self.headers {
                            let h: Vec<String> = rs
                                .columns
                                .iter()
                                .enumerate()
                                .map(|(i, c)| pad(c, i))
                                .collect();
                            let _ = writeln!(out, "{}", h.join("  "));
                            let d: Vec<String> = width.iter().map(|w| "-".repeat(*w)).collect();
                            let _ = writeln!(out, "{}", d.join("  "));
                        }
                        for r in &cells {
                            let v: Vec<String> =
                                r.iter().enumerate().map(|(i, c)| pad(c, i)).collect();
                            let _ = writeln!(out, "{}", v.join("  "));
                        }
                    }
                    Mode::Markdown => {
                        let row = |v: Vec<String>| format!("| {} |", v.join(" | "));
                        let _ = writeln!(out, "{}", row(header));
                        let _ = writeln!(out, "{}", rule("|", "|", "|", "-"));
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
                        let _ = writeln!(out, "{}", row(header));
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

    /// The `.parameter` values as statement parameters.
    fn params(&self) -> littlesql::Params {
        let mut p = littlesql::Params {
            names_only: true,
            ..Default::default()
        };
        for (k, v) in &self.param_list {
            match k.strip_prefix('?').and_then(|n| n.parse::<usize>().ok()) {
                Some(n) if n >= 1 => {
                    if p.positional.len() < n {
                        p.positional.resize(n, Value::Null);
                    }
                    p.positional[n - 1] = v.clone();
                }
                _ => p.named.push((k.clone(), v.clone())),
            }
        }
        p
    }

    fn query_rows(&self, sql: &str) -> Result<Vec<Vec<Value>>, String> {
        self.db
            .query(sql)
            .map(|rs| rs.rows)
            .map_err(|e| e.to_string())
    }

    /// The connection's files as (quoted name, prefix for listings):
    /// main, temp (once used) and attached files.
    fn databases(&self) -> Vec<(String, String)> {
        self.query_rows("PRAGMA database_list")
            .unwrap_or_default()
            .iter()
            .map(|r| {
                let name = r[1].to_text();
                let prefix = if name == "main" {
                    String::new()
                } else {
                    format!("{name}.")
                };
                (dump::quote_name(&name), prefix)
            })
            .collect()
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
                let mut names = Vec::new();
                for (db, prefix) in self.databases() {
                    let sql = format!(
                        "SELECT name FROM {db}.sqlite_schema WHERE type IN ('table', 'view') \
                         AND name NOT LIKE 'sqlite_%'{}",
                        Self::like_pattern(arg1)
                    );
                    match self.query_rows(&sql) {
                        Ok(rows) => {
                            names.extend(rows.iter().map(|r| format!("{prefix}{}", r[0].to_text())))
                        }
                        Err(e) => {
                            self.print_error(&e);
                            return Flow::Continue;
                        }
                    }
                }
                names.sort();
                print_columns(&mut self.out, &names);
            }
            ".indexes" | ".indices" => {
                let filter = match arg1 {
                    Some(t) => format!(" AND tbl_name LIKE '{}'", t.replace('\'', "''")),
                    None => String::new(),
                };
                let mut names = Vec::new();
                for (db, prefix) in self.databases() {
                    let sql =
                        format!("SELECT name FROM {db}.sqlite_schema WHERE type = 'index'{filter}");
                    match self.query_rows(&sql) {
                        Ok(rows) => {
                            names.extend(rows.iter().map(|r| format!("{prefix}{}", r[0].to_text())))
                        }
                        Err(e) => {
                            self.print_error(&e);
                            return Flow::Continue;
                        }
                    }
                }
                names.sort();
                print_columns(&mut self.out, &names);
            }
            ".schema" => {
                let filter = match arg1 {
                    Some(p) => {
                        let p = p.replace('\'', "''");
                        format!(" AND (name LIKE '{p}' OR tbl_name LIKE '{p}')")
                    }
                    None => String::new(),
                };
                for (db, prefix) in self.databases() {
                    let sql = format!(
                        "SELECT sql, type, name FROM {db}.sqlite_schema WHERE sql IS NOT NULL \
                         AND name NOT LIKE 'sqlite_%'{filter} ORDER BY rowid"
                    );
                    let rows = match self.query_rows(&sql) {
                        Ok(r) => r,
                        Err(e) => {
                            self.print_error(&e);
                            return Flow::Continue;
                        }
                    };
                    for r in rows {
                        let mut text = add_schema(&r[0].to_text(), &prefix);
                        // Like sqlite3, follow a view with its column names.
                        if r[1].to_text() == "view" {
                            let name = r[2].to_text();
                            let q =
                                format!("SELECT * FROM {db}.{} LIMIT 0", dump::quote_name(&name));
                            if let Ok(rs) = self.db.query(&q) {
                                let cols: Vec<String> =
                                    rs.columns.iter().map(|c| dump::quote_name(c)).collect();
                                text.push_str(&format!(
                                    "\n/* {prefix}{}({}) */",
                                    dump::quote_name(&name),
                                    cols.join(",")
                                ));
                            }
                        }
                        let _ = writeln!(self.out, "{text};");
                    }
                }
            }
            ".databases" => match self.query_rows("PRAGMA database_list") {
                Ok(rows) => {
                    for r in rows {
                        let file = r[2].to_text();
                        let name = r[1].to_text();
                        let ro = name == "main" && self.readonly;
                        let _ = writeln!(
                            self.out,
                            "{name}: {} {}",
                            if file.is_empty() { "\"\"" } else { &file },
                            if ro { "r/o" } else { "r/w" }
                        );
                    }
                }
                Err(e) => self.print_error(&e),
            },
            ".dump" => match dump::dump(&self.db, arg1) {
                Ok(text) => {
                    let _ = write!(self.out, "{text}");
                }
                Err(e) => self.print_error(&e),
            },
            ".mode" => match arg1.and_then(Mode::parse) {
                Some(m) => self.mode = m,
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
            ".parameter" | ".param" => match (arg1, args.get(2)) {
                (Some("set"), Some(name)) => {
                    // The value is the rest of the line, an SQL expression,
                    // as in sqlite3 (so quotes are kept).
                    let after_set = line.find("set").map_or("", |i| &line[i + 3..]).trim_start();
                    let value = after_set.strip_prefix(name.as_str()).unwrap_or("").trim();
                    if value.is_empty() {
                        self.print_error("usage: .parameter set NAME VALUE");
                    } else {
                        match self.db.query(&format!("SELECT {value}")) {
                            Ok(rs) => {
                                let v = rs.rows[0][0].clone();
                                self.param_list.retain(|(k, _)| k != name);
                                self.param_list.push((name.clone(), v));
                            }
                            Err(e) => self.print_error(&e.to_string()),
                        }
                    }
                }
                (Some("unset"), Some(name)) => self.param_list.retain(|(k, _)| k != name),
                (Some("clear") | Some("init"), _) => self.param_list.clear(),
                (Some("list"), _) | (None, _) => {
                    let w = self
                        .param_list
                        .iter()
                        .map(|(k, _)| k.len())
                        .max()
                        .unwrap_or(0);
                    for (k, v) in &self.param_list {
                        let _ = writeln!(self.out, "{k:<w$} {}", quote_value(v));
                    }
                }
                _ => {
                    self.print_error("usage: .parameter set NAME VALUE | unset NAME | list | clear")
                }
            },
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
        self.run_script_as(text, ErrStyle::Script)
    }

    fn run_script_as(&mut self, text: &str, style: ErrStyle) -> Flow {
        let saved = (self.style, self.lineno, self.buf_line);
        self.style = style;
        self.lineno = 0;
        let mut buf = String::new();
        let mut flow = Flow::Continue;
        for line in text.lines() {
            if let Some(Flow::Quit) = self.feed(&mut buf, line) {
                flow = Flow::Quit;
                break;
            }
        }
        if matches!(flow, Flow::Continue) && !buf.trim().is_empty() {
            let sql = std::mem::take(&mut buf);
            if !self.run_sql(&sql) && self.bail {
                flow = Flow::Quit;
            }
        }
        (self.style, self.lineno, self.buf_line) = saved;
        flow
    }

    /// Add one line of input. Returns `None` while a statement is still
    /// incomplete.
    fn feed(&mut self, buf: &mut String, line: &str) -> Option<Flow> {
        self.lineno += 1;
        if buf.trim().is_empty() {
            self.buf_line = self.lineno;
        }
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

/// A value as the sqlite3 shell writes it in list and CSV modes: the bytes
/// of its text form, up to the first NUL (the shell prints C strings), so
/// blobs come out raw.
fn cell_bytes(v: &Value, null: &str) -> Vec<u8> {
    let bytes = match v {
        Value::Null => return null.as_bytes().to_vec(),
        Value::Blob(b) => b.to_vec(),
        other => other.to_text().into_bytes(),
    };
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    bytes[..end].to_vec()
}

/// A value as an SQL literal.
fn quote_value(v: &Value) -> String {
    match v {
        Value::Null => "NULL".into(),
        Value::Text(t) => format!("'{}'", t.replace('\'', "''")),
        Value::Blob(b) => format!(
            "X'{}'",
            b.iter().map(|x| format!("{x:02X}")).collect::<String>()
        ),
        other => other.to_text(),
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

/// A stored CREATE statement shown for a database other than main: the
/// object's name gets the `db.` prefix, as sqlite3's `.schema` writes it.
fn add_schema(sql: &str, prefix: &str) -> String {
    if prefix.is_empty() {
        return sql.to_string();
    }
    let words = [
        "CREATE TABLE ",
        "CREATE VIEW ",
        "CREATE INDEX ",
        "CREATE UNIQUE INDEX ",
        "CREATE TRIGGER ",
    ];
    for w in words {
        if sql.len() >= w.len() && sql[..w.len()].eq_ignore_ascii_case(w) {
            return format!("{}{prefix}{}", &sql[..w.len()], &sql[w.len()..]);
        }
    }
    sql.to_string()
}

/// Print names in columns, as sqlite3's `.tables` does.
fn print_columns(out: &mut Box<dyn Write>, names: &[String]) {
    if names.is_empty() {
        return;
    }
    // sqlite3 pads every name to the longest and puts two spaces between
    // columns (keeping the padding at the end of a line).
    let w = names.iter().map(|n| n.chars().count()).max().unwrap_or(0);
    let per_row = (80 / (w + 2)).max(1);
    let rows = names.len().div_ceil(per_row);
    for r in 0..rows {
        let cells: Vec<String> = (0..per_row)
            .filter_map(|c| names.get(c * rows + r))
            .map(|n| format!("{n:<w$}"))
            .collect();
        let _ = writeln!(out, "{}", cells.join("  "));
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
    // Engine panics are caught per statement and reported as errors.
    std::panic::set_hook(Box::new(|_| {}));
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
    let mut shell = Shell {
        db,
        path,
        readonly,
        mode,
        headers: headers.unwrap_or(false),
        separator,
        null,
        eqp,
        stats: false,
        timer: false,
        bail,
        limits: Limits::default(),
        param_list: Vec::new(),
        errors: 0,
        out: Box::new(std::io::BufWriter::new(std::io::stdout())),
        style: ErrStyle::Arg,
        lineno: 0,
        buf_line: 0,
    };
    for c in &cmds {
        if let Flow::Quit = shell.run_script_as(c, ErrStyle::Arg) {
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
    shell.style = if interactive {
        ErrStyle::Interactive
    } else {
        ErrStyle::Script
    };
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
