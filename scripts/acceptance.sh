#!/usr/bin/env bash
# Run the acceptance checks in docs/ACCEPTANCE.md.
#
#   scripts/acceptance.sh        # every implemented milestone
#   scripts/acceptance.sh M3     # milestones up to and including M3
#
# Exits non-zero if any check fails. Needs: cargo, sqlite3, git (to fetch
# the sqllogictest sample), and for M5 node.
set -uo pipefail
cd "$(dirname "$0")/.."

# Milestones whose checks exist. Extend as milestones land.
IMPLEMENTED=(M0 M1 M2 M3 M4)
UPTO="${1:-${IMPLEMENTED[-1]}}"

SCRATCH="$(mktemp -d)"
trap 'rm -rf "$SCRATCH"' EXIT
mkdir -p results
FAILED=()
PASSED=0

want() { # want M3 -> true if M3 is implemented and <= UPTO
  local m="${1#M}" u="${UPTO#M}"
  [[ " ${IMPLEMENTED[*]} " == *" $1 "* ]] && [ "$m" -le "$u" ]
}

check() { # check ID "description" command...
  local id="$1" desc="$2"
  shift 2
  printf '%-5s %-58s ' "$id" "$desc"
  local log="$SCRATCH/$id.log"
  if "$@" >"$log" 2>&1; then
    echo "ok"
    PASSED=$((PASSED + 1))
  else
    echo "FAILED"
    sed 's/^/      /' "$log" | tail -25
    FAILED+=("$id")
  fi
}

BIN=target/release
LSQL="$BIN/littlesql"
SLT="$BIN/slt"

build_fixture() { # build_fixture NAME -> path of a fresh database
  local db="$SCRATCH/fixture-$1.db"
  if [ ! -f "$db" ]; then
    sqlite3 "$db" < "tests/differential/fixtures/$1.sql" || return 1
  fi
  echo "$db"
}

# ---------------------------------------------------------------- M0
if want M0; then
  check A0.1 "Code is formatted" cargo fmt --all --check
  check A0.2 "No lint warnings" cargo clippy --workspace --all-targets -- -D warnings
  check A0.3 "Unit and integration tests pass" cargo test --workspace -q
  a04() {
    # Every package in the dependency graph must be one of ours.
    local others
    others=$(cargo tree --workspace --prefix none --edges normal,build,dev \
      | sed 's/ (.*//; s/ v[0-9].*//' | sort -u \
      | grep -vxE 'littlesql|littlesql-cli|littlesql-wasm|slt' || true)
    if [ -n "$others" ]; then
      echo "third-party crates found:"
      echo "$others"
      return 1
    fi
  }
  check A0.4 "No third-party crates anywhere in the workspace" a04
  a05() {
    local missing=0
    for f in LICENSE SECURITY.md README.md .github/workflows/ci.yml \
      docs/RESEARCH.md docs/DECISIONS.md docs/PLAN.md docs/ACCEPTANCE.md \
      docs/HANDOVER.md docs/NEEDS-VIN.md tests/slt/SAMPLE tests/slt/MANIFEST.md \
      tests/slt/FLOOR; do
      [ -s "$f" ] || { echo "missing or empty: $f"; missing=1; }
    done
    return $missing
  }
  check A0.5 "Required files exist" a05
fi

# Everything after M0 needs release binaries.
if want M1; then
  cargo build --release --workspace -q 2>"$SCRATCH/build.log" || {
    cat "$SCRATCH/build.log"
    echo "release build failed"
    exit 1
  }
fi

# ---------------------------------------------------------------- M1
if want M1; then
  a11() {
    local bad=0
    for fx in tests/differential/fixtures/*.sql; do
      local name db
      name=$(basename "$fx" .sql)
      db=$(build_fixture "$name") || return 1
      # .tables lays names out in columns; compare the set of names.
      diff <("$LSQL" -readonly "$db" .tables | tr -s ' ' '\n' | sed '/^$/d' | sort) \
        <(sqlite3 "$db" .tables | tr -s ' ' '\n' | sed '/^$/d' | sort) \
        || { echo "$name: .tables differs"; bad=1; }
      diff <("$LSQL" -readonly "$db" .schema) <(sqlite3 "$db" .schema) \
        || { echo "$name: .schema differs"; bad=1; }
    done
    return $bad
  }
  check A1.1 "Tables and schema match sqlite3 on every fixture" a11
  a12() {
    local bad=0 n=0
    for fx in tests/differential/fixtures/*.sql; do
      local name db
      name=$(basename "$fx" .sql)
      db=$(build_fixture "$name") || return 1
      for t in $(sqlite3 "$db" "SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'"); do
        n=$((n + 1))
        cmp -s <("$LSQL" -readonly "$db" "SELECT * FROM \"$t\"") \
          <(sqlite3 "$db" "SELECT * FROM \"$t\"") \
          || { echo "$name.$t: dump differs"; bad=1; }
      done
    done
    echo "$n tables compared"
    return $bad
  }
  check A1.2 "Full-table dumps match sqlite3 byte for byte" a12
fi

# ---------------------------------------------------------------- M2
if want M2; then
  [ -d corpus/test ] || scripts/fetch-sqllogictest.sh >/dev/null
  floor=$(awk '$1 == "read" { print $2 }' tests/slt/FLOOR)
  a21() {
    "$SLT" run --mode read --quiet 1 --suite "sqllogictest sample" \
      --json results/slt-read.json --failures results/slt-read-failures.txt \
      --min-rate "$floor" corpus/test
  }
  check A2.1 "sqllogictest sample, read mode, at or above the floor" a21
  check A2.2 "Results written" test -s results/slt-read.json
fi

# ---------------------------------------------------------------- M3
if want M3; then
  a31() {
    "$SLT" diff --json results/differential.json \
      --failures results/differential-failures.txt tests/differential
  }
  check A3.1 "Differential suite: no unexpected mismatches" a31
fi

# ---------------------------------------------------------------- M4
if want M4; then
  a41() {
    local bad=0 n=0
    for f in tests/explain/*.sql; do
      local fx db
      fx=$(sed -n 's/^-- fixture: *//p' "$f" | head -1)
      db=$(build_fixture "$fx") || return 1
      while IFS= read -r q; do
        [ -z "$q" ] && continue
        n=$((n + 1))
        if ! diff <("$LSQL" -readonly "$db" "EXPLAIN QUERY PLAN $q" 2>&1) \
          <(sqlite3 "$db" "EXPLAIN QUERY PLAN $q" 2>&1) >"$SCRATCH/eqp.diff"; then
          echo "plan differs: $q"
          cat "$SCRATCH/eqp.diff"
          bad=1
        fi
      done < <(grep -v '^--' "$f")
    done
    echo "$n plans compared"
    return $bad
  }
  check A4.1 "Query plans match sqlite3's EXPLAIN QUERY PLAN" a41
  a42() {
    "$SLT" run --mode read --quiet 1 --timeout-ms 60000 corpus/test/select5.test \
      | tail -2 | grep -q ' 0 '
  }
  check A4.2 "select5 (joins of up to 64 tables) runs without failures" a42
fi

# ---------------------------------------------------------------- M5
if want M5; then
  a51() {
    scripts/build-playground.sh >/dev/null || return 1
    local big
    big=$(find playground/dist -type f -size +25M)
    [ -z "$big" ] || { echo "files over 25 MiB: $big"; return 1; }
  }
  check A5.1 "Playground builds; every file under 25 MiB" a51
  check A5.2 "Playground works end to end (Node smoke test)" node scripts/playground-smoke.mjs
  check A5.3 "Published numbers match the latest results" scripts/publish-results.sh --check
fi

# ---------------------------------------------------------------- M7
if want M7; then
  floor_native=$(awk '$1 == "native" { print $2 }' tests/slt/FLOOR)
  a71() {
    "$SLT" run --mode native --quiet 1 --suite "sqllogictest sample" \
      --json results/slt-native.json --failures results/slt-native-failures.txt \
      --min-rate "$floor_native" corpus/test
  }
  check A7.1 "sqllogictest sample, native mode, at or above the floor" a71
  check A7.2 "Files written by Little SQL pass sqlite3's integrity check" scripts/write-check.sh
fi

# ---------------------------------------------------------------- M8
if want M8; then
  check A8.1 "Transactions roll back; WAL recovery matches sqlite3" scripts/crash-check.sh
fi

# ---------------------------------------------------------------- M9
if want M9; then
  check A9.1 "Benchmarks run" scripts/bench.sh --quick
fi

echo
if [ ${#FAILED[@]} -eq 0 ]; then
  echo "All $PASSED checks passed (milestones up to $UPTO)."
else
  echo "$PASSED passed, ${#FAILED[@]} failed: ${FAILED[*]}"
  exit 1
fi
