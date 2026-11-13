# Research notes

Collected before any code was written (2026-11-12). Each section says what the
source is, what I took from it, and how it shaped the design.

## SQLite file format

Source: <https://www.sqlite.org/fileformat2.html>

- **Header (100 bytes, page 1).** Magic `SQLite format 3\0`; page size at
  offset 16 (big-endian u16, the value 1 means 65536); write/read version at
  18/19 (1 = rollback journal, 2 = WAL); reserved bytes per page at 20
  (usable size = page size − reserved); file change counter at 24; database
  size in pages at 28 (only trusted when the change counter equals the
  version-valid-for number at 92); first freelist trunk at 32 and freelist
  count at 36; schema cookie at 40; schema format at 44; text encoding at 56
  (1 = UTF-8, 2/3 = UTF-16).
- **Pages.** Four b-tree page kinds: interior index (2), interior table (5),
  leaf index (10), leaf table (13). The page header is 8 bytes on leaves and
  12 on interior pages (the extra four are the right-most child pointer). The
  cell pointer array follows the header; cell content grows from the end of
  the page backwards. Page 1's b-tree header starts at byte 100.
- **Table b-trees** are keyed by a 64-bit rowid. Leaf cell: payload-size
  varint, rowid varint, payload, optional 4-byte overflow page number.
  Interior cell: 4-byte left child, rowid varint (all keys in the left subtree
  are ≤ that rowid).
- **Index b-trees** are keyed by a record whose last field is the rowid.
  Interior cells carry a real entry as well as a left-child pointer, so an
  in-order walk must visit child, cell, child, cell, …, right-most child.
- **Overflow.** With usable size U: table-leaf max local payload X = U − 35;
  index max local X = ((U − 12) · 64 / 255) − 23; min local
  M = ((U − 12) · 32 / 255) − 23; K = M + ((P − M) mod (U − 4)). A payload of
  P bytes stores P locally if P ≤ X, otherwise K if K ≤ X, otherwise M. The
  rest lives on a linked list of overflow pages (4-byte next pointer, then
  data).
- **Varints** are 1–9 bytes, big-endian, 7 bits per byte with a continuation
  bit; the ninth byte contributes all 8 bits.
- **Records.** A header (header-length varint, then one serial-type varint per
  column) followed by the bodies. Serial types: 0 NULL, 1–6 big-endian
  integers of 1/2/3/4/6/8 bytes, 7 IEEE double, 8 and 9 the constants 0 and 1,
  N ≥ 12 even = blob of (N−12)/2 bytes, N ≥ 13 odd = text of (N−13)/2 bytes.
  A record may have fewer fields than the table has columns (after
  `ALTER TABLE ADD COLUMN`); the missing ones take the column default.
- **`sqlite_schema`** is the table rooted at page 1 with columns
  `(type, name, tbl_name, rootpage, sql)`. The column layout of every table is
  only recoverable by parsing the stored `CREATE TABLE` text, so a reader
  needs a DDL parser from day one. Automatic indexes
  (`sqlite_autoindex_<table>_<n>`) have a NULL `sql`; their columns come from
  the table's UNIQUE / PRIMARY KEY constraints in declaration order.
- **`INTEGER PRIMARY KEY`** columns alias the rowid and are stored as NULL in
  the record.
- **Freelist.** Trunk pages hold a next-trunk pointer, a count and that many
  leaf page numbers.

## Write-ahead log

Sources: <https://www.sqlite.org/wal.html>,
<https://www.sqlite.org/walformat.html>,
<https://www.sqlite.org/atomiccommit.html>

- The `-wal` file has a 32-byte header (magic 0x377f0682 or 0x377f0683,
  format 3007000, page size, checkpoint sequence, two salts, two checksum
  words) followed by frames: a 24-byte frame header (page number, database
  size in pages for commit frames or 0, the two salts, a cumulative checksum)
  and one page image.
- A frame is valid only if its salts match the header and its checksum, which
  chains from the previous frame, matches. A transaction is committed when a
  frame with a non-zero "database size" field is valid. A reader uses the
  newest frame for each page up to the last commit frame.
- The `-shm` wal-index is only a shared-memory accelerator; SQLite rebuilds it
  from the WAL when it is missing. A single-process engine can therefore
  write WAL files that the real `sqlite3` recovers without ever writing a
  `-shm` file.
- A checkpoint copies the newest frame of each page back into the database
  file, syncs, and then the WAL can be restarted with new salts.

## Types, comparison and expression semantics

Sources: <https://www.sqlite.org/datatype3.html>,
<https://www.sqlite.org/lang_expr.html>, <https://www.sqlite.org/nulls.html>

- Storage classes NULL < INTEGER/REAL < TEXT < BLOB. Column *affinity*
  (derived from the declared type by substring rules: INT → INTEGER;
  CHAR/CLOB/TEXT → TEXT; BLOB or no type → BLOB; REAL/FLOA/DOUB → REAL;
  otherwise NUMERIC) is applied when a value is stored and before
  comparisons.
- Comparison affinity: if one side has INTEGER/REAL/NUMERIC affinity and the
  other has TEXT, BLOB or none, NUMERIC affinity is applied to the other; if
  one side has TEXT affinity and the other none, TEXT is applied to the
  other. This also decides whether an index may be used for a term.
- Collation precedence: explicit `COLLATE` on the left operand, then the
  right, then a column's declared collation (left first), then BINARY.
- Integer arithmetic that overflows falls back to floating point; integer
  division truncates; division by zero yields NULL. Real-to-text uses
  `%!.15g`.

## Query planning

Sources: <https://www.sqlite.org/queryplanner.html>,
<https://www.sqlite.org/optoverview.html>,
<https://www.sqlite.org/eqp.html>

- SQLite splits WHERE into AND-connected terms, uses terms of the shape
  `column op expr` to constrain an index (equalities on a prefix of the index
  columns, then at most one range), and evaluates everything else as a filter
  at the outermost loop where all its tables are available.
- Joins are nested loops. When an inner loop has an equality term but no
  index, SQLite builds a transient "automatic index". This matters in
  practice: `select5.test` joins up to 64 tables and is impossible without it.
- `EXPLAIN QUERY PLAN` output (`SCAN t`, `SEARCH t USING INDEX i (a=?)`,
  `USE TEMP B-TREE FOR ORDER BY`) is a good, familiar vocabulary for the
  playground's EXPLAIN view.

## sqllogictest

Sources: <https://www.sqlite.org/sqllogictest/doc/trunk/about.wiki>, the
harness source `src/sqllogictest.c` and `src/slt_sqlite.c`, mirror at
<https://github.com/gregrahn/sqllogictest> (commit `c93ac416`).

- 622 test files, about 1.1 GB: `select1`–`select5`, `evidence/` (12),
  `index/` (about 220 files, sub-suites between / commute / delete / in /
  orderby / orderby_nosort / random / view, each at 10, 100, 1000 or 10000
  rows), `random/` (391 files: aggregates, expr, groupby, select).
- Records are `statement ok|error`, `query <types> <sort> [label]` with the
  SQL, a `----` line and the expected values, plus `hash-threshold N`, `halt`
  and the conditionals `skipif <db>` / `onlyif <db>`.
- Result formatting, from `slt_sqlite.c`: NULL prints as `NULL`; type `T`
  prints the text, `(empty)` for the empty string, and every byte outside
  `' '..'~'` becomes `@`; type `I` prints `sqlite3_column_int` (a 32-bit
  truncation) with `%d`; type `R` prints `%.3f`. Sort modes: `nosort`,
  `rowsort` (sort rows as text), `valuesort` (sort all values). When there
  are more values than the hash threshold the expected output is
  `N values hashing to <md5>` where the MD5 covers every value followed by a
  newline.
- The tests need `CREATE TABLE` and `INSERT` before any query can run. That
  shaped the two scoring modes (see DECISIONS.md D5).
- Licence: the corpus is offered under GPL, BSD, MIT or CC0 at the user's
  choice (`COPYRIGHT.md`), so vendoring a sample under MIT is fine.
- sqlite.org serves the Fossil repository behind a robot check, so scripts
  fetch the GitHub mirror instead.

## Existing small engines worth learning from

- **SQLite itself** — <https://www.sqlite.org/arch.html>. The reference for
  every behaviour; the split into pager / b-tree / VDBE informed the crate
  layout even though this engine interprets a plan tree instead of bytecode.
- **Limbo / Turso** — <https://github.com/tursodatabase/turso>. A Rust
  rewrite of SQLite. Useful as proof that the on-disk format can be
  implemented cleanly in safe Rust, and for its use of differential testing
  against SQLite.
- **CodeCrafters "Build your own SQLite"** —
  <https://app.codecrafters.io/courses/sqlite/overview>. The usual on-ramp:
  header, schema table, table scan, index search. It stops well short of a
  planner, joins or writes.
- **cstack/db_tutorial** — <https://cstack.github.io/db_tutorial/>. A B-tree
  with its own file format; good for seeing leaf splits spelled out.
- **toydb** — <https://github.com/erikgrinaker/toydb>. Clear planner and
  executor structure, with its own storage format.
- **chidb** — <http://chi.cs.uchicago.edu/chidb/>. A teaching engine that
  reads a subset of the SQLite format; its architecture document explains
  b-tree cells well.
- **DuckDB, CockroachDB, Dolt** all run sqllogictest in CI. Dolt publishes its
  pass rate as a headline correctness number
  (<https://docs.dolthub.com/sql-reference/benchmarks/correctness>), which is
  the reporting model used here.

## Demo dataset

- "Resale flat prices based on registration date from Jan-2017 onwards",
  Housing & Development Board, dataset `d_8b84c4ee58e3cfc0ece0d773c8ca6abc` at
  <https://data.gov.sg/datasets/d_8b84c4ee58e3cfc0ece0d773c8ca6abc/view>,
  under the Singapore Open Data Licence v1.0
  (<https://data.gov.sg/open-data-licence>).
- 244,539 rows and 24 MB as CSV on 2026-11-12: too close to the 25 MiB
  per-file limit once indexed, so the playground ships a subset (see
  DECISIONS.md D11).
