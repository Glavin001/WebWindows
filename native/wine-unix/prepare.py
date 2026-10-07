#!/usr/bin/env python3
"""Prepares Wine's Unix-side sources for the Emscripten build.

Copies the parts of the pinned Wine source tree the build compiles into
OUT, then applies the edits below: wasm32 is added as a CPU that uses the
i386 data layout (CONTEXT and friends), without the i386 inline assembly.
Each edit names the exact text it replaces and fails if that text is
missing, so a Wine upgrade cannot apply an edit silently in the wrong place.

    prepare.py WINE_SRC WINE_BUILD OUT
"""

import os
import shutil
import sys

wine_src, wine_build, out = sys.argv[1:4]

COPY = ['include', 'server', 'dlls/win32u', 'dlls/ntdll']

# (file, old, new, count): replace `old` with `new`, which must occur
# `count` times.
EDITS = [
    # The i386 CONTEXT, register and exception layouts apply to wasm32: the
    # Unix side must see the guest's structures as the i386 guest lays them out.
    ('include/winnt.h',
     '#ifdef __i386__\n\n#define CONTEXT_CONTROL CONTEXT_I386_CONTROL',
     '#if defined(__i386__) || defined(__wasm32__)\n\n#define CONTEXT_CONTROL CONTEXT_I386_CONTROL', 1),
    # The current TEB is kept by the host (runtime/wine), which the Unix side
    # asks through a function.
    ('include/winnt.h',
     '#elif !defined(RC_INVOKED)\n# error You must define NtCurrentTeb() for your architecture',
     '#elif defined(__wasm32__)\nNTSYSAPI struct _TEB * WINAPI NtCurrentTeb(void);\n'
     '#elif !defined(RC_INVOKED)\n# error You must define NtCurrentTeb() for your architecture', 1),
    # wineserver: a wasm32 host runs i386 programs.
    ('server/registry.c',
     '#ifdef __i386__\n    if (prefix_type == PREFIX_32BIT) supported_machines[count++] = IMAGE_FILE_MACHINE_I386;',
     '#if defined(__i386__) || defined(__wasm32__)\n    if (prefix_type == PREFIX_32BIT) supported_machines[count++] = IMAGE_FILE_MACHINE_I386;', 1),
    # Linux socket filters need linux/filter.h, which Emscripten lacks.
    ('server/sock.c',
     '#elif defined(IP_UNICAST_IF) && defined(SO_ATTACH_FILTER) && defined(SO_BINDTODEVICE)',
     '#elif defined(IP_UNICAST_IF) && defined(SO_ATTACH_FILTER) && defined(SO_BINDTODEVICE) && defined(HAVE_LINUX_FILTER_H)', 1),
    # Emscripten has no socketpair, so this probe always fails; the EOF
    # fallback it picks is right, so only report it when debugging.
    ('server/sock.c',
     '        fprintf( stderr, "sock_init: ERROR in sock_check_pollhup()\\n" );',
     '        if (debug_level) fprintf( stderr, "sock_init: ERROR in sock_check_pollhup()\\n" );', 1),
    # The session's shared memory (windows, classes, queues) is read by
    # win32u in-process straight from the server's block (see
    # wasm_server_session_view): one block big enough not to grow keeps
    # every object in it.
    ('server/mapping.c',
     'size_t size = max( sizeof(*shared_session) + sizeof(object_shm_t) * 512, 0x10000 );',
     'size_t size = max( sizeof(*shared_session) + sizeof(object_shm_t) * 512, 0x400000 );', 1),
    # Wine's desktop and message-only windows belong to explorer's process;
    # here the page's one process asks the server to create them, so their
    # shared handle entries would name it as the owner and win32u would look
    # for a client-side window structure that does not exist. Clear the
    # owner, as if another process held them.
    ('server/window.c',
     '            detach_window_thread( desktop->top_window );',
     '            detach_window_thread( desktop->top_window );\n'
     '            clear_user_handle_owner( desktop->top_window->handle );', 1),
    ('server/window.c',
     '            detach_window_thread( desktop->msg_window );',
     '            detach_window_thread( desktop->msg_window );\n'
     '            clear_user_handle_owner( desktop->msg_window->handle );', 1),
    ('server/window.c',
     'DECL_HANDLER(get_desktop_window)\n{',
     'extern void clear_user_handle_owner( user_handle_t handle );\n\n'
     'DECL_HANDLER(get_desktop_window)\n{', 1),
    # ntdll's Unix side on wasm32 runs i386 code.
    ('dlls/ntdll/unix/unix_private.h',
     '#ifdef __i386__\nstatic const WORD current_machine = IMAGE_FILE_MACHINE_I386;',
     '#if defined(__i386__) || defined(__wasm32__)\nstatic const WORD current_machine = IMAGE_FILE_MACHINE_I386;', 1),
]

# Text appended to files (file, text).
APPENDS = [
    # ntdll's Unix start-up (not run here) sets the data directory's NT
    # name; the in-process client sets it instead (inproc/client.c).
    ('dlls/ntdll/unix/env.c', '''

void wasm_set_nt_data_dir( const WCHAR *dir )
{
    nt_data_dir = dir;
}
'''),
    ('server/user.c', '''

/* No process or thread owns this user object (see prepare.py). */
void clear_user_handle_owner( user_handle_t handle )
{
    user_entry_t *entry = (user_entry_t *)handle_to_entry( handle );
    if (!entry) return;
    entry->tid = 0;
    entry->pid = 0;
}
'''),
    # In-process clients map the session's shared memory by pointer: a view
    # of the session mapping is the server's own block.
    ('server/mapping.c', '''

const void *wasm_server_session_view( struct process *process, obj_handle_t handle, mem_size_t offset,
                                      mem_size_t *size )
{
    struct mapping *mapping;
    struct session_block *block;
    const void *ret = NULL;

    if (!(mapping = (struct mapping *)get_handle_obj( process, handle, SECTION_MAP_READ, &mapping_ops )))
        return NULL;
    if (mapping == session_mapping)
    {
        LIST_FOR_EACH_ENTRY( block, &session.blocks, struct session_block, entry )
        {
            if (block->offset != offset) continue;
            *size = block->block_size;
            ret = block->data;
            break;
        }
    }
    release_object( mapping );
    return ret;
}
'''),
    # One non-blocking step of the server's main loop, for the in-process
    # server (inproc/server.c): run expired timers, handle ready file
    # descriptors, and return the milliseconds until the next timer.
    ('server/fd.c', '''

int wasm_server_poll(void)
{
    int i, ret;

    set_current_time();
    get_next_timeout( NULL );
    ret = poll( pollfd, nb_users, 0 );
    set_current_time();
    if (ret > 0)
    {
        for (i = 0; i < nb_users; i++)
        {
            if (pollfd[i].revents)
            {
                fd_poll_event( poll_users[i], pollfd[i].revents );
                if (!--ret) break;
            }
        }
    }
    return get_next_timeout( NULL );
}
'''),
]


def copy_tree(rel):
    src = os.path.join(wine_src, rel)
    dst = os.path.join(out, rel)
    if os.path.exists(dst):
        shutil.rmtree(dst)
    shutil.copytree(src, dst)


def main():
    os.makedirs(out, exist_ok=True)
    for rel in COPY:
        copy_tree(rel)
    # Generated headers from the configured build tree (server_protocol.h is
    # in the source tree; config.h comes from gen-config.sh).
    for rel, (old, new, count) in [(e[0], e[1:]) for e in EDITS]:
        path = os.path.join(out, rel)
        text = open(path).read()
        n = text.count(old)
        if n == 0 or (count is not None and n != count):
            sys.exit(f'prepare.py: {rel}: expected {count or "some"} occurrence(s) of an edit, found {n}:\n{old}')
        open(path, 'w').write(text.replace(old, new))
    for rel, text in APPENDS:
        with open(os.path.join(out, rel), 'a') as f:
            f.write(text)


main()
