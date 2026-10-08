/* Windows API workloads for tools/bench/suite.mjs: each benchmark repeats a
 * mix of calls into one area of the API and prints its name, a checksum of
 * the results (the same on every Windows implementation) and the time it
 * took, measured with QueryPerformanceCounter.
 *
 *   apibench [scale] [name...]
 *
 * Console only (kernel32, msvcrt, advapi32): runs on translated Wine without
 * Wine's Unix side. guibench.c covers GDI and window messages. */
#include <windows.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <wchar.h>

static int scale = 1;
static LARGE_INTEGER freq;

static double now(void) {
    LARGE_INTEGER t;
    QueryPerformanceCounter(&t);
    return (double)t.QuadPart / (double)freq.QuadPart;
}

static unsigned rng = 12345;
static unsigned next(void) {
    rng = rng * 1103515245u + 12345u;
    return rng >> 8;
}

/* ---- Heap ---- */

static unsigned bench_heap(void) {
    HANDLE heap = GetProcessHeap();
    enum { SLOTS = 4096 };
    void *slot[SLOTS] = {0};
    unsigned sum = 0;
    for (int i = 0; i < 400000 * scale; i++) {
        unsigned k = next() % SLOTS;
        if (slot[k]) {
            sum += ((unsigned char *)slot[k])[0];
            HeapFree(heap, 0, slot[k]);
            slot[k] = 0;
        } else {
            size_t n = 16 + next() % 2048;
            slot[k] = HeapAlloc(heap, 0, n);
            ((unsigned char *)slot[k])[0] = (unsigned char)n;
        }
    }
    for (int k = 0; k < SLOTS; k++) HeapFree(heap, 0, slot[k]);
    return sum;
}

static unsigned bench_malloc(void) {
    enum { SLOTS = 2048 };
    char *slot[SLOTS] = {0};
    unsigned sum = 0;
    for (int i = 0; i < 300000 * scale; i++) {
        unsigned k = next() % SLOTS;
        if (!slot[k]) {
            slot[k] = malloc(8 + next() % 512);
            slot[k][0] = (char)i;
        } else if (next() & 1) {
            slot[k] = realloc(slot[k], 8 + next() % 1024);
            sum += (unsigned char)slot[k][0];
        } else {
            sum += (unsigned char)slot[k][0];
            free(slot[k]);
            slot[k] = 0;
        }
    }
    for (int k = 0; k < SLOTS; k++) free(slot[k]);
    return sum;
}

/* ---- Files ---- */

static unsigned bench_files(void) {
    char buf[4096], name[MAX_PATH];
    unsigned sum = 0;
    CreateDirectoryA("bench_dir", NULL);
    for (int round = 0; round < 4 * scale; round++) {
        for (int i = 0; i < 200; i++) {
            sprintf(name, "bench_dir\\f%03d.dat", i);
            HANDLE h = CreateFileA(name, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
            DWORD n;
            memset(buf, 'a' + i % 26, sizeof buf);
            for (int k = 0; k < 4; k++) WriteFile(h, buf, sizeof buf, &n, NULL);
            CloseHandle(h);
        }
        WIN32_FIND_DATAA fd;
        HANDLE f = FindFirstFileA("bench_dir\\*.dat", &fd);
        int found = 0;
        if (f != INVALID_HANDLE_VALUE) {
            do found++;
            while (FindNextFileA(f, &fd));
            FindClose(f);
        }
        sum += found;
        for (int i = 0; i < 200; i++) {
            sprintf(name, "bench_dir\\f%03d.dat", i);
            HANDLE h = CreateFileA(name, GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, 0, NULL);
            DWORD n, size = GetFileSize(h, NULL);
            sum += size;
            while (ReadFile(h, buf, sizeof buf, &n, NULL) && n) sum += (unsigned char)buf[n - 1];
            CloseHandle(h);
            DeleteFileA(name);
        }
    }
    RemoveDirectoryA("bench_dir");
    return sum;
}

static unsigned bench_seek(void) {
    char buf[512];
    DWORD n;
    unsigned sum = 0;
    HANDLE h = CreateFileA("bench_seek.dat", GENERIC_READ | GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    for (int i = 0; i < 2048; i++) {
        memset(buf, i & 0xff, sizeof buf);
        WriteFile(h, buf, sizeof buf, &n, NULL);
    }
    for (int i = 0; i < 100000 * scale; i++) {
        unsigned block = next() % 2048;
        SetFilePointer(h, block * 512 + (next() % 256), NULL, FILE_BEGIN);
        if (i & 7) {
            ReadFile(h, buf, 64, &n, NULL);
            sum += (unsigned char)buf[0] + n;
        } else {
            buf[0] = (char)i;
            WriteFile(h, buf, 16, &n, NULL);
        }
    }
    CloseHandle(h);
    DeleteFileA("bench_seek.dat");
    return sum;
}

/* ---- Strings ---- */

static unsigned bench_strings(void) {
    char a[256];
    WCHAR w[256], w2[256];
    unsigned sum = 0;
    for (int i = 0; i < 100000 * scale; i++) {
        int len = sprintf(a, "Item %d of %x: %s", i, i * 7, (i & 1) ? "Odd" : "even");
        int wl = MultiByteToWideChar(CP_ACP, 0, a, len + 1, w, 256);
        lstrcpyW(w2, w);
        CharUpperW(w2);
        sum += CompareStringW(LOCALE_USER_DEFAULT, NORM_IGNORECASE, w, -1, w2, -1);
        sum += lstrcmpiW(w, w2) == 0;
        sum += WideCharToMultiByte(CP_ACP, 0, w2, -1, a, sizeof a, NULL, NULL);
        int x, y;
        if (sscanf(a, "ITEM %d OF %x", &x, &y) == 2) sum += x ^ y;
        sum += (unsigned)wcslen(w) + wl;
    }
    return sum;
}

/* ---- Synchronization, TLS, time ---- */

static unsigned bench_sync(void) {
    CRITICAL_SECTION cs;
    InitializeCriticalSection(&cs);
    DWORD tls = TlsAlloc();
    volatile LONG counter = 0;
    unsigned sum = 0;
    for (int i = 0; i < 1000000 * scale; i++) {
        EnterCriticalSection(&cs);
        InterlockedIncrement(&counter);
        TlsSetValue(tls, (void *)(INT_PTR)i);
        sum += (unsigned)(INT_PTR)TlsGetValue(tls) & 1;
        LeaveCriticalSection(&cs);
        if ((i & 255) == 0) {
            LARGE_INTEGER t;
            QueryPerformanceCounter(&t);
            sum += GetTickCount() > 0;
        }
    }
    TlsFree(tls);
    DeleteCriticalSection(&cs);
    return sum + (unsigned)counter;
}

/* ---- Callbacks across the DLL boundary ---- */

static int cmp_int(const void *a, const void *b) {
    int x = *(const int *)a, y = *(const int *)b;
    return (x > y) - (x < y);
}

static unsigned bench_qsort(void) {
    int n = 200000 * scale;
    int *v = malloc(n * sizeof *v);
    unsigned sum = 0;
    for (int round = 0; round < 3; round++) {
        for (int i = 0; i < n; i++) v[i] = (int)next();
        qsort(v, n, sizeof *v, cmp_int);
        for (int i = 0; i < n; i += 1000) {
            int key = v[i];
            sum += bsearch(&key, v, n, sizeof *v, cmp_int) != NULL;
            sum += (unsigned)v[i] >> 20;
        }
    }
    free(v);
    return sum;
}

/* ---- Registry ---- */

static unsigned bench_registry(void) {
    HKEY key;
    unsigned sum = 0;
    if (RegCreateKeyExA(HKEY_CURRENT_USER, "Software\\WebWindowsBench", 0, NULL, 0, KEY_ALL_ACCESS, NULL, &key, NULL))
        return 0;
    char name[32];
    for (int i = 0; i < 20000 * scale; i++) {
        sprintf(name, "v%d", i % 200);
        DWORD v = i, type, size = sizeof v;
        RegSetValueExA(key, name, 0, REG_DWORD, (const BYTE *)&v, sizeof v);
        if (RegQueryValueExA(key, name, NULL, &type, (BYTE *)&v, &size) == 0) sum += v & 0xff;
    }
    RegCloseKey(key);
    RegDeleteKeyA(HKEY_CURRENT_USER, "Software\\WebWindowsBench");
    return sum;
}

static const struct {
    const char *name;
    unsigned (*run)(void);
} benches[] = {
    {"heap", bench_heap},     {"malloc", bench_malloc},   {"files", bench_files},
    {"seek", bench_seek},     {"strings", bench_strings}, {"sync", bench_sync},
    {"qsort", bench_qsort},   {"registry", bench_registry},
};

int main(int argc, char **argv) {
    QueryPerformanceFrequency(&freq);
    if (argc > 1) scale = atoi(argv[1]);
    if (scale < 1) scale = 1;
    for (unsigned i = 0; i < sizeof benches / sizeof benches[0]; i++) {
        int want = argc <= 2;
        for (int a = 2; a < argc; a++) want |= strcmp(argv[a], benches[i].name) == 0;
        if (!want) continue;
        rng = 12345;
        double t0 = now();
        unsigned sum = benches[i].run();
        printf("%-12s %12u %8.3f\n", benches[i].name, sum, now() - t0);
        fflush(stdout);
    }
    return 0;
}
