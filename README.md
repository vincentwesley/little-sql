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

Open a database and run a query:

```bash
./target/release/littlesql mydata.db "SELECT name, count(*) FROM sqlite_schema GROUP BY type"
```

Or start the interactive shell, which works like `sqlite3`:

```text
$ ./target/release/littlesql shop.db
Little SQL 0.1.0
Enter ".help" for usage hints.
littlesql> .tables
customers     lines         order_totals  orders        products
littlesql> .mode box
littlesql> SELECT city, count(*) AS n, round(avg(credit), 2) AS avg_credit
      ...>   FROM customers WHERE city IS NOT NULL
      ...>   GROUP BY city ORDER BY avg_credit DESC LIMIT 3;
┌─────────────┬────┬────────────┐
│    city     │ n  │ avg_credit │
├─────────────┼────┼────────────┤
│ Jurong West │ 43 │ 76.15      │
│ Punggol     │ 43 │ 75.16      │
│ Clementi    │ 43 │ 74.92      │
└─────────────┴────┴────────────┘
littlesql> EXPLAIN QUERY PLAN SELECT * FROM orders WHERE customer_id = 7 ORDER BY placed DESC;
QUERY PLAN
`--SEARCH orders USING INDEX orders_customer (customer_id=?)
```

`EXPLAIN` (without `QUERY PLAN`) runs the query and shows the same tree with
how many times each loop ran and how many rows it visited. `.eqp on` prints
that tree after every query; `.stats on` prints pages read. `.help` lists
every command, and `littlesql -help` every option.

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
| `crates/slt` | The test runner: sqllogictest (read and native modes) and the differential suite |

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

MIT, see [LICENSE](LICENSE). The SQLite file format, SQL dialect and
sqllogictest corpus are the work of the SQLite project and are in the public
domain. Security reports: see [SECURITY.md](SECURITY.md).
