#!/bin/sh
# Shrinks a failing program to the smallest one that still fails, with cvise.
#
#   tests/programs/reduce.sh target/program-failures/csmith-1066-O0.c O0
#
# The "still fails" test: native and translated outputs differ (or the
# translator crashes) while the native build still compiles and terminates.
set -e
src=$(realpath "$1")
opt=${2:-O0}
root=$(cd "$(dirname "$0")/../.." && pwd)
work=$(mktemp -d)
cp "$src" "$work/prog.c"
cat > "$work/test.sh" <<EOT
#!/bin/sh
set -e
gcc -m32 -$opt -w -I$root/tests/csmith/runtime prog.c $root/tests/programs/pc53.c -o native 2>/dev/null
i686-w64-mingw32-gcc -$opt -w -I$root/tests/csmith/runtime prog.c -o prog.exe 2>/dev/null
timeout 5 ./native > want.txt || exit 1
if ! $root/target/debug/wwt translate prog.exe -o prog.wasm 2>/dev/null; then exit 0; fi
timeout 30 node $root/runtime/node/run.mjs --wasm prog.wasm prog.exe > got.txt 2>/dev/null || true
! cmp -s want.txt got.txt
EOT
chmod +x "$work/test.sh"
cd "$work" && cvise --n 4 ./test.sh prog.c
echo "reduced program: $work/prog.c"
