# Decisions

Every product and technical decision, with its reason. Newest entries are at
the bottom; a decision that was later changed is marked and points at the one
that replaced it.

## D1. Rust workspace with four crates

`littlesql` (the engine, a library with **zero dependencies**),
`littlesql-cli` (the `littlesql` binary), `littlesql-wasm` (the WebAssembly
build) and `slt` (the test runner). *Why:* the brief says the core must be
written from scratch, and a dependency-free engine crate makes that checkable
with one look at `Cargo.toml`. Keeping the runner separate stops test-only
code from leaking into the engine.

## D2. No third-party crates anywhere, for now

The CLI parses its own arguments, the runner has its own MD5, and the wasm
crate exports a hand-written C ABI instead of using wasm-bindgen. *Why:* the
whole workspace then builds offline with a stock toolchain, CI needs no
caches or extra tools, and the wasm build is a plain
`cargo build --target wasm32-unknown-unknown`. The brief allows libraries
outside the engine; none has been worth its cost yet.

## D3. Plan-tree interpreter, not bytecode

SQL is parsed to an AST, bound to a resolved plan (every column reference
becomes a cursor number and a column index), and the plan is interpreted
directly. *Why:* a VDBE-style bytecode machine is how SQLite does it, but a
plan tree is far easier to read and to render in an EXPLAIN view, and it is
fast enough for this project's goals.

## D4. Index use is always a narrowing, never a replacement

When the planner turns a WHERE term into an index seek, the original term is
still evaluated as a filter on every row the seek returns. *Why:* the seek
then only has to return a superset of the right rows, so subtle affinity,
collation and NULL rules cannot make an indexed plan return different rows
from a full scan. The cost is one redundant comparison per row.

## D5. Two sqllogictest scores, reported separately

sqllogictest files create and fill their tables before querying them, so a
read-only engine cannot run them alone.

- **Read mode** (available from the first query milestone): every
  data-changing statement is executed by the real `sqlite3` tool into a real
  database file; Little SQL then opens that file and answers the queries.
  Only queries (and `statement` records that are SELECTs) are scored.
  Statements executed by sqlite3 are counted as *setup*, never as passes.
- **Native mode** (once writes exist): Little SQL executes everything itself
  and every record is scored.

*Why:* read mode tests exactly what the project is about first — reading
real SQLite files — and keeps the score honest by not taking credit for work
sqlite3 did.

## D6. What counts as a pass

A record passes only if the engine produced the expected output. An
unsupported feature, a parse error, a panic, a timeout or a file the runner
could not finish are all failures. Records guarded by `skipif sqlite` or
`onlyif <other database>` are not applicable to SQLite and are excluded from
the denominator, exactly as the reference harness excludes them. After a
`halt` the rest of a file is excluded for the same reason. The runner prints
the excluded count next to the score.

## D7. CI runs a fixed sample; the full corpus runs on demand

The sample is vendored under `tests/slt/` and listed with reasons in
`tests/slt/MANIFEST.md`. `scripts/fetch-sqllogictest.sh` clones the full
corpus and `scripts/slt-full.sh` runs it. *Why:* the corpus is 1.1 GB and
millions of queries; CI must stay under a few minutes and must not depend on
a download.

## D8. Differential tests against the sqlite3 CLI

`tests/differential/*.sql` holds queries grouped by feature. The runner
builds fixture databases with sqlite3, runs every query through both
`sqlite3` and Little SQL and compares the rows. *Why:* sqllogictest covers
expression and join semantics deeply but barely touches real-world file
features (overflow pages, many-page b-trees, WITHOUT ROWID, collations,
text functions); differential tests cover those.

## D9. Values are reference-counted, the engine is single-threaded

`Value::Text` holds an `Rc<str>` and rows are `Rc<[Value]>`. *Why:* rows are
copied between cursors, sorters and hash tables constantly; `Rc` makes that a
pointer copy. A database handle is therefore not `Send`; the runner gets
parallelism by giving each test file its own handle on its own thread.

## D10. Storage is a trait

The pager reads and writes through `Storage` (file-backed natively, an
in-memory buffer in WebAssembly and tests). *Why:* the same engine code runs
in the browser, where there is no file system.

## D11. Demo database: HDB resale prices, 2022 onwards

The playground ships a SQLite file built by `scripts/build-demo-db.sh` from
the data.gov.sg dataset, limited to transactions from January 2022 so the
file stays well under the 25 MiB static-hosting limit, with a `towns` lookup
table and indexes so joins and index searches have something to show.
*Why:* the full dataset with indexes would sit at the limit; recent years
are the interesting ones.

## D12. Writes always go through a write-ahead log

The first write switches a database to WAL mode (header bytes 18 and 19).
Commits append checksummed frames to `<db>-wal` in SQLite's own format and a
checkpoint copies them back. *Why:* one commit path instead of two (rollback
journal and WAL), and the WAL is the one the brief asks for. The files stay
readable and recoverable by the real sqlite3, which is how the write path is
tested.

## D13. No locking: one process at a time

Little SQL takes no file locks and writes no `-shm` file. *Why:*
multi-process concurrency is out of scope; the README says so plainly.
SQLite rebuilds the wal-index itself when it later opens the file.

## D14. Matching the reference runner's output rules exactly

Two scoring details follow what SQLite's own test harness (`slt_sqlite.c`)
produced when the expected answers were recorded, because those answers are
the standard we are graded against:

- `R` values are printed the way SQLite's `printf("%.3f")` prints them: at
  most 16 significant digits, then zeros (`-9223372036854776000.000`).
- A query that SQLite aborts with `integer overflow` was recorded as
  returning no rows. If Little SQL raises the same error and the expected
  result is empty, the record passes. Only that exact run-time error
  qualifies; a parse error or an unsupported feature never passes.

*Why:* both were verified against sqlite3 3.45 on
`evidence/slt_lang_aggfunc.test` (lines 480-496): sqlite3 itself raises the
overflow, and its own printf gives the 16-digit rendering. Anything looser
would risk counting a failure as a pass.

## D15. The corpus is fetched, not vendored

`scripts/fetch-sqllogictest.sh` does a sparse, shallow fetch of the files
listed in `tests/slt/SAMPLE` (or of everything with `--full`) from a pinned
commit of a GitHub mirror of SQLite's sqllogictest. *Why:* the sample is
66 MB and the full corpus 1.1 GB; committing either would bloat every clone
of a small project. Pinning the commit keeps the score reproducible. The
sample choice is explained in `tests/slt/MANIFEST.md`.

## D16. The planner uses SQLite's no-statistics heuristics

Without ANALYZE data SQLite assumes an equality on the first column of an
index leaves about 10 rows, each further equality halves that, and each
range bound keeps a quarter. Little SQL uses the same rules, prefers a
covering index for a full scan, walks an index (or the rowid, backwards if
needed) instead of sorting when that gives the ORDER BY or GROUP BY order,
and looks up `IN (...)` values one by one. *Why:* plans that agree with
SQLite's can be checked mechanically (acceptance check A4.1 compares
`EXPLAIN QUERY PLAN` output line by line), and the heuristics are good ones.
Join order is still greedy (cheapest next table), which is what lets
select5's 64-table joins plan instantly.

## D17. Bare columns follow SQLite's row choice

In `SELECT a, b FROM t GROUP BY a`, `b` comes from the last row of the group
when the engine hashes groups (SQLite sorts and keeps the last row), from the
first row when groups stream from an index in order (as SQLite does), and
from the row that set the value of a lone `min()`/`max()`. *Why:* SQLite
documents the value as arbitrary but programs depend on its actual choice;
the differential suite checks it.

## D18. `UNION` keeps the later of two equal rows

`SELECT 1 UNION SELECT 1.0` returns `1.0`, and `'a' COLLATE NOCASE UNION
'A'` returns `A`, because SQLite inserts rows into a temporary index where an
equal key replaces the earlier one. Little SQL does the same. INTERSECT and
EXCEPT return the left side's (last) row.

## D19. The demo data is committed; results are published from reports

The 2022-onwards slice of the HDB dataset is committed as a 1.4 MB
`data/hdb-resale-2022-onwards.csv.gz`, and `scripts/build-demo-db.sh` turns
it into the 18 MB demo database. *Why:* the source dataset grows every
month, so downloading it at build time would change the demo and its
example answers; the compressed CSV is small, while the database is a build
product.

Test numbers are never typed by hand: the runner writes JSON reports,
`scripts/publish-results.mjs` copies them into `docs/results/` (committed),
builds `results.json` for the playground and rewrites the README table.
Acceptance check A5.3 fails if a fresh run disagrees with what is published.

## D20. The playground: a module web worker over a raw C ABI

The page (`playground/`) is plain HTML, CSS and JavaScript with no build
step and no framework. The engine runs in a web worker so a slow query
never freezes the page; if it traps (an engine bug), the worker starts a new
instance and reopens the database. Each query's result carries its plan with
loop and row counts, shown above the rows, because seeing how a query ran
is the point of the playground. *Why no framework or bundler:* nothing here
needs one, and a static directory can be hosted anywhere.

## D21. B-tree changes: rebuild the page, split greedily, never rebalance

A changed page is decoded into a list of raw cells and laid out again from
scratch. An overfull page is packed greedily into as many pages as needed
(the last group keeps the original page number, so the parent's pointer
stays valid); a root that overflows keeps its page number and becomes an
interior page over new children. Deletes leave pages under-full and only
act when a page becomes empty: an empty leaf is freed and unlinked, and an
interior page left with no cells is merged with a sibling through the
parent's divider (replacing it by its only child would leave leaves at
different depths, which sqlite3's integrity check rightly rejects; the
write-check found exactly that bug). *Why:* SQLite's balance algorithm
redistributes cells across three siblings to keep pages full; that buys
space efficiency, not correctness, and is the hardest code in SQLite.
Greedy packing is ideal for the common append pattern (rowids increasing)
and every file it writes passes sqlite3's `PRAGMA integrity_check`.

## D22. UPDATE and DELETE select their rows with the planner first

The target rows of UPDATE and DELETE are found by running an ordinary
SELECT (`SELECT rowid, *, <new values> FROM t WHERE ...`) through the
binder, planner and executor, collected in full before anything changes.
*Why:* it reuses index selection for WHERE, and collecting first rules out
the Halloween problem (an update seeing its own output) without any cursor
invalidation logic. The cost is memory proportional to the rows changed.

## D23. Triggers resolve NEW and OLD in the binder

While a trigger body runs, the current row is pushed on a stack that the
binder consults: `NEW.x` and `OLD.x` bind to that row's values as
constants. Bodies then run as ordinary statements in the same transaction.
BEFORE, AFTER and INSTEAD OF (on views) triggers, `UPDATE OF`, `WHEN`, and
SQLite's default of no recursive triggers are supported; `RAISE()` is not.
*Why:* no new execution machinery is needed, and the semantics match
SQLite's for everything the evidence tests use.

## D24. What writes do not do (yet)

- TEMP tables and TEMP triggers are rejected; TEMP views are kept in the
  connection and never written to the file.
- Foreign keys are parsed and not enforced, which is SQLite's own default
  (`PRAGMA foreign_keys` is off unless a program turns it on).
- Writing to a table with an expression or partial index is refused,
  because the index could not be kept up to date.
- `ALTER TABLE`, savepoints, `VACUUM` and `RAISE()` are not implemented;
  `VACUUM` and `ANALYZE` are accepted and do nothing.

## D25. One reference sqlite3: 3.45.1

Every comparison (read-mode setup, differential tests, planner output,
write and crash checks) uses sqlite3 3.45.1, the version in Ubuntu 24.04.
CI runs on `ubuntu-24.04` and fails early if the version differs. *Why:*
SQLite's answers change between versions (3.46 added `%U`, `%V` and friends
to `strftime`, for example; 3.45 returns NULL for them, as Little SQL does),
and the published reports name their reference. A moving reference would
make the score drift for reasons that have nothing to do with this engine.

## D26. Performance work, guided by profiles

Measured with `scripts/bench.sh` and profiled with valgrind's callgrind
(no `perf` in the build machine). In order of effect:

- **Insert in place.** A new cell that fits a leaf's free gap is written
  straight into the page bytes; only splits take the decode-and-rebuild
  path. 200,000-row insert: 2.1 s → 0.34 s.
- **View and subquery flattening.** A view or FROM-subquery without
  aggregation, DISTINCT, LIMIT, ORDER BY, compound parts or outer joins is
  merged into the outer query (its tables join the outer ones, its WHERE
  terms become outer terms, its columns become expressions), so the planner
  sees base tables and indexes instead of a materialized copy. Columns of
  the view are marked as read only when the outer query uses them, so
  covering-index decisions match SQLite's.
- **Allocation-free key comparison.** Index searches compare the search
  key with the stored record bytes field by field; numbers and BINARY text
  never become values.
- **Top-N sorting.** `ORDER BY ... LIMIT n` keeps only the best
  `n + offset` rows during the scan (partial selection when the buffer
  doubles, with a sequence number so the result equals a stable sort).
- **Even page splits.** Splitting packs pages full only when the new cell
  is the last on its page (appends); otherwise cells are spread evenly.
  Random-order inserts had left trails of near-empty pages (a test file
  used 877 pages where sqlite3 used 95; now 129).
- **Delete in place.** A deleted cell's page is compacted in one pass
  without decoding cells into separate buffers. (An earlier version kept a
  freeblock list instead; sqlite3's integrity check rejected it because
  adjacent freeblocks must be merged, and the simpler compaction was chosen.
  Little SQL's own integrity check now verifies free-space accounting too.)

Not done: bytecode compilation, a page cache smarter than clear-when-full,
and SQLite's three-sibling balancing.

## D27. Window functions run over captured rows

A SELECT with window functions runs as usual up to HAVING, but instead of
projecting, each surviving row is captured (every cursor's current row and
the group's aggregate values, all reference-counted). Each window is then
computed over the captured rows: sort by partition and ORDER BY (stable),
find peer groups, work out each row's frame (ROWS, GROUPS, or RANGE with
UNBOUNDED / CURRENT ROW bounds), and evaluate. Aggregates over a frame that
starts at the partition's start are computed incrementally; other frames
are recomputed per row. Finally rows are projected (DISTINCT, ORDER BY,
LIMIT as usual) with each window's value available to the result
expressions, so `rank() OVER (...) * 10` or `sum(count(*)) OVER ()` work.
*Why:* the window step needs the full row set anyway; capturing the
executor's own row state means result expressions are evaluated by the
ordinary interpreter with no rewriting of the query. `RANGE` frames with a
numeric offset and `EXCLUDE` are reported as unsupported rather than
approximated.

## D28. JSON: a small parser, SQLite's output rules, subtype by binding

JSON text is parsed into a tree that keeps numbers as written, and written
back minified, which is what SQLite's functions return. SQLite marks values
produced by JSON functions with a hidden "subtype" so that
`json_array(json_array(1))` nests instead of quoting; Little SQL's values
have no subtype, so the binder wraps such arguments of the JSON builders
(`json_array`, `json_object`, `json_quote`, the `json_group_*` aggregates)
in an internal tagging function instead. BLOB arguments are read as JSONB,
SQLite 3.45's binary JSON, as sqlite3 does. *Why:* the observable results
match sqlite3 (the differential file `json.sql` covers nesting, paths,
operators, duplicate keys, numbers, Unicode, JSONB and errors) without
adding a field to every value. Editing functions (`json_set`, `json_insert`,
`json_remove`, `json_patch`) and the table-valued `json_each` /
`json_tree` are not implemented.

## D29. ALTER TABLE rewrites stored SQL with token rules, then re-checks

SQLite (3.26+) renames a table or column everywhere it is referenced,
including inside views and triggers, by re-parsing them. Little SQL keeps
no parse positions in its AST, so it rewrites the stored SQL on tokens: a
name denotes the table after FROM, JOIN, INTO, UPDATE, TABLE, REFERENCES,
the `ON` of an index or trigger, in a FROM list, or before a `.`; a column
name is renamed where it can only mean that column (its definition,
constraints and index lists, `UPDATE OF` lists, qualified references to
the table or its aliases, and unqualified references in statements where
no other table has a column of that name). After the rewrite the schema
is reloaded and every view is bound; if anything fails, the statement is
rolled back with an error naming the object. *Why:* the observable result
matches sqlite3 on the write-check workload (identical `.dump`), and the
re-check turns any case the token rules get wrong into a refused ALTER
rather than a broken schema. DROP COLUMN refuses, in SQLite's words,
columns that are part of a key, an index or a constraint, and rewrites
every row.

## D30. Savepoints and RAISE() reuse the statement savepoint

The pager's snapshot of uncommitted pages (used to undo a failing
statement inside a transaction) also implements SAVEPOINT / ROLLBACK TO,
together with the connection's TEMP views. RAISE() is an internal function
that fails with a dedicated error; the statement runner then applies
SQLite's rules for each kind (ABORT undoes the statement, FAIL keeps what
it already did, ROLLBACK ends the transaction, IGNORE skips the row).

## D31. Parameters bind as constants

`?`, `?N`, `:name`, `@name` and `$name` are numbered per statement exactly
as SQLite numbers them and bound as literal values while the statement is
bound (there are no prepared statements to reuse, so nothing is lost).
Unbound parameters are NULL, as in SQLite. Views and triggers may not use
parameters. The shell's `.parameter` binds named parameters by name only,
like sqlite3's.

## D32. Constant propagation only where it is exactly implied

From `a = 5` and `b = a` the planner derives `b = 5` when all three
comparisons are BINARY equalities in one affinity class, so equality is
transitive under SQLite's comparison rules. The derived term is an extra
filter (DECISIONS D4: plans only narrow; every original term is still
checked), so a derivation can never change an answer, only the plan.
