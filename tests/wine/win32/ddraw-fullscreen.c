/* DirectDraw the way fullscreen games of the era use it: exclusive mode,
 * a 640x480 display mode in 16 and 32 bits, a primary surface with a back
 * buffer flipped each frame, pixels written through Lock, and the desktop's
 * mode back afterwards. (On Wine's no3d renderer, a 16-bit back buffer
 * written through Lock and flipped still shows its previous contents, so
 * that is checked in 32 bits only; see docs/milestone-5.md.)
 *
 * libs: -lddraw -ldxguid -lgdi32 -luser32
 */
#include <windows.h>
#include <ddraw.h>
#include <stdio.h>

static IDirectDraw7 *dd;
static HWND hwnd;

static void mode(const char *what)
{
    DDSURFACEDESC2 desc = { sizeof(desc) };
    IDirectDraw7_GetDisplayMode(dd, &desc);
    printf("%s: %lux%lu, %lu bits\n", what, desc.dwWidth, desc.dwHeight, desc.ddpfPixelFormat.dwRGBBitCount);
}

static HRESULT CALLBACK count_mode(DDSURFACEDESC2 *desc, void *ctx)
{
    if (desc->dwWidth == 640 && desc->dwHeight == 480) ++*(int *)ctx;
    return DDENUMRET_OK;
}

static int frames(int bits, int check_lock)
{
    DDSURFACEDESC2 desc = { sizeof(desc) };
    DDSCAPS2 caps = { DDSCAPS_BACKBUFFER };
    IDirectDrawSurface7 *primary, *back;
    DDBLTFX fx = { sizeof(fx) };
    HRESULT hr;
    int i, ok = 0;

    hr = IDirectDraw7_SetDisplayMode(dd, 640, 480, bits, 0, 0);
    printf("SetDisplayMode 640x480x%d: %08lx\n", bits, hr);
    if (FAILED(hr)) return 0;
    mode("display mode");
    {
        RECT r;
        GetWindowRect(hwnd, &r);
        printf("screen: %dx%d, window: %ldx%ld\n", GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN),
               r.right - r.left, r.bottom - r.top);
    }

    desc.dwFlags = DDSD_CAPS | DDSD_BACKBUFFERCOUNT;
    desc.ddsCaps.dwCaps = DDSCAPS_PRIMARYSURFACE | DDSCAPS_FLIP | DDSCAPS_COMPLEX;
    desc.dwBackBufferCount = 1;
    hr = IDirectDraw7_CreateSurface(dd, &desc, &primary, NULL);
    printf("flipping primary surface: %08lx\n", hr);
    if (FAILED(hr)) return 0;
    hr = IDirectDrawSurface7_GetAttachedSurface(primary, &caps, &back);
    printf("back buffer: %08lx\n", hr);

    for (i = 0; i < 10; i++)
    {
        fx.dwFillColor = i & 1 ? 0 : 0xffffffff;
        IDirectDrawSurface7_Blt(back, NULL, NULL, NULL, DDBLT_COLORFILL | DDBLT_WAIT, &fx);
        if (SUCCEEDED(IDirectDrawSurface7_Flip(primary, NULL, DDFLIP_WAIT))) ok++;
    }
    printf("flips: %d\n", ok);

    /* Pixels through Lock, then shown. */
    if (check_lock)
    {
        DDSURFACEDESC2 l = { sizeof(l) };
        if (SUCCEEDED(IDirectDrawSurface7_Lock(back, NULL, &l, DDLOCK_WAIT | DDLOCK_WRITEONLY, NULL)))
        {
            int x, y, bpp = l.ddpfPixelFormat.dwRGBBitCount / 8;
            for (y = 0; y < 480; y++)
                for (x = 0; x < 640; x++)
                {
                    BYTE *p = (BYTE *)l.lpSurface + y * l.lPitch + x * bpp;
                    DWORD c = x < 320 ? l.ddpfPixelFormat.dwRBitMask : l.ddpfPixelFormat.dwBBitMask;
                    memcpy(p, &c, bpp);
                }
            IDirectDrawSurface7_Unlock(back, NULL);
            printf("Lock: %d bits, pitch at least the width: %d\n", bpp * 8, l.lPitch >= 640 * bpp);
        }
        printf("Flip: %08lx\n", IDirectDrawSurface7_Flip(primary, NULL, DDFLIP_WAIT));
    }
    if (check_lock)
    {
        /* What the window shows now: red left, blue right. */
        HDC dc = GetDC(hwnd);
        COLORREF l = GetPixel(dc, 100, 100), r = GetPixel(dc, 500, 100);
        int ok = GetRValue(l) > 192 && GetBValue(l) < 64 && GetBValue(r) > 192 && GetRValue(r) < 64;
        printf("on the screen: %d\n", ok);
        if (!ok) fprintf(stderr, "left %06lx right %06lx\n", l, r);
        ReleaseDC(hwnd, dc);
    }
    IDirectDrawSurface7_Release(back);
    IDirectDrawSurface7_Release(primary);
    return 1;
}

int main(void)
{
    WNDCLASSA wc = { 0, DefWindowProcA, 0, 0, NULL, NULL, NULL, NULL, NULL, "ddraw-fullscreen" };
    HRESULT hr;
    int n = 0;

    setvbuf(stdout, NULL, _IONBF, 0);
    wc.hInstance = GetModuleHandleA(NULL);
    RegisterClassA(&wc);
    hwnd = CreateWindowExA(WS_EX_TOPMOST, "ddraw-fullscreen", "ddraw", WS_POPUP | WS_VISIBLE, 0, 0, GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN), NULL,
                           NULL, wc.hInstance, NULL);

    hr = DirectDrawCreateEx(NULL, (void **)&dd, &IID_IDirectDraw7, NULL);
    printf("DirectDrawCreateEx: %08lx\n", hr);
    if (FAILED(hr)) return 1;
    IDirectDraw7_EnumDisplayModes(dd, 0, NULL, &n, count_mode);
    printf("640x480 modes: %d\n", n > 0);
    hr = IDirectDraw7_SetCooperativeLevel(dd, hwnd, DDSCL_EXCLUSIVE | DDSCL_FULLSCREEN);
    printf("SetCooperativeLevel exclusive: %08lx\n", hr);

    frames(16, 0);
    frames(32, 1);

    printf("RestoreDisplayMode: %08lx\n", IDirectDraw7_RestoreDisplayMode(dd));
    IDirectDraw7_SetCooperativeLevel(dd, hwnd, DDSCL_NORMAL);
    {
        DEVMODEA dm = { .dmSize = sizeof(dm) };
        EnumDisplaySettingsA(NULL, ENUM_CURRENT_SETTINGS, &dm);
        printf("desktop back: %d\n", dm.dmPelsWidth != 640 || dm.dmPelsHeight != 480 || dm.dmBitsPerPel >= 24);
    }
    IDirectDraw7_Release(dd);
    DestroyWindow(hwnd);
    printf("done\n");
    return 0;
}
