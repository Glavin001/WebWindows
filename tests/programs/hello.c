/* A tiny console program with no C runtime: the first milestone target.
   Build: i686-w64-mingw32-gcc -O2 -nostdlib -o hello.exe hello.c -lkernel32 -Wl,-e,_start */
#include <windows.h>

static unsigned fib(unsigned n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); }

static int fmt_uint(char *out, unsigned v) {
    char tmp[16];
    int n = 0, i = 0;
    do { tmp[n++] = '0' + v % 10; v /= 10; } while (v);
    while (n) out[i++] = tmp[--n];
    return i;
}

void start(void) {
    HANDLE out = GetStdHandle(STD_OUTPUT_HANDLE);
    char buf[64];
    DWORD written;
    const char msg[] = "Hello from translated x86!\n";
    WriteFile(out, msg, sizeof msg - 1, &written, 0);
    int n = 0;
    const char pre[] = "fib(20) = ";
    for (int i = 0; pre[i]; i++) buf[n++] = pre[i];
    n += fmt_uint(buf + n, fib(20));
    buf[n++] = '\n';
    WriteFile(out, buf, n, &written, 0);
    ExitProcess(fib(10) == 55 ? 0 : 1);
}
