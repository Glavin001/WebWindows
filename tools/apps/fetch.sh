#!/bin/sh
# Downloads the real Windows programs tests/wine/apps.mjs runs, 32-bit and
# 64-bit builds, into target/apps/{x86,x64}. Needs curl, unzip and 7z.
#
#   sh tools/apps/fetch.sh
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
apps=$root/target/apps
dl=$apps/dl
mkdir -p "$dl" "$apps/x86" "$apps/x64"
cd "$dl"

get() { [ -s "$(basename "$1")" ] || curl -fsSLO "$1"; }

get https://www.nasm.us/pub/nasm/releasebuilds/2.16.03/win32/nasm-2.16.03-win32.zip
get https://www.nasm.us/pub/nasm/releasebuilds/2.16.03/win64/nasm-2.16.03-win64.zip
get https://www.7-zip.org/a/7z2301-extra.7z
get https://www.sqlite.org/2026/sqlite-tools-win-x64-3530400.zip
get https://curl.se/windows/dl-8.22.0_3/curl-8.22.0_3-win64-mingw.zip
for a in 32 64; do
  for p in putty plink; do
    [ -s "$p$a.exe" ] || curl -fsSL -o "$p$a.exe" "https://the.earth.li/~sgtatham/putty/0.85/w$a/$p.exe"
  done
done

rm -rf x && mkdir x
unzip -oq nasm-2.16.03-win32.zip -d x/n32
unzip -oq nasm-2.16.03-win64.zip -d x/n64
unzip -oq sqlite-tools-win-x64-3530400.zip -d x
unzip -oq curl-8.22.0_3-win64-mingw.zip -d x
7z x -y -ox/7z 7z2301-extra.7z >/dev/null

cp x/n32/nasm-2.16.03/nasm.exe x/n32/nasm-2.16.03/ndisasm.exe "$apps/x86/"
cp x/n64/nasm-2.16.03/nasm.exe x/n64/nasm-2.16.03/ndisasm.exe "$apps/x64/"
cp x/7z/7za.exe "$apps/x86/"
cp x/7z/x64/7za.exe "$apps/x64/"
cp x/sqlite3.exe x/sqldiff.exe "$apps/x64/"
cp x/curl-8.22.0_3-win64-mingw/bin/curl.exe x/curl-8.22.0_3-win64-mingw/bin/trurl.exe "$apps/x64/"
for p in putty plink; do
  cp "$p"32.exe "$apps/x86/$p.exe"
  cp "$p"64.exe "$apps/x64/$p.exe"
done
ls "$apps/x86" "$apps/x64"
