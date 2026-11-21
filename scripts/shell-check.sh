#!/usr/bin/env bash
# Acceptance check A7.3: whole shell scripts give sqlite3's exact output.
#
# Each tests/shell/*.sql runs through `sqlite3 main.db` and through
# `littlesql main.db`, each time in a fresh, empty directory (so ATTACHed
# files start empty too). Compared: everything printed (stdout and
# stderr; sqlite3's extra lines that point at the failing token are
# dropped, see DECISIONS D34), then for every database file left behind,
# `sqlite3 FILE .dump`, and sqlite3's integrity check of Little SQL's copy.
set -euo pipefail
cd "$(dirname "$0")/.."
LSQL="$PWD/target/release/littlesql"
[ -x "$LSQL" ] || cargo build --release -q -p littlesql-cli
T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT
fail=0
n=0
# Drop sqlite3's "  SQL" / "    ^--- error here" pair after an error.
strip_carets() {
  awk '/\^--- error here$/ { has = 0; next }
       { if (has) print held; held = $0; has = 1 }
       END { if (has) print held }'
}
for f in tests/shell/*.sql; do
  n=$((n + 1))
  name=$(basename "$f" .sql)
  for who in sqlite littlesql; do
    rm -rf "${T:?}/$who" && mkdir -p "$T/$who"
    if [ "$who" = sqlite ]; then
      (cd "$T/$who" && sqlite3 main.db < "$OLDPWD/$f" 2>&1 | strip_carets > "$T/$who.out" || true)
    else
      (cd "$T/$who" && "$LSQL" main.db < "$OLDPWD/$f" > "$T/$who.out" 2>&1 || true)
    fi
  done
  # Paths in PRAGMA database_list / .databases name the run's directory.
  sed -i "s|$T/sqlite|DIR|g" "$T/sqlite.out"
  sed -i "s|$T/littlesql|DIR|g" "$T/littlesql.out"
  if ! diff "$T/sqlite.out" "$T/littlesql.out" > "$T/diff"; then
    echo "$name: output differs"
    head -20 "$T/diff"
    fail=1
  fi
  for db in "$T"/sqlite/*.db; do
    [ -e "$db" ] || continue
    base=$(basename "$db")
    ours="$T/littlesql/$base"
    if [ ! -e "$ours" ]; then
      echo "$name: Little SQL left no $base"
      fail=1
      continue
    fi
    check=$(sqlite3 "$ours" "PRAGMA integrity_check")
    if [ "$check" != "ok" ]; then
      echo "$name: $base fails sqlite3's integrity check: $check"
      fail=1
    fi
    if ! diff <(sqlite3 "$db" .dump) <(sqlite3 "$ours" .dump) > "$T/dump.diff"; then
      echo "$name: $base differs from sqlite3's"
      head -10 "$T/dump.diff"
      fail=1
    fi
  done
done
echo "$n shell scripts compared"
exit $fail
