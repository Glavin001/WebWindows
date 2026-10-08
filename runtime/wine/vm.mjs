// NT virtual memory for the guest region: reserve/commit/protect/query with
// 4 KB pages and 64 KB allocation granularity, as NtAllocateVirtualMemory and
// friends need. Contents live in the machine's shared memory; this tracks
// state only (WebAssembly has no page protection, so protections are
// recorded and reported but not enforced).

export const MEM_COMMIT = 0x1000;
export const MEM_RESERVE = 0x2000;
export const MEM_DECOMMIT = 0x4000;
export const MEM_RELEASE = 0x8000;
export const MEM_FREE = 0x10000;
export const MEM_PRIVATE = 0x20000;
export const MEM_MAPPED = 0x40000;
export const MEM_RESET = 0x80000;
export const MEM_TOP_DOWN = 0x100000;
export const MEM_IMAGE = 0x1000000;

export const PAGE_NOACCESS = 0x01;
export const PAGE_READONLY = 0x02;
export const PAGE_READWRITE = 0x04;
export const PAGE_WRITECOPY = 0x08;
export const PAGE_EXECUTE = 0x10;
export const PAGE_EXECUTE_READ = 0x20;
export const PAGE_EXECUTE_READWRITE = 0x40;
export const PAGE_EXECUTE_WRITECOPY = 0x80;
export const PAGE_GUARD = 0x100;

const PAGE = 0x1000;
const GRAN = 0x10000;

export class VirtualMemory {
  constructor(machine, lo, hi) {
    this.m = machine;
    this.lo = lo;
    this.hi = hi;
    const pages = Math.ceil(hi / PAGE);
    // Per page: protection (0 = not committed) and the region it belongs to.
    this.prot = new Uint16Array(pages);
    this.region = new Int32Array(pages).fill(-1);
    this.regions = []; // id -> {base, size, type, allocProt, name}
    /** Called with a page range [p0, p1) whose protection changed. */
    this.onProtect = null;
  }

  pageOf(a) {
    return Math.floor(a / PAGE);
  }

  isFree(base, size) {
    for (let p = this.pageOf(base); p < this.pageOf(base + size - 1) + 1; p++) {
      if (p >= this.region.length || this.region[p] !== -1) return false;
    }
    return true;
  }

  findFree(size, { topDown = false, align = GRAN, lo = this.lo, hi = this.hi } = {}) {
    size = Math.ceil(size / PAGE) * PAGE;
    if (topDown) {
      for (let a = Math.floor((hi - size) / align) * align; a >= lo; a -= align) {
        if (this.isFree(a, size)) return a;
      }
    } else {
      for (let a = Math.ceil(lo / align) * align; a + size <= hi; a += align) {
        if (this.isFree(a, size)) return a;
        // Skip quickly past the region blocking this address.
        const r = this.region[this.pageOf(a)];
        if (r >= 0) {
          const reg = this.regions[r];
          a = Math.ceil((reg.base + reg.size) / align) * align - align;
        }
      }
    }
    return 0;
  }

  /** Reserves a region (at `base` when non-zero). Returns its base or 0. */
  reserve(base, size, { type = MEM_PRIVATE, prot = PAGE_READWRITE, topDown = false, name = '' } = {}) {
    size = Math.ceil(size / PAGE) * PAGE;
    if (base) {
      base = Math.floor(base / PAGE) * PAGE;
      if (base < this.lo || base + size > this.hi || !this.isFree(base, size)) return 0;
    } else {
      base = this.findFree(size, { topDown });
      if (!base) return 0;
    }
    const id = this.regions.length;
    this.regions.push({ base, size, type, allocProt: prot, name });
    for (let p = this.pageOf(base); p < this.pageOf(base + size); p++) this.region[p] = id;
    return base;
  }

  /** Commits pages in [addr, addr+size), zeroing pages not yet committed. */
  commit(addr, size, prot) {
    const p0 = this.pageOf(addr);
    const p1 = this.pageOf(addr + size - 1) + 1;
    for (let p = p0; p < p1; p++) {
      if (this.region[p] < 0) return false;
    }
    // Zero each run of pages not yet committed with one fill.
    for (let p = p0; p < p1; ) {
      if (this.prot[p]) {
        this.prot[p++] = prot;
        continue;
      }
      let q = p;
      while (q < p1 && !this.prot[q]) this.prot[q++] = prot;
      this.m.u8.fill(0, p * PAGE, q * PAGE);
      p = q;
    }
    this.onProtect?.(p0, p1);
    return true;
  }

  decommit(addr, size) {
    const p0 = this.pageOf(addr);
    const p1 = this.pageOf(addr + size - 1) + 1;
    for (let p = p0; p < p1; p++) this.prot[p] = 0;
    this.onProtect?.(p0, p1);
  }

  release(base) {
    const id = this.region[this.pageOf(base)];
    if (id < 0) return false;
    const r = this.regions[id];
    if (r.base !== base) return false;
    for (let p = this.pageOf(r.base); p < this.pageOf(r.base + r.size); p++) {
      this.region[p] = -1;
      this.prot[p] = 0;
    }
    r.released = true;
    this.onProtect?.(this.pageOf(r.base), this.pageOf(r.base + r.size));
    return true;
  }

  regionAt(addr) {
    const id = this.region[this.pageOf(addr)];
    return id >= 0 ? this.regions[id] : null;
  }

  /** Changes protection; returns the old protection of the first page, or -1. */
  protect(addr, size, prot) {
    const p0 = this.pageOf(addr);
    const p1 = this.pageOf(addr + size - 1) + 1;
    for (let p = p0; p < p1; p++) if (!this.prot[p]) return -1;
    const old = this.prot[p0];
    for (let p = p0; p < p1; p++) this.prot[p] = prot;
    this.onProtect?.(p0, p1);
    return old;
  }

  /** MEMORY_BASIC_INFORMATION for the run of pages starting at `addr`. */
  query(addr) {
    const p0 = this.pageOf(addr);
    const base = p0 * PAGE;
    if (p0 >= this.region.length) return null;
    const id = this.region[p0];
    const prot = this.prot[p0];
    let p = p0;
    while (p < this.region.length && this.region[p] === id && this.prot[p] === prot) p++;
    if (id < 0) {
      return { base, allocBase: 0, allocProt: 0, size: (p - p0) * PAGE, state: MEM_FREE, prot: PAGE_NOACCESS, type: 0 };
    }
    const r = this.regions[id];
    return {
      base,
      allocBase: r.base,
      allocProt: r.allocProt,
      size: (p - p0) * PAGE,
      state: prot ? MEM_COMMIT : MEM_RESERVE,
      prot: prot,
      type: r.type,
    };
  }
}
