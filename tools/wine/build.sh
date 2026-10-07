#!/bin/sh
# Fetches the pinned Wine release and builds the i386 PE DLLs the runtime
# translates (Milestone 2). Only Wine's Windows side is used; its Unix side
# is replaced by runtime/wine.
#
#   tools/wine/build.sh [dll ...]      # default: the DLLs a console app needs
#   tools/wine/build.sh programs/cmd   # one of Wine's programs (cmd.exe)
#   tools/wine/build.sh kernel32/tests # a DLL's conformance tests
#   tools/wine/build.sh fonts          # the bitmap fonts (.fon) Wine generates;
#                                      # needs FreeType's headers (libfreetype-dev)
#
# Environment: WINE_SRC (default /opt/wine-src/wine-$VERSION), WINE_BUILD
# (default /opt/wine-build). Needs gcc, flex, bison and gcc-mingw-w64-i686.
set -e
VERSION=11.0
WINE_SRC=${WINE_SRC:-/opt/wine-src/wine-$VERSION}
WINE_BUILD=${WINE_BUILD:-/opt/wine-build}
DLLS=${*:-"ntdll kernelbase kernel32 msvcrt ucrtbase"}

if [ ! -d "$WINE_SRC" ]; then
  mkdir -p "$(dirname "$WINE_SRC")"
  curl -sSfL "https://dl.winehq.org/wine/source/$VERSION/wine-$VERSION.tar.xz" | tar xJ -C "$(dirname "$WINE_SRC")"
fi
# wined3d's WebGPU backend (native/wined3d-wgpu): hooks patched in once, the
# backend and the d3dgpu protocol header copied next to wined3d's sources.
root=$(cd "$(dirname "$0")/../.." && pwd)
wpatch=$root/native/wined3d-wgpu/wined3d-wgpu.patch
if ! cmp -s "$wpatch" "$WINE_SRC/.wined3d-wgpu.patch"; then
  # A changed patch replaces the one applied before.
  [ ! -f "$WINE_SRC/.wined3d-wgpu.patch" ] || patch -d "$WINE_SRC" -p1 -R < "$WINE_SRC/.wined3d-wgpu.patch"
  patch -d "$WINE_SRC" -p1 < "$wpatch"
  cp "$wpatch" "$WINE_SRC/.wined3d-wgpu.patch"
fi
for f in native/wined3d-wgpu/adapter_wgpu.c native/wined3d-wgpu/wined3d_nogl.c crates/d3dgpu-proto/include/d3dgpu_proto.h; do
  cmp -s "$root/$f" "$WINE_SRC/dlls/wined3d/$(basename "$f")" || cp "$root/$f" "$WINE_SRC/dlls/wined3d/"
done
mkdir -p "$WINE_BUILD"
cd "$WINE_BUILD"
if [ ! -f Makefile ]; then
  "$WINE_SRC/configure" --enable-archs=i386 --without-x --without-freetype --without-wayland \
    --without-vulkan --without-gstreamer --without-pulse --without-alsa --without-oss --without-cups \
    --without-dbus --without-gnutls --without-sane --without-usb --without-v4l2 --without-pcap \
    --without-netapi --without-krb5 --without-gssapi --without-opencl --without-sdl --without-udev \
    --without-unwind --without-capi --without-gphoto --without-inotify --without-xinerama \
    --without-fontconfig --without-opengl --without-pcsclite --without-ffmpeg > configure.log
fi
target() {
  case $1 in
    programs/*) n=${1#programs/}; case $n in *.*) echo "$1/i386-windows/$n" ;; *) echo "$1/i386-windows/$n.exe" ;; esac ;;
    */tests) d=${1%/tests}; echo "dlls/$1/i386-windows/${d}_test.exe" ;;
    *) echo "dlls/$1/i386-windows/$1.dll" ;;
  esac
}
targets=""
keep=""
for d in $DLLS; do
  if [ "$d" = fonts ]; then
    # sfnt2fon converts the TrueType sources; Wine was configured without
    # FreeType (the browser build brings its own), so build it here.
    mkdir -p tools/sfnt2fon
    gcc -O2 -o tools/sfnt2fon/sfnt2fon "$WINE_SRC/tools/sfnt2fon/sfnt2fon.c" -Itools/sfnt2fon -Iinclude \
      -I"$WINE_SRC/include" -D__WINESRC__ -DHAVE_FT2BUILD_H -DSONAME_LIBFREETYPE='"libfreetype.so.6"' \
      $(pkg-config --cflags --libs freetype2)
    targets="$targets $(grep -oE 'fonts/[a-z_0-9]+\.fon' Makefile | sort -u | tr '\n' ' ')"
    # Keep make from relinking it without FreeType.
    keep="-o tools/sfnt2fon/sfnt2fon"
    continue
  fi
  targets="$targets $(target "$d")"
done
make -j"$(nproc)" $keep $targets
for t in $targets; do ls -la "$t"; done
