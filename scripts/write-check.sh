#!/usr/bin/env bash
# Acceptance check A7.2: files written by Little SQL are valid SQLite files
# with the same content sqlite3 produces.
#
# 1. tests/write/workload.sql runs in Little SQL (new file) and in sqlite3.
# 2. tests/write/dml.sql changes a copy of a sqlite3-built fixture, in both.
# For each pair: sqlite3's PRAGMA integrity_check must say "ok" on Little
# SQL's file, and `sqlite3 FILE .dump` must be identical for both files.
set -euo pipefail
cd "$(dirname "$0")/.."
LSQL=target/release/littlesql
[ -x "$LSQL" ] || cargo build --release -q -p littlesql-cli
T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT
fail=0

compare() { # compare NAME OURS THEIRS
  local check
  check=$(sqlite3 "$2" "PRAGMA integrity_check")
  if [ "$check" != "ok" ]; then
    echo "$1: sqlite3 integrity_check on Little SQL's file:"
    echo "$check" | head -20
    fail=1
  fi
  if ! diff <(sqlite3 "$2" .dump) <(sqlite3 "$3" .dump) > "$T/$1.diff"; then
    echo "$1: dumps differ:"
    head -30 "$T/$1.diff"
    fail=1
  fi
  echo "$1: $(sqlite3 "$2" "SELECT count(*) FROM sqlite_schema") schema objects, $(stat -c %s "$2") bytes"
}

# 1. A new database written entirely by Little SQL.
"$LSQL" "$T/ours.db" < tests/write/workload.sql
sqlite3 "$T/theirs.db" < tests/write/workload.sql
compare workload "$T/ours.db" "$T/theirs.db"

# 2. Existing files built by sqlite3 (UTF-8 4 KiB pages, 512-byte pages,
#    UTF-16), changed by both.
sqlite3 "$T/shop.db" < tests/differential/fixtures/shop.sql
cp "$T/shop.db" "$T/shop-ours.db"
"$LSQL" "$T/shop-ours.db" < tests/write/dml.sql
sqlite3 "$T/shop.db" < tests/write/dml.sql
compare shop "$T/shop-ours.db" "$T/shop.db"

for fx in pages utf16; do
  sqlite3 "$T/$fx.db" < "tests/differential/fixtures/$fx.sql"
  cp "$T/$fx.db" "$T/$fx-ours.db"
done
PAGES="DELETE FROM big WHERE id % 3 = 0; UPDATE big SET payload = payload || payload WHERE id % 10 = 1; INSERT INTO big(k, payload, n) SELECT k || 'x', payload, n FROM big WHERE id < 400; DELETE FROM wr WHERE b % 4 = 0; INSERT INTO wr SELECT a || 'z', b, c FROM wr WHERE b < 300;"
"$LSQL" "$T/pages-ours.db" "$PAGES" >/dev/null
sqlite3 "$T/pages.db" "$PAGES"
compare pages "$T/pages-ours.db" "$T/pages.db"
UTF="INSERT INTO words(w, lang) VALUES ('Grüße', 'de'), ('你好', 'zh'); UPDATE words SET w = upper(w) WHERE lang = 'en'; DELETE FROM words WHERE lang IS NULL;"
"$LSQL" "$T/utf16-ours.db" "$UTF" >/dev/null
sqlite3 "$T/utf16.db" "$UTF"
compare utf16 "$T/utf16-ours.db" "$T/utf16.db"

exit $fail
