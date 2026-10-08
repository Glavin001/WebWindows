/* The system window classes are there to look up and subclass, as Unreal's
 * InitWindowing does: GetClassInfoExW(NULL, base) for each control it wraps,
 * the rich edit one after loading riched32.dll and the common controls after
 * InitCommonControls. A class the program registers is found by its module.
 * libs: -lcomctl32
 */
#include <windows.h>
#include <commctrl.h>
#include <stdio.h>

static LRESULT CALLBACK proc(HWND hwnd, UINT msg, WPARAM wp, LPARAM lp)
{
    return DefWindowProcW(hwnd, msg, wp, lp);
}

int main(void)
{
    static const WCHAR *system[] = { L"LISTBOX", L"STATIC", L"EDIT", L"BUTTON", L"COMBOBOX", L"SCROLLBAR", L"MDICLIENT" };
    static const WCHAR *common[] = { L"msctls_trackbar32", L"msctls_progress32", L"SysListView32", L"SysTreeView32",
                                     L"SysTabControl32", L"msctls_updown32", L"ToolbarWindow32", L"tooltips_class32" };
    WNDCLASSEXW wc = { sizeof(wc) }, out;
    unsigned i;

    for (i = 0; i < ARRAYSIZE(system); i++)
    {
        out.cbSize = sizeof(out);
        printf("%ls: %d\n", system[i], GetClassInfoExW(NULL, system[i], &out) != 0);
    }
    InitCommonControls();
    for (i = 0; i < ARRAYSIZE(common); i++)
    {
        out.cbSize = sizeof(out);
        printf("%ls: %d\n", common[i], GetClassInfoExW(NULL, common[i], &out) != 0);
    }
    printf("riched32.dll: %d\n", LoadLibraryW(L"RICHED32.DLL") != NULL);
    out.cbSize = sizeof(out);
    printf("RICHEDIT: %d\n", GetClassInfoExW(NULL, L"RICHEDIT", &out) != 0);

    /* Subclass one the way Unreal does: same class info, new name and proc. */
    out.cbSize = sizeof(out);
    GetClassInfoExW(NULL, L"LISTBOX", &out);
    out.lpfnWndProc = proc;
    out.lpszClassName = L"WListBox";
    out.hInstance = GetModuleHandleW(NULL);
    printf("RegisterClassExW: %d\n", RegisterClassExW(&out) != 0);
    wc.cbSize = sizeof(wc);
    printf("GetClassInfoExW(module): %d\n", GetClassInfoExW(GetModuleHandleW(NULL), L"WListBox", &wc) != 0);
    printf("proc matches: %d\n", wc.lpfnWndProc == proc);
    printf("UnregisterClassW: %d\n", UnregisterClassW(L"WListBox", GetModuleHandleW(NULL)));
    return 0;
}
