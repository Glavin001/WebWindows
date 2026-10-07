# M1 spikes

The memory design rests on two facts the plan marked unconfirmed. Both are
checked here and in CI.

## 1. Shared memory size (`memory/run.mjs`)

Allocates shared WebAssembly memories of growing size and stores/loads at an
address above 2 GB from both WebAssembly and JavaScript.

Result on Node 22 (V8 12.4), October 2026:

| Memory | Allocated | Addresses above 2 GB |
| --- | --- | --- |
| 1 GB guest + 64 MB native | yes | — |
| 2 GB guest + 64 MB native | yes | work |
| 3 GB | yes | work |
| 4 GB − 64 KB | yes | work |

V8 accepts shared memories up to the full 32-bit range, so the default layout
(1 GB guest limit) and the 2 GB ceiling both fit. Still open: Safari's and
Firefox's practical limits (run the same checks there before M8).

## 2. Emscripten above the guest limit (`emscripten/build.sh`)

Builds a C program with `-sGLOBAL_BASE=<guest limit>` and pthreads, then
checks where static data, BSS, the main stack, `malloc` (small and 64 MB)
and a thread's stack land, and that native code can read and write guest
addresses below the limit through plain pointers.

Result with Emscripten (latest, October 2026), guest limits of 1 GB and
2 GB: **every region lands above the limit**, guest accesses work, and
pointers above 2 GB need only `-sMAXIMUM_MEMORY=4GB` (the JavaScript glue
handles them unsigned). The plan's fallback (custom linker layout and
allocator) is not needed.

```
== guest limit 2147483648
static data    0x800002b0 above limit
bss            0x800003b0 above limit
main stack     0x80200f2c above limit
malloc small   0x80201580 above limit
malloc 64 MB   0x802015c8 above limit
thread stack   0x8430385c above limit
guest access   ok
RESULT PASS
```

Note for M2: with `GLOBAL_BASE` at the limit, `INITIAL_MEMORY` must cover
the whole guest region, so the browser reserves it up front (it is committed
lazily by the OS).
