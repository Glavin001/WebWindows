#!/bin/sh
# Compiles the HLSL test corpus to DXBC with vkd3d-compiler (vkd3d 1.19 or
# newer) and writes the fixtures next to it. Each .hlsl file lists its
# entry points and profiles on a "// compile:" line.
#
#   VKD3D_COMPILER=/path/to/vkd3d-compiler tools/dxbc/compile.sh [dir]
set -eu
cd "$(dirname "$0")/../.."
VC=${VKD3D_COMPILER:-vkd3d-compiler}
dir=${1:-crates/d3dgpu-dxbc/tests/hlsl}
out=$dir/../fixtures
mkdir -p "$out"
for f in "$dir"/*.hlsl; do
  base=$(basename "$f" .hlsl)
  sed -n 's#^// compile: ##p' "$f" | tr ',' '\n' | while read -r entry profile; do
    [ -n "$entry" ] || continue
    dst="$out/$base.$entry.$profile.dxbc"
    if ! "$VC" -x hlsl -b dxbc-tpf -p "$profile" -e "$entry" -o "$dst" "$f" 2> "$dst.log"; then
      echo "FAILED $base $entry $profile: $(head -3 "$dst.log")"
      rm -f "$dst"
    fi
    rm -f "$dst.log"
  done
done
ls "$out" | wc -l
