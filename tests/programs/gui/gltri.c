/* OpenGL 1.1 on translated Wine (native/opengl32, over Direct3D 9): a
 * window, a pixel format, a context, then one frame of what Quake II's
 * renderer does: a clear, a perspective triangle with per-vertex colours,
 * depth-tested quads, a texture with nearest filtering in GL_REPLACE mode,
 * an alpha-blended quad and a 2D overlay in an orthographic projection;
 * then SwapBuffers. Prints the result of each step.
 *
 * Window at (40,30), client area 320x240, dark blue background:
 *   - a yellow square at (10,10)-(60,60) (2D overlay, drawn last);
 *   - a triangle with red, green and blue corners in the middle;
 *   - at (230,20)-(310,100) a green square in front of a red one
 *     (the red one drawn later, farther away: the depth test hides it);
 *   - at (230,140)-(310,220) a 2x2 texture: red, white / white, red,
 *     with half-transparent white blended over its right half.
 */
#include <windows.h>
#include <GL/gl.h>
#include <stdio.h>

static HDC dc;

static void quad2d(float x0, float y0, float x1, float y1, float z)
{
    /* Window coordinates, y down, in the orthographic projection below. */
    glBegin(GL_QUADS);
    glTexCoord2f(0, 0); glVertex3f(x0, y0, z);
    glTexCoord2f(1, 0); glVertex3f(x1, y0, z);
    glTexCoord2f(1, 1); glVertex3f(x1, y1, z);
    glTexCoord2f(0, 1); glVertex3f(x0, y1, z);
    glEnd();
}

static void frame(GLuint tex)
{
    glViewport(0, 0, 320, 240);
    glClearColor(0, 0, 0.5f, 1);
    glClear(GL_COLOR_BUFFER_BIT | GL_DEPTH_BUFFER_BIT);

    /* The triangle, in perspective. */
    glMatrixMode(GL_PROJECTION);
    glLoadIdentity();
    glFrustum(-0.1, 0.1, -0.075, 0.075, 0.1, 100);
    glMatrixMode(GL_MODELVIEW);
    glLoadIdentity();
    glTranslatef(0, 0, -3);
    glDisable(GL_TEXTURE_2D);
    glBegin(GL_TRIANGLES);
    glColor3f(1, 0, 0); glVertex3f(-0.8f, -0.8f, 0);
    glColor3f(0, 1, 0); glVertex3f(0.8f, -0.8f, 0);
    glColor3f(0, 0, 1); glVertex3f(0, 0.8f, 0);
    glEnd();

    /* Orthographic, y down, depth from 0 (near) to 1 (far). */
    glMatrixMode(GL_PROJECTION);
    glLoadIdentity();
    glOrtho(0, 320, 240, 0, 0, -1);
    glMatrixMode(GL_MODELVIEW);
    glLoadIdentity();

    glEnable(GL_DEPTH_TEST);
    glDepthFunc(GL_LEQUAL);
    glColor3f(0, 1, 0);
    quad2d(230, 20, 310, 100, 0.25f);
    glColor3f(1, 0, 0);
    quad2d(230, 20, 310, 100, 0.75f);
    glDisable(GL_DEPTH_TEST);

    glEnable(GL_TEXTURE_2D);
    glBindTexture(GL_TEXTURE_2D, tex);
    glTexEnvf(GL_TEXTURE_ENV, GL_TEXTURE_ENV_MODE, GL_REPLACE);
    glColor3f(0, 0, 0); /* replaced by the texture */
    quad2d(230, 140, 310, 220, 0);
    glDisable(GL_TEXTURE_2D);

    glEnable(GL_BLEND);
    glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
    glColor4f(1, 1, 1, 0.5f);
    quad2d(270, 140, 310, 220, 0);
    glDisable(GL_BLEND);

    glColor3f(1, 1, 0);
    quad2d(10, 10, 60, 60, 0);
}

static LRESULT CALLBACK wndproc(HWND hwnd, UINT msg, WPARAM wp, LPARAM lp)
{
    if (msg == WM_DESTROY) PostQuitMessage(0);
    return DefWindowProcA(hwnd, msg, wp, lp);
}

int main(void)
{
    static const GLubyte checker[] = { 255, 0, 0, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 0, 0, 255 };
    PIXELFORMATDESCRIPTOR pfd = { sizeof(pfd), 1, PFD_DRAW_TO_WINDOW | PFD_SUPPORT_OPENGL | PFD_DOUBLEBUFFER, PFD_TYPE_RGBA, 32 };
    WNDCLASSA wc = { 0 };
    RECT rc = { 0, 0, 320, 240 };
    HWND hwnd;
    HGLRC rc_gl;
    GLuint tex;
    MSG msg;
    int fmt;

    setvbuf(stdout, NULL, _IONBF, 0);
    wc.lpfnWndProc = wndproc;
    wc.hInstance = GetModuleHandleA(NULL);
    wc.lpszClassName = "gltri";
    RegisterClassA(&wc);
    AdjustWindowRect(&rc, WS_OVERLAPPEDWINDOW, FALSE);
    hwnd = CreateWindowA("gltri", "OpenGL", WS_OVERLAPPEDWINDOW, 40, 30, rc.right - rc.left, rc.bottom - rc.top,
                         NULL, NULL, wc.hInstance, NULL);
    ShowWindow(hwnd, SW_SHOW);
    UpdateWindow(hwnd);
    dc = GetDC(hwnd);

    pfd.cDepthBits = 24;
    fmt = ChoosePixelFormat(dc, &pfd);
    printf("ChoosePixelFormat: %d\n", fmt);
    printf("SetPixelFormat: %d\n", SetPixelFormat(dc, fmt, &pfd));
    rc_gl = wglCreateContext(dc);
    printf("wglCreateContext: %s\n", rc_gl ? "ok" : "failed");
    if (!rc_gl) return 1;
    printf("wglMakeCurrent: %d\n", wglMakeCurrent(dc, rc_gl));
    printf("GL_RENDERER: %s\n", (const char *)glGetString(GL_RENDERER));
    printf("GL_VERSION: %s\n", (const char *)glGetString(GL_VERSION));

    glGenTextures(1, &tex);
    glBindTexture(GL_TEXTURE_2D, tex);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, 2, 2, 0, GL_RGBA, GL_UNSIGNED_BYTE, checker);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);

    frame(tex);
    printf("SwapBuffers: %d\n", SwapBuffers(dc));
    printf("glGetError: %#x\n", glGetError());

    while (GetMessageA(&msg, NULL, 0, 0))
    {
        TranslateMessage(&msg);
        DispatchMessageA(&msg);
    }
    wglMakeCurrent(NULL, NULL);
    wglDeleteContext(rc_gl);
    return 0;
}
