#!/bin/sh
# Extracts the fxc-compiled shaders that Wine's Direct3D 10/11 conformance
# tests embed as DWORD arrays (tag wine-11.0) into target/dxbc-corpus/, one
# .dxbc file per shader. Not committed: the stress test in
# crates/d3dgpu-dxbc/tests/corpus.rs runs on whatever is there.
set -eu
TAG=wine-11.0
root=$(cd "$(dirname "$0")/../.." && pwd)
out="$root/target/dxbc-corpus"
mkdir -p "$out"
for f in d3d11/tests/d3d11.c d3d10core/tests/d3d10core.c; do
  name=$(basename "$f" .c)
  src="$out/$name.c"
  [ -s "$src" ] || curl -sSfL "https://raw.githubusercontent.com/wine-mirror/wine/$TAG/dlls/$f" -o "$src"
  python3 -I - "$src" "$out/$name" <<'PY'
import re, struct, sys
text = open(sys.argv[1], encoding='utf-8', errors='replace').read()
# Each container starts with the "DXBC" magic and declares its size in
# bytes as its seventh dword; read exactly that many hex words.
tokens = [(m.start(), m.group()) for m in re.finditer(r'0x[0-9a-fA-F]{8}', text)]
n = 0
i = 0
while i < len(tokens):
    if tokens[i][1].lower() != '0x43425844' or i + 7 > len(tokens):
        i += 1
        continue
    size = int(tokens[i + 6][1], 16)
    words = [int(t, 16) for _, t in tokens[i:i + size // 4]]
    if size % 4 == 0 and len(words) == size // 4:
        open(f"{sys.argv[2]}.{n:03}.dxbc", 'wb').write(struct.pack(f'<{len(words)}I', *words))
        n += 1
        i += size // 4
    else:
        i += 1
print(f"{sys.argv[1]}: {n} shaders")
PY
done
