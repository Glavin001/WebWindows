# Real games

Games run from their own folder, as the user's copy: in the browser through
the page's folder picker, in Node with `runtime/node/wine.mjs --folder`. None
of their files are in the repository.

| Game | Graphics | State |
| --- | --- | --- |
| Cave Story (freeware) | DirectDraw | Playable with sound (Milestone 5) |
| Quake II demo 3.14 | Software renderer (`ref_soft`, DirectDraw/GDI) | `demo1` renders and runs, in Node and in the browser |
| Quake (shareware) on FTEQW | Direct3D 9 | Starts; exits before its window opens |

## Quake II demo

The demo is `q2-314-demo-x86.exe` (39,015,499 bytes, SHA-1
`5b4dedc59ceee306956a3e48a8bdf6dd33bc91ed`), id Software's self-extracting
ZIP, mirrored for example at
`https://ftp.gwdg.de/pub/misc/ftp.idsoftware.com/idstuff/quake2/`. Unzip it
(`unzip q2-314-demo-x86.exe 'Install/Data/*'`) and use `Install/Data` as the
game folder:

```sh
node runtime/node/wine.mjs --folder --screenshot q2.png --run-for 120000 \
  Install/Data/quake2.exe +set vid_ref soft +set vid_fullscreen 0 +map demo1
```

On the page: choose the `Install/Data` folder, pick `quake2.exe`, give the
same arguments, tick "on Wine" and run.

Quake II has no Direct3D renderer: `ref_soft` draws with DirectDraw or a DIB
section, `ref_gl` with OpenGL, which this Wine is built without (an OpenGL
layer over WebGPU would be its own project).

What it needed:

* **Winsock.** `quake2.exe` imports `wsock32`; Wine's `ws2_32`, `wsock32`,
  `iphlpapi`, `dnsapi` and `nsi` are in the bundle's media group, and the host
  answers `ws2_32`'s Unix calls (name lookups) with no network. Single-player
  talks to its own server in memory.
* **Self-modifying code.** The software renderer patches immediates in its
  assembly every frame. Each patch drops the page's translations; fast mode
  translates within the code section around a miss and keeps translations by
  content, so the few versions the code cycles through are translated once.
  (Translating a 256 KB window each time, data included, ran Node out of
  memory in a minute.)
* **Threads.** See below.

## Quake on FTEQW

FTEQW (`fteqw.exe` from fte.triptohell.info, GPL) with the shareware
`id1/pak0.pak`, run with `+vid_renderer d3d9`. It loads `d3d9.dll` and
`d3dcompiler_47.dll` (built and bundled for programs that compile HLSL at run
time). It now gets past its worker-thread setup and the game data check, then
exits with code 5 before opening its window; not looked into yet.

## Fixes these games found

* **Waits completed only when nothing else could run.** With threads always
  ready, a blocked thread whose wait was satisfied never got its turn back
  (two FTEQW workers spinning on a set event starved the ones woken to reset
  it). Waits are now completed at least once a slice and right after an event
  is set or a semaphore or mutex released.
* **Spinning without system calls.** FTEQW's main thread waits for its
  workers in a loop that makes no system calls. Translated loops now check
  the thread's slice deadline at their back edges and yield (ABI version 5:
  a `preempt` import, a `tick` address, `cpu.PREEMPT_AT`).
* **`*.*` matched nothing.** Wine's `FindFirstFile` turns `*.*` into NT's
  DOS wildcards (`<`, `>`, `"`), which the host's directory listing now
  understands; it also answers the `FileIdExtdBothDirectoryInformation` class
  Wine 11 asks for.
