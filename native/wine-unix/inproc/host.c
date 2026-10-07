/*
 * What Wine's Unix-side libraries import from ntdll's Unix side, provided
 * by the host instead: virtual memory, image sections, files and process
 * information stay in the JavaScript host (runtime/wine/syscalls.mjs),
 * which implements them for translated code already. Each call here
 * forwards its arguments, as the 32-bit values they are on wasm32, to the
 * host's implementation of the same NT call. Pointers need no translation:
 * the module and the guest share one address space.
 *
 * KeUserModeCallback (win32u calling back into user32, e.g. to run a window
 * procedure) goes to the host too, which runs the guest's
 * KiUserCallbackDispatcher until it returns with NtCallbackReturn.
 */

#include "config.h"

#include <pthread.h>
#include <stdint.h>
#include <setjmp.h>
#include <stdarg.h>
#include <string.h>
#include <stdlib.h>

#include "ntstatus.h"
#define WIN32_NO_STATUS
#include "unix_private.h"
#include "ntuser.h"

#include <emscripten.h>

/* The module lives above 2 GB, so pointers reach JavaScript as negative
 * 32-bit integers: each one is made unsigned (>>> 0). */
EM_JS( NTSTATUS, host_nt_call, (const char *name, const ULONG *args, int count), {
    const view = new Uint32Array(HEAPU8.buffer, args >>> 0, count);
    return Module.hostNtCall(UTF8ToString(name >>> 0), Array.from(view));
});

EM_JS( NTSTATUS, host_user_callback, (ULONG id, const void *args, ULONG len, void **ret_ptr, ULONG *ret_len), {
    return Module.hostUserCallback(id >>> 0, args >>> 0, len >>> 0, ret_ptr >>> 0, ret_len >>> 0);
});

#define A(x) ((ULONG)(ULONG_PTR)(x))
#define FORWARD(name, ...) \
    do { const ULONG args[] = { __VA_ARGS__ }; return host_nt_call( #name, args, ARRAY_SIZE(args) ); } while (0)

/* ---- Drive Z: (the module's own file system, e.g. Wine's fonts) --------
 * Directories on Z: are listed here; their handles are numbered from
 * Z_HANDLE_BASE, apart from wineserver's and the host's. */
#include <dirent.h>
#include <sys/stat.h>

#define Z_HANDLE_BASE 0x7f000000
#define Z_MAX_HANDLES 64
static DIR *z_dirs[Z_MAX_HANDLES];

static BOOL is_z_path( const UNICODE_STRING *name )
{
    const WCHAR *p = name->Buffer;
    unsigned int len = name->Length / sizeof(WCHAR);
    if (len >= 4 && p[0] == '\\' && p[1] == '?' && p[2] == '?' && p[3] == '\\') { p += 4; len -= 4; }
    return len >= 2 && (p[0] == 'Z' || p[0] == 'z') && p[1] == ':';
}

BOOL z_close( HANDLE handle )
{
    ULONG_PTR i = (ULONG_PTR)handle - Z_HANDLE_BASE;
    if (i >= Z_MAX_HANDLES || !z_dirs[i]) return FALSE;
    closedir( z_dirs[i] );
    z_dirs[i] = NULL;
    return TRUE;
}

static NTSTATUS z_open_dir( HANDLE *handle, OBJECT_ATTRIBUTES *attr, IO_STATUS_BLOCK *io )
{
    WCHAR name[MAX_PATH];
    unsigned int len = min( attr->ObjectName->Length / sizeof(WCHAR), MAX_PATH - 1 ), i;
    char *unix_name;
    NTSTATUS status;

    memcpy( name, attr->ObjectName->Buffer, len * sizeof(WCHAR) );
    name[len] = 0;
    if ((status = ntdll_get_unix_file_name( name, &unix_name, FILE_OPEN ))) return status;
    for (i = 0; i < Z_MAX_HANDLES && z_dirs[i]; i++) ;
    if (i == Z_MAX_HANDLES) status = STATUS_TOO_MANY_OPENED_FILES;
    else if (!(z_dirs[i] = opendir( unix_name ))) status = STATUS_OBJECT_NAME_NOT_FOUND;
    else
    {
        *handle = (HANDLE)(ULONG_PTR)(Z_HANDLE_BASE + i);
        io->Status = STATUS_SUCCESS;
        io->Information = FILE_OPENED;
    }
    free( unix_name );
    return status;
}

/* One FILE_BOTH_DIR_INFORMATION entry per call is enough for the callers. */
static NTSTATUS z_query_dir( HANDLE handle, IO_STATUS_BLOCK *io, void *buffer, ULONG length )
{
    DIR *dir = z_dirs[(ULONG_PTR)handle - Z_HANDLE_BASE];
    FILE_BOTH_DIR_INFORMATION *info = buffer;
    struct dirent *de;
    unsigned int i, len;

    while ((de = readdir( dir )) && (!strcmp( de->d_name, "." ) || !strcmp( de->d_name, ".." ))) ;
    if (!de) return STATUS_NO_MORE_FILES;
    len = strlen( de->d_name );
    if (offsetof( FILE_BOTH_DIR_INFORMATION, FileName[len] ) > length) return STATUS_BUFFER_OVERFLOW;
    memset( info, 0, sizeof(*info) );
    info->FileAttributes = de->d_type == DT_DIR ? FILE_ATTRIBUTE_DIRECTORY : FILE_ATTRIBUTE_NORMAL;
    info->FileNameLength = len * sizeof(WCHAR);
    for (i = 0; i < len; i++) info->FileName[i] = (unsigned char)de->d_name[i];
    io->Status = STATUS_SUCCESS;
    io->Information = offsetof( FILE_BOTH_DIR_INFORMATION, FileName[len] );
    return STATUS_SUCCESS;
}

NTSTATUS WINAPI NtAllocateVirtualMemory( HANDLE process, PVOID *ret, ULONG_PTR zero_bits, SIZE_T *size_ptr,
                                         ULONG type, ULONG protect )
{
    FORWARD( NtAllocateVirtualMemory, A(process), A(ret), A(zero_bits), A(size_ptr), type, protect );
}

NTSTATUS WINAPI NtFreeVirtualMemory( HANDLE process, PVOID *addr_ptr, SIZE_T *size_ptr, ULONG type )
{
    FORWARD( NtFreeVirtualMemory, A(process), A(addr_ptr), A(size_ptr), type );
}

extern const void *wasm_server_map_view( unsigned int handle, unsigned long long offset, unsigned int *size );

/* The host numbers its own handles from 0x40000 (runtime/wine/host.mjs);
 * smaller ones come from wineserver. */
static BOOL is_server_handle( HANDLE handle )
{
    return (ULONG_PTR)handle < 0x40000;
}

NTSTATUS WINAPI NtMapViewOfSection( HANDLE handle, HANDLE process, PVOID *addr_ptr, ULONG_PTR zero_bits,
                                    SIZE_T commit_size, const LARGE_INTEGER *offset_ptr, SIZE_T *size_ptr,
                                    SECTION_INHERIT inherit, ULONG alloc_type, ULONG protect )
{
    if (is_server_handle( handle ))
    {
        unsigned int size;
        const void *data = wasm_server_map_view( HandleToULong( handle ), offset_ptr ? offset_ptr->QuadPart : 0, &size );
        if (!data) return STATUS_NOT_SUPPORTED;
        *addr_ptr = (void *)data;
        *size_ptr = size;
        return STATUS_SUCCESS;
    }
    FORWARD( NtMapViewOfSection, A(handle), A(process), A(addr_ptr), A(zero_bits), A(commit_size),
             A(offset_ptr), A(size_ptr), inherit, alloc_type, protect );
}

NTSTATUS WINAPI NtUnmapViewOfSection( HANDLE process, PVOID addr )
{
    /* Views of server memory (above the guest region) are not mappings. */
    if ((ULONG_PTR)addr >= 0x80000000) return STATUS_SUCCESS;
    FORWARD( NtUnmapViewOfSection, A(process), A(addr) );
}

NTSTATUS WINAPI NtOpenFile( HANDLE *handle, ACCESS_MASK access, OBJECT_ATTRIBUTES *attr, IO_STATUS_BLOCK *io,
                            ULONG sharing, ULONG options )
{
    if (attr && attr->ObjectName && is_z_path( attr->ObjectName )) return z_open_dir( handle, attr, io );
    FORWARD( NtOpenFile, A(handle), access, A(attr), A(io), sharing, options );
}

static NTSTATUS z_query_attributes( const OBJECT_ATTRIBUTES *attr, FILE_NETWORK_OPEN_INFORMATION *info )
{
    WCHAR name[MAX_PATH];
    unsigned int len = min( attr->ObjectName->Length / sizeof(WCHAR), MAX_PATH - 1 );
    struct stat st;
    char *unix_name;
    NTSTATUS status;
    LONGLONG time;

    memcpy( name, attr->ObjectName->Buffer, len * sizeof(WCHAR) );
    name[len] = 0;
    if ((status = ntdll_get_unix_file_name( name, &unix_name, FILE_OPEN ))) return status;
    if (stat( unix_name, &st ) == -1) status = STATUS_OBJECT_NAME_NOT_FOUND;
    else
    {
        /* Unix seconds to FILETIME (100 ns since 1601). */
        time = (LONGLONG)st.st_mtime * 10000000 + 116444736000000000LL;
        info->CreationTime.QuadPart = info->LastAccessTime.QuadPart = time;
        info->LastWriteTime.QuadPart = info->ChangeTime.QuadPart = time;
        info->EndOfFile.QuadPart = st.st_size;
        info->AllocationSize.QuadPart = (st.st_size + 4095) & ~4095;
        info->FileAttributes = S_ISDIR( st.st_mode ) ? FILE_ATTRIBUTE_DIRECTORY : FILE_ATTRIBUTE_ARCHIVE;
    }
    free( unix_name );
    return status;
}

NTSTATUS WINAPI NtQueryFullAttributesFile( const OBJECT_ATTRIBUTES *attr, FILE_NETWORK_OPEN_INFORMATION *info )
{
    if (attr && attr->ObjectName && is_z_path( attr->ObjectName )) return z_query_attributes( attr, info );
    FORWARD( NtQueryFullAttributesFile, A(attr), A(info) );
}

NTSTATUS WINAPI NtQueryDirectoryFile( HANDLE handle, HANDLE event, PIO_APC_ROUTINE apc_routine, void *apc_context,
                                      IO_STATUS_BLOCK *io, void *buffer, ULONG length,
                                      FILE_INFORMATION_CLASS info_class, BOOLEAN single_entry,
                                      UNICODE_STRING *mask, BOOLEAN restart_scan )
{
    if ((ULONG_PTR)handle - Z_HANDLE_BASE < Z_MAX_HANDLES) return z_query_dir( handle, io, buffer, length );
    FORWARD( NtQueryDirectoryFile, A(handle), A(event), A(apc_routine), A(apc_context), A(io), A(buffer), length,
             info_class, single_entry, A(mask), restart_scan );
}

NTSTATUS WINAPI NtDeviceIoControlFile( HANDLE handle, HANDLE event, PIO_APC_ROUTINE apc, void *apc_context,
                                       IO_STATUS_BLOCK *io, ULONG code, void *in_buffer, ULONG in_size,
                                       void *out_buffer, ULONG out_size )
{
    FORWARD( NtDeviceIoControlFile, A(handle), A(event), A(apc), A(apc_context), A(io), code, A(in_buffer),
             in_size, A(out_buffer), out_size );
}

NTSTATUS WINAPI NtQuerySystemInformation( SYSTEM_INFORMATION_CLASS class, void *info, ULONG size, ULONG *ret_size )
{
    FORWARD( NtQuerySystemInformation, class, A(info), size, A(ret_size) );
}

NTSTATUS WINAPI NtQueryObject( HANDLE handle, OBJECT_INFORMATION_CLASS info_class, void *ptr, ULONG len,
                               ULONG *used_len )
{
    FORWARD( NtQueryObject, A(handle), info_class, A(ptr), len, A(used_len) );
}

NTSTATUS WINAPI NtGetContextThread( HANDLE handle, CONTEXT *context )
{
    FORWARD( NtGetContextThread, A(handle), A(context) );
}

NTSTATUS WINAPI NtRaiseException( EXCEPTION_RECORD *rec, CONTEXT *context, BOOL first_chance )
{
    FORWARD( NtRaiseException, A(rec), A(context), first_chance );
}

NTSTATUS WINAPI NtResumeThread( HANDLE handle, ULONG *count )
{
    FORWARD( NtResumeThread, A(handle), A(count) );
}

/* win32u starts explorer.exe for the desktop window in Wine; one process
 * per page has no child processes yet (the desktop is created in-process,
 * see the browser driver). */
NTSTATUS WINAPI NtCreateUserProcess( HANDLE *process_handle_ptr, HANDLE *thread_handle_ptr,
                                     ACCESS_MASK process_access, ACCESS_MASK thread_access,
                                     OBJECT_ATTRIBUTES *process_attr, OBJECT_ATTRIBUTES *thread_attr,
                                     ULONG process_flags, ULONG thread_flags,
                                     RTL_USER_PROCESS_PARAMETERS *params, PS_CREATE_INFO *info,
                                     PS_ATTRIBUTE_LIST *ps_attr )
{
    return STATUS_NOT_SUPPORTED;
}

NTSTATUS KeUserModeCallback( ULONG id, const void *args, ULONG len, void **ret_ptr, ULONG *ret_len )
{
    return host_user_callback( id, args, len, ret_ptr, ret_len );
}

/* Exceptions in user callbacks: win32u brackets some calls with a jump
 * buffer to recover from faults; faults stop the program for now (M5). */
void ntdll_set_exception_jmp_buf( jmp_buf jmp )
{
}

void wine_server_send_fd( int fd )
{
}

/* Data files (fonts, NLS) live under /wine/share/wine in the in-memory file
 * system; there is no build tree. */
const char *data_dir = "/wine/share/wine";
const char *build_dir = NULL;
ULONG_PTR user_space_wow_limit = 0x7fffffff;

/* One thread per module until M5: locks have nothing to exclude. */
int pthread_mutex_init( pthread_mutex_t *m, const pthread_mutexattr_t *a ) { return 0; }
int pthread_mutex_destroy( pthread_mutex_t *m ) { return 0; }
int pthread_mutex_lock( pthread_mutex_t *m ) { return 0; }
int pthread_mutex_unlock( pthread_mutex_t *m ) { return 0; }
int pthread_mutexattr_init( pthread_mutexattr_t *a ) { return 0; }
int pthread_mutexattr_destroy( pthread_mutexattr_t *a ) { return 0; }
int pthread_mutexattr_settype( pthread_mutexattr_t *a, int type ) { return 0; }

int pthread_once( pthread_once_t *once, void (*func)(void) )
{
    if (!*(volatile int *)once)
    {
        *(volatile int *)once = 1;
        func();
    }
    return 0;
}

/* Paths between the module's file system (fonts and other data, under
 * /wine) and Windows names: as in Wine, the Unix root is drive Z:. Other
 * drives are the host's (runtime/wine) and have no Unix name here. */
NTSTATUS ntdll_get_dos_file_name( const char *unix_name, WCHAR **dos, UINT disposition )
{
    size_t i, len = strlen( unix_name );
    WCHAR *buffer = malloc( (len + 3) * sizeof(WCHAR) );

    *dos = NULL;
    if (!buffer) return STATUS_NO_MEMORY;
    buffer[0] = 'Z';
    buffer[1] = ':';
    for (i = 0; i <= len; i++) buffer[i + 2] = unix_name[i] == '/' ? '\\' : (unsigned char)unix_name[i];
    *dos = buffer;
    return STATUS_SUCCESS;
}

NTSTATUS ntdll_get_unix_file_name( const WCHAR *dos, char **unix_name, UINT disposition )
{
    size_t i, len;
    char *buffer;

    /* WCHAR is 16-bit; wchar_t (and L"", wcslen) are 32-bit here. */
    *unix_name = NULL;
    if (dos[0] == '\\' && dos[1] == '?' && dos[2] == '?' && dos[3] == '\\') dos += 4;
    if ((dos[0] != 'Z' && dos[0] != 'z') || dos[1] != ':') return STATUS_OBJECT_PATH_NOT_FOUND;
    dos += 2;
    for (len = 0; dos[len]; len++) ;
    if (!(buffer = malloc( len + 1 ))) return STATUS_NO_MEMORY;
    for (i = 0; i <= len; i++) buffer[i] = dos[i] == '\\' ? '/' : (char)dos[i];
    *unix_name = buffer;
    return STATUS_SUCCESS;
}

/* libc's thread pointer: the module runs one thread (M5 brings more), and
 * the wasm-workers libc that shared memory selects leaves this to the
 * threading runtime, which is not linked. Code that asks (dlerror's buffer)
 * gets one zeroed thread block. */
uintptr_t __get_tp( void )
{
    static uint64_t thread_block[64];
    return (uintptr_t)thread_block;
}
