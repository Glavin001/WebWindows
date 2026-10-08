/* Directory listings give names in the case they were created with, as
 * Windows does (lookups ignore case): programs that find their files by the
 * names they list (Far Cry's shaders) depend on it.
 */
#include <windows.h>
#include <stdio.h>

int main(void)
{
    WIN32_FIND_DATAA fd;
    HANDLE h, f;

    printf("CreateDirectory: %d\n", CreateDirectoryA("MixedCase", NULL));
    f = CreateFileA("MixedCase\\SomeFile.TXT", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    printf("CreateFile: %d\n", f != INVALID_HANDLE_VALUE);
    CloseHandle(f);
    f = CreateFileA("mixedcase\\SOMEFILE.txt", GENERIC_READ, 0, NULL, OPEN_EXISTING, 0, NULL);
    printf("opened in another case: %d\n", f != INVALID_HANDLE_VALUE);
    CloseHandle(f);
    h = FindFirstFileA("mixedcase\\*.txt", &fd);
    printf("listed: %s\n", h != INVALID_HANDLE_VALUE ? fd.cFileName : "(nothing)");
    FindClose(h);
    h = FindFirstFileA("Mixed*", &fd);
    printf("directory listed: %s\n", h != INVALID_HANDLE_VALUE ? fd.cFileName : "(nothing)");
    FindClose(h);
    DeleteFileA("MixedCase\\SomeFile.TXT");
    RemoveDirectoryA("MixedCase");
    return 0;
}
