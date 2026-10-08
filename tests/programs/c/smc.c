/* Self-modifying code: patch an instruction of a function in the image, and
 * rewrite generated code in an executable buffer, calling each through a
 * pointer before and after the write. */
#include <stdio.h>
#include <string.h>
#include <stdint.h>
#ifdef _WIN32
#include <windows.h>
static void make_writable(void *p, size_t n) {
    DWORD old;
    VirtualProtect(p, n, PAGE_EXECUTE_READWRITE, &old);
}
static unsigned char *exec_buffer(size_t n) {
    return VirtualAlloc(0, n, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE);
}
#else
#include <sys/mman.h>
static void make_writable(void *p, size_t n) {
    uintptr_t a = (uintptr_t)p & ~(uintptr_t)0xfff;
    mprotect((void *)a, ((uintptr_t)p + n - a + 0xfff) & ~(uintptr_t)0xfff,
             PROT_READ | PROT_WRITE | PROT_EXEC);
}
static unsigned char *exec_buffer(size_t n) {
    return mmap(0, n, PROT_READ | PROT_WRITE | PROT_EXEC, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
}
#endif

__attribute__((noinline)) int patched(void) { return 0x11111111; }

typedef int (*fn_t)(void);

/* Replaces the 32-bit immediate `from` in the first bytes of `f`. */
static int patch(unsigned char *f, uint32_t from, uint32_t to) {
    for (int i = 0; i < 32; i++) {
        uint32_t v;
        memcpy(&v, f + i, 4);
        if (v == from) {
            make_writable(f + i, 4);
            memcpy(f + i, &to, 4);
            return 1;
        }
    }
    return 0;
}

/* mov eax, imm32; ret */
static void emit(unsigned char *p, uint32_t imm) {
    p[0] = 0xb8;
    memcpy(p + 1, &imm, 4);
    p[5] = 0xc3;
}

int main(void) {
    fn_t volatile f = patched;
    printf("image before %#x\n", f());
    int ok = patch((unsigned char *)f, 0x11111111, 0x22222222);
    printf("image patched %d after %#x\n", ok, f());

    unsigned char *buf = exec_buffer(4096);
    fn_t volatile g = (fn_t)buf;
    emit(buf, 0x33333333);
    printf("buffer first %#x\n", g());
    emit(buf, 0x44444444);
    printf("buffer rewritten %#x\n", g());
    /* A store into the page that does not touch the code. */
    buf[100] = 1;
    printf("buffer again %#x\n", g());
    return 0;
}
