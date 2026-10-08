/* Reads through a null-region pointer (tests/web/traps.mjs): an access
 * violation at the load, with memory traps or faithful checks alike. */
#include <stdio.h>

int main(int argc, char **argv)
{
    volatile int *p = (volatile int *)(argc * 16);
    (void)argv;
    printf("reading %p\n", (void *)p);
    return *p;
}
