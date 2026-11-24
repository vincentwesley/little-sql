#!/usr/bin/env bash
# Run the whole sqllogictest corpus (622 files, about 5.9 million records)
# in native and read mode, writing results/slt-{native,read}-full.json.
# Fetches the full corpus (1.1 GB) into ./corpus first if needed.
#
#   scripts/slt-full.sh [CORPUS_TEST_DIR]
#
# Then `node scripts/publish-results.mjs` publishes the numbers.
set -euo pipefail
cd "$(dirname "$0")/.."
DIR="${1:-corpus/test}"
if [ "$DIR" = corpus/test ] && [ "$(find corpus/test -name '*.test' 2>/dev/null | wc -l)" -lt 600 ]; then
  scripts/fetch-sqllogictest.sh --full
fi
cargo build --release -q -p slt
for mode in native read; do
  ./target/release/slt run --mode "$mode" --quiet 1 --suite "sqllogictest full corpus" \
    --json "results/slt-$mode-full.json" --failures "results/slt-$mode-full-failures.txt" "$DIR" \
    | tail -2
done
