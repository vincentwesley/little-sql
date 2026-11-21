#!/usr/bin/env bash
# Benchmarks: the same SQL through Little SQL and sqlite3, best of N runs
# (each run is a fresh process, so times include start-up and opening).
#
#   scripts/bench.sh            # 5 runs each, 200 000-row write tests
#   scripts/bench.sh --quick    # 1 run each, 20 000-row write tests
#
# Reads use the demo database (scripts/build-demo-db.sh). Prints a
# markdown table; also written to results/bench.md.
set -euo pipefail
cd "$(dirname "$0")/.."
RUNS=5
N=200000
if [ "${1:-}" = "--quick" ]; then RUNS=1; N=20000; fi
cargo build --release -q -p littlesql-cli
[ -f target/demo/hdb.db ] || scripts/build-demo-db.sh >/dev/null
LSQL=target/release/littlesql
DEMO=target/demo/hdb.db
T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT

now() { date +%s.%N; }
best() { # best ENGINE DB SQL -> seconds (best of RUNS); DB "fresh" = new file each run
  local engine="$1" db="$2" sql="$3" b=999999
  for _ in $(seq 1 "$RUNS"); do
    local file="$db"
    if [ "$db" = fresh ]; then file="$T/w-$engine.db"; rm -f "$file" "$file-wal" "$file-shm"; fi
    local t0 t1
    t0=$(now)
    if [ "$engine" = littlesql ]; then "$LSQL" "$file" "$sql" > /dev/null; else sqlite3 "$file" "$sql" > /dev/null; fi || { echo "benchmark failed ($engine): ${sql:0:80}" >&2; exit 1; }
    t1=$(now)
    b=$(echo "$t1 - $t0" | bc -l | awk -v b="$b" '{ print ($1 < b) ? $1 : b }')
  done
  echo "$b"
}

SEED="CREATE TABLE t(id INTEGER PRIMARY KEY, k INT, s TEXT, r REAL);
WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < $N)
INSERT INTO t(k, s, r) SELECT (i * 7919) % 100000, 'row ' || i, i / 7.0 FROM n;"
SEED="${SEED//$'\n'/ }"  # one line: the case list is split with read

US=$'\x1f'  # field separator (SQL contains | and ||)
declare -a CASES=(
  "Scan + GROUP BY (128k rows, view join)${US}$DEMO${US}SELECT town, count(*), avg(price) FROM sales GROUP BY town"
  "Sort, top 10${US}$DEMO${US}SELECT * FROM resale ORDER BY price DESC LIMIT 10"
  "Index range search${US}$DEMO${US}SELECT count(*), avg(price) FROM resale WHERE town_id = 7 AND month BETWEEN '2024-01' AND '2024-12'"
  "Covering index scan${US}$DEMO${US}SELECT flat_type, min(price), max(price) FROM resale GROUP BY flat_type"
  "LIKE filter, full scan${US}$DEMO${US}SELECT count(*) FROM resale WHERE street LIKE '%AVE 3%'"
  "Correlated subquery (27 towns)${US}$DEMO${US}SELECT name, (SELECT max(price) FROM resale r WHERE r.town_id = t.id) FROM towns t"
  "Join + GROUP BY region${US}$DEMO${US}SELECT t.region, r.flat_type, count(*) FROM resale r JOIN towns t ON t.id = r.town_id GROUP BY 1, 2"
  "Bulk INSERT ... SELECT ($N rows)${US}fresh${US}$SEED"
  "Bulk insert + CREATE INDEX${US}fresh${US}$SEED CREATE INDEX t_k ON t(k);"
  "Bulk insert + UPDATE half${US}fresh${US}$SEED UPDATE t SET r = r * 2 WHERE id % 2 = 0;"
  "Bulk insert + DELETE half${US}fresh${US}$SEED DELETE FROM t WHERE k < 50000;"
)

out="| Benchmark | Little SQL | sqlite3 | Ratio |\n|---|---:|---:|---:|\n"
for c in "${CASES[@]}"; do
  IFS="$US" read -r name db sql <<< "$c"
  a=$(best littlesql "$db" "$sql")
  b=$(best sqlite3 "$db" "$sql")
  ratio=$(echo "$a / $b" | bc -l)
  out+=$(printf '| %s | %.3f s | %.3f s | %.1f× |' "$name" "$a" "$b" "$ratio")"\n"
done
mkdir -p results
printf "%b" "$out" | tee results/bench.md
echo
echo "sqlite3 $(sqlite3 --version | cut -d' ' -f1), $RUNS run(s) each, $(nproc) CPUs"
