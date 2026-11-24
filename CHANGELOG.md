# Changelog

## [1.0.0]

The first release: a SQLite-compatible database engine written from
scratch in Rust, with no dependencies, as a command-line shell and a
browser playground.

**Scores** (against SQLite 3.45.1):

- sqllogictest, the SQLite project's corpus: 100 % of 5,939,879 applicable
  records with Little SQL running every statement, and 100 % of 5,729,099
  queries on files written by sqlite3.
- 443/443 differential tests, 57/57 query plans and 18 whole shell scripts
  identical to sqlite3's output.

**What it does:**

- Reads and writes real SQLite files: every page size, overflow pages,
  UTF-8 and UTF-16, WITHOUT ROWID tables, the freelist and the write-ahead
  log (with crash recovery). Files it writes pass sqlite3's integrity check.
- SQL: joins, subqueries, CTEs (also recursive), views, window functions,
  JSON (including `json_each` / `json_tree`), aggregates, date and time
  functions, and SQLite's exact type, collation and number formatting rules.
- Writes: DDL and DML, constraints, upsert, `RETURNING`, `UPDATE ... FROM`,
  triggers, transactions and savepoints, `ALTER TABLE`, STRICT tables,
  generated columns, expression and partial indexes, TEMP objects and
  `ATTACH`.
- A query planner that follows SQLite's no-statistics rules, with SQLite's
  own cost model for single-table queries; `EXPLAIN QUERY PLAN` output
  matches sqlite3's.
- The `littlesql` shell, compatible with sqlite3's output modes and error
  messages, plus `.dump`, `.schema`, `.tables` and plan trees with row
  counts.
- A WebAssembly playground with a demo database of Singapore HDB resale
  prices, per-query plans, writes and download.

**Known limits:** joins use simpler planning heuristics than SQLite (same
answers, sometimes different plans), no `jsonb_*` functions, no foreign key
enforcement, one process at a time, and queries run a few times slower than
sqlite3. The full list is in the README.
