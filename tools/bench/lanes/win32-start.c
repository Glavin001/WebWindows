/* Entry point without a C runtime: main, then ExitProcess. */
__declspec(dllimport) void __stdcall ExitProcess(unsigned int);
int main(void);
void start(void) { ExitProcess(main()); }
void *memset(void *d, int c, unsigned long n) { unsigned char *p = d; while (n--) *p++ = (unsigned char)c; return d; }
void *memcpy(void *d, const void *s, unsigned long n) { unsigned char *p = d; const unsigned char *q = s; while (n--) *p++ = *q++; return d; }
void __main(void) {}
