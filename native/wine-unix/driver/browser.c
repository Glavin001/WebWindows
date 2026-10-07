/*
 * The browser display driver (Milestone 4): win32u's user driver for a
 * page instead of X11, Wayland or macOS.
 *
 * win32u draws every top-level window into a window surface (32-bit,
 * top-down) with its own DIB engine; this driver's flush hands the changed
 * rectangle to the host (runtime/wine/display.mjs), which keeps one image
 * per top-level window and composes them, in stacking order, onto the
 * screen it shows in a canvas. Window placement and stacking come from
 * WindowPosChanged. Keyboard and mouse events queued by the page are sent
 * as hardware input from ProcessEvents, which win32u calls when it looks
 * for messages, and from waits (wasm_process_input, see inproc/client.c).
 */

#include "config.h"

#include <stdarg.h>
#include <stdlib.h>
#include <string.h>

#include "ntstatus.h"
#define WIN32_NO_STATUS
#include "windef.h"
#include "winbase.h"
#include "wingdi.h"
#include "winuser.h"
#include "ntgdi.h"
#include "ntuser.h"
#include "wine/gdi_driver.h"
#include "wine/debug.h"

#include <emscripten.h>

WINE_DEFAULT_DEBUG_CHANNEL(browser);

/* ---- Host interface (runtime/wine/display.mjs) ------------------------- */

EM_JS( void, host_screen_size, (int *width, int *height), {
    const s = Module.display?.size ?? { width: 800, height: 600 };
    HEAP32[ptr(width) / 4] = s.width;
    HEAP32[ptr(height) / 4] = s.height;
});

/* A top-level window's surface changed: `dirty` (surface coordinates) of
 * the `width` x `height` BGRX image at `bits`, whose top-left is at
 * (`left`, `top`) on the screen. */
EM_JS( void, host_surface_flush, (UINT hwnd, int left, int top, int width, int height,
                                  int dl, int dt, int dr, int db, const void *bits, int stride), {
    Module.display?.flush(hwnd >>> 0, left, top, width, height, dl, dt, dr, db, ptr(bits), stride);
});

/* Placement and stacking: `visible` is the window's area on the screen;
 * `after` is the window it now sits below (0: top), or -1 if unchanged. */
EM_JS( void, host_window_pos, (UINT hwnd, int shown, int left, int top, int right, int bottom, int after), {
    Module.display?.windowPos(hwnd >>> 0, shown, left, top, right, bottom, after);
});

EM_JS( void, host_window_destroyed, (UINT hwnd), {
    Module.display?.destroyWindow(hwnd >>> 0);
});

/* Next queued input event: returns 0 when there is none, otherwise fills
 * the INPUT structure (type, then the mouse or keyboard fields). */
EM_JS( int, host_next_input, (INPUT *input), {
    return Module.display?.nextInput(ptr(input)) ?? 0;
});

/* ---- Displays ----------------------------------------------------------- */

static UINT BROWSER_UpdateDisplayDevices( const struct gdi_device_manager *manager, void *param )
{
    struct pci_id pci_id = {0};
    struct gdi_monitor monitor = {0};
    DEVMODEW mode = {.dmSize = sizeof(mode)};
    int width, height;

    host_screen_size( &width, &height );
    manager->add_gpu( "WebWindows", &pci_id, NULL, param );
    manager->add_source( "Browser", DISPLAY_DEVICE_ATTACHED_TO_DESKTOP | DISPLAY_DEVICE_PRIMARY_DEVICE, 96, param );
    SetRect( &monitor.rc_monitor, 0, 0, width, height );
    monitor.rc_work = monitor.rc_monitor;
    manager->add_monitor( &monitor, param );

    mode.dmFields = DM_DISPLAYORIENTATION | DM_BITSPERPEL | DM_PELSWIDTH | DM_PELSHEIGHT |
                    DM_DISPLAYFLAGS | DM_DISPLAYFREQUENCY | DM_POSITION;
    mode.dmBitsPerPel = 32;
    mode.dmPelsWidth = width;
    mode.dmPelsHeight = height;
    mode.dmDisplayFrequency = 60;
    manager->add_modes( &mode, 1, &mode, param );
    return STATUS_SUCCESS;
}

/* ---- Window surfaces ---------------------------------------------------- */

static void browser_surface_set_clip( struct window_surface *surface, const RECT *rects, UINT count )
{
}

static BOOL browser_surface_flush( struct window_surface *surface, const RECT *rect, const RECT *dirty,
                                   const BITMAPINFO *color_info, const void *color_bits, BOOL shape_changed,
                                   const BITMAPINFO *shape_info, const void *shape_bits )
{
    int width = color_info->bmiHeader.biWidth;
    int height = abs( color_info->bmiHeader.biHeight );

    host_surface_flush( HandleToUlong( surface->hwnd ), rect->left, rect->top, width, height,
                        dirty->left, dirty->top, dirty->right, dirty->bottom, color_bits, width * 4 );
    return TRUE;
}

static void browser_surface_destroy( struct window_surface *surface )
{
}

static const struct window_surface_funcs browser_surface_funcs =
{
    browser_surface_set_clip,
    browser_surface_flush,
    browser_surface_destroy,
};

static BOOL BROWSER_CreateWindowSurface( HWND hwnd, BOOL layered, const RECT *surface_rect,
                                         struct window_surface **surface )
{
    char buffer[FIELD_OFFSET( BITMAPINFO, bmiColors[256] )];
    BITMAPINFO *info = (BITMAPINFO *)buffer;
    struct window_surface *previous;
    int width = surface_rect->right - surface_rect->left;
    int height = surface_rect->bottom - surface_rect->top;

    if ((previous = *surface) && previous->funcs == &browser_surface_funcs) return TRUE;
    if (previous) window_surface_release( previous );

    memset( info, 0, sizeof(*info) );
    info->bmiHeader.biSize        = sizeof(info->bmiHeader);
    info->bmiHeader.biWidth       = width;
    info->bmiHeader.biHeight      = -height; /* top-down */
    info->bmiHeader.biPlanes      = 1;
    info->bmiHeader.biBitCount    = 32;
    info->bmiHeader.biSizeImage   = width * height * 4;
    info->bmiHeader.biCompression = BI_RGB;
    *surface = window_surface_create( sizeof(struct window_surface), &browser_surface_funcs, hwnd,
                                      surface_rect, info, 0 );
    return TRUE;
}

/* ---- Windows ------------------------------------------------------------- */

/* wineserver creates the desktop window with an empty rectangle; in Wine the
 * display driver gives it the screen's size (as winex11 does). */
static void BROWSER_SetDesktopWindow( HWND hwnd )
{
    RECT rect = NtUserGetVirtualScreenRect( MDT_RAW_DPI );
    NtUserSetWindowPos( hwnd, 0, rect.left, rect.top, rect.right - rect.left, rect.bottom - rect.top,
                        SWP_NOZORDER | SWP_NOACTIVATE | SWP_DEFERERASE );
}

static BOOL BROWSER_CreateWindow( HWND hwnd )
{
    return TRUE;
}

static void BROWSER_DestroyWindow( HWND hwnd )
{
    host_window_destroyed( HandleToUlong( hwnd ));
}

static void BROWSER_WindowPosChanged( HWND hwnd, HWND insert_after, HWND owner_hint, UINT swp_flags,
                                      const struct window_rects *new_rects, struct window_surface *surface )
{
    HWND parent = NtUserGetAncestor( hwnd, GA_PARENT );
    BOOL shown = (NtUserGetWindowLongW( hwnd, GWL_STYLE ) & WS_VISIBLE) != 0;
    int after = -1;

    /* Child windows draw into their top-level window's surface. */
    if (parent && parent != NtUserGetDesktopWindow() && hwnd != NtUserGetDesktopWindow()) return;
    if (!(swp_flags & SWP_NOZORDER))
        after = (insert_after == HWND_TOP || insert_after == HWND_TOPMOST || insert_after == HWND_NOTOPMOST)
                ? 0 : (insert_after == HWND_BOTTOM ? 1 : HandleToUlong( insert_after ));
    host_window_pos( HandleToUlong( hwnd ), shown, new_rects->visible.left, new_rects->visible.top,
                     new_rects->visible.right, new_rects->visible.bottom, after );
}

/* Every top-level window gets a surface (the null driver's default
 * declines, which leaves windows undrawn). */
static BOOL BROWSER_WindowPosChanging( HWND hwnd, UINT swp_flags, BOOL shaped, const struct window_rects *rects )
{
    return TRUE;
}

static UINT BROWSER_ShowWindow( HWND hwnd, INT cmd, RECT *rect, UINT swp )
{
    return swp;
}

/* ---- Input -------------------------------------------------------------- */

static BOOL send_queued_input(void)
{
    INPUT input;
    BOOL any = FALSE;

    while (host_next_input( &input ))
    {
        NTSTATUS status = NtUserSendHardwareInput( 0, 0, &input, 0 );
        if (input.type == INPUT_MOUSE)
            TRACE( "mouse %d,%d flags %#x: %#x\n", (int)input.mi.dx, (int)input.mi.dy, (UINT)input.mi.dwFlags, (UINT)status );
        else
            TRACE( "key vk %#x scan %#x flags %#x: %#x\n", input.ki.wVk, input.ki.wScan, (UINT)input.ki.dwFlags, (UINT)status );
        any = TRUE;
    }
    return any;
}

/* The page is the only "window manager": with no foreground window (as
 * when a program has just shown its first window), the thread's active
 * window becomes the foreground one, which is where wineserver sends
 * keyboard input (X11 does this when the window manager focuses it). */
static void update_foreground(void)
{
    GUITHREADINFO info = {.cbSize = sizeof(info)};
    HWND foreground = NtUserGetForegroundWindow();

    if (foreground && foreground != NtUserGetDesktopWindow()) return;
    if (!NtUserGetGUIThreadInfo( GetCurrentThreadId(), &info ) || !info.hwndActive) return;
    TRACE( "foreground %p\n", info.hwndActive );
    NtUserSetForegroundWindow( info.hwndActive );
}

static BOOL BROWSER_ProcessEvents( DWORD mask )
{
    update_foreground();
    return send_queued_input();
}

/* From the wait loop (inproc/client.c), when the host reports input. */
void wasm_process_input(void)
{
    update_foreground();
    send_queued_input();
}

static void BROWSER_SetCursor( HWND hwnd, HCURSOR cursor )
{
}

static const struct user_driver_funcs browser_driver_funcs =
{
    .pUpdateDisplayDevices = BROWSER_UpdateDisplayDevices,
    .pCreateWindow = BROWSER_CreateWindow,
    .pSetDesktopWindow = BROWSER_SetDesktopWindow,
    .pDestroyWindow = BROWSER_DestroyWindow,
    .pCreateWindowSurface = BROWSER_CreateWindowSurface,
    .pWindowPosChanging = BROWSER_WindowPosChanging,
    .pWindowPosChanged = BROWSER_WindowPosChanged,
    .pShowWindow = BROWSER_ShowWindow,
    .pProcessEvents = BROWSER_ProcessEvents,
    .pSetCursor = BROWSER_SetCursor,
};

void browser_driver_init(void)
{
    __wine_set_user_driver( &browser_driver_funcs, WINE_GDI_DRIVER_VERSION );
}
