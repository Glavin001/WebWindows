/* One of two DLLs linked at the same base (tests/web/picker.mjs): the
   loader moves one of them, and the page caches both translations. */
__declspec(dllexport) int a_value(void) { return 40; }
