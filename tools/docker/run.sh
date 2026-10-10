#!/bin/sh
# Runs a command in the build container (tools/docker/Dockerfile), with the
# repository mounted at /work, so what it builds lands in this checkout:
#
#   tools/docker/run.sh tools/docker/build-all.sh      # everything the page needs
#   tools/docker/run.sh tools/wine/build.sh wined3d    # one of Wine's DLLs
#   tools/docker/run.sh node tests/programs/check.mjs  # x86 program tests
#   tools/docker/run.sh                                # a shell
#
# Wine's source and build trees (/opt/wine-src, /opt/wine-build,
# /opt/wine-build64, where its Makefiles expect them), cargo's registry and
# rustup's toolchains are kept in named Docker volumes between runs. The
# image is built on first use and again when the Dockerfile changes (its
# tag is the file's hash). The image is x86-64 (Rosetta on Apple Silicon):
# the program tests compare against native x86 runs.
set -e
repo=$(cd "$(dirname "$0")/../.." && pwd)
tag="webwindows-dev:$(git -C "$repo" hash-object tools/docker/Dockerfile | cut -c1-12)"
if ! docker image inspect "$tag" > /dev/null 2>&1; then
  echo "building $tag (once; several minutes)" >&2
  docker build --platform linux/amd64 -t "$tag" "$repo/tools/docker" >&2
fi
tty=
[ -t 0 ] && [ -t 1 ] && tty=-it
[ $# -gt 0 ] || set -- bash
# A large stack: the translator recurses deeply on some functions.
exec docker run --rm $tty --platform linux/amd64 --ulimit stack=268435456:268435456 \
  -v "$repo":/work -w /work \
  -v webwindows-wine-src:/opt/wine-src \
  -v webwindows-wine-build:/opt/wine-build \
  -v webwindows-wine-build64:/opt/wine-build64 \
  -v webwindows-cargo:/root/.cargo/registry \
  -v webwindows-rustup:/root/.rustup \
  "$tag" bash -c '. /opt/emsdk/emsdk_env.sh > /dev/null 2>&1; exec "$@"' bash "$@"
