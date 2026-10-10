# Milestone 3 — fully in the browser

Status as of October 7, 2026.

## Done-when criteria

| Criterion (from the plan) | Status | Evidence |
| --- | --- | --- |
| Picking a folder in Chrome runs a console program | Done | `node tests/web/picker.mjs`: headless Chromium picks a folder holding `bin/readfile.exe` and `bin/data.txt`; the page lists the folder's programs, runs the chosen one on translated Wine with the folder as `C:\app` and its own directory as the working directory, and the program prints the data file next to it. |
| The second launch skips translation | Done | Same test: the first launch translates the `.exe` in the browser (~190 ms); the second loads the cached translation from the origin private file system. Wine's DLLs come pre-translated and compiled through streaming compilation, which the browser caches. |
| Test layer 3 starts with Wine's `kernel32` tests | Started | `node tests/wine/winetest.mjs kernel32_test.exe` runs each of the 33 units on translated Wine; results are tracked against `tests/wine/baseline/kernel32_test.json`, and CI fails when a unit gets worse. |

The translator compiled to WebAssembly and fast mode were done during M1
(see [milestone-1.md](milestone-1.md)).

The folder picker is a native dialog that automation cannot click, so the
test supplies a directory from the origin private file system through
`showDirectoryPicker`, the same `FileSystemDirectoryHandle` the real picker
returns; everything after that is the page's own code. Browsers without
`showDirectoryPicker` use a `webkitdirectory` file input.

## Wine's kernel32 tests (baseline)

| | |
| --- | --- |
| Units | 33 |
| Finished | 14 (4 with no failures: `codepage`, `format_msg`, `generated`, `power`) |
| Crashed or timed out | 19 |
| Checks executed in finished units | 997,140 |
| Failed | 774 (99.92% pass) |

Most of the crashes are expected at this stage: the tests deliberately
raise exceptions (SEH dispatch is M5), start threads (M5) and child
processes, or use objects that live in wineserver (atoms, named pipes,
mailslots, change notifications), which arrives with M4.

## Found and fixed on the way

* **Lost output.** The Node hosts wrote guest output with
  `process.stdout.write`, which only queues the bytes when stdout is a
  pipe; the guest runs in one synchronous call, and the `process.exit`
  after it dropped what was queued. Large outputs (a test unit's summary
  line) were cut off only when piped. Output is now written synchronously.
* **Far pointers.** Data decoded as code produced `jmp far [m]`, whose
  6-byte operand reached code generation as an ordinary load and stopped
  the translator. Far-pointer operands are now unsupported instructions
  (a regression test covers them).
* **Stale translations.** The Node Wine runner cached translations by image
  content only; the key now includes the translator build. The page's cache
  keyed by the translator's ABI version only; its key now includes a hash of
  the translator module too.
* **A game's own DLLs were translated on every launch.** The page cached the
  `.exe` only; Far Cry's is a 27 KB stub, and its 27 DLLs took 50 s to
  translate each time (64 s to the menu, warm or cold). They are now cached
  by content in the origin private file system, with the base each was
  translated at (games' DLLs share bases, and the loader moves them to the
  same place each launch). The program loads DLLs synchronously, without
  returning to the worker's event loop, so cached translations are read and
  compiled before it starts and new ones are written through synchronous
  file handles opened beforehand. Far Cry reaches its menu in 13 s on the
  second launch (screen up in 3 s, from 37 s). `tests/web/picker.mjs` covers
  it with a program and two DLLs at one base.

## Not yet measured

* The plan's M3 measurements — how much code the first ahead-of-time pass
  finds across 20 games, and launch time for a large game, including
  whether a service worker lets the browser cache compiled code for modules
  generated in the page — need games, which need M4 and M5.
