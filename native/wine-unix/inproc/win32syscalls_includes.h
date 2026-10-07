/* What the generated system call thunks (gen-syscalls.py) need: the same
 * headers as win32u's syscall.c, for every prototype in the table. */
#include "ntstatus.h"
#define WIN32_NO_STATUS
#include "windef.h"
#include "winnt.h"
#include "ntgdi_private.h"
#include "ntuser_private.h"
#include "ntuser.h"
#include "wine/unixlib.h"
