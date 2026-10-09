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
  if [ "$d" = opengl32 ] && [ "$ARCH" = i386 ]; then
    # OpenGL 1.1 over Direct3D 9 (native/opengl32), in place of Wine's
    # opengl32, which needs a host OpenGL driver. The functions it does not
    # implement are generated stubs, from MinGW's GL/gl.h. (x86_64 keeps
    # Wine's: the Direct3D bridge serves i386 so far.)
    mkdir -p dlls/opengl32/i386-windows
    gl_h=$(echo '#include <GL/gl.h>' | i686-w64-mingw32-gcc -E -H -x c - 2>&1 >/dev/null | grep -m1 'GL/gl.h$' | sed 's/^\.* //')
    node "$repo/native/opengl32/gen-stubs.mjs" "$gl_h" "$repo/native/opengl32/opengl32.c" > dlls/opengl32/stubs.c
    i686-w64-mingw32-gcc -O2 -Wall -shared -nostartfiles -Wl,-e,_DllMain@12 -Wl,--kill-at \
      -I"$repo/native/opengl32" -o dlls/opengl32/i386-windows/opengl32.dll \
      "$repo/native/opengl32/opengl32.c" dlls/opengl32/stubs.c -ld3d9 -luser32 -lgdi32 -lkernel32 -lmsvcrt
    ls -la dlls/opengl32/i386-windows/opengl32.dll
    # And OpenGL 2.1 on the browser's WebGL 2 (native/opengl32-webgl), which
    # the browser bundle ships as opengl32.dll: gl4es (pinned, MIT) turns
    # OpenGL into OpenGL ES, whose calls the host runs on WebGL 2.
    gl4es=$WINE_BUILD/gl4es
    gl4es_commit=ec16bedd8819c475326f4f1a3063772c6d986e06
    if [ "$(cat "$gl4es/.built" 2>/dev/null)" != $gl4es_commit ]; then
      rm -rf "$gl4es"
      mkdir -p "$gl4es"
      git -C "$gl4es" init -q
      git -C "$gl4es" fetch -q --depth 1 https://github.com/ptitSeb/gl4es $gl4es_commit
      git -C "$gl4es" checkout -q FETCH_HEAD
      # Two ARB_imaging getters lack the calling convention of their
      # exported aliases, which i686 needs to link.
      sed -i 's/^void gl4es_glGetMinmaxParameter\([if]\)v(/void APIENTRY_GL4ES gl4es_glGetMinmaxParameter\1v(/' \
        "$gl4es/src/gl/getter.c"
      cmake -S "$gl4es" -B "$gl4es/build" -DCMAKE_SYSTEM_NAME=Windows -DCMAKE_C_COMPILER=i686-w64-mingw32-gcc \
        -DCMAKE_RC_COMPILER=i686-w64-mingw32-windres -DCMAKE_BUILD_TYPE=Release -DNOX11=ON -DNOEGL=ON \
        -DSTATICLIB=ON -DNO_LOADER=ON -DNO_INIT_CONSTRUCTOR=ON -DDEFAULT_ES=2 > "$gl4es/cmake.log"
      make -C "$gl4es/build" -j"$(nproc)" > "$gl4es/make.log"
      echo $gl4es_commit > "$gl4es/.built"
    fi
    w=$repo/native/opengl32-webgl
    mkdir -p dlls/opengl32/webgl/GLES3
    cp "$gl4es"/include/GLES/*.h dlls/opengl32/webgl/GLES3/
    node "$w/gen-gles.mjs" "$gl4es/include/GLES/gl3.h" guest > dlls/opengl32/webgl/gles_thunks.c
    node "$w/gen-gles.mjs" "$gl4es/include/GLES/gl3.h" host | cmp -s - "$repo/runtime/wine/gles-table.mjs" ||
      echo "warning: runtime/wine/gles-table.mjs differs from gen-gles.mjs's output" >&2
    i686-w64-mingw32-nm "$gl4es/lib/libOPENGL32.a" > dlls/opengl32/webgl/gl4es.sym
    node "$w/gen-def.mjs" "$WINE_SRC/dlls/opengl32/opengl32.spec" "$w/wgl.c" dlls/opengl32/webgl/gl4es.sym \
      > dlls/opengl32/webgl/opengl32.def
    i686-w64-mingw32-gcc -O2 -Wall -Wno-unused-function -Wno-attributes -shared -static-libgcc \
      -Wl,--enable-stdcall-fixup -Wl,--kill-at -I"$w" -Idlls/opengl32/webgl -I"$gl4es/include" \
      -o dlls/opengl32/i386-windows/opengl32-webgl.dll "$w/wgl.c" "$w/gles.c" dlls/opengl32/webgl/gles_thunks.c \
      dlls/opengl32/webgl/opengl32.def "$gl4es/lib/libOPENGL32.a" -lgdi32 -luser32 -lkernel32
    ls -la dlls/opengl32/i386-windows/opengl32-webgl.dll
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
