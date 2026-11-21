#!/usr/bin/env bash
# Acceptance check A8.1: transactions and crash recovery.
#
# 1. ROLLBACK restores the state before BEGIN.
# 2. Little SQL is killed with SIGKILL after several commits and in the
#    middle of an open transaction. The WAL it leaves behind must be
#    recovered by sqlite3 and by Little SQL to the same content: every
#    committed transaction, nothing from the open one.
# 3. The WAL's last frames are torn (truncated mid-frame, then garbage
#    appended): both engines must fall back to the same earlier commit.
set -euo pipefail
cd "$(dirname "$0")/.."
LSQL=target/release/littlesql
[ -x "$LSQL" ] || cargo build --release -q -p littlesql-cli
T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT
fail=0
Q="SELECT count(*), sum(v), max(id), group_concat(tag) FROM t; PRAGMA integrity_check;"

# 1. Rollback.
"$LSQL" "$T/r.db" "CREATE TABLE t(id INTEGER PRIMARY KEY, v INT, tag TEXT); INSERT INTO t(v, tag) VALUES (1, 'a'), (2, 'b');" \
  "BEGIN; DELETE FROM t; INSERT INTO t(v, tag) VALUES (99, 'z'); CREATE TABLE gone(x); ROLLBACK;"
got=$("$LSQL" "$T/r.db" "SELECT count(*), sum(v) FROM t; SELECT count(*) FROM sqlite_schema WHERE name = 'gone'")
if [ "$got" != $'2|3\n0' ]; then echo "rollback: unexpected state: $got"; fail=1; fi

# 2. Kill -9 with committed and uncommitted work.
{
  echo "CREATE TABLE t(id INTEGER PRIMARY KEY, v INT, tag TEXT);"
  echo "CREATE INDEX t_v ON t(v);"
  for i in $(seq 1 40); do
    echo "INSERT INTO t(v, tag) VALUES ($i, printf('%.*c', 300, 'x') || '$i');"
  done
  echo "UPDATE t SET v = v * 10 WHERE id % 4 = 0;"
  echo "DELETE FROM t WHERE id % 9 = 0;"
  echo "BEGIN;"
  echo "INSERT INTO t(v, tag) VALUES (-1, 'uncommitted');"
  echo "DELETE FROM t;"
  echo ".print ready"
  sleep 30
} | "$LSQL" "$T/k.db" > "$T/k.out" 2>&1 &
pid=$!
for _ in $(seq 1 100); do grep -q ready "$T/k.out" 2>/dev/null && break; sleep 0.1; done
# The pipe's writer is a subshell; kill the shell itself, hard.
pkill -9 -f "$LSQL $T/k.db" || true
wait "$pid" 2>/dev/null || true
if [ ! -s "$T/k.db-wal" ]; then echo "crash: no WAL was left behind"; fail=1; fi
mkdir "$T/a" "$T/b"
cp "$T/k.db" "$T/k.db-wal" "$T/a/"
cp "$T/k.db" "$T/k.db-wal" "$T/b/"
want=$(sqlite3 "$T/a/k.db" "$Q")
ours=$("$LSQL" -readonly "$T/b/k.db" "$Q")
expect=$(sqlite3 :memory: "CREATE TABLE t(id INTEGER PRIMARY KEY, v INT, tag TEXT);
  WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i < 40)
  INSERT INTO t(v, tag) SELECT i, printf('%.*c', 300, 'x') || i FROM n;
  UPDATE t SET v = v * 10 WHERE id % 4 = 0; DELETE FROM t WHERE id % 9 = 0; $Q")
if [ "$want" != "$expect" ]; then echo "crash: sqlite3 recovered something else:"; echo "$want" | cut -c1-200; fail=1; fi
if [ "$ours" != "$expect" ]; then echo "crash: Little SQL recovered something else:"; echo "$ours" | cut -c1-200; fail=1; fi
echo "crash: both engines recovered $(echo "$expect" | head -1 | cut -d'|' -f1) committed rows"

# 3. A torn WAL: drop the last 1000 bytes and append garbage.
mkdir "$T/c" "$T/d"
for d in c d; do
  cp "$T/k.db" "$T/k.db-wal" "$T/$d/"
  size=$(stat -c %s "$T/$d/k.db-wal")
  truncate -s $((size - 1000)) "$T/$d/k.db-wal"
  head -c 3000 /dev/urandom >> "$T/$d/k.db-wal"
done
want=$(sqlite3 "$T/c/k.db" "$Q")
ours=$("$LSQL" -readonly "$T/d/k.db" "$Q")
if [ "$want" != "$ours" ]; then
  echo "torn WAL: the engines disagree"
  echo "  sqlite3:    $(echo "$want" | head -1 | cut -c1-120)"
  echo "  Little SQL: $(echo "$ours" | head -1 | cut -c1-120)"
  fail=1
else
  echo "torn WAL: both engines recovered $(echo "$want" | head -1 | cut -d'|' -f1) rows"
fi
exit $fail
