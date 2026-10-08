// Windows threads for translated Wine (Milestone 5).
//
// Every Windows thread runs in this one JavaScript thread (the page's
// worker, or Node's main thread); the scheduler switches between them. A
// thread's state is all in the shared memory: its registers in its CPU
// state, its frames on its guest stack. Translated frames can be abandoned
// at any time (a translated call that does not return to its return address
// hands the address up to the dispatcher loop), so a thread blocked in a
// system call made directly from guest code "yields": the system call
// returns the YIELD address, the translated frames return to the
// dispatcher loop, and the thread resumes later at its return address.
//
// Waits deeper down cannot yield that way: win32u's message waits and
// waits in a window procedure (a user callback) have Wine's Unix side or
// the host's own frames below them. Those run the other threads on top of
// themselves until something changes, then check again. A thread run that
// way can still yield back, so only threads that block inside such nested
// waits stack up.
//
// Threads switch at system calls: when one blocks, and when one has run for
// a time slice and others are ready. Programs of the era were written for
// one processor; code that spins without system calls waiting for another
// thread is not preempted.

import { ProcessExit, hex } from '../runtime.mjs';

/** Thrown to end the current thread (NtTerminateThread on itself). */
export class ThreadExit extends Error {
  constructor(status) {
    super(`thread exited with ${hex(status)}`);
    this.status = status;
  }
}

const STATUS_SUCCESS = 0;
const STATUS_TIMEOUT = 0x102;
const STATUS_ALERTED = 0x101;
const STATUS_NO_YIELD_PERFORMED = 0x40000024;
const SLICE_MS = 20;

/** Milliseconds until an NT timeout (100 ns units; negative relative, positive absolute, null infinite). */
export function timeoutDeadline(h, ptr) {
  if (!ptr) return Infinity;
  const t = h.m.dv.getBigInt64(ptr, true);
  if (t === -0x8000000000000000n) return Infinity;
  if (t <= 0n) return performance.now() + Number(-t) / 10000;
  // Absolute: 100 ns since 1601.
  const unixMs = Number(t - 116444736000000000n) / 10000;
  return performance.now() + (unixMs - Date.now());
}

export class Thread {
  constructor(fields) {
    Object.assign(this, fields);
    this.state = 'ready'; // ready, running, blocked, waiting (nested), dead
    this.suspend ??= 0;
    this.alerted = false;
    this.nest = 0; // host callbacks (window procedures) running on this thread
    this.pending = null; // the blocked system call: {ret, esp, check, deadline, timeoutStatus}
    this.sliceStart = 0;
  }
}

export class Scheduler {
  /** @param {import('./host.mjs').WineHost} h */
  constructor(h) {
    this.h = h;
    this.m = h.m;
    this.threads = [];
    this.current = null;
    this.yieldAddr = h.m.abi.yield_address >>> 0;
    this.stopAddr = h.m.abi.stop_address >>> 0;
    this.keyed = new Map(); // keyed events: key -> releases not yet waited for
    this.realWait = null; // blocks for real (input or time); set by the host
    this.next = 0;
  }

  add(t) {
    this.threads.push(t);
    return t;
  }

  live() {
    return this.threads.filter((t) => t.state !== 'dead');
  }

  byTid(tid) {
    return this.threads.find((t) => t.tid === tid && t.state !== 'dead');
  }

  byTeb(teb) {
    return this.threads.find((t) => t.teb === teb && t.state !== 'dead');
  }

  /** Makes `t` current for the host and Wine's Unix side. */
  switchTo(t) {
    if (this.current === t) return;
    this.current = t;
    this.h.cpu = t.cpu;
    this.h.teb = t.teb;
    this.h.unix?.M._wasm_switch_thread(t.teb);
  }

  runnable(t) {
    return t.state === 'ready' && t.suspend === 0;
  }

  /** Runs `t` until it yields, blocks or ends. */
  runThread(t) {
    const prev = this.current;
    const M = this.h.unix?.M;
    // A thread that ends deep in Wine's Unix side leaves its frames there:
    // the module's stack pointer comes back to where it was.
    const sp = M?.stackSave();
    this.switchTo(t);
    t.state = 'running';
    t.sliceStart = performance.now();
    this.armSlice(t);
    try {
      const eip = this.m.run(t.cpu, t.resume);
      if (eip === this.stopAddr) throw new Error(`thread ${t.tid} returned from its start routine`);
      if (t.state === 'running') throw new Error(`thread ${hex(t.tid)} yielded without blocking (in ${t.inCall})`);
    } catch (e) {
      if (!(e instanceof ThreadExit)) throw e;
      M?.stackRestore(sp);
      if (t.state !== 'dead') this.h.endThread(t, e.status);
    } finally {
      if (prev && prev.state !== 'dead') this.switchTo(prev);
    }
  }

  ended(t, status) {
    t.state = 'dead';
    t.exitStatus = status;
    this.lastExit = status;
    if (this.h.trace) this.h.log(`thread ${hex(t.tid)} ended with ${hex(status)}`);
  }

  /** Completes a blocked system call: eax = status, back to the caller. */
  complete(t, status) {
    const p = t.pending;
    t.pending = null;
    this.m.setReg(t.cpu, 0, status >>> 0);
    this.m.setReg(t.cpu, 4, p.espAfter);
    t.resume = p.ret;
    t.state = 'ready';
  }

  /** Checks blocked threads; returns whether one became ready. */
  poll() {
    this.lastPoll = performance.now();
    this.serverTimers();
    let woke = false;
    const now = performance.now();
    for (const t of this.threads) {
      if (t.state !== 'blocked') continue;
      const prev = this.current;
      this.switchTo(t);
      let status;
      try {
        status = this.probe(t.pending.check);
      } finally {
        if (prev) this.switchTo(prev);
      }
      if (status === undefined && now >= t.pending.deadline) status = t.pending.timeoutStatus;
      if (status !== undefined) {
        this.complete(t, status);
        woke = true;
      }
    }
    return woke;
  }

  /**
   * After an object was signalled (an event set, a semaphore or mutex
   * released): completes every wait it satisfies now, as Windows does
   * inside SetEvent. Waits are otherwise checked only when the scheduler
   * gets to them, and a waiter would miss a manual-reset event that is set
   * and reset again before then (condition variables built on events do
   * exactly that: the first waiter to wake resets it for the others).
   */
  signalled() {
    this.poll();
    for (const t of this.threads) {
      const w = t.nestedWait;
      if (t === this.current || t.state !== 'waiting' || !w || w.status !== undefined) continue;
      const prev = this.current;
      this.switchTo(t);
      try {
        w.status = this.probe(w.check);
      } finally {
        if (prev) this.switchTo(prev);
      }
    }
  }

  pick(except) {
    const n = this.threads.length;
    for (let k = 0; k < n; k++) {
      const t = this.threads[(this.next + k) % n];
      if (t !== except && this.runnable(t)) {
        this.next = (this.next + k + 1) % n;
        return t;
      }
    }
    return null;
  }

  /** Milliseconds until the earliest deadline of a blocked thread (-1: none). */
  untilDeadline() {
    let d = Infinity;
    for (const t of this.threads) if (t.state === 'blocked') d = Math.min(d, t.pending.deadline, t.pending.wakeAt?.() ?? Infinity);
    if (d === Infinity) return -1;
    return Math.max(0, Math.ceil(d - performance.now()));
  }

  /** Runs wineserver's expired timers; returns ms until its next one (-1: none). */
  serverTimers() {
    return this.h.unix ? this.h.unix.M._wasm_server_run() : -1;
  }

  /** Blocks for real until input arrives or `ms` pass; returns realWait's result. */
  idle(ms) {
    // The server's timers (WM_TIMER, waitable timers) run while waiting.
    const next = this.serverTimers();
    if (next >= 0) ms = ms < 0 ? next : Math.min(ms, next);
    if (ms < 0 && globalThis.process?.env?.WWT_THREAD_DUMP) {
      this.dumpAt = 0;
      this.maybeDump();
    }
    const r = this.realWait(ms);
    if (r > 0) this.h.unix?.M._wasm_process_input();
    return r;
  }

  /** The process: runs threads until it exits (ProcessExit propagates). */
  run() {
    // WWT_THREAD_DUMP=ms: prints the threads' states once, after that long.
    this.dumpAt = globalThis.process?.env?.WWT_THREAD_DUMP ? performance.now() + Number(process.env.WWT_THREAD_DUMP) : Infinity;
    for (;;) {
      this.maybeDump();
      // The embedder's check between slices (wine.mjs: --run-for).
      this.onSlice?.();
      // Waits satisfied meanwhile complete at least once a slice, even
      // while other threads keep the scheduler busy.
      if (performance.now() - (this.lastPoll ?? 0) >= SLICE_MS) this.poll();
      const t = this.pick(null);
      if (t) {
        this.runThread(t);
        continue;
      }
      if (this.poll()) continue;
      // The last thread ended: so does the process, with its exit code.
      if (!this.live().length) throw new ProcessExit(this.lastExit ?? 0);
      const ms = this.untilDeadline();
      const r = this.idle(ms);
      if (r < 0 && ms < 0) throw new Error(`deadlock: ${this.describe()}`);
    }
  }

  maybeDump() {
    if (!(performance.now() > this.dumpAt)) return;
    this.dumpAt = Infinity;
    const lines = [`threads: ${this.describe()}`];
    for (const t of this.live()) lines.push(`  ${hex(t.tid)} ${t.state} nest ${t.nest} in ${t.inCall ?? '-'} eip ${hex(t.pending ? t.pending.ret : t.resume)} esp ${hex(this.m.reg(t.cpu, 4))}`);
    if (globalThis.process?.env?.WWT_THREAD_DUMP_STACK) {
      Error.stackTraceLimit = 200;
      lines.push(new Error().stack.split('\n').filter((l) => !l.includes('wasm-function')).join('\n'));
    }
    this.h.stderr(new TextEncoder().encode(lines.join('\n') + '\n'));
  }

  describe() {
    return this.live()
      .map((t) => `thread ${hex(t.tid)} ${t.state}${t.suspend ? ' suspended' : ''}${t.pending ? ` in ${t.pending.name}` : ''}`)
      .join(', ');
  }

  /** Whether the current thread can yield (its system call came straight from guest code at its base). */
  canYield() {
    const t = this.current;
    return t && t.nest === 0 && t.state === 'running';
  }

  /** Runs a wait's check: a poll, during which no other thread runs. */
  probe(check) {
    this.polling = (this.polling ?? 0) + 1;
    try {
      return check();
    } finally {
      this.polling--;
    }
  }

  /**
   * Blocks the current thread in a system call until `check()` returns a
   * status or the deadline passes (then `timeoutStatus`). Returns the status
   * now, or the YIELD address when the thread yielded.
   */
  block(name, ret, esp, check, deadline, timeoutStatus = STATUS_TIMEOUT, wakeAt = undefined) {
    let status = this.probe(check);
    if (status !== undefined) return { status };
    if (performance.now() >= deadline) return { status: timeoutStatus };
    const t = this.current;
    if (this.canYield()) {
      // A system call pops its return address; a Unix library call
      // (__wine_unix_call) its arguments too.
      const espAfter = this.h.sys?.espAfter ?? esp + 4;
      t.pending = { name, ret, esp, espAfter, check, deadline, timeoutStatus, wakeAt };
      t.state = 'blocked';
      return { yield: true };
    }
    // Nested: run other threads until the condition holds (or a signal
    // satisfied it meanwhile: signalled() records that in nestedWait).
    const w = { check, status: undefined };
    const outer = t.nestedWait;
    t.nestedWait = w;
    try {
      for (;;) {
        const until = Math.min(deadline, wakeAt?.() ?? Infinity);
        const left = until === Infinity ? -1 : Math.max(0, Math.ceil(until - performance.now()));
        this.waitNested(left);
        status = w.status ?? this.probe(check);
        if (status !== undefined) return { status };
        if (performance.now() >= deadline) return { status: timeoutStatus };
      }
    } finally {
      t.nestedWait = outer;
    }
  }

  /**
   * A wait that cannot yield (Wine's Unix side waiting for a wakeup, or a
   * wait under a host callback): runs other threads, or blocks for real
   * when none can run. Returns 1 for input, 0 otherwise, -1 when nothing
   * can ever happen.
   */
  waitNested(ms) {
    this.maybeDump();
    // Polling a wait (a zero timeout) must not run other threads: if Wine's
    // Unix side still waits a moment (its clock trails the host's), sleep.
    if (this.polling) return this.idle(ms < 0 || ms > 1 ? 1 : ms) > 0 ? 1 : 0;
    const self = this.current;
    // A blocked thread can wait here too: polling it can call back into
    // its guest code (a window procedure). It goes back to its state after.
    const before = self?.state;
    if (self) self.state = 'waiting';
    // Another exception on its way out (ProcessExit from a thread run
    // here) wins over this thread's own end.
    let done = false;
    try {
      let ran = false;
      for (;;) {
        const t = this.pick(self);
        if (!t) break;
        this.runThread(t);
        ran = true;
        // Give the waiter a chance to see what the other thread did.
        break;
      }
      if (!ran && this.poll()) ran = true;
      let r = 0;
      if (!ran) {
        const d = this.untilDeadline();
        const wait = d < 0 ? ms : ms < 0 ? d : Math.min(ms, d);
        r = this.idle(wait);
        if (r < 0 && d >= 0) r = 0;
      }
      done = true;
      return r;
    } finally {
      if (self) {
        if (self.state === 'waiting') self.state = before;
        this.switchTo(self);
        if (self.killed && done) throw new ThreadExit(self.exitStatus ?? 0);
      }
    }
  }

  /**
   * At a system call: whether to let other threads run, because the
   * current one used up its slice or a sleeping thread's time came (a
   * timer thread should run on time).
   */
  shouldPreempt() {
    const t = this.current;
    if (!t) return false;
    const now = performance.now();
    let due = now - t.sliceStart >= SLICE_MS;
    if (!due) {
      for (const o of this.threads) {
        if (o.state === 'blocked' && Math.min(o.pending.deadline, o.pending.wakeAt?.() ?? Infinity) <= now) due = true;
      }
    }
    if (!due) return false;
    // The embedder's check between slices (wine.mjs: --run-for), also for a
    // thread that keeps running.
    this.onSlice?.();
    // Blocked threads whose wait is satisfied join the turn order now:
    // with other threads always ready, nothing else would complete their
    // waits (two threads spinning on a set event would starve the ones
    // woken to reset it).
    this.poll();
    return this.threads.some((o) => o !== t && this.runnable(o));
  }

  /**
   * Sets the deadline translated loops check (cpu.PREEMPT_AT, against the
   * tick count the ticker keeps in KUSER_SHARED_DATA): the end of the slice.
   */
  armSlice(t) {
    const tick = this.m.u32[this.m.tickAddr >>> 2];
    this.m.u32[(t.cpu + this.m.abi.cpu.PREEMPT_AT) >>> 2] = (tick + SLICE_MS) >>> 0;
  }

  /**
   * A translated loop reached its slice deadline (Machine.onPreempt): a
   * thread that spins without system calls (waiting for another thread to
   * set a flag) lets the others run. It yields as at a system call and
   * resumes at the loop header later (its state is written back; fast mode
   * translates the header as an entry). Only a thread running at its base
   * can: one under a callback or a nested wait keeps running, as running
   * others on top of it could bury it under a wait that never ends.
   */
  preempt(cpu, eip) {
    this.onSlice?.();
    const t = this.current;
    if (t && t.cpu === cpu && this.canYield()) {
      this.poll();
      if (this.threads.some((o) => o !== t && this.runnable(o))) {
        t.resume = eip;
        t.state = 'ready';
        return true;
      }
    }
    if (t) this.armSlice(t);
    return false;
  }

  /** Lets others run: the current thread continues at `ret` later. */
  yieldAt(ret) {
    const t = this.current;
    t.resume = ret;
    t.state = 'ready';
  }

  // ---- Waits the host implements itself ----------------------------------------

  /** NtWaitForAlertByThreadId(address, timeout): RtlWaitOnAddress, critical sections, SRW locks. */
  waitForAlert(name, ret, esp, timeoutPtr) {
    const t = this.current;
    const check = () => {
      if (!t.alerted) return undefined;
      t.alerted = false;
      return STATUS_ALERTED;
    };
    return this.block(name, ret, esp, check, timeoutDeadline(this.h, timeoutPtr));
  }

  alert(tid) {
    const t = this.byTid(tid);
    if (!t) return 0xc000000b; // STATUS_INVALID_CID
    t.alerted = true;
    return STATUS_SUCCESS;
  }

  /** NtWaitForKeyedEvent / NtReleaseKeyedEvent (RtlRunOnce): a release wakes one waiter on the key. */
  waitKeyed(name, ret, esp, key, timeoutPtr) {
    const check = () => {
      const n = this.keyed.get(key) ?? 0;
      if (!n) return undefined;
      if (n === 1) this.keyed.delete(key);
      else this.keyed.set(key, n - 1);
      return STATUS_SUCCESS;
    };
    return this.block(name, ret, esp, check, timeoutDeadline(this.h, timeoutPtr));
  }

  releaseKeyed(key) {
    this.keyed.set(key, (this.keyed.get(key) ?? 0) + 1);
    return STATUS_SUCCESS;
  }

  /** NtDelayExecution(alertable, timeout): Sleep. */
  delay(name, ret, esp, timeoutPtr) {
    return this.sleepUntil(name, ret, esp, timeoutDeadline(this.h, timeoutPtr));
  }

  /** NtYieldExecution: lets ready threads run. */
  yieldExecution(ret, esp) {
    const others = this.threads.some((o) => o !== this.current && this.runnable(o));
    if (!others) return { status: STATUS_NO_YIELD_PERFORMED };
    return this.sleepUntil('NtYieldExecution', ret, esp, performance.now());
  }

  sleepUntil(name, ret, esp, deadline) {
    const others = this.threads.some((o) => o !== this.current && this.runnable(o));
    // Sleep(0) with nothing else to run returns at once.
    if (deadline <= performance.now() && !others) return { status: STATUS_SUCCESS };
    // Others get a turn even for a zero timeout: the first check fails.
    let first = true;
    const check = () => {
      if (first) return (first = false), undefined;
      return performance.now() >= deadline ? STATUS_SUCCESS : undefined;
    };
    return this.block(name, ret, esp, check, Math.max(deadline, performance.now() + 0.001), STATUS_SUCCESS);
  }
}

export { STATUS_TIMEOUT };
