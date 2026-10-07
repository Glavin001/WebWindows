#!/bin/sh
# Fetches GCC's execution torture tests (gcc.c-torture/execute) at a pinned
# commit of the GCC 13 release branch into target/gcc-src, without the rest
# of GCC. Run them with:
#
#   node tests/programs/check.mjs --torture target/gcc-src/gcc/testsuite/gcc.c-torture/execute
set -eu
COMMIT=0bba35ae26a574150f6d1ae55f8ab1f4c2314a56
root=$(cd "$(dirname "$0")/../.." && pwd)
dir="$root/target/gcc-src"
if [ "$(git -C "$dir" rev-parse HEAD 2>/dev/null)" = "$COMMIT" ]; then exit 0; fi
rm -rf "$dir"
mkdir -p "$dir"
cd "$dir"
git init -q
git remote add origin https://github.com/gcc-mirror/gcc.git
git sparse-checkout set --no-cone gcc/testsuite/gcc.c-torture/execute/
git fetch -q --depth 1 --filter=blob:none origin "$COMMIT"
git checkout -q FETCH_HEAD
echo "gcc.c-torture/execute: $(ls gcc/testsuite/gcc.c-torture/execute/*.c | wc -l) tests"
