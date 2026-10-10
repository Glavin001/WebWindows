/* Code that rewrites the instructions it is about to run.
 *
 * x86 runs the bytes in memory, also those of the next instructions in
 * line after a store changed them; packers and copy protection (SecuROM)
 * decrypt code that way, an instruction or two ahead of running it. Each
 * function here is written into executable memory, flips a byte of an
 * instruction further on with xor (`sub eax, 10` <-> `add eax, 10`) and then
 * runs it: within the same block, and after a jump. Each runs three times,
 * so the byte flips back and forth: 15, -5, 15.
 */
#include <windows.h>
#include <stdio.h>
#include <string.h>

typedef int (*fn)(void);

/* mov eax, 5; xor byte [patch], 0x28; <patch>: sub eax, 10; ret */
static void same_block(unsigned char *p)
{
    unsigned char *patch = p + 12;
    unsigned char code[] = {
        0xb8, 0x05, 0x00, 0x00, 0x00,       /* mov eax, 5 */
        0x80, 0x35, 0, 0, 0, 0, 0x28,       /* xor byte [patch], 0x28 */
        0x2d, 0x0a, 0x00, 0x00, 0x00,       /* sub eax, 10 (0x2d ^ 0x28 = 0x05: add eax, 10) */
        0xc3,                               /* ret */
    };
    memcpy(code + 7, &patch, 4);
    memcpy(p, code, sizeof code);
}

/* mov eax, 5; xor byte [patch], 0x28; jmp next; next: <patch>: sub eax, 10; ret */
static void after_jump(unsigned char *p)
{
    unsigned char *patch = p + 14;
    unsigned char code[] = {
        0xb8, 0x05, 0x00, 0x00, 0x00,       /* mov eax, 5 */
        0x80, 0x35, 0, 0, 0, 0, 0x28,       /* xor byte [patch], 0x28 */
        0xeb, 0x00,                         /* jmp next */
        0x2d, 0x0a, 0x00, 0x00, 0x00,       /* next: sub eax, 10 / add eax, 10 */
        0xc3,                               /* ret */
    };
    memcpy(code + 7, &patch, 4);
    memcpy(p, code, sizeof code);
}

static void run(const char *name, void (*write)(unsigned char *))
{
    unsigned char *p = VirtualAlloc(NULL, 4096, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE);
    int i;
    if (!p) {
        printf("%s: VirtualAlloc failed\n", name);
        return;
    }
    write(p);
    FlushInstructionCache(GetCurrentProcess(), p, 4096);
    printf("%s:", name);
    for (i = 0; i < 3; i++) printf(" %d", ((fn)p)());
    printf("\n");
    VirtualFree(p, 0, MEM_RELEASE);
}

int main(void)
{
    run("same block", same_block);
    run("after a jump", after_jump);
    return 0;
}
