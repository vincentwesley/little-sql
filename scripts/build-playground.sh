#!/usr/bin/env bash
# Build the static playground into playground/dist/:
#   index.html, style.css, app.js, worker.js, engine.js  (copied)
#   littlesql.wasm   (the engine, built for wasm32-unknown-unknown)
#   demo.db          (scripts/build-demo-db.sh)
#   results.json     (docs/results/results.json, see publish-results.mjs)
#
# The result is a plain static site: serve the directory with any web
# server (for example `python3 -m http.server -d playground/dist`).
# Needs: cargo with the wasm32-unknown-unknown target, sqlite3.
set -euo pipefail
cd "$(dirname "$0")/.."
OUT=playground/dist

cargo build --release --target wasm32-unknown-unknown -p littlesql-wasm -q
[ -f target/demo/hdb.db ] || scripts/build-demo-db.sh target/demo/hdb.db >/dev/null

rm -rf "$OUT"
mkdir -p "$OUT"
cp playground/index.html playground/style.css playground/app.js \
   playground/worker.js playground/engine.js "$OUT/"
cp target/wasm32-unknown-unknown/release/littlesql_wasm.wasm "$OUT/littlesql.wasm"
cp target/demo/hdb.db "$OUT/demo.db"
cp docs/results/results.json "$OUT/results.json"
echo "playground built in $OUT:"
ls -l "$OUT" | awk 'NR > 1 { printf "  %-16s %10d bytes\n", $9, $5 }'
