/* DirectDraw the way 2D games of the era use it, windowed: a primary
 * surface with a clipper on a window, offscreen surfaces filled by Lock and
 * by color fills, a color-keyed Blt, GetDC on a surface, and reading the
 * result back. Wine's ddraw runs on wined3d with 3D off (no OpenGL or
 * Direct3D underneath), presenting through GDI.
 *
 * libs: -lddraw -ldxguid -lgdi32 -luser32
 */
#include <windows.h>
#include <ddraw.h>
#include <stdio.h>

static IDirectDraw7 *dd;

static IDirectDrawSurface7 *offscreen(int w, int h)
{
    DDSURFACEDESC2 desc = { sizeof(desc) };
    IDirectDrawSurface7 *s = NULL;
    HRESULT hr;

    desc.dwFlags = DDSD_CAPS | DDSD_WIDTH | DDSD_HEIGHT;
    desc.ddsCaps.dwCaps = DDSCAPS_OFFSCREENPLAIN | DDSCAPS_SYSTEMMEMORY;
    desc.dwWidth = w;
    desc.dwHeight = h;
    hr = IDirectDraw7_CreateSurface(dd, &desc, &s, NULL);
    if (FAILED(hr)) printf("CreateSurface %dx%d: %08lx\n", w, h, hr);
    return s;
}

/* The pixel at (x, y) as 0x00RRGGBB, read with Lock. */
static DWORD pixel(IDirectDrawSurface7 *s, int x, int y)
{
    DDSURFACEDESC2 desc = { sizeof(desc) };
    DWORD v = 0xdeadbeef;
    if (SUCCEEDED(IDirectDrawSurface7_Lock(s, NULL, &desc, DDLOCK_WAIT | DDLOCK_READONLY, NULL)))
    {
        BYTE *p = (BYTE *)desc.lpSurface + y * desc.lPitch;
        switch (desc.ddpfPixelFormat.dwRGBBitCount)
        {
        case 32: v = ((DWORD *)p)[x] & 0xffffff; break;
        case 16: { WORD c = ((WORD *)p)[x];
                   v = ((c >> 11) & 31) * 255 / 31 << 16 | ((c >> 5) & 63) * 255 / 63 << 8 | (c & 31) * 255 / 31; break; }
        }
        IDirectDrawSurface7_Unlock(s, NULL);
    }
    return v;
}

static void fill(IDirectDrawSurface7 *s, RECT *r, DWORD color)
{
    DDBLTFX fx = { sizeof(fx) };
    fx.dwFillColor = color;
    IDirectDrawSurface7_Blt(s, r, NULL, NULL, DDBLT_COLORFILL | DDBLT_WAIT, &fx);
}

int main(void)
{
    WNDCLASSA wc = { 0, DefWindowProcA, 0, 0, NULL, NULL, NULL, (HBRUSH)(COLOR_WINDOW + 1), NULL, "ddraw-test" };
    DDSURFACEDESC2 desc = { sizeof(desc) };
    IDirectDrawSurface7 *primary, *back, *sprite;
    IDirectDrawClipper *clipper;
    DDCOLORKEY key = { 0, 0 };
    DDPIXELFORMAT pf = { sizeof(pf) };
    RECT r = { 0, 0, 64, 64 }, all = { 0, 0, 160, 120 };
    HWND hwnd;
    HDC dc;
    HRESULT hr;
    DWORD red, green, blue, black;

    setvbuf(stdout, NULL, _IONBF, 0);
    wc.hInstance = GetModuleHandleA(NULL);
    RegisterClassA(&wc);
    hwnd = CreateWindowA("ddraw-test", "ddraw", WS_OVERLAPPEDWINDOW | WS_VISIBLE, 0, 0, 200, 180, NULL, NULL,
                         wc.hInstance, NULL);

    hr = DirectDrawCreateEx(NULL, (void **)&dd, &IID_IDirectDraw7, NULL);
    printf("DirectDrawCreateEx: %08lx\n", hr);
    if (FAILED(hr)) return 1;
    printf("SetCooperativeLevel: %08lx\n", IDirectDraw7_SetCooperativeLevel(dd, hwnd, DDSCL_NORMAL));

    desc.dwFlags = DDSD_CAPS;
    desc.ddsCaps.dwCaps = DDSCAPS_PRIMARYSURFACE;
    hr = IDirectDraw7_CreateSurface(dd, &desc, &primary, NULL);
    printf("primary surface: %08lx\n", hr);
    if (FAILED(hr)) return 1;
    printf("CreateClipper: %08lx\n", IDirectDraw7_CreateClipper(dd, 0, &clipper, NULL));
    IDirectDrawClipper_SetHWnd(clipper, 0, hwnd);
    printf("SetClipper: %08lx\n", IDirectDrawSurface7_SetClipper(primary, clipper));
    IDirectDrawSurface7_GetPixelFormat(primary, &pf);
    printf("primary is RGB: %d\n", !!(pf.dwFlags & DDPF_RGB));

    back = offscreen(160, 120);
    sprite = offscreen(64, 64);
    if (!back || !sprite) return 1;
    IDirectDrawSurface7_GetPixelFormat(back, &pf);
    red = pf.dwRBitMask;
    green = pf.dwGBitMask;
    blue = pf.dwBBitMask;
    black = 0;

    /* The background blue, the sprite red with a black (color-keyed) square. */
    fill(back, &all, blue);
    fill(sprite, NULL, red);
    {
        RECT hole = { 16, 16, 48, 48 };
        fill(sprite, &hole, black);
    }
    IDirectDrawSurface7_SetColorKey(sprite, DDCKEY_SRCBLT, &key);
    {
        RECT dst = { 40, 20, 104, 84 };
        hr = IDirectDrawSurface7_Blt(back, &dst, sprite, &r, DDBLT_KEYSRC | DDBLT_WAIT, NULL);
        printf("color-keyed Blt: %08lx\n", hr);
    }
    printf("background: %06lx, sprite: %06lx, through the hole: %06lx\n",
           pixel(back, 5, 5), pixel(back, 42, 22), pixel(back, 72, 52));

    /* GDI on a surface. */
    if (SUCCEEDED(IDirectDrawSurface7_GetDC(back, &dc)))
    {
        RECT g = { 120, 90, 150, 110 };
        HBRUSH b = CreateSolidBrush(RGB(0, 255, 0));
        FillRect(dc, &g, b);
        DeleteObject(b);
        IDirectDrawSurface7_ReleaseDC(back, dc);
        printf("GetDC fill: %06lx\n", pixel(back, 130, 100));
    }
    (void)green;

    /* To the window, through the clipper. */
    {
        RECT client;
        POINT o = { 0, 0 };
        GetClientRect(hwnd, &client);
        ClientToScreen(hwnd, &o);
        OffsetRect(&client, o.x, o.y);
        hr = IDirectDrawSurface7_Blt(primary, &client, back, NULL, DDBLT_WAIT, NULL);
        printf("Blt to the primary surface: %08lx\n", hr);
    }
    {
        /* The window shows it: read it back with GDI. */
        HDC wdc = GetDC(hwnd);
        COLORREF c = GetPixel(wdc, 2, 2);
        printf("window pixel is blue: %d\n", GetRValue(c) < 64 && GetGValue(c) < 64 && GetBValue(c) > 192);
        ReleaseDC(hwnd, wdc);
    }

    IDirectDrawSurface7_Release(sprite);
    IDirectDrawSurface7_Release(back);
    IDirectDrawClipper_Release(clipper);
    IDirectDrawSurface7_Release(primary);
    IDirectDraw7_Release(dd);
    DestroyWindow(hwnd);
    printf("done\n");
    return 0;
}
