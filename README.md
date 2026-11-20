# Little SQL

A small SQL database engine written from scratch in Rust. It opens real
SQLite database files, answers queries on them and writes to them: it reads
and writes the SQLite file format and its write-ahead log, walks and splits
B-trees, parses SQL, plans queries with indexes, and runs joins, grouping,
ordering, aggregates, subqueries, transactions and triggers. Files it writes
pass sqlite3's own integrity check. It ships as a command-line shell and as
a WebAssembly playground that runs in the browser.

The engine has no dependencies: no SQL parser, storage or query library,
and no third-party crates at all.

## How well does it work?

Little SQL is graded by [sqllogictest](https://sqlite.org/sqllogictest), the
SQLite project's public corpus of SQL tests, and by differential tests that
run the same queries through the `sqlite3` tool and compare the answers.

<!-- results:start -->
| Suite | Passed | Failed | Pass rate |
|-------|-------:|-------:|----------:|
| sqllogictest, full corpus (622 files), native mode | 5,939,879 | 0 | 100.00 % |
| sqllogictest, full corpus (622 files), read mode | 5,729,099 | 0 | 100.00 % |
| sqllogictest, CI sample (49 files), native mode | 323,361 | 0 | 100.00 % |
| sqllogictest, CI sample (49 files), read mode | 240,267 | 0 | 100.00 % |
| Differential tests against sqlite3 3.45.1 | 267 | 6 | 97.80 % |
<!-- results:end -->

What the numbers mean:

- **Native mode** is the real score: Little SQL runs every statement of
  every test file itself, the `CREATE`, `INSERT`, `UPDATE` and `DELETE`
  statements included, and each one counts.
- **Read mode** isolates the query side: the real `sqlite3` runs each file's
  setup statements into a database file, and Little SQL opens that file and
  answers every query. Setup statements are never counted as passes.
- Records marked `skipif sqlite` or `onlyif <another engine>` do not apply to
  SQLite's dialect and are not counted (about 1.5 million in the full
  corpus).
- Any error, crash, timeout or unsupported feature on a counted record is a
  failure. Rates are rounded down, so a run with a single failure never shows
  100.00 %.
- The six differential failures are features not implemented yet: window
  functions (5) and JSON functions (1). They stay in the count.
- 100 % means every applicable record passes, not that Little SQL is a
  complete SQLite. The corpus concentrates on core SELECT semantics, joins,
  indexes, views and basic DML; features it never exercises (window
  functions, for example) show up in the differential tests and in
  [Not supported](#not-supported-yet) below.

Details of the scoring rules are in [docs/DECISIONS.md](docs/DECISIONS.md)
(D5, D6, D14), and which test files the CI sample includes and why is in
[tests/slt/MANIFEST.md](tests/slt/MANIFEST.md).

## Quick start

You need a Rust toolchain ([rustup.rs](https://rustup.rs)).

```bash
git clone https://github.com/vincentwesley/little-sql
cd little-sql
cargo build --release
```

Build the demo database (needs `sqlite3`) and query it:

```bash
scripts/build-demo-db.sh          # writes target/demo/hdb.db
./target/release/littlesql target/demo/hdb.db "SELECT count(*), max(price) FROM resale"
```

Or start the interactive shell, which works like `sqlite3`:

```text
$ ./target/release/littlesql target/demo/hdb.db
Little SQL 0.1.0
Enter ".help" for usage hints.
littlesql> .tables
resale  sales   towns
littlesql> .mode box
littlesql> SELECT region, count(*) AS sales, round(avg(price)) AS avg_price
      ...>   FROM sales WHERE month >= '2026-01'
      ...>   GROUP BY region ORDER BY avg_price DESC;
┌────────────┬───────┬───────────┐
│   region   │ sales │ avg_price │
├────────────┼───────┼───────────┤
│ Central    │ 4446  │ 770249.0  │
│ East       │ 3779  │ 701592.0  │
│ North-East │ 5339  │ 670435.0  │
│ West       │ 5110  │ 606126.0  │
│ North      │ 3799  │ 588373.0  │
└────────────┴───────┴───────────┘
littlesql> EXPLAIN QUERY PLAN SELECT * FROM resale WHERE town_id = 5 AND month >= '2026-06';
QUERY PLAN
`--SEARCH resale USING INDEX resale_town_month (town_id=? AND month>=?)
```

`EXPLAIN` (without `QUERY PLAN`) runs the query and shows the same tree with
how many times each loop ran and how many rows it visited. `.eqp on` prints
that tree after every query; `.stats on` prints pages read. `.help` lists
every command, and `littlesql -help` every option.

## The playground

The engine also runs in the browser, compiled to WebAssembly: open the demo
database (every HDB resale flat sale in Singapore since 2022) or any SQLite
file of your own, run queries, see each query's plan with how many rows
every loop visited, and see the test pass rate by feature. The file you open
never leaves your browser.

```bash
rustup target add wasm32-unknown-unknown
scripts/build-playground.sh                 # builds playground/dist/
python3 -m http.server -d playground/dist   # then open http://localhost:8000
```

`playground/dist/` is a plain static site (the largest file, the demo
database, is 18 MB), so any static host can serve it. CI builds it on every
push and keeps it as a downloadable artifact.

## What it supports

- **File format:** every page size (512 B to 64 KiB), overflow pages, all
  record serial types, UTF-8 and UTF-16 databases, `WITHOUT ROWID` tables,
  automatic indexes for `UNIQUE` and `PRIMARY KEY`, the freelist, and the
  write-ahead log (read, written, recovered and checkpointed).
- **Writes:** `CREATE TABLE` (also `AS SELECT`), `CREATE INDEX`,
  `CREATE VIEW` (also `TEMP`), `CREATE TRIGGER`, `DROP`, `INSERT` (values,
  `SELECT`, `DEFAULT VALUES`, `WITH`), `UPDATE`, `DELETE`, `OR IGNORE` /
  `OR REPLACE`, `NOT NULL`, `UNIQUE`, `PRIMARY KEY` and `CHECK` constraints,
  `AUTOINCREMENT`, and `BEGIN` / `COMMIT` / `ROLLBACK` (a failing statement
  inside a transaction undoes only itself). Triggers: `BEFORE`, `AFTER`,
  `INSTEAD OF` on views, `UPDATE OF`, `WHEN`. `PRAGMA integrity_check` checks
  the whole file structurally.
- **Queries:** `SELECT` with joins (inner, left, cross, natural, `USING`,
  simple right joins), `WHERE`, `GROUP BY`, `HAVING`, `ORDER BY` (with
  `NULLS FIRST/LAST`), `LIMIT`/`OFFSET`, `DISTINCT`, compound queries
  (`UNION [ALL]`, `INTERSECT`, `EXCEPT`), `VALUES`, views, scalar, `IN`,
  `EXISTS` and correlated subqueries, common table expressions including
  `WITH RECURSIVE`.
- **Semantics:** SQLite's type affinity, comparison and collation rules
  (`BINARY`, `NOCASE`, `RTRIM`), integer overflow to real, three-valued
  logic, and its exact number formatting.
- **Functions:** the core scalar functions (string, numeric, `printf`,
  `like`/`glob`, math), the aggregates (`count`, `sum`, `total`, `avg`, `min`,
  `max`, `group_concat`, with `DISTINCT` and `FILTER`), and the date and time
  functions.
- **Planner:** rowid lookups and ranges, index searches on an equality prefix
  plus one range, `IN` lists and subqueries through an index, covering
  indexes, index order used instead of sorting (forwards or backwards),
  streaming `GROUP BY` over sorted input, automatic hash indexes for joins
  without a usable index, greedy join ordering, and `INDEXED BY` /
  `NOT INDEXED`. On the planner test set its `EXPLAIN QUERY PLAN` output is
  identical to SQLite's.

## Not supported (yet)

- Window functions, JSON functions, virtual tables, `ATTACH`, bound
  parameters, `ALTER TABLE`, savepoints, `RAISE()`, TEMP tables.
- Expression and partial indexes: tables with them are readable (the
  planner ignores those indexes) but Little SQL refuses to write to them.
- Foreign keys are parsed but not enforced, which is also SQLite's default.
- Concurrent access from several processes: Little SQL takes no file locks.
  Use one process at a time.

## Performance

`scripts/bench.sh` runs the same SQL through Little SQL and sqlite3 3.45.1
(best of 5 runs, each a fresh process, on a 4-core cloud VM). Reads use the
demo database (128,000 rows); writes build a 200,000-row table.

| Benchmark | Little SQL | sqlite3 | Ratio |
|---|---:|---:|---:|
| Scan + GROUP BY (128k rows, view join) | 0.083 s | 0.021 s | 3.9× |
| Sort, top 10 | 0.095 s | 0.013 s | 7.3× |
| Index range search | 0.003 s | 0.003 s | 0.9× |
| Covering index scan | 0.033 s | 0.013 s | 2.4× |
| LIKE filter, full scan | 0.061 s | 0.013 s | 4.6× |
| Correlated subquery (27 towns) | 0.092 s | 0.016 s | 5.6× |
| Join + GROUP BY region | 0.094 s | 0.058 s | 1.6× |
| Bulk INSERT ... SELECT (200000 rows) | 0.340 s | 0.073 s | 4.7× |
| Bulk insert + CREATE INDEX | 0.705 s | 0.121 s | 5.8× |
| Bulk insert + UPDATE half | 0.739 s | 0.113 s | 6.5× |
| Bulk insert + DELETE half | 0.551 s | 0.107 s | 5.2× |

Little SQL is a tree-walking interpreter over decoded rows, where SQLite
compiles to bytecode and has two decades of tuning, so it is slower; the
point is that it stays within a small factor while using the same plans. The
biggest wins came from profiling (DECISIONS D26): inserting into a leaf's
free space in place, comparing index keys against record bytes without
decoding them, flattening simple views into the outer query, keeping only
the top rows for `ORDER BY ... LIMIT`, and splitting pages evenly except
when appending.

## How it is built

```text
SQL text ─▶ lexer ─▶ parser ─▶ AST ─▶ binder/planner ─▶ plan tree ─▶ executor
                                            │                          │
                                         schema ◀── B-tree cursors ◀── pager ◀── storage (file / bytes)
                                                                         └── WAL
```

| Crate | What it is |
|-------|------------|
| `crates/littlesql` | The engine: storage, pager, WAL, B-trees, records, schema, SQL parser, binder/planner, executor, functions |
| `crates/littlesql-cli` | The `littlesql` shell |
| `crates/littlesql-wasm` | The engine as WebAssembly, behind a small C ABI |
| `crates/slt` | The test runner: sqllogictest (read and native modes) and the differential suite |
| `playground/` | The browser playground (HTML, CSS, JavaScript; no build step) |

[docs/PLAN.md](docs/PLAN.md) describes the architecture and milestones,
[docs/DECISIONS.md](docs/DECISIONS.md) every design decision and why, and
[docs/RESEARCH.md](docs/RESEARCH.md) the sources it was built from.

## Running the tests

```bash
cargo test --workspace                                 # unit and integration tests
scripts/fetch-sqllogictest.sh                          # the CI sample (66 MB)
./target/release/slt run --mode native corpus/test     # sqllogictest, native mode
./target/release/slt run corpus/test                   # sqllogictest, read mode
./target/release/slt diff tests/differential           # differential tests
scripts/acceptance.sh                                  # every acceptance check
```

`sqlite3` 3.45.1 (the version in Ubuntu 24.04) must be installed for the
read mode, the differential tests and the acceptance checks; other versions
answer a few queries differently. `--failures FILE` writes every failing
record with its reason. For the full corpus (1.1 GB download, about a minute
per mode on four cores):

```bash
scripts/slt-full.sh                  # writes results/slt-{native,read}-full.json
node scripts/publish-results.mjs     # updates this README's table
```

## Licence and credits

MIT, see [LICENSE](LICENSE).

The demo database contains information from Resale flat prices based on
registration date from Jan-2017 onwards, accessed on 12 November 2026 from
[data.gov.sg](https://data.gov.sg/datasets/d_8b84c4ee58e3cfc0ece0d773c8ca6abc/view),
which is made available under the terms of the
[Singapore Open Data Licence version 1.0](https://data.gov.sg/open-data-licence).

The SQLite file format, SQL dialect and
sqllogictest corpus are the work of the SQLite project and are in the public
domain. Security reports: see [SECURITY.md](SECURITY.md).
