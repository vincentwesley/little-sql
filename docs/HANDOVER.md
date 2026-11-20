# Handover

Updated after every milestone so a new session can continue from here.
This is the final state at the end of the first build (M0–M10).

## Status (2026-11-20)

All milestones in PLAN.md are done. `scripts/acceptance.sh` passes all 19
checks, verified from a fresh clone of `claude/nice-brown-acniv0` (1 min
45 s on a 4-core VM). The whole project is in one pull request:
[vincentwesley/little-sql#1](https://github.com/vincentwesley/little-sql/pull/1).

| Suite | Passed | Failed | Rate |
|-------|-------:|-------:|-----:|
| sqllogictest full corpus (622 files), native mode | 5,939,879 | 0 | 100.00 % |
| sqllogictest full corpus (622 files), read mode | 5,729,099 | 0 | 100.00 % |
| sqllogictest CI sample (49 files), native mode | 323,361 | 0 | 100.00 % |
| sqllogictest CI sample (49 files), read mode | 240,267 | 0 | 100.00 % |
| Differential tests vs sqlite3 3.45.1 (17 files, 5 fixtures) | 324 | 0 | 100.00 % |
| Planner: plans identical to sqlite3's `EXPLAIN QUERY PLAN` | 30 | 0 | — |

Reference: sqlite3 3.45.1 (DECISIONS D25). Numbers come from
`docs/results/*.json` via `node scripts/publish-results.mjs`; never edit them
by hand.

## Where things are

- `crates/littlesql` — the engine, zero dependencies: `storage`, `pager`,
  `wal`, `btree` (read), `btree_write` (insert/split/delete/merge, overflow,
  freelist), `record`, `schema`, `sql/` (lexer, parser, AST), `bind`
  (resolution, planner, view flattening, windows), `plan` (plan + EXPLAIN),
  `exec` (executor, windows), `write` (DDL, DML, constraints, triggers,
  transactions), `check` (integrity check), `func`, `datetime`, `json`, `db`.
- `crates/littlesql-cli` — the `littlesql` shell.
- `crates/littlesql-wasm` + `playground/` — WebAssembly build and static site.
- `crates/slt` — sqllogictest and differential runners.
- `tests/` — differential suite + fixtures, planner suite, write workloads,
  sqllogictest sample list (`tests/slt/SAMPLE`, `MANIFEST.md`, `FLOOR`).
- `scripts/` — `acceptance.sh`, `fetch-sqllogictest.sh`, `slt-full.sh`,
  `build-demo-db.sh`, `build-playground.sh`, `playground-smoke.mjs`,
  `publish-results.mjs`, `write-check.sh`, `crash-check.sh`, `bench.sh`.
- `data/` — the HDB resale CSV (2022 onwards, Singapore Open Data Licence).
- `docs/` — PLAN, DECISIONS (D1–D28), RESEARCH, ACCEPTANCE, results/.

## How to continue

- Environment: `apt-get install -y sqlite3` (must be 3.45.1, as in Ubuntu
  24.04); `rustup` reads `rust-toolchain.toml` (Rust 1.97.0 with the wasm
  target); Node 22 for the playground scripts.
- `scripts/fetch-sqllogictest.sh`, then `scripts/acceptance.sh`.
- After any change that can move a score: `scripts/slt-full.sh`,
  `scripts/acceptance.sh`, `node scripts/publish-results.mjs`,
  `scripts/build-playground.sh`, and commit `docs/results/` with README.

## Known issues

- Join order is greedy; it can pick a worse order than SQLite's for 3+
  table joins with skewed sizes (answers are still correct).
- GROUP BY over a join can choose an index order where SQLite sorts. A
  valid plan, just different from SQLite's; left out of the plan-equality
  suite.
- Page splits are greedy (appends) or even (elsewhere) rather than SQLite's
  three-sibling balance; files are valid but less densely packed after
  random inserts and deletes.
- The CLI has no line editing (no dependencies); `rlwrap` works.
- `EXPLAIN QUERY PLAN` shows nothing for the window-function step.

## Done and verified

Final pass rate: **5,939,879 / 5,939,879 (100.00 %)** on the full
sqllogictest corpus in native mode; 324 / 324 differential tests.

- M0 Foundations: Cargo workspace, MIT licence, SECURITY.md, README for
  users, CI on GitHub Actions (fmt, clippy, tests, build, acceptance;
  sqlite3 pinned), acceptance script — A0.1–A0.5 pass.
- M1 File format reader: all page sizes, overflow, every serial type,
  UTF-16, WITHOUT ROWID, schema; `.tables` / `.schema` / dumps identical to
  sqlite3 — A1.1–A1.2 pass.
- M2 SQL front end and runner: parser, expressions, functions, read-mode
  sqllogictest with a published score — A2.1–A2.2 pass.
- M3 Joins, grouping, subqueries, views, CTEs (including recursive) — the
  differential suite (A3.1) passes with no known failures.
- M4 Planner and EXPLAIN: index and rowid searches, IN lookups, covering
  indexes, ORDER BY / GROUP BY from index order, hash joins, view
  flattening; 30 plans identical to sqlite3's — A4.1–A4.2 pass.
- M5 Playground: wasm (875 KB), static site, HDB demo database (18 MB,
  credited), per-query plans with loop and row counts, pass rate by
  feature, writes and download; tested in Node and in Chromium —
  A5.1–A5.3 pass.
- M6 Read hardening: read mode reached 100 % on the full corpus, so
  nothing was left to climb; window functions and JSON were then added and
  verified differentially.
- M7 Writes: DDL, DML, constraints (NOT NULL, UNIQUE, PRIMARY KEY, CHECK),
  AUTOINCREMENT, triggers, TEMP views; native mode 100 %; files written by
  Little SQL pass sqlite3's integrity check with identical `.dump` —
  A7.1–A7.2 pass.
- M8 WAL and transactions: WAL commits, statement savepoints, ROLLBACK,
  kill -9 and torn-WAL recovery identical in sqlite3 and Little SQL — A8.1
  passes.
- M9 Performance: profiled and optimized (DECISIONS D26); reads 1–7×,
  writes about 5–6.5× sqlite3 — A9.1 passes.
- M10 Final review: fresh-clone acceptance (19/19), README read-through
  and CLI / playground run as a stranger (fixed: a new empty database
  reported corruption on `.tables`; README test commands), one pull
  request.

## Not done, and why

- **Hosting the playground.** Optional in the brief, and it needs the
  repository owner to enable GitHub Pages (or choose another host); see
  NEEDS-VIN. CI keeps the built site as a downloadable artifact.
- **JSON editing and table functions** (`json_set`, `json_each`, ...),
  **ALTER TABLE**, **savepoints**, **RAISE()**, **TEMP tables and
  triggers**, **virtual tables**, **ATTACH**, **bound parameters**,
  **RANGE frames with a numeric offset**, **frame EXCLUDE**: not exercised
  by the corpus; each is rejected with a clear "not supported" error rather
  than approximated. Most useful next: ALTER TABLE (ADD COLUMN, RENAME),
  `json_each`, savepoints.
- **Multi-process access** (file locks, `-shm`): out of scope by design
  (DECISIONS D13).
- **SQLite-grade speed**: would need a bytecode VM and three-sibling
  balancing; the current factor is published in the README.

## Items in NEEDS-VIN.md

- Nothing is required. One optional item: publishing the playground on a
  static host such as GitHub Pages (steps in docs/NEEDS-VIN.md).
