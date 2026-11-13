# Plan

## Architecture

```
            SQL text
               │
     ┌─────────▼─────────┐
     │ sql::lexer/parser │  hand-written Pratt parser → AST
     └─────────┬─────────┘
     ┌─────────▼─────────┐
     │ bind + plan       │  name resolution, WHERE analysis, index choice,
     │                   │  join order → plan tree (what EXPLAIN shows)
     └─────────┬─────────┘
     ┌─────────▼─────────┐
     │ exec              │  nested-loop joins, aggregation, sort, DISTINCT,
     │                   │  compound selects, subqueries; later DML
     └─────────┬─────────┘
     ┌─────────▼─────────┐
     │ btree             │  table and index cursors, seek, overflow chains;
     │                   │  later insert / delete with page splits
     └─────────┬─────────┘
     ┌─────────▼─────────┐
     │ pager (+ wal)     │  page cache over a Storage; later dirty pages,
     │                   │  WAL frames, commit, rollback, checkpoint
     └─────────┬─────────┘
          Storage trait: file | memory
```

Crates: `crates/littlesql` (engine), `crates/littlesql-cli`,
`crates/littlesql-wasm`, `crates/slt` (sqllogictest + differential runner).
The playground is static files under `playground/`.

## Milestones

Each milestone ends with: format, lint, tests and build pass locally; the
checks in ACCEPTANCE.md for that milestone pass; HANDOVER.md is updated; one
push.

| # | Milestone | Outcome |
|---|-----------|---------|
| M0 | Foundations | Workspace, CI, licence, SECURITY.md, docs, acceptance script skeleton |
| M1 | File format reader | Header, pager, b-tree traversal, records, overflow, schema; CLI can list tables and dump any table byte-for-byte equal to sqlite3 |
| M2 | SQL front end and single-table queries | Lexer, parser, expressions, functions, WHERE, ORDER BY, LIMIT, DISTINCT; sqllogictest runner (read mode) and the first published score |
| M3 | Joins, grouping, subqueries | Inner/left/cross joins, GROUP BY/HAVING, aggregates, compound selects, scalar/IN/EXISTS subqueries (correlated too), views, CTEs |
| M4 | Planner and EXPLAIN | Rowid and index seeks, range scans, covering indexes, automatic hash indexes for joins, greedy join order, `EXPLAIN QUERY PLAN`, run statistics; differential test suite |
| M5 | Playground | WebAssembly build, static site, demo database, EXPLAIN view, pass-rate panel; README for users |
| M6 | Read side hardening | Work the failure list down until the pass rate stops climbing; WITHOUT ROWID tables; WAL-mode files |
| M7 | Writes | CREATE/DROP TABLE/INDEX/VIEW, INSERT, UPDATE, DELETE with b-tree page splits, overflow, freelist; native sqllogictest mode; `PRAGMA integrity_check` by sqlite3 on every file written |
| M8 | WAL and transactions | BEGIN/COMMIT/ROLLBACK, WAL frames in SQLite's format, crash recovery, checkpoint; crash tests verified by sqlite3 |
| M9 | Performance | Benchmarks against sqlite3, lazy column decoding, ORDER BY via index, min/max via index |
| M10 | Final review | Fresh-clone acceptance run, stranger's read-through, pull request |

M2–M4 are developed against the same executor design, so their boundaries are
about what is *verified*, not about rewrites.

## Stack

- Rust stable (edition 2021), `cargo fmt`, `cargo clippy -D warnings`.
- GitHub Actions: fmt, clippy, tests, wasm build, sqllogictest sample,
  differential tests.
- `sqlite3` CLI as the oracle (apt package in CI).
- Playground: plain HTML/CSS/JS, no build step, no framework.

## Risks

- **Semantics drift from SQLite.** Mitigated by the two oracles, run from M2.
- **`select5.test`** (64-way joins) needs join ordering and automatic
  indexes; planned in M4.
- **Write path corrupting files.** Mitigated by running sqlite3's
  `PRAGMA integrity_check` on every database the tests write.
- **Session ends early.** HANDOVER.md is updated at every milestone.
