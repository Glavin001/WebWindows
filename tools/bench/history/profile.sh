#!/bin/bash
# CPU profiles of the suite's programs on the last checkpoint, for the
# speed log page: tools/bench/history/profile.sh (after build.sh).
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../../.." && pwd)
H=${HIST_DIR:-$root/target/history}
R=$H/wt-$(tail -1 "$here/checkpoints.txt" | cut -d' ' -f2)
B=$root/target/bench W=$root/tools/bench/workloads O=$H/prof
mkdir -p "$O"
cd "$(mktemp -d)"
p() { WWT=$R/target/release/wwt node "$R/tools/bench/profile.mjs" --top 12 -- node "$R/runtime/node/wine.mjs" "$@"; }
p "$B/suite-coremark.exe" > "$O/coremark.txt" 2>&1
p "$B/suite-sqlite.exe" --verify --size 50 speedtest.db > "$O/sqlite.txt" 2>&1
p --file "$W/bench.lua=C:\\bench.lua" "$B/suite-lua.exe" 'C:\bench.lua' 4 > "$O/lua.txt" 2>&1
for b in heap malloc files seek strings sync qsort; do p "$B/suite-apibench.exe" 4 $b > "$O/api-$b.txt" 2>&1; done
