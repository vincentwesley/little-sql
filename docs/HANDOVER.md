# Handover

Updated after every milestone so a new session can continue from here.

## Status (after M5, 2026-11-20)

M0–M5 are done: `scripts/acceptance.sh` passes all 15 checks for M0–M5.

| Suite | Passed | Failed | Rate |
|-------|-------:|-------:|-----:|
| sqllogictest full corpus, read mode | 5,729,099 | 0 | 100.00 % |
| sqllogictest CI sample (49 files), read mode | 240,267 | 0 | 100.00 % |
| Differential (14 files, 5 fixtures) | 251 | 6 | 97.66 % |
| Planner: plans identical to sqlite3's EQP | 29 | 0 | — |

The read side has stopped climbing (nothing left to fix in read mode), so
per the brief the next work is writes. The six differential failures are
known (window functions ×5, JSON ×1), marked `-- known-failure:` and still
counted.

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

**Stubs**: `write.rs` (all DML/DDL returns Unsupported), `check.rs`
(integrity check returns ok without checking). `Database::explicit_txn` and
`reload_schema` are unused until writes land.

## Next steps, in order

1. M7 writes: B-tree insert/split/delete, freelist, CREATE/DROP/INSERT/
   UPDATE/DELETE, real `check.rs`; `slt run --mode native`;
   `scripts/write-check.sh`; add a `native` line to `tests/slt/FLOOR`; add
   M7 to `IMPLEMENTED` in `scripts/acceptance.sh`; publish native results
   (`node scripts/publish-results.mjs`).
2. M8 transactions + WAL crash tests (`scripts/crash-check.sh`); M9
   benchmarks (`scripts/bench.sh`); M10 fresh-clone acceptance + stranger
   review + single PR.
3. Read-side extras if time allows: window functions, JSON.

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
