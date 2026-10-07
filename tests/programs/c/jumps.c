/* setjmp/longjmp across frames, varargs, and recursion through pointers. */
#include <stdio.h>
#include <setjmp.h>
#include <stdarg.h>

static jmp_buf env;
static int depth_reached;

static void dive(int n) {
    depth_reached = n;
    if (n == 25) longjmp(env, n * 2);
    dive(n + 1);
}

static double avg(int n, ...) {
    va_list ap;
    va_start(ap, n);
    double s = 0;
    for (int i = 0; i < n; i++) s += va_arg(ap, double);
    va_end(ap);
    return s / n;
}

static long mix(const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    long r = 0;
    for (const char *p = fmt; *p; p++) {
        if (*p == 'i') r = r * 7 + va_arg(ap, int);
        else if (*p == 'l') r = r * 11 + (long)va_arg(ap, long long);
        else if (*p == 'd') r = r * 13 + (long)(va_arg(ap, double) * 100);
    }
    va_end(ap);
    return r;
}

int main(void) {
    int v = setjmp(env);
    if (v == 0) {
        dive(0);
        printf("not reached\n");
    }
    printf("longjmp returned %d at depth %d\n", v, depth_reached);
    printf("avg %.3f\n", avg(4, 1.5, 2.5, 3.25, 4.0));
    printf("mix %ld\n", mix("ildil", 3, 5LL, 2.5, 9, 77LL));
    return v;
}
