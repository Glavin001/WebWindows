/*
 * wineserver in-process, for the browser (Milestone 4).
 *
 * Wine runs wineserver as its own process and talks to it over Unix
 * sockets. In the browser there is one Windows process per page, so the
 * server is linked into the same WebAssembly module as Wine's Unix side and
 * its request handlers are called directly: wasm_server_call does what the
 * server's call_req_handler does, but takes the request from, and puts the
 * reply into, the client's request buffer instead of a pipe.
 *
 * Everything else is the real server: objects, handles, message queues,
 * windows, hooks, the registry. Waits still go through the thread's wait
 * pipe, which the client reads (see client.c).
 */

#include "config.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "ntstatus.h"
#define WIN32_NO_STATUS
#include "windef.h"
#include "winternl.h"
#include "wine/server.h"

#include "file.h"
#include "handle.h"
#include "process.h"
#include "thread.h"
#include "unicode.h"
#include "request.h"
#include "request_handlers.h"

#include <emscripten.h>

/* fd.c (see prepare.py) */
extern int wasm_server_poll(void);

static struct thread *inproc_thread;  /* the thread requests come from */
static int wait_pipe[2] = { -1, -1 };

#define CLIENT_REPLY_FD 1000
#define CLIENT_WAIT_FD  1001

static void make_dir( const char *dir )
{
    if (mkdir( dir, 0700 ) == -1 && errno != EEXIST) fatal_error( "mkdir %s: %s\n", dir, strerror( errno ));
}

/* What the server's main() does before its main loop, minus the master
 * socket: the configuration directory is /wine/prefix in the in-memory file
 * system (registry files are kept there). */
EMSCRIPTEN_KEEPALIVE int wasm_server_start(void)
{
    make_dir( "/wine" );
    make_dir( "/wine/prefix" );
    if (chdir( "/wine/prefix" ) == -1) fatal_error( "chdir /wine/prefix\n" );
    config_dir_fd = open( ".", O_RDONLY );
    make_dir( "/wine/prefix/server" );
    if (chdir( "/wine/prefix/server" ) == -1) fatal_error( "chdir server dir\n" );
    server_dir = strdup( "/wine/prefix/server" );
    server_dir_fd = open( ".", O_RDONLY );

    sock_init();
    set_current_time();
    init_signals();
    init_memory();
    init_directories( load_intl_file() );
    init_threading();
    init_registry();
    server_start_time = current_time;
    return 0;
}

/* Creates the process and its first thread, as the server does when a
 * client connects, and hands the thread the pipes init_first_thread
 * expects to receive over the socket. Returns the client end of the wait
 * pipe, which client.c reads for wakeups. */
EMSCRIPTEN_KEEPALIVE int wasm_server_new_process(void)
{
    int msg_pipe[2], request_pipe[2], reply_pipe[2];
    struct process *process;

    if (pipe( msg_pipe ) == -1 || pipe( request_pipe ) == -1 || pipe( reply_pipe ) == -1 || pipe( wait_pipe ) == -1)
        fatal_error( "pipe: %s\n", strerror( errno ));
    fcntl( wait_pipe[0], F_SETFL, O_NONBLOCK );

    if (!(process = create_process( msg_pipe[0], NULL, 0, NULL, NULL, NULL, 0, NULL )))
        fatal_error( "create_process failed: %08x\n", get_error() );
    if (!(inproc_thread = create_thread( request_pipe[0], process, NULL )))
        fatal_error( "create_thread failed: %08x\n", get_error() );
    release_object( process );
    thread_add_inflight_fd( inproc_thread, CLIENT_REPLY_FD, reply_pipe[1] );
    thread_add_inflight_fd( inproc_thread, CLIENT_WAIT_FD, wait_pipe[1] );
    return wait_pipe[0];
}

EMSCRIPTEN_KEEPALIVE int wasm_server_client_reply_fd(void) { return CLIENT_REPLY_FD; }
EMSCRIPTEN_KEEPALIVE int wasm_server_client_wait_fd(void) { return CLIENT_WAIT_FD; }

/* Threads (Milestone 5). Each Windows thread has its own server thread;
 * the host runs one at a time and selects the one requests come from. */
void *wasm_server_get_thread(void) { return inproc_thread; }
void wasm_server_set_thread( void *thread ) { inproc_thread = thread; }

/* The request pipe a new thread gets: new_thread receives its read end as
 * an fd the calling thread sent. The write end stays open for the thread's
 * life (the server ends a thread whose request pipe closes). */
int wasm_server_send_request_fd( int *write_end )
{
    int request_pipe[2];

    if (!inproc_thread || pipe( request_pipe ) == -1) return -1;
    thread_add_inflight_fd( inproc_thread, request_pipe[0], request_pipe[0] );
    *write_end = request_pipe[1];
    return request_pipe[0];
}

/* Gives a thread new_thread created its reply and wait pipes (sent over the
 * socket in Wine) and returns it, holding a reference; the client end of
 * the wait pipe goes to `wait_fd`. */
void *wasm_server_thread_setup( unsigned int tid, int *wait_fd )
{
    int reply_pipe[2], thread_wait[2];
    struct thread *thread = get_thread_from_id( tid );

    if (!thread) return NULL;
    if (pipe( reply_pipe ) == -1 || pipe( thread_wait ) == -1) fatal_error( "pipe: %s\n", strerror( errno ));
    fcntl( thread_wait[0], F_SETFL, O_NONBLOCK );
    thread_add_inflight_fd( thread, CLIENT_REPLY_FD, reply_pipe[1] );
    thread_add_inflight_fd( thread, CLIENT_WAIT_FD, thread_wait[1] );
    *wait_fd = thread_wait[0];
    return thread;
}

/* A thread ended (it asked the server to terminate itself, or another
 * thread terminated it): the server marks it terminated, which wakes
 * waiters on it, and the reference from setup is dropped. */
void wasm_server_thread_ended( void *ptr )
{
    struct thread *thread = ptr;

    if (thread->state != TERMINATED) kill_thread( thread, 0 );
    release_object( thread );
}

/* One request: the client's struct __server_request_info in, the reply in
 * the same buffer out. */
EMSCRIPTEN_KEEPALIVE unsigned int wasm_server_call( void *req_ptr )
{
    struct __server_request_info *r = req_ptr;
    struct thread *thread = inproc_thread;
    union generic_reply reply;
    enum request req = r->u.req.request_header.req;
    data_size_t reply_max = r->u.req.request_header.reply_size;
    data_size_t size = r->u.req.request_header.request_size;
    unsigned int i, error;

    if (!thread) return STATUS_INVALID_CID;
    memcpy( &thread->req, &r->u.req, sizeof(thread->req) );
    thread->req_data = NULL;
    if (size)
    {
        char *p;
        if (!(thread->req_data = p = malloc( size ))) return STATUS_NO_MEMORY;
        for (i = 0; i < r->data_count; i++)
        {
            memcpy( p, r->data[i].ptr, r->data[i].size );
            p += r->data[i].size;
        }
    }

    current = thread;
    /* The server's clock moves only when its loop runs; a request with a
     * timeout of "now" (a poll) must see it expired, not wait for it. */
    set_current_time();
    thread->reply_size = 0;
    clear_error();
    memset( &reply, 0, sizeof(reply) );
    if (debug_level) trace_request();
    if (req < REQ_NB_REQUESTS) req_handlers[req]( &thread->req, &reply );
    else set_error( STATUS_NOT_IMPLEMENTED );

    error = thread->error;
    reply.reply_header.error = error;
    reply.reply_header.reply_size = thread->reply_size;
    if (debug_level) trace_reply( req, &reply );
    memcpy( &r->u.reply, &reply, sizeof(reply) );
    if (thread->reply_size && r->reply_data)
        memcpy( r->reply_data, thread->reply_data, min( thread->reply_size, reply_max ));
    free( thread->reply_data );
    thread->reply_data = NULL;
    free( thread->req_data );
    thread->req_data = NULL;
    current = NULL;
    return error;
}

/* Runs expired server timers and pending file descriptor events; returns
 * the milliseconds until the next timer, or -1 when there is none. */
EMSCRIPTEN_KEEPALIVE int wasm_server_run(void)
{
    return wasm_server_poll();
}

extern const void *wasm_server_session_view( struct process *process, obj_handle_t handle, mem_size_t offset,
                                             mem_size_t *size );

/* A view of a server section for an in-process client: the session's
 * shared memory is mapped by pointer. NULL for other sections. */
const void *wasm_server_map_view( unsigned int handle, unsigned long long offset, unsigned int *size )
{
    mem_size_t sz = 0;
    const void *ret;

    if (!inproc_thread) return NULL;
    ret = wasm_server_session_view( inproc_thread->process, handle, offset, &sz );
    clear_error();
    *size = sz;
    return ret;
}

EMSCRIPTEN_KEEPALIVE void wasm_server_set_debug( int level )
{
    debug_level = level;
}
