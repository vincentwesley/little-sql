# Handover

Updated after every milestone so a new session can continue from here.

## Status (checkpoint, 2026-11-13)

Work in progress on M0–M4 together; nothing below has been through the
acceptance script yet (it does not exist yet).

**Written and compiling**

- `crates/littlesql` (engine, zero dependencies): `value`, `record`,
  `storage`, `pager` (with dirty pages, commit, checkpoint), `wal` (SQLite
  WAL format, read + append), `btree` (read cursors, seek, overflow),
  `schema`, `sql/{lexer,ast,parser}`, `bind` (name resolution + join
  planner), `plan` (bound plan + EXPLAIN rendering), `exec` (executor),
  `func`, `datetime`, `db` (public `Database`).
- `crates/slt`: sqllogictest runner (`slt run`, read and native modes) and
  differential runner (`slt diff`), own MD5, JSON/text reports.
- `crates/littlesql-cli`: **placeholder** `main.rs` (two args: db, sql).
- Docs: RESEARCH, DECISIONS, PLAN, ACCEPTANCE, NEEDS-VIN.

**First measurement** (read mode, sqlite3 3.45.1 as reference):
select1 + select2 + select3 + evidence = 5640 pass / 4 fail (99.93 %).
The 4 failures are `sum()`/`total()` near the int64 limit in
`evidence/slt_lang_aggfunc.test`. select4, select5, `index/` and `random/`
have not been run yet.

**Stubs** — `write.rs` (all DML/DDL returns Unsupported) and `check.rs`
(integrity check returns ok without checking).

## Next steps, in order

1. `cargo clippy` is not clean yet (unused `mut`, dead fields); fix.
2. Run select4, select5, a slice of `index/` and `random/`; fix failures.
3. Real CLI (shell, dot commands, modes).
4. M0 leftovers: LICENSE (MIT), SECURITY.md, README, CI workflow,
   `scripts/acceptance.sh`, `scripts/fetch-sqllogictest.sh`.
5. Vendor the CI sample into `tests/slt/` with `MANIFEST.md` and `FLOOR`.
6. Differential tests under `tests/differential/` with fixtures.
7. Then M5 (wasm + playground + demo DB), M6–M9 per PLAN.md.

## Environment notes for a new session

- `sqlite3` comes from `apt-get install -y sqlite3`; the wasm target from
  `rustup target add wasm32-unknown-unknown`.
- The corpus: `git clone --depth 1 https://github.com/gregrahn/sqllogictest`
  (sqlite.org's Fossil server blocks scripts). In the first session it was
  at `/home/user/gregrahn/sqllogictest`.
- HDB CSV: `https://api-open.data.gov.sg/v1/public/api/datasets/d_8b84c4ee58e3cfc0ece0d773c8ca6abc/poll-download`
  returns a JSON with a signed download URL.
- Probe command:
  `cargo build --release && ./target/release/slt run --failures /tmp/f.txt <corpus>/test/select1.test`

## Known issues

- `slt run DIR` derives categories from the path relative to DIR, so pass
  the corpus `test/` directory (or the vendored `tests/slt/`) as the root.
- Recursive CTEs, window functions, RIGHT/FULL JOIN (beyond the simple
  two-table RIGHT JOIN), triggers and row values are not implemented.
