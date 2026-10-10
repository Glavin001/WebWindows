/*
 * The unix call plumbing for gles_thunks.c (generated) and the OpenGL ES
 * functions that need guest memory: strings, which gl4es keeps pointers to,
 * and buffer mapping, which WebGL 2 does not have (a mapping is a guest copy
 * read with getBufferSubData and written back with bufferSubData).
 *
 * Copyright 2026 the WebWindows authors
 *
 * This library is free software; you can redistribute it and/or
 * modify it under the terms of the GNU Lesser General Public
 * License as published by the Free Software Foundation; either
 * version 2.1 of the License, or (at your option) any later version.
 */
#include <stdlib.h>
#include "gles.h"

typedef NTSTATUS (WINAPI *unix_dispatcher)(UINT64 handle, unsigned int code, void *args);
typedef NTSTATUS (WINAPI *query_memory)(HANDLE process, const void *addr, int cls, void *info, SIZE_T size,
        SIZE_T *ret);

static unix_dispatcher *dispatcher;
static UINT64 handle;

BOOL gles_init(HMODULE module)
{
    HMODULE ntdll = GetModuleHandleA("ntdll.dll");
    query_memory query = (query_memory)GetProcAddress(ntdll, "NtQueryVirtualMemory");

    if (handle)
        return TRUE;
    dispatcher = (unix_dispatcher *)GetProcAddress(ntdll, "__wine_unix_call_dispatcher");
    /* MemoryWineUnixFuncs: the module's unix library. */
    if (!dispatcher || !query || query(GetCurrentProcess(), module, 1000, &handle, sizeof(handle), NULL))
        handle = 0;
    return !!handle;
}

NTSTATUS gles_call(DWORD code, DWORD *words)
{
    return handle ? (*dispatcher)(handle, code, words) : 0xc0000002; /* STATUS_NOT_IMPLEMENTED */
}

void *gles_proc(const char *name)
{
    const struct gles_entry *e;
    size_t len = strlen(name);

    for (e = gles_entries; e->name; ++e)
        if (!strcmp(e->name, name))
            return e->proc;
    /* gl4es asks for some core functions with an extension's suffix. */
    if (len > 3 && (!strcmp(name + len - 3, "OES") || !strcmp(name + len - 3, "EXT")))
    {
        char core[128];

        if (len - 3 >= sizeof(core))
            return NULL;
        memcpy(core, name, len - 3);
        core[len - 3] = 0;
        return gles_proc(core);
    }
    return NULL;
}

/* ---- Strings ------------------------------------------------------------ */

struct string
{
    GLenum name;
    GLuint index;
    char *text;
};

static struct string *strings;
static unsigned int string_count;

static const GLubyte *get_string(GLenum name, GLuint index)
{
    DWORD w[5];
    unsigned int i;
    char *text;

    for (i = 0; i < string_count; ++i)
        if (strings[i].name == name && strings[i].index == index)
            return (const GLubyte *)strings[i].text;
    w[1] = name;
    w[2] = index;
    w[3] = 0;
    w[4] = 0;
    if (gles_call(GLES_STRING, w) || !(text = malloc(w[0] + 1)))
        return NULL;
    w[1] = name;
    w[2] = index;
    w[3] = (DWORD)(ULONG_PTR)text;
    w[4] = w[0] + 1;
    gles_call(GLES_STRING, w);
    if (!(strings = realloc(strings, (string_count + 1) * sizeof(*strings))))
        return NULL;
    strings[string_count].name = name;
    strings[string_count].index = index;
    strings[string_count].text = text;
    return (const GLubyte *)strings[string_count++].text;
}

const GLubyte *GL_APIENTRY g_glGetString(GLenum name)
{
    return get_string(name, ~0u);
}

const GLubyte *GL_APIENTRY g_glGetStringi(GLenum name, GLuint index)
{
    return get_string(name, index);
}

/* ---- Buffer mapping ----------------------------------------------------- */

struct mapping
{
    GLenum target;
    GLintptr offset;
    GLsizeiptr length;
    GLbitfield access;
    void *data;
};

static struct mapping mappings[16];

static struct mapping *find_mapping(GLenum target)
{
    unsigned int i;

    for (i = 0; i < ARRAY_SIZE(mappings); ++i)
        if (mappings[i].data && mappings[i].target == target)
            return &mappings[i];
    return NULL;
}

static void write_back(struct mapping *m, GLintptr offset, GLsizeiptr length)
{
    void (GL_APIENTRY *sub_data)(GLenum, GLintptr, GLsizeiptr, const GLvoid *) = gles_proc("glBufferSubData");

    sub_data(m->target, m->offset + offset, length, (const BYTE *)m->data + offset);
}

void *GL_APIENTRY g_glMapBufferRange(GLenum target, GLintptr offset, GLsizeiptr length, GLbitfield access)
{
    struct mapping *m = NULL;
    unsigned int i;
    DWORD w[6];

    if (find_mapping(target))
        return NULL;
    for (i = 0; i < ARRAY_SIZE(mappings) && !m; ++i)
        if (!mappings[i].data)
            m = &mappings[i];
    if (!m || !(m->data = malloc(length ? length : 1)))
        return NULL;
    m->target = target;
    m->offset = offset;
    m->length = length;
    m->access = access;
    if (access & GL_MAP_READ_BIT)
    {
        w[1] = target;
        w[2] = offset;
        w[3] = length;
        w[4] = (DWORD)(ULONG_PTR)m->data;
        gles_call(GLES_GET_BUFFER, w);
    }
    return m->data;
}

void GL_APIENTRY g_glFlushMappedBufferRange(GLenum target, GLintptr offset, GLsizeiptr length)
{
    struct mapping *m = find_mapping(target);

    if (m && offset >= 0 && offset + length <= m->length)
        write_back(m, offset, length);
}

GLboolean GL_APIENTRY g_glUnmapBuffer(GLenum target)
{
    struct mapping *m = find_mapping(target);

    if (!m)
        return GL_FALSE;
    if ((m->access & GL_MAP_WRITE_BIT) && !(m->access & GL_MAP_FLUSH_EXPLICIT_BIT))
        write_back(m, 0, m->length);
    free(m->data);
    m->data = NULL;
    return GL_TRUE;
}

void GL_APIENTRY g_glGetBufferPointerv(GLenum target, GLenum pname, GLvoid **params)
{
    struct mapping *m = find_mapping(target);

    *params = m ? m->data : NULL;
}
