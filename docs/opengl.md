# OpenGL on WebGL 2

Windows programs that draw with OpenGL get an `opengl32.dll` that runs it on
the browser's WebGL 2 (`native/opengl32-webgl`, host side
`runtime/wine/webgl.mjs`).

```
program ──OpenGL 1.x–2.1──▶ opengl32.dll (guest, x86 translated to wasm)
                             ├─ WGL: contexts, pixel formats, swaps (wgl.c)
                             └─ gl4es: OpenGL ─▶ OpenGL ES 2 (fixed function
                                as generated GLSL ES shaders, immediate mode,
                                display lists, texture format conversion …)
                                      │ one unix call per ES call (gles_thunks.c)
                                      ▼
                             runtime/wine/webgl.mjs ──▶ WebGL 2 context
                             (OffscreenCanvas in the program's worker)
```

[gl4es](https://github.com/ptitSeb/gl4es) (MIT, pinned in
`tools/wine/build.sh`) does the hard part: the whole of desktop OpenGL 2.1 and
the extensions games of its era use, fixed function included, in terms of
OpenGL ES 2. It is built as a static library for i686 MinGW and linked into
the DLL with `wgl.c`, with two fixes applied by the build: two getters that
lack their exported aliases' calling convention (an i686 link error), and
the fixed-function arrays (`glVertexPointer` and the rest), which share
their state with generic attributes (`gl_Vertex` is attribute 0) but left
the buffer a `glVertexAttribPointer` had set, so a program mixing shaders
and fixed function drew from the wrong memory.

## The ES calls

`gen-gles.mjs` reads gl4es's `GLES3/gl3.h` and generates both sides of each
of the 246 OpenGL ES 3.0 functions:

- the guest's `t_glX` stubs (`gles_thunks.c`, at build time), which pack the
  arguments into 32-bit words (floats by their bits, 64-bit integers as two
  words) and make unix call `0x1100 + index`;
- the host's table (`runtime/wine/gles-table.mjs`, checked in; the build
  warns when it no longer matches) of names and argument kinds, which
  `WebGLBridge` decodes into calls of its methods of the same names.

The program's threads all run in the page's worker, as does the WebGL 2
context, so every call is synchronous, as with a driver.

What OpenGL ES has and WebGL 2 does not, the bridge provides:

| ES | WebGL 2 | Bridge |
|----|---------|--------|
| integer object names, made on first bind | objects | a table per kind; names are made on bind |
| uniform locations as integers, consecutive in arrays | `WebGLUniformLocation` | integers per program, assigned at link |
| client-side vertex arrays and indices | buffers only | copied into scratch buffers at each draw (the vertex range from the draw, or the largest index; index buffers keep a copy for that) |
| `glMapBufferRange` | none | in the guest (`gles.c`): a copy, read with `getBufferSubData`, written back with `bufferSubData` |
| `glGetString` | strings in JS | the guest caches each string (`GLES_STRING`) |

The extension string names what WebGL 2 does natively in ES terms (NPOT,
depth textures, packed depth-stencil, 32-bit indices, anisotropic filtering,
S3TC where the browser has it). gl4es then targets OpenGL 2.1.

## Presenting

`wglSwapBuffers` reads the drawing buffer back (`GLES_PRESENT`, RGBA to BGRA)
and draws it into the window with `SetDIBitsToDevice`, so it composes with
GDI like any window. On SwiftShader (headless Chromium, no GPU), `glbench`
with 60 textured cubes and 500 particles draws at about 85 fps at 640x480:
2 ms of scene and 7.5 ms of present.

## Node

Node has no WebGL, so Node runs keep `native/opengl32`, OpenGL 1.1 over
Direct3D 9 (and so over wined3d's WebGPU backend, recordable with
`--d3d-record`). The build makes both; the browser bundle
(`runtime/node/wine-bundle.mjs`) ships the WebGL one as `opengl32.dll`.

## Testing

`tests/web/gui.mjs` runs, in headless Chromium:

- `gltri`: depth test, texturing, blending, a 2D overlay (pixel checks);
- `glbench`: textured cubes and particles for 3 seconds;
- `gl2test`: 17 checks, each a draw read back with `glReadPixels`: GLSL
  programs with uniform arrays (by name and by consecutive locations),
  vertex and index buffers, client-side arrays and indices, render to
  texture, BGRA uploads, display lists, alpha test, linear fog, stencil,
  multitexturing, `glCopyTexSubImage2D` and generated mipmaps.

`?unixtrace=gl` on the page logs every ES call with its arguments, result
and GL error, and the client-side data each draw copies.
