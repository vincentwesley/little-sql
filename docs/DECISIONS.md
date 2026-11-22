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

Without ANALYZE data SQLite assumes a table holds about a million rows
(see D39), an equality on the first column of an index leaves about 10
rows, each further equality halves that, and each
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

## D33. Table-valued functions are correlated subquery sources

`json_each` and `json_tree` in FROM bind to a source whose body evaluates
the arguments and walks the JSON, rather than to a general virtual-table
interface. Their arguments may name FROM items to their left (`FROM t,
json_each(t.doc)`), so they are bound with the partly built scope visible,
the cursors they read are recorded, and the join planner only places the
function after those items. The source is re-run for every outer row, as
SQLite re-runs `xFilter`. The `id` column counts byte offsets in SQLite's
binary JSON (JSONB) encoding, which Little SQL computes from the parsed
tree rather than storing JSONB; this is exact for standard JSON (string
escapes are kept as written, see D35) and can differ only for JSON5
spellings such as `0x1F` or `'single quotes'`, which SQLite sizes as
written but Little SQL stores rewritten. The first row of a
`json_tree` that starts below the root copies SQLite's quirky key and
path derivation exactly. Reason: one small, checked feature that answers
the common uses (unnesting a JSON list, searching documents) without a
virtual-table layer nothing else needs yet.

## D34. The shell words SQL errors as sqlite3 does

Errors found while preparing a statement are reported as parse errors and
the rest as runtime errors, in the three forms sqlite3 uses: `Error: in
prepare, ...` / `Error: stepping, ...` for command-line SQL and `-cmd`,
`Parse error: ...` at the prompt, and `Parse error near line N: ...` for
piped input and `.read`, with the result code appended for constraint,
corruption and I/O errors. sqlite3 also prints the statement with a caret
under the failing token; Little SQL does not track error offsets, so it
leaves that out. Reason: scripts and people comparing the two shells see
the same messages.

## D35. JSON text keeps its escapes; JSON5 input is accepted

SQLite 3.45 stores a parsed string with its escapes as written and prints
it back the same way (`"\/"` stays `"\/"`, `"\u0041"` stays `"\u0041"`),
decoding only when a value leaves JSON as SQL text. Little SQL's JSON tree
does the same: a string holds its escaped body, keys are compared by
decoded text, and values coming from SQL are escaped once. That makes
`json()` output and json_each's byte offsets match exactly. SQLite also
accepts JSON5 in every JSON function (unquoted keys, trailing commas,
single quotes, hex, `.5`, `+1`, `Infinity`, `NaN`, comments, extra
whitespace) and rewrites it as standard JSON; the parser does the same,
including SQLite's quirks (`\v` becomes `\u0009`, any letter case of
`inf` / `nan`). `json_valid(x, flags)` separates strict RFC 8259 text from
JSON5, and `json_error_position` reports SQLite's error position. On
18,000 randomly mutated inputs, validity and output matched sqlite3
exactly, and error positions matched in about 99 % of the malformed cases
(the rest are trailing-text and number-prefix corner cases). Reason: the
JSON functions should give sqlite3's answers for the inputs people really
send, which include JSON5 and escaped text.

## D36. RANGE offsets and EXCLUDE follow SQLite's rules exactly

`RANGE n PRECEDING / FOLLOWING` needs exactly one ORDER BY term (SQLite's
error otherwise). Each row's bounds are its ORDER BY value shifted by n
(towards earlier rows for PRECEDING, so the direction flips under DESC),
found by binary search in the sorted partition; a NULL value frames its
NULL peers, and text or blob values are not shifted, as in SQLite's
`windowCodeRangeTest`. Offsets may be fractional for RANGE but must be
whole for ROWS and GROUPS, with SQLite's error messages. `EXCLUDE CURRENT
ROW / GROUP / TIES` removes rows from the frame of aggregates and of
`first_value`, `last_value` and `nth_value`; frames with an exclusion are
evaluated per row rather than incrementally. Reason: completes the window
function grammar SQLite 3.45 accepts, verified against sqlite3 on
NULLs, mixed types, DESC and NULLS FIRST orderings.

## D37. Several database files: each object knows its file

TEMP objects and ATTACH need more than one file per connection. Rather
than thread a database number through the whole engine, every table, view
and trigger records the pager of the file it was loaded from, and cursors
and the b-tree writer use that pager. The connection keeps its files in
`PRAGMA database_list` order (main, then temp, which is an in-memory file
created when the connection opens, then attached files), and binding uses
a merged schema in which unqualified names resolve temp first, then main,
then attached files in attach order, as in SQLite; `db.name` looks in one
file. A write statement first picks its target file (from a `db.`
qualifier, `TEMP`, or the file of the object it names), and the existing
DDL and DML code writes there; existence checks look only in that file,
so a TEMP table may shadow a main one. Transactions and savepoints span
every file (each pager keeps its own savepoint, and COMMIT commits each
file that changed; like SQLite with WAL files, there is no atomic commit
across files). SQLite's resolution rules are copied where tests showed
them: names inside a non-TEMP view or trigger resolve in that object's own
file; a non-TEMP view or trigger may not name another file; dropping a
table drops TEMP triggers on it; ATTACH works inside a transaction and
DETACH only refuses a file with uncommitted changes. *Why:* the smallest
change that gives SQLite's behaviour, and every rule is checked by the
shell scripts in `tests/shell` (acceptance A7.3).

## D38. Real numbers are converted with SQLite's long double arithmetic

SQLite 3.45 on x86-64 turns text into doubles (`sqlite3AtoF`) and doubles
into digits (`sqlite3FpDecode`, used by printf, by real-to-text and by
`.dump`) with 80-bit long double arithmetic, which is not always correctly
rounded: about one random literal in 2,000 is stored as a different double
than a correctly rounding parser produces, and printf prints 18 or 19 real
digits where an exact conversion would print more. `fp.rs` emulates the
long double steps bit for bit (64-bit mantissa products rounded to nearest
even, powers of ten rounded the way the C compiler rounds them, the final
rounding to double) and reproduces 3.45's rounding rules, including its
quirk that `%.0f` and `%.0g` truncate instead of rounding. Checked against
sqlite3 on 7,000 random printf / text / round / CAST cases and 4,000 random
literals through `.dump`: all identical (before: 449 printf differences).
*Why:* stored values and printed text must be the ones sqlite3 gives, and
a correctly rounded conversion is, here, the wrong answer.

## D39. Planner: SQLite's table-size assumption and two tie rules

Without ANALYZE, SQLite assumes every table holds about a million rows
(`nRowLogEst` 200) whatever its real size. Little SQL used the real size,
so small tables were scanned where SQLite searches an index. It now
assumes the same million rows, prefers an index search to a scan when the
estimates tie (a range on an indexed column), scans through a covering
index only when the index is narrower than the table, and, when two
tables tie for the outer loop, prefers the one that lets the next table
use a real index instead of an automatic one. On 30 extra plans over
empty, tiny and real tables, differences from sqlite3 fell from 16 to 4
(the rest are SQLite's Bloom filters and automatic covering indexes); the
24 that match joined the planner suite, now 57 plans, all identical.

## D40. Upsert, RETURNING and UPDATE ... FROM reuse the existing write path

`INSERT ... ON CONFLICT` is resolved where uniqueness was already checked:
the inserter lists the table's constraints (rowid, primary key, unique
indexes) with the upsert targets first, as SQLite checks them, and each
conflict resolves to store, skip or update. `DO UPDATE` runs through the
same function as `UPDATE` (constraints, indexes and UPDATE triggers), with
`excluded.*` and the existing row made visible to binding as named rows
rather than through a new kind of plan node. `RETURNING` evaluates its
expressions against each changed row the same way, after the row is
written. `UPDATE ... FROM` runs one SELECT over the target and the FROM
items, then keeps one update per target row (its first position and last
values), which is what SQLite does. Every shape was compared with sqlite3
in `tests/shell/upsert_returning.sql` and `update_from.sql`.

## D41. Expression and partial indexes: maintained and planned

Writes keep expression and partial indexes exact: each key is evaluated
against the row (a partial index skips rows whose WHERE is not true), and
uniqueness through them is enforced with SQLite's error text. The CREATE
INDEX checks (no subqueries, only deterministic functions) match SQLite's
wording.

The planner uses them the way SQLite does. Each table in a query has its
expression and partial indexes bound over its own cursor, so their key
expressions and WHERE terms are plan expressions that can be compared
structurally with the query's. A partial index qualifies when every term
of its WHERE is one of the query's terms (also in commuted form, `5 = x`
as `x = 5`) or is `x IS NOT NULL` and a query term implies it, by
SQLite's exprImpliesNotNull rules (comparisons, BETWEEN, IN, arithmetic).
An expression key is matched by putting a stand-in column in place of the
expression in a query term, after which the ordinary constraint logic
applies; EXPLAIN shows it as `<expr>`, as SQLite does. ORDER BY can be
delivered by an expression index, and a qualifying partial index is
scanned instead of the table (SQLite assumes it holds half the rows)
unless the table order already gives the ORDER BY.

Checked against sqlite3 on the shell script `index_planning.sql` (plans
and answers) and 700 random scripts of queries, updates and deletes over
such indexes, all with identical output. On random queries the plans
agree 79 % of the time; the rest differ where SQLite weighs the cost of
sorting against walking an index in order, and where it sorts only the
right part of an ORDER BY. Neither is modelled by this planner for any
index (D16), and neither changes answers.

## D42. Generated columns: virtual ones are computed by the binder

A STORED generated column is computed on every write and stored like any
column. A VIRTUAL one is not in the record at all (as in SQLite's file
format), so reading one means computing it. Rather than teach the row
decoder to evaluate expressions, the binder replaces a reference to a
virtual column with its expression over the same table cursor (a `Gen`
plan node that applies the column's affinity and collation, and is NULL
on a LEFT JOIN's null row). Because that node still names its table and
column, the planner treats it as a column: an index on a virtual column
is used for `=`, ranges, `IN` and ORDER BY, as SQLite uses it. Writes
compute generated columns in dependency order after the rowid is chosen;
loops, defaults, primary keys and writes to them fail with SQLite's
messages. All of it is compared with sqlite3 in `tests/shell/generated.sql`.

## D43. Schema pragmas are one function of the schema

`PRAGMA table_info` and its relatives depend only on the schema, so they
moved to `pragma.rs` and the same code serves the table-valued
`pragma_table_info(...)` and friends. The executor has no database handle,
so the schema a SELECT is bound against is made available to it for the
duration of the statement; correlated use such as
`FROM sqlite_schema s, pragma_table_info(s.name)` works like
`json_each`. Only the schema pragmas are offered as functions;
`pragma_function_list` is not, because its rows (SQLite's 200-odd
functions and their flags) cannot be honestly reproduced.
`sqlite_source_id()` reports Little SQL's own version, as
`sqlite_version()` does.

## D44. An internal error in the shell rolls back instead of exiting

The CLI catches a panic inside the engine for one statement, rolls back
whatever that statement and any open transaction left uncommitted, and
reports `internal error (please report it)`. The database file stays
consistent and the session continues; the engine itself is unchanged.

## D45. STRICT tables check types after affinity

A STRICT table accepts only the six type names (INT, INTEGER, REAL, TEXT,
BLOB, ANY) at CREATE time, and each written value is checked after the
column's affinity has been applied, so `'5'` goes into an INTEGER column
as 5 while `'x'` fails with "cannot store TEXT value in INTEGER column
t.c", exactly as in SQLite. ANY keeps values as given.
