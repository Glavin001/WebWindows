// System calls for threads and waits (Milestone 5), on the scheduler in
// ./threads.mjs. They take precedence over Wine's Unix side: waits on
// server objects go to it with a zero timeout, and block in the scheduler
// (which can switch threads) instead of in the module.

import { Thread, ThreadExit, timeoutDeadline, STATUS_TIMEOUT } from './threads.mjs';
import { saveContext, restoreContext } from './exceptions.mjs';
import L from './layout.json' with { type: 'json' };

const CURRENT_PROCESS = 0xffffffff;
const CURRENT_THREAD = 0xfffffffe;
const STATUS_SUCCESS = 0;
const STATUS_PENDING = 0x103;
const STATUS_NOT_SUPPORTED = 0xc00000bb;
const STATUS_INVALID_HANDLE = 0xc0000008;
const STATUS_INVALID_INFO_CLASS = 0xc0000003;
const STATUS_INFO_LENGTH_MISMATCH = 0xc0000004;
const THREAD_CREATE_FLAGS_CREATE_SUSPENDED = 1;
const PS_ATTRIBUTE_CLIENT_ID = 0x10003;
const PS_ATTRIBUTE_TEB_ADDRESS = 0x10004;

/** Scratch memory Wine's Unix side can write results to. */
function scratch(h) {
  if (!h.threadScratch) {
    h.threadScratch = h.unix.malloc(64);
    h.m.u8.fill(0, h.threadScratch, h.threadScratch + 64);
  }
  return h.threadScratch;
}

/** A zero timeout (8 bytes) for polling waits. */
function zeroTimeout(h) {
  return scratch(h) + 48;
}

/** {tid, exitCode, teb} for a server thread handle, or null. */
export function threadInfo(h, handle) {
  if (handle === CURRENT_THREAD) {
    const t = h.threads.current;
    return { tid: t.tid, exitCode: STATUS_PENDING, teb: t.teb };
  }
  const s = scratch(h);
  const status = h.unix.call('wasm_thread_info', 'hppp', handle, s, s + 4, s + 8) >>> 0;
  if (status) return null;
  return { tid: h.u32(s), exitCode: h.u32(s + 4), teb: h.u32(s + 8) };
}

function unixCall(h, name, ...args) {
  return h.unix.syscalls.get(name)(args) >>> 0;
}

/** Polls a server wait: the status, or undefined while it would block. */
function poll(h, name, args) {
  return () => {
    const s = unixCall(h, name, ...args);
    return s === STATUS_TIMEOUT ? undefined : s;
  };
}

// win32u's message waits (user32's GetMessage, MsgWaitForMultipleObjects),
// which Wine waits for inside its Unix side: here they poll the queue with
// a zero timeout and block in the scheduler, so a thread waiting for
// messages can switch out like any other waiting thread (otherwise a
// thread sending a message to another one's window could wait on top of
// the very thread that has to answer).
const WAIT_TIMEOUT = 0x102;
const QS_KEY = 0x1, QS_MOUSE = 0x6, QS_POSTMESSAGE = 0x8, QS_TIMER = 0x10, QS_PAINT = 0x20, QS_SENDMESSAGE = 0x40;
const QS_ALLINPUT = 0x4ff;
const MWMO_INPUTAVAILABLE = 4;

/** Calls a win32u system call by name with arguments laid out in scratch guest memory. */
function win32u(h, name, ...args) {
  h.win32uIds ??= new Map([...h.unix.win32uNames].map(([id, n]) => [n, id]));
  h.win32uArgs ??= h.alloc(0x1000, 4, 'win32u arguments');
  const at = h.win32uArgs + (h.win32uDepth = (h.win32uDepth ?? 0) + 1) * 64;
  args.forEach((v, i) => h.w32(at + i * 4, v));
  const t = h.threads.current;
  t.nest++;
  try {
    return h.unix.win32uSyscall(h.win32uIds.get(name), at) >>> 0;
  } finally {
    t.nest--;
    h.win32uDepth--;
  }
}

/** Whether the queue holds input for `mask`, without waiting. */
const queueReady = (h, mask) => win32u(h, 'NtUserMsgWaitForMultipleObjectsEx', 0, 0, 0, mask, MWMO_INPUTAVAILABLE) !== WAIT_TIMEOUT;

export const WIN32U_WAITS = {
  NtUserGetMessage(a) {
    const [msg, hwnd, first, last] = [a(0), a(1), a(2), a(3)];
    let mask = QS_POSTMESSAGE | QS_SENDMESSAGE;
    if (first || last) {
      if (first <= 0x109 && last >= 0x100) mask |= QS_KEY;
      if ((first <= 0x20e && last >= 0x200) || (first <= 0xad && last >= 0xa0)) mask |= QS_MOUSE;
      if ((first <= 0x113 && last >= 0x113) || (first <= 0x118 && last >= 0x118)) mask |= QS_TIMER;
      if (first <= 0xf && last >= 0xf) mask |= QS_PAINT;
    } else mask = QS_ALLINPUT;
    // Once something is there, Wine's GetMessage takes it (or handles sent
    // messages and comes back to wait, rarely, inside the Unix side).
    const get = () => win32u(this, 'NtUserGetMessage', msg, hwnd, first, last);
    if (queueReady(this, mask) || !this.threads.canYield()) return get();
    return this.threads.block('NtUserGetMessage', this.sys.ret, this.sys.esp, () => (queueReady(this, mask) ? get() : undefined), Infinity);
  },
  NtUserMsgWaitForMultipleObjectsEx(a) {
    const [count, handles, timeout, mask, flags] = [a(0), a(1), a(2), a(3), a(4)];
    const check = () => {
      const s = win32u(this, 'NtUserMsgWaitForMultipleObjectsEx', count, handles, 0, mask, flags);
      return s === WAIT_TIMEOUT ? undefined : s;
    };
    const deadline = timeout === 0xffffffff ? Infinity : performance.now() + timeout;
    return this.threads.block('NtUserMsgWaitForMultipleObjectsEx', this.sys.ret, this.sys.esp, check, deadline, WAIT_TIMEOUT);
  },
  NtUserWaitMessage() {
    const check = () => (win32u(this, 'NtUserMsgWaitForMultipleObjectsEx', 0, 0, 0, QS_ALLINPUT, 0) === WAIT_TIMEOUT ? undefined : 1);
    return this.threads.block('NtUserWaitMessage', this.sys.ret, this.sys.esp, check, Infinity);
  },
};

export const THREAD_SYSCALLS = {
  // -- waits --------------------------------------------------------------------
  NtWaitForSingleObject(a) {
    const handle = a(0);
    if (this.isHostHandle(handle)) return STATUS_SUCCESS;
    const check = poll(this, 'NtWaitForSingleObject', [handle, a(1), zeroTimeout(this)]);
    return this.threads.block('NtWaitForSingleObject', this.sys.ret, this.sys.esp, check, timeoutDeadline(this, a(2)));
  },
  NtWaitForMultipleObjects(a) {
    const [count, handles, type, alertable, timeout] = [a(0), a(1), a(2), a(3), a(4)];
    for (let i = 0; i < count; i++) if (this.isHostHandle(this.u32(handles + i * 4))) return STATUS_SUCCESS;
    const check = poll(this, 'NtWaitForMultipleObjects', [count, handles, type, alertable, zeroTimeout(this)]);
    return this.threads.block('NtWaitForMultipleObjects', this.sys.ret, this.sys.esp, check, timeoutDeadline(this, timeout));
  },
  NtSignalAndWaitForSingleObject(a) {
    const [signal, wait, alertable, timeout] = [a(0), a(1), a(2), a(3)];
    const first = unixCall(this, 'NtSignalAndWaitForSingleObject', signal, wait, alertable, zeroTimeout(this));
    if (first !== STATUS_TIMEOUT) return first;
    const check = poll(this, 'NtWaitForSingleObject', [wait, alertable, zeroTimeout(this)]);
    return this.threads.block('NtSignalAndWaitForSingleObject', this.sys.ret, this.sys.esp, check, timeoutDeadline(this, timeout));
  },
  NtDelayExecution(a) {
    return this.threads.delay('NtDelayExecution', this.sys.ret, this.sys.esp, a(1));
  },
  NtYieldExecution() {
    return this.threads.yieldExecution(this.sys.ret, this.sys.esp);
  },
  NtWaitForAlertByThreadId(a) {
    return this.threads.waitForAlert('NtWaitForAlertByThreadId', this.sys.ret, this.sys.esp, a(1));
  },
  NtAlertThreadByThreadId(a) {
    return this.threads.alert(a(0));
  },
  NtAlertMultipleThreadByThreadId(a) {
    // RtlWakeAddressAll: thread ids at a(0), a(1) of them.
    for (let i = 0; i < a(1); i++) this.threads.alert(this.u32(a(0) + i * 4));
    return STATUS_SUCCESS;
  },
  // Completion ports: thread pool workers wait here.
  NtRemoveIoCompletion(a) {
    const check = poll(this, 'NtRemoveIoCompletion', [a(0), a(1), a(2), a(3), zeroTimeout(this)]);
    return this.threads.block('NtRemoveIoCompletion', this.sys.ret, this.sys.esp, check, timeoutDeadline(this, a(4)));
  },
  NtRemoveIoCompletionEx(a) {
    const check = poll(this, 'NtRemoveIoCompletionEx', [a(0), a(1), a(2), a(3), zeroTimeout(this), a(5)]);
    return this.threads.block('NtRemoveIoCompletionEx', this.sys.ret, this.sys.esp, check, timeoutDeadline(this, a(4)));
  },
  NtWaitForKeyedEvent(a) {
    return this.threads.waitKeyed('NtWaitForKeyedEvent', this.sys.ret, this.sys.esp, a(1), a(3));
  },
  NtReleaseKeyedEvent(a) {
    return this.threads.releaseKeyed(a(1));
  },

  // -- threads ---------------------------------------------------------------------
  NtCreateThreadEx(a) {
    const [phandle, access, , process, start, param, flags, , , stackReserve, attrList] = [
      a(0), a(1), a(2), a(3), a(4), a(5), a(6), a(7), a(8), a(9), a(10),
    ];
    if (process !== CURRENT_PROCESS) return STATUS_NOT_SUPPORTED;
    const t = this.newThread(start, param, stackReserve);
    const s = scratch(this);
    const status = this.unix.call('wasm_create_thread', 'piipp', t.teb, flags, access, s, s + 4) >>> 0;
    if (status) return status;
    t.tid = this.u32(s + 4);
    t.suspend = flags & THREAD_CREATE_FLAGS_CREATE_SUSPENDED ? 1 : 0;
    this.threads.add(t);
    this.w32(phandle, this.u32(s));
    if (attrList) {
      // PS_ATTRIBUTE_LIST: total length, then {attribute, size, value, return length} entries.
      const end = attrList + this.u32(attrList);
      for (let at = attrList + 4; at + 16 <= end; at += 16) {
        const kind = this.u32(at);
        const ptr = this.u32(at + 8);
        if (kind === PS_ATTRIBUTE_CLIENT_ID && ptr) {
          this.w32(ptr, this.u32(t.teb + L.TEB.ClientId));
          this.w32(ptr + 4, t.tid);
        } else if (kind === PS_ATTRIBUTE_TEB_ADDRESS && ptr) {
          this.w32(ptr, t.teb);
        }
      }
    }
    if (this.trace) this.log(`thread ${t.tid.toString(16)} created at ${start.toString(16)}`);
    return STATUS_SUCCESS;
  },
  NtTerminateThread(a) {
    const handle = a(0);
    const status = a(1);
    const self = this.threads.current;
    const info = handle === 0 ? null : threadInfo(this, handle);
    if (handle === 0 || handle === CURRENT_THREAD || info?.tid === self.tid) {
      this.unix.call('wasm_exit_thread', 'i', status);
      this.endThread(self, status);
      throw new ThreadExit(status);
    }
    if (!info) return STATUS_INVALID_HANDLE;
    const r = this.unix.call('wasm_terminate_thread', 'hi', handle, status) >>> 0;
    if (r) return r;
    const t = this.threads.byTid(info.tid);
    if (t) {
      this.unix.call('wasm_forget_thread', 'p', t.teb);
      if (t.state === 'waiting') {
        // Its frames are below this one: it ends when its wait returns.
        t.killed = true;
        t.exitStatus = status;
      } else {
        this.endThread(t, status);
      }
    }
    return STATUS_SUCCESS;
  },
  NtSuspendThread(a) {
    return suspendResume(this, a(0), a(1), false);
  },
  NtResumeThread(a) {
    return suspendResume(this, a(0), a(1), true);
  },
  NtAlertResumeThread(a) {
    return suspendResume(this, a(0), a(1), true);
  },
  NtQueryInformationThread(a) {
    const [handle, cls, buf, len, pret] = [a(0), a(1), a(2), a(3), a(4)];
    const ret = (n) => (pret && this.w32(pret, n), STATUS_SUCCESS);
    const info = threadInfo(this, handle);
    if (!info) return STATUS_INVALID_HANDLE;
    switch (cls) {
      case 0: // ThreadBasicInformation
        if (len < 28) return STATUS_INFO_LENGTH_MISMATCH;
        this.m.u8.fill(0, buf, buf + 28);
        this.w32(buf, info.exitCode);
        this.w32(buf + 4, info.teb);
        this.w32(buf + 8, this.u32(this.teb + L.TEB.ClientId));
        this.w32(buf + 12, info.tid);
        this.w32(buf + 16, 1);
        this.w32(buf + 20, 8);
        this.w32(buf + 24, 8);
        return ret(28);
      case 1: // ThreadTimes
        if (len < 32) return STATUS_INFO_LENGTH_MISMATCH;
        this.m.u8.fill(0, buf, buf + 32);
        return ret(32);
      case 9: // ThreadQuerySetWin32StartAddress
        this.w32(buf, this.threads.byTid(info.tid)?.start ?? 0);
        return ret(4);
      case 12: // ThreadAmILastThread
        this.w32(buf, this.threads.live().length === 1 ? 1 : 0);
        return ret(4);
      case 17: // ThreadIsIoPending
      case 18: // ThreadHideFromDebugger
        this.w32(buf, 0);
        return ret(cls === 18 ? 1 : 4);
      default:
        this.log(`NtQueryInformationThread class ${cls} not implemented`);
        return STATUS_INVALID_INFO_CLASS;
    }
  },
  NtGetCurrentProcessorNumber() {
    return 0;
  },
  NtGetContextThread(a, cpu) {
    const t = threadOf(this, a(0));
    if (!t) return STATUS_INVALID_HANDLE;
    const ctx = a(1);
    const want = this.u32(ctx);
    if (t === this.threads.current) {
      // The thread as it returns from this call: eip in the system call stub.
      const esp = this.m.reg(cpu, 4);
      saveContext(this, cpu, ctx, this.u32(esp));
      this.w32(ctx + L.CONTEXT.Esp, esp + 4);
    } else {
      saveContext(this, t.cpu, ctx, t.pending ? t.pending.ret : t.resume);
      if (t.pending) this.w32(ctx + L.CONTEXT.Esp, t.pending.esp + 4);
    }
    this.w32(ctx + L.CONTEXT.ContextFlags, want);
    return STATUS_SUCCESS;
  },
  NtSetContextThread(a, cpu) {
    const t = threadOf(this, a(0));
    if (!t) return STATUS_INVALID_HANDLE;
    if (t === this.threads.current) {
      const eip = restoreContext(this, cpu, a(1));
      return eip === null ? STATUS_SUCCESS : { jump: eip };
    }
    if (t.pending) return STATUS_NOT_SUPPORTED; // blocked in a system call
    const eip = restoreContext(this, t.cpu, a(1));
    if (eip !== null) t.resume = eip;
    return STATUS_SUCCESS;
  },
};

function threadOf(h, handle) {
  if (handle === CURRENT_THREAD) return h.threads.current;
  const info = threadInfo(h, handle);
  return info ? h.threads.byTid(info.tid) : null;
}

function suspendResume(h, handle, pcount, resume) {
  const s = scratch(h);
  const prev = h.unix.call('wasm_suspend_thread', 'hip', handle, resume ? 1 : 0, s);
  if (prev < 0) return h.u32(s);
  if (pcount) h.w32(pcount, prev);
  const t = threadOf(h, handle);
  if (!t) return STATUS_SUCCESS;
  t.suspend = resume ? Math.max(0, prev - 1) : prev + 1;
  if (!resume && t === h.threads.current) {
    // Suspending itself: blocks until another thread resumes it.
    return h.threads.block('NtSuspendThread', h.sys.ret, h.sys.esp, () => (t.suspend ? undefined : STATUS_SUCCESS), Infinity);
  }
  return STATUS_SUCCESS;
}

export { Thread };
