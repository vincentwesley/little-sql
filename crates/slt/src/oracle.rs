//! A long-running `sqlite3` process used as the reference implementation.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

pub struct Oracle {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    seq: u64,
}

/// What sqlite3 printed for one statement.
pub struct Reply {
    pub ok: bool,
    pub lines: Vec<String>,
}

impl Oracle {
    /// Start sqlite3 on `db` (created if missing). `ascii` selects the
    /// unambiguous ASCII output mode used by the differential tests.
    pub fn spawn(db: &Path, ascii: bool) -> Result<Oracle, String> {
        // stderr is merged into stdout so that error messages arrive in
        // order with the completion markers.
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("exec sqlite3 -batch \"$0\" 2>&1")
            .arg(db)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot start sqlite3: {e}"))?;
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match reader.read_until(b'\n', &mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let line = String::from_utf8_lossy(&buf)
                            .trim_end_matches(['\n', '\r'])
                            .to_string();
                        if tx.send(line).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        let mut o = Oracle {
            child,
            stdin,
            lines: rx,
            seq: 0,
        };
        // Durability is irrelevant for a scratch database; speed is not.
        o.exec("PRAGMA synchronous=OFF")?;
        o.exec("PRAGMA journal_mode=MEMORY")?;
        if ascii {
            o.raw(".mode ascii\n.nullvalue NULL\n")?;
        }
        Ok(o)
    }

    fn raw(&mut self, text: &str) -> Result<(), String> {
        self.stdin
            .write_all(text.as_bytes())
            .map_err(|e| format!("sqlite3 went away: {e}"))
    }

    /// Run one statement and wait for it to finish.
    pub fn exec(&mut self, sql: &str) -> Result<Reply, String> {
        self.seq += 1;
        let marker = format!("__slt_done_{}__", self.seq);
        let sql = sql.trim().trim_end_matches(';');
        self.raw(&format!("{sql}\n;\n.print {marker}\n"))?;
        self.stdin
            .flush()
            .map_err(|e| format!("sqlite3 went away: {e}"))?;
        let mut reply = Reply {
            ok: true,
            lines: Vec::new(),
        };
        loop {
            match self.lines.recv_timeout(Duration::from_secs(120)) {
                Ok(line) => {
                    // In ASCII mode the marker follows the last record
                    // separator on the same line.
                    if let Some(rest) = line.strip_suffix(&marker) {
                        if !rest.is_empty() {
                            reply.lines.push(rest.to_string());
                        }
                        return Ok(reply);
                    }
                    if line.starts_with("Parse error")
                        || line.starts_with("Runtime error")
                        || line.starts_with("Error:")
                    {
                        reply.ok = false;
                    }
                    reply.lines.push(line);
                }
                Err(_) => {
                    let _ = self.child.kill();
                    return Err("sqlite3 did not answer within 120 seconds".into());
                }
            }
        }
    }
}

impl Drop for Oracle {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Version string of the sqlite3 on PATH.
pub fn sqlite_version() -> String {
    Command::new("sqlite3")
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.split_whitespace().next().map(String::from))
        .unwrap_or_else(|| "unknown".into())
}
