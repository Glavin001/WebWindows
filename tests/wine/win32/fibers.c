/* Fiber stacks and freeing memory, as Windows does them: each fiber runs on
 * a stack of its own, private memory that DeleteFiber frees again (ntdll's
 * RtlCreateUserStack asks the system for it, ProcessThreadStackAllocation),
 * and VirtualFree releases only a whole allocation from its base, never an
 * image. A system that let the stack request succeed without allocating
 * gave fibers whatever address was left on the stack, and DeleteFiber then
 * freed whatever held it (ntdll itself, once).
 */
#include <windows.h>
#include <stdio.h>

static void *main_fiber;
static void *stack_base[3];

static void CALLBACK fiber_proc(void *arg)
{
    int n = (int)(INT_PTR)arg;
    MEMORY_BASIC_INFORMATION mbi;

    VirtualQuery(&mbi, &mbi, sizeof(mbi));
    stack_base[n] = mbi.AllocationBase;
    printf("fiber %d: stack is private memory: %d\n", n, mbi.Type == MEM_PRIVATE);
    SwitchToFiber(main_fiber);
}

static MEMORY_BASIC_INFORMATION query(const void *p)
{
    MEMORY_BASIC_INFORMATION mbi;
    VirtualQuery(p, &mbi, sizeof(mbi));
    return mbi;
}

int main(void)
{
    MEMORY_BASIC_INFORMATION mbi;
    HMODULE ntdll = GetModuleHandleA("ntdll.dll");
    void *fibers[3];
    char *p;
    int i;

    VirtualQuery(&mbi, &mbi, sizeof(mbi));
    main_fiber = ConvertThreadToFiber(NULL);
    for (i = 0; i < 3; i++)
    {
        fibers[i] = CreateFiber(0, fiber_proc, (void *)(INT_PTR)i);
        SwitchToFiber(fibers[i]);
    }
    printf("fiber stacks are not the thread's: %d\n",
           stack_base[0] != mbi.AllocationBase && stack_base[1] != mbi.AllocationBase);
    printf("each fiber has its own stack: %d\n",
           stack_base[0] != stack_base[1] && stack_base[1] != stack_base[2] && stack_base[0] != stack_base[2]);
    for (i = 0; i < 3; i++) DeleteFiber(fibers[i]);
    printf("DeleteFiber frees the stacks: %d\n",
           query(stack_base[0]).State == MEM_FREE && query(stack_base[2]).State == MEM_FREE);
    printf("ntdll is still an image: %d\n", query(ntdll).Type == MEM_IMAGE);

    printf("VirtualFree of an image fails: %d\n", !VirtualFree(ntdll, 0, MEM_RELEASE));
    printf("and leaves it mapped: %d\n", query(ntdll).Type == MEM_IMAGE);

    p = VirtualAlloc(NULL, 0x10000, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE);
    printf("VirtualFree from inside an allocation fails: %d\n", !VirtualFree(p + 0x1000, 0, MEM_RELEASE));
    printf("and leaves it allocated: %d\n", query(p).State == MEM_COMMIT);
    printf("VirtualFree from its base: %d\n", VirtualFree(p, 0, MEM_RELEASE));
    printf("frees it: %d\n", query(p).State == MEM_FREE);
    return 0;
}
