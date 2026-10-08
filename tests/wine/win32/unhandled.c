/* An exception no handler takes: the top-level filter set with
 * SetUnhandledExceptionFilter sees it, and the process ends with the
 * exception code (STATUS_INTEGER_DIVIDE_BY_ZERO; the exit status's low
 * byte is 0x94).
 *
 * exit: 148
 */
#include <windows.h>
#include <stdio.h>

static LONG WINAPI filter(EXCEPTION_POINTERS *ep)
{
    printf("filter: code %08lx\n", ep->ExceptionRecord->ExceptionCode);
    return EXCEPTION_EXECUTE_HANDLER;
}

int main(void)
{
    volatile int zero = 0, hundred = 100;
    setvbuf(stdout, NULL, _IONBF, 0);
    SetUnhandledExceptionFilter(filter);
    printf("dividing by zero\n");
    printf("%d\n", hundred / zero);
    printf("not reached\n");
    return 0;
}
