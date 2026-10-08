#!/bin/sh
# Extracts the shader model 1-3 bytecode that Wine's Direct3D 8/9
# conformance tests embed as DWORD arrays (tag wine-11.0) into
# target/d3d9-corpus/, one .d3dbc file per shader. Not committed (LGPL):
# the stress test in crates/d3dgpu-shader/tests/corpus.rs runs on whatever
# is there. Some of these shaders are deliberately invalid (the tests check
# that CreateShader rejects them).
set -eu
TAG=wine-11.0
root=$(cd "$(dirname "$0")/../.." && pwd)
out="$root/target/d3d9-corpus"
mkdir -p "$out"
for f in d3d9/tests/visual.c d3d9/tests/device.c d3d8/tests/visual.c d3d8/tests/device.c; do
  name=$(echo "$f" | sed 's#/tests/#-#; s#\.c$##')
  src="$out/$name.c"
  [ -s "$src" ] || curl -sSfL "https://raw.githubusercontent.com/wine-mirror/wine/$TAG/dlls/$f" -o "$src"
  python3 -I - "$src" "$out/$name" <<'PY'
import re, struct, sys
text = open(sys.argv[1], encoding='utf-8', errors='replace').read()
# A shader starts with a version token (0xfffe/0xffff, major 1-3) and ends
# with the end token 0x0000ffff.
words = [int(m.group(), 16) for m in re.finditer(r'0x[0-9a-fA-F]{8}', text)]
n = 0
i = 0
while i < len(words):
    w = words[i]
    if (w >> 16) in (0xfffe, 0xffff) and 1 <= (w >> 8) & 0xff <= 3:
        try:
            end = words.index(0x0000ffff, i + 1)
        except ValueError:
            break
        if end - i < 4096:
            open(f"{sys.argv[2]}.{n:03}.d3dbc", 'wb').write(struct.pack(f'<{end - i + 1}I', *words[i:end + 1]))
            n += 1
            i = end + 1
            continue
    i += 1
print(f"{sys.argv[1]}: {n} shaders")
PY
done
