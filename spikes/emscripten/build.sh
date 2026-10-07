#!/bin/sh
# Builds and runs the spike for 1 GB and 2 GB guest limits.
# Needs Emscripten (emsdk) on PATH, or EMSDK pointing at it.
set -e
cd "$(dirname "$0")"
if [ -n "$EMSDK" ]; then . "$EMSDK/emsdk_env.sh" > /dev/null 2>&1 || true; export PATH="$EMSDK/upstream/emscripten:$PATH"; fi
mkdir -p out
for limit in 1073741824 2147483648; do
  emcc -O2 spike.c -o out/spike-$limit.js \
    -DGUEST_LIMIT=$limit -sGLOBAL_BASE=$limit \
    -pthread -sPTHREAD_POOL_SIZE=2 \
    -sINITIAL_MEMORY=$((limit + 128 * 1024 * 1024)) -sMAXIMUM_MEMORY=4GB -sALLOW_MEMORY_GROWTH \
    -sSTACK_SIZE=1MB -sEXIT_RUNTIME
  echo "== guest limit $limit"
  node out/spike-$limit.js
done
