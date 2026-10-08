/*
 * opengl32.dll for WebWindows: OpenGL 1.1 over Direct3D 9.
 *
 * Wine's own opengl32 needs a host OpenGL driver, and the browser has none.
 * This one turns the fixed-function GL 1.1 that games of the era use (Quake
 * II's ref_gl among them) into Direct3D 9 calls on Wine's d3d9, which
 * already draws with WebGPU (wined3d's adapter_wgpu and the d3dgpu render
 * core). Fixed-function shading, texture management and presentation all
 * come from there.
 *
 * Immediate mode is batched: glBegin/glEnd primitives become triangle (or
 * line, or point) lists and accumulate in one vertex array until a state
 * change, glClear, glFinish or a buffer swap draws them with one
 * DrawPrimitiveUP. Quake II issues thousands of glBegin/glEnd pairs a frame,
 * and each Direct3D draw costs a trip through wined3d.
 *
 * Matrices: GL's column-major array, with column vectors, has the same
 * memory layout as the transposed matrix Direct3D's row vectors need, so
 * GL matrices go to SetTransform as they are. The projection is followed by
 * a fix-up that maps GL's clip-space depth [-w, w] to Direct3D's [0, w] and
 * moves Direct3D 9's pixel centres to GL's.
 *
 * One context, used from one thread at a time.
 */
#define COBJMACROS
#include "gl.h"
#include <d3d9.h>
#include <math.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* Extensions this implementation provides (through wglGetProcAddress). */
#define GL_TEXTURE0_SGIS 0x835E
#define GL_TEXTURE1_SGIS 0x835F
#define GL_TEXTURE0_ARB 0x84C0
#define GL_TEXTURE1_ARB 0x84C1
#define GL_MAX_TEXTURE_UNITS_ARB 0x84E2
#define GL_ACTIVE_TEXTURE_ARB 0x84E0
#define GL_BGR_EXT 0x80E0
#define GL_BGRA_EXT 0x80E1
#define GL_CLAMP_TO_EDGE 0x812F

#define UNITS 2
#define MAX_BATCH 12288 /* vertices drawn at once */
#define MAX_RAW 4096    /* vertices between one glBegin and its glEnd */

/* ------------------------------------------------------------------ */
/* Logging                                                             */

static int (__cdecl *wine_dbg_output)(const char *);

static void gl_log(const char *fmt, ...)
{
    char buf[512];
    va_list ap;
    va_start(ap, fmt);
    _vsnprintf(buf, sizeof(buf) - 1, fmt, ap);
    va_end(ap);
    buf[sizeof(buf) - 1] = 0;
    if (wine_dbg_output) wine_dbg_output(buf);
    else OutputDebugStringA(buf);
}

/* Each unimplemented function and unsupported argument is reported once. */
static const void *reported[256];
static int nreported;

static BOOL first_report(const void *key)
{
    int i;
    for (i = 0; i < nreported; i++)
        if (reported[i] == key) return FALSE;
    if (nreported < 256) reported[nreported++] = key;
    return TRUE;
}

void gl_unimplemented(const char *name)
{
    if (first_report(name)) gl_log("fixme:opengl32: %s not implemented\n", name);
}

static void gl_unsupported(const char *what, GLenum value)
{
    /* Keyed by the message and the value. */
    static struct { const char *what; GLenum value; } seen[64];
    static int nseen;
    int i;
    for (i = 0; i < nseen; i++)
        if (seen[i].what == what && seen[i].value == value) return;
    if (nseen < 64) { seen[nseen].what = what; seen[nseen].value = value; nseen++; }
    gl_log("fixme:opengl32: %s 0x%x not supported\n", what, value);
}

/* ------------------------------------------------------------------ */
/* State                                                               */

struct vertex
{
    float x, y, z;
    D3DCOLOR color;
    float tex[UNITS][2];
};
#define VERTEX_FVF (D3DFVF_XYZ | D3DFVF_DIFFUSE | D3DFVF_TEX2)

enum base_format { BASE_RGB, BASE_RGBA, BASE_ALPHA, BASE_LUMINANCE, BASE_LUMINANCE_ALPHA, BASE_INTENSITY };

struct texture
{
    IDirect3DTexture9 *tex;
    int width, height, levels;
    enum base_format base;
    GLint min_filter, mag_filter, wrap_s, wrap_t;
};

struct unit
{
    BOOL enabled;
    GLuint bound;
    GLint env_mode;
};

struct array
{
    BOOL enabled;
    GLint size;
    GLenum type;
    GLsizei stride;
    const void *pointer;
};

struct context
{
    HDC hdc;
    HWND hwnd;
    IDirect3D9 *d3d;
    IDirect3DDevice9 *device;
    int width, height; /* back buffer */
    BOOL in_scene;

    GLenum matrix_mode;
    float modelview[32][16], projection[8][16], texture[8][16];
    int modelview_top, projection_top, texture_top;
    BOOL transforms_dirty;

    GLint viewport[4];
    float depth_near, depth_far;
    GLint scissor[4];

    /* Capabilities, for glIsEnabled and state derived from several. */
    BOOL blend, alpha_test, depth_test, cull, scissor_test, fog, stencil_test, dither;
    GLenum cull_face, front_face;

    struct unit units[UNITS];
    int active_unit, client_unit;

    struct texture **textures;
    GLuint ntextures;
    GLuint next_name;

    /* Immediate mode. */
    BOOL in_begin;
    GLenum mode;
    struct vertex current;
    float current_rgba[4];
    struct vertex raw[MAX_RAW];
    int nraw;
    struct vertex batch[MAX_BATCH];
    int nbatch;
    D3DPRIMITIVETYPE batch_type;

    struct array vertex_array, color_array, texcoord_array[UNITS];

    D3DCOLOR clear_color;
    float clear_depth;
    DWORD clear_stencil;

    GLint unpack_alignment, unpack_row_length, unpack_skip_rows, unpack_skip_pixels;
    GLint pack_alignment;

    GLenum error;
};

static struct context *ctx;
static int pixel_format;

#define CTX(...) do { if (!ctx) return __VA_ARGS__; } while (0)
#define DEV (ctx->device)

static void set_error(GLenum e)
{
    if (ctx && ctx->error == GL_NO_ERROR) ctx->error = e;
}

/* ------------------------------------------------------------------ */
/* Matrices (GL column-major: element (row r, column c) at [c * 4 + r]) */

static const float identity[16] = { 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1 };

static void mat_mul(float *out, const float *a, const float *b)
{
    float r[16];
    int i, j, k;
    for (j = 0; j < 4; j++)
        for (i = 0; i < 4; i++)
        {
            float s = 0;
            for (k = 0; k < 4; k++) s += a[k * 4 + i] * b[j * 4 + k];
            r[j * 4 + i] = s;
        }
    memcpy(out, r, sizeof(r));
}

static float *current_matrix(void)
{
    switch (ctx->matrix_mode)
    {
    case GL_PROJECTION: return ctx->projection[ctx->projection_top];
    case GL_TEXTURE: return ctx->texture[ctx->texture_top];
    default: return ctx->modelview[ctx->modelview_top];
    }
}

/* ------------------------------------------------------------------ */
/* Drawing                                                             */

static void apply_transforms(void)
{
    float fix[16], proj[16];
    D3DMATRIX m;
    int w = ctx->viewport[2] > 0 ? ctx->viewport[2] : 1;
    int h = ctx->viewport[3] > 0 ? ctx->viewport[3] : 1;

    /* Clip-space z from [-w, w] to [0, w]; x and y by half a pixel, from
     * Direct3D 9's pixel centres (on integers) to GL's (on halves). */
    memcpy(fix, identity, sizeof(fix));
    fix[2 * 4 + 2] = 0.5f;
    fix[3 * 4 + 2] = 0.5f;
    fix[3 * 4 + 0] = -1.0f / w;
    fix[3 * 4 + 1] = 1.0f / h;
    mat_mul(proj, fix, ctx->projection[ctx->projection_top]);

    memcpy(&m, ctx->modelview[ctx->modelview_top], sizeof(m));
    IDirect3DDevice9_SetTransform(DEV, D3DTS_VIEW, &m);
    memcpy(&m, proj, sizeof(m));
    IDirect3DDevice9_SetTransform(DEV, D3DTS_PROJECTION, &m);
    ctx->transforms_dirty = FALSE;
}

static void flush(void)
{
    UINT count;
    if (!ctx || !ctx->nbatch) return;
    if (ctx->transforms_dirty) apply_transforms();
    switch (ctx->batch_type)
    {
    case D3DPT_POINTLIST: count = ctx->nbatch; break;
    case D3DPT_LINELIST: count = ctx->nbatch / 2; break;
    default: count = ctx->nbatch / 3; break;
    }
    if (count) IDirect3DDevice9_DrawPrimitiveUP(DEV, ctx->batch_type, count, ctx->batch, sizeof(struct vertex));
    ctx->nbatch = 0;
}

/* Before any call that changes Direct3D state. */
#define FLUSH() flush()

static D3DPRIMITIVETYPE batch_type_of(GLenum mode)
{
    switch (mode)
    {
    case GL_POINTS: return D3DPT_POINTLIST;
    case GL_LINES: case GL_LINE_STRIP: case GL_LINE_LOOP: return D3DPT_LINELIST;
    default: return D3DPT_TRIANGLELIST;
    }
}

/* Vertices the raw primitive becomes in the batch. */
static int batch_count(GLenum mode, int n)
{
    switch (mode)
    {
    case GL_POINTS: return n;
    case GL_LINES: return n & ~1;
    case GL_LINE_STRIP: return n >= 2 ? 2 * (n - 1) : 0;
    case GL_LINE_LOOP: return n >= 2 ? 2 * n : 0;
    case GL_TRIANGLES: return n - n % 3;
    case GL_TRIANGLE_STRIP: case GL_TRIANGLE_FAN: case GL_POLYGON: return n >= 3 ? 3 * (n - 2) : 0;
    case GL_QUADS: return (n / 4) * 6;
    case GL_QUAD_STRIP: return n >= 4 ? ((n - 2) / 2) * 6 : 0;
    default: return 0;
    }
}

static void emit_raw(void)
{
    struct vertex *out, *v = ctx->raw;
    int n = ctx->nraw, i, need = batch_count(ctx->mode, n);
    if (!need) return;
    if (ctx->nbatch + need > MAX_BATCH) flush();
    if (need > MAX_BATCH) return;
    out = ctx->batch + ctx->nbatch;
    ctx->nbatch += need;
#define PUT(i) (*out++ = v[i])
    switch (ctx->mode)
    {
    case GL_POINTS: case GL_LINES: case GL_TRIANGLES:
        memcpy(out, v, need * sizeof(*v));
        break;
    case GL_LINE_STRIP:
        for (i = 0; i + 1 < n; i++) { PUT(i); PUT(i + 1); }
        break;
    case GL_LINE_LOOP:
        for (i = 0; i < n; i++) { PUT(i); PUT((i + 1) % n); }
        break;
    case GL_TRIANGLE_STRIP:
        for (i = 0; i + 2 < n; i++)
        {
            if (i & 1) { PUT(i + 1); PUT(i); PUT(i + 2); }
            else { PUT(i); PUT(i + 1); PUT(i + 2); }
        }
        break;
    case GL_TRIANGLE_FAN: case GL_POLYGON:
        for (i = 1; i + 1 < n; i++) { PUT(0); PUT(i); PUT(i + 1); }
        break;
    case GL_QUADS:
        for (i = 0; i + 3 < n; i += 4) { PUT(i); PUT(i + 1); PUT(i + 2); PUT(i); PUT(i + 2); PUT(i + 3); }
        break;
    case GL_QUAD_STRIP:
        for (i = 0; i + 3 < n; i += 2) { PUT(i); PUT(i + 1); PUT(i + 3); PUT(i); PUT(i + 3); PUT(i + 2); }
        break;
    }
#undef PUT
}

static void vertex(float x, float y, float z)
{
    struct vertex *v;
    if (!ctx || !ctx->in_begin) return;
    if (ctx->nraw == MAX_RAW)
    {
        gl_unsupported("vertices between glBegin and glEnd above", MAX_RAW);
        return;
    }
    v = &ctx->raw[ctx->nraw++];
    *v = ctx->current;
    v->x = x;
    v->y = y;
    v->z = z;
}

static BYTE to_byte(float f)
{
    return f <= 0 ? 0 : f >= 1 ? 255 : (BYTE)(f * 255.0f + 0.5f);
}

static void color(float r, float g, float b, float a)
{
    CTX();
    ctx->current_rgba[0] = r;
    ctx->current_rgba[1] = g;
    ctx->current_rgba[2] = b;
    ctx->current_rgba[3] = a;
    ctx->current.color = D3DCOLOR_ARGB(to_byte(a), to_byte(r), to_byte(g), to_byte(b));
}

static void texcoord(int unit, float s, float t)
{
    CTX();
    ctx->current.tex[unit][0] = s;
    ctx->current.tex[unit][1] = t;
}

/* ------------------------------------------------------------------ */
/* Textures                                                            */

static struct texture *get_texture(GLuint name, BOOL create)
{
    if (name >= ctx->ntextures)
    {
        GLuint n = ctx->ntextures ? ctx->ntextures : 256;
        struct texture **t;
        if (!create) return NULL;
        while (n <= name) n *= 2;
        t = realloc(ctx->textures, n * sizeof(*t));
        if (!t) return NULL;
        memset(t + ctx->ntextures, 0, (n - ctx->ntextures) * sizeof(*t));
        ctx->textures = t;
        ctx->ntextures = n;
    }
    if (!ctx->textures[name] && create)
    {
        struct texture *t = calloc(1, sizeof(*t));
        if (!t) return NULL;
        t->min_filter = GL_NEAREST_MIPMAP_LINEAR;
        t->mag_filter = GL_LINEAR;
        t->wrap_s = t->wrap_t = GL_REPEAT;
        ctx->textures[name] = t;
    }
    return ctx->textures[name];
}

static D3DTEXTUREADDRESS address_mode(GLint wrap)
{
    return wrap == GL_CLAMP || wrap == GL_CLAMP_TO_EDGE ? D3DTADDRESS_CLAMP : D3DTADDRESS_WRAP;
}

/* Sets up texture stage i from GL texture unit i. */
static void update_stage(int i)
{
    struct unit *u = &ctx->units[i];
    struct texture *t = u->enabled && u->bound ? get_texture(u->bound, FALSE) : NULL;
    DWORD current = i == 0 ? D3DTA_DIFFUSE : D3DTA_CURRENT;
    DWORD min = D3DTEXF_POINT, mip = D3DTEXF_NONE;
    BOOL alpha_only, has_alpha;

    if (!t || !t->tex)
    {
        IDirect3DDevice9_SetTexture(DEV, i, NULL);
        if (i == 0)
        {
            IDirect3DDevice9_SetTextureStageState(DEV, 0, D3DTSS_COLOROP, D3DTOP_SELECTARG1);
            IDirect3DDevice9_SetTextureStageState(DEV, 0, D3DTSS_COLORARG1, D3DTA_DIFFUSE);
            IDirect3DDevice9_SetTextureStageState(DEV, 0, D3DTSS_ALPHAOP, D3DTOP_SELECTARG1);
            IDirect3DDevice9_SetTextureStageState(DEV, 0, D3DTSS_ALPHAARG1, D3DTA_DIFFUSE);
        }
        else
        {
            IDirect3DDevice9_SetTextureStageState(DEV, i, D3DTSS_COLOROP, D3DTOP_DISABLE);
            IDirect3DDevice9_SetTextureStageState(DEV, i, D3DTSS_ALPHAOP, D3DTOP_DISABLE);
        }
        return;
    }

    IDirect3DDevice9_SetTexture(DEV, i, (IDirect3DBaseTexture9 *)t->tex);
    switch (t->min_filter)
    {
    case GL_LINEAR: min = D3DTEXF_LINEAR; break;
    case GL_NEAREST_MIPMAP_NEAREST: mip = D3DTEXF_POINT; break;
    case GL_LINEAR_MIPMAP_NEAREST: min = D3DTEXF_LINEAR; mip = D3DTEXF_POINT; break;
    case GL_NEAREST_MIPMAP_LINEAR: mip = D3DTEXF_LINEAR; break;
    case GL_LINEAR_MIPMAP_LINEAR: min = D3DTEXF_LINEAR; mip = D3DTEXF_LINEAR; break;
    }
    IDirect3DDevice9_SetSamplerState(DEV, i, D3DSAMP_MINFILTER, min);
    IDirect3DDevice9_SetSamplerState(DEV, i, D3DSAMP_MIPFILTER, mip);
    IDirect3DDevice9_SetSamplerState(DEV, i, D3DSAMP_MAGFILTER, t->mag_filter == GL_NEAREST ? D3DTEXF_POINT : D3DTEXF_LINEAR);
    IDirect3DDevice9_SetSamplerState(DEV, i, D3DSAMP_ADDRESSU, address_mode(t->wrap_s));
    IDirect3DDevice9_SetSamplerState(DEV, i, D3DSAMP_ADDRESSV, address_mode(t->wrap_t));
    IDirect3DDevice9_SetTextureStageState(DEV, i, D3DTSS_TEXCOORDINDEX, i);

    /* GL_ALPHA textures are stored white, so that modulating leaves the
     * colour as it was; textures without alpha are stored opaque. */
    alpha_only = t->base == BASE_ALPHA;
    has_alpha = t->base != BASE_RGB && t->base != BASE_LUMINANCE;
    switch (u->env_mode)
    {
    case GL_REPLACE:
        IDirect3DDevice9_SetTextureStageState(DEV, i, D3DTSS_COLOROP, D3DTOP_SELECTARG1);
        IDirect3DDevice9_SetTextureStageState(DEV, i, D3DTSS_COLORARG1, alpha_only ? current : D3DTA_TEXTURE);
        IDirect3DDevice9_SetTextureStageState(DEV, i, D3DTSS_ALPHAOP, D3DTOP_SELECTARG1);
        IDirect3DDevice9_SetTextureStageState(DEV, i, D3DTSS_ALPHAARG1, has_alpha ? D3DTA_TEXTURE : current);
        break;
    case GL_DECAL:
        IDirect3DDevice9_SetTextureStageState(DEV, i, D3DTSS_COLOROP, D3DTOP_BLENDTEXTUREALPHA);
        IDirect3DDevice9_SetTextureStageState(DEV, i, D3DTSS_COLORARG1, D3DTA_TEXTURE);
        IDirect3DDevice9_SetTextureStageState(DEV, i, D3DTSS_COLORARG2, current);
        IDirect3DDevice9_SetTextureStageState(DEV, i, D3DTSS_ALPHAOP, D3DTOP_SELECTARG1);
        IDirect3DDevice9_SetTextureStageState(DEV, i, D3DTSS_ALPHAARG1, current);
        break;
    default: /* GL_MODULATE; GL_BLEND approximated by it */
        IDirect3DDevice9_SetTextureStageState(DEV, i, D3DTSS_COLOROP, D3DTOP_MODULATE);
        IDirect3DDevice9_SetTextureStageState(DEV, i, D3DTSS_COLORARG1, D3DTA_TEXTURE);
        IDirect3DDevice9_SetTextureStageState(DEV, i, D3DTSS_COLORARG2, current);
        IDirect3DDevice9_SetTextureStageState(DEV, i, D3DTSS_ALPHAOP, D3DTOP_MODULATE);
        IDirect3DDevice9_SetTextureStageState(DEV, i, D3DTSS_ALPHAARG1, D3DTA_TEXTURE);
        IDirect3DDevice9_SetTextureStageState(DEV, i, D3DTSS_ALPHAARG2, current);
        break;
    }
}

static void update_stages_using(GLuint name)
{
    int i;
    for (i = 0; i < UNITS; i++)
        if (ctx->units[i].bound == name && ctx->units[i].enabled) update_stage(i);
}

static enum base_format base_format_of(GLint internal)
{
    switch (internal)
    {
    case 1: case GL_LUMINANCE: case GL_LUMINANCE4: case GL_LUMINANCE8: case GL_LUMINANCE12: case GL_LUMINANCE16:
        return BASE_LUMINANCE;
    case 2: case GL_LUMINANCE_ALPHA: case GL_LUMINANCE4_ALPHA4: case GL_LUMINANCE6_ALPHA2: case GL_LUMINANCE8_ALPHA8:
    case GL_LUMINANCE12_ALPHA4: case GL_LUMINANCE12_ALPHA12: case GL_LUMINANCE16_ALPHA16:
        return BASE_LUMINANCE_ALPHA;
    case GL_ALPHA: case GL_ALPHA4: case GL_ALPHA8: case GL_ALPHA12: case GL_ALPHA16:
        return BASE_ALPHA;
    case GL_INTENSITY: case GL_INTENSITY4: case GL_INTENSITY8: case GL_INTENSITY12: case GL_INTENSITY16:
        return BASE_INTENSITY;
    case 3: case GL_RGB: case GL_R3_G3_B2: case GL_RGB4: case GL_RGB5: case GL_RGB8: case GL_RGB10: case GL_RGB12: case GL_RGB16:
        return BASE_RGB;
    default:
        return BASE_RGBA;
    }
}

/* Converts w x h pixels from client memory (GL's unpack rules) to the
 * A8R8G8B8 rows of a locked surface. */
static BOOL convert_pixels(BYTE *dst, int pitch, int w, int h, GLenum format, GLenum type, const BYTE *src,
                           enum base_format base)
{
    int comps, row, x, y, len;
    if (type != GL_UNSIGNED_BYTE)
    {
        gl_unsupported("pixel type", type);
        return FALSE;
    }
    switch (format)
    {
    case GL_RGBA: case GL_BGRA_EXT: comps = 4; break;
    case GL_RGB: case GL_BGR_EXT: comps = 3; break;
    case GL_LUMINANCE_ALPHA: comps = 2; break;
    case GL_LUMINANCE: case GL_ALPHA: case GL_RED: case GL_GREEN: case GL_BLUE: case GL_COLOR_INDEX: comps = 1; break;
    default: gl_unsupported("pixel format", format); return FALSE;
    }
    len = ctx->unpack_row_length > 0 ? ctx->unpack_row_length : w;
    row = len * comps;
    if (ctx->unpack_alignment > 1) row = (row + ctx->unpack_alignment - 1) / ctx->unpack_alignment * ctx->unpack_alignment;
    src += ctx->unpack_skip_rows * row + ctx->unpack_skip_pixels * comps;

    for (y = 0; y < h; y++)
    {
        const BYTE *s = src + y * row;
        BYTE *d = dst + y * pitch;
        for (x = 0; x < w; x++, s += comps, d += 4)
        {
            BYTE r, g, b, a = 255;
            switch (format)
            {
            case GL_RGBA: r = s[0]; g = s[1]; b = s[2]; a = s[3]; break;
            case GL_BGRA_EXT: b = s[0]; g = s[1]; r = s[2]; a = s[3]; break;
            case GL_RGB: r = s[0]; g = s[1]; b = s[2]; break;
            case GL_BGR_EXT: b = s[0]; g = s[1]; r = s[2]; break;
            case GL_LUMINANCE_ALPHA: r = g = b = s[0]; a = s[1]; break;
            case GL_ALPHA: r = g = b = 0; a = s[0]; break;
            case GL_RED: r = s[0]; g = b = 0; break;
            case GL_GREEN: g = s[0]; r = b = 0; break;
            case GL_BLUE: b = s[0]; r = g = 0; break;
            default: r = g = b = s[0]; break;
            }
            switch (base)
            {
            case BASE_RGB: a = 255; break;
            case BASE_LUMINANCE: g = b = r; a = 255; break;
            case BASE_LUMINANCE_ALPHA: g = b = r; break;
            case BASE_INTENSITY: g = b = a = r; break;
            case BASE_ALPHA: r = g = b = 255; break;
            default: break;
            }
            d[0] = b;
            d[1] = g;
            d[2] = r;
            d[3] = a;
        }
    }
    return TRUE;
}

static void upload(struct texture *t, GLint level, int x, int y, int w, int h, GLenum format, GLenum type,
                   const void *pixels)
{
    D3DLOCKED_RECT lr;
    RECT rect = { x, y, x + w, y + h };
    if (!pixels || !t->tex || level >= t->levels || w <= 0 || h <= 0) return;
    if (FAILED(IDirect3DTexture9_LockRect(t->tex, level, &lr, &rect, 0))) return;
    convert_pixels(lr.pBits, lr.Pitch, w, h, format, type, pixels, t->base);
    IDirect3DTexture9_UnlockRect(t->tex, level);
}

/* ------------------------------------------------------------------ */
/* Render state                                                        */

static D3DBLEND blend_factor(GLenum f)
{
    switch (f)
    {
    case GL_ZERO: return D3DBLEND_ZERO;
    case GL_ONE: return D3DBLEND_ONE;
    case GL_SRC_COLOR: return D3DBLEND_SRCCOLOR;
    case GL_ONE_MINUS_SRC_COLOR: return D3DBLEND_INVSRCCOLOR;
    case GL_SRC_ALPHA: return D3DBLEND_SRCALPHA;
    case GL_ONE_MINUS_SRC_ALPHA: return D3DBLEND_INVSRCALPHA;
    case GL_DST_ALPHA: return D3DBLEND_DESTALPHA;
    case GL_ONE_MINUS_DST_ALPHA: return D3DBLEND_INVDESTALPHA;
    case GL_DST_COLOR: return D3DBLEND_DESTCOLOR;
    case GL_ONE_MINUS_DST_COLOR: return D3DBLEND_INVDESTCOLOR;
    case GL_SRC_ALPHA_SATURATE: return D3DBLEND_SRCALPHASAT;
    default: gl_unsupported("blend factor", f); return D3DBLEND_ONE;
    }
}

/* GL_NEVER..GL_ALWAYS and D3DCMP_NEVER..D3DCMP_ALWAYS are in the same order. */
static D3DCMPFUNC compare_func(GLenum f)
{
    return f >= GL_NEVER && f <= GL_ALWAYS ? (D3DCMPFUNC)(f - GL_NEVER + D3DCMP_NEVER) : D3DCMP_ALWAYS;
}

static void update_cull(void)
{
    GLenum culled;
    D3DCULL mode = D3DCULL_NONE;
    if (ctx->cull && ctx->cull_face != GL_FRONT_AND_BACK)
    {
        /* Both APIs name windings as seen on the screen. */
        culled = ctx->cull_face == GL_FRONT ? ctx->front_face : ctx->front_face == GL_CCW ? GL_CW : GL_CCW;
        mode = culled == GL_CW ? D3DCULL_CW : D3DCULL_CCW;
    }
    IDirect3DDevice9_SetRenderState(DEV, D3DRS_CULLMODE, mode);
}

static void clamp_rect(int *x, int *y, int *w, int *h)
{
    if (*x < 0) { *w += *x; *x = 0; }
    if (*y < 0) { *h += *y; *y = 0; }
    if (*x + *w > ctx->width) *w = ctx->width - *x;
    if (*y + *h > ctx->height) *h = ctx->height - *y;
    if (*w < 0) *w = 0;
    if (*h < 0) *h = 0;
}

static void update_viewport(void)
{
    D3DVIEWPORT9 vp;
    /* GL counts rows from the bottom. */
    int x = ctx->viewport[0], y = ctx->height - (ctx->viewport[1] + ctx->viewport[3]);
    int w = ctx->viewport[2], h = ctx->viewport[3];
    clamp_rect(&x, &y, &w, &h);
    vp.X = x;
    vp.Y = y;
    vp.Width = w ? w : 1;
    vp.Height = h ? h : 1;
    vp.MinZ = ctx->depth_near;
    vp.MaxZ = ctx->depth_far;
    IDirect3DDevice9_SetViewport(DEV, &vp);
    ctx->transforms_dirty = TRUE;
}

static void update_scissor(void)
{
    RECT r;
    int x = ctx->scissor[0], y = ctx->height - (ctx->scissor[1] + ctx->scissor[3]);
    int w = ctx->scissor[2], h = ctx->scissor[3];
    clamp_rect(&x, &y, &w, &h);
    r.left = x;
    r.top = y;
    r.right = x + w;
    r.bottom = y + h;
    IDirect3DDevice9_SetScissorRect(DEV, &r);
}

static void set_capability(GLenum cap, BOOL on)
{
    CTX();
    FLUSH();
    switch (cap)
    {
    case GL_TEXTURE_2D:
        ctx->units[ctx->active_unit].enabled = on;
        update_stage(ctx->active_unit);
        break;
    case GL_BLEND:
        ctx->blend = on;
        IDirect3DDevice9_SetRenderState(DEV, D3DRS_ALPHABLENDENABLE, on);
        break;
    case GL_ALPHA_TEST:
        ctx->alpha_test = on;
        IDirect3DDevice9_SetRenderState(DEV, D3DRS_ALPHATESTENABLE, on);
        break;
    case GL_DEPTH_TEST:
        ctx->depth_test = on;
        IDirect3DDevice9_SetRenderState(DEV, D3DRS_ZENABLE, on ? D3DZB_TRUE : D3DZB_FALSE);
        break;
    case GL_CULL_FACE:
        ctx->cull = on;
        update_cull();
        break;
    case GL_SCISSOR_TEST:
        ctx->scissor_test = on;
        IDirect3DDevice9_SetRenderState(DEV, D3DRS_SCISSORTESTENABLE, on);
        break;
    case GL_STENCIL_TEST:
        ctx->stencil_test = on;
        IDirect3DDevice9_SetRenderState(DEV, D3DRS_STENCILENABLE, on);
        break;
    case GL_DITHER:
        ctx->dither = on;
        IDirect3DDevice9_SetRenderState(DEV, D3DRS_DITHERENABLE, on);
        break;
    case GL_FOG:
        ctx->fog = on;
        if (on) gl_unsupported("glEnable", cap);
        break;
    case GL_LIGHTING: case GL_NORMALIZE: case GL_COLOR_MATERIAL: case GL_POINT_SMOOTH: case GL_LINE_SMOOTH:
    case GL_POLYGON_SMOOTH: case GL_POLYGON_OFFSET_FILL: case GL_TEXTURE_1D: case GL_TEXTURE_GEN_S:
    case GL_TEXTURE_GEN_T:
        if (on) gl_unsupported("glEnable", cap);
        break;
    default:
        gl_unsupported(on ? "glEnable" : "glDisable", cap);
        break;
    }
}

/* ------------------------------------------------------------------ */
/* Contexts and pixel formats                                          */

static void init_state(void)
{
    int i;
    IDirect3DDevice9 *d = DEV;
    IDirect3DDevice9_SetFVF(d, VERTEX_FVF);
    IDirect3DDevice9_SetRenderState(d, D3DRS_LIGHTING, FALSE);
    IDirect3DDevice9_SetRenderState(d, D3DRS_ZENABLE, D3DZB_FALSE);
    IDirect3DDevice9_SetRenderState(d, D3DRS_ZFUNC, D3DCMP_LESS);
    IDirect3DDevice9_SetRenderState(d, D3DRS_ZWRITEENABLE, TRUE);
    IDirect3DDevice9_SetRenderState(d, D3DRS_CULLMODE, D3DCULL_NONE);
    IDirect3DDevice9_SetRenderState(d, D3DRS_ALPHABLENDENABLE, FALSE);
    IDirect3DDevice9_SetRenderState(d, D3DRS_SRCBLEND, D3DBLEND_ONE);
    IDirect3DDevice9_SetRenderState(d, D3DRS_DESTBLEND, D3DBLEND_ZERO);
    IDirect3DDevice9_SetRenderState(d, D3DRS_ALPHATESTENABLE, FALSE);
    IDirect3DDevice9_SetRenderState(d, D3DRS_ALPHAFUNC, D3DCMP_ALWAYS);
    IDirect3DDevice9_SetRenderState(d, D3DRS_ALPHAREF, 0);
    IDirect3DDevice9_SetRenderState(d, D3DRS_SHADEMODE, D3DSHADE_GOURAUD);
    IDirect3DDevice9_SetRenderState(d, D3DRS_SCISSORTESTENABLE, FALSE);
    IDirect3DDevice9_SetRenderState(d, D3DRS_DITHERENABLE, TRUE);
    IDirect3DDevice9_SetRenderState(d, D3DRS_FOGENABLE, FALSE);
    IDirect3DDevice9_SetRenderState(d, D3DRS_SPECULARENABLE, FALSE);
    IDirect3DDevice9_SetTransform(d, D3DTS_WORLD, (const D3DMATRIX *)identity);

    ctx->matrix_mode = GL_MODELVIEW;
    memcpy(ctx->modelview[0], identity, sizeof(identity));
    memcpy(ctx->projection[0], identity, sizeof(identity));
    memcpy(ctx->texture[0], identity, sizeof(identity));
    ctx->transforms_dirty = TRUE;
    ctx->viewport[0] = ctx->viewport[1] = 0;
    ctx->viewport[2] = ctx->width;
    ctx->viewport[3] = ctx->height;
    ctx->depth_near = 0;
    ctx->depth_far = 1;
    memcpy(ctx->scissor, ctx->viewport, sizeof(ctx->scissor));
    ctx->cull_face = GL_BACK;
    ctx->front_face = GL_CCW;
    ctx->dither = TRUE;
    for (i = 0; i < UNITS; i++)
    {
        ctx->units[i].env_mode = GL_MODULATE;
        update_stage(i);
    }
    color(1, 1, 1, 1);
    ctx->clear_depth = 1;
    ctx->unpack_alignment = ctx->pack_alignment = 4;
    ctx->next_name = 1;
    update_viewport();
    update_scissor();
}

static BOOL create_device(struct context *c)
{
    D3DPRESENT_PARAMETERS pp;
    RECT r;
    HRESULT hr;

    if (!GetClientRect(c->hwnd, &r)) memset(&r, 0, sizeof(r));
    c->width = r.right - r.left > 0 ? r.right - r.left : 1;
    c->height = r.bottom - r.top > 0 ? r.bottom - r.top : 1;

    c->d3d = Direct3DCreate9(D3D_SDK_VERSION);
    if (!c->d3d)
    {
        gl_log("err:opengl32: Direct3DCreate9 failed\n");
        return FALSE;
    }
    memset(&pp, 0, sizeof(pp));
    pp.BackBufferWidth = c->width;
    pp.BackBufferHeight = c->height;
    pp.BackBufferFormat = D3DFMT_X8R8G8B8;
    pp.BackBufferCount = 1;
    pp.SwapEffect = D3DSWAPEFFECT_DISCARD;
    pp.hDeviceWindow = c->hwnd;
    pp.Windowed = TRUE;
    pp.EnableAutoDepthStencil = TRUE;
    pp.AutoDepthStencilFormat = D3DFMT_D24S8;
    pp.PresentationInterval = D3DPRESENT_INTERVAL_IMMEDIATE;
    hr = IDirect3D9_CreateDevice(c->d3d, D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, c->hwnd,
                                 D3DCREATE_SOFTWARE_VERTEXPROCESSING | D3DCREATE_FPU_PRESERVE, &pp, &c->device);
    if (FAILED(hr))
    {
        gl_log("err:opengl32: CreateDevice failed (0x%08lx)\n", hr);
        IDirect3D9_Release(c->d3d);
        c->d3d = NULL;
        return FALSE;
    }
    gl_log("trace:opengl32: context on window %p, %dx%d\n", c->hwnd, c->width, c->height);
    return TRUE;
}

HGLRC WINAPI wglCreateContext(HDC hdc)
{
    struct context *c = calloc(1, sizeof(*c)), *prev = ctx;
    if (!c) return NULL;
    c->hdc = hdc;
    c->hwnd = WindowFromDC(hdc);
    if (!create_device(c))
    {
        free(c);
        return NULL;
    }
    ctx = c;
    init_state();
    ctx = prev;
    return (HGLRC)c;
}

HGLRC WINAPI wglCreateLayerContext(HDC hdc, int layer)
{
    return layer ? NULL : wglCreateContext(hdc);
}

BOOL WINAPI wglMakeCurrent(HDC hdc, HGLRC rc)
{
    if (ctx && ctx != (struct context *)rc) flush();
    ctx = (struct context *)rc;
    if (ctx)
    {
        ctx->hdc = hdc;
        if (!ctx->in_scene) ctx->in_scene = SUCCEEDED(IDirect3DDevice9_BeginScene(DEV));
    }
    return TRUE;
}

BOOL WINAPI wglDeleteContext(HGLRC rc)
{
    struct context *c = (struct context *)rc, *prev = ctx;
    GLuint i;
    if (!c) return FALSE;
    ctx = c;
    flush();
    if (c->in_scene) IDirect3DDevice9_EndScene(c->device);
    for (i = 0; i < c->ntextures; i++)
    {
        if (!c->textures[i]) continue;
        if (c->textures[i]->tex) IDirect3DTexture9_Release(c->textures[i]->tex);
        free(c->textures[i]);
    }
    free(c->textures);
    IDirect3DDevice9_Release(c->device);
    IDirect3D9_Release(c->d3d);
    free(c);
    ctx = prev == c ? NULL : prev;
    return TRUE;
}

HGLRC WINAPI wglGetCurrentContext(void)
{
    return (HGLRC)ctx;
}

HDC WINAPI wglGetCurrentDC(void)
{
    return ctx ? ctx->hdc : NULL;
}

BOOL WINAPI wglShareLists(HGLRC a, HGLRC b)
{
    return TRUE;
}

BOOL WINAPI wglCopyContext(HGLRC src, HGLRC dst, UINT mask)
{
    return FALSE;
}

static BOOL swap(void)
{
    CTX(FALSE);
    flush();
    if (ctx->in_scene) IDirect3DDevice9_EndScene(DEV);
    IDirect3DDevice9_Present(DEV, NULL, NULL, NULL, NULL);
    ctx->in_scene = SUCCEEDED(IDirect3DDevice9_BeginScene(DEV));
    return TRUE;
}

BOOL WINAPI wglSwapBuffers(HDC hdc)
{
    return swap();
}

BOOL WINAPI wglSwapLayerBuffers(HDC hdc, UINT planes)
{
    return swap();
}

DWORD WINAPI wglSwapMultipleBuffers(UINT n, const WGLSWAP *swaps)
{
    return swap() ? n : 0;
}

/* One pixel format: double-buffered 32-bit RGBA with 24-bit depth and 8-bit
 * stencil, what the Direct3D device has. */
static void describe(PIXELFORMATDESCRIPTOR *pfd)
{
    memset(pfd, 0, sizeof(*pfd));
    pfd->nSize = sizeof(*pfd);
    pfd->nVersion = 1;
    pfd->dwFlags = PFD_DRAW_TO_WINDOW | PFD_SUPPORT_OPENGL | PFD_DOUBLEBUFFER;
    pfd->iPixelType = PFD_TYPE_RGBA;
    pfd->cColorBits = 32;
    pfd->cRedBits = 8;
    pfd->cRedShift = 16;
    pfd->cGreenBits = 8;
    pfd->cGreenShift = 8;
    pfd->cBlueBits = 8;
    pfd->cBlueShift = 0;
    pfd->cAlphaBits = 8;
    pfd->cAlphaShift = 24;
    pfd->cDepthBits = 24;
    pfd->cStencilBits = 8;
    pfd->iLayerType = PFD_MAIN_PLANE;
}

int WINAPI wglChoosePixelFormat(HDC hdc, const PIXELFORMATDESCRIPTOR *pfd)
{
    return 1;
}

int WINAPI wglDescribePixelFormat(HDC hdc, int format, UINT size, PIXELFORMATDESCRIPTOR *pfd)
{
    if (pfd && size >= sizeof(*pfd) && format == 1) describe(pfd);
    return 1;
}

int WINAPI wglGetPixelFormat(HDC hdc)
{
    return pixel_format;
}

BOOL WINAPI wglSetPixelFormat(HDC hdc, int format, const PIXELFORMATDESCRIPTOR *pfd)
{
    if (format != 1) return FALSE;
    pixel_format = format;
    return TRUE;
}

BOOL WINAPI wglUseFontBitmapsA(HDC hdc, DWORD first, DWORD count, DWORD base)
{
    gl_unimplemented("wglUseFontBitmapsA");
    return FALSE;
}

BOOL WINAPI wglUseFontBitmapsW(HDC hdc, DWORD first, DWORD count, DWORD base)
{
    gl_unimplemented("wglUseFontBitmapsW");
    return FALSE;
}

BOOL WINAPI wglUseFontOutlinesA(HDC hdc, DWORD first, DWORD count, DWORD base, FLOAT dev, FLOAT ext, int format,
                                LPGLYPHMETRICSFLOAT gmf)
{
    gl_unimplemented("wglUseFontOutlinesA");
    return FALSE;
}

BOOL WINAPI wglUseFontOutlinesW(HDC hdc, DWORD first, DWORD count, DWORD base, FLOAT dev, FLOAT ext, int format,
                                LPGLYPHMETRICSFLOAT gmf)
{
    gl_unimplemented("wglUseFontOutlinesW");
    return FALSE;
}

BOOL WINAPI wglDescribeLayerPlane(HDC hdc, int format, int plane, UINT size, LPLAYERPLANEDESCRIPTOR lpd)
{
    return FALSE;
}

int WINAPI wglSetLayerPaletteEntries(HDC hdc, int plane, int start, int count, const COLORREF *entries)
{
    return 0;
}

int WINAPI wglGetLayerPaletteEntries(HDC hdc, int plane, int start, int count, COLORREF *entries)
{
    return 0;
}

BOOL WINAPI wglRealizeLayerPalette(HDC hdc, int plane, BOOL realize)
{
    return FALSE;
}

/* ------------------------------------------------------------------ */
/* Extensions                                                          */

static void APIENTRY glActiveTextureARB(GLenum unit)
{
    int i = unit - GL_TEXTURE0_ARB;
    CTX();
    if (i >= 0 && i < UNITS) ctx->active_unit = i;
}

static void APIENTRY glClientActiveTextureARB(GLenum unit)
{
    int i = unit - GL_TEXTURE0_ARB;
    CTX();
    if (i >= 0 && i < UNITS) ctx->client_unit = i;
}

static void APIENTRY glMultiTexCoord2fARB(GLenum unit, GLfloat s, GLfloat t)
{
    int i = unit - GL_TEXTURE0_ARB;
    if (i >= 0 && i < UNITS) texcoord(i, s, t);
}

static void APIENTRY glMultiTexCoord2fvARB(GLenum unit, const GLfloat *v)
{
    glMultiTexCoord2fARB(unit, v[0], v[1]);
}

static void APIENTRY glSelectTextureSGIS(GLenum unit)
{
    int i = unit - GL_TEXTURE0_SGIS;
    CTX();
    if (i >= 0 && i < UNITS) ctx->active_unit = ctx->client_unit = i;
}

static void APIENTRY glMTexCoord2fSGIS(GLenum unit, GLfloat s, GLfloat t)
{
    int i = unit - GL_TEXTURE0_SGIS;
    if (i >= 0 && i < UNITS) texcoord(i, s, t);
}

static void APIENTRY glLockArraysEXT(GLint first, GLsizei count)
{
}

static void APIENTRY glUnlockArraysEXT(void)
{
}

static BOOL APIENTRY wglSwapIntervalEXT(int interval)
{
    return TRUE;
}

static int APIENTRY wglGetSwapIntervalEXT(void)
{
    return 0;
}

static const struct { const char *name; PROC proc; } extension_procs[] = {
    { "glActiveTextureARB", (PROC)glActiveTextureARB },
    { "glClientActiveTextureARB", (PROC)glClientActiveTextureARB },
    { "glMultiTexCoord2fARB", (PROC)glMultiTexCoord2fARB },
    { "glMultiTexCoord2fvARB", (PROC)glMultiTexCoord2fvARB },
    { "glSelectTextureSGIS", (PROC)glSelectTextureSGIS },
    { "glMTexCoord2fSGIS", (PROC)glMTexCoord2fSGIS },
    { "glLockArraysEXT", (PROC)glLockArraysEXT },
    { "glUnlockArraysEXT", (PROC)glUnlockArraysEXT },
    { "wglSwapIntervalEXT", (PROC)wglSwapIntervalEXT },
    { "wglGetSwapIntervalEXT", (PROC)wglGetSwapIntervalEXT },
};

/* Multitexture is not advertised yet: programs then draw lightmaps in a
 * second pass, the path tested so far. */
static const char extensions[] = "GL_EXT_compiled_vertex_array";

PROC WINAPI wglGetProcAddress(LPCSTR name)
{
    unsigned i;
    for (i = 0; i < sizeof(extension_procs) / sizeof(extension_procs[0]); i++)
        if (!strcmp(name, extension_procs[i].name)) return extension_procs[i].proc;
    return NULL;
}

/* ------------------------------------------------------------------ */
/* GL: immediate mode                                                  */

void APIENTRY glBegin(GLenum mode)
{
    D3DPRIMITIVETYPE type;
    CTX();
    if (ctx->in_begin) { set_error(GL_INVALID_OPERATION); return; }
    if (mode > GL_POLYGON) { set_error(GL_INVALID_ENUM); return; }
    type = batch_type_of(mode);
    if (ctx->nbatch && type != ctx->batch_type) flush();
    ctx->batch_type = type;
    ctx->mode = mode;
    ctx->nraw = 0;
    ctx->in_begin = TRUE;
}

void APIENTRY glEnd(void)
{
    CTX();
    if (!ctx->in_begin) { set_error(GL_INVALID_OPERATION); return; }
    ctx->in_begin = FALSE;
    emit_raw();
    ctx->nraw = 0;
}

void APIENTRY glVertex2f(GLfloat x, GLfloat y) { vertex(x, y, 0); }
void APIENTRY glVertex2fv(const GLfloat *v) { vertex(v[0], v[1], 0); }
void APIENTRY glVertex2i(GLint x, GLint y) { vertex((float)x, (float)y, 0); }
void APIENTRY glVertex2d(GLdouble x, GLdouble y) { vertex((float)x, (float)y, 0); }
void APIENTRY glVertex2s(GLshort x, GLshort y) { vertex(x, y, 0); }
void APIENTRY glVertex3f(GLfloat x, GLfloat y, GLfloat z) { vertex(x, y, z); }
void APIENTRY glVertex3fv(const GLfloat *v) { vertex(v[0], v[1], v[2]); }
void APIENTRY glVertex3d(GLdouble x, GLdouble y, GLdouble z) { vertex((float)x, (float)y, (float)z); }
void APIENTRY glVertex3dv(const GLdouble *v) { vertex((float)v[0], (float)v[1], (float)v[2]); }
void APIENTRY glVertex3i(GLint x, GLint y, GLint z) { vertex((float)x, (float)y, (float)z); }

void APIENTRY glColor3f(GLfloat r, GLfloat g, GLfloat b) { color(r, g, b, 1); }
void APIENTRY glColor3fv(const GLfloat *v) { color(v[0], v[1], v[2], 1); }
void APIENTRY glColor3ub(GLubyte r, GLubyte g, GLubyte b) { color(r / 255.0f, g / 255.0f, b / 255.0f, 1); }
void APIENTRY glColor3ubv(const GLubyte *v) { color(v[0] / 255.0f, v[1] / 255.0f, v[2] / 255.0f, 1); }
void APIENTRY glColor3d(GLdouble r, GLdouble g, GLdouble b) { color((float)r, (float)g, (float)b, 1); }
void APIENTRY glColor4f(GLfloat r, GLfloat g, GLfloat b, GLfloat a) { color(r, g, b, a); }
void APIENTRY glColor4fv(const GLfloat *v) { color(v[0], v[1], v[2], v[3]); }
void APIENTRY glColor4ub(GLubyte r, GLubyte g, GLubyte b, GLubyte a)
{
    color(r / 255.0f, g / 255.0f, b / 255.0f, a / 255.0f);
}
void APIENTRY glColor4ubv(const GLubyte *v) { color(v[0] / 255.0f, v[1] / 255.0f, v[2] / 255.0f, v[3] / 255.0f); }
void APIENTRY glColor4d(GLdouble r, GLdouble g, GLdouble b, GLdouble a) { color((float)r, (float)g, (float)b, (float)a); }

void APIENTRY glTexCoord2f(GLfloat s, GLfloat t) { texcoord(0, s, t); }
void APIENTRY glTexCoord2fv(const GLfloat *v) { texcoord(0, v[0], v[1]); }
void APIENTRY glTexCoord2d(GLdouble s, GLdouble t) { texcoord(0, (float)s, (float)t); }
void APIENTRY glTexCoord2i(GLint s, GLint t) { texcoord(0, (float)s, (float)t); }

/* No lighting: normals have nothing to do. */
void APIENTRY glNormal3f(GLfloat x, GLfloat y, GLfloat z) {}
void APIENTRY glNormal3fv(const GLfloat *v) {}

/* ------------------------------------------------------------------ */
/* GL: vertex arrays (drawn through the immediate-mode path)            */

static void set_array(struct array *a, GLint size, GLenum type, GLsizei stride, const GLvoid *pointer)
{
    a->size = size;
    a->type = type;
    a->stride = stride;
    a->pointer = pointer;
}

void APIENTRY glVertexPointer(GLint size, GLenum type, GLsizei stride, const GLvoid *pointer)
{
    CTX();
    set_array(&ctx->vertex_array, size, type, stride, pointer);
}

void APIENTRY glColorPointer(GLint size, GLenum type, GLsizei stride, const GLvoid *pointer)
{
    CTX();
    set_array(&ctx->color_array, size, type, stride, pointer);
}

void APIENTRY glTexCoordPointer(GLint size, GLenum type, GLsizei stride, const GLvoid *pointer)
{
    CTX();
    set_array(&ctx->texcoord_array[ctx->client_unit], size, type, stride, pointer);
}

static struct array *client_array(GLenum cap)
{
    switch (cap)
    {
    case GL_VERTEX_ARRAY: return &ctx->vertex_array;
    case GL_COLOR_ARRAY: return &ctx->color_array;
    case GL_TEXTURE_COORD_ARRAY: return &ctx->texcoord_array[ctx->client_unit];
    default: return NULL;
    }
}

void APIENTRY glEnableClientState(GLenum cap)
{
    struct array *a;
    CTX();
    if ((a = client_array(cap))) a->enabled = TRUE;
}

void APIENTRY glDisableClientState(GLenum cap)
{
    struct array *a;
    CTX();
    if ((a = client_array(cap))) a->enabled = FALSE;
}

static void fetch(const struct array *a, GLint i, float *out)
{
    static const int sizes[] = { 1, 1, 2, 2, 4, 4, 4 }; /* GL_BYTE.. GL_FLOAT */
    int k, elem = a->type >= GL_BYTE && a->type <= GL_FLOAT ? sizes[a->type - GL_BYTE] : a->type == GL_DOUBLE ? 8 : 4;
    const BYTE *p = (const BYTE *)a->pointer + i * (a->stride ? a->stride : a->size * elem);
    for (k = 0; k < a->size && k < 4; k++)
    {
        switch (a->type)
        {
        case GL_UNSIGNED_BYTE: out[k] = p[k] / 255.0f; break;
        case GL_SHORT: out[k] = ((const GLshort *)p)[k]; break;
        case GL_INT: out[k] = (float)((const GLint *)p)[k]; break;
        case GL_DOUBLE: out[k] = (float)((const GLdouble *)p)[k]; break;
        default: out[k] = ((const GLfloat *)p)[k]; break;
        }
    }
}

void APIENTRY glArrayElement(GLint i)
{
    float v[4] = { 0, 0, 0, 1 };
    int u;
    CTX();
    for (u = 0; u < UNITS; u++)
        if (ctx->texcoord_array[u].enabled)
        {
            fetch(&ctx->texcoord_array[u], i, v);
            texcoord(u, v[0], v[1]);
        }
    if (ctx->color_array.enabled)
    {
        v[3] = 1;
        fetch(&ctx->color_array, i, v);
        color(v[0], v[1], v[2], v[3]);
    }
    if (ctx->vertex_array.enabled)
    {
        v[2] = 0;
        fetch(&ctx->vertex_array, i, v);
        vertex(v[0], v[1], v[2]);
    }
}

void APIENTRY glDrawArrays(GLenum mode, GLint first, GLsizei count)
{
    GLint i;
    glBegin(mode);
    for (i = 0; i < count; i++) glArrayElement(first + i);
    glEnd();
}

void APIENTRY glDrawElements(GLenum mode, GLsizei count, GLenum type, const GLvoid *indices)
{
    GLsizei i;
    glBegin(mode);
    for (i = 0; i < count; i++)
    {
        GLint index = type == GL_UNSIGNED_BYTE ? ((const GLubyte *)indices)[i]
                      : type == GL_UNSIGNED_SHORT ? ((const GLushort *)indices)[i]
                                                  : (GLint)((const GLuint *)indices)[i];
        glArrayElement(index);
    }
    glEnd();
}

/* ------------------------------------------------------------------ */
/* GL: matrices                                                        */

static void matrix_changed(void)
{
    if (ctx->matrix_mode != GL_TEXTURE) ctx->transforms_dirty = TRUE;
}

void APIENTRY glMatrixMode(GLenum mode)
{
    CTX();
    ctx->matrix_mode = mode;
}

void APIENTRY glLoadIdentity(void)
{
    CTX();
    FLUSH();
    memcpy(current_matrix(), identity, sizeof(identity));
    matrix_changed();
}

void APIENTRY glLoadMatrixf(const GLfloat *m)
{
    CTX();
    FLUSH();
    memcpy(current_matrix(), m, 16 * sizeof(float));
    matrix_changed();
}

void APIENTRY glLoadMatrixd(const GLdouble *m)
{
    float f[16];
    int i;
    for (i = 0; i < 16; i++) f[i] = (float)m[i];
    glLoadMatrixf(f);
}

void APIENTRY glMultMatrixf(const GLfloat *m)
{
    float *c;
    CTX();
    FLUSH();
    c = current_matrix();
    mat_mul(c, c, m);
    matrix_changed();
}

void APIENTRY glMultMatrixd(const GLdouble *m)
{
    float f[16];
    int i;
    for (i = 0; i < 16; i++) f[i] = (float)m[i];
    glMultMatrixf(f);
}

void APIENTRY glPushMatrix(void)
{
    int *top, max;
    float (*stack)[16];
    CTX();
    switch (ctx->matrix_mode)
    {
    case GL_PROJECTION: top = &ctx->projection_top; stack = ctx->projection; max = 8; break;
    case GL_TEXTURE: top = &ctx->texture_top; stack = ctx->texture; max = 8; break;
    default: top = &ctx->modelview_top; stack = ctx->modelview; max = 32; break;
    }
    if (*top + 1 >= max) { set_error(GL_STACK_OVERFLOW); return; }
    memcpy(stack[*top + 1], stack[*top], sizeof(stack[0]));
    (*top)++;
}

void APIENTRY glPopMatrix(void)
{
    int *top;
    CTX();
    switch (ctx->matrix_mode)
    {
    case GL_PROJECTION: top = &ctx->projection_top; break;
    case GL_TEXTURE: top = &ctx->texture_top; break;
    default: top = &ctx->modelview_top; break;
    }
    if (!*top) { set_error(GL_STACK_UNDERFLOW); return; }
    FLUSH();
    (*top)--;
    matrix_changed();
}

void APIENTRY glTranslatef(GLfloat x, GLfloat y, GLfloat z)
{
    float m[16];
    memcpy(m, identity, sizeof(m));
    m[12] = x;
    m[13] = y;
    m[14] = z;
    glMultMatrixf(m);
}

void APIENTRY glTranslated(GLdouble x, GLdouble y, GLdouble z) { glTranslatef((float)x, (float)y, (float)z); }

void APIENTRY glScalef(GLfloat x, GLfloat y, GLfloat z)
{
    float m[16];
    memcpy(m, identity, sizeof(m));
    m[0] = x;
    m[5] = y;
    m[10] = z;
    glMultMatrixf(m);
}

void APIENTRY glScaled(GLdouble x, GLdouble y, GLdouble z) { glScalef((float)x, (float)y, (float)z); }

void APIENTRY glRotatef(GLfloat angle, GLfloat x, GLfloat y, GLfloat z)
{
    float m[16], len = (float)sqrt(x * x + y * y + z * z), c, s, t;
    double a = angle * 3.14159265358979323846 / 180.0;
    if (len == 0) return;
    x /= len;
    y /= len;
    z /= len;
    c = (float)cos(a);
    s = (float)sin(a);
    t = 1 - c;
    m[0] = x * x * t + c;
    m[1] = y * x * t + z * s;
    m[2] = x * z * t - y * s;
    m[3] = 0;
    m[4] = x * y * t - z * s;
    m[5] = y * y * t + c;
    m[6] = y * z * t + x * s;
    m[7] = 0;
    m[8] = x * z * t + y * s;
    m[9] = y * z * t - x * s;
    m[10] = z * z * t + c;
    m[11] = 0;
    m[12] = m[13] = m[14] = 0;
    m[15] = 1;
    glMultMatrixf(m);
}

void APIENTRY glRotated(GLdouble angle, GLdouble x, GLdouble y, GLdouble z)
{
    glRotatef((float)angle, (float)x, (float)y, (float)z);
}

void APIENTRY glOrtho(GLdouble l, GLdouble r, GLdouble b, GLdouble t, GLdouble n, GLdouble f)
{
    float m[16];
    memset(m, 0, sizeof(m));
    m[0] = (float)(2 / (r - l));
    m[5] = (float)(2 / (t - b));
    m[10] = (float)(-2 / (f - n));
    m[12] = (float)(-(r + l) / (r - l));
    m[13] = (float)(-(t + b) / (t - b));
    m[14] = (float)(-(f + n) / (f - n));
    m[15] = 1;
    glMultMatrixf(m);
}

void APIENTRY glFrustum(GLdouble l, GLdouble r, GLdouble b, GLdouble t, GLdouble n, GLdouble f)
{
    float m[16];
    memset(m, 0, sizeof(m));
    m[0] = (float)(2 * n / (r - l));
    m[5] = (float)(2 * n / (t - b));
    m[8] = (float)((r + l) / (r - l));
    m[9] = (float)((t + b) / (t - b));
    m[10] = (float)(-(f + n) / (f - n));
    m[11] = -1;
    m[14] = (float)(-2 * f * n / (f - n));
    glMultMatrixf(m);
}

/* ------------------------------------------------------------------ */
/* GL: state                                                           */

void APIENTRY glEnable(GLenum cap) { set_capability(cap, TRUE); }
void APIENTRY glDisable(GLenum cap) { set_capability(cap, FALSE); }

GLboolean APIENTRY glIsEnabled(GLenum cap)
{
    CTX(GL_FALSE);
    switch (cap)
    {
    case GL_TEXTURE_2D: return ctx->units[ctx->active_unit].enabled;
    case GL_BLEND: return ctx->blend;
    case GL_ALPHA_TEST: return ctx->alpha_test;
    case GL_DEPTH_TEST: return ctx->depth_test;
    case GL_CULL_FACE: return ctx->cull;
    case GL_SCISSOR_TEST: return ctx->scissor_test;
    case GL_STENCIL_TEST: return ctx->stencil_test;
    case GL_FOG: return ctx->fog;
    case GL_DITHER: return ctx->dither;
    case GL_VERTEX_ARRAY: return ctx->vertex_array.enabled;
    case GL_COLOR_ARRAY: return ctx->color_array.enabled;
    case GL_TEXTURE_COORD_ARRAY: return ctx->texcoord_array[ctx->client_unit].enabled;
    default: return GL_FALSE;
    }
}

void APIENTRY glBlendFunc(GLenum src, GLenum dst)
{
    CTX();
    FLUSH();
    IDirect3DDevice9_SetRenderState(DEV, D3DRS_SRCBLEND, blend_factor(src));
    IDirect3DDevice9_SetRenderState(DEV, D3DRS_DESTBLEND, blend_factor(dst));
}

void APIENTRY glAlphaFunc(GLenum func, GLclampf ref)
{
    CTX();
    FLUSH();
    IDirect3DDevice9_SetRenderState(DEV, D3DRS_ALPHAFUNC, compare_func(func));
    IDirect3DDevice9_SetRenderState(DEV, D3DRS_ALPHAREF, to_byte(ref));
}

void APIENTRY glDepthFunc(GLenum func)
{
    CTX();
    FLUSH();
    IDirect3DDevice9_SetRenderState(DEV, D3DRS_ZFUNC, compare_func(func));
}

void APIENTRY glDepthMask(GLboolean flag)
{
    CTX();
    FLUSH();
    IDirect3DDevice9_SetRenderState(DEV, D3DRS_ZWRITEENABLE, flag ? TRUE : FALSE);
}

void APIENTRY glDepthRange(GLclampd n, GLclampd f)
{
    CTX();
    FLUSH();
    ctx->depth_near = (float)(n < 0 ? 0 : n > 1 ? 1 : n);
    ctx->depth_far = (float)(f < 0 ? 0 : f > 1 ? 1 : f);
    update_viewport();
}

void APIENTRY glCullFace(GLenum mode)
{
    CTX();
    FLUSH();
    ctx->cull_face = mode;
    update_cull();
}

void APIENTRY glFrontFace(GLenum mode)
{
    CTX();
    FLUSH();
    ctx->front_face = mode;
    update_cull();
}

void APIENTRY glColorMask(GLboolean r, GLboolean g, GLboolean b, GLboolean a)
{
    CTX();
    FLUSH();
    IDirect3DDevice9_SetRenderState(DEV, D3DRS_COLORWRITEENABLE,
                                    (r ? D3DCOLORWRITEENABLE_RED : 0) | (g ? D3DCOLORWRITEENABLE_GREEN : 0)
                                        | (b ? D3DCOLORWRITEENABLE_BLUE : 0) | (a ? D3DCOLORWRITEENABLE_ALPHA : 0));
}

void APIENTRY glShadeModel(GLenum mode)
{
    CTX();
    FLUSH();
    IDirect3DDevice9_SetRenderState(DEV, D3DRS_SHADEMODE, mode == GL_FLAT ? D3DSHADE_FLAT : D3DSHADE_GOURAUD);
}

void APIENTRY glPolygonMode(GLenum face, GLenum mode)
{
    CTX();
    FLUSH();
    IDirect3DDevice9_SetRenderState(DEV, D3DRS_FILLMODE,
                                    mode == GL_LINE ? D3DFILL_WIREFRAME : mode == GL_POINT ? D3DFILL_POINT : D3DFILL_SOLID);
}

void APIENTRY glPointSize(GLfloat size)
{
    DWORD bits;
    CTX();
    FLUSH();
    memcpy(&bits, &size, sizeof(bits));
    IDirect3DDevice9_SetRenderState(DEV, D3DRS_POINTSIZE, bits);
}

void APIENTRY glStencilFunc(GLenum func, GLint ref, GLuint mask)
{
    CTX();
    FLUSH();
    IDirect3DDevice9_SetRenderState(DEV, D3DRS_STENCILFUNC, compare_func(func));
    IDirect3DDevice9_SetRenderState(DEV, D3DRS_STENCILREF, ref);
    IDirect3DDevice9_SetRenderState(DEV, D3DRS_STENCILMASK, mask);
}

static D3DSTENCILOP stencil_op(GLenum op)
{
    switch (op)
    {
    case GL_ZERO: return D3DSTENCILOP_ZERO;
    case GL_REPLACE: return D3DSTENCILOP_REPLACE;
    case GL_INCR: return D3DSTENCILOP_INCRSAT;
    case GL_DECR: return D3DSTENCILOP_DECRSAT;
    case GL_INVERT: return D3DSTENCILOP_INVERT;
    default: return D3DSTENCILOP_KEEP;
    }
}

void APIENTRY glStencilOp(GLenum fail, GLenum zfail, GLenum zpass)
{
    CTX();
    FLUSH();
    IDirect3DDevice9_SetRenderState(DEV, D3DRS_STENCILFAIL, stencil_op(fail));
    IDirect3DDevice9_SetRenderState(DEV, D3DRS_STENCILZFAIL, stencil_op(zfail));
    IDirect3DDevice9_SetRenderState(DEV, D3DRS_STENCILPASS, stencil_op(zpass));
}

void APIENTRY glStencilMask(GLuint mask)
{
    CTX();
    FLUSH();
    IDirect3DDevice9_SetRenderState(DEV, D3DRS_STENCILWRITEMASK, mask);
}

void APIENTRY glViewport(GLint x, GLint y, GLsizei w, GLsizei h)
{
    CTX();
    FLUSH();
    ctx->viewport[0] = x;
    ctx->viewport[1] = y;
    ctx->viewport[2] = w;
    ctx->viewport[3] = h;
    update_viewport();
}

void APIENTRY glScissor(GLint x, GLint y, GLsizei w, GLsizei h)
{
    CTX();
    FLUSH();
    ctx->scissor[0] = x;
    ctx->scissor[1] = y;
    ctx->scissor[2] = w;
    ctx->scissor[3] = h;
    update_scissor();
}

void APIENTRY glClearColor(GLclampf r, GLclampf g, GLclampf b, GLclampf a)
{
    CTX();
    ctx->clear_color = D3DCOLOR_ARGB(to_byte(a), to_byte(r), to_byte(g), to_byte(b));
}

void APIENTRY glClearDepth(GLclampd depth)
{
    CTX();
    ctx->clear_depth = (float)depth;
}

void APIENTRY glClearStencil(GLint s)
{
    CTX();
    ctx->clear_stencil = s;
}

void APIENTRY glClear(GLbitfield mask)
{
    D3DVIEWPORT9 full = { 0, 0, 0, 0, 0, 1 };
    DWORD flags = 0;
    CTX();
    FLUSH();
    if (mask & GL_COLOR_BUFFER_BIT) flags |= D3DCLEAR_TARGET;
    if (mask & GL_DEPTH_BUFFER_BIT) flags |= D3DCLEAR_ZBUFFER;
    if (mask & GL_STENCIL_BUFFER_BIT) flags |= D3DCLEAR_STENCIL;
    if (!flags) return;
    /* Direct3D clears the viewport, GL the whole buffer (both within the
     * scissor rectangle). */
    full.Width = ctx->width;
    full.Height = ctx->height;
    IDirect3DDevice9_SetViewport(DEV, &full);
    IDirect3DDevice9_Clear(DEV, 0, NULL, flags, ctx->clear_color, ctx->clear_depth, ctx->clear_stencil);
    update_viewport();
}

void APIENTRY glFinish(void) { flush(); }
void APIENTRY glFlush(void) { flush(); }
void APIENTRY glHint(GLenum target, GLenum mode) {}
void APIENTRY glDrawBuffer(GLenum mode) {}
void APIENTRY glReadBuffer(GLenum mode) {}
void APIENTRY glLineWidth(GLfloat width) {}
void APIENTRY glPolygonOffset(GLfloat factor, GLfloat units) {}

void APIENTRY glPixelStorei(GLenum name, GLint value)
{
    CTX();
    switch (name)
    {
    case GL_UNPACK_ALIGNMENT: ctx->unpack_alignment = value; break;
    case GL_UNPACK_ROW_LENGTH: ctx->unpack_row_length = value; break;
    case GL_UNPACK_SKIP_ROWS: ctx->unpack_skip_rows = value; break;
    case GL_UNPACK_SKIP_PIXELS: ctx->unpack_skip_pixels = value; break;
    case GL_PACK_ALIGNMENT: ctx->pack_alignment = value; break;
    default: gl_unsupported("glPixelStore", name); break;
    }
}

void APIENTRY glPixelStoref(GLenum name, GLfloat value) { glPixelStorei(name, (GLint)value); }

GLenum APIENTRY glGetError(void)
{
    GLenum e;
    CTX(GL_NO_ERROR);
    e = ctx->error;
    ctx->error = GL_NO_ERROR;
    return e;
}

const GLubyte *APIENTRY glGetString(GLenum name)
{
    switch (name)
    {
    case GL_VENDOR: return (const GLubyte *)"WebWindows";
    case GL_RENDERER: return (const GLubyte *)"OpenGL over Direct3D 9 on WebGPU";
    case GL_VERSION: return (const GLubyte *)"1.1.0";
    case GL_EXTENSIONS: return (const GLubyte *)extensions;
    default: set_error(GL_INVALID_ENUM); return NULL;
    }
}

/* Queries answered as floats; the typed glGet functions convert. */
static int get_floats(GLenum name, float *out)
{
    switch (name)
    {
    case GL_MODELVIEW_MATRIX: memcpy(out, ctx->modelview[ctx->modelview_top], 64); return 16;
    case GL_PROJECTION_MATRIX: memcpy(out, ctx->projection[ctx->projection_top], 64); return 16;
    case GL_TEXTURE_MATRIX: memcpy(out, ctx->texture[ctx->texture_top], 64); return 16;
    case GL_VIEWPORT: out[0] = (float)ctx->viewport[0]; out[1] = (float)ctx->viewport[1];
        out[2] = (float)ctx->viewport[2]; out[3] = (float)ctx->viewport[3]; return 4;
    case GL_SCISSOR_BOX: out[0] = (float)ctx->scissor[0]; out[1] = (float)ctx->scissor[1];
        out[2] = (float)ctx->scissor[2]; out[3] = (float)ctx->scissor[3]; return 4;
    case GL_DEPTH_RANGE: out[0] = ctx->depth_near; out[1] = ctx->depth_far; return 2;
    case GL_CURRENT_COLOR: memcpy(out, ctx->current_rgba, 16); return 4;
    case GL_MATRIX_MODE: out[0] = (float)ctx->matrix_mode; return 1;
    case GL_MAX_TEXTURE_SIZE: out[0] = 2048; return 1;
    case GL_MAX_TEXTURE_UNITS_ARB: out[0] = UNITS; return 1;
    case GL_ACTIVE_TEXTURE_ARB: out[0] = (float)(GL_TEXTURE0_ARB + ctx->active_unit); return 1;
    case GL_TEXTURE_BINDING_2D: out[0] = (float)ctx->units[ctx->active_unit].bound; return 1;
    case GL_MAX_MODELVIEW_STACK_DEPTH: out[0] = 32; return 1;
    case GL_MAX_PROJECTION_STACK_DEPTH: case GL_MAX_TEXTURE_STACK_DEPTH: out[0] = 8; return 1;
    case GL_MAX_VIEWPORT_DIMS: out[0] = out[1] = 4096; return 2;
    case GL_RED_BITS: case GL_GREEN_BITS: case GL_BLUE_BITS: case GL_ALPHA_BITS: case GL_STENCIL_BITS: out[0] = 8; return 1;
    case GL_DEPTH_BITS: out[0] = 24; return 1;
    case GL_DOUBLEBUFFER: out[0] = 1; return 1;
    case GL_UNPACK_ALIGNMENT: out[0] = (float)ctx->unpack_alignment; return 1;
    case GL_PACK_ALIGNMENT: out[0] = (float)ctx->pack_alignment; return 1;
    default:
        gl_unsupported("glGet", name);
        out[0] = 0;
        return 1;
    }
}

void APIENTRY glGetFloatv(GLenum name, GLfloat *params)
{
    CTX();
    get_floats(name, params);
}

void APIENTRY glGetDoublev(GLenum name, GLdouble *params)
{
    float f[16];
    int i, n;
    CTX();
    n = get_floats(name, f);
    for (i = 0; i < n; i++) params[i] = f[i];
}

void APIENTRY glGetIntegerv(GLenum name, GLint *params)
{
    float f[16];
    int i, n;
    CTX();
    n = get_floats(name, f);
    for (i = 0; i < n; i++) params[i] = (GLint)f[i];
}

void APIENTRY glGetBooleanv(GLenum name, GLboolean *params)
{
    float f[16];
    int i, n;
    CTX();
    n = get_floats(name, f);
    for (i = 0; i < n; i++) params[i] = f[i] != 0;
}

/* ------------------------------------------------------------------ */
/* GL: textures                                                        */

void APIENTRY glGenTextures(GLsizei n, GLuint *names)
{
    GLsizei i;
    CTX();
    for (i = 0; i < n; i++)
    {
        while (get_texture(ctx->next_name, FALSE)) ctx->next_name++;
        names[i] = ctx->next_name;
        get_texture(ctx->next_name++, TRUE);
    }
}

void APIENTRY glDeleteTextures(GLsizei n, const GLuint *names)
{
    GLsizei i;
    int u;
    CTX();
    FLUSH();
    for (i = 0; i < n; i++)
    {
        struct texture *t = names[i] ? get_texture(names[i], FALSE) : NULL;
        if (!t) continue;
        for (u = 0; u < UNITS; u++)
            if (ctx->units[u].bound == names[i])
            {
                ctx->units[u].bound = 0;
                update_stage(u);
            }
        if (t->tex) IDirect3DTexture9_Release(t->tex);
        free(t);
        ctx->textures[names[i]] = NULL;
    }
}

GLboolean APIENTRY glIsTexture(GLuint name)
{
    CTX(GL_FALSE);
    return name && get_texture(name, FALSE) ? GL_TRUE : GL_FALSE;
}

void APIENTRY glBindTexture(GLenum target, GLuint name)
{
    struct unit *u;
    CTX();
    if (target != GL_TEXTURE_2D) { gl_unsupported("glBindTexture target", target); return; }
    u = &ctx->units[ctx->active_unit];
    if (u->bound == name) return;
    FLUSH();
    if (name) get_texture(name, TRUE);
    u->bound = name;
    if (u->enabled) update_stage(ctx->active_unit);
}

static struct texture *bound_texture(GLenum target)
{
    GLuint name = ctx->units[ctx->active_unit].bound;
    if (target != GL_TEXTURE_2D) { gl_unsupported("texture target", target); return NULL; }
    /* Texture object 0 is a texture too. */
    return get_texture(name, TRUE);
}

void APIENTRY glTexImage2D(GLenum target, GLint level, GLint internal, GLsizei w, GLsizei h, GLint border,
                           GLenum format, GLenum type, const GLvoid *pixels)
{
    struct texture *t;
    CTX();
    if (!(t = bound_texture(target)) || w <= 0 || h <= 0) return;
    FLUSH();
    if (level == 0)
    {
        enum base_format base = base_format_of(internal);
        if (!t->tex || t->width != w || t->height != h || t->base != base)
        {
            if (t->tex) IDirect3DTexture9_Release(t->tex);
            t->tex = NULL;
            /* With every level: a GL texture gets its levels one call at a time. */
            if (FAILED(IDirect3DDevice9_CreateTexture(DEV, w, h, 0, 0, D3DFMT_A8R8G8B8, D3DPOOL_MANAGED, &t->tex, NULL)))
            {
                gl_log("err:opengl32: CreateTexture %dx%d failed\n", w, h);
                return;
            }
            t->width = w;
            t->height = h;
            t->levels = IDirect3DTexture9_GetLevelCount(t->tex);
            t->base = base;
            update_stages_using(ctx->units[ctx->active_unit].bound);
        }
    }
    upload(t, level, 0, 0, w, h, format, type, pixels);
}

void APIENTRY glTexSubImage2D(GLenum target, GLint level, GLint x, GLint y, GLsizei w, GLsizei h, GLenum format,
                              GLenum type, const GLvoid *pixels)
{
    struct texture *t;
    CTX();
    if (!(t = bound_texture(target))) return;
    FLUSH();
    upload(t, level, x, y, w, h, format, type, pixels);
}

static void tex_parameter(GLenum target, GLenum name, GLint value)
{
    struct texture *t;
    CTX();
    if (!(t = bound_texture(target))) return;
    FLUSH();
    switch (name)
    {
    case GL_TEXTURE_MIN_FILTER: t->min_filter = value; break;
    case GL_TEXTURE_MAG_FILTER: t->mag_filter = value; break;
    case GL_TEXTURE_WRAP_S: t->wrap_s = value; break;
    case GL_TEXTURE_WRAP_T: t->wrap_t = value; break;
    default: gl_unsupported("glTexParameter", name); return;
    }
    update_stages_using(ctx->units[ctx->active_unit].bound);
}

void APIENTRY glTexParameteri(GLenum target, GLenum name, GLint value) { tex_parameter(target, name, value); }
void APIENTRY glTexParameterf(GLenum target, GLenum name, GLfloat value) { tex_parameter(target, name, (GLint)value); }
void APIENTRY glTexParameteriv(GLenum target, GLenum name, const GLint *v) { tex_parameter(target, name, v[0]); }
void APIENTRY glTexParameterfv(GLenum target, GLenum name, const GLfloat *v) { tex_parameter(target, name, (GLint)v[0]); }

static void tex_env(GLenum target, GLenum name, GLint value)
{
    CTX();
    if (target != GL_TEXTURE_ENV || name != GL_TEXTURE_ENV_MODE)
    {
        gl_unsupported("glTexEnv", name);
        return;
    }
    FLUSH();
    ctx->units[ctx->active_unit].env_mode = value;
    update_stage(ctx->active_unit);
}

void APIENTRY glTexEnvi(GLenum target, GLenum name, GLint value) { tex_env(target, name, value); }
void APIENTRY glTexEnvf(GLenum target, GLenum name, GLfloat value) { tex_env(target, name, (GLint)value); }
void APIENTRY glTexEnviv(GLenum target, GLenum name, const GLint *v) { tex_env(target, name, v[0]); }
void APIENTRY glTexEnvfv(GLenum target, GLenum name, const GLfloat *v)
{
    /* GL_TEXTURE_ENV_COLOR (for GL_BLEND) is not used. */
    if (name == GL_TEXTURE_ENV_MODE) tex_env(target, name, (GLint)v[0]);
}

/* ------------------------------------------------------------------ */
/* GL: reading back                                                    */

void APIENTRY glReadPixels(GLint x, GLint y, GLsizei w, GLsizei h, GLenum format, GLenum type, GLvoid *pixels)
{
    IDirect3DSurface9 *rt = NULL, *copy = NULL;
    D3DSURFACE_DESC desc;
    D3DLOCKED_RECT lr;
    int comps = format == GL_RGBA || format == GL_BGRA_EXT ? 4 : 3, row, i, j;
    CTX();
    if (type != GL_UNSIGNED_BYTE || (format != GL_RGB && format != GL_RGBA && format != GL_BGR_EXT && format != GL_BGRA_EXT))
    {
        gl_unsupported("glReadPixels format", format);
        return;
    }
    flush();
    if (FAILED(IDirect3DDevice9_GetRenderTarget(DEV, 0, &rt))) return;
    IDirect3DSurface9_GetDesc(rt, &desc);
    if (SUCCEEDED(IDirect3DDevice9_CreateOffscreenPlainSurface(DEV, desc.Width, desc.Height, desc.Format,
                                                               D3DPOOL_SYSTEMMEM, &copy, NULL))
        && SUCCEEDED(IDirect3DDevice9_GetRenderTargetData(DEV, rt, copy))
        && SUCCEEDED(IDirect3DSurface9_LockRect(copy, &lr, NULL, D3DLOCK_READONLY)))
    {
        row = w * comps;
        if (ctx->pack_alignment > 1) row = (row + ctx->pack_alignment - 1) / ctx->pack_alignment * ctx->pack_alignment;
        for (j = 0; j < h; j++)
        {
            /* GL's rows go up from the bottom. */
            int sy = (int)desc.Height - 1 - (y + j);
            BYTE *d = (BYTE *)pixels + j * row;
            for (i = 0; i < w; i++, d += comps)
            {
                int sx = x + i;
                const BYTE *s = (const BYTE *)lr.pBits + sy * lr.Pitch + sx * 4;
                BOOL inside = sx >= 0 && sy >= 0 && sx < (int)desc.Width && sy < (int)desc.Height;
                BYTE b = inside ? s[0] : 0, g = inside ? s[1] : 0, r = inside ? s[2] : 0;
                if (format == GL_BGR_EXT || format == GL_BGRA_EXT) { d[0] = b; d[1] = g; d[2] = r; }
                else { d[0] = r; d[1] = g; d[2] = b; }
                if (comps == 4) d[3] = 255;
            }
        }
        IDirect3DSurface9_UnlockRect(copy);
    }
    if (copy) IDirect3DSurface9_Release(copy);
    IDirect3DSurface9_Release(rt);
}

/* ------------------------------------------------------------------ */

BOOL WINAPI DllMain(HINSTANCE instance, DWORD reason, LPVOID reserved)
{
    if (reason == DLL_PROCESS_ATTACH)
    {
        HMODULE ntdll = GetModuleHandleA("ntdll.dll");
        if (ntdll) wine_dbg_output = (void *)GetProcAddress(ntdll, "__wine_dbg_output");
        DisableThreadLibraryCalls(instance);
    }
    return TRUE;
}
