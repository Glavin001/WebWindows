#!/bin/sh
# Fetches the pinned Wine release and builds the i386 PE DLLs the runtime
# translates (Milestone 2), or with ARCH=x86_64 the x86_64 ones for 64-bit
# programs, in a separate build tree. Only Wine's Windows side is used; its
# Unix side is replaced by runtime/wine.
#
#   tools/wine/build.sh [dll ...]      # default: the DLLs a console app needs
#   tools/wine/build.sh programs/cmd   # one of Wine's programs (cmd.exe)
#   tools/wine/build.sh kernel32/tests # a DLL's conformance tests
#   tools/wine/build.sh winepulse.drv  # the audio driver stub (native/audio)
#   tools/wine/build.sh fonts          # the bitmap fonts (.fon) Wine generates;
#                                      # needs FreeType's headers (libfreetype-dev)
#
# Environment: ARCH (i386, the default, or x86_64), WINE_SRC (default
# /opt/wine-src/wine-$VERSION), WINE_BUILD (default /opt/wine-build, or
# /opt/wine-build64 for x86_64). Needs gcc, flex, bison and
# gcc-mingw-w64-i686 (gcc-mingw-w64-x86-64 for x86_64).
set -e
repo=$(cd "$(dirname "$0")/../.." && pwd)
VERSION=11.0
ARCH=${ARCH:-i386}
case $ARCH in
  i386) default_build=/opt/wine-build ;;
  x86_64) default_build=/opt/wine-build64 ;;
  *) echo "ARCH must be i386 or x86_64" >&2; exit 2 ;;
esac
WINE_SRC=${WINE_SRC:-/opt/wine-src/wine-$VERSION}
WINE_BUILD=${WINE_BUILD:-$default_build}
DLLS=${*:-"ntdll kernelbase kernel32 msvcrt ucrtbase"}

if [ ! -d "$WINE_SRC" ]; then
  mkdir -p "$(dirname "$WINE_SRC")"
  curl -sSfL "https://dl.winehq.org/wine/source/$VERSION/wine-$VERSION.tar.xz" | tar xJ -C "$(dirname "$WINE_SRC")"
fi
mkdir -p "$WINE_BUILD"
cd "$WINE_BUILD"
if [ ! -f Makefile ]; then
  # An x86_64 build needs --enable-win64 for its Unix-side tools to be
  # 64-bit; only its PE side is used.
  win64=
  [ "$ARCH" = x86_64 ] && win64=--enable-win64
  "$WINE_SRC/configure" $win64 --enable-archs=$ARCH --without-x --without-freetype --without-wayland \
    --without-vulkan --without-gstreamer --without-pulse --without-alsa --without-oss --without-cups \
    --without-dbus --without-gnutls --without-sane --without-usb --without-v4l2 --without-pcap \
    --without-netapi --without-krb5 --without-gssapi --without-opencl --without-sdl --without-udev \
    --without-unwind --without-capi --without-gphoto --without-inotify --without-xinerama \
    --without-fontconfig --without-opengl --without-pcsclite --without-ffmpeg > configure.log
fi
target() {
  case $1 in
    programs/*) n=${1#programs/}; case $n in *.*) echo "$1/$ARCH-windows/$n" ;; *) echo "$1/$ARCH-windows/$n.exe" ;; esac ;;
    */tests) d=${1%/tests}; echo "dlls/$1/$ARCH-windows/${d}_test.exe" ;;
    *) echo "dlls/$1/$ARCH-windows/$1.dll" ;;
  esac
}
targets=""
keep=""
for d in $DLLS; do
  if [ "$d" = winepulse.drv ]; then
    # The audio driver stub whose Unix side is the browser host
    # (native/audio/winepulse.c).
    mkdir -p "dlls/winepulse.drv/$ARCH-windows"
    if [ "$ARCH" = x86_64 ]; then cc=x86_64-w64-mingw32-gcc; entry=DllMain; else cc=i686-w64-mingw32-gcc; entry=_DllMain@12; fi
    $cc -O2 -shared -nostdlib -Wl,-e,$entry -o "dlls/winepulse.drv/$ARCH-windows/winepulse.drv" "$repo/native/audio/winepulse.c" -lkernel32
    ls -la "dlls/winepulse.drv/$ARCH-windows/winepulse.drv"
    continue
  fi
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
[ -z "$targets" ] || make -j"$(nproc)" $keep $targets
for t in $targets; do ls -la "$t"; done
