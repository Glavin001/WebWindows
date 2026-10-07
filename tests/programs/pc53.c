/* Linked into the native reference builds: sets x87 precision control to
   53 bits (double), matching the translator's f64 registers. */
__attribute__((constructor)) static void pc53(void) {
    unsigned short cw;
    __asm__ volatile("fnstcw %0" : "=m"(cw));
    cw = (cw & ~0x300) | 0x200;
    __asm__ volatile("fldcw %0" : : "m"(cw));
}
