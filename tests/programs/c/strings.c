/* String and memory functions, struct copies, sprintf. */
#include <stdio.h>
#include <string.h>
#include <stdlib.h>

struct rec { char name[24]; int vals[9]; double w; };

static void rev(char *s) { size_t n = strlen(s); for (size_t i = 0; i < n / 2; i++) { char t = s[i]; s[i] = s[n - 1 - i]; s[n - 1 - i] = t; } }

int main(void) {
    char buf[256];
    struct rec a, b;
    memset(&a, 0, sizeof a);
    strcpy(a.name, "translated");
    for (int i = 0; i < 9; i++) a.vals[i] = i * i - 3;
    a.w = 2.5;
    b = a;
    b.vals[3] = 1000;
    printf("%s %d %d %d\n", b.name, b.vals[2], b.vals[3], memcmp(&a, &b, sizeof a) != 0);
    sprintf(buf, "%5d|%-5d|%05d|%x|%X|%o|%c|%s|%.3s|%%", 42, 42, 42, 0xbeef, 0xbeef, 8, 'Z', "str", "abcdef");
    printf("%s\n", buf);
    strcpy(buf, "hello world");
    rev(buf);
    printf("%s %d\n", buf, (int)strlen(buf));
    char *p = malloc(1000);
    for (int i = 0; i < 1000; i++) p[i] = (char)(i * 7);
    memmove(p + 10, p, 500);
    unsigned sum = 0;
    for (int i = 0; i < 1000; i++) sum = sum * 33 + (unsigned char)p[i];
    printf("memmove %u\n", sum);
    free(p);
    printf("strcmp %d %d %d\n", strcmp("abc", "abd") < 0, strncmp("abcx", "abcy", 3), strchr("find me", 'm') - "find me");
    return 0;
}
