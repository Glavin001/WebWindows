/* A program with two DLLs of its own in its folder, at the same preferred
   base, as a game's are (tests/web/picker.mjs). */
#include <stdio.h>

__declspec(dllimport) int a_value(void);
__declspec(dllimport) int b_value(void);

int main(void) {
  printf("dlls: %d\n", a_value() + b_value());
  return 0;
}
