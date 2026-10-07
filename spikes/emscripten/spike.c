/* Spike (M1): can Emscripten keep its static data, stack and heap entirely
 * above the guest limit, leaving the low region to the Windows process?
 *
 * Built with -sGLOBAL_BASE=<guest limit>; checks the addresses of a global,
 * a stack local, malloc'd memory and a thread's stack, then reads and writes
 * guest addresses below the limit through plain pointers. */
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#ifndef GUEST_LIMIT
#error define GUEST_LIMIT
#endif

static int global_in_data = 42;
static char big_bss[1 << 20];

static void *thread_main(void *arg) {
    int local = 7;
    *(uintptr_t *)arg = (uintptr_t)&local;
    return 0;
}

static int check(const char *what, uintptr_t addr) {
    int ok = addr >= (uintptr_t)GUEST_LIMIT;
    printf("%-14s %#010lx %s\n", what, (unsigned long)addr, ok ? "above limit" : "BELOW LIMIT");
    return ok;
}

int main(void) {
    int local = 1;
    int ok = 1;
    ok &= check("static data", (uintptr_t)&global_in_data);
    ok &= check("bss", (uintptr_t)big_bss);
    ok &= check("main stack", (uintptr_t)&local);
    void *small = malloc(64);
    void *large = malloc(64 << 20);
    ok &= check("malloc small", (uintptr_t)small);
    ok &= check("malloc 64 MB", (uintptr_t)large);
    uintptr_t tstack = 0;
    pthread_t t;
    if (pthread_create(&t, 0, thread_main, &tstack) == 0) {
        pthread_join(t, 0);
        ok &= check("thread stack", tstack);
    } else {
        printf("pthread_create failed\n");
        ok = 0;
    }
    /* Guest memory below the limit is ordinary memory to native code. */
    volatile uint32_t *guest = (volatile uint32_t *)(uintptr_t)0x00400000;
    *guest = 0xC0FFEE;
    volatile uint8_t *high = (volatile uint8_t *)(uintptr_t)(GUEST_LIMIT - 16);
    memset((void *)high, 0x5a, 16);
    ok &= *guest == 0xC0FFEE && high[15] == 0x5a;
    printf("guest access   %s\n", *guest == 0xC0FFEE ? "ok" : "FAILED");
    printf("RESULT %s\n", ok ? "PASS" : "FAIL");
    return ok ? 0 : 1;
}
