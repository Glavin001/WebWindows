/* Shared by opengl32.c and the generated stubs.c. */
#ifndef WEBWINDOWS_OPENGL32_H
#define WEBWINDOWS_OPENGL32_H

/* This DLL defines the gl and wgl functions the headers declare: without
 * _GDI32_ they would be declared dllimport. */
#define _GDI32_
#include <windows.h>
#include <GL/gl.h>

void gl_unimplemented(const char *name);

#endif
