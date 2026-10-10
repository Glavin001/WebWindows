# Working in this repository

Notes for agents and people picking the work up. The project itself:
[README.md](README.md). The graphics and games work, its state and its
gotchas: [docs/handoff.md](docs/handoff.md). Performance work on games:
[docs/game-performance.md](docs/game-performance.md).

## Where things run

Building is CPU work and happens in a Linux container; the browser, and so
the GPU, is on the host. The container mounts this checkout, so what it
builds lands in `target/` and `runtime/d3dgpu/pkg`, which the page loads
from the host.

| Where | What |
| --- | --- |
| Build container (`tools/docker/run.sh …`) | the translator, its WebAssembly build, Wine's PE DLLs (MinGW), Wine's Unix side (Emscripten), the Wine bundle, the d3dgpu module, `cargo test`, the x86 program tests (`tests/programs/check.mjs`: their reference runs need an x86 CPU), anything run in Node on translated Wine (`runtime/node/wine.mjs`, `tests/wine/*.mjs`) |
| Host | `node runtime/web/serve.mjs`, Chrome, and the scripts that drive it: `tests/web/*.mjs`, `tools/web/drive.mjs`, `tools/web/bench/*.mjs` |

```sh
tools/docker/run.sh tools/docker/build-all.sh      # everything the page needs (incremental)
tools/docker/run.sh tools/wine/build.sh wined3d    # then: tools/docker/run.sh node runtime/node/wine-bundle.mjs
tools/docker/run.sh cargo test --workspace
tools/docker/run.sh node tests/programs/check.mjs --jobs 8
tools/docker/run.sh                                # a shell in the container

nvm use                                            # Node from .nvmrc (22.22), on the host
node runtime/web/serve.mjs 8080                    # http://localhost:8080/runtime/web/
```

- **The image** (`tools/docker/Dockerfile`) matches CI's runners: Ubuntu
  24.04, x86-64 (Rosetta on Apple Silicon), Node 22, Rust stable, Emscripten
  `EMSDK_VERSION` from `.github/workflows/ci.yml`, MinGW, csmith, binaryen,
  wabt and wasm-tools. `run.sh` builds it on first use and whenever the
  Dockerfile changes.
- **Volumes** keep Wine's source and build trees (`/opt/wine-src`,
  `/opt/wine-build`, `/opt/wine-build64`, where Wine's Makefiles expect
  them), cargo's registry and rustup's toolchains between runs
  (`webwindows-*`; `docker volume ls`).
- **One checkout per agent.** Two sessions in one working tree switch
  branches and commit each other's files. Give each its own git worktree
  (`git worktree add`), and its own Wine trees, since two Wine builds in
  one tree at once corrupt it: `WEBWINDOWS_VOLUMES=webwindows-<name>
  tools/docker/run.sh …` (seed them by copying the default volumes, or let
  the first build make them). Each worktree has its own `target/`, so its
  first `build-all.sh` builds everything, and its own `node_modules`
  (Playwright).
- **`target/` belongs to the container.** It holds Linux binaries
  (`target/release/wwt`) next to the WebAssembly the page loads. To build
  Rust natively on a Mac, for a quick check, use another directory:
  `CARGO_TARGET_DIR=target/host cargo …`.
- **The host needs** Docker (OrbStack on a Mac), Node 22.4 or later (the
  page profiler uses Node's `WebSocket`; `.nvmrc`), Google Chrome, and
  Playwright (`npm install --no-save playwright@1`). The browser tests that
  compile a test program first (`tests/web/gui.mjs`, `tests/web/picker.mjs`)
  also need MinGW on the host.
- **Don't install toolchains with Homebrew on macOS 14.** Homebrew has no
  bottles for it, so every formula compiles from source, and anything that
  depends on Homebrew's Rust or LLVM builds LLVM (hours). Put tools in the
  Dockerfile; Rust tools on the host go through `cargo install`.

## Measuring in the browser

- **Real GPU:** `tools/web/drive.mjs --headed` and
  `tools/web/bench/farcry.mjs --headed` run the installed Google Chrome in a
  window. Headless runs use SwiftShader (no GPU): fine for the CPU side and
  correctness, misleading for frame rates once rendering is no longer the
  bottleneck.
- **Vsync:** Direct3D 9 games present with vsync, which caps them at the
  display's rate; benchmarks measure without it (`?d3dvsync=0`).
- **Where time goes:** `drive.mjs`'s `profile MS` (CPU profiles of the
  page's workers by DLL and function; `--syms target/wine-syms`, Wine's
  unstripped DLLs copied out of the build volume), and the status line's
  `waits:` and `idle` (see docs/game-performance.md).

## Conventions

See [docs/handoff.md §9](docs/handoff.md#9-conventions): imperative commit
subjects with a body that explains the why and the measurements, docs
updated with behaviour, baselines only rewritten for deliberate
improvements, and the relevant suites run before pushing.
