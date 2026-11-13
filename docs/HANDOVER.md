# Handover

Updated after every milestone so a new session can continue from here.

## Status (after M4, 2026-11-13)

M0–M4 are done: `scripts/acceptance.sh` passes all 12 checks for M0–M4.

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
- `scripts/fetch-sqllogictest.sh` (sample or `--full`), `scripts/acceptance.sh`.
- CI: `.github/workflows/ci.yml` (fmt, clippy, test, build; then the
  acceptance checks on the sample; manual dispatch input `full` for the
  whole corpus).

**Stubs**: `write.rs` (all DML/DDL returns Unsupported), `check.rs`
(integrity check returns ok without checking). `Database::explicit_txn` and
`reload_schema` are unused until writes land.

## Next steps, in order

1. M5: `crates/littlesql-wasm` (raw C ABI, no wasm-bindgen), static
   playground under `playground/` built into `playground/dist/` by
   `scripts/build-playground.sh`, demo DB from the HDB CSV
   (`scripts/build-demo-db.sh`), `scripts/playground-smoke.mjs`,
   `scripts/publish-results.sh [--check]` that writes the README table and
   `results.json` from `results/*.json`. Then add M5 to `IMPLEMENTED` in
   `scripts/acceptance.sh`.
2. M7 writes: B-tree insert/split/delete, freelist, CREATE/DROP/INSERT/
   UPDATE/DELETE, real `check.rs`; `slt run --mode native`; `write-check.sh`.
3. M8 transactions + WAL crash tests (`crash-check.sh`); M9 benchmarks
   (`bench.sh`); M10 fresh-clone acceptance + stranger review + single PR.
4. Read-side extras if time allows: window functions, JSON.

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
