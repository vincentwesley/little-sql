# Acceptance checks

Every check is run by `scripts/acceptance.sh`, which exits non-zero if any
check fails. `scripts/acceptance.sh M3` runs the checks up to and including
milestone M3; with no argument it runs everything that is implemented.

Requirements on the machine: a Rust stable toolchain with the
`wasm32-unknown-unknown` target, `sqlite3`, `python3` (static file server for
the playground smoke test) and `node`.

| ID | Milestone | Check | How the script checks it |
|----|-----------|-------|--------------------------|
| A0.1 | M0 | Code is formatted | `cargo fmt --all --check` |
| A0.2 | M0 | No lint warnings | `cargo clippy --workspace --all-targets -- -D warnings` |
| A0.3 | M0 | Unit and integration tests pass | `cargo test --workspace` |
| A0.4 | M0 | Engine crate has no dependencies | `cargo tree -p littlesql` prints one line |
| A0.5 | M0 | Required files exist | LICENSE, SECURITY.md, README.md, CI workflow, all docs |
| A1.1 | M1 | Tables and schema match sqlite3 | `littlesql db .tables` / `.schema` vs `sqlite3` on the fixture databases |
| A1.2 | M1 | Full-table dumps match sqlite3 | `SELECT *` of every fixture table (overflow pages, multi-level b-trees, all serial types, 512-byte to 64 KiB pages) |
| A2.1 | M2 | sqllogictest sample runs in read mode | `slt run --mode read tests/slt` completes and writes `results/slt-read.json` |
| A2.2 | M2 | Score did not regress | Pass rate ≥ the floor recorded in `tests/slt/FLOOR` |
| A3.1 | M3 | Differential suite passes | `slt diff tests/differential` reports zero mismatches other than those listed in `tests/differential/KNOWN_FAILURES` |
| A4.1 | M4 | Planner uses indexes | `EXPLAIN QUERY PLAN` output for fixture queries contains the expected `SEARCH … USING INDEX` lines (`tests/explain/*.txt`) |
| A4.2 | M4 | select5 (64-table joins) finishes | Part of the sample; the runner has a per-file time limit |
| A5.1 | M5 | wasm builds and is small | `scripts/build-playground.sh`; every file under `playground/` < 25 MiB |
| A5.2 | M5 | Playground works end to end | `node scripts/playground-smoke.mjs` loads the wasm in Node, opens the demo database, runs a query and an EXPLAIN |
| A5.3 | M5 | Published numbers are current | README table and `playground/results.json` equal the output of the latest runner results |
| A7.1 | M7 | Native mode runs | `slt run --mode native tests/slt` completes; floor in `tests/slt/FLOOR` |
| A7.2 | M7 | Written files are valid SQLite | Write tests end with `sqlite3 file "PRAGMA integrity_check"` = `ok` and equal content |
| A8.1 | M8 | Transactions and crash recovery | Rollback restores state; a WAL left without checkpoint is recovered by both Little SQL and sqlite3 with identical content |
| A9.1 | M9 | Benchmarks run | `scripts/bench.sh` prints a table of timings against sqlite3 |

The final run from a fresh clone is recorded in HANDOVER.md.
