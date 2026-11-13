//! `slt`: the test runner behind Little SQL's published pass rates.
//!
//! ```text
//! slt run  [--mode read|native] [--suite NAME] [--jobs N] [--json FILE]
//!          [--failures FILE] [--timeout-ms N] [--min-rate PCT] PATH...
//! slt diff [--json FILE] [--failures FILE] DIR
//! ```

mod diff;
mod md5;
mod oracle;
mod report;
mod run;
mod script;

use run::{FileResult, Mode, Options};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

fn usage() -> ! {
    eprintln!(
        "usage:\n  slt run  [--mode read|native] [--suite NAME] [--jobs N] [--json FILE] [--failures FILE]\n           [--timeout-ms N] [--min-rate PCT] PATH...\n  slt diff [--json FILE] [--failures FILE] DIR"
    );
    std::process::exit(2);
}

fn collect(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            if p.file_name().is_some_and(|n| n != "fixtures") {
                collect(&p, ext, out);
            }
        } else if p.extension().is_some_and(|e| e == ext) {
            out.push(p);
        }
    }
}

fn scratch_dir() -> PathBuf {
    let shm = Path::new("/dev/shm");
    if shm.is_dir() {
        shm.to_path_buf()
    } else {
        std::env::temp_dir()
    }
}

struct Args {
    flags: BTreeMap<String, String>,
    paths: Vec<String>,
}

fn parse_args(args: &[String]) -> Args {
    let mut flags = BTreeMap::new();
    let mut paths = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if let Some(name) = args[i].strip_prefix("--") {
            let Some(v) = args.get(i + 1) else { usage() };
            flags.insert(name.to_string(), v.clone());
            i += 2;
        } else {
            paths.push(args[i].clone());
            i += 1;
        }
    }
    Args { flags, paths }
}

fn write_failures(path: &str, results: &[FileResult]) {
    let mut out = String::new();
    let mut by_reason: BTreeMap<String, u64> = BTreeMap::new();
    for r in results {
        for f in &r.failures {
            // Group by the message with digits and quoted text removed.
            let mut key: String = f.reason.chars().filter(|c| !c.is_ascii_digit()).collect();
            if let Some(i) = key.find(": expected") {
                key.truncate(i);
            }
            key.truncate(70);
            *by_reason.entry(key).or_default() += 1;
            out.push_str(&format!(
                "{}:{}: {}\n    {}\n",
                f.file,
                f.line,
                f.reason,
                f.sql.replace('\n', "\n    ")
            ));
        }
    }
    let mut top: Vec<(&String, &u64)> = by_reason.iter().collect();
    top.sort_by(|a, b| b.1.cmp(a.1));
    let mut head = String::from("# failure reasons (sampled, most common first)\n");
    for (k, n) in top.iter().take(40) {
        head.push_str(&format!("# {n:>7}  {k}\n"));
    }
    if let Err(e) = std::fs::write(path, head + "\n" + &out) {
        eprintln!("cannot write {path}: {e}");
    }
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = argv.first() else { usage() };
    let args = parse_args(&argv[1..]);
    if args.paths.is_empty() {
        usage();
    }
    let started = Instant::now();
    let version = oracle::sqlite_version();
    let (results, summary) = match cmd.as_str() {
        "run" => {
            let mode = match args.flags.get("mode").map(String::as_str) {
                None | Some("read") => Mode::Read,
                Some("native") => Mode::Native,
                _ => usage(),
            };
            let jobs: usize = args
                .flags
                .get("jobs")
                .and_then(|j| j.parse().ok())
                .unwrap_or_else(|| std::thread::available_parallelism().map_or(2, |n| n.get()));
            let mut files: Vec<(PathBuf, PathBuf)> = Vec::new();
            for p in &args.paths {
                let p = PathBuf::from(p);
                if p.is_dir() {
                    let mut found = Vec::new();
                    collect(&p, "test", &mut found);
                    for f in found {
                        let rel = f.strip_prefix(&p).unwrap_or(&f).to_path_buf();
                        files.push((f, rel));
                    }
                } else {
                    let rel = PathBuf::from(p.file_name().unwrap_or_default());
                    files.push((p, rel));
                }
            }
            if files.is_empty() {
                eprintln!("no .test files found");
                std::process::exit(2);
            }
            // Largest files first so the workers finish at similar times.
            files.sort_by_key(|(f, _)| {
                std::cmp::Reverse(f.metadata().map(|m| m.len()).unwrap_or(0))
            });
            let total = files.len();
            let queue = Arc::new(Mutex::new(files));
            let done = Arc::new(Mutex::new(Vec::<FileResult>::new()));
            let opts = Arc::new(Options {
                mode,
                scratch: scratch_dir(),
                statement_millis: args
                    .flags
                    .get("timeout-ms")
                    .and_then(|t| t.parse().ok())
                    .unwrap_or(30_000),
                max_failures_per_file: 25,
            });
            let quiet = args.flags.contains_key("quiet");
            let mut handles = Vec::new();
            for worker in 0..jobs.max(1) {
                let (queue, done, opts) = (queue.clone(), done.clone(), opts.clone());
                // Deeply nested expressions recurse; give workers room.
                let h = std::thread::Builder::new()
                    .stack_size(512 << 20)
                    .spawn(move || loop {
                        let Some((path, rel)) = queue.lock().unwrap().pop() else {
                            break;
                        };
                        let r = run::run_file(&path, &rel, &opts, worker);
                        let mut d = done.lock().unwrap();
                        if !quiet {
                            eprintln!(
                                "[{}/{}] {} pass={} fail={} ({} ms)",
                                d.len() + 1,
                                total,
                                r.file,
                                r.tally.pass,
                                r.tally.fail,
                                r.millis
                            );
                        }
                        d.push(r);
                    });
                handles.push(h.expect("cannot start worker thread"));
            }
            for h in handles {
                let _ = h.join();
            }
            let mut results = std::mem::take(&mut *done.lock().unwrap());
            results.sort_by(|a, b| a.file.cmp(&b.file));
            if results.len() != total {
                eprintln!(
                    "a worker crashed: {} of {total} files finished",
                    results.len()
                );
                std::process::exit(1);
            }
            let suite = args
                .flags
                .get("suite")
                .cloned()
                .unwrap_or_else(|| "sqllogictest".into());
            let mode_name = if mode == Mode::Read { "read" } else { "native" };
            let s = report::summarize(
                &suite,
                mode_name,
                &version,
                &results,
                started.elapsed().as_secs_f64(),
            );
            (results, s)
        }
        "diff" => {
            let mut files = Vec::new();
            for p in &args.paths {
                collect(Path::new(p), "sql", &mut files);
            }
            if files.is_empty() {
                eprintln!("no .sql files found");
                std::process::exit(2);
            }
            let scratch = scratch_dir();
            let results: Vec<FileResult> =
                files.iter().map(|f| diff::run_file(f, &scratch)).collect();
            let s = report::summarize(
                "differential",
                "read",
                &version,
                &results,
                started.elapsed().as_secs_f64(),
            );
            (results, s)
        }
        _ => usage(),
    };
    print!("{}", summary.to_text());
    if let Some(path) = args.flags.get("json") {
        if let Some(dir) = Path::new(path).parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(path, summary.to_json()) {
            eprintln!("cannot write {path}: {e}");
            std::process::exit(1);
        }
    }
    if let Some(path) = args.flags.get("failures") {
        write_failures(path, &results);
    }
    if cmd == "diff" {
        let unexpected: Vec<_> = results
            .iter()
            .flat_map(|r| &r.failures)
            .filter(|f| !f.reason.starts_with("known:"))
            .collect();
        for f in &unexpected {
            eprintln!(
                "{}:{}: {}\n    {}",
                f.file,
                f.line,
                f.reason,
                f.sql.replace('\n', "\n    ")
            );
        }
        if !unexpected.is_empty() {
            eprintln!("{} unexpected differential result(s)", unexpected.len());
            std::process::exit(1);
        }
    }
    if let Some(min) = args
        .flags
        .get("min-rate")
        .and_then(|m| m.parse::<f64>().ok())
    {
        if summary.total.rate() + 1e-9 < min {
            eprintln!(
                "pass rate {:.2}% is below the floor of {min:.2}%",
                summary.total.rate()
            );
            std::process::exit(1);
        }
    }
}
