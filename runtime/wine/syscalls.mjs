// System calls for translated Wine, standing in for ntdll's Unix side and
// the wineserver (Milestone 2: memory, files in an in-memory file system,
// image sections, one thread). Each function receives `a(i)`, the i-th
// argument, and returns an NTSTATUS (or {jump} to resume elsewhere).
//
// The same functions serve 32-bit and 64-bit Wine: structures come from the
// host's layout (`this.L`), and pointer-sized fields (pointers, handles,
// SIZE_T) are read and written with `this.ptr` and `this.wptr`.

import { afdOpen, afdIoctl, afdClose } from './afd.mjs';
import { ProcessExit, GuestFault, hex } from '../runtime.mjs';
import { parsePe, GL_UNIXLIB, WS2_32_UNIXLIB } from './host.mjs';
import { WIN32U_UNIXLIB } from './unix.mjs';
import { aliasImportThunks } from './thunks.mjs';
import { WINED3D_UNIXLIB } from './d3d.mjs';
import { AUDIO_UNIXLIB } from './audio.mjs';
import { ntRaiseException, restoreContext } from './exceptions.mjs';

import {
  MEM_COMMIT, MEM_RESERVE, MEM_RELEASE, MEM_DECOMMIT, MEM_TOP_DOWN, MEM_MAPPED, MEM_IMAGE,
  PAGE_READWRITE, PAGE_READONLY, PAGE_EXECUTE_READ, PAGE_EXECUTE_READWRITE, PAGE_WRITECOPY,
} from './vm.mjs';

/** The Unix-library handle of DLLs whose Unix calls are not implemented. */
export const STUB_UNIXLIB = 0x3000;

export const STATUS = {
  DLL_NOT_FOUND: 0xc0000135,
  SUCCESS: 0,
  BUFFER_OVERFLOW: 0x80000005,
  NO_MORE_FILES: 0x80000006,
  NO_MORE_ENTRIES: 0x8000001a,
  NOT_IMPLEMENTED: 0xc0000002,
  INVALID_INFO_CLASS: 0xc0000003,
  INFO_LENGTH_MISMATCH: 0xc0000004,
  INVALID_HANDLE: 0xc0000008,
  INVALID_PARAMETER: 0xc000000d,
  NO_SUCH_FILE: 0xc000000f,
  END_OF_FILE: 0xc0000011,
  NO_MEMORY: 0xc0000017,
  CONFLICTING_ADDRESSES: 0xc0000018,
  NOT_MAPPED_VIEW: 0xc0000019,
  UNABLE_TO_FREE_VM: 0xc000001a,
  ACCESS_DENIED: 0xc0000022,
  ACCESS_VIOLATION: 0xc0000005,
  BUFFER_TOO_SMALL: 0xc0000023,
  OBJECT_TYPE_MISMATCH: 0xc0000024,
  OBJECT_NAME_INVALID: 0xc0000033,
  OBJECT_NAME_NOT_FOUND: 0xc0000034,
  OBJECT_NAME_COLLISION: 0xc0000035,
  OBJECT_PATH_NOT_FOUND: 0xc000003a,
  INVALID_PAGE_PROTECTION: 0xc0000045,
  MEMORY_NOT_ALLOCATED: 0xc00000a0,
  FILE_IS_A_DIRECTORY: 0xc00000ba,
  NOT_SUPPORTED: 0xc00000bb,
  NOT_A_DIRECTORY: 0xc0000103,
  INVALID_IMAGE_FORMAT: 0xc000007b,
  IMAGE_NOT_AT_BASE: 0x40000003,
  OBJECT_NAME_EXISTS: 0x40000000,
  TIMEOUT: 0x102,
  PENDING: 0x103,
  INVALID_ADDRESS: 0xc0000141,
};

const SEC_IMAGE = 0x1000000;
const FILE_ATTRIBUTE_DIRECTORY = 0x10;
const FILE_ATTRIBUTE_ARCHIVE = 0x20;
const FILE_ATTRIBUTE_NORMAL = 0x80;
const FILETIME_2020 = 132223104000000000n;

// ---- helpers -----------------------------------------------------------------

/** Writes an IO_STATUS_BLOCK. */
function iosb(h, addr, status, info) {
  if (!addr) return;
  h.w32(addr, status);
  h.wptr(addr + h.L.IO_STATUS_BLOCK.Information, info);
}

// ---- File contents ----
//
// A file's bytes live in `h.files` (path -> Uint8Array whose length is the
// file size). Every handle reads them from there, so a write through one
// handle is seen by the others. Growing files keep spare capacity in the
// view's buffer, so appending does not copy the whole file each time.

/** The current bytes of an open regular file. */
function fileBytes(h, f) {
  return (f.path && h.files.get(f.path)) || f.data || new Uint8Array(0);
}

/** Sets a file's size, zero-filling any new bytes. */
function setFileSize(h, f, size) {
  const cur = fileBytes(h, f);
  let next;
  if (size <= cur.length) {
    next = cur.subarray(0, size);
  } else if (cur.byteOffset === 0 && size <= cur.buffer.byteLength && !(cur.buffer instanceof SharedArrayBuffer)) {
    next = new Uint8Array(cur.buffer, 0, size);
    next.fill(0, cur.length);
  } else {
    const cap = Math.max(size, cur.length * 2, 0x10000);
    let buf = h.spareFileBuffer;
    if (buf && buf.byteLength >= cap) {
      h.spareFileBuffer = null;
    } else {
      buf = new ArrayBuffer(cap);
    }
    next = new Uint8Array(buf, 0, size);
    next.set(cur);
    next.fill(0, cur.length);
  }
  if (f.path) h.files.set(f.path, next);
  f.data = next;
  return next;
}

/** Writes `bytes` at `pos`, extending the file as needed. */
function writeFileBytes(h, f, pos, bytes) {
  let data = fileBytes(h, f);
  if (pos + bytes.length > data.length) data = setFileSize(h, f, pos + bytes.length);
  data.set(bytes, pos);
  h.onFileWrite?.(f.path);
}

/** Deletes files whose last handle closed with a deletion pending. */
function closeFile(h, f) {
  if (!f.path || !h.deletePending?.has(f.path)) return;
  for (const o of h.handles.values()) if (o !== f && o.type === 'file' && o.path === f.path) return;
  h.deletePending.delete(f.path);
  if (f.dir) {
    h.dirs.delete(f.path);
    return;
  }
  const bytes = h.files.get(f.path);
  h.files.delete(f.path);
  // Keep the largest buffer of a deleted file for the next file that grows:
  // a program that deletes and recreates a file (SQLite's journal, once per
  // transaction) then reuses it instead of growing a new one from nothing.
  if (bytes && bytes.byteOffset === 0 && !(bytes.buffer instanceof SharedArrayBuffer)) {
    if (!h.spareFileBuffer || bytes.buffer.byteLength > h.spareFileBuffer.byteLength) h.spareFileBuffer = bytes.buffer;
  }
}

/** Resolves an OBJECT_ATTRIBUTES to a DOS path. */
function oaPath(h, oa) {
  if (!oa) return null;
  const root = h.ptr(oa + h.L.OBJECT_ATTRIBUTES.RootDirectory);
  const name = oaName(h, oa);
  if (name === null) return null;
  return h.ntToDos(name, root);
}

function oaName(h, oa) {
  if (!oa) return null;
  return h.ustr(h.ptr(oa + h.L.OBJECT_ATTRIBUTES.ObjectName));
}

/** Rounds down to a multiple of `n` (exact above 4 GB, unlike `& ~(n - 1)`). */
function alignDown(v, n) {
  return v - (v % n);
}

function fileInfo(h, path) {
  const f = h.fileAt(path);
  if (f) return { dir: false, size: f.length, data: f };
  if (h.isDir(path)) return { dir: true, size: 0 };
  return null;
}

function writeTimes(h, at) {
  for (let i = 0; i < 4; i++) h.w64(at + i * 8, FILETIME_2020);
}

function memInfo(h, addr, buf) {
  const q = h.vm.query(addr);
  if (!q) return STATUS.INVALID_PARAMETER;
  const M = h.L.MEMORY_BASIC_INFORMATION;
  h.m.u8.fill(0, buf, buf + M.__size);
  h.wptr(buf + M.BaseAddress, q.base);
  h.wptr(buf + M.AllocationBase, q.allocBase);
  h.w32(buf + M.AllocationProtect, q.allocProt);
  h.wptr(buf + M.RegionSize, q.size);
  h.w32(buf + M.State, q.state);
  h.w32(buf + M.Protect, q.prot);
  h.w32(buf + M.Type, q.type);
  return STATUS.SUCCESS;
}

/** Restores registers from a CONTEXT and resumes there. */
function continueContext(h, cpu, ctx) {
  if (h.x64) return continueContext64(h, cpu, ctx);
  const eip = restoreContext(h, cpu, ctx);
  return eip === null ? STATUS.SUCCESS : { jump: eip };
}

/** The x86-64 CONTEXT: sixteen registers, flags and rip. */
function continueContext64(h, cpu, ctx) {
  const C = h.L.CONTEXT;
  const m = h.m;
  const regs = ['Rax', 'Rcx', 'Rdx', 'Rbx', 'Rsp', 'Rbp', 'Rsi', 'Rdi', 'R8', 'R9', 'R10', 'R11', 'R12', 'R13', 'R14', 'R15'];
  regs.forEach((r, i) => m.setReg64(cpu, i, h.u64(ctx + C[r])));
  const ef = h.u32(ctx + C.EFlags);
  const abi = m.abi;
  m.w32(cpu + abi.cpu.FK, 0);
  m.dv.setBigUint64(cpu + abi.cpu64.FR, BigInt(ef & 0x8d5), true);
  m.w32(cpu + abi.cpu.DF, (ef >>> 10) & 1);
  return { jump: h.ptr(ctx + C.Rip) };
}

// ---- system calls ------------------------------------------------------------

export const SYSCALLS = {
  // -- virtual memory
  NtAllocateVirtualMemory(a) {
    const [proc, pbase, , psize, type, prot] = [a(0), a(1), a(2), a(3), a(4), a(5)];
    let base = this.ptr(pbase);
    let size = this.ptr(psize);
    if (!size) return STATUS.INVALID_PARAMETER;
    void proc;
    if (type & MEM_RESERVE || !base) {
      const want = base ? alignDown(base, 0x10000) : 0;
      const got = this.vm.reserve(want, size + (base ? base - want : 0), { prot, topDown: !!(type & MEM_TOP_DOWN) });
      if (!got) return base ? STATUS.CONFLICTING_ADDRESSES : STATUS.NO_MEMORY;
      if (!base) base = got;
      else size += base - want, (base = want);
    }
    if (type & MEM_COMMIT) {
      const start = alignDown(base, 0x1000);
      const end = Math.ceil((base + size) / 0x1000) * 0x1000;
      if (!this.vm.commit(start, end - start, prot)) return STATUS.CONFLICTING_ADDRESSES;
      base = start;
      size = end - start;
    } else {
      size = Math.ceil(size / 0x1000) * 0x1000;
    }
    this.wptr(pbase, base);
    this.wptr(psize, size);
    return STATUS.SUCCESS;
  },
  NtFreeVirtualMemory(a) {
    const [, pbase, psize, type] = [a(0), a(1), a(2), a(3)];
    const base = this.ptr(pbase);
    const size = this.ptr(psize);
    if (type & MEM_RELEASE) {
      const r = this.vm.regionAt(base);
      if (!r || !this.vm.release(r.base)) return STATUS.MEMORY_NOT_ALLOCATED;
      this.wptr(pbase, r.base);
      this.wptr(psize, r.size);
    } else if (type & MEM_DECOMMIT) {
      this.vm.decommit(base, size || 0x1000);
    }
    return STATUS.SUCCESS;
  },
  NtProtectVirtualMemory(a) {
    const [, pbase, psize, prot, pold] = [a(0), a(1), a(2), a(3), a(4)];
    const base = alignDown(this.ptr(pbase), 0x1000);
    const end = Math.ceil((this.ptr(pbase) + this.ptr(psize)) / 0x1000) * 0x1000;
    const old = this.vm.protect(base, end - base, prot);
    if (old < 0) return STATUS.NOT_MAPPED_VIEW;
    if (pold) this.w32(pold, old);
    // The loader restores an image's protection after filling its imports.
    if (this.aliasThunks) {
      for (const [b, img] of this.images) {
        if (base >= b && base < b + img.size) aliasImportThunks(this.m, b, b + img.size, this.x64);
      }
    }
    this.wptr(pbase, base);
    this.wptr(psize, end - base);
    return STATUS.SUCCESS;
  },
  NtQueryVirtualMemory(a) {
    const [, addr, cls, buf, len, pret] = [a(0), a(1), a(2), a(3), a(4), a(5)];
    if (cls === 0) {
      const n = this.L.MEMORY_BASIC_INFORMATION.__size;
      if (len < n) return STATUS.INFO_LENGTH_MISMATCH;
      const s = memInfo(this, addr, buf);
      if (pret) this.wptr(pret, n);
      return s;
    }
    if (cls === 2) {
      // MemoryMappedFilenameInformation
      const img = [...this.images.entries()].find(([b, i]) => addr >= b && addr < b + i.size);
      if (!img) return STATUS.INVALID_ADDRESS;
      const name = '\\Device\\HarddiskVolume1' + img[1].path.slice(2);
      const us = this.L.UNICODE_STRING.__size;
      this.putUstr(buf, buf + us, name);
      if (pret) this.wptr(pret, us + name.length * 2 + 2);
      return STATUS.SUCCESS;
    }
    if (cls === 1000) {
      // MemoryWineUnixFuncs: the handle a DLL uses for __wine_unix_call.
      // win32u's Unix side is in the Emscripten module (./unix.mjs).
      const img = this.images.get(addr);
      // ... and wined3d's WebGPU backend in the host (./d3d.mjs).
      const handle = !img ? 0
        : this.unix && /\\win32u\.dll$/i.test(img.path) ? WIN32U_UNIXLIB
        : /\\wined3d\.dll$/i.test(img.path) ? WINED3D_UNIXLIB
        : 0;
      if (handle) {
        this.w32(buf, handle);
        this.w32(buf + 4, 0);
        return STATUS.SUCCESS;
      }
      // DLLs that refuse to load without a Unix library get one whose
      // calls all fail (STATUS_NOT_IMPLEMENTED), so that programs importing
      // them start: crypt32 (the system's certificate stores). (ws2_32 has
      // its own below: matched here first, it got this one, and its name
      // lookups returned success with nothing filled in.)
      if (img && /\\crypt32\.dll$/i.test(img.path)) {
        this.w32(buf, STUB_UNIXLIB);
        this.w32(buf + 4, 0);
        return STATUS.SUCCESS;
      }
      // The audio driver mmdevapi loads (./audio.mjs).
      if (this.audio && img && /\\winepulse\.drv$/i.test(img.path)) {
        this.w32(buf, AUDIO_UNIXLIB);
        this.w32(buf + 4, 0);
        return STATUS.SUCCESS;
      }
      if (img && /\\ws2_32\.dll$/i.test(img.path)) {
        this.w32(buf, WS2_32_UNIXLIB);
        this.w32(buf + 4, 0);
        return STATUS.SUCCESS;
      }
      if (img && /\\opengl32\.dll$/i.test(img.path)) {
        this.w32(buf, GL_UNIXLIB);
        this.w32(buf + 4, 0);
        return STATUS.SUCCESS;
      }
      return STATUS.DLL_NOT_FOUND;
    }
    return STATUS.INVALID_INFO_CLASS;
  },
  // -- user callbacks (see WineHost.userCallback)
  NtCallbackReturn(a) {
    const n = this.callbackResults.length;
    if (!n) return 0xc0000258; // STATUS_NO_CALLBACK_ACTIVE
    this.callbackResults[n - 1] = { ptr: a(0), len: a(1), status: a(2) };
    return { jump: this.m.stopAddress };
  },
  NtFlushInstructionCache() {
    return STATUS.SUCCESS;
  },
  NtFlushProcessWriteBuffers() {
    return STATUS.SUCCESS;
  },

  // -- handles and objects
  NtClose(a) {
    const h = a(0);
    if (!this.handles.has(h)) return STATUS.INVALID_HANDLE;
    const obj = this.handles.get(h);
    this.handles.delete(h);
    if (obj?.type === 'file') closeFile(this, obj);
    if (obj?.type === 'socket' && ![...this.handles.values()].includes(obj)) afdClose(this, obj);
    return STATUS.SUCCESS;
  },
  NtDuplicateObject(a) {
    const [, src, , pdst] = [a(0), a(1), a(2), a(3)];
    const obj = this.object(src);
    if (!obj) return STATUS.INVALID_HANDLE;
    if (pdst) this.wptr(pdst, this.newHandle(obj));
    return STATUS.SUCCESS;
  },
  NtCreateEvent(a) {
    this.wptr(a(0), this.newHandle({ type: 'event', signaled: !!a(4), manual: a(3) === 0 }));
    return STATUS.SUCCESS;
  },
  NtCreateKeyedEvent(a) {
    this.wptr(a(0), this.newHandle({ type: 'keyedevent' }));
    return STATUS.SUCCESS;
  },
  NtCreateMutant(a) {
    this.wptr(a(0), this.newHandle({ type: 'mutant' }));
    return STATUS.SUCCESS;
  },
  NtCreateSemaphore(a) {
    this.wptr(a(0), this.newHandle({ type: 'semaphore', count: a(3), max: a(4) }));
    return STATUS.SUCCESS;
  },
  NtSetEvent(a) {
    const e = this.object(a(0));
    if (e) e.signaled = true;
    return STATUS.SUCCESS;
  },
  NtResetEvent(a) {
    const e = this.object(a(0));
    if (e) e.signaled = false;
    return STATUS.SUCCESS;
  },
  NtClearEvent(a) {
    const e = this.object(a(0));
    if (e) e.signaled = false;
    return STATUS.SUCCESS;
  },
  NtReleaseMutant() {
    return STATUS.SUCCESS;
  },
  NtReleaseSemaphore() {
    return STATUS.SUCCESS;
  },

  // -- process, thread, system
  NtQueryInformationProcess(a) {
    const [handle, cls, buf, len, pret] = [a(0), a(1), a(2), a(3), a(4)];
    const ret = (n) => (pret && this.w32(pret, n), STATUS.SUCCESS);
    if (!handle) return STATUS.INVALID_HANDLE;
    switch (cls) {
      case 0: { // ProcessBasicInformation
        const B = this.L.PROCESS_BASIC_INFORMATION;
        if (len < B.__size) return STATUS.INFO_LENGTH_MISMATCH;
        this.m.u8.fill(0, buf, buf + B.__size);
        this.wptr(buf + B.PebBaseAddress, this.peb);
        this.wptr(buf + B.PebBaseAddress + this.ps, 1); // AffinityMask
        this.wptr(buf + B.UniqueProcessId, 0x20);
        return ret(B.__size);
      }
      case 7: // ProcessDebugPort
      case 30: // ProcessDebugObjectHandle
        this.wptr(buf, 0);
        return cls === 30 ? 0xc0000353 : ret(this.ps);
      case 12: // ProcessDefaultHardErrorMode
      case 31: // ProcessDebugFlags
        this.w32(buf, cls === 31 ? 1 : 0);
        return ret(4);
      case 21: // ProcessAffinityMask: the one processor
        if (len < this.ps) return STATUS.INFO_LENGTH_MISMATCH;
        this.wptr(buf, 1);
        return ret(this.ps);
      case 26: // ProcessWow64Information
        this.wptr(buf, 0);
        return ret(this.ps);
      case 34: // ProcessExecuteFlags
        this.w32(buf, 0x30); // MEM_EXECUTE_OPTION_PERMANENT | DISABLE_ATL_THUNK_EMULATION... permissive
        return ret(4);
      case 36: // ProcessCookie
        this.w32(buf, 0x12345678);
        return ret(4);
      case 37: { // ProcessImageInformation (SECTION_IMAGE_INFORMATION)
        sectionImageInfo(this, this.exe.info, this.exe.base, buf);
        return ret(this.L.SECTION_IMAGE_INFORMATION.__size);
      }
      case 27: // ProcessImageFileName
      case 43: { // ProcessImageFileNameWin32
        const name = cls === 27 ? '\\Device\\HarddiskVolume1' + this.exePath.slice(2) : this.exePath;
        const us = this.L.UNICODE_STRING.__size;
        // Too small: the length needed, for the caller to allocate.
        if (len < us + name.length * 2 + 2) return (ret(us + name.length * 2 + 2), STATUS.INFO_LENGTH_MISMATCH);
        this.putUstr(buf, buf + us, name);
        return ret(us + name.length * 2 + 2);
      }
      case 20: // ProcessHandleCount
        this.w32(buf, this.handles.size);
        return ret(4);
      case 18: // ProcessPriorityClass
        this.m.u8[buf] = 0;
        this.m.u8[buf + 1] = 2;
        return ret(2);
      default:
        if (cls === 3) {
          // ProcessVmCounters: VM_COUNTERS(_EX), all zero but the private bytes.
          if (len < 44) return STATUS.INFO_LENGTH_MISMATCH;
          const n = Math.min(len, 48);
          this.m.u8.fill(0, buf, buf + n);
          if (n >= 48) this.w32(buf + 44, 0x2000000);
          return ret(n);
        }
        this.fixme(`NtQueryInformationProcess class ${cls} not implemented`);
        return STATUS.INVALID_INFO_CLASS;
    }
  },
  NtSetInformationProcess() {
    return STATUS.SUCCESS;
  },
  NtSetInformationThread() {
    return STATUS.SUCCESS;
  },
  NtQuerySystemInformation(a) {
    const [cls, buf, len, pret] = [a(0), a(1), a(2), a(3)];
    const ret = (n) => (pret && this.w32(pret, n), STATUS.SUCCESS);
    switch (cls) {
      case 0: // SystemBasicInformation
      case 0x3e: { // SystemEmulationBasicInformation
        const B = this.L.SYSTEM_BASIC_INFORMATION;
        if (len < B.__size) return STATUS.INFO_LENGTH_MISMATCH;
        this.m.u8.fill(0, buf, buf + B.__size);
        this.w32(buf + B.PageSize - 4, 156250); // KeMaximumIncrement
        this.w32(buf + B.PageSize, 0x1000);
        this.w32(buf + B.MmNumberOfPhysicalPages, 0x40000);
        this.w32(buf + B.MmLowestPhysicalPage, 1);
        this.w32(buf + B.MmHighestPhysicalPage, 0x40000);
        this.w32(buf + B.AllocationGranularity, 0x10000);
        this.wptr(buf + B.LowestUserAddress, 0x10000);
        this.wptr(buf + B.HighestUserAddress, this.m.thunkBase - 1);
        this.wptr(buf + B.ActiveProcessorsAffinityMask, 1);
        this.m.u8[buf + B.NumberOfProcessors] = 1;
        return ret(B.__size);
      }
      case 1: // SystemCpuInformation
      case 0x3f: {
        this.m.u8.fill(0, buf, buf + Math.min(len, 32));
        this.w16(buf, this.x64 ? 9 : 0); // PROCESSOR_ARCHITECTURE_AMD64 / _INTEL
        this.w16(buf + 2, 6); // level
        this.w16(buf + 4, 0x0f29);
        this.w16(buf + 6, 1); // MaximumProcessors (GetSystemInfo's dwNumberOfProcessors)
        this.w32(buf + 8, 0x1 | 0x8 | 0x40); // feature set
        return ret(12);
      }
      case 3: // SystemTimeOfDayInformation
        this.m.u8.fill(0, buf, buf + Math.min(len, 48));
        this.w64(buf, BigInt(Date.now()) * 10000n + 116444736000000000n);
        this.w64(buf + 8, BigInt(Date.now()) * 10000n + 116444736000000000n);
        return ret(Math.min(len, 48));
      case 2: { // SystemPerformanceInformation: free memory (GlobalMemoryStatusEx)
        if (len < 0x138) return STATUS.INFO_LENGTH_MISMATCH;
        this.m.u8.fill(0, buf, buf + 0x138);
        this.w32(buf + 0x2c, 0x30000); // AvailablePages
        this.w32(buf + 0x30, 0x8000); // TotalCommittedPages
        this.w32(buf + 0x34, 0x40000); // TotalCommitLimit
        this.w32(buf + 0x38, 0x8000); // PeakCommitment
        return ret(0x138);
      }
      case 0x4c: // SystemFirmwareTableInformation etc.
      default:
        this.fixme(`NtQuerySystemInformation class ${hex(cls)} not implemented`);
        // No length needed: callers that size a buffer from it (kernel32's
        // firmware tables) then see the class as unavailable.
        ret(0);
        return STATUS.INVALID_INFO_CLASS;
    }
  },
  NtQuerySystemInformationEx(a, cpu, base) {
    return SYSCALLS.NtQuerySystemInformation.call(this, (i) => a([0, 3, 4, 5][i]), cpu, base);
  },
  NtQuerySystemTime(a) {
    this.w64(a(0), BigInt(Date.now()) * 10000n + 116444736000000000n);
    return STATUS.SUCCESS;
  },
  NtQueryPerformanceCounter(a) {
    if (!this.writable(a(0), 8) || (a(1) && !this.writable(a(1), 8))) return STATUS.ACCESS_VIOLATION;
    this.w64(a(0), BigInt(Math.floor(performance.now() * 10000)));
    if (a(1)) this.w64(a(1), 10000000n);
    return STATUS.SUCCESS;
  },
  NtQueryTimerResolution(a) {
    this.w32(a(0), 156250);
    this.w32(a(1), 5000);
    this.w32(a(2), 156250);
    return STATUS.SUCCESS;
  },
  NtGetTickCount() {
    return Math.floor(performance.now()) >>> 0;
  },
  NtTerminateProcess(a) {
    if (a(0) === 0) {
      // Every thread but this one (ExitProcess does this first).
      for (const t of this.threads.live()) {
        if (t === this.threads.current) continue;
        this.unix?.call('wasm_forget_thread', 'p', t.teb);
        if (t.state === 'waiting') t.killed = true;
        else this.endThread(t, a(1));
      }
      return STATUS.SUCCESS;
    }
    throw new ProcessExit(a(1));
  },
  NtContinue(a, cpu) {
    return continueContext(this, cpu, a(0));
  },
  NtContinueEx(a, cpu) {
    return continueContext(this, cpu, a(0));
  },
  NtRaiseException(a, cpu) {
    if (this.x64) return SYSCALLS64.NtRaiseException.call(this, a, cpu);
    return ntRaiseException(this, cpu, a(0), a(1), a(2));
  },
  NtGetCurrentProcessorNumber() {
    return 0;
  },
  NtAllocateLocallyUniqueId(a) {
    this.w64(a(0), BigInt(++this.luid || (this.luid = 1000)));
    return STATUS.SUCCESS;
  },
  NtQueryInstallUILanguage(a) {
    this.w16(a(0), 0x409);
    return STATUS.SUCCESS;
  },
  NtQueryDefaultUILanguage(a) {
    this.w16(a(0), 0x409);
    return STATUS.SUCCESS;
  },
  NtQueryDefaultLocale(a) {
    this.w32(a(1), a(0) ? this.userLocale : 0x409);
    return STATUS.SUCCESS;
  },

  // -- NLS
  NtInitializeNlsFiles(a) {
    const r = mapReadOnlyFile(this, 'c:\\windows\\system32\\locale.nls');
    if (!r) return STATUS.OBJECT_NAME_NOT_FOUND;
    this.wptr(a(0), r.base);
    this.w32(a(1), 0x409);
    if (a(2)) this.w64(a(2), r.size);
    return STATUS.SUCCESS;
  },
  NtGetNlsSectionPtr(a) {
    const [type, id, , pptr, psize] = [a(0), a(1), a(2), a(3), a(4)];
    const norm = { 1: 'normnfc', 2: 'normnfd', 5: 'normnfkc', 6: 'normnfkd', 13: 'normidna' };
    const name =
      type === 9 ? 'sortdefault' : type === 10 ? 'l_intl' : type === 11 ? `c_${String(id).padStart(3, '0')}` : type === 12 ? norm[id] : null;
    if (!name) return STATUS.INVALID_PARAMETER;
    const r = mapReadOnlyFile(this, `c:\\windows\\system32\\${name}.nls`);
    if (!r) return STATUS.OBJECT_NAME_NOT_FOUND;
    this.wptr(pptr, r.base);
    if (psize) this.wptr(psize, r.size);
    return STATUS.SUCCESS;
  },

  // -- registry: empty for now
  NtOpenKey(a) {
    this.log(`NtOpenKey ${oaName(this, a(2))}`);
    return STATUS.OBJECT_NAME_NOT_FOUND;
  },
  NtOpenKeyEx(a) {
    this.log(`NtOpenKeyEx ${oaName(this, a(2))}`);
    return STATUS.OBJECT_NAME_NOT_FOUND;
  },
  NtCreateKey(a) {
    this.log(`NtCreateKey ${oaName(this, a(2))}`);
    return STATUS.OBJECT_NAME_NOT_FOUND;
  },

  // -- files
  NtCreateFile(a) {
    const [ph, , oa, piosb, , attrs, , disposition, options] = [a(0), a(1), a(2), a(3), a(4), a(5), a(6), a(7), a(8)];
    void attrs;
    return openFile(this, ph, oa, piosb, disposition, options);
  },
  NtOpenFile(a) {
    return openFile(this, a(0), a(2), a(3), 1 /* FILE_OPEN */, a(5));
  },
  NtQueryAttributesFile(a) {
    const path = oaPath(this, a(0));
    const fi = path && fileInfo(this, path);
    this.log(`NtQueryAttributesFile ${path} -> ${fi ? (fi.dir ? 'dir' : fi.size) : 'missing'}`);
    if (!fi) return STATUS.OBJECT_NAME_NOT_FOUND;
    const buf = a(1);
    writeTimes(this, buf);
    this.w32(buf + 32, fi.dir ? FILE_ATTRIBUTE_DIRECTORY : FILE_ATTRIBUTE_ARCHIVE);
    return STATUS.SUCCESS;
  },
  NtQueryFullAttributesFile(a) {
    const path = oaPath(this, a(0));
    const fi = path && fileInfo(this, path);
    if (!fi) return STATUS.OBJECT_NAME_NOT_FOUND;
    const buf = a(1);
    writeTimes(this, buf);
    this.w64(buf + 32, fi.size);
    this.w64(buf + 40, fi.size);
    this.w32(buf + 48, fi.dir ? FILE_ATTRIBUTE_DIRECTORY : FILE_ATTRIBUTE_ARCHIVE);
    return STATUS.SUCCESS;
  },
  NtQueryInformationFile(a) {
    const [h, piosb, buf, len, cls] = [a(0), a(1), a(2), a(3), a(4)];
    const f = this.object(h);
    if (!f || f.type !== 'file') return STATUS.INVALID_HANDLE;
    const size = f.dir || f.std ? 0 : fileBytes(this, f).length;
    const done = (n) => (iosb(this, piosb, 0, n), STATUS.SUCCESS);
    switch (cls) {
      case 4: // FileBasicInformation
        writeTimes(this, buf);
        this.w32(buf + 32, f.dir ? FILE_ATTRIBUTE_DIRECTORY : FILE_ATTRIBUTE_ARCHIVE);
        return done(40);
      case 5: // FileStandardInformation
        this.w64(buf, size);
        this.w64(buf + 8, size);
        this.w32(buf + 16, 1);
        this.m.u8[buf + 20] = 0;
        this.m.u8[buf + 21] = f.dir ? 1 : 0;
        return done(24);
      case 14: // FilePositionInformation
        this.w64(buf, f.pos ?? 0);
        return done(8);
      case 20: // FileEndOfFileInformation
        this.w64(buf, size);
        return done(8);
      case 34: // FileNetworkOpenInformation
        writeTimes(this, buf);
        this.w64(buf + 32, size);
        this.w64(buf + 40, size);
        this.w32(buf + 48, f.dir ? FILE_ATTRIBUTE_DIRECTORY : FILE_ATTRIBUTE_ARCHIVE);
        return done(56);
      case 68: // FileStatInformation (the CRT's stat): id, four times, sizes, attributes, tag, links, access
        if (len < 72) return STATUS.INFO_LENGTH_MISMATCH;
        this.m.u8.fill(0, buf, buf + 72);
        this.w64(buf, 0);
        writeTimes(this, buf + 8);
        this.w64(buf + 40, (size + 4095) & ~4095);
        this.w64(buf + 48, size);
        this.w32(buf + 56, f.dir ? FILE_ATTRIBUTE_DIRECTORY : FILE_ATTRIBUTE_ARCHIVE);
        this.w32(buf + 64, 1);
        this.w32(buf + 68, 0x1f01ff); // FILE_ALL_ACCESS
        return done(72);
      case 35: // FileAttributeTagInformation
        this.w32(buf, f.dir ? FILE_ATTRIBUTE_DIRECTORY : FILE_ATTRIBUTE_ARCHIVE);
        this.w32(buf + 4, 0);
        return done(8);
      case 9: { // FileNameInformation
        const name = (f.path ?? '').slice(2);
        this.w32(buf, name.length * 2);
        for (let i = 0; i < name.length && 4 + i * 2 < len; i++) this.w16(buf + 4 + i * 2, name.charCodeAt(i));
        return done(4 + name.length * 2);
      }
      default:
        this.fixme(`NtQueryInformationFile class ${cls} not implemented`);
        return STATUS.INVALID_INFO_CLASS;
    }
  },
  NtQueryVolumeInformationFile(a) {
    const [, piosb, buf, , cls] = [a(0), a(1), a(2), a(3), a(4)];
    if (cls === 4) {
      // FileFsDeviceInformation: FILE_DEVICE_DISK
      this.w32(buf, 7);
      this.w32(buf + 4, 0);
      iosb(this, piosb, 0, 8);
      return STATUS.SUCCESS;
    }
    return STATUS.INVALID_INFO_CLASS;
  },
  NtSetInformationFile(a) {
    const [h, piosb, buf, , cls] = [a(0), a(1), a(2), a(3), a(4)];
    const f = this.object(h);
    if (!f) return STATUS.INVALID_HANDLE;
    switch (cls) {
      case 14: // FilePositionInformation
        f.pos = Number(this.u64(buf));
        break;
      case 20: // FileEndOfFileInformation
        if (f.type === 'file' && !f.dir && !f.std) setFileSize(this, f, Number(this.u64(buf)));
        break;
      case 13: // FileDispositionInformation: BOOLEAN DeleteFile
      case 64: {
        // FileDispositionInformationEx: FILE_DISPOSITION_DELETE (1)
        const del = cls === 13 ? this.m.u8[buf] !== 0 : (this.u32(buf) & 1) !== 0;
        if (!f.path) break;
        this.deletePending ??= new Set();
        if (del) this.deletePending.add(f.path);
        else this.deletePending.delete(f.path);
        break;
      }
    }
    iosb(this, piosb, 0, 0);
    return STATUS.SUCCESS;
  },
  // One process: byte-range locks never conflict.
  NtLockFile(a) {
    iosb(this, a(4), 0, 0);
    return STATUS.SUCCESS;
  },
  NtUnlockFile(a) {
    iosb(this, a(1), 0, 0);
    return STATUS.SUCCESS;
  },
  NtReadFile(a) {
    const [h, , , , piosb, buf, len, poff] = [a(0), a(1), a(2), a(3), a(4), a(5), a(6), a(7)];
    const f = this.object(h);
    if (!f || f.type !== 'file') return STATUS.INVALID_HANDLE;
    if (f.std) {
      iosb(this, piosb, STATUS.END_OF_FILE, 0);
      return STATUS.END_OF_FILE;
    }
    let pos = f.pos ?? 0;
    if (poff) {
      const o = this.u64(poff);
      if (o !== 0xfffffffffffffffen && o !== 0xffffffffffffffffn) pos = Number(o);
    }
    const data = fileBytes(this, f);
    const n = Math.max(0, Math.min(len, data.length - pos));
    if (n === 0 && len > 0) {
      iosb(this, piosb, STATUS.END_OF_FILE, 0);
      return STATUS.END_OF_FILE;
    }
    this.m.u8.set(data.subarray(pos, pos + n), buf);
    f.pos = pos + n;
    iosb(this, piosb, 0, n);
    return STATUS.SUCCESS;
  },
  NtWriteFile(a) {
    const [h, , , , piosb, buf, len, poff] = [a(0), a(1), a(2), a(3), a(4), a(5), a(6), a(7)];
    const f = this.object(h);
    if (!f || f.type !== 'file') return STATUS.INVALID_HANDLE;
    if (f.std === 'stdout') this.stdout(this.m.u8.slice(buf, buf + len));
    else if (f.std === 'stderr') this.stderr(this.m.u8.slice(buf, buf + len));
    else if (!f.dir) {
      // An explicit offset, or FILE_WRITE_TO_END_OF_FILE (-1), or the file
      // pointer (FILE_USE_FILE_POINTER_POSITION, -2, or no offset).
      let pos = f.pos ?? 0;
      if (poff) {
        const o = this.u64(poff);
        if (o === 0xffffffffffffffffn) pos = fileBytes(this, f).length;
        else if (o !== 0xfffffffffffffffen) pos = Number(o);
      }
      writeFileBytes(this, f, pos, this.m.u8.subarray(buf, buf + len));
      f.pos = pos + len;
    }
    iosb(this, piosb, 0, len);
    return STATUS.SUCCESS;
  },
  NtFlushBuffersFile(a) {
    iosb(this, a(1), 0, 0);
    return STATUS.SUCCESS;
  },
  NtDeviceIoControlFile(a) {
    const code = a(5);
    const obj = this.object(a(0));
    if (obj?.type === 'socket') return afdIoctl(this, obj, a, iosb);
    this.log(`NtDeviceIoControlFile ${hex(code)} on ${hex(a(0))}`);
    return STATUS.NOT_SUPPORTED;
  },
  NtFsControlFile() {
    return STATUS.NOT_SUPPORTED;
  },
  NtQueryDirectoryFile(a) {
    const [handle, piosb, buf, len, cls, single, pmask, restart] = [a(0), a(4), a(5), a(6), a(7), a(8) & 0xff, a(9), a(10) & 0xff];
    const dir = this.object(handle);
    if (!dir || dir.type !== 'file') return STATUS.INVALID_HANDLE;
    if (!dir.dir) return STATUS.INVALID_PARAMETER;
    // FILE_INFORMATION_CLASS -> offset of FileName (and of FileId, if any).
    // 60/63 (FileIdExtd[Both]DirectoryInformation, which Wine 11's
    // FindFirstFileEx asks for) have a 128-bit FileId; the low half is set.
    const LAYOUT = { 1: [64], 2: [68], 3: [94], 12: [12], 37: [104, 96], 38: [80, 72], 60: [88, 72], 63: [114, 72] };
    const layout = LAYOUT[cls];
    if (!layout) {
      this.fixme(`NtQueryDirectoryFile class ${cls} not implemented`);
      return STATUS.INVALID_INFO_CLASS;
    }
    // The listing is taken on the first call (or a restart), with its mask.
    if (restart || !dir.listing) {
      const mask = pmask ? this.ustr(pmask) : '*';
      // NT masks: * and ?, and the DOS wildcards kernelbase turns *.* and
      // friends into: < (any run, up to the last dot), > (any character, or
      // none at a dot or the end) and " (a dot, or the end).
      const pattern = mask
        .toLowerCase()
        .replace(/[.+^${}()|[\]\\]/g, '\\$&')
        .replace(/[*<]/g, '.*')
        .replace(/\?/g, '.')
        .replace(/>/g, '.?')
        .replace(/"/g, '(?:\\.|$)');
      const re = new RegExp(`^${pattern}$`);
      const prefix = dir.path + '\\';
      const names = new Map(/^[a-z]:$/.test(dir.path) ? [] : [['.', true], ['..', true]]);
      for (const k of this.files.keys()) {
        if (!k.startsWith(prefix)) continue;
        const rest = k.slice(prefix.length);
        const i = rest.indexOf('\\');
        names.set(i < 0 ? rest : rest.slice(0, i), i >= 0);
      }
      dir.listing = [...names]
        .filter(([n]) => re.test(n))
        .map(([name, isDir]) => ({ name: this.caseNames.get(prefix + name) ?? name, isDir }));
      dir.listPos = 0;
      if (!dir.listing.length) {
        iosb(this, piosb, STATUS.NO_SUCH_FILE, 0);
        return STATUS.NO_SUCH_FILE;
      }
    }
    if (dir.listPos >= dir.listing.length) {
      iosb(this, piosb, STATUS.NO_MORE_FILES, 0);
      return STATUS.NO_MORE_FILES;
    }
    let at = 0;
    let last = -1;
    while (dir.listPos < dir.listing.length) {
      const { name, isDir } = dir.listing[dir.listPos];
      const size = layout[0] + name.length * 2;
      if (at + size > len) {
        if (last < 0) return STATUS.BUFFER_OVERFLOW;
        break;
      }
      const e = buf + at;
      this.m.u8.fill(0, e, e + layout[0]);
      if (cls === 12) {
        this.w32(e + 8, name.length * 2);
      } else {
        const data = isDir ? null : this.files.get(`${dir.path}\\${name}`);
        writeTimes(this, e + 8);
        this.w64(e + 40, data?.length ?? 0);
        this.w64(e + 48, data ? (data.length + 4095) & ~4095 : 0);
        this.w32(e + 56, isDir ? FILE_ATTRIBUTE_DIRECTORY : FILE_ATTRIBUTE_ARCHIVE);
        this.w32(e + 60, name.length * 2);
        if (layout[1]) this.w64(e + layout[1], dir.listPos + 1);
      }
      for (let i = 0; i < name.length; i++) this.w16(e + layout[0] + i * 2, name.charCodeAt(i));
      if (last >= 0) this.w32(buf + last, at - last);
      last = at;
      at = (at + size + 7) & ~7;
      dir.listPos++;
      if (single) break;
    }
    iosb(this, piosb, 0, last >= 0 ? at : 0);
    return STATUS.SUCCESS;
  },

  // -- sections
  NtCreateSection(a) {
    const [ph, , oa, psize, prot, attrs, file] = [a(0), a(1), a(2), a(3), a(4), a(5), a(6)];
    const f = file ? this.object(file) : null;
    if (file && (!f || f.type !== 'file')) return STATUS.INVALID_HANDLE;
    // A maximum size of zero (or none) means the whole file.
    const size = (psize && Number(this.u64(psize))) || (f?.data?.length ?? 0);
    const sec = { type: 'section', image: !!(attrs & SEC_IMAGE), file: f, size, prot, name: oaName(this, oa) };
    if (sec.image) {
      if (!f?.data || f.data[0] !== 0x4d || f.data[1] !== 0x5a) return STATUS.INVALID_IMAGE_FORMAT;
    }
    this.wptr(ph, this.newHandle(sec));
    return STATUS.SUCCESS;
  },
  NtOpenSection() {
    return STATUS.OBJECT_NAME_NOT_FOUND;
  },
  NtQuerySection(a) {
    const [h, cls, buf, , pret] = [a(0), a(1), a(2), a(3), a(4)];
    const s = this.object(h);
    if (!s || s.type !== 'section') return STATUS.INVALID_HANDLE;
    if (cls === 1 && s.image) {
      const info = parsePe(s.file.data);
      sectionImageInfo(this, info, info.imageBase, buf);
      if (pret) this.wptr(pret, this.L.SECTION_IMAGE_INFORMATION.__size);
      return STATUS.SUCCESS;
    }
    if (cls === 0) {
      // SectionBasicInformation: base, attributes, size
      const ps = this.ps;
      this.wptr(buf, 0);
      this.w32(buf + ps, s.image ? SEC_IMAGE : 0x8000000);
      this.w64(buf + ps * 2, s.size);
      if (pret) this.wptr(pret, ps * 2 + 8);
      return STATUS.SUCCESS;
    }
    return STATUS.INVALID_INFO_CLASS;
  },
  NtMapViewOfSection(a) {
    const [h, , pbase, , , poff, psize, , , prot] = [a(0), a(1), a(2), a(3), a(4), a(5), a(6), a(7), a(8), a(9)];
    const s = this.object(h);
    if (!s || s.type !== 'section') return STATUS.INVALID_HANDLE;
    if (s.image) {
      const r = this.mapImageFile(s.file.path, s.file.data);
      if (r.status) return r.status;
      this.wptr(pbase, r.base);
      this.wptr(psize, r.size);
      return STATUS.SUCCESS;
    }
    // Data section: copy the file (or zero-fill).
    const off = poff ? Number(this.u64(poff)) : 0;
    let size = this.ptr(psize) || s.size - off;
    const want = this.ptr(pbase);
    const base = this.vm.reserve(want, size, { type: MEM_MAPPED, prot });
    if (!base) return want ? STATUS.CONFLICTING_ADDRESSES : STATUS.NO_MEMORY;
    this.vm.commit(base, size, prot || PAGE_READONLY);
    if (s.file?.data) this.m.u8.set(s.file.data.subarray(off, off + size), base);
    this.wptr(pbase, base);
    this.wptr(psize, Math.ceil(size / 0x1000) * 0x1000);
    return STATUS.SUCCESS;
  },
  NtMapViewOfSectionEx(a, cpu, base) {
    // (handle, process, *base, *offset, *size, alloc_type, protect, params, count)
    const map = [0, 1, 2, -1, -1, 3, 4, -1, 5, 6];
    return SYSCALLS.NtMapViewOfSection.call(this, (i) => (map[i] < 0 ? 0 : a(map[i])), cpu, base);
  },
  NtUnmapViewOfSection(a) {
    const r = this.vm.regionAt(a(1));
    if (!r) return STATUS.NOT_MAPPED_VIEW;
    this.vm.release(r.base);
    return STATUS.SUCCESS;
  },
  NtAreMappedFilesTheSame(a) {
    const r1 = this.vm.regionAt(a(0));
    const r2 = this.vm.regionAt(a(1));
    return r1 && r2 && r1.name && r1.name === r2.name ? STATUS.SUCCESS : 0xc00002f6;
  },
};

function sectionImageInfo(h, info, base, buf) {
  const S = h.L.SECTION_IMAGE_INFORMATION;
  h.m.u8.fill(0, buf, buf + S.__size);
  h.wptr(buf + S.TransferAddress, base + info.entryRva);
  h.wptr(buf + S.MaximumStackSize, info.stackReserve);
  h.wptr(buf + S.CommittedStackSize, info.stackCommit);
  h.w32(buf + S.SubSystemType, info.subsystem);
  h.w16(buf + S.MinorSubsystemVersion, info.minorSubsystem);
  h.w16(buf + S.MajorSubsystemVersion, info.majorSubsystem);
  h.w16(buf + S.MajorOperatingSystemVersion, info.majorOs);
  h.w16(buf + S.MinorOperatingSystemVersion, info.minorOs);
  h.w16(buf + S.ImageCharacteristics, info.characteristics);
  h.w16(buf + S.DllCharacteristics, info.dllCharacteristics);
  h.w16(buf + S.Machine, info.machine);
  h.m.u8[buf + S.ImageContainsCode] = 1;
  h.m.u8[buf + S.ImageFlags] = 0;
  h.w32(buf + S.LoaderFlags, info.loaderFlags);
  h.w32(buf + S.ImageFileSize, info.sizeOfImage);
  h.w32(buf + S.CheckSum, info.checksum);
}

/** Maps a data file read-only (NLS tables). Cached per path. */
function mapReadOnlyFile(h, path) {
  h.nlsMaps ??= new Map();
  if (h.nlsMaps.has(path)) return h.nlsMaps.get(path);
  const data = h.fileAt(path);
  if (!data) {
    h.log(`missing file ${path}`);
    return null;
  }
  const size = Math.ceil(data.length / 0x1000) * 0x1000;
  const base = h.vm.reserve(0, size, { type: MEM_MAPPED, prot: PAGE_READONLY, name: path });
  h.vm.commit(base, size, PAGE_READONLY);
  h.m.u8.set(data, base);
  const r = { base, size: data.length };
  h.nlsMaps.set(path, r);
  return r;
}

function openFile(h, ph, oa, piosb, disposition, options = 0) {
  // A socket (./afd.mjs).
  if (/^\\Device\\Afd/i.test(oaName(h, oa))) {
    h.wptr(ph, h.newHandle(afdOpen()));
    iosb(h, piosb, 0, 0);
    return STATUS.SUCCESS;
  }
  const path = oaPath(h, oa);
  if (!path) return STATUS.OBJECT_NAME_INVALID;
  const raw = oaName(h, oa);
  if (/^\\Device\\ConDrv/i.test(raw) || /\\conin\$|\\conout\$/i.test(raw)) {
    h.wptr(ph, h.newHandle({ type: 'file', std: /in/i.test(raw) ? 'stdin' : 'stdout', path: null }));
    iosb(h, piosb, 0, 1);
    return STATUS.SUCCESS;
  }
  let fi = fileInfo(h, path);
  h.log(`open ${path} (disposition ${disposition}) -> ${fi ? 'found' : 'missing'}`);
  // FILE_SUPERSEDE 0, OPEN 1, CREATE 2, OPEN_IF 3, OVERWRITE 4, OVERWRITE_IF 5
  if (!fi) {
    if (disposition === 1 || disposition === 4) {
      const parent = path.replace(/\\[^\\]*$/, '');
      return h.isDir(parent) ? STATUS.OBJECT_NAME_NOT_FOUND : STATUS.OBJECT_PATH_NOT_FOUND;
    }
    if (options & 1) {
      // FILE_DIRECTORY_FILE: a new directory (CreateDirectory).
      h.dirs.add(path);
    } else {
      h.files.set(path, new Uint8Array(0));
    }
    // The name as the program wrote it, for directory listings.
    const given = raw.replace(/^\\\?\?\\/, '');
    if (given.toLowerCase() === path) h.rememberCase(given);
    fi = fileInfo(h, path);
  } else if (disposition === 2) {
    return STATUS.OBJECT_NAME_COLLISION;
  } else if (!fi.dir && (disposition === 0 || disposition === 4 || disposition === 5)) {
    h.files.set(path, new Uint8Array(0));
    fi = fileInfo(h, path);
  }
  const obj = { type: 'file', path, dir: fi.dir, data: fi.dir ? null : h.fileAt(path), pos: 0 };
  if (options & 0x1000) {
    // FILE_DELETE_ON_CLOSE
    h.deletePending ??= new Set();
    h.deletePending.add(path);
  }
  h.wptr(ph, h.newHandle(obj));
  iosb(h, piosb, 0, 1); // FILE_OPENED
  return STATUS.SUCCESS;
}

/**
 * x86-64 versions of the calls ./thread-syscalls.mjs and ./exceptions.mjs
 * implement for i386 (one thread; exceptions stop the program), until those
 * serve both architectures.
 */
export const SYSCALLS64 = {
  NtQueryInformationThread(a) {
    const [, cls, buf, len, pret] = [a(0), a(1), a(2), a(3), a(4)];
    if (cls === 0) {
      // ThreadBasicInformation
      const B = this.L.THREAD_BASIC_INFORMATION;
      if (len < B.__size) return STATUS.INFO_LENGTH_MISMATCH;
      this.m.u8.fill(0, buf, buf + B.__size);
      this.wptr(buf + B.TebBaseAddress, this.teb);
      this.wptr(buf + B.ClientId, 0x20);
      this.wptr(buf + B.ClientId + this.ps, 0x24);
      this.wptr(buf + B.AffinityMask, 1);
      if (pret) this.w32(pret, B.__size);
      return STATUS.SUCCESS;
    }
    if (cls === 9) {
      // ThreadQuerySetWin32StartAddress
      this.wptr(buf, 0);
      if (pret) this.w32(pret, this.ps);
      return STATUS.SUCCESS;
    }
    this.fixme(`NtQueryInformationThread class ${cls} not implemented`);
    return STATUS.INVALID_INFO_CLASS;
  },
  NtRaiseException(a) {
    const rec = a(0);
    const ps = this.ps;
    // EXCEPTION_RECORD: code, flags, record, address, count, information[]
    const code = this.u32(rec);
    const addr = this.ptr(rec + 8 + ps);
    const nparams = this.u32(rec + 8 + ps * 2);
    const params = [];
    for (let i = 0; i < Math.min(nparams, 4); i++) params.push(hex(this.ptr(rec + 8 + ps * 3 + i * ps)));
    throw new GuestFault(code, addr, 0, `exception ${hex(code)} raised at ${hex(addr)} params [${params.join(', ')}] (no SEH dispatch yet)`);
  },
  NtSetContextThread(a, cpu) {
    if (a(0) === 0xfffffffe) return continueContext(this, cpu, a(1));
    return STATUS.NOT_IMPLEMENTED;
  },
  NtTerminateThread(a) {
    throw new ProcessExit(a(1));
  },

  // One thread on x86-64 until the scheduler's system calls
  // (./thread-syscalls.mjs) serve it: a wait on an unsignaled object would
  // never end, so waits report success.
  NtWaitForSingleObject() {
    return STATUS.SUCCESS;
  },
  NtWaitForMultipleObjects() {
    return STATUS.SUCCESS;
  },
  NtWaitForKeyedEvent() {
    return STATUS.SUCCESS;
  },
  NtReleaseKeyedEvent() {
    return STATUS.SUCCESS;
  },
  NtDelayExecution() {
    return STATUS.SUCCESS;
  },
  NtYieldExecution() {
    return STATUS.SUCCESS;
  },
};
