// Windows exceptions for translated Wine (Milestone 5).
//
// A fault in translated code (divide by zero, a bad address, int3, ud2...)
// writes the thread's registers back to its CPU state and calls the host,
// which does what Wine's Unix side does for a signal on i386
// (dlls/ntdll/unix/signal_i386.c): it saves the registers as a CONTEXT,
// puts an EXCEPTION_RECORD and the CONTEXT on the guest stack below the
// faulting frame, and continues at ntdll's KiUserExceptionDispatcher. From
// there Wine's own dispatcher (translated, like the rest of ntdll) calls
// the vectored handlers, walks the FS:[0] handler chain and either resumes
// with NtContinue or unwinds to a handler, which jumps to its target. The
// translated frames below are abandoned: a translated call that does not
// return to its return address hands the address up to the dispatcher loop.
//
// RtlRaiseException arrives as NtRaiseException with a CONTEXT already
// filled in; the first chance goes to the same dispatcher, the second
// chance (no handler) ends the process with the exception code.

import { ProcessExit, hex } from '../runtime.mjs';
import L from './layout.json' with { type: 'json' };

const C = L.CONTEXT;
const CONTEXT_i386 = 0x10000;
const CONTEXT_CONTROL = CONTEXT_i386 | 0x1;
const CONTEXT_INTEGER = CONTEXT_i386 | 0x2;
const CONTEXT_SEGMENTS = CONTEXT_i386 | 0x4;
const CONTEXT_FLOATING_POINT = CONTEXT_i386 | 0x8;
const CONTEXT_EXTENDED_REGISTERS = CONTEXT_i386 | 0x20;
const CONTEXT_DEBUG_REGISTERS = CONTEXT_i386 | 0x10;
const CONTEXT_FULL = CONTEXT_CONTROL | CONTEXT_INTEGER | CONTEXT_SEGMENTS;

// Debug registers: Dr0-Dr3, Dr6, Dr7 after ContextFlags. They are kept per
// thread and reported back; hardware breakpoints do not fire.
const DEBUG_OFFSETS = [4, 8, 12, 16, 20, 24];

function debugRegs(h, cpu) {
  h.debugRegisters ??= new Map();
  let d = h.debugRegisters.get(cpu);
  if (!d) h.debugRegisters.set(cpu, (d = [0, 0, 0, 0, 0, 0]));
  return d;
}

// EXCEPTION_RECORD (i386): code, flags, nested record, address, count,
// then up to 15 parameters.
const EXCEPTION_NONCONTINUABLE = 1;

// struct exc_stack_layout in signal_i386.c: pointers to the record and the
// context (KiUserExceptionDispatcher's arguments), the record, the context
// and a CONTEXT_EX after it.
const LAYOUT_REC = 0x08;
const LAYOUT_CONTEXT = 0x58;
const LAYOUT_SIZE = 0x340;

const GPR_OFFSETS = [C.Eax, C.Ecx, C.Edx, C.Ebx, C.Esp, C.Ebp, C.Esi, C.Edi];
const SEG_OFFSETS = [C.SegEs, C.SegCs, C.SegSs, C.SegDs, C.SegFs, C.SegGs];

// ---- 80-bit extended precision ---------------------------------------------

/** Writes an f64 as an x87 80-bit value (exact). */
export function writeF80(dv, at, v) {
  const b = new DataView(new ArrayBuffer(8));
  b.setFloat64(0, v, true);
  const lo = b.getUint32(0, true);
  const hi = b.getUint32(4, true);
  const sign = hi >>> 31;
  const exp = (hi >>> 20) & 0x7ff;
  let mant = (BigInt(hi & 0xfffff) << 32n) | BigInt(lo);
  let e;
  if (exp === 0x7ff) {
    e = 0x7fff;
    mant = (1n << 63n) | (mant << 11n);
  } else if (exp === 0) {
    if (mant === 0n) {
      e = 0;
    } else {
      // Denormal f64: normalize into the wider exponent range.
      let shift = 0;
      while (!(mant & (1n << 52n))) {
        mant <<= 1n;
        shift++;
      }
      e = 1 - 1023 - shift + 16383;
      mant <<= 11n;
    }
  } else {
    e = exp - 1023 + 16383;
    mant = (1n << 63n) | (mant << 11n);
  }
  dv.setBigUint64(at, mant & 0xffffffffffffffffn, true);
  dv.setUint16(at + 8, (sign << 15) | e, true);
}

/** Reads an x87 80-bit value as f64 (rounded to nearest). */
export function readF80(dv, at) {
  const mant = dv.getBigUint64(at, true);
  const se = dv.getUint16(at + 8, true);
  const sign = se & 0x8000 ? -1 : 1;
  const exp = se & 0x7fff;
  if (exp === 0x7fff) return (mant << 1n) & 0xffffffffffffffffn ? NaN : sign * Infinity;
  if (mant === 0n) return sign * 0;
  // value = mant * 2^(exp - 16383 - 63); Number(BigInt) rounds to nearest.
  return sign * Number(mant) * 2 ** ((exp === 0 ? 1 : exp) - 16383 - 63);
}

// ---- CONTEXT --------------------------------------------------------------------

/**
 * Fills a CONTEXT from a thread's CPU state, as at `eip`. Arithmetic flags
 * are exact only when the translated code wrote them back (faults write
 * back registers, not lazy flags; see opt::FAULT_SYNC).
 */
export function saveContext(h, cpu, ctx, eip) {
  cpu >>>= 0; // a pointer from WebAssembly arrives as a signed i32
  const m = h.m;
  const A = m.abi.cpu;
  const dv = m.dv;
  m.u8.fill(0, ctx, ctx + C.__size);
  h.w32(ctx + C.ContextFlags, CONTEXT_FULL | CONTEXT_FLOATING_POINT | CONTEXT_EXTENDED_REGISTERS | CONTEXT_DEBUG_REGISTERS);
  debugRegs(h, cpu).forEach((v, i) => h.w32(ctx + DEBUG_OFFSETS[i], v));
  GPR_OFFSETS.forEach((off, i) => h.w32(ctx + off, m.reg(cpu, i)));
  SEG_OFFSETS.forEach((off, i) => h.w32(ctx + off, dv.getUint16(cpu + A.SEG_SEL + i * 2, true)));
  h.w32(ctx + C.Eip, eip);
  h.w32(ctx + C.EFlags, m.eflags(cpu));

  // x87: FloatSave (FNSAVE layout) and ExtendedRegisters (FXSAVE layout).
  const top = m.u32[(cpu + A.FPU_TOP) >>> 2] & 7;
  const cw = dv.getUint16(cpu + A.FPU_CW, true);
  const sw = (dv.getUint16(cpu + A.FPU_SW, true) & ~0x3800) | (top << 11);
  const valid = m.u8[cpu + A.FPU_TAG];
  let tags = 0;
  for (let p = 0; p < 8; p++) tags |= (valid & (1 << p) ? 0 : 3) << (p * 2);
  const fs = ctx + C.FloatSave;
  h.w32(fs + 0, 0xffff0000 | cw);
  h.w32(fs + 4, 0xffff0000 | sw);
  h.w32(fs + 8, 0xffff0000 | tags);
  const xs = ctx + C.ExtendedRegisters;
  dv.setUint16(xs + 0, cw, true);
  dv.setUint16(xs + 2, sw, true);
  m.u8[xs + 4] = valid;
  h.w32(xs + 24, m.u32[(cpu + A.MXCSR) >>> 2]);
  h.w32(xs + 28, 0xffff);
  for (let j = 0; j < 8; j++) {
    const v = dv.getFloat64(cpu + A.FPU_ST + ((top + j) & 7) * 8, true);
    writeF80(dv, fs + 28 + j * 10, v);
    writeF80(dv, xs + 32 + j * 16, v);
  }
  m.u8.copyWithin(xs + 160, cpu + A.XMM, cpu + A.XMM + 128);
}

/**
 * Loads a thread's CPU state from the parts of a CONTEXT its flags name;
 * returns its eip, or null when it does not include the control registers.
 */
export function restoreContext(h, cpu, ctx) {
  cpu >>>= 0; // a pointer from WebAssembly arrives as a signed i32
  const m = h.m;
  const A = m.abi.cpu;
  const dv = m.dv;
  const flags = h.u32(ctx + C.ContextFlags);
  const has = (f) => (flags & f) === f;
  if (has(CONTEXT_DEBUG_REGISTERS)) {
    const d = debugRegs(h, cpu);
    DEBUG_OFFSETS.forEach((off, i) => (d[i] = h.u32(ctx + off)));
  }
  if (has(CONTEXT_INTEGER)) {
    for (const i of [0, 1, 2, 3, 6, 7]) m.setReg(cpu, i, h.u32(ctx + GPR_OFFSETS[i]));
  }
  if (has(CONTEXT_CONTROL)) {
    m.setReg(cpu, 4, h.u32(ctx + C.Esp));
    m.setReg(cpu, 5, h.u32(ctx + C.Ebp));
    // EFlags: arithmetic flags explicitly, direction flag, system bits.
    const ef = h.u32(ctx + C.EFlags);
    m.u32[(cpu + A.FK) >>> 2] = 0;
    m.u32[(cpu + A.FR) >>> 2] = ef & 0x8d5;
    m.u32[(cpu + A.DF) >>> 2] = (ef >>> 10) & 1;
    m.u32[(cpu + A.EFLAGS_SYS) >>> 2] = ef & 0x00247300;
  }
  if (has(CONTEXT_EXTENDED_REGISTERS)) {
    const xs = ctx + C.ExtendedRegisters;
    const sw = dv.getUint16(xs + 2, true);
    const top = (sw >>> 11) & 7;
    dv.setUint16(cpu + A.FPU_CW, dv.getUint16(xs, true), true);
    dv.setUint16(cpu + A.FPU_SW, sw & ~0x3800, true);
    m.u32[(cpu + A.FPU_TOP) >>> 2] = top;
    m.u8[cpu + A.FPU_TAG] = m.u8[xs + 4];
    for (let j = 0; j < 8; j++) dv.setFloat64(cpu + A.FPU_ST + ((top + j) & 7) * 8, readF80(dv, xs + 32 + j * 16), true);
    m.u32[(cpu + A.MXCSR) >>> 2] = h.u32(xs + 24);
    m.u8.copyWithin(cpu + A.XMM, xs + 160, xs + 288);
  } else if (has(CONTEXT_FLOATING_POINT)) {
    const fs = ctx + C.FloatSave;
    const sw = h.u32(fs + 4) & 0xffff;
    const tags = h.u32(fs + 8) & 0xffff;
    const top = (sw >>> 11) & 7;
    dv.setUint16(cpu + A.FPU_CW, h.u32(fs) & 0xffff, true);
    dv.setUint16(cpu + A.FPU_SW, sw & ~0x3800, true);
    m.u32[(cpu + A.FPU_TOP) >>> 2] = top;
    let valid = 0;
    for (let p = 0; p < 8; p++) if (((tags >>> (p * 2)) & 3) !== 3) valid |= 1 << p;
    m.u8[cpu + A.FPU_TAG] = valid;
    for (let j = 0; j < 8; j++) dv.setFloat64(cpu + A.FPU_ST + ((top + j) & 7) * 8, readF80(dv, fs + 28 + j * 10), true);
  }
  return has(CONTEXT_CONTROL) ? h.u32(ctx + C.Eip) : null;
}

// ---- Raising -------------------------------------------------------------------

/**
 * Copies a record and context into a new exc_stack_layout below `esp` and
 * points the thread at KiUserExceptionDispatcher(rec, context). Returns the
 * dispatcher's address, to continue there.
 */
function dispatch(h, cpu, esp, writeRecord, writeContext) {
  cpu >>>= 0; // a pointer from WebAssembly arrives as a signed i32
  const m = h.m;
  const frame = ((esp - LAYOUT_SIZE) & ~63) >>> 0;
  const rec = frame + LAYOUT_REC;
  const ctx = frame + LAYOUT_CONTEXT;
  m.u8.fill(0, frame, frame + LAYOUT_SIZE);
  h.w32(frame, rec);
  h.w32(frame + 4, ctx);
  writeRecord(rec);
  writeContext(ctx);
  // CONTEXT_EX after the context (context_init_xstate with no xstate).
  const xctx = ctx + C.__size;
  h.w32(xctx + 0, -C.__size); // All.Offset
  h.w32(xctx + 4, C.__size + 24); // All.Length
  h.w32(xctx + 8, -C.__size); // Legacy.Offset
  h.w32(xctx + 12, C.__size); // Legacy.Length
  h.w32(xctx + 16, 0); // XState.Offset
  h.w32(xctx + 20, 25); // XState.Length
  m.setReg(cpu, 4, frame);
  // The dispatcher runs with the direction flag clear.
  m.u32[(cpu + m.abi.cpu.DF) >>> 2] = 0;
  return h.kiUserExceptionDispatcher;
}

/** Maps a translated-code fault to an exception record's code and parameters. */
function faultRecord(m, cpu, code, eip, info) {
  const F = m.abi.fault;
  switch (code) {
    case F.ACCESS_VIOLATION:
      return { code, address: eip, params: [0, info] };
    case F.ACCESS_VIOLATION_WRITE:
      return { code: F.ACCESS_VIOLATION, address: eip, params: [1, info] };
    case F.ACCESS_VIOLATION_EXECUTE:
      return { code: F.ACCESS_VIOLATION, address: eip, params: [8, info] };
    case F.GENERAL_PROTECTION:
      // The selector for an LDT selector, else all ones.
      return { code: F.ACCESS_VIOLATION, address: eip, params: [0, info || 0xffffffff] };
    case F.BREAKPOINT:
      return { code, address: eip, params: [0, 0, 0] };
    case F.SOFTWARE_INTERRUPT: {
      const n = m.u32[(cpu + m.abi.cpu.FAULT_ADDR) >>> 2];
      if (n === 3) return { code: F.BREAKPOINT, address: eip, params: [0, 0, 0] };
      if (n === 0x2d) {
        // Debug services: print and symbol (un)loading resume after
        // `int 2d` and the byte that follows it; the others are a
        // breakpoint reported after the instruction.
        const service = m.reg(cpu, 0);
        if ([1, 3, 4, 5].includes(service)) return { resume: eip + 3 };
        return { code: F.BREAKPOINT, address: eip + 2, eip: eip + 2, params: [service, m.reg(cpu, 1), m.reg(cpu, 2)] };
      }
      if (n === 0x29) {
        // __fastfail
        return { code: 0xc0000409, flags: EXCEPTION_NONCONTINUABLE, address: eip, params: [m.reg(cpu, 1)], secondChance: true };
      }
      // Other vectors are a general protection fault.
      return { code: F.ACCESS_VIOLATION, address: eip, params: [0, 0xffffffff] };
    }
    case F.UNSUPPORTED:
      return { code: F.ILLEGAL_INSTRUCTION, address: eip, params: [] };
    default:
      return { code, address: eip, params: [] };
  }
}

function writeRec(h, at, r) {
  h.w32(at + 0, r.code);
  h.w32(at + 4, r.flags ?? 0);
  h.w32(at + 8, 0);
  h.w32(at + 12, r.address);
  h.w32(at + 16, r.params.length);
  r.params.forEach((p, i) => h.w32(at + 20 + i * 4, p));
}

/** Installs fault handling for translated code on a Wine host. */
export function installExceptions(h, ntdllExport) {
  h.kiUserExceptionDispatcher = ntdllExport('KiUserExceptionDispatcher');
  // WWT_TRACE_FAULTS=N (or the host's traceFaults): the first N faults on
  // stderr, where they happened and the frames above, before the program's
  // handlers run.
  const envFaults = Number(globalThis.process?.env?.WWT_TRACE_FAULTS ?? 0);
  let traced = 0;
  // The thread's SEH frames (TEB ExceptionList), each with its handler.
  const sehChain = (cpu) => {
    const out = [];
    let f = h.u32(h.m.r32(cpu + h.m.abi.cpu.FS_BASE));
    for (let i = 0; i < 6 && f !== 0xffffffff && f > 0x10000; i++) {
      out.push(`${hex(f)}:${h.describeAddress(h.u32(f + 4))}`);
      f = h.u32(f);
    }
    return out.join(' ');
  };
  // Guest bytes from `addr`, as hex, where committed: code a program
  // decrypted or wrote at run time is only in memory, not in its file.
  const bytesAt = (addr, n) => {
    let s = '';
    for (let a = addr >>> 0; a < (addr >>> 0) + n; a++) s += a >= 0x10000 && h.vm.prot[h.vm.pageOf(a)] ? h.m.u8[a].toString(16).padStart(2, '0') : '??';
    return s;
  };
  h.m.onFault = (cpu, code, eip, info, trap) => {
    if (traced < (h.traceFaults ?? envFaults) && ++traced) {
      const t = h.threads.current;
      const regs = ['eax', 'ecx', 'edx', 'ebx', 'esp', 'ebp', 'esi', 'edi'].map((n, i) => `${n}=${hex(h.m.reg(cpu, i) >>> 0)}`).join(' ');
      h.stderr(
        new TextEncoder().encode(
          `[fault] ${hex(code >>> 0)} at ${h.describeAddress(eip >>> 0)} info ${hex(info >>> 0)} thread ${hex(t?.tid ?? 0)}; frames ${t ? h.backtrace(t, 12).join(' < ') : '-'}; seh ${sehChain(cpu)} esp ${hex(h.m.reg(cpu, 4))}\n` +
            `[fault]   ${regs}\n` +
            `[fault]   code ${hex((eip - 32) >>> 0)}: ${bytesAt(eip - 32, 32)} | ${bytesAt(eip, 16)}\n`,
        ),
      );
    }
    return raiseFault(h, cpu, code, eip, info, trap);
  };
  // Code runs from committed memory (no-execute protection is not enforced:
  // programs of the era run code from data pages).
  h.m.canExecute = (addr) => addr >= 0x10000 && h.vm.prot[h.vm.pageOf(addr)] !== 0;
}

/**
 * Where to put the exception frame for a bounds trap (fast mode), when the
 * stack pointer written back last may be above the faulting function's
 * newest pushes and locals: below the innermost handler registration on
 * the stack, if lower, and a further margin for what the function pushed
 * since (larger frames probe the stack with a call, which writes back).
 */
const TRAP_MARGIN = 0x1000;
function trapEsp(h, cpu) {
  const m = h.m;
  let esp = m.reg(cpu, 4) >>> 0;
  const teb = m.u32[(cpu + m.abi.cpu.FS_BASE) >>> 2];
  if (teb) {
    const handler = h.u32(teb); // NtTib.ExceptionList
    const base = h.u32(teb + 4); // NtTib.StackBase
    const limit = h.u32(teb + 8); // NtTib.StackLimit
    if (handler >= limit && handler < base && handler < esp) esp = handler;
    if (esp - TRAP_MARGIN >= limit) esp -= TRAP_MARGIN;
  }
  return esp;
}

/**
 * A fault in translated code: returns where to continue. `trap`: a bounds
 * trap (fast mode), with the registers as last written back.
 */
export function raiseFault(h, cpu, code, eip, info, trap = false) {
  const m = h.m;
  const r = faultRecord(m, cpu, code, eip, info);
  if (r.resume !== undefined) return r.resume;
  if (code === m.abi.fault.UNSUPPORTED) h.log(`unsupported instruction at ${hex(eip)}: raising an illegal instruction exception`);
  if (h.traceExceptions) h.log(`exception ${hex(r.code)} at ${hex(r.address)}`);
  if (r.secondChance) return secondChance(h, r.code, r.address);
  return dispatch(
    h,
    cpu,
    trap ? trapEsp(h, cpu) : m.reg(cpu, 4),
    (rec) => writeRec(h, rec, r),
    (ctx) => saveContext(h, cpu, ctx, r.eip ?? eip),
  );
}

/** NtRaiseException(rec, context, first_chance). */
export function ntRaiseException(h, cpu, rec, ctx, firstChance) {
  const code = h.u32(rec);
  const address = h.u32(rec + 12);
  if (h.traceExceptions) h.log(`NtRaiseException ${hex(code)} at ${hex(address)} first chance ${firstChance}`);
  if (!firstChance) return secondChance(h, code, address, rec);
  const n = Math.min(h.u32(rec + 16), 15);
  const esp = h.m.reg(cpu, 4);
  return {
    jump: dispatch(
      h,
      cpu,
      esp,
      (to) => h.m.u8.copyWithin(to, rec, rec + 20 + n * 4),
      (to) => {
        h.m.u8.copyWithin(to, ctx, ctx + C.__size);
        // RtlRaiseException's context: the breakpoint fixup is the kernel's.
        if (code === h.m.abi.fault.BREAKPOINT) h.w32(to + C.Eip, h.u32(to + C.Eip) - 1);
      },
    ),
  };
}

/** No handler took the exception: the process ends with its code. */
function secondChance(h, code, address, rec) {
  const params = [];
  if (rec) for (let i = 0; i < Math.min(h.u32(rec + 16), 4); i++) params.push(hex(h.u32(rec + 20 + i * 4)));
  h.log(`unhandled exception ${hex(code)} at ${hex(address)}${params.length ? ` params [${params.join(', ')}]` : ''}`);
  throw new ProcessExit(code);
}
