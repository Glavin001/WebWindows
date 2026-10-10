/* OpenGL 1.x-2.0 on translated Wine, checked: each test draws into the
 * back buffer, reads pixels back with glReadPixels and compares them.
 * Prints "ok"/"not ok" per check and a summary; the exit code is the
 * number of failures.
 *
 * GLSL programs (attributes, a matrix, uniform arrays set by "name[i]" and
 * by consecutive locations), vertex and index buffers, client-side arrays
 * with client-side indices, render to texture with a framebuffer object,
 * BGRA texture uploads, display lists, alpha test, linear fog, the stencil
 * test, multitexturing, glCopyTexSubImage2D and generated mipmaps.
 */
#include <windows.h>
#include <GL/gl.h>
#include <GL/glext.h>
#include <stdio.h>
#include <stdlib.h>

#define W 128
#define H 128

static int failures, checks;

#define FN(type, name) static type name##_
FN(PFNGLCREATESHADERPROC, glCreateShader);
FN(PFNGLSHADERSOURCEPROC, glShaderSource);
FN(PFNGLCOMPILESHADERPROC, glCompileShader);
FN(PFNGLGETSHADERIVPROC, glGetShaderiv);
FN(PFNGLGETSHADERINFOLOGPROC, glGetShaderInfoLog);
FN(PFNGLCREATEPROGRAMPROC, glCreateProgram);
FN(PFNGLATTACHSHADERPROC, glAttachShader);
FN(PFNGLBINDATTRIBLOCATIONPROC, glBindAttribLocation);
FN(PFNGLLINKPROGRAMPROC, glLinkProgram);
FN(PFNGLGETPROGRAMIVPROC, glGetProgramiv);
FN(PFNGLGETPROGRAMINFOLOGPROC, glGetProgramInfoLog);
FN(PFNGLUSEPROGRAMPROC, glUseProgram);
FN(PFNGLGETUNIFORMLOCATIONPROC, glGetUniformLocation);
FN(PFNGLUNIFORM4FPROC, glUniform4f);
FN(PFNGLUNIFORM4FVPROC, glUniform4fv);
FN(PFNGLUNIFORMMATRIX4FVPROC, glUniformMatrix4fv);
FN(PFNGLVERTEXATTRIBPOINTERPROC, glVertexAttribPointer);
FN(PFNGLENABLEVERTEXATTRIBARRAYPROC, glEnableVertexAttribArray);
FN(PFNGLDISABLEVERTEXATTRIBARRAYPROC, glDisableVertexAttribArray);
FN(PFNGLGENBUFFERSPROC, glGenBuffers);
FN(PFNGLBINDBUFFERPROC, glBindBuffer);
FN(PFNGLBUFFERDATAPROC, glBufferData);
FN(PFNGLGENFRAMEBUFFERSEXTPROC, glGenFramebuffersEXT);
FN(PFNGLBINDFRAMEBUFFEREXTPROC, glBindFramebufferEXT);
FN(PFNGLFRAMEBUFFERTEXTURE2DEXTPROC, glFramebufferTexture2DEXT);
FN(PFNGLCHECKFRAMEBUFFERSTATUSEXTPROC, glCheckFramebufferStatusEXT);
FN(PFNGLACTIVETEXTUREARBPROC, glActiveTextureARB);
FN(PFNGLMULTITEXCOORD2FARBPROC, glMultiTexCoord2fARB);

static BOOL load(void)
{
#define LOAD(name) if (!(name##_ = (void *)wglGetProcAddress(#name))) { printf("missing %s\n", #name); return FALSE; }
    LOAD(glCreateShader) LOAD(glShaderSource) LOAD(glCompileShader) LOAD(glGetShaderiv)
    LOAD(glGetShaderInfoLog) LOAD(glCreateProgram) LOAD(glAttachShader) LOAD(glBindAttribLocation)
    LOAD(glLinkProgram) LOAD(glGetProgramiv) LOAD(glGetProgramInfoLog) LOAD(glUseProgram)
    LOAD(glGetUniformLocation) LOAD(glUniform4f) LOAD(glUniform4fv) LOAD(glUniformMatrix4fv)
    LOAD(glVertexAttribPointer) LOAD(glEnableVertexAttribArray) LOAD(glDisableVertexAttribArray)
    LOAD(glGenBuffers) LOAD(glBindBuffer) LOAD(glBufferData) LOAD(glGenFramebuffersEXT)
    LOAD(glBindFramebufferEXT) LOAD(glFramebufferTexture2DEXT) LOAD(glCheckFramebufferStatusEXT)
    LOAD(glActiveTextureARB) LOAD(glMultiTexCoord2fARB)
    return TRUE;
}

/* The pixel at (x, y), GL's window coordinates (y up), as 0xRRGGBB. */
static DWORD pixel(int x, int y)
{
    BYTE p[4];

    glReadPixels(x, y, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, p);
    return (p[0] << 16) | (p[1] << 8) | p[2];
}

static BOOL close_to(DWORD a, DWORD b)
{
    int i;

    for (i = 0; i < 24; i += 8)
        if (abs((int)((a >> i) & 0xff) - (int)((b >> i) & 0xff)) > 3)
            return FALSE;
    return TRUE;
}

static void check_pixel(const char *name, int x, int y, DWORD want)
{
    DWORD got = pixel(x, y);
    GLenum err = glGetError();

    ++checks;
    if (close_to(got, want) && !err)
    {
        printf("ok %d - %s\n", checks, name);
        return;
    }
    ++failures;
    printf("not ok %d - %s: pixel (%d,%d) %06lx, expected %06lx, error %#x\n", checks, name, x, y,
            (unsigned long)got, (unsigned long)want, err);
}

static void begin(void)
{
    glBindFramebufferEXT_(GL_FRAMEBUFFER_EXT, 0);
    glViewport(0, 0, W, H);
    glClearColor(0, 0, 0, 1);
    glClearDepth(1);
    glClearStencil(0);
    glClear(GL_COLOR_BUFFER_BIT | GL_DEPTH_BUFFER_BIT | GL_STENCIL_BUFFER_BIT);
    glMatrixMode(GL_PROJECTION);
    glLoadIdentity();
    glOrtho(0, W, 0, H, -1, 1);
    glMatrixMode(GL_MODELVIEW);
    glLoadIdentity();
    glColor4f(1, 1, 1, 1);
}

static void rect(float x0, float y0, float x1, float y1)
{
    glBegin(GL_QUADS);
    glTexCoord2f(0, 0); glVertex2f(x0, y0);
    glTexCoord2f(1, 0); glVertex2f(x1, y0);
    glTexCoord2f(1, 1); glVertex2f(x1, y1);
    glTexCoord2f(0, 1); glVertex2f(x0, y1);
    glEnd();
}

static GLuint shader(GLenum type, const char *src)
{
    GLuint s = glCreateShader_(type);
    GLint ok;
    char log[1024];

    glShaderSource_(s, 1, &src, NULL);
    glCompileShader_(s);
    glGetShaderiv_(s, GL_COMPILE_STATUS, &ok);
    if (!ok)
    {
        glGetShaderInfoLog_(s, sizeof(log), NULL, log);
        printf("shader: %s\n", log);
    }
    return s;
}

static void test_glsl(void)
{
    static const char vs[] =
        "uniform mat4 mvp;\n"
        "attribute vec2 pos;\n"
        "attribute vec3 col;\n"
        "varying vec3 v_col;\n"
        "void main() { v_col = col; gl_Position = mvp * vec4(pos, 0.0, 1.0); }\n";
    static const char fs[] =
        "uniform vec4 colors[3];\n"
        "uniform vec4 tint;\n"
        "varying vec3 v_col;\n"
        "void main() { gl_FragColor = vec4(v_col, 1.0) * tint + colors[0] + colors[1] + colors[2]; }\n";
    /* Two triangles covering the left half: per-vertex colour (0.5, 0, 0)
     * in the bottom-left corner, black elsewhere. */
    static const float verts[] = {0, 0, 64, 0, 64, 128, 0, 128};
    static const float cols[] = {0.5f, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
    static const GLushort idx[] = {0, 1, 2, 0, 2, 3};
    float mvp[16] = {2.0f / W, 0, 0, 0, 0, 2.0f / H, 0, 0, 0, 0, 1, 0, -1, -1, 0, 1};
    float c1[4] = {0, 0.5f, 0, 0}, c2[4] = {0, 0, 0.25f, 0};
    GLuint prog = glCreateProgram_(), bufs[3];
    GLint ok, loc;
    char log[1024];

    glAttachShader_(prog, shader(GL_VERTEX_SHADER, vs));
    glAttachShader_(prog, shader(GL_FRAGMENT_SHADER, fs));
    glBindAttribLocation_(prog, 0, "pos");
    glBindAttribLocation_(prog, 1, "col");
    glLinkProgram_(prog);
    glGetProgramiv_(prog, GL_LINK_STATUS, &ok);
    if (!ok)
    {
        glGetProgramInfoLog_(prog, sizeof(log), NULL, log);
        printf("link: %s\n", log);
    }
    begin();
    glUseProgram_(prog);
    glUniformMatrix4fv_(glGetUniformLocation_(prog, "mvp"), 1, GL_FALSE, mvp);
    glUniform4f_(glGetUniformLocation_(prog, "tint"), 1, 1, 1, 1);
    /* colors[0] by its base name, colors[1] by name, colors[2] by the
     * location after colors[1]'s. */
    glUniform4f_(glGetUniformLocation_(prog, "colors"), 0, 0, 0, 0);
    loc = glGetUniformLocation_(prog, "colors[1]");
    glUniform4fv_(loc, 1, c1);
    glUniform4fv_(loc + 1, 1, c2);
    glGenBuffers_(3, bufs);
    glBindBuffer_(GL_ARRAY_BUFFER, bufs[0]);
    glBufferData_(GL_ARRAY_BUFFER, sizeof(verts), verts, GL_STATIC_DRAW);
    glVertexAttribPointer_(0, 2, GL_FLOAT, GL_FALSE, 0, 0);
    glBindBuffer_(GL_ARRAY_BUFFER, bufs[1]);
    glBufferData_(GL_ARRAY_BUFFER, sizeof(cols), cols, GL_STATIC_DRAW);
    glVertexAttribPointer_(1, 3, GL_FLOAT, GL_FALSE, 0, 0);
    glBindBuffer_(GL_ELEMENT_ARRAY_BUFFER, bufs[2]);
    glBufferData_(GL_ELEMENT_ARRAY_BUFFER, sizeof(idx), idx, GL_STATIC_DRAW);
    glEnableVertexAttribArray_(0);
    glEnableVertexAttribArray_(1);
    glDrawElements(GL_TRIANGLES, 6, GL_UNSIGNED_SHORT, 0);
    glDisableVertexAttribArray_(0);
    glDisableVertexAttribArray_(1);
    glBindBuffer_(GL_ARRAY_BUFFER, 0);
    glBindBuffer_(GL_ELEMENT_ARRAY_BUFFER, 0);
    glUseProgram_(0);
    check_pixel("GLSL: uniform arrays, buffers, indices", 32, 127, 0x008040);
    check_pixel("GLSL: interpolated attribute", 1, 1, 0x7e8040);
    check_pixel("GLSL: outside the triangles", 100, 64, 0x000000);
}

static void test_client_arrays(void)
{
    static const float verts[] = {64, 0, 128, 0, 128, 64, 64, 64};
    static const GLubyte cols[] = {255, 0, 255, 255, 255, 0, 255, 255, 255, 0, 255, 255, 255, 0, 255, 255};
    static const GLubyte idx[] = {0, 1, 2, 2, 3, 0};

    begin();
    glEnableClientState(GL_VERTEX_ARRAY);
    glEnableClientState(GL_COLOR_ARRAY);
    glVertexPointer(2, GL_FLOAT, 0, verts);
    glColorPointer(4, GL_UNSIGNED_BYTE, 0, cols);
    glDrawElements(GL_TRIANGLES, 6, GL_UNSIGNED_BYTE, idx);
    glDisableClientState(GL_VERTEX_ARRAY);
    glDisableClientState(GL_COLOR_ARRAY);
    check_pixel("client arrays and indices", 96, 32, 0xff00ff);
    check_pixel("client arrays: nothing elsewhere", 32, 96, 0x000000);
}

static GLuint texture(int w, int h, GLenum format, const void *data)
{
    GLuint t;

    glGenTextures(1, &t);
    glBindTexture(GL_TEXTURE_2D, t);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, w, h, 0, format, GL_UNSIGNED_BYTE, data);
    return t;
}

static void test_fbo(void)
{
    GLuint fbo, tex = texture(64, 64, GL_RGBA, NULL);
    GLenum status;

    glGenFramebuffersEXT_(1, &fbo);
    glBindFramebufferEXT_(GL_FRAMEBUFFER_EXT, fbo);
    glFramebufferTexture2DEXT_(GL_FRAMEBUFFER_EXT, GL_COLOR_ATTACHMENT0_EXT, GL_TEXTURE_2D, tex, 0);
    status = glCheckFramebufferStatusEXT_(GL_FRAMEBUFFER_EXT);
    ++checks;
    if (status == GL_FRAMEBUFFER_COMPLETE_EXT) printf("ok %d - framebuffer complete\n", checks);
    else printf("not ok %d - framebuffer status %#x\n", checks, status), ++failures;
    glViewport(0, 0, 64, 64);
    glClearColor(0, 1, 0, 1);
    glClear(GL_COLOR_BUFFER_BIT);
    glBindFramebufferEXT_(GL_FRAMEBUFFER_EXT, 0);
    begin();
    glEnable(GL_TEXTURE_2D);
    glBindTexture(GL_TEXTURE_2D, tex);
    rect(0, 0, 64, 64);
    glDisable(GL_TEXTURE_2D);
    check_pixel("render to texture", 32, 32, 0x00ff00);
}

static void test_bgra_and_lists(void)
{
    /* BGRA bytes: a blue row under a red one. */
    static const GLubyte px[] = {255, 0, 0, 255, 255, 0, 0, 255, 0, 0, 255, 255, 0, 0, 255, 255};
    GLuint tex = texture(2, 2, GL_BGRA, px), list = glGenLists(1);

    begin();
    glNewList(list, GL_COMPILE);
    glEnable(GL_TEXTURE_2D);
    glBindTexture(GL_TEXTURE_2D, tex);
    rect(0, 0, 64, 64);
    glDisable(GL_TEXTURE_2D);
    glEndList();
    glCallList(list);
    check_pixel("BGRA texture in a display list (row 0)", 16, 16, 0x0000ff);
    check_pixel("BGRA texture in a display list (row 1)", 16, 48, 0xff0000);
}

static void test_alpha_fog(void)
{
    begin();
    /* Alpha test: alpha 0.25 fails GL_GREATER 0.5, so nothing is drawn. */
    glEnable(GL_ALPHA_TEST);
    glAlphaFunc(GL_GREATER, 0.5f);
    glColor4f(1, 1, 1, 0.25f);
    rect(0, 0, 64, 64);
    glColor4f(1, 1, 0, 0.75f);
    rect(64, 0, 128, 64);
    glDisable(GL_ALPHA_TEST);
    check_pixel("alpha test: rejected", 32, 32, 0x000000);
    check_pixel("alpha test: passed", 96, 32, 0xffff00);
    /* Linear fog over eye distance 0..2, the quad at distance 1 (z = -1
     * in an orthographic projection from -2 to 2): half fog colour. */
    glMatrixMode(GL_PROJECTION);
    glLoadIdentity();
    glOrtho(0, W, 0, H, -2, 2);
    glMatrixMode(GL_MODELVIEW);
    glEnable(GL_FOG);
    glFogi(GL_FOG_MODE, GL_LINEAR);
    glFogf(GL_FOG_START, 0);
    glFogf(GL_FOG_END, 2);
    {
        float fog[4] = {0, 0, 1, 1};
        glFogfv(GL_FOG_COLOR, fog);
    }
    glColor4f(1, 0, 0, 1);
    glBegin(GL_QUADS);
    glVertex3f(0, 64, -1); glVertex3f(64, 64, -1); glVertex3f(64, 128, -1); glVertex3f(0, 128, -1);
    glEnd();
    glDisable(GL_FOG);
    check_pixel("linear fog", 32, 96, 0x800080);
}

static void test_stencil_multitex(void)
{
    static const GLubyte red[] = {255, 0, 0, 255}, grey[] = {128, 128, 128, 255};
    GLuint t0, t1;

    begin();
    glEnable(GL_STENCIL_TEST);
    glStencilFunc(GL_ALWAYS, 1, 0xff);
    glStencilOp(GL_KEEP, GL_KEEP, GL_REPLACE);
    glColorMask(GL_FALSE, GL_FALSE, GL_FALSE, GL_FALSE);
    rect(0, 0, 32, 128);
    glColorMask(GL_TRUE, GL_TRUE, GL_TRUE, GL_TRUE);
    glStencilFunc(GL_EQUAL, 1, 0xff);
    glStencilOp(GL_KEEP, GL_KEEP, GL_KEEP);
    glColor4f(0, 1, 1, 1);
    rect(0, 0, 64, 128);
    glDisable(GL_STENCIL_TEST);
    check_pixel("stencil: inside", 16, 64, 0x00ffff);
    check_pixel("stencil: outside", 48, 64, 0x000000);

    /* Two units: red modulated by grey. */
    t0 = texture(1, 1, GL_RGBA, red);
    t1 = texture(1, 1, GL_RGBA, grey);
    glColor4f(1, 1, 1, 1);
    glActiveTextureARB_(GL_TEXTURE0_ARB);
    glEnable(GL_TEXTURE_2D);
    glBindTexture(GL_TEXTURE_2D, t0);
    glTexEnvi(GL_TEXTURE_ENV, GL_TEXTURE_ENV_MODE, GL_MODULATE);
    glActiveTextureARB_(GL_TEXTURE1_ARB);
    glEnable(GL_TEXTURE_2D);
    glBindTexture(GL_TEXTURE_2D, t1);
    glTexEnvi(GL_TEXTURE_ENV, GL_TEXTURE_ENV_MODE, GL_MODULATE);
    glBegin(GL_QUADS);
    glMultiTexCoord2fARB_(GL_TEXTURE0_ARB, 0, 0); glMultiTexCoord2fARB_(GL_TEXTURE1_ARB, 0, 0); glVertex2f(64, 0);
    glMultiTexCoord2fARB_(GL_TEXTURE0_ARB, 1, 0); glMultiTexCoord2fARB_(GL_TEXTURE1_ARB, 1, 0); glVertex2f(128, 0);
    glMultiTexCoord2fARB_(GL_TEXTURE0_ARB, 1, 1); glMultiTexCoord2fARB_(GL_TEXTURE1_ARB, 1, 1); glVertex2f(128, 64);
    glMultiTexCoord2fARB_(GL_TEXTURE0_ARB, 0, 1); glMultiTexCoord2fARB_(GL_TEXTURE1_ARB, 0, 1); glVertex2f(64, 64);
    glEnd();
    glDisable(GL_TEXTURE_2D);
    glActiveTextureARB_(GL_TEXTURE0_ARB);
    glDisable(GL_TEXTURE_2D);
    check_pixel("multitexture modulate", 96, 32, 0x800000);
}

static void test_copy_mipmap(void)
{
    static GLubyte px[16 * 16 * 4];
    GLuint tex;
    int i;

    /* Copy a yellow block of the frame into a texture, then draw it. */
    begin();
    glColor4f(1, 1, 0, 1);
    rect(0, 0, 16, 16);
    tex = texture(16, 16, GL_RGBA, NULL);
    glCopyTexSubImage2D(GL_TEXTURE_2D, 0, 0, 0, 0, 0, 16, 16);
    glClear(GL_COLOR_BUFFER_BIT);
    glColor4f(1, 1, 1, 1);
    glEnable(GL_TEXTURE_2D);
    rect(64, 64, 128, 128);
    check_pixel("glCopyTexSubImage2D", 96, 96, 0xffff00);

    /* A 16x16 checker of red and blue texels, mipmapped by the GL, drawn
     * at 1x1: the last level is their average. */
    for (i = 0; i < 16 * 16; ++i)
    {
        int odd = ((i % 16) + (i / 16)) & 1;
        px[4 * i] = odd ? 0 : 255;
        px[4 * i + 1] = 0;
        px[4 * i + 2] = odd ? 255 : 0;
        px[4 * i + 3] = 255;
    }
    glGenTextures(1, &tex);
    glBindTexture(GL_TEXTURE_2D, tex);
    glTexParameteri(GL_TEXTURE_2D, GL_GENERATE_MIPMAP, GL_TRUE);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST_MIPMAP_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, 16, 16, 0, GL_RGBA, GL_UNSIGNED_BYTE, px);
    rect(10, 10, 11, 11);
    glDisable(GL_TEXTURE_2D);
    check_pixel("generated mipmaps", 10, 10, 0x800080);
}

static LRESULT CALLBACK proc(HWND hwnd, UINT msg, WPARAM wp, LPARAM lp)
{
    return DefWindowProcA(hwnd, msg, wp, lp);
}

int main(void)
{
    PIXELFORMATDESCRIPTOR pfd = {sizeof(pfd), 1, PFD_DRAW_TO_WINDOW | PFD_SUPPORT_OPENGL | PFD_DOUBLEBUFFER,
            PFD_TYPE_RGBA, 32};
    WNDCLASSA wc = {0};
    RECT rc = {0, 0, W, H};
    HWND hwnd;
    HDC dc;

    wc.lpfnWndProc = proc;
    wc.hInstance = GetModuleHandleA(NULL);
    wc.lpszClassName = "gl2test";
    RegisterClassA(&wc);
    AdjustWindowRect(&rc, WS_OVERLAPPEDWINDOW, FALSE);
    hwnd = CreateWindowA("gl2test", "OpenGL 2 test", WS_OVERLAPPEDWINDOW | WS_VISIBLE, 40, 30, rc.right - rc.left,
            rc.bottom - rc.top, NULL, NULL, wc.hInstance, NULL);
    dc = GetDC(hwnd);
    pfd.cDepthBits = 24;
    pfd.cStencilBits = 8;
    SetPixelFormat(dc, ChoosePixelFormat(dc, &pfd), &pfd);
    wglMakeCurrent(dc, wglCreateContext(dc));
    printf("GL_RENDERER: %s\nGL_VERSION: %s\n", glGetString(GL_RENDERER), glGetString(GL_VERSION));
    if (!load())
        return 99;
    test_glsl();
    test_client_arrays();
    test_fbo();
    test_bgra_and_lists();
    test_alpha_fog();
    test_stencil_multitex();
    test_copy_mipmap();
    SwapBuffers(dc);
    printf("gl2test: %d checks, %d failures\n", checks, failures);
    return failures;
}
