/* The clocks and timers games pace their frames with: timeGetTime,
 * QueryPerformanceCounter, Sleep, winmm's multimedia timers (timeSetEvent,
 * periodic and one-shot, calling back on their own thread or setting an
 * event), WM_TIMER and waitable timers. Prints only what holds on any
 * machine (counts in ranges), so the output is the same natively.
 *
 * libs: -lwinmm
 */
#include <windows.h>
#include <mmsystem.h>
#include <stdio.h>

static volatile LONG ticks;
static volatile DWORD callback_thread;

static void CALLBACK tick(UINT id, UINT msg, DWORD_PTR user, DWORD_PTR dw1, DWORD_PTR dw2)
{
    InterlockedIncrement(&ticks);
    callback_thread = GetCurrentThreadId();
}

static int in_range(LONG v, LONG lo, LONG hi)
{
    if (v >= lo && v <= hi) return 1;
    fprintf(stderr, "%ld is not in [%ld, %ld]\n", v, lo, hi);
    return 0;
}

int main(void)
{
    LARGE_INTEGER f, a, b;
    DWORD t0, t1;
    UINT id;
    HANDLE ev, wt;
    MSG msg;
    int timers = 0;

    setvbuf(stdout, NULL, _IONBF, 0);

    /* Sleep and the clocks agree. */
    printf("timeBeginPeriod: %u\n", timeBeginPeriod(1));
    QueryPerformanceFrequency(&f);
    QueryPerformanceCounter(&a);
    t0 = timeGetTime();
    Sleep(100);
    t1 = timeGetTime();
    QueryPerformanceCounter(&b);
    printf("Sleep(100) by timeGetTime: %d\n", in_range(t1 - t0, 95, 400));
    printf("Sleep(100) by the performance counter: %d\n",
           in_range((LONG)((b.QuadPart - a.QuadPart) * 1000 / f.QuadPart), 95, 400));
    printf("GetTickCount moves: %d\n", (t0 = GetTickCount(), Sleep(50), GetTickCount() - t0 >= 30));

    /* A periodic multimedia timer, 10 ms, for about 300 ms: ticks for the
     * time that passed (a loaded machine oversleeps), 15 to 35 in 300 ms. */
    t0 = timeGetTime();
    id = timeSetEvent(10, 1, tick, 0, TIME_PERIODIC | TIME_CALLBACK_FUNCTION);
    printf("timeSetEvent: %d\n", id != 0);
    Sleep(300);
    timeKillEvent(id);
    t1 = timeGetTime() - t0;
    printf("periodic ticks in 300 ms: %d\n", in_range(ticks, t1 / 20, t1 / 10 + 5));
    printf("on another thread: %d\n", callback_thread && callback_thread != GetCurrentThreadId());
    ticks = 0;
    Sleep(50);
    printf("stopped after timeKillEvent: %d\n", ticks == 0);

    /* One-shot, setting an event. */
    ev = CreateEventA(NULL, FALSE, FALSE, NULL);
    t0 = timeGetTime();
    id = timeSetEvent(50, 1, (LPTIMECALLBACK)ev, 0, TIME_ONESHOT | TIME_CALLBACK_EVENT_SET);
    printf("one-shot event set: %d\n", WaitForSingleObject(ev, 2000) == WAIT_OBJECT_0);
    printf("after about 50 ms: %d\n", in_range(timeGetTime() - t0, 40, 500));

    /* WM_TIMER, through a message loop. */
    SetTimer(NULL, 0, 20, NULL);
    t0 = GetTickCount();
    while (timers < 5 && GetTickCount() - t0 < 2000 && GetMessageA(&msg, NULL, 0, 0))
        if (msg.message == WM_TIMER) timers++;
    printf("WM_TIMER five times: %d\n", timers == 5);

    /* A waitable timer, relative 30 ms. */
    wt = CreateWaitableTimerA(NULL, TRUE, NULL);
    a.QuadPart = -30 * 10000;
    SetWaitableTimer(wt, &a, 0, NULL, NULL, FALSE);
    t0 = timeGetTime();
    printf("waitable timer: %d\n", WaitForSingleObject(wt, 2000) == WAIT_OBJECT_0);
    printf("after about 30 ms: %d\n", in_range(timeGetTime() - t0, 20, 500));

    timeEndPeriod(1);
    printf("done\n");
    return 0;
}
