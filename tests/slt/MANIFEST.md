# The sqllogictest sample

The full sqllogictest corpus is 622 files, about 1.1 GB and 5.7 million
queries. CI runs a fixed sample of 49 files listed in [`SAMPLE`](SAMPLE),
fetched by `scripts/fetch-sqllogictest.sh` from a pinned commit
(`c93ac416f05635ff5cc490009add936d9753d01d` of
[gregrahn/sqllogictest](https://github.com/gregrahn/sqllogictest), a GitHub
mirror of [SQLite's own repository](https://sqlite.org/sqllogictest)).
The tests are public domain. Nothing from the corpus is committed here.

The sample takes about 15 seconds on four cores. The full set runs with
`scripts/slt-full.sh` (about a minute on eight cores, after a 1.1 GB fetch).

## What is in the sample, and why

| Group | Files | Why |
|-------|-------|-----|
| `select1`–`select5` | 5 (all) | The hand-written core. Every SELECT feature on 1–5 tables, correlated subqueries, and select5's joins of up to 64 tables, which catch bad join ordering. |
| `evidence/` | 12 (all) | One file per SQL language feature (aggregates, IN, CREATE/DROP, REPLACE, UPDATE, views), each statement tied to a sentence in the SQLite docs. Small, and the only place several features appear. |
| `index/<kind>/<rows>/` | 28 (the first non-empty file of every directory) | Each directory pairs a query shape (BETWEEN, IN, ORDER BY with and without a sort, commuted comparisons, views, deletes, random predicates) with a table size from 1 to 10 000 rows. One file per directory covers every index access path at every size; the other files in a directory are more of the same shape. |
| `random/<generator>/` | 4 (`slt_good_0` of each) | Randomly generated expressions, aggregates, GROUP BY and multi-table SELECTs. Their value is breadth of expression semantics (NULL handling, affinity, integer/real arithmetic); one file of each already has 10 000 queries. |

`index/view/10/slt_good_0.test` is empty upstream, so that directory
contributes `slt_good_1.test` instead.

## How the score is counted

See `docs/DECISIONS.md` D5, D6 and D14. In short: only `query` records and
`statement` records Little SQL itself executes are scored; records marked
`skipif sqlite` / `onlyif <other engine>` are "not applicable"; in read mode
the setup statements are run by sqlite3 and are never counted as passes. Any
error, crash, timeout or unsupported feature on a scored record is a
failure.

## The floor

[`FLOOR`](FLOOR) holds the lowest pass rate CI accepts for each mode. It is
set a little under the current score so CI catches a regression; raise it
when the score goes up.
