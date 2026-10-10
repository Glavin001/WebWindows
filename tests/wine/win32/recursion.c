/* Deep recursion, as Unreal's package loader and script interpreter do on
 * a thread with a big stack: deeper than the WebAssembly stack holds when
 * every guest call nests a WebAssembly call, so translated calls past a
 * depth budget return to the dispatch loop (the return addresses are on
 * the guest stack, which is all a return needs).
 */
#include <windows.h>
#include <stdio.h>

static int __attribute__((noinline)) depth(volatile int *up, int n)
{
    volatile int here[4];
    here[0] = n;
    here[1] = up ? up[0] : -1;
    if (n == 0) return here[1] + 1;
    return depth(here, n - 1) + (here[0] == n);
}

/* Mutual recursion through a function pointer (an indirect call). */
static int (*volatile other)(int);
static int __attribute__((noinline)) ping(int n) { return n ? other(n - 1) + 1 : 0; }
static int __attribute__((noinline)) pong(int n) { return n ? ping(n - 1) + 1 : 0; }

static DWORD WINAPI deep(void *arg)
{
    int n = (int)(INT_PTR)arg;
    printf("depth(%d): %d\n", n, depth(NULL, n));
    other = pong;
    printf("ping(%d): %d\n", n, ping(n));
    return 0;
}

int main(void)
{
    static const int n[] = { 10, 1000, 100000 };
    unsigned i;

    setvbuf(stdout, NULL, _IONBF, 0);
    for (i = 0; i < 3; i++)
    {
        HANDLE t = CreateThread(NULL, 64 << 20, deep, (void *)(INT_PTR)n[i], STACK_SIZE_PARAM_IS_A_RESERVATION, NULL);
        WaitForSingleObject(t, INFINITE);
        CloseHandle(t);
    }
    return 0;
}
