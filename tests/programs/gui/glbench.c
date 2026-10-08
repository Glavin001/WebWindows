/* OpenGL benchmark, the OpenGL 1.1 counterpart of d3d9bench: a grid of
 * spinning textured cubes, each drawn with its own matrix and glBegin/glEnd
 * (one draw per cube, as a game of the era draws its objects), and a cloud
 * of particles drawn as one blended batch of quads in a 2D projection. It
 * renders as fast as it can and prints, once a second, the frame rate and
 * where each frame's time went: "scene" is the program's GL calls from
 * glClear to the last one before the swap, "present" is SwapBuffers.
 *
 *   glbench [cubes [particles [seconds [materials [width height]]]]]
 *
 * Defaults: 400 cubes, 2000 particles, run until the window is closed
 * (seconds 0), 1 material, a 640x480 client area. With seconds, it stops
 * after that long and prints a summary line. With more than one material,
 * consecutive cubes use different ones: each draw then also changes the
 * texture, the magnification filter and (every other material) blending.
 */
#include <windows.h>
#include <GL/gl.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>

static volatile int quit;

static LRESULT CALLBACK proc(HWND hwnd, UINT msg, WPARAM wp, LPARAM lp)
{
    if (msg == WM_DESTROY)
    {
        quit = 1;
        PostQuitMessage(0);
        return 0;
    }
    return DefWindowProcA(hwnd, msg, wp, lp);
}

static double now_ms(void)
{
    static LARGE_INTEGER freq;
    LARGE_INTEGER t;
    if (!freq.QuadPart) QueryPerformanceFrequency(&freq);
    QueryPerformanceCounter(&t);
    return (double)t.QuadPart * 1000.0 / (double)freq.QuadPart;
}

/* A cube with a colour per face: 6 quads, 24 vertices. */
static float cube_pos[24][3], cube_uv[24][2];
static GLubyte cube_rgb[6][3] = {{255,128,128}, {128,255,128}, {128,128,255}, {255,255,128}, {255,128,255}, {128,255,255}};

static void make_cube(void)
{
    static const float n[6][3] = {{0,0,-1}, {0,0,1}, {-1,0,0}, {1,0,0}, {0,1,0}, {0,-1,0}};
    /* Corners in quad order. */
    static const float st[4][2] = {{-1,-1}, {1,-1}, {1,1}, {-1,1}};
    int f, k;

    for (f = 0; f < 6; ++f)
    {
        float a[3] = {n[f][1], n[f][2], n[f][0]}, b[3];
        b[0] = n[f][1] * a[2] - n[f][2] * a[1];
        b[1] = n[f][2] * a[0] - n[f][0] * a[2];
        b[2] = n[f][0] * a[1] - n[f][1] * a[0];
        for (k = 0; k < 4; ++k)
        {
            float s = st[k][0], t = st[k][1];
            cube_pos[f * 4 + k][0] = 0.7f * (n[f][0] + s * a[0] + t * b[0]);
            cube_pos[f * 4 + k][1] = 0.7f * (n[f][1] + s * a[1] + t * b[1]);
            cube_pos[f * 4 + k][2] = 0.7f * (n[f][2] + s * a[2] + t * b[2]);
            cube_uv[f * 4 + k][0] = (s + 1) * 0.5f;
            cube_uv[f * 4 + k][1] = (t + 1) * 0.5f;
        }
    }
}

int main(int argc, char **argv)
{
    int cubes = argc > 1 ? atoi(argv[1]) : 400;
    int particles = argc > 2 ? atoi(argv[2]) : 2000;
    double seconds = argc > 3 ? atof(argv[3]) : 0.0;
    int materials = argc > 4 ? atoi(argv[4]) : 1;
    int width = argc > 6 ? atoi(argv[5]) : 640;
    int height = argc > 6 ? atoi(argv[6]) : 480;
    PIXELFORMATDESCRIPTOR pfd = {sizeof(pfd), 1, PFD_DRAW_TO_WINDOW | PFD_SUPPORT_OPENGL | PFD_DOUBLEBUFFER, PFD_TYPE_RGBA, 32};
    static GLubyte pixels[64 * 64 * 4];
    GLuint tex[16];
    WNDCLASSA wc = {0};
    RECT rc = {0, 0, width, height};
    HGLRC glrc;
    HWND hwnd;
    HDC dc;
    MSG msg;
    int i, x, y, grid, rows;
    double start, last, scene_ms = 0, present_ms = 0, worst_ms = 0;
    double total_scene = 0, total_present = 0;
    long frames = 0, total_frames = 0;
    float dist;

    if (cubes < 0) cubes = 0;
    if (materials < 1) materials = 1;
    if (materials > 16) materials = 16;
    if (particles < 0) particles = 0;
    if (width < 64 || height < 64) width = 640, height = 480;
    printf("glbench: %d cubes, %d particles, %d materials, %dx%d\n", cubes, particles, materials, width, height);

    wc.lpfnWndProc = proc;
    wc.hInstance = GetModuleHandleA(NULL);
    wc.hCursor = LoadCursorA(NULL, (LPCSTR)IDC_ARROW);
    wc.lpszClassName = "glbench";
    RegisterClassA(&wc);
    rc.right = width;
    rc.bottom = height;
    AdjustWindowRect(&rc, WS_OVERLAPPEDWINDOW, FALSE);
    hwnd = CreateWindowA("glbench", "OpenGL benchmark", WS_OVERLAPPEDWINDOW, 10, 10,
            rc.right - rc.left, rc.bottom - rc.top, NULL, NULL, wc.hInstance, NULL);
    ShowWindow(hwnd, SW_SHOW);
    UpdateWindow(hwnd);
    dc = GetDC(hwnd);

    pfd.cDepthBits = 24;
    if (!SetPixelFormat(dc, ChoosePixelFormat(dc, &pfd), &pfd))
    {
        printf("SetPixelFormat failed\n");
        return 1;
    }
    if (!(glrc = wglCreateContext(dc)) || !wglMakeCurrent(dc, glrc))
    {
        printf("wglCreateContext failed\n");
        return 1;
    }
    printf("renderer: %s (OpenGL %s)\n", (const char *)glGetString(GL_RENDERER), (const char *)glGetString(GL_VERSION));

    /* Textures: checkerboards in different colours, with some alpha. */
    glGenTextures(materials, tex);
    for (i = 0; i < materials; ++i)
    {
        for (y = 0; y < 64; ++y)
            for (x = 0; x < 64; ++x)
            {
                GLubyte *p = &pixels[(y * 64 + x) * 4];
                int on = ((x >> 3) ^ (y >> 3)) & 1;
                p[0] = on ? 255 : 64 + i * 12;
                p[1] = on ? 255 : 96 + i * 8;
                p[2] = on ? 255 : 160;
                p[3] = on ? 255 : 160;
            }
        glBindTexture(GL_TEXTURE_2D, tex[i]);
        glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, 64, 64, 0, GL_RGBA, GL_UNSIGNED_BYTE, pixels);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
    }
    make_cube();

    grid = 1;
    while (grid * grid < cubes) ++grid;
    rows = cubes ? (cubes + grid - 1) / grid : 1;
    /* Far enough back for the whole grid in a 60 degree field of view. */
    dist = grid * 2.2f * 0.5f / tanf(3.14159265f / 6.0f) + 2.0f;
    printf("draws per frame: %d\n", cubes + (particles ? 1 : 0));

    start = last = now_ms();
    while (!quit)
    {
        double t0, t1, t2, t;
        float fh = 0.5f * tanf(3.14159265f / 6.0f), aspect = (float)width / (float)height;

        while (PeekMessageA(&msg, NULL, 0, 0, PM_REMOVE))
        {
            TranslateMessage(&msg);
            DispatchMessageA(&msg);
        }
        if (quit) break;

        t0 = now_ms();
        t = (t0 - start) / 1000.0;
        glViewport(0, 0, width, height);
        glClearColor(16 / 255.0f, 24 / 255.0f, 48 / 255.0f, 1);
        glClear(GL_COLOR_BUFFER_BIT | GL_DEPTH_BUFFER_BIT);

        /* Cubes: perspective, depth, a texture; per cube a matrix and tint. */
        glMatrixMode(GL_PROJECTION);
        glLoadIdentity();
        glFrustum(-fh * aspect, fh * aspect, -fh, fh, 0.5, dist * 2.0 + 4.0);
        glMatrixMode(GL_MODELVIEW);
        glLoadIdentity();
        glEnable(GL_DEPTH_TEST);
        glDepthFunc(GL_LESS);
        glDisable(GL_BLEND);
        glDisable(GL_CULL_FACE);
        glEnable(GL_TEXTURE_2D);
        glTexEnvi(GL_TEXTURE_ENV, GL_TEXTURE_ENV_MODE, GL_MODULATE);
        glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
        glBindTexture(GL_TEXTURE_2D, tex[0]);
        for (i = 0; i < cubes; ++i)
        {
            float tint[3] = {0.6f + 0.4f * sinf(i * 0.5f), 0.6f + 0.4f * sinf(i * 0.7f + 2.0f), 0.6f + 0.4f * sinf(i * 0.9f + 4.0f)};
            int f, k;
            if (materials > 1)
            {
                int m = i % materials;
                glBindTexture(GL_TEXTURE_2D, tex[m]);
                glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, m & 2 ? GL_NEAREST : GL_LINEAR);
                if (m & 1) glEnable(GL_BLEND);
                else glDisable(GL_BLEND);
            }
            glPushMatrix();
            glTranslatef(((i % grid) - (grid - 1) * 0.5f) * 2.2f, ((i / grid) - (rows - 1) * 0.5f) * 2.2f, -dist);
            glRotatef((float)(t * 1.3 + i * 0.37) * 57.29578f, 0, 1, 0);
            glRotatef((float)(t * 0.7 + i * 0.21) * 57.29578f, 1, 0, 0);
            glBegin(GL_QUADS);
            for (f = 0; f < 6; ++f)
            {
                glColor3f(cube_rgb[f][0] / 255.0f * tint[0], cube_rgb[f][1] / 255.0f * tint[1], cube_rgb[f][2] / 255.0f * tint[2]);
                for (k = 0; k < 4; ++k)
                {
                    glTexCoord2fv(cube_uv[f * 4 + k]);
                    glVertex3fv(cube_pos[f * 4 + k]);
                }
            }
            glEnd();
            glPopMatrix();
        }

        /* Particles: 2D, untextured, blended, one batch. */
        if (particles)
        {
            glMatrixMode(GL_PROJECTION);
            glLoadIdentity();
            glOrtho(0, width, height, 0, -1, 1);
            glMatrixMode(GL_MODELVIEW);
            glLoadIdentity();
            glDisable(GL_DEPTH_TEST);
            glDisable(GL_TEXTURE_2D);
            glEnable(GL_BLEND);
            glBegin(GL_QUADS);
            for (i = 0; i < particles; ++i)
            {
                float a = (float)t * (0.3f + (i % 7) * 0.05f) + i * 2.399f;
                float rad = (0.1f + 0.4f * (float)((i * 7919) % 1000) / 1000.0f) * height;
                float px = width * 0.5f + cosf(a) * rad, py = height * 0.5f + sinf(a) * rad * 0.8f;
                glColor4ub((GLubyte)(128 + (i * 37) % 128), (GLubyte)(128 + (i * 61) % 128), 255, 160);
                glVertex2f(px - 2, py - 2);
                glVertex2f(px + 2, py - 2);
                glVertex2f(px + 2, py + 2);
                glVertex2f(px - 2, py + 2);
            }
            glEnd();
        }

        glFlush();
        t1 = now_ms();
        SwapBuffers(dc);
        t2 = now_ms();

        scene_ms += t1 - t0;
        present_ms += t2 - t1;
        if (t2 - t0 > worst_ms) worst_ms = t2 - t0;
        ++frames;
        if (t2 - last >= 1000.0)
        {
            char title[128];
            double fps = frames * 1000.0 / (t2 - last);
            printf("%.1f fps: frame %.2f ms (scene %.2f ms, %.1f us/draw; present %.2f ms), worst %.1f ms\n",
                    fps, (t2 - last) / frames, scene_ms / frames,
                    cubes ? scene_ms / frames * 1000.0 / (cubes + (particles ? 1 : 0)) : 0.0,
                    present_ms / frames, worst_ms);
            fflush(stdout);
            sprintf(title, "OpenGL benchmark - %.1f fps", fps);
            SetWindowTextA(hwnd, title);
            total_frames += frames;
            total_scene += scene_ms;
            total_present += present_ms;
            frames = 0;
            scene_ms = present_ms = worst_ms = 0;
            last = t2;
        }
        if (seconds > 0 && t2 - start >= seconds * 1000.0)
            break;
    }

    total_frames += frames;
    total_scene += scene_ms;
    total_present += present_ms;
    if (total_frames)
        printf("summary: %ld frames in %.1f s: %.1f fps, scene %.2f ms, present %.2f ms\n",
                total_frames, (now_ms() - start) / 1000.0, total_frames * 1000.0 / (now_ms() - start),
                total_scene / total_frames, total_present / total_frames);
    fflush(stdout);

    glDeleteTextures(materials, tex);
    wglMakeCurrent(NULL, NULL);
    wglDeleteContext(glrc);
    ReleaseDC(hwnd, dc);
    return 0;
}
