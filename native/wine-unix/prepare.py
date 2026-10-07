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
    # ntdll's Unix side on wasm32 runs i386 code.
    ('dlls/ntdll/unix/unix_private.h',
     '#ifdef __i386__\nstatic const WORD current_machine = IMAGE_FILE_MACHINE_I386;',
     '#if defined(__i386__) || defined(__wasm32__)\nstatic const WORD current_machine = IMAGE_FILE_MACHINE_I386;', 1),
]

# Text appended to files (file, text).
APPENDS = [
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
