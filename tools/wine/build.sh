#!/bin/sh
# Fetches the pinned Wine release and builds the i386 PE DLLs the runtime
# translates (Milestone 2). Only Wine's Windows side is used; its Unix side
# is replaced by runtime/wine.
#
#   tools/wine/build.sh [dll ...]      # default: the DLLs a console app needs
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
targets=""
for d in $DLLS; do targets="$targets dlls/$d/i386-windows/$d.dll"; done
make -j"$(nproc)" $targets
for d in $DLLS; do ls -la "dlls/$d/i386-windows/$d.dll"; done
