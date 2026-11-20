# Little SQL

A small SQL database engine written from scratch in Rust. It opens real
SQLite database files and answers queries on them: it reads the SQLite file
format, walks its B-trees, parses SQL, plans queries with indexes, and runs
joins, grouping, ordering, aggregates and subqueries. It ships as a
command-line shell and as a WebAssembly playground that runs in the browser.

The engine has no dependencies: no SQL parser, storage or query library,
and no third-party crates at all.

## How well does it work?

Little SQL is graded by [sqllogictest](https://sqlite.org/sqllogictest), the
SQLite project's public corpus of SQL tests, and by differential tests that
run the same queries through the `sqlite3` tool and compare the answers.

<!-- results:start -->
| Suite | Passed | Failed | Pass rate |
|-------|-------:|-------:|----------:|
| sqllogictest, full corpus (622 files), read mode | 5,729,099 | 0 | 100.00 % |
| sqllogictest, CI sample (49 files), read mode | 240,267 | 0 | 100.00 % |
| Differential tests against sqlite3 3.45.1 | 251 | 6 | 97.66 % |
<!-- results:end -->

What the numbers mean:

- **Read mode.** Each test file first builds tables with `CREATE` and
  `INSERT` statements. Little SQL cannot write yet, so in read mode the real
  `sqlite3` runs those setup statements into a database file, and Little SQL
  opens the file and answers every query. Setup statements are never counted
  as passes. A native mode, where Little SQL also runs the setup itself,
  arrives with write support.
- Records marked `skipif sqlite` or `onlyif <another engine>` do not apply to
  SQLite's dialect and are not counted (about 1.5 million in the full
  corpus).
- Any error, crash, timeout or unsupported feature on a counted record is a
  failure. Rates are rounded down, so a run with a single failure never shows
  100.00 %.
- The six differential failures are features not implemented yet: window
  functions (5) and JSON functions (1). They stay in the count.

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
  automatic indexes for `UNIQUE` and `PRIMARY KEY`, and reading a database
  together with its write-ahead log.
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

- Writing: `INSERT`, `UPDATE`, `DELETE`, `CREATE`, `DROP` and transactions.
  These are next on the plan.
- Window functions, JSON functions, triggers, virtual tables, `ATTACH`,
  bound parameters, expression and partial indexes (tables with them are
  still readable; the planner just ignores those indexes).
- Concurrent access from several processes: Little SQL takes no file locks.

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
cargo test --workspace                      # unit tests
scripts/fetch-sqllogictest.sh               # the CI sample (66 MB)
./target/release/slt run corpus/test        # sqllogictest, read mode
./target/release/slt diff tests/differential   # differential tests
scripts/acceptance.sh                       # every acceptance check
```

`sqlite3` must be installed for the sqllogictest read mode, the differential
tests and the acceptance checks. For the full corpus (1.1 GB):

```bash
scripts/fetch-sqllogictest.sh --full
./target/release/slt run corpus/test
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
