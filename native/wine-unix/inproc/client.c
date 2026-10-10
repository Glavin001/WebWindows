/*
 * The client side of the in-process wineserver: what ntdll's Unix side
 * (dlls/ntdll/unix/server.c) does over the server socket, done with direct
 * calls into the server linked into this module (see server.c).
 *
 * Wine's own sync.c and registry.c are compiled unchanged on top of this:
 * they build requests with SERVER_START_REQ and call wine_server_call and
 * server_select, which are defined here.
 */

#include "config.h"

#include <errno.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/mman.h>
#include <sys/time.h>

#include "ntstatus.h"
#define WIN32_NO_STATUS
#include "unix_private.h"
#include "wine/debug.h"

#include <emscripten.h>

extern unsigned int wasm_server_call( void *req_ptr );
extern int wasm_server_new_process(void);
extern int wasm_server_start(void);
extern int wasm_server_run(void);
extern int wasm_server_client_reply_fd(void);
extern int wasm_server_client_wait_fd(void);

extern void wasm_process_input(void);
extern void wasm_set_nt_data_dir( const WCHAR *dir );
extern void wasm_init_case_tables(void);
extern void browser_driver_init(void);

/* Blocks the thread for up to `ms` milliseconds (-1: until something
 * happens) while nothing is runnable. Returns 1 when the host has input to
 * deliver, 0 when the time passed, -1 when nothing can ever arrive. */
EM_JS( int, host_wait, (int ms), {
    return Module.hostWait ? Module.hostWait(ms) : -1;
});

extern void *wasm_server_get_thread(void);
extern void wasm_server_set_thread( void *thread );
extern int wasm_server_send_request_fd( int *write_end );
extern void *wasm_server_thread_setup( unsigned int tid, int *wait_fd );
extern void wasm_server_thread_ended( void *thread );

/* Whether the page queued keyboard or mouse input. */
EM_JS( int, host_has_input, (void), {
    return Module.display?.hasInput() ? 1 : 0;
});

static TEB *current_teb;

/* Input reaches Wine in the waits below. A program that polls for messages
 * instead (a game's PeekMessage-then-render loop) never waits, but win32u
 * yields the thread whenever such a poll finds nothing (PeekMessage, and
 * MsgWaitForMultipleObjects with no timeout, through NtYieldExecution), so
 * input is delivered there too. This replaces libc's sched_yield, which
 * nothing else in the module calls. */
int sched_yield(void)
{
    if (host_has_input()) wasm_process_input();
    return 0;
}

TEB * WINAPI NtCurrentTeb(void)
{
    return current_teb;
}

/* Threads (Milestone 5): the host runs Windows threads one at a time and
 * switches between them; each has its TEB and its server thread. */
static struct inproc_thread
{
    TEB  *teb;
    void *server;       /* struct thread in the server */
    int   request_end;  /* write end of its request pipe */
} threads[256];

static struct inproc_thread *find_thread( TEB *teb )
{
    unsigned int i;
    for (i = 0; i < ARRAY_SIZE(threads); i++) if (threads[i].teb == teb) return &threads[i];
    return NULL;
}

/* Makes `teb`'s thread the current one: NtCurrentTeb() and the thread
 * server requests come from. */
EMSCRIPTEN_KEEPALIVE void wasm_switch_thread( TEB *teb )
{
    struct inproc_thread *t = find_thread( teb );
    current_teb = teb;
    if (t) wasm_server_set_thread( t->server );
}

/* NtCreateThreadEx's server part, called on the creating thread: the host
 * built the new thread's TEB and stack. The new thread introduces itself to
 * the server (init_thread) as start_thread does. */
EMSCRIPTEN_KEEPALIVE unsigned int wasm_create_thread( TEB *teb, unsigned int flags, unsigned int access,
                                                       HANDLE *handle, unsigned int *tid_ret )
{
    struct inproc_thread *t = find_thread( NULL );
    struct ntdll_thread_data *data = (struct ntdll_thread_data *)&teb->GdiTebBatch;
    TEB *parent = current_teb;
    unsigned int ret, tid = 0;
    int request_fd, request_end, wait;

    if (!t) return STATUS_TOO_MANY_THREADS;
    if ((request_fd = wasm_server_send_request_fd( &request_end )) == -1) return STATUS_TOO_MANY_OPENED_FILES;
    SERVER_START_REQ( new_thread )
    {
        req->process    = wine_server_obj_handle( NtCurrentProcess() );
        req->access     = access ? access : THREAD_ALL_ACCESS;
        req->flags      = flags;
        req->request_fd = request_fd;
        if (!(ret = wine_server_call( req )))
        {
            *handle = wine_server_ptr_handle( reply->handle );
            tid = reply->tid;
        }
    }
    SERVER_END_REQ;
    if (ret)
    {
        close( request_end );
        return ret;
    }

    t->teb = teb;
    t->request_end = request_end;
    if (!(t->server = wasm_server_thread_setup( tid, &wait ))) return STATUS_INTERNAL_ERROR;
    data->request_fd = -1;
    data->reply_fd   = wasm_server_client_reply_fd();
    data->wait_fd[0] = wait;
    data->wait_fd[1] = wasm_server_client_wait_fd();
    teb->ClientId.UniqueProcess = parent->ClientId.UniqueProcess;
    teb->ClientId.UniqueThread  = ULongToHandle( tid );
    *tid_ret = tid;

    wasm_switch_thread( teb );
    SERVER_START_REQ( init_thread )
    {
        req->unix_tid = tid;
        req->teb      = wine_server_client_ptr( teb );
        req->entry    = 0;
        req->reply_fd = wasm_server_client_reply_fd();
        req->wait_fd  = wasm_server_client_wait_fd();
        ret = wine_server_call( req );
    }
    SERVER_END_REQ;
    wasm_switch_thread( parent );
    return ret;
}

/* The current thread ends (NtTerminateThread on itself): the server marks
 * it terminated with `status`. The host switches to another thread before
 * the next request. */
EMSCRIPTEN_KEEPALIVE void wasm_exit_thread( unsigned int status )
{
    struct inproc_thread *t = find_thread( current_teb );

    SERVER_START_REQ( terminate_thread )
    {
        req->handle    = wine_server_obj_handle( GetCurrentThread() );
        req->exit_code = status;
        wine_server_call( req );
    }
    SERVER_END_REQ;
    if (!t) return;
    wasm_server_thread_ended( t->server );
    close( t->request_end );
    close( ntdll_get_thread_data()->wait_fd[0] );
    memset( t, 0, sizeof(*t) );
}

/* Another thread was terminated (the server already marked it). */
EMSCRIPTEN_KEEPALIVE void wasm_forget_thread( TEB *teb )
{
    struct inproc_thread *t = find_thread( teb );

    if (!t) return;
    wasm_server_thread_ended( t->server );
    close( t->request_end );
    close( ((struct ntdll_thread_data *)&teb->GdiTebBatch)->wait_fd[0] );
    memset( t, 0, sizeof(*t) );
}

/* Server requests the host needs for threads it schedules. */
EMSCRIPTEN_KEEPALIVE unsigned int wasm_thread_info( HANDLE handle, unsigned int *tid, unsigned int *exit_code,
                                                    unsigned int *teb )
{
    unsigned int ret;

    SERVER_START_REQ( get_thread_info )
    {
        req->handle = wine_server_obj_handle( handle );
        req->access = THREAD_QUERY_LIMITED_INFORMATION;
        if (!(ret = wine_server_call( req )))
        {
            *tid = reply->tid;
            *exit_code = reply->exit_code;
            *teb = (unsigned int)reply->teb;
        }
    }
    SERVER_END_REQ;
    return ret;
}

EMSCRIPTEN_KEEPALIVE unsigned int wasm_terminate_thread( HANDLE handle, unsigned int status )
{
    unsigned int ret;

    SERVER_START_REQ( terminate_thread )
    {
        req->handle    = wine_server_obj_handle( handle );
        req->exit_code = status;
        ret = wine_server_call( req );
    }
    SERVER_END_REQ;
    return ret;
}

/* NtSuspendThread / NtResumeThread: the new count, or -1 with the status
 * in `status`. */
EMSCRIPTEN_KEEPALIVE int wasm_suspend_thread( HANDLE handle, int resume, unsigned int *status )
{
    int count = -1;

    if (resume)
    {
        SERVER_START_REQ( resume_thread )
        {
            req->handle = wine_server_obj_handle( handle );
            if (!(*status = wine_server_call( req ))) count = reply->count;
        }
        SERVER_END_REQ;
    }
    else
    {
        SERVER_START_REQ( suspend_thread )
        {
            req->handle = wine_server_obj_handle( handle );
            if (!(*status = wine_server_call( req ))) count = reply->count;
        }
        SERVER_END_REQ;
    }
    return count;
}

/* Sleep (NtDelayExecution): blocks until the time given has passed, or
 * for good when `tv` is NULL. Input that arrives meanwhile goes to the
 * server's queues (it waits there for the program, as on Windows). */
int wasm_sleep( const struct timeval *tv )
{
    double end = tv ? emscripten_get_now() + tv->tv_sec * 1000.0 + tv->tv_usec / 1000.0 : 0;

    for (;;)
    {
        int ms = -1, woke;

        if (tv)
        {
            double left = end - emscripten_get_now();
            if (left <= 0) return 0;
            ms = (int)left + 1;
        }
        woke = host_wait( ms );
        if (woke > 0) wasm_process_input();
        else if (woke < 0 && ms < 0)
        {
            fprintf( stderr, "wine: the thread sleeps forever and nothing can wake it\n" );
            abort();
        }
    }
}

unsigned int server_call_unlocked( void *req_ptr )
{
    return wasm_server_call( req_ptr );
}

unsigned int CDECL wine_server_call( void *req_ptr )
{
    return wasm_server_call( req_ptr );
}

/* Waits for the server to wake the thread for `cookie`: runs the server's
 * timers and lets the host block (or take input) until the wakeup arrives. */
static int wait_select_reply( void *cookie )
{
    struct wake_up_reply reply;
    int ms = -1, timers_ran = 0;

    for (;;)
    {
        int wait_fd = ntdll_get_thread_data()->wait_fd[0];
        int ret = read( wait_fd, &reply, sizeof(reply) ), woke;
        if (ret == sizeof(reply))
        {
            if (!reply.cookie) return STATUS_THREAD_IS_TERMINATING;
            if (wine_server_get_ptr( reply.cookie ) == cookie) return reply.signaled;
            continue;  /* a stale wakeup for an earlier wait */
        }
        if (ret >= 0 || errno != EAGAIN) return STATUS_INTERNAL_ERROR;
        /* Run the server's timers, then look again before blocking: one of
         * them may be this wait's own timeout. A poll (a timeout of "now",
         * as DirectInput's check for events every time a game reads a
         * device) is woken that way, and blocking until the next timer
         * after it cost a millisecond each time. */
        if (!timers_ran)
        {
            ms = wasm_server_run();
            timers_ran = 1;
            continue;
        }
        timers_ran = 0;
        woke = host_wait( ms );
        if (woke > 0) wasm_process_input();  /* the page queued keyboard or mouse input */
        else if (woke < 0 && ms < 0)
        {
            fprintf( stderr, "wine: a wait can never be satisfied (no timer, no input)\n" );
            return STATUS_INTERNAL_ERROR;
        }
    }
}

unsigned int server_select( const union select_op *select_op, data_size_t size, UINT flags,
                            timeout_t abs_timeout, struct context_data *context, struct user_apc *user_apc )
{
    unsigned int ret;
    int cookie, signaled;
    obj_handle_t apc_handle = 0;
    union apc_result result;
    union apc_call call;

    memset( &result, 0, sizeof(result) );
    do
    {
        for (;;)
        {
            SERVER_START_REQ( select )
            {
                req->flags    = flags;
                req->cookie   = wine_server_client_ptr( &cookie );
                req->prev_apc = apc_handle;
                req->timeout  = abs_timeout;
                req->size     = size;
                wine_server_add_data( req, &result, sizeof(result) );
                wine_server_add_data( req, select_op, size );
                wine_server_set_reply( req, &call, sizeof(call) );
                ret = server_call_unlocked( req );
                signaled   = reply->signaled;
                apc_handle = reply->apc_handle;
            }
            SERVER_END_REQ;
            /* System APCs (asynchronous I/O completion and the like) arrive
             * with threads and asynchronous files (M5); none are queued yet. */
            if (ret != STATUS_KERNEL_APC) break;
            result.type = APC_NONE;
        }
        if (signaled) break;
        ret = wait_select_reply( &cookie );
    }
    while (ret == STATUS_KERNEL_APC);

    if (ret == STATUS_USER_APC && user_apc) *user_apc = call.user;
    return ret;
}

unsigned int server_wait( const union select_op *select_op, data_size_t size, UINT flags,
                          const LARGE_INTEGER *timeout )
{
    timeout_t abs_timeout = timeout ? timeout->QuadPart : TIMEOUT_INFINITE;
    struct user_apc apc;
    unsigned int ret;

    if (abs_timeout < 0)
    {
        LARGE_INTEGER now;
        NtQueryPerformanceCounter( &now, NULL );
        abs_timeout -= now.QuadPart;
    }
    ret = server_select( select_op, size, flags, abs_timeout, NULL, &apc );
    /* User APCs are delivered by the host when it runs guest code (M5). */
    if (ret == STATUS_USER_APC) ret = STATUS_USER_APC;
    return ret;
}

unsigned int server_wait_for_object( HANDLE handle, BOOL alertable, const LARGE_INTEGER *timeout )
{
    union select_op select_op;
    UINT flags = SELECT_INTERRUPTIBLE;

    if (alertable) flags |= SELECT_ALERTABLE;
    select_op.wait.op = SELECT_WAIT;
    select_op.wait.handles[0] = wine_server_obj_handle( handle );
    return server_wait( &select_op, offsetof( union select_op, wait.handles[1] ), flags, timeout );
}

extern BOOL z_close( HANDLE handle );

NTSTATUS WINAPI NtClose( HANDLE handle )
{
    unsigned int ret;

    if (z_close( handle )) return STATUS_SUCCESS;  /* a directory on drive Z: (host.c) */

    SERVER_START_REQ( close_handle )
    {
        req->handle = wine_server_obj_handle( handle );
        ret = wine_server_call( req );
    }
    SERVER_END_REQ;
    return ret;
}

NTSTATUS WINAPI NtDuplicateObject( HANDLE source_process, HANDLE source, HANDLE dest_process, HANDLE *dest,
                                   ACCESS_MASK access, ULONG attributes, ULONG options )
{
    unsigned int ret;

    if (dest) *dest = 0;
    SERVER_START_REQ( dup_handle )
    {
        req->src_process = wine_server_obj_handle( source_process );
        req->src_handle  = wine_server_obj_handle( source );
        req->dst_process = wine_server_obj_handle( dest_process );
        req->access      = access;
        req->attributes  = attributes;
        req->options     = options;
        if (!(ret = wine_server_call( req )) && dest) *dest = wine_server_ptr_handle( reply->handle );
    }
    SERVER_END_REQ;
    return ret;
}

/* Registry files are loaded by the server itself; loading a hive from a
 * guest file (NtLoadKey) is not supported yet. */
NTSTATUS get_nt_and_unix_names( OBJECT_ATTRIBUTES *attr, UNICODE_STRING *nt_name, char **unix_name_ret,
                                UINT disposition, BOOL open_reparse )
{
    return STATUS_NOT_SUPPORTED;
}

NTSTATUS open_unix_file( HANDLE *handle, const char *unix_name, ACCESS_MASK access,
                         OBJECT_ATTRIBUTES *attr, ULONG attributes, ULONG sharing, ULONG disposition,
                         ULONG options, void *ea_buffer, ULONG ea_length )
{
    return STATUS_NOT_SUPPORTED;
}

/* Debug output goes to stderr: errors always, everything else when the
 * host turns tracing on (wasm_set_trace). A header returning -1 tells
 * wine_dbg_log to drop the message. */
static int trace_all;
static char trace_channels[512];  /* ",name,name," */

/* Like WINEDEBUG: "all" or a comma-separated list of channel names. */
EMSCRIPTEN_KEEPALIVE void wasm_set_trace( const char *channels )
{
    trace_all = channels && !strcmp( channels, "all" );
    snprintf( trace_channels, sizeof(trace_channels), ",%s,", channels ? channels : "" );
}

static int channel_traced( const struct __wine_debug_channel *channel )
{
    char name[sizeof(channel->name) + 3];
    if (trace_all) return 1;
    if (!channel) return 0;
    snprintf( name, sizeof(name), ",%s,", channel->name );
    return strstr( trace_channels, name ) != NULL;
}

int __wine_dbg_header( enum __wine_debug_class cls, struct __wine_debug_channel *channel,
                       const char *function )
{
    static const char * const classes[] = { "fixme", "err", "warn", "trace" };
    if (cls != __WINE_DBCL_ERR && !channel_traced( channel )) return -1;
    return fprintf( stderr, "%s:%s:%s ", classes[cls], channel ? channel->name : "", function ? function : "" );
}

int __wine_dbg_output( const char *str )
{
    return fputs( str, stderr );
}

const char * __cdecl __wine_dbg_strdup( const char *str )
{
    static char buffers[8][512];
    static int next;
    char *ret = buffers[next++ & 7];
    snprintf( ret, sizeof(buffers[0]), "%s", str );
    return ret;
}

/* Process start: the server, then this process and its first thread. The
 * host passes the guest TEB and PEB it built (runtime/wine/host.mjs). */
EMSCRIPTEN_KEEPALIVE unsigned int wasm_init_process( TEB *teb, PEB *peb, unsigned int unix_pid )
{
    struct ntdll_thread_data *data;
    unsigned int ret;
    USHORT machines[8];

    current_teb = teb;
    threads[0].teb = teb;
    {
        /* Wine's data (fonts, NLS) on drive Z:, as ntdll's start-up would set it. */
        static const WCHAR data_dir[] = {'\\','?','?','\\','Z',':','\\','w','i','n','e','\\','s','h','a','r','e',
                                         '\\','w','i','n','e',0};
        wasm_set_nt_data_dir( data_dir );
    }
    wasm_init_case_tables();
    browser_driver_init();
    wasm_server_start();
    data = ntdll_get_thread_data();
    data->wait_fd[0] = wasm_server_new_process();
    threads[0].server = wasm_server_get_thread();
    threads[0].request_end = -1;
    data->wait_fd[1] = wasm_server_client_wait_fd();
    data->reply_fd = wasm_server_client_reply_fd();
    data->request_fd = -1;

    SERVER_START_REQ( init_first_thread )
    {
        req->unix_pid    = unix_pid;
        req->unix_tid    = unix_pid;
        req->reply_fd    = wasm_server_client_reply_fd();
        req->wait_fd     = wasm_server_client_wait_fd();
        req->debug_level = 0;
        wine_server_set_reply( req, machines, sizeof(machines) );
        if (!(ret = wine_server_call( req )))
        {
            teb->ClientId.UniqueProcess = ULongToHandle( reply->pid );
            teb->ClientId.UniqueThread  = ULongToHandle( reply->tid );
            peb->SessionId = reply->session_id;
        }
    }
    SERVER_END_REQ;
    if (ret) return ret;

    SERVER_START_REQ( init_process_done )
    {
        req->teb = wine_server_client_ptr( teb );
        req->peb = wine_server_client_ptr( peb );
        ret = wine_server_call( req );
    }
    SERVER_END_REQ;
    return ret;
}

/* One Windows thread per module instance until M5: the locks around server
 * calls and the fd cache have nothing to exclude. */
pthread_mutex_t fd_cache_mutex = PTHREAD_MUTEX_INITIALIZER;

void server_enter_uninterrupted_section( pthread_mutex_t *mutex, sigset_t *sigset )
{
}

void server_leave_uninterrupted_section( pthread_mutex_t *mutex, sigset_t *sigset )
{
}

/* No file descriptors cross the in-process boundary. */
int wine_server_receive_fd( obj_handle_t *handle )
{
    return -1;
}

void *anon_mmap_alloc( size_t size, int prot )
{
    void *ptr = NULL;
    if (posix_memalign( &ptr, 65536, size )) return MAP_FAILED;
    memset( ptr, 0, size );
    return ptr;
}

unsigned char __cdecl __wine_dbg_get_channel_flags( struct __wine_debug_channel *channel )
{
    return 0;
}

/* File and debugger calls that sync.c makes for job and debug objects; the
 * host implements files (runtime/wine/syscalls.mjs). */
NTSTATUS WINAPI NtReadFile( HANDLE handle, HANDLE event, PIO_APC_ROUTINE apc, void *apc_user,
                            IO_STATUS_BLOCK *io, void *buffer, ULONG length,
                            LARGE_INTEGER *offset, ULONG *key )
{
    return STATUS_NOT_IMPLEMENTED;
}

NTSTATUS WINAPI NtSetInformationFile( HANDLE handle, IO_STATUS_BLOCK *io, void *ptr, ULONG len,
                                      FILE_INFORMATION_CLASS class )
{
    return STATUS_NOT_IMPLEMENTED;
}

NTSTATUS WINAPI NtDebugContinue( HANDLE handle, CLIENT_ID *client, NTSTATUS status )
{
    return STATUS_NOT_IMPLEMENTED;
}
