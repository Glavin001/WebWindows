/* Prints data.txt from the current directory: checks that a program run
   from a chosen folder sees the folder's other files (M3 picker test). */
#include <stdio.h>

int main(void)
{
    char line[256];
    FILE *f = fopen("data.txt", "r");
    if (!f) {
        printf("cannot open data.txt\n");
        return 1;
    }
    while (fgets(line, sizeof line, f)) printf("data: %s", line);
    fclose(f);
    return 0;
}
