// Whether translated code has to watch its stores for writes to code
// (wwt::abi::cpu::CODE_WRITABLE): only while some page with translated code
// is writable. A Windows program can change its code only after making the
// page writable (VirtualProtect, or memory allocated executable and
// writable); a write to a read-only page faults. So for the usual program,
// whose code is in read-only image sections, stores skip the store map and
// need only the guest-limit check. The runtime does not enforce protections:
// a program that writes to read-only code (and would crash on Windows) is
// not noticed.

import { PAGE_EXECUTE_READWRITE, PAGE_EXECUTE_WRITECOPY, PAGE_READWRITE, PAGE_WRITECOPY } from './vm.mjs';

const WRITABLE = PAGE_READWRITE | PAGE_WRITECOPY | PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY;
const STORE_CODE = 1; // wwt::abi::store_map::CODE

export class CodeWriteWatch {
  /**
   * @param {import('../runtime.mjs').Machine} machine
   * @param {import('./vm.mjs').VirtualMemory} vm
   */
  constructor(machine, vm) {
    this.m = machine;
    this.vm = vm;
    /** Writable pages with translated code. */
    this.pages = new Set();
    machine.onCodePage = (p) => this.check(p, p + 1);
    vm.onProtect = (p0, p1) => this.check(p0, p1);
    this.check(0, machine.guestLimit >>> 12);
  }

  hasCode(p) {
    return (this.m.u8[this.m.storeMap + p] & STORE_CODE) !== 0;
  }

  /** Pages the runtime keeps no protection for count as writable. */
  writable(p) {
    if (p >= this.vm.prot.length) return true;
    return (this.vm.prot[p] & WRITABLE) !== 0;
  }

  check(p0, p1) {
    for (let p = p0; p < p1; p++) {
      if (this.hasCode(p) && this.writable(p)) this.pages.add(p);
      else this.pages.delete(p);
    }
    // Translated code drops a page's CODE bit when a store hits it.
    for (const p of this.pages) if (!this.hasCode(p)) this.pages.delete(p);
    this.m.setCodeWritable(this.pages.size > 0);
  }
}
