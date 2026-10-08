/* What games read to size themselves to the processor: the number of
 * processors, the affinity masks (Far Cry measures each processor on a
 * thread pinned to it), the time stamp counter's rate (timed against the
 * performance counter) and the speed in the registry (~MHz, which wineboot
 * writes).
 * Prints only what holds on any machine, so the output is the same natively.
 */
#include <windows.h>
#include <stdio.h>
#include <intrin.h>

static DWORD WINAPI on_cpu(void *arg)
{
    *(LONG *)arg = 1;
    return 0;
}

int main(void)
{
    DWORD_PTR process_mask = 0, system_mask = 0;
    LONG ran = 0;
    HANDLE thread;
    SYSTEM_INFO si;
    HKEY key;
    DWORD mhz = 0, size = sizeof(mhz);
    LARGE_INTEGER f, q0, q1;
    unsigned long long t0, t1;
    double rate;

    GetSystemInfo(&si);
    printf("processors: %d\n", si.dwNumberOfProcessors >= 1);
    printf("active processor mask: %d\n", si.dwActiveProcessorMask != 0);
    printf("GetProcessAffinityMask: %d\n", GetProcessAffinityMask(GetCurrentProcess(), &process_mask, &system_mask));
    printf("process mask: %d, system mask: %d\n", process_mask != 0, system_mask != 0);
    thread = CreateThread(NULL, 0, on_cpu, (void *)&ran, CREATE_SUSPENDED, NULL);
    printf("SetThreadAffinityMask on the first processor: %d\n", SetThreadAffinityMask(thread, process_mask & -process_mask) != 0);
    ResumeThread(thread);
    printf("pinned thread ran: %d\n", WaitForSingleObject(thread, 5000) == WAIT_OBJECT_0 && ran);
    CloseHandle(thread);

    QueryPerformanceFrequency(&f);
    QueryPerformanceCounter(&q0);
    t0 = __rdtsc();
    Sleep(100);
    t1 = __rdtsc();
    QueryPerformanceCounter(&q1);
    rate = (double)(t1 - t0) / ((double)(q1.QuadPart - q0.QuadPart) / f.QuadPart) / 1e6;
    printf("rdtsc runs at 100 MHz or more: %d\n", rate >= 100);
    printf("rdtsc moves between reads: %d\n", __rdtsc() != __rdtsc());

    printf("CentralProcessor\\0: %ld\n",
           RegOpenKeyExA(HKEY_LOCAL_MACHINE, "HARDWARE\\DESCRIPTION\\System\\CentralProcessor\\0", 0, KEY_READ, &key));
    printf("~MHz: %ld\n", RegQueryValueExA(key, "~MHz", NULL, NULL, (BYTE *)&mhz, &size));
    printf("~MHz is 100 or more: %d\n", mhz >= 100);
    RegCloseKey(key);
    return 0;
}
