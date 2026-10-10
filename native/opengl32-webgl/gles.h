/*
 * opengl32 for WebWindows on WebGL 2: the OpenGL ES 3.0 calls gl4es makes,
 * executed by the host (runtime/wine/webgl.mjs) through unix calls.
 *
 * Copyright 2026 the WebWindows authors
 *
 * This library is free software; you can redistribute it and/or
 * modify it under the terms of the GNU Lesser General Public
 * License as published by the Free Software Foundation; either
 * version 2.1 of the License, or (at your option) any later version.
 */
#ifndef WEBWINDOWS_GLES_H
#define WEBWINDOWS_GLES_H

#include <windows.h>
#include <string.h>
#include <GLES3/gl3.h>

#ifndef ARRAY_SIZE
#define ARRAY_SIZE(a) (sizeof(a) / sizeof((a)[0]))
#endif

/* Unix calls (runtime/wine/webgl.mjs). Each takes an array of 32-bit words;
 * for a GL function, word 0 receives its result and the arguments follow. */
#define GLES_OPEN 0x1000           /* {status} */
#define GLES_MAKE_CURRENT 0x1001   /* {0, hwnd, width, height} */
#define GLES_PRESENT 0x1002        /* {0, width, height, bgra pixels (bottom-up)} */
#define GLES_STRING 0x1003         /* {length, name, index or ~0, buffer, size} */
#define GLES_GET_BUFFER 0x1004     /* {0, target, offset, size, destination} */
#define GLES_CALL_BASE 0x1100      /* + the function's index in gles-table.mjs */

struct gles_entry
{
    const char *name;
    void *proc;
};

/* Every OpenGL ES 3.0 function, for gl4es's GetProcAddress. */
extern const struct gles_entry gles_entries[];

BOOL gles_init(HMODULE module);
NTSTATUS gles_call(DWORD code, DWORD *words);
void *gles_proc(const char *name);

/* The hand-written ones (gles.c). */
const GLubyte *GL_APIENTRY g_glGetString(GLenum name);
const GLubyte *GL_APIENTRY g_glGetStringi(GLenum name, GLuint index);
void *GL_APIENTRY g_glMapBufferRange(GLenum target, GLintptr offset, GLsizeiptr length, GLbitfield access);
GLboolean GL_APIENTRY g_glUnmapBuffer(GLenum target);
void GL_APIENTRY g_glFlushMappedBufferRange(GLenum target, GLintptr offset, GLsizeiptr length);
void GL_APIENTRY g_glGetBufferPointerv(GLenum target, GLenum pname, GLvoid **params);

#endif
