/* The Windows version, as programs read it from the registry.
 *
 * Every Windows (and every Wine prefix, which wineboot fills from wine.inf)
 * has CurrentVersion, CurrentBuildNumber and ProductName under
 * HKLM\Software\Microsoft\Windows NT\CurrentVersion. Installers and copy
 * protection read them (F.E.A.R.'s SecuROM quits when they are missing).
 * (GetVersionEx need not agree: since Windows 8.1 it reports 6.2.9200 to a
 * program whose manifest does not name a later Windows.)
 */
#include <windows.h>
#include <stdio.h>
#include <string.h>

int main(void)
{
    HKEY key;
    char version[64], build[64], product[256];
    DWORD size, type;
    OSVERSIONINFOA vi = { sizeof(vi) };
    LONG r = RegOpenKeyExA(HKEY_LOCAL_MACHINE, "Software\\Microsoft\\Windows NT\\CurrentVersion", 0, KEY_READ, &key);
    printf("open: %ld\n", r);
    if (r) return 1;
    size = sizeof(version);
    r = RegQueryValueExA(key, "CurrentVersion", NULL, &type, (BYTE *)version, &size);
    printf("CurrentVersion: %ld, type %lu, has a dot: %d\n", r, type, !r && strchr(version, '.') != NULL);
    size = sizeof(build);
    r = RegQueryValueExA(key, "CurrentBuildNumber", NULL, &type, (BYTE *)build, &size);
    printf("CurrentBuildNumber: %ld, type %lu\n", r, type);
    size = sizeof(product);
    r = RegQueryValueExA(key, "ProductName", NULL, &type, (BYTE *)product, &size);
    printf("ProductName: %ld, type %lu, starts with Windows: %d\n", r, type, !r && !strncmp(product, "Windows", 7));
    RegCloseKey(key);
    GetVersionExA(&vi);
    printf("GetVersionEx: NT %d, at least 6.2: %d\n", vi.dwPlatformId == VER_PLATFORM_WIN32_NT,
           vi.dwMajorVersion > 6 || (vi.dwMajorVersion == 6 && vi.dwMinorVersion >= 2));
    return 0;
}
