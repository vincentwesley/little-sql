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
