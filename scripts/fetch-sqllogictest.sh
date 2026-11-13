#!/usr/bin/env bash
# Fetch the sqllogictest corpus into ./corpus at a pinned commit.
#
#   scripts/fetch-sqllogictest.sh          # only the CI sample (tests/slt/SAMPLE)
#   scripts/fetch-sqllogictest.sh --full   # every test file (about 1.1 GB)
#
# Source: a GitHub mirror of SQLite's sqllogictest Fossil repository
# (https://sqlite.org/sqllogictest). The tests are public domain.
set -euo pipefail
cd "$(dirname "$0")/.."

REPO=https://github.com/gregrahn/sqllogictest
COMMIT=c93ac416f05635ff5cc490009add936d9753d01d
DEST=corpus

full=0
[ "${1:-}" = "--full" ] && full=1

if [ ! -d "$DEST/.git" ]; then
  rm -rf "$DEST"
  git init -q "$DEST"
  git -C "$DEST" remote add origin "$REPO"
fi
export GIT_LFS_SKIP_SMUDGE=1
git -C "$DEST" config core.sparseCheckout true
if [ $full = 1 ]; then
  echo "test/" > "$DEST/.git/info/sparse-checkout"
else
  grep -v '^#' tests/slt/SAMPLE | sed '/^$/d; s|^|test/|' > "$DEST/.git/info/sparse-checkout"
fi
git -C "$DEST" fetch -q --depth 1 --filter=blob:none origin "$COMMIT"
git -C "$DEST" checkout -q FETCH_HEAD
echo "corpus ready: $(find "$DEST/test" -name '*.test' | wc -l) test files in $DEST/test"
