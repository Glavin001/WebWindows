/*
 * Entry points into win32u's Unix side, for the host (runtime/wine/unix.mjs).
 *
 * user32 and gdi32 reach win32u through system calls numbered from 0x1000;
 * the host passes the number and the caller's arguments (its stack slots on
 * i386, a copy of the registers and stack slots on x86_64), and the
 * generated thunks (gen-syscalls.py) call each function with its own
 * signature. win32u.dll's DllMain makes one Unix call, init, which
 * registers the table with ntdll; here the table is static, so registering
 * it only records that win32u started.
 */

#include "config.h"
#include <stdarg.h>

#include "ntstatus.h"
#define WIN32_NO_STATUS
#include "windef.h"
#include "winnt.h"
#include "winternl.h"
#include "wine/unixlib.h"

#include <emscripten.h>

extern const unsigned int win32u_syscall_first, win32u_syscall_count;
extern ULONG_PTR (*const win32u_syscall_thunks[])( const ULONG_PTR * );
extern const unixlib_entry_t win32u_unix_call_funcs[];

static BOOL win32u_started;

EMSCRIPTEN_KEEPALIVE ULONG_PTR wasm_win32u_syscall( unsigned int id, const ULONG_PTR *args )
{
    unsigned int index = id - win32u_syscall_first;
    if (index >= win32u_syscall_count) return STATUS_INVALID_SYSTEM_SERVICE;
    return win32u_syscall_thunks[index]( args );
}

EMSCRIPTEN_KEEPALIVE NTSTATUS wasm_win32u_unix_call( unsigned int code, void *args )
{
    return win32u_unix_call_funcs[code]( args );
}

BOOLEAN KeAddSystemServiceTable( ULONG_PTR *funcs, ULONG_PTR *counters, ULONG limit,
                                 BYTE *arguments, ULONG index )
{
    win32u_started = TRUE;
    return TRUE;
}

void ntdll_add_syscall_debug_info( UINT idx, const char **syscall_names, const char **usercall_names )
{
}

