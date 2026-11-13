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
| A0.4 | M0 | No third-party crates anywhere | `cargo tree --workspace` lists only this workspace's four packages |
| A0.5 | M0 | Required files exist | LICENSE, SECURITY.md, README.md, CI workflow, every doc, the slt sample list, manifest and floor |
| A1.1 | M1 | Tables and schema match sqlite3 | `littlesql FILE .tables` / `.schema` vs `sqlite3` on every fixture in `tests/differential/fixtures/` |
| A1.2 | M1 | Full-table dumps match sqlite3 | `SELECT *` of every fixture table, compared byte for byte (overflow pages, deep b-trees, every serial type, UTF-16, WITHOUT ROWID, 512 B and 64 KiB pages) |
| A2.1 | M2 | sqllogictest sample runs in read mode, at or above the floor | `slt run --mode read --min-rate <FLOOR> corpus/test` (sample from `tests/slt/SAMPLE`) |
| A2.2 | M2 | Results written | `results/slt-read.json` exists |
| A3.1 | M3 | Differential suite passes | `slt diff tests/differential` reports no mismatch except statements marked `-- known-failure:` (which still count as failures in the published rate) |
| A4.1 | M4 | Planner chooses the same plans as SQLite | For every query in `tests/explain/*.sql`, `EXPLAIN QUERY PLAN` output is identical to sqlite3's |
| A4.2 | M4 | select5 (64-table joins) finishes | `select5.test` runs with zero failures within the per-statement time limit |
| A5.1 | M5 | Playground builds and is small | `scripts/build-playground.sh`; every file under `playground/dist/` < 25 MiB |
| A5.2 | M5 | Playground works end to end | `node scripts/playground-smoke.mjs` loads the wasm in Node, opens the demo database, runs a query and an EXPLAIN |
| A5.3 | M5 | Published numbers are current | `scripts/publish-results.sh --check`: the README table and `playground/dist/results.json` equal the latest runner results |
| A7.1 | M7 | Native mode runs | `slt run --mode native --min-rate <FLOOR> corpus/test` |
| A7.2 | M7 | Written files are valid SQLite | `scripts/write-check.sh`: files written by Little SQL pass `sqlite3 FILE "PRAGMA integrity_check"` and have the same content |
| A8.1 | M8 | Transactions and crash recovery | `scripts/crash-check.sh`: rollback restores state; a WAL left without checkpoint is recovered by both Little SQL and sqlite3 with identical content |
| A9.1 | M9 | Benchmarks run | `scripts/bench.sh --quick` prints a table of timings against sqlite3 |

The final run from a fresh clone is recorded in HANDOVER.md.
