#!/bin/sh
# CoreMark on every tier; see tools/bench/coremark.mjs for the options and
# docs/performance.md for the workflow.
#
#   tools/bench/coremark.sh [--runs 3] [--variants] ...
#
# For the browser: node tests/web/browser.mjs target/bench/coremark.exe \
#   "Correct operation validated" --wine
exec node "$(dirname "$0")/coremark.mjs" "$@"
