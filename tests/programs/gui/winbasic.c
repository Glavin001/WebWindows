/* GUI basics on translated Wine (M4): creates, shows and paints a window,
 * printing what user32 reports at each step. */
#include <windows.h>
#include <stdio.h>

static int paints;

static LRESULT CALLBACK proc(HWND hwnd, UINT msg, WPARAM wp, LPARAM lp)
{
    if (msg == WM_PAINT)
    {
        PAINTSTRUCT ps;
        RECT rc;
        HDC dc = BeginPaint(hwnd, &ps);
        GetClientRect(hwnd, &rc);
        FillRect(dc, &rc, (HBRUSH)GetStockObject(WHITE_BRUSH));
        {
            SIZE sz;
            TEXTMETRICA tm;
            char face[64];
            BOOL ok = TextOutA(dc, 10, 10, "Hello, browser", 14);
            GetTextExtentPoint32A(dc, "Hello, browser", 14, &sz);
            GetTextMetricsA(dc, &tm);
            GetTextFaceA(dc, sizeof(face), face);
            printf("TextOut %d, extent %ldx%ld, height %ld, face %s\n", ok, sz.cx, sz.cy, tm.tmHeight, face);
            {
                HFONT f = CreateFontA(-16, 0, 0, 0, FW_NORMAL, 0, 0, 0, ANSI_CHARSET, 0, 0, 0, 0, "Tahoma");
                HFONT old = SelectObject(dc, f);
                BOOL ok2 = TextOutA(dc, 10, 40, "Tahoma text", 11);
                BOOL ok3 = GetTextExtentPoint32A(dc, "Tahoma text", 11, &sz);
                BOOL ok4 = GetTextMetricsA(dc, &tm);
                GetTextFaceA(dc, sizeof(face), face);
                printf("Tahoma: TextOut %d, extent %d %ldx%ld, metrics %d height %ld, face %s\n", ok2, ok3, sz.cx,
                       sz.cy, ok4, tm.tmHeight, face);
                SelectObject(dc, old);
                DeleteObject(f);
            }
        }
        {
            /* Colour checks: solid brushes, then a 4-bit DIB with a palette. */
            static const COLORREF colors[] = { RGB(255,0,0), RGB(0,255,0), RGB(0,0,255), RGB(128,128,128) };
            struct { BITMAPINFOHEADER h; RGBQUAD pal[16]; } bi = {{ sizeof(BITMAPINFOHEADER), 4, -1, 1, 4, BI_RGB }};
            BYTE bits[4] = { 0x01, 0x23, 0, 0 };
            int i;
            for (i = 0; i < 4; i++)
            {
                RECT c = { 10 + i * 30, 120, 35 + i * 30, 145 };
                HBRUSH b = CreateSolidBrush(colors[i]);
                FillRect(dc, &c, b);
                DeleteObject(b);
            }
            bi.pal[0] = (RGBQUAD){ 0, 0, 255, 0 };     /* red */
            bi.pal[1] = (RGBQUAD){ 0, 255, 0, 0 };     /* green */
            bi.pal[2] = (RGBQUAD){ 255, 0, 0, 0 };     /* blue */
            bi.pal[3] = (RGBQUAD){ 128, 128, 128, 0 }; /* grey */
            StretchDIBits(dc, 10, 150, 120, 20, 0, 0, 4, 1, bits, (BITMAPINFO *)&bi, DIB_RGB_COLORS, SRCCOPY);
        }
        EndPaint(hwnd, &ps);
        paints++;
        printf("WM_PAINT %d: paint rect %ld,%ld-%ld,%ld\n", paints, ps.rcPaint.left, ps.rcPaint.top,
               ps.rcPaint.right, ps.rcPaint.bottom);
        return 0;
    }
    if (msg == WM_DESTROY) PostQuitMessage(0);
    return DefWindowProcA(hwnd, msg, wp, lp);
}

int main(void)
{
    WNDCLASSA wc = {0};
    RECT r;
    HWND hwnd;
    MSG msg;

    SetRect(&r, -1, -1, -1, -1);
    SetLastError(0);
    {
        BOOL ok = GetWindowRect(GetDesktopWindow(), &r);
        printf("desktop %p rect %ld,%ld-%ld,%ld (ret %d, error %lu)\n", GetDesktopWindow(), r.left, r.top,
               r.right, r.bottom, ok, GetLastError());
    }
    printf("screen %dx%d\n", GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));

    wc.lpfnWndProc = proc;
    wc.hInstance = GetModuleHandleA(NULL);
    wc.hCursor = LoadCursorA(NULL, (LPCSTR)IDC_ARROW);
    wc.hbrBackground = (HBRUSH)(COLOR_WINDOW + 1);
    wc.lpszClassName = "WinBasic";
    printf("RegisterClass %d\n", RegisterClassA(&wc) != 0);

    hwnd = CreateWindowA("WinBasic", "WinBasic", WS_OVERLAPPEDWINDOW, 40, 30, 320, 200, NULL, NULL,
                         wc.hInstance, NULL);
    printf("CreateWindow %p\n", hwnd);
    GetWindowRect(hwnd, &r);
    printf("window rect %ld,%ld-%ld,%ld\n", r.left, r.top, r.right, r.bottom);
    GetClientRect(hwnd, &r);
    printf("client rect %ld,%ld-%ld,%ld\n", r.left, r.top, r.right, r.bottom);
    ShowWindow(hwnd, SW_SHOW);
    printf("visible %d, update rect %d\n", IsWindowVisible(hwnd), GetUpdateRect(hwnd, &r, FALSE));
    printf("SM_CXMINTRACK %d SM_CYCAPTION %d SM_CXFRAME %d\n", GetSystemMetrics(SM_CXMINTRACK), GetSystemMetrics(SM_CYCAPTION), GetSystemMetrics(SM_CXFRAME));
    UpdateWindow(hwnd);
    printf("after UpdateWindow: %d paints\n", paints);

    /* A few rounds of the message loop, then quit. */
    SetTimer(hwnd, 1, 100, NULL);
    while (GetMessageA(&msg, NULL, 0, 0))
    {
        if (msg.message == WM_TIMER) { printf("timer: %d paints\n", paints); KillTimer(hwnd, 1); break; }
        DispatchMessageA(&msg);
    }
    printf("done\n");
    return 0;
}
