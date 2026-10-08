/* A tiny DirectDraw and DirectSound program for the browser test
 * (tests/web/gui.mjs): a red square moving across a blue window at about
 * 60 frames a second, drawn on an offscreen surface and blitted to the
 * primary surface through a clipper, while a DirectSound buffer loops a
 * tone. It runs until its window is closed.
 *
 *   i686-w64-mingw32-gcc -O2 -o ddsound.exe ddsound.c -lddraw -ldsound -ldxguid -lgdi32 -luser32 -lwinmm
 */
#include <windows.h>
#include <ddraw.h>
#include <dsound.h>
#include <math.h>

#define W 320
#define H 240

static LRESULT CALLBACK proc(HWND hwnd, UINT msg, WPARAM wp, LPARAM lp)
{
    if (msg == WM_DESTROY) PostQuitMessage(0);
    return DefWindowProcA(hwnd, msg, wp, lp);
}

static void fill(IDirectDrawSurface7 *s, RECT *r, DWORD color)
{
    DDBLTFX fx = { sizeof(fx) };
    fx.dwFillColor = color;
    IDirectDrawSurface7_Blt(s, r, NULL, NULL, DDBLT_COLORFILL | DDBLT_WAIT, &fx);
}

static void tone(HWND hwnd)
{
    WAVEFORMATEX wf = { WAVE_FORMAT_PCM, 1, 22050, 44100, 2, 16, 0 };
    DSBUFFERDESC desc = { sizeof(desc) };
    IDirectSound *ds;
    IDirectSoundBuffer *buf;
    void *p1, *p2;
    DWORD n1, n2, i;

    if (FAILED(DirectSoundCreate(NULL, &ds, NULL))) return;
    IDirectSound_SetCooperativeLevel(ds, hwnd, DSSCL_NORMAL);
    desc.dwBufferBytes = 22050 * 2; /* one second */
    desc.lpwfxFormat = &wf;
    if (FAILED(IDirectSound_CreateSoundBuffer(ds, &desc, &buf, NULL))) return;
    if (SUCCEEDED(IDirectSoundBuffer_Lock(buf, 0, 0, &p1, &n1, &p2, &n2, DSBLOCK_ENTIREBUFFER)))
    {
        for (i = 0; i < n1 / 2; i++) ((short *)p1)[i] = (short)(6000 * sin(i * 2 * 3.14159265 * 440 / 22050));
        IDirectSoundBuffer_Unlock(buf, p1, n1, p2, n2);
    }
    IDirectSoundBuffer_Play(buf, 0, 0, DSBPLAY_LOOPING);
}

int WINAPI WinMain(HINSTANCE inst, HINSTANCE prev, LPSTR cmd, int show)
{
    WNDCLASSA wc = { 0, proc, 0, 0, inst, NULL, NULL, NULL, NULL, "ddsound" };
    DDSURFACEDESC2 desc = { sizeof(desc) };
    IDirectDraw7 *dd;
    IDirectDrawSurface7 *primary, *back;
    IDirectDrawClipper *clipper;
    DDPIXELFORMAT pf = { sizeof(pf) };
    RECT rc = { 0, 0, W, H };
    HWND hwnd;
    MSG msg;
    int x = 0;

    wc.hCursor = LoadCursorA(NULL, (LPCSTR)IDC_ARROW);
    RegisterClassA(&wc);
    AdjustWindowRect(&rc, WS_OVERLAPPEDWINDOW, FALSE);
    hwnd = CreateWindowA("ddsound", "DirectDraw and DirectSound", WS_OVERLAPPEDWINDOW | WS_VISIBLE, 20, 20,
                         rc.right - rc.left, rc.bottom - rc.top, NULL, NULL, inst, NULL);

    if (FAILED(DirectDrawCreateEx(NULL, (void **)&dd, &IID_IDirectDraw7, NULL))) return 1;
    IDirectDraw7_SetCooperativeLevel(dd, hwnd, DDSCL_NORMAL);
    desc.dwFlags = DDSD_CAPS;
    desc.ddsCaps.dwCaps = DDSCAPS_PRIMARYSURFACE;
    if (FAILED(IDirectDraw7_CreateSurface(dd, &desc, &primary, NULL))) return 1;
    IDirectDraw7_CreateClipper(dd, 0, &clipper, NULL);
    IDirectDrawClipper_SetHWnd(clipper, 0, hwnd);
    IDirectDrawSurface7_SetClipper(primary, clipper);
    desc.dwFlags = DDSD_CAPS | DDSD_WIDTH | DDSD_HEIGHT;
    desc.ddsCaps.dwCaps = DDSCAPS_OFFSCREENPLAIN | DDSCAPS_SYSTEMMEMORY;
    desc.dwWidth = W;
    desc.dwHeight = H;
    if (FAILED(IDirectDraw7_CreateSurface(dd, &desc, &back, NULL))) return 1;
    IDirectDrawSurface7_GetPixelFormat(back, &pf);

    tone(hwnd);
    for (;;)
    {
        RECT all = { 0, 0, W, H }, square, client;
        POINT o = { 0, 0 };

        while (PeekMessageA(&msg, NULL, 0, 0, PM_REMOVE))
        {
            if (msg.message == WM_QUIT) return 0;
            TranslateMessage(&msg);
            DispatchMessageA(&msg);
        }
        fill(back, &all, pf.dwBBitMask);
        SetRect(&square, x, 80, x + 80, 160);
        fill(back, &square, pf.dwRBitMask);
        x = (x + 4) % (W - 80);
        GetClientRect(hwnd, &client);
        ClientToScreen(hwnd, &o);
        OffsetRect(&client, o.x, o.y);
        IDirectDrawSurface7_Blt(primary, &client, back, NULL, DDBLT_WAIT, NULL);
        Sleep(16);
    }
}
