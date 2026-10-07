/* Windows threads: creation, joins and exit codes, events, mutexes,
 * semaphores, critical sections, interlocked operations, thread-local
 * storage, Sleep, a suspended start, and waits on several objects. Only the
 * main thread prints, after joining, so the output does not depend on the
 * order threads run in.
 */
#include <windows.h>
#include <stdio.h>

static LONG counter;
static CRITICAL_SECTION cs;
static int shared_total;
static DWORD tls;

static DWORD WINAPI add_thread(void *arg)
{
    int i, n = (int)(INT_PTR)arg;
    for (i = 0; i < 1000; i++)
    {
        InterlockedIncrement(&counter);
        EnterCriticalSection(&cs);
        shared_total += n;
        LeaveCriticalSection(&cs);
        if (i % 100 == 0) Sleep(0);
    }
    return 100 + n;
}

static DWORD WINAPI tls_thread(void *arg)
{
    TlsSetValue(tls, arg);
    Sleep(5);
    return TlsGetValue(tls) == arg ? 1 : 0;
}

/* Producer and consumer around a two-slot buffer. */
static HANDLE slots_free, slots_full;
static int buffer[2], sum_consumed;

static DWORD WINAPI producer(void *arg)
{
    int i;
    for (i = 1; i <= 50; i++)
    {
        WaitForSingleObject(slots_free, INFINITE);
        buffer[i % 2] = i;
        ReleaseSemaphore(slots_full, 1, NULL);
    }
    return 0;
}

static DWORD WINAPI consumer(void *arg)
{
    int i;
    for (i = 1; i <= 50; i++)
    {
        WaitForSingleObject(slots_full, INFINITE);
        sum_consumed += buffer[i % 2];
        ReleaseSemaphore(slots_free, 1, NULL);
    }
    return 0;
}

static HANDLE go, done;
static volatile int started;

static DWORD WINAPI waiter(void *arg)
{
    started = 1;
    WaitForSingleObject(go, INFINITE);
    SetEvent(done);
    return 7;
}

static HANDLE mutex;
static int in_mutex, mutex_overlap;

static DWORD WINAPI mutex_thread(void *arg)
{
    int i;
    for (i = 0; i < 20; i++)
    {
        WaitForSingleObject(mutex, INFINITE);
        if (in_mutex++) mutex_overlap = 1;
        Sleep(1);
        in_mutex--;
        ReleaseMutex(mutex);
    }
    return 0;
}

static DWORD WINAPI sleeper(void *arg)
{
    Sleep((DWORD)(INT_PTR)arg);
    return GetCurrentThreadId() != 0;
}

int main(void)
{
    HANDLE h[4];
    DWORD code, ret, t0;
    int i;

    setvbuf(stdout, NULL, _IONBF, 0);
    InitializeCriticalSection(&cs);

    /* Four threads add to a counter and a total; joins and exit codes. */
    for (i = 0; i < 4; i++) h[i] = CreateThread(NULL, 0, add_thread, (void *)(INT_PTR)(i + 1), 0, NULL);
    ret = WaitForMultipleObjects(4, h, TRUE, INFINITE);
    printf("wait all: %lu\n", ret);
    printf("counter %ld, total %d\n", counter, shared_total);
    for (i = 0; i < 4; i++)
    {
        GetExitCodeThread(h[i], &code);
        printf("thread %d exit code %lu\n", i, code);
        CloseHandle(h[i]);
    }

    /* Thread-local storage. */
    tls = TlsAlloc();
    TlsSetValue(tls, (void *)0x1234);
    h[0] = CreateThread(NULL, 0, tls_thread, (void *)0x5678, 0, NULL);
    h[1] = CreateThread(NULL, 0, tls_thread, (void *)0x9abc, 0, NULL);
    WaitForMultipleObjects(2, h, TRUE, INFINITE);
    GetExitCodeThread(h[0], &code);
    printf("tls: main %p, thread 1 %lu", TlsGetValue(tls), code);
    GetExitCodeThread(h[1], &code);
    printf(", thread 2 %lu\n", code);
    CloseHandle(h[0]);
    CloseHandle(h[1]);

    /* Producer and consumer. */
    slots_free = CreateSemaphoreA(NULL, 2, 2, NULL);
    slots_full = CreateSemaphoreA(NULL, 0, 2, NULL);
    h[0] = CreateThread(NULL, 0, producer, NULL, 0, NULL);
    h[1] = CreateThread(NULL, 0, consumer, NULL, 0, NULL);
    WaitForMultipleObjects(2, h, TRUE, INFINITE);
    printf("consumed %d\n", sum_consumed);
    CloseHandle(h[0]);
    CloseHandle(h[1]);

    /* A suspended thread, events, and a still-running exit code. */
    go = CreateEventA(NULL, TRUE, FALSE, NULL);
    done = CreateEventA(NULL, FALSE, FALSE, NULL);
    h[0] = CreateThread(NULL, 0, waiter, NULL, CREATE_SUSPENDED, NULL);
    Sleep(10);
    printf("suspended thread started: %d\n", started);
    GetExitCodeThread(h[0], &code);
    printf("exit code while running: %s\n", code == STILL_ACTIVE ? "STILL_ACTIVE" : "other");
    ResumeThread(h[0]);
    printf("wait done (timeout 10): %lu\n", WaitForSingleObject(done, 10));
    SetEvent(go);
    printf("wait done: %lu\n", WaitForSingleObject(done, INFINITE));
    WaitForSingleObject(h[0], INFINITE);
    GetExitCodeThread(h[0], &code);
    printf("waiter exit code %lu\n", code);
    CloseHandle(h[0]);

    /* A mutex between two threads. */
    mutex = CreateMutexA(NULL, FALSE, NULL);
    h[0] = CreateThread(NULL, 0, mutex_thread, NULL, 0, NULL);
    h[1] = CreateThread(NULL, 0, mutex_thread, NULL, 0, NULL);
    WaitForMultipleObjects(2, h, TRUE, INFINITE);
    printf("mutex overlap: %d\n", mutex_overlap);
    CloseHandle(h[0]);
    CloseHandle(h[1]);

    /* Wait for any of several threads: the shortest sleeper finishes first. */
    t0 = GetTickCount();
    h[0] = CreateThread(NULL, 0, sleeper, (void *)300, 0, NULL);
    h[1] = CreateThread(NULL, 0, sleeper, (void *)20, 0, NULL);
    h[2] = CreateThread(NULL, 0, sleeper, (void *)200, 0, NULL);
    ret = WaitForMultipleObjects(3, h, FALSE, INFINITE);
    printf("first to finish: %lu\n", ret);
    WaitForMultipleObjects(3, h, TRUE, INFINITE);
    printf("all finished after at least 300 ms: %d\n", GetTickCount() - t0 >= 290);
    for (i = 0; i < 3; i++) CloseHandle(h[i]);

    DeleteCriticalSection(&cs);
    printf("done\n");
    return 0;
}
