/* File times and sizes as programs read them: a file written now has a
 * time close to now, the same through GetFileTime and a directory listing,
 * and the listing gives its size (games compare file times with what their
 * caches recorded, and recompile what looks changed).
 */
#include <windows.h>
#include <stdio.h>

static ULONGLONG u64(FILETIME t)
{
    return ((ULONGLONG)t.dwHighDateTime << 32) | t.dwLowDateTime;
}

int main(void)
{
    HANDLE f, h;
    FILETIME now, created, accessed, written, later;
    WIN32_FIND_DATAA fd;
    DWORD n;
    ULONGLONG diff;

    GetSystemTimeAsFileTime(&now);
    f = CreateFileA("times.txt", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    WriteFile(f, "twelve bytes", 12, &n, NULL);
    printf("GetFileTime: %d\n", GetFileTime(f, &created, &accessed, &written));
    CloseHandle(f);
    diff = u64(written) > u64(now) ? u64(written) - u64(now) : u64(now) - u64(written);
    printf("written within a minute of now: %d\n", diff < 600000000ull);
    h = FindFirstFileA("times.txt", &fd);
    printf("listed: %d\n", h != INVALID_HANDLE_VALUE);
    printf("listing has the same write time: %d\n", u64(fd.ftLastWriteTime) == u64(written));
    printf("listing has the size: %lu\n", fd.nFileSizeLow);
    FindClose(h);
    Sleep(50);
    f = CreateFileA("times.txt", GENERIC_WRITE, 0, NULL, OPEN_EXISTING, 0, NULL);
    SetFilePointer(f, 0, NULL, FILE_END);
    WriteFile(f, "!", 1, &n, NULL);
    GetFileTime(f, NULL, NULL, &later);
    CloseHandle(f);
    printf("a later write does not go back in time: %d\n", u64(later) >= u64(written));
    DeleteFileA("times.txt");
    return 0;
}
