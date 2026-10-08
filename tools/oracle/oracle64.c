/* Real-hardware oracle for the x86-64 instruction test suite.
 *
 * The 64-bit counterpart of oracle.c: runs single x86-64 instructions
 * natively and records the resulting registers, flags, memory window and
 * FPU/SSE state (FXSAVE, which includes xmm8-15 in 64-bit mode).
 *
 * Build: gcc -O1 -no-pie -fno-pie -o oracle64 oracle64.c
 * Usage: oracle64 < cases.bin > results.bin
 *
 * Same fixed layout as oracle.c (crates/wwt-testkit/src/layout.rs), mapped
 * below 2 GB so 32-bit displacements reach it:
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
    uint32_t pad;
    uint64_t regs[16];
    uint64_t rflags;
    uint8_t mem[MEM_SIZE];
    uint8_t fx[512];
};
struct Result {
    uint64_t regs[16];
    uint64_t rflags;
    uint32_t marker;     /* 0 fall-through, 1 target, 2 return target, 0xff fault */
    uint32_t fault_sig;  /* signal number or 0 */
    uint64_t fault_rip;
    uint8_t mem[MEM_SIZE];
    uint8_t fx[512];
};
#pragma pack(pop)

uint64_t g_in_regs[16];
uint64_t g_in_flags;
uint64_t g_out_regs[16];
uint64_t g_out_flags;
uint64_t g_saved_rsp;
uint64_t g_ins_ptr = INS;
uint64_t g_epilogue_ptr;
volatile uint8_t g_marker;
uint8_t g_in_fx[512] __attribute__((aligned(16)));
uint8_t g_out_fx[512] __attribute__((aligned(16)));

extern void run_case(void);
extern char case_epilogue[];

/* Registers are loaded and stored RIP-relative; the instruction under test
   runs with every general register (rsp included) set from the case. */
__asm__(
    ".text\n"
    ".globl run_case\n"
    "run_case:\n"
    "  pushq %rbx\n"
    "  pushq %rbp\n"
    "  pushq %r12\n"
    "  pushq %r13\n"
    "  pushq %r14\n"
    "  pushq %r15\n"
    "  movq %rsp, g_saved_rsp(%rip)\n"
    "  fxrstor g_in_fx(%rip)\n"
    "  pushq g_in_flags(%rip)\n"
    "  popfq\n"
    "  movq g_in_regs+0(%rip), %rax\n"
    "  movq g_in_regs+8(%rip), %rcx\n"
    "  movq g_in_regs+16(%rip), %rdx\n"
    "  movq g_in_regs+24(%rip), %rbx\n"
    "  movq g_in_regs+40(%rip), %rbp\n"
    "  movq g_in_regs+48(%rip), %rsi\n"
    "  movq g_in_regs+56(%rip), %rdi\n"
    "  movq g_in_regs+64(%rip), %r8\n"
    "  movq g_in_regs+72(%rip), %r9\n"
    "  movq g_in_regs+80(%rip), %r10\n"
    "  movq g_in_regs+88(%rip), %r11\n"
    "  movq g_in_regs+96(%rip), %r12\n"
    "  movq g_in_regs+104(%rip), %r13\n"
    "  movq g_in_regs+112(%rip), %r14\n"
    "  movq g_in_regs+120(%rip), %r15\n"
    "  movq g_in_regs+32(%rip), %rsp\n"
    "  jmp *g_ins_ptr(%rip)\n"
    ".globl case_epilogue\n"
    "case_epilogue:\n"
    "  movq %rax, g_out_regs+0(%rip)\n"
    "  movq %rcx, g_out_regs+8(%rip)\n"
    "  movq %rdx, g_out_regs+16(%rip)\n"
    "  movq %rbx, g_out_regs+24(%rip)\n"
    "  movq %rsp, g_out_regs+32(%rip)\n"
    "  movq %rbp, g_out_regs+40(%rip)\n"
    "  movq %rsi, g_out_regs+48(%rip)\n"
    "  movq %rdi, g_out_regs+56(%rip)\n"
    "  movq %r8, g_out_regs+64(%rip)\n"
    "  movq %r9, g_out_regs+72(%rip)\n"
    "  movq %r10, g_out_regs+80(%rip)\n"
    "  movq %r11, g_out_regs+88(%rip)\n"
    "  movq %r12, g_out_regs+96(%rip)\n"
    "  movq %r13, g_out_regs+104(%rip)\n"
    "  movq %r14, g_out_regs+112(%rip)\n"
    "  movq %r15, g_out_regs+120(%rip)\n"
    "  movq g_saved_rsp(%rip), %rsp\n"
    "  pushfq\n"
    "  popq g_out_flags(%rip)\n"
    "  fxsave g_out_fx(%rip)\n"
    "  cld\n"
    "  popq %r15\n"
    "  popq %r14\n"
    "  popq %r13\n"
    "  popq %r12\n"
    "  popq %rbp\n"
    "  popq %rbx\n"
    "  ret\n");

static sigjmp_buf g_jb;
static volatile uint64_t g_fault_sig, g_fault_rip;

static void on_fault(int sig, siginfo_t *si, void *uc_) {
    ucontext_t *uc = uc_;
    greg_t *g = uc->uc_mcontext.gregs;
    (void)si;
    g_fault_sig = sig;
    g_fault_rip = g[REG_RIP];
    /* Record the registers at the fault for comparison. */
    static const int order[16] = {REG_RAX, REG_RCX, REG_RDX, REG_RBX, REG_RSP, REG_RBP,
                                  REG_RSI, REG_RDI, REG_R8,  REG_R9,  REG_R10, REG_R11,
                                  REG_R12, REG_R13, REG_R14, REG_R15};
    for (int i = 0; i < 16; i++) g_out_regs[i] = g[order[i]];
    g_out_flags = g[REG_EFL];
    siglongjmp(g_jb, 1);
}

/* mov byte [abs32 g_marker], k ; jmp [abs32 g_epilogue_ptr] -- touches
   neither flags nor the stack. The SIB forms (04 25, 24 25) address
   absolutely: the plain disp32 forms are RIP-relative in 64-bit mode. */
static void put_stub(uint8_t *p, uint8_t k) {
    uint32_t a = (uint32_t)(uintptr_t)&g_marker;
    uint32_t e = (uint32_t)(uintptr_t)&g_epilogue_ptr;
    p[0] = 0xC6; p[1] = 0x04; p[2] = 0x25; memcpy(p + 3, &a, 4); p[7] = k;
    p[8] = 0xFF; p[9] = 0x24; p[10] = 0x25; memcpy(p + 11, &e, 4);
}

int main(void) {
    if ((uintptr_t)&g_marker >= 0x80000000u || (uintptr_t)&g_epilogue_ptr >= 0x80000000u) {
        fprintf(stderr, "oracle64: globals must be below 2 GB (build with -no-pie)\n");
        return 1;
    }
    uint8_t *mem = mmap((void *)(uintptr_t)MEM_BASE, 0x1000, PROT_READ | PROT_WRITE,
                        MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED_NOREPLACE, -1, 0);
    uint8_t *code = mmap((void *)(uintptr_t)CODE_BASE, 0x1000, PROT_READ | PROT_WRITE | PROT_EXEC,
                         MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED_NOREPLACE, -1, 0);
    if (mem != (void *)(uintptr_t)MEM_BASE || code != (void *)(uintptr_t)CODE_BASE) {
        fprintf(stderr, "oracle64: cannot map fixed pages\n");
        return 1;
    }
    g_epilogue_ptr = (uint64_t)(uintptr_t)case_epilogue;

    static uint8_t altstack[65536];
    stack_t ss = {.ss_sp = altstack, .ss_size = sizeof altstack, .ss_flags = 0};
    sigaltstack(&ss, 0);
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = on_fault;
    sa.sa_flags = SA_SIGINFO | SA_ONSTACK | SA_NODEFER;
    int sigs[] = {SIGSEGV, SIGBUS, SIGFPE, SIGILL, SIGTRAP};
    for (unsigned i = 0; i < sizeof sigs / sizeof *sigs; i++) sigaction(sigs[i], &sa, 0);

    static struct Case c;
    static struct Result r;
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
        g_in_flags = c.rflags;
        memcpy(g_in_fx, c.fx, 512);
        g_marker = 0xEE;
        g_fault_sig = 0;
        g_fault_rip = 0;
        memset(&r, 0, sizeof r);
        if (sigsetjmp(g_jb, 1) == 0) {
            run_case();
            r.marker = g_marker;
        } else {
            __asm__ volatile("cld; fninit");
            r.marker = 0xff;
        }
        memcpy(r.regs, g_out_regs, sizeof r.regs);
        r.rflags = g_out_flags;
        r.fault_sig = g_fault_sig;
        r.fault_rip = g_fault_rip;
        memcpy(r.mem, mem, MEM_SIZE);
        if (r.marker != 0xff) memcpy(r.fx, g_out_fx, 512);
        fwrite(&r, sizeof r, 1, stdout);
    }
    return 0;
}
