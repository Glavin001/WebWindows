/* Windows exceptions from faulting instructions and RaiseException.
 *
 * A vectored handler sees each fault, checks the exception record and the
 * registers in the CONTEXT, changes eax and skips the faulting instruction;
 * the code after it sees the new eax. A handler frame on FS:[0] (structured
 * exception handling as MSVC lays it out) catches an access violation and
 * leaves with longjmp, which unwinds the frame through RtlUnwind.
 */
#include <windows.h>
#include <setjmp.h>
#include <stdio.h>

static int skip;          /* bytes to skip after the faulting instruction */
static DWORD last_code;
static DWORD last_eip_offset;
static void *fault_label;

static LONG CALLBACK vectored(EXCEPTION_POINTERS *ep)
{
    EXCEPTION_RECORD *rec = ep->ExceptionRecord;
    CONTEXT *ctx = ep->ContextRecord;
    if (!skip) return EXCEPTION_CONTINUE_SEARCH;
    last_code = rec->ExceptionCode;
    last_eip_offset = ctx->Eip - (DWORD)fault_label;
    printf("  code %08lx", rec->ExceptionCode);
    if (rec->ExceptionCode == EXCEPTION_ACCESS_VIOLATION)
        printf(", params %lu [%lu, %08lx]", rec->NumberParameters,
               (unsigned long)rec->ExceptionInformation[0],
               (unsigned long)rec->ExceptionInformation[1]);
    printf(", address %s, esi %08lx, edi %08lx\n",
           rec->ExceptionAddress == fault_label ? "at the instruction" : "elsewhere",
           ctx->Esi, ctx->Edi);
    ctx->Eip += skip;
    ctx->Eax = 0x600d;
    skip = 0;
    return EXCEPTION_CONTINUE_EXECUTION;
}

/* Runs one faulting instruction (given as asm with a label), with known
 * values in esi and edi; returns eax after it. */
#define FAULT(name, len, setup, insn)                                         \
    static DWORD name(void)                                                   \
    {                                                                         \
        DWORD eax;                                                            \
        __asm__ volatile("lea 1f, %%eax\n\t"                                  \
                         "mov %%eax, %1\n\t"                                  \
                         "movl $" #len ", %2\n\t"                             \
                         "mov $0x5151, %%esi\n\t"                             \
                         "mov $0xd1d1, %%edi\n\t"                             \
                         setup "\n"                                           \
                         "1:\t" insn "\n\t"                                   \
                         : "=a"(eax), "=m"(fault_label), "=m"(skip)           \
                         :                                                    \
                         : "ecx", "edx", "esi", "edi", "memory");             \
        return eax;                                                           \
    }

FAULT(divide_by_zero, 2, "xor %%ecx, %%ecx\n\txor %%edx, %%edx\n\tmov $7, %%eax", "idiv %%ecx")
FAULT(divide_overflow, 2, "mov $0x80000000, %%eax\n\tcdq\n\tmov $-1, %%ecx", "idiv %%ecx")
FAULT(null_read, 2, "mov $0x10, %%eax", "mov (%%eax), %%eax")
FAULT(high_write, 2, "mov $0xfffffff0, %%eax", "mov %%eax, (%%eax)")
FAULT(breakpoint, 1, "", "int3")
FAULT(illegal, 2, "", "ud2")
FAULT(privileged, 1, "", "hlt")

static void run(const char *name, DWORD (*f)(void))
{
    DWORD eax;
    printf("%s:\n", name);
    eax = f();
    printf("  eip offset %lu, eax after %08lx\n", last_eip_offset, eax);
}

/* ---- A handler frame on FS:[0], left with longjmp ---- */

static jmp_buf env;

struct frame
{
    struct frame *prev;
    void *handler;
};

static EXCEPTION_DISPOSITION __cdecl frame_handler(EXCEPTION_RECORD *rec, void *frame,
                                                   CONTEXT *ctx, void *dispatcher)
{
    if (rec->ExceptionFlags & EXCEPTION_UNWINDING)
    {
        printf("  frame handler: unwinding\n");
        return ExceptionContinueSearch;
    }
    printf("  frame handler: code %08lx, eip %s\n", rec->ExceptionCode,
           ctx->Eip == (DWORD)rec->ExceptionAddress ? "matches the address" : "differs");
    longjmp(env, 42);
}

static void frame_test(void)
{
    struct frame reg;
    volatile int *volatile p = (int *)8;
    int r;

    printf("handler frame:\n");
    if ((r = setjmp(env)) == 0)
    {
        __asm__ volatile("mov %%fs:0, %%eax\n\t"
                         "mov %%eax, %0\n\t"
                         "lea %0, %%eax\n\t"
                         "mov %%eax, %%fs:0"
                         : "=m"(reg) : : "eax", "memory");
        reg.handler = frame_handler;
        *p = 1;
        printf("  not reached\n");
    }
    else
    {
        struct frame *head;
        __asm__ volatile("mov %%fs:0, %0" : "=r"(head));
        printf("  longjmp returned %d, frame %s\n", r, head == &reg ? "still registered" : "removed");
    }
}

/* ---- RaiseException ---- */

static LONG CALLBACK raised(EXCEPTION_POINTERS *ep)
{
    EXCEPTION_RECORD *rec = ep->ExceptionRecord;
    if (rec->ExceptionCode != 0xe0000001) return EXCEPTION_CONTINUE_SEARCH;
    printf("  code %08lx, flags %lu, params %lu [%lx, %lx]\n", rec->ExceptionCode,
           rec->ExceptionFlags, rec->NumberParameters,
           (unsigned long)rec->ExceptionInformation[0], (unsigned long)rec->ExceptionInformation[1]);
    return EXCEPTION_CONTINUE_EXECUTION;
}

int main(void)
{
    ULONG_PTR params[2] = { 0x1234, 0xabcd };
    void *h;

    setvbuf(stdout, NULL, _IONBF, 0);
    h = AddVectoredExceptionHandler(1, vectored);
    run("divide by zero", divide_by_zero);
    run("divide overflow", divide_overflow);
    run("null read", null_read);
    run("write above the address space", high_write);
    run("int3", breakpoint);
    run("ud2", illegal);
    run("hlt", privileged);
    RemoveVectoredExceptionHandler(h);

    frame_test();

    printf("RaiseException:\n");
    h = AddVectoredExceptionHandler(1, raised);
    RaiseException(0xe0000001, 0, 2, params);
    printf("  returned\n");
    RemoveVectoredExceptionHandler(h);
    printf("done\n");
    return 0;
}
