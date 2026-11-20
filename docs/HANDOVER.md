# Handover

Updated after every milestone so a new session can continue from here.

## Status (after M8, 2026-11-20)

M0–M5, M7 and M8 are done: `scripts/acceptance.sh` passes all 18 checks.

| Suite | Passed | Failed | Rate |
|-------|-------:|-------:|-----:|
| sqllogictest full corpus, native mode | 5,939,879 | 0 | 100.00 % |
| sqllogictest full corpus, read mode | 5,729,099 | 0 | 100.00 % |
| sqllogictest CI sample, native mode | 323,361 | 0 | 100.00 % |
| sqllogictest CI sample, read mode | 240,267 | 0 | 100.00 % |
| Differential (14 files, 5 fixtures) | 252 | 6 | 97.67 % |
| Planner: plans identical to sqlite3's EQP | 29 | 0 | — |

The six differential failures are known gaps (window functions ×5,
JSON ×1), marked `-- known-failure:` and still counted.

**What exists**

- `crates/littlesql` (engine, zero dependencies): storage, pager, WAL
  (read + append), B-tree read cursors, records, schema (incl. WITHOUT ROWID
  primary keys as a pseudo-index), lexer/parser/AST, binder + planner
  (SQLite's no-stats heuristics, IN lookups, ORDER BY / GROUP BY by index
  order, covering scans, INDEXED BY / NOT INDEXED), executor (nested loops,
  hash joins, streaming or hashed aggregation, recursive CTEs), functions,
  date/time, `Database` API.
- `crates/littlesql-cli`: the `littlesql` shell, output identical to sqlite3
  in list/csv/tabs/line/json/column/table/box/markdown modes.
- `crates/slt`: sqllogictest runner (read and native modes), differential
  runner, reports.
- `crates/littlesql-wasm` + `playground/` (see below), `data/` (HDB CSV).
- `scripts/fetch-sqllogictest.sh` (sample or `--full`), `scripts/acceptance.sh`,
  `build-demo-db.sh`, `build-playground.sh`, `playground-smoke.mjs`,
  `publish-results.mjs`.
- `rust-toolchain.toml` pins Rust 1.97.0 (CI's newer clippy had broken the
  first CI run).
- CI: `.github/workflows/ci.yml` (fmt, clippy, test, build; then the
  acceptance checks on the sample; manual dispatch input `full` for the
  whole corpus).

- Writes (M7): `btree_write.rs` (insert/split/delete/merge, overflow,
  freelist), `write.rs` (DDL, DML, constraints incl. CHECK, triggers, TEMP
  views, transactions with statement savepoints), `check.rs` (real
  integrity check). Tests: `crates/littlesql/tests/writes.rs` (randomized
  against a model), `scripts/write-check.sh` (sqlite3 validates our files,
  `.dump` identical), `scripts/crash-check.sh` (kill -9 + torn WAL,
  recovery identical in both engines).
- `scripts/slt-full.sh`: the whole corpus in both modes.

## Next steps, in order

1. M9 performance: `scripts/bench.sh [--quick]` (timings vs sqlite3 on the
   demo DB and generated data), profile and fix the worst cases; add M9 to
   `IMPLEMENTED` in `scripts/acceptance.sh`.
2. Optional features: window functions (the 5 known differential
   failures), JSON functions, ALTER TABLE.
3. M10: fresh-clone acceptance run, stranger review of README / CLI /
   playground, final HANDOVER lists, one PR for the whole project.

## Playground (M5)

`scripts/build-playground.sh` → `playground/dist/` (static: wasm 875 KB,
demo DB 18 MB). `node scripts/playground-smoke.mjs` tests it in Node; it was
also checked in Chromium via Playwright (queries, plans, test page, opening a
local file; no console errors). Results shown on the page and in README come
from `docs/results/*.json` via `node scripts/publish-results.mjs`; after
any change to the score, rerun the suites into `results/` and publish.
The full-corpus report is produced with
`slt run --suite "sqllogictest full corpus" --json results/slt-read-full.json <corpus>/test`.

## Environment notes for a new session

- `sqlite3` comes from `apt-get install -y sqlite3`; the wasm target from
  `rustup target add wasm32-unknown-unknown`.
- Corpus: `scripts/fetch-sqllogictest.sh [--full]` (into `./corpus`,
  gitignored). In the first session a full clone was also at
  `/home/user/gregrahn/sqllogictest`.
- HDB CSV: `https://api-open.data.gov.sg/v1/public/api/datasets/d_8b84c4ee58e3cfc0ece0d773c8ca6abc/poll-download`
  returns JSON with a signed download URL. A copy was in the session
  scratchpad (`hdb/hdb.csv`); it is not in the repo.

## Known issues

- `slt run DIR` derives categories from the path relative to DIR, so pass
  the corpus `test/` directory as the root.
- Join order is greedy; it can pick a worse order than SQLite's for 3+
  table joins with skewed sizes (results are still correct).
- The CLI has no line editing (no dependencies); use `rlwrap` if wanted.
