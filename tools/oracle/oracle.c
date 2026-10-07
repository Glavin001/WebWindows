/* Real-hardware oracle for the instruction test suite.
 *
 * Runs single x86 instructions natively in a 32-bit Linux process and
 * records the resulting registers, flags, memory window and FPU/SSE state.
 * The translator's output is compared against these recordings.
 *
 * Build: gcc -m32 -O1 -no-pie -fno-pie -o oracle oracle.c
 * Usage: oracle < cases.bin > results.bin
 *
 * Fixed layout (shared with crates/wwt-testkit/src/layout.rs):
 *   MEM_BASE 0x00200000: 512-byte data window (operands, stack)
 *   INS      0x00300100: the instruction under test
 *   INS+len           : fall-through stub (marker 0)
 *   TGT      INS+0x40 : branch-target stub (marker 1)
 *   RET_TGT  INS+0x60 : return-target stub (marker 2)
 */
#define _GNU_SOURCE
#include <setjmp.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <ucontext.h>
#include <unistd.h>

#define MEM_BASE 0x00200000u
#define MEM_SIZE 512u
#define CODE_BASE 0x00300000u
#define INS (CODE_BASE + 0x100u)
#define TGT (INS + 0x40u)
#define RET_TGT (INS + 0x60u)

#pragma pack(push, 1)
struct Case {
    uint8_t code[16];
    uint32_t code_len;
    uint32_t regs[8];
    uint32_t eflags;
    uint32_t pad;
    uint8_t mem[MEM_SIZE];
    uint8_t fx[512];
};
struct Result {
    uint32_t regs[8];
    uint32_t eflags;
    uint32_t marker;     /* 0 fall-through, 1 target, 2 return target, 0xff fault */
    uint32_t fault_sig;  /* signal number or 0 */
    uint32_t fault_eip;
    uint8_t mem[MEM_SIZE];
    uint8_t fx[512];
};
#pragma pack(pop)

uint32_t g_in_regs[8];
uint32_t g_in_flags;
uint32_t g_out_regs[8];
uint32_t g_out_flags;
uint32_t g_saved_esp;
uint32_t g_ins_ptr = INS;
uint32_t g_epilogue_ptr;
volatile uint8_t g_marker;
uint8_t g_in_fx[512] __attribute__((aligned(16)));
uint8_t g_out_fx[512] __attribute__((aligned(16)));

extern void run_case(void);
extern char case_epilogue[];

__asm__(
    ".text\n"
    ".globl run_case\n"
    "run_case:\n"
    "  pushal\n"
    "  movl %esp, g_saved_esp\n"
    "  fxrstor g_in_fx\n"
    "  pushl g_in_flags\n"
    "  popfl\n"
    "  movl g_in_regs+0, %eax\n"
    "  movl g_in_regs+4, %ecx\n"
    "  movl g_in_regs+8, %edx\n"
    "  movl g_in_regs+12, %ebx\n"
    "  movl g_in_regs+20, %ebp\n"
    "  movl g_in_regs+24, %esi\n"
    "  movl g_in_regs+28, %edi\n"
    "  movl g_in_regs+16, %esp\n"
    "  jmp *g_ins_ptr\n"
    ".globl case_epilogue\n"
    "case_epilogue:\n"
    "  movl %eax, g_out_regs+0\n"
    "  movl %ecx, g_out_regs+4\n"
    "  movl %edx, g_out_regs+8\n"
    "  movl %ebx, g_out_regs+12\n"
    "  movl %esp, g_out_regs+16\n"
    "  movl %ebp, g_out_regs+20\n"
    "  movl %esi, g_out_regs+24\n"
    "  movl %edi, g_out_regs+28\n"
    "  movl g_saved_esp, %esp\n"
    "  pushfl\n"
    "  popl g_out_flags\n"
    "  fxsave g_out_fx\n"
    "  cld\n"
    "  popal\n"
    "  ret\n");

static sigjmp_buf g_jb;
static volatile uint32_t g_fault_sig, g_fault_eip;

static void on_fault(int sig, siginfo_t *si, void *uc_) {
    ucontext_t *uc = uc_;
    (void)si;
    g_fault_sig = sig;
    g_fault_eip = uc->uc_mcontext.gregs[REG_EIP];
    /* Record the registers at the fault for comparison. */
    g_out_regs[0] = uc->uc_mcontext.gregs[REG_EAX];
    g_out_regs[1] = uc->uc_mcontext.gregs[REG_ECX];
    g_out_regs[2] = uc->uc_mcontext.gregs[REG_EDX];
    g_out_regs[3] = uc->uc_mcontext.gregs[REG_EBX];
    g_out_regs[4] = uc->uc_mcontext.gregs[REG_ESP];
    g_out_regs[5] = uc->uc_mcontext.gregs[REG_EBP];
    g_out_regs[6] = uc->uc_mcontext.gregs[REG_ESI];
    g_out_regs[7] = uc->uc_mcontext.gregs[REG_EDI];
    g_out_flags = uc->uc_mcontext.gregs[REG_EFL];
    siglongjmp(g_jb, 1);
}

/* mov byte [g_marker], k ; jmp [g_epilogue_ptr] -- touches neither flags
   nor the stack. */
static void put_stub(uint8_t *p, uint8_t k) {
    uint32_t a = (uint32_t)(uintptr_t)&g_marker;
    uint32_t e = (uint32_t)(uintptr_t)&g_epilogue_ptr;
    p[0] = 0xC6; p[1] = 0x05; memcpy(p + 2, &a, 4); p[6] = k;
    p[7] = 0xFF; p[8] = 0x25; memcpy(p + 9, &e, 4);
}

int main(void) {
    uint8_t *mem = mmap((void *)MEM_BASE, 0x1000, PROT_READ | PROT_WRITE,
                        MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED_NOREPLACE, -1, 0);
    uint8_t *code = mmap((void *)CODE_BASE, 0x1000, PROT_READ | PROT_WRITE | PROT_EXEC,
                         MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED_NOREPLACE, -1, 0);
    if (mem != (void *)MEM_BASE || code != (void *)CODE_BASE) {
        fprintf(stderr, "oracle: cannot map fixed pages\n");
        return 1;
    }
    g_epilogue_ptr = (uint32_t)(uintptr_t)case_epilogue;

    static uint8_t altstack[65536];
    stack_t ss = {.ss_sp = altstack, .ss_size = sizeof altstack, .ss_flags = 0};
    sigaltstack(&ss, 0);
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = on_fault;
    sa.sa_flags = SA_SIGINFO | SA_ONSTACK | SA_NODEFER;
    int sigs[] = {SIGSEGV, SIGBUS, SIGFPE, SIGILL, SIGTRAP};
    for (unsigned i = 0; i < sizeof sigs / sizeof *sigs; i++) sigaction(sigs[i], &sa, 0);

    struct Case c;
    struct Result r;
    while (fread(&c, sizeof c, 1, stdin) == 1) {
        memset(code, 0xCC, 0x1000);
        memcpy(code + 0x100, c.code, c.code_len);
        put_stub(code + 0x100 + c.code_len, 0);
        put_stub(code + 0x140, 1);
        put_stub(code + 0x160, 2);
        __builtin___clear_cache((char *)code, (char *)code + 0x1000);
        memset(mem, 0, 0x1000);
        memcpy(mem, c.mem, MEM_SIZE);
        memcpy(g_in_regs, c.regs, sizeof g_in_regs);
        g_in_flags = c.eflags;
        memcpy(g_in_fx, c.fx, 512);
        g_marker = 0xEE;
        g_fault_sig = 0;
        g_fault_eip = 0;
        memset(&r, 0, sizeof r);
        if (sigsetjmp(g_jb, 1) == 0) {
            run_case();
            r.marker = g_marker;
        } else {
            __asm__ volatile("cld; fninit");
            r.marker = 0xff;
        }
        memcpy(r.regs, g_out_regs, sizeof r.regs);
        r.eflags = g_out_flags;
        r.fault_sig = g_fault_sig;
        r.fault_eip = g_fault_eip;
        memcpy(r.mem, mem, MEM_SIZE);
        if (r.marker != 0xff) memcpy(r.fx, g_out_fx, 512);
        fwrite(&r, sizeof r, 1, stdout);
    }
    return 0;
}
