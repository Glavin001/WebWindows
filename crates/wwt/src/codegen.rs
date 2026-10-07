//! Layer 6 — Generate WebAssembly.
//!
//! One WebAssembly function per x86 function, with type `(cpu: i32) -> i32`:
//! the result is the x86 address to continue at. With 64-bit memory
//! ([`CodegenConfig::mem64`]) the CPU pointer, native addresses and the
//! runtime-table globals are i64, and x86-64 code addresses are i64 too
//! (`(cpu) -> i64`, see [`ModuleGen::code64`]). Branches and loops are
//! rebuilt as structured control flow from the dominator tree (Ramsey,
//! "Beyond Relooper", 2022); irreducible graphs fall back to a dispatch loop.
//! Calls between functions in the module are direct; everything else goes
//! through the two-level address lookup into the shared function table.

use std::borrow::Cow;
use std::collections::HashMap;

use wasm_encoder::{
    BlockType, CodeSection, ConstExpr, CustomSection, ElementSection, Elements, EntityType,
    FunctionSection, GlobalType, ImportSection, Instruction as W, MemArg, MemoryType, Module,
    RefType, TableType, TypeSection, ValType,
};

use crate::abi::{self, addr::NULL_LIMIT, cpu, cpu64, fault, imports};
use crate::flags::{self, E};
use crate::ir::*;
use crate::opt::{self, Analysis, StateMask, ALL_STATE, FAULT_SYNC};

#[derive(Debug, Clone)]
pub struct CodegenConfig {
    pub mem_checks: bool,
    pub smc_checks: bool,
    /// Import a 64-bit (memory64) memory. Guest addresses narrower than the
    /// memory are zero-extended; with a 32-bit memory, x86-64 addresses are
    /// checked against the guest limit and wrapped.
    pub mem64: bool,
}

impl Default for CodegenConfig {
    fn default() -> Self {
        CodegenConfig {
            mem_checks: true,
            smc_checks: true,
            mem64: false,
        }
    }
}

/// Largest memory64 size browsers allow (16 GB) in 64 KB pages.
pub const MAX_PAGES_64: u64 = 1 << 18;

// Type indices.
const T_FN: u32 = 0;
const T_FAULT: u32 = 1;
const T_CODE_WRITE: u32 = 2;
const T_MATH: u32 = 3;
const T_FIRST_HELPER: u32 = 4;

// Imported function indices.
const F_FAULT: u32 = 0;
const F_CODE_WRITE: u32 = 1;
const F_MATH: u32 = 2;
const NUM_FUNC_IMPORTS: u32 = 3;

// Imported global indices.
const G_TABLE_BASE: u32 = 0;
const G_LOOKUP_L1: u32 = 1;
const G_GUEST_CHECK: u32 = 2;
const G_CODE_BITMAP: u32 = 3;
/// 64-bit code only.
const G_CODE_PAGES: u32 = 4;

fn val_type(t: Ty) -> ValType {
    match t {
        Ty::I32 => ValType::I32,
        Ty::I64 => ValType::I64,
        Ty::F32 => ValType::F32,
        Ty::F64 => ValType::F64,
        Ty::V128 => ValType::V128,
    }
}

fn memarg(offset: u32, align_log2: u32) -> MemArg {
    MemArg {
        offset: offset as u64,
        align: align_log2,
        memory_index: 0,
    }
}

/// Builds one module from a set of lifted, optimized functions.
pub struct ModuleGen<'a> {
    pub cfg: &'a CodegenConfig,
    /// x86 entry address -> index among translated functions.
    func_index: HashMap<u64, u32>,
    /// Code addresses are 64-bit (x86-64 code on a 64-bit memory): functions
    /// return an i64, eip lives in [`cpu64::RIP`] and the lookup covers
    /// [`imports::CODE_PAGES`] pages. Otherwise code addresses are 32-bit
    /// (x86-64 images then sit below 4 GB).
    pub code64: bool,
    helpers: Vec<Helper>,
    helper_types: Vec<(Vec<Ty>, Ty)>,
}

impl<'a> ModuleGen<'a> {
    pub fn new(cfg: &'a CodegenConfig, funcs: &[Function]) -> ModuleGen<'a> {
        let mut func_index = HashMap::new();
        for (i, f) in funcs.iter().enumerate() {
            func_index.insert(f.entry, i as u32);
        }
        // Eflags must stay first: EvalCond calls it by index.
        let mut helpers = vec![Helper::Eflags, Helper::EvalCond];
        for f in funcs {
            for b in &f.blocks {
                for inst in &b.insts {
                    if let Op::CallHelper(h, _) = &inst.op {
                        if *h == Helper::EvalCond64 && !helpers.contains(&Helper::Eflags64) {
                            helpers.push(Helper::Eflags64);
                        }
                        if !helpers.contains(h) {
                            helpers.push(*h);
                        }
                    }
                }
            }
        }
        let code64 = cfg.mem64 && funcs.first().is_some_and(|f| f.mode == Mode::X64);
        ModuleGen {
            cfg,
            func_index,
            code64,
            helpers,
            helper_types: vec![],
        }
    }

    /// Disables direct calls between the module's functions (for modules
    /// whose functions share entry addresses, like instruction tests).
    pub fn without_direct_calls(mut self) -> Self {
        self.func_index.clear();
        self
    }

    fn helper_func_index(&self, h: Helper) -> u32 {
        NUM_FUNC_IMPORTS + self.helpers.iter().position(|x| *x == h).unwrap() as u32
    }

    fn translated_func_index(&self, i: u32) -> u32 {
        NUM_FUNC_IMPORTS + self.helpers.len() as u32 + i
    }

    pub fn direct_target(&self, addr: u64) -> Option<u32> {
        self.func_index
            .get(&addr)
            .map(|&i| self.translated_func_index(i))
    }

    /// Type of the CPU pointer and of addresses into the memory.
    fn addr_type(&self) -> ValType {
        if self.cfg.mem64 {
            ValType::I64
        } else {
            ValType::I32
        }
    }

    /// Type of x86 code addresses.
    fn code_type(&self) -> ValType {
        if self.code64 {
            ValType::I64
        } else {
            ValType::I32
        }
    }

    /// Generates the module bytes.
    pub fn build(mut self, funcs: &[Function], meta_json: &str) -> Vec<u8> {
        let mut module = Module::new();
        let at = self.addr_type();
        let ct = self.code_type();
        for f in funcs {
            assert_eq!(
                f.mem64, self.cfg.mem64,
                "function and module memory width differ"
            );
            assert_eq!(
                f.mode == Mode::X64 && f.mem64,
                self.code64,
                "function and module code width differ"
            );
        }

        // Types.
        let mut types = TypeSection::new();
        types.ty().function([at], [ct]);
        types.ty().function([at, ValType::I32, ct, ct], []);
        types.ty().function([at, at], []);
        types
            .ty()
            .function([ValType::I32, ValType::F64, ValType::F64], [ValType::F64]);
        let mut helper_type_idx = vec![];
        for h in &self.helpers {
            let (params, ret) = h.signature();
            let key = (params.to_vec(), ret);
            let idx = match self.helper_types.iter().position(|k| *k == key) {
                Some(i) => i,
                None => {
                    self.helper_types.push(key.clone());
                    types
                        .ty()
                        .function(params.iter().map(|&t| val_type(t)), [val_type(ret)]);
                    self.helper_types.len() - 1
                }
            };
            helper_type_idx.push(T_FIRST_HELPER + idx as u32);
        }
        module.section(&types);

        // Imports.
        let mut imp = ImportSection::new();
        imp.import(
            imports::MODULE,
            imports::FAULT,
            EntityType::Function(T_FAULT),
        );
        imp.import(
            imports::MODULE,
            imports::CODE_WRITE,
            EntityType::Function(T_CODE_WRITE),
        );
        imp.import(imports::MODULE, imports::MATH, EntityType::Function(T_MATH));
        imp.import(
            imports::MODULE,
            imports::MEMORY,
            MemoryType {
                minimum: 1,
                maximum: Some(if self.cfg.mem64 { MAX_PAGES_64 } else { 65536 }),
                memory64: self.cfg.mem64,
                shared: true,
                page_size_log2: None,
            },
        );
        imp.import(
            imports::MODULE,
            imports::TABLE,
            TableType {
                element_type: RefType::FUNCREF,
                minimum: 1,
                maximum: None,
                table64: false,
                shared: false,
            },
        );
        let mut globals = vec![
            imports::TABLE_BASE,
            imports::LOOKUP_L1,
            imports::GUEST_LIMIT,
            imports::CODE_BITMAP,
        ];
        if self.code64 {
            globals.push(imports::CODE_PAGES);
        }
        for name in globals {
            imp.import(
                imports::MODULE,
                name,
                GlobalType {
                    val_type: if name == imports::TABLE_BASE {
                        ValType::I32
                    } else {
                        at
                    },
                    mutable: false,
                    shared: false,
                },
            );
        }
        module.section(&imp);

        // Functions.
        let mut fsec = FunctionSection::new();
        for &t in &helper_type_idx {
            fsec.function(t);
        }
        for _ in funcs {
            fsec.function(T_FN);
        }
        module.section(&fsec);

        // Element segment placing translated functions at table_base.
        let mut elems = ElementSection::new();
        let idxs: Vec<u32> = (0..funcs.len() as u32)
            .map(|i| self.translated_func_index(i))
            .collect();
        if !idxs.is_empty() {
            elems.active(
                Some(0),
                &ConstExpr::global_get(G_TABLE_BASE),
                Elements::Functions(Cow::Borrowed(&idxs)),
            );
        }
        module.section(&elems);

        // Code.
        let mut code = CodeSection::new();
        for h in self.helpers.clone() {
            let eflags_index = match h {
                Helper::EvalCond64 => self.helper_func_index(Helper::Eflags64),
                _ => self.helper_func_index(Helper::Eflags),
            };
            code.function(&gen_helper(h, eflags_index));
        }
        for f in funcs {
            code.function(&self.gen_function(f));
        }
        module.section(&code);

        // Function address map and metadata.
        let mut map = Vec::with_capacity(4 + funcs.len() * 8);
        map.extend_from_slice(&(funcs.len() as u32).to_le_bytes());
        for f in funcs {
            if self.code64 {
                map.extend_from_slice(&f.entry.to_le_bytes());
            } else {
                map.extend_from_slice(&(f.entry as u32).to_le_bytes());
            }
        }
        module.section(&CustomSection {
            name: Cow::Borrowed(if self.code64 {
                abi::FUNCS64_SECTION
            } else {
                abi::FUNCS_SECTION
            }),
            data: Cow::Owned(map),
        });
        module.section(&CustomSection {
            name: Cow::Borrowed(abi::META_SECTION),
            data: Cow::Borrowed(meta_json.as_bytes()),
        });
        module.finish()
    }

    pub fn gen_function(&self, f: &Function) -> wasm_encoder::Function {
        let a = opt::analyze(f);
        let mut g = FnGen::new(self, f, &a);
        g.run();
        g.finish()
    }
}

// ---- Function generation ------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ctx {
    If,
    Loop(BlockId),
    Block(BlockId),
    /// The dispatch loop of the irreducible fallback.
    Dispatch,
}

struct FnGen<'g, 'a> {
    m: &'g ModuleGen<'a>,
    f: &'g Function,
    a: &'g Analysis,
    out: Vec<W<'static>>,
    /// vreg -> local index.
    local_of: Vec<u32>,
    local_types: Vec<ValType>,
    ctx: Vec<Ctx>,
    rpo_num: Vec<u32>,
    idom: Vec<u32>,
    children: Vec<Vec<BlockId>>,
    merge: Vec<bool>,
    loop_header: Vec<bool>,
    /// Scratch locals.
    tmp_i32: u32,
    tmp_i32b: u32,
    tmp_i64: u32,
    tmp_i64b: u32,
    /// Scratch address in the memory's address type.
    tmp_ma: u32,
    label_local: u32,
    emitted: Vec<bool>,
}

const UNASSIGNED: u32 = u32::MAX;

impl<'g, 'a> FnGen<'g, 'a> {
    fn new(m: &'g ModuleGen<'a>, f: &'g Function, a: &'g Analysis) -> Self {
        let n = f.blocks.len();
        let mut g = FnGen {
            m,
            f,
            a,
            out: vec![],
            local_of: vec![UNASSIGNED; f.vtypes.len()],
            local_types: vec![],
            ctx: vec![],
            rpo_num: vec![u32::MAX; n],
            idom: vec![u32::MAX; n],
            children: vec![vec![]; n],
            merge: vec![false; n],
            loop_header: vec![false; n],
            tmp_i32: 0,
            tmp_i32b: 0,
            tmp_i64: 0,
            tmp_i64b: 0,
            tmp_ma: 0,
            label_local: 0,
            emitted: vec![false; n],
        };
        g.local_of[CPU as usize] = 0;
        g.assign_locals();
        g.tmp_i32 = g.new_local(ValType::I32);
        g.tmp_i32b = g.new_local(ValType::I32);
        g.tmp_i64 = g.new_local(ValType::I64);
        // Scratch for 64-bit addresses, only in code that has them (keeps
        // 32-bit output unchanged).
        let wide = f.mode == Mode::X64 || m.cfg.mem64;
        g.tmp_i64b = if wide {
            g.new_local(ValType::I64)
        } else {
            UNASSIGNED
        };
        g.label_local = g.new_local(ValType::I32);
        g.tmp_ma = if wide {
            g.new_local(m.addr_type())
        } else {
            UNASSIGNED
        };
        g
    }

    fn new_local(&mut self, t: ValType) -> u32 {
        self.local_types.push(t);
        self.local_types.len() as u32 // param 0 is cpu
    }

    /// Gives every vreg a local. Temporaries that live within one block share
    /// locals (per type) once their last use has passed.
    fn assign_locals(&mut self) {
        let f = self.f;
        // Global vregs (state and cross-block temps) get dedicated locals.
        let mut block_of: Vec<u32> = vec![u32::MAX; f.vtypes.len()];
        let mut multi: Vec<bool> = vec![false; f.vtypes.len()];
        for (bi, b) in f.blocks.iter().enumerate() {
            let mut touch = |v: V| {
                let bo = &mut block_of[v as usize];
                if *bo == u32::MAX {
                    *bo = bi as u32;
                } else if *bo != bi as u32 {
                    multi[v as usize] = true;
                }
            };
            for inst in &b.insts {
                if let Some(d) = inst.dst {
                    touch(d);
                }
                for u in inst.op.uses() {
                    touch(u);
                }
            }
            for u in b.term.uses() {
                touch(u);
            }
        }
        for v in 0..f.vtypes.len() {
            if v as V == CPU || block_of[v] == u32::MAX {
                continue;
            }
            let global = (v as V) < NUM_STATE || multi[v] || self.a.dense[v] != u32::MAX;
            if global {
                let l = self.new_local(val_type(f.vtypes[v]));
                self.local_of[v] = l;
            }
        }
        // Block-local temporaries.
        let mut pools: HashMap<ValType, Vec<u32>> = HashMap::new();
        for b in &f.blocks {
            let mut last_use: HashMap<V, usize> = HashMap::new();
            for (k, inst) in b.insts.iter().enumerate() {
                for u in inst.op.uses() {
                    last_use.insert(u, k);
                }
            }
            let term_k = b.insts.len();
            for u in b.term.uses() {
                last_use.insert(u, term_k);
            }
            let mut frees: Vec<Vec<(V, ValType)>> = vec![vec![]; b.insts.len() + 1];
            for (k, inst) in b.insts.iter().enumerate() {
                // Release locals whose last use was this instruction before
                // assigning its destination (operands are read first).
                for (v, t) in std::mem::take(&mut frees[k]) {
                    pools.entry(t).or_default().push(self.local_of[v as usize]);
                }
                if let Some(d) = inst.dst {
                    if self.local_of[d as usize] == UNASSIGNED {
                        let t = val_type(f.vtypes[d as usize]);
                        let l = match pools.entry(t).or_default().pop() {
                            Some(l) => l,
                            None => self.new_local(t),
                        };
                        self.local_of[d as usize] = l;
                        let lu = last_use.get(&d).copied().unwrap_or(k);
                        if lu > k {
                            frees[lu].push((d, t));
                        } else {
                            pools.entry(t).or_default().push(l);
                        }
                    }
                }
            }
            for (v, t) in std::mem::take(&mut frees[term_k]) {
                pools.entry(t).or_default().push(self.local_of[v as usize]);
            }
        }
    }

    fn finish(self) -> wasm_encoder::Function {
        let mut func =
            wasm_encoder::Function::new_with_locals_types(self.local_types.iter().copied());
        for i in &self.out {
            func.instruction(i);
        }
        func.instruction(&W::End);
        func
    }

    fn emit(&mut self, i: W<'static>) {
        self.out.push(i);
    }

    fn get(&mut self, v: V) {
        let l = self.local_of[v as usize];
        debug_assert!(l != UNASSIGNED, "vreg {v} has no local");
        self.emit(W::LocalGet(l));
    }

    fn set(&mut self, v: V) {
        let l = self.local_of[v as usize];
        self.emit(W::LocalSet(l));
    }

    // ---- State write-back ----------------------------------------------------

    fn sync(&mut self, mask: StateMask) {
        for v in 0..NUM_STATE {
            if mask >> v & 1 == 0 || v == CPU {
                continue;
            }
            if self.local_of[v as usize] == UNASSIGNED {
                continue;
            }
            let (off, ty) = self.f.state_home(v).unwrap();
            self.emit(W::LocalGet(0));
            self.get(v);
            match (ty, state_home_size(v, self.f.mode)) {
                (Ty::V128, _) => self.emit(W::V128Store(memarg(off, 4))),
                (Ty::I64, _) => self.emit(W::I64Store(memarg(off, 3))),
                (_, 2) => self.emit(W::I32Store16(memarg(off, 1))),
                _ => self.emit(W::I32Store(memarg(off, 2))),
            }
        }
    }

    fn reload(&mut self, mask: StateMask) {
        for v in 0..NUM_STATE {
            if mask >> v & 1 == 0 || v == CPU {
                continue;
            }
            if self.local_of[v as usize] == UNASSIGNED {
                continue;
            }
            let (off, ty) = self.f.state_home(v).unwrap();
            self.emit(W::LocalGet(0));
            match (ty, state_home_size(v, self.f.mode)) {
                (Ty::V128, _) => self.emit(W::V128Load(memarg(off, 4))),
                (Ty::I64, _) => self.emit(W::I64Load(memarg(off, 3))),
                (_, 2) => self.emit(W::I32Load16U(memarg(off, 1))),
                _ => self.emit(W::I32Load(memarg(off, 2))),
            }
            self.set(v);
        }
    }

    fn code64(&self) -> bool {
        self.m.code64
    }

    /// A scratch local of the code address type.
    fn tmp_code(&self) -> u32 {
        if self.code64() {
            self.tmp_i64
        } else {
            self.tmp_i32
        }
    }

    /// Pushes the code address `a`.
    fn code_const(&mut self, a: u64) {
        if self.code64() {
            self.emit(W::I64Const(a as i64));
        } else {
            self.emit(W::I32Const(a as i32));
        }
    }

    /// Pushes vreg `v` as a code address. With 32-bit code addresses a
    /// 64-bit value above 4 GB becomes 0, which faults in the dispatcher.
    fn get_code(&mut self, v: V) {
        self.get(v);
        match (self.f.ty(v), self.code64()) {
            (Ty::I32, true) => self.emit(W::I64ExtendI32U),
            (Ty::I64, false) => {
                self.emit(W::LocalTee(self.tmp_i64));
                self.emit(W::I32WrapI64);
                self.emit(W::I32Const(0));
                self.emit(W::LocalGet(self.tmp_i64));
                self.emit(W::I64Const(32));
                self.emit(W::I64ShrU);
                self.emit(W::I64Eqz);
                self.emit(W::Select);
            }
            _ => {}
        }
    }

    /// Stores the code address on the stack (after the CPU pointer) as eip.
    fn store_eip(&mut self) {
        if self.code64() {
            self.emit(W::I64Store(memarg(cpu64::RIP, 3)));
        } else {
            self.emit(W::I32Store(memarg(cpu::EIP, 2)));
        }
    }

    fn store_eip_const(&mut self, eip: u64) {
        self.emit(W::LocalGet(0));
        self.code_const(eip);
        self.store_eip();
    }

    /// Sets the fault information from the value of type `t` on the stack.
    fn set_fault_info(&mut self, t: Ty) {
        if self.code64() {
            if t == Ty::I32 {
                self.emit(W::I64ExtendI32U);
            }
            self.emit(W::LocalSet(self.tmp_i64b));
        } else {
            if t == Ty::I64 {
                self.emit(W::I32WrapI64);
            }
            self.emit(W::LocalSet(self.tmp_i32b));
        }
    }

    /// Raises a fault: writes back `mask`, records eip and calls the host.
    /// The address/info value must already be in `tmp_i32b` (`tmp_i64b`
    /// for 64-bit code).
    fn raise(&mut self, mask: StateMask, code: u32, eip: u64) {
        self.sync(mask);
        self.store_eip_const(eip);
        self.emit(W::LocalGet(0));
        self.emit(W::I32Const(code as i32));
        self.code_const(eip);
        if self.code64() {
            self.emit(W::LocalGet(self.tmp_i64b));
        } else {
            self.emit(W::LocalGet(self.tmp_i32b));
        }
        self.emit(W::Call(F_FAULT));
        self.emit(W::Unreachable);
    }

    // ---- Lookup --------------------------------------------------------------

    /// Pushes the table index for the address in local `t`.
    fn lookup_index(&mut self, t: u32) {
        if self.code64() {
            // page = min(t >> 12, code_pages); L1 entries are 8-byte
            // pointers to the L2 pages.
            self.emit(W::GlobalGet(G_LOOKUP_L1));
            self.emit(W::LocalGet(t));
            self.emit(W::I64Const(12));
            self.emit(W::I64ShrU);
            self.emit(W::LocalTee(self.tmp_i64b));
            self.emit(W::GlobalGet(G_CODE_PAGES));
            self.emit(W::LocalGet(self.tmp_i64b));
            self.emit(W::GlobalGet(G_CODE_PAGES));
            self.emit(W::I64LtU);
            self.emit(W::Select);
            self.emit(W::I64Const(3));
            self.emit(W::I64Shl);
            self.emit(W::I64Add);
            self.emit(W::I64Load(memarg(0, 3)));
            self.emit(W::LocalGet(t));
            self.emit(W::I64Const(0xfff));
            self.emit(W::I64And);
            self.emit(W::I64Const(2));
            self.emit(W::I64Shl);
            self.emit(W::I64Add);
            self.emit(W::I32Load(memarg(0, 2)));
            return;
        }
        if self.m.cfg.mem64 {
            // L1 entries are 8-byte pointers to the L2 pages.
            self.emit(W::GlobalGet(G_LOOKUP_L1));
            self.emit(W::LocalGet(t));
            self.emit(W::I32Const(12));
            self.emit(W::I32ShrU);
            self.emit(W::I64ExtendI32U);
            self.emit(W::I64Const(3));
            self.emit(W::I64Shl);
            self.emit(W::I64Add);
            self.emit(W::I64Load(memarg(0, 3)));
            self.emit(W::LocalGet(t));
            self.emit(W::I32Const(0xfff));
            self.emit(W::I32And);
            self.emit(W::I32Const(2));
            self.emit(W::I32Shl);
            self.emit(W::I64ExtendI32U);
            self.emit(W::I64Add);
            self.emit(W::I32Load(memarg(0, 2)));
            return;
        }
        self.emit(W::GlobalGet(G_LOOKUP_L1));
        self.emit(W::LocalGet(t));
        self.emit(W::I32Const(12));
        self.emit(W::I32ShrU);
        self.emit(W::I32Const(2));
        self.emit(W::I32Shl);
        self.emit(W::I32Add);
        self.emit(W::I32Load(memarg(0, 2)));
        self.emit(W::LocalGet(t));
        self.emit(W::I32Const(0xfff));
        self.emit(W::I32And);
        self.emit(W::I32Const(2));
        self.emit(W::I32Shl);
        self.emit(W::I32Add);
        self.emit(W::I32Load(memarg(0, 2)));
    }

    /// Calls (or tail-calls) the x86 address in local `t` through the table.
    fn call_lookup(&mut self, t: u32, tail: bool) {
        self.emit(W::LocalGet(0));
        self.emit(W::LocalGet(t));
        self.store_eip();
        self.emit(W::LocalGet(0));
        self.lookup_index(t);
        if tail {
            self.emit(W::ReturnCallIndirect {
                type_index: T_FN,
                table_index: 0,
            });
        } else {
            self.emit(W::CallIndirect {
                type_index: T_FN,
                table_index: 0,
            });
        }
    }

    // ---- Structure -----------------------------------------------------------

    fn run(&mut self) {
        let f = self.f;
        let order = f.rpo();
        for (i, &b) in order.iter().enumerate() {
            self.rpo_num[b as usize] = i as u32;
        }
        let preds = f.predecessors();
        self.compute_dominators(&order, &preds);
        // Loop headers, merge nodes and reducibility.
        let mut reducible = true;
        for &b in &order {
            for &p in &preds[b as usize] {
                if self.rpo_num[p as usize] == u32::MAX {
                    continue;
                }
                if self.rpo_num[p as usize] >= self.rpo_num[b as usize] {
                    self.loop_header[b as usize] = true;
                    if !self.dominates(b, p) {
                        reducible = false;
                    }
                }
            }
            let fwd = preds[b as usize]
                .iter()
                .filter(|&&p| {
                    self.rpo_num[p as usize] != u32::MAX
                        && self.rpo_num[p as usize] < self.rpo_num[b as usize]
                })
                .count();
            if fwd >= 2 {
                self.merge[b as usize] = true;
            }
            if let Term::Switch { targets, .. } = &f.blocks[b as usize].term {
                for &t in targets {
                    self.merge[t as usize] = true;
                }
            }
        }
        for &b in &order {
            let d = self.idom[b as usize];
            if b != 0 && d != u32::MAX {
                self.children[d as usize].push(b);
            }
        }
        // Entry: load live state.
        let live = self.a.state_live_in(0) & ALL_STATE;
        self.reload(live);
        if reducible {
            self.do_tree(0);
        } else {
            self.dispatch_loop(&order);
        }
        self.emit(W::Unreachable);
    }

    fn compute_dominators(&mut self, order: &[BlockId], preds: &[Vec<BlockId>]) {
        self.idom[0] = 0;
        let mut changed = true;
        while changed {
            changed = false;
            for &b in order.iter().skip(1) {
                let mut new: u32 = u32::MAX;
                for &p in &preds[b as usize] {
                    if self.idom[p as usize] == u32::MAX {
                        continue;
                    }
                    new = if new == u32::MAX {
                        p
                    } else {
                        self.intersect(p, new)
                    };
                }
                if new != self.idom[b as usize] {
                    self.idom[b as usize] = new;
                    changed = true;
                }
            }
        }
    }

    fn intersect(&self, mut a: u32, mut b: u32) -> u32 {
        while a != b {
            while self.rpo_num[a as usize] > self.rpo_num[b as usize] {
                a = self.idom[a as usize];
            }
            while self.rpo_num[b as usize] > self.rpo_num[a as usize] {
                b = self.idom[b as usize];
            }
        }
        a
    }

    fn dominates(&self, a: BlockId, mut b: BlockId) -> bool {
        loop {
            if a == b {
                return true;
            }
            if b == 0 {
                return false;
            }
            let d = self.idom[b as usize];
            if d == u32::MAX || d == b {
                return false;
            }
            b = d;
        }
    }

    fn do_tree(&mut self, x: BlockId) {
        assert!(!self.emitted[x as usize], "block emitted twice");
        self.emitted[x as usize] = true;
        let mut merges: Vec<BlockId> = self.children[x as usize]
            .iter()
            .copied()
            .filter(|&c| self.merge[c as usize])
            .collect();
        merges.sort_by_key(|&c| std::cmp::Reverse(self.rpo_num[c as usize]));
        if self.loop_header[x as usize] {
            self.emit(W::Loop(BlockType::Empty));
            self.ctx.push(Ctx::Loop(x));
            self.node_within(x, &merges);
            self.ctx.pop();
            self.emit(W::End);
        } else {
            self.node_within(x, &merges);
        }
    }

    fn node_within(&mut self, x: BlockId, ys: &[BlockId]) {
        match ys.split_first() {
            Some((&y, rest)) => {
                self.emit(W::Block(BlockType::Empty));
                self.ctx.push(Ctx::Block(y));
                self.node_within(x, rest);
                self.ctx.pop();
                self.emit(W::End);
                self.do_tree(y);
            }
            None => {
                self.block_body(x);
                self.terminator(x);
            }
        }
    }

    fn label(&self, c: Ctx) -> u32 {
        let pos = self
            .ctx
            .iter()
            .rposition(|&x| x == c)
            .unwrap_or_else(|| panic!("label {c:?} not in context"));
        (self.ctx.len() - 1 - pos) as u32
    }

    fn do_branch(&mut self, src: BlockId, tgt: BlockId) {
        if self.ctx.contains(&Ctx::Dispatch) {
            self.emit(W::I32Const(tgt as i32));
            self.emit(W::LocalSet(self.label_local));
            let l = self.label(Ctx::Dispatch);
            self.emit(W::Br(l));
            return;
        }
        if self.rpo_num[tgt as usize] <= self.rpo_num[src as usize] {
            let l = self.label(Ctx::Loop(tgt));
            self.emit(W::Br(l));
        } else if self.merge[tgt as usize] {
            let l = self.label(Ctx::Block(tgt));
            self.emit(W::Br(l));
        } else {
            self.do_tree(tgt);
        }
    }

    /// A non-merge successor whose code always leaves the function.
    fn is_dead_end(&self, src: BlockId, b: BlockId) -> bool {
        !self.ctx.contains(&Ctx::Dispatch)
            && !self.merge[b as usize]
            && !self.loop_header[b as usize]
            && self.rpo_num[b as usize] > self.rpo_num[src as usize]
            && self.f.blocks[b as usize].term.successors().is_empty()
    }

    fn terminator(&mut self, x: BlockId) {
        let blk = &self.f.blocks[x as usize];
        let dirty = self.a.dirty_end[x as usize];
        match blk.term.clone() {
            Term::Jump(t) => self.do_branch(x, t),
            Term::Branch { t, f, .. } if t == f => self.do_branch(x, t),
            Term::Branch { cond, t, f } => {
                if self.is_dead_end(x, t) && !self.ctx.contains(&Ctx::Dispatch) {
                    self.get(cond);
                    self.emit(W::If(BlockType::Empty));
                    self.ctx.push(Ctx::If);
                    self.do_tree(t);
                    self.ctx.pop();
                    self.emit(W::End);
                    self.do_branch(x, f);
                } else if self.is_dead_end(x, f) && !self.ctx.contains(&Ctx::Dispatch) {
                    self.get(cond);
                    self.emit(W::I32Eqz);
                    self.emit(W::If(BlockType::Empty));
                    self.ctx.push(Ctx::If);
                    self.do_tree(f);
                    self.ctx.pop();
                    self.emit(W::End);
                    self.do_branch(x, t);
                } else {
                    self.get(cond);
                    self.emit(W::If(BlockType::Empty));
                    self.ctx.push(Ctx::If);
                    self.do_branch(x, t);
                    self.emit(W::Else);
                    self.do_branch(x, f);
                    self.ctx.pop();
                    self.emit(W::End);
                }
            }
            Term::Switch {
                index,
                targets,
                fallback,
            } => {
                self.get(index);
                self.emit(W::I32Const(targets.len() as i32));
                self.emit(W::I32LtU);
                self.emit(W::If(BlockType::Empty));
                self.ctx.push(Ctx::If);
                if self.ctx.contains(&Ctx::Dispatch) {
                    // Irreducible fallback: set the label from a table.
                    for (k, &t) in targets.iter().enumerate() {
                        self.get(index);
                        self.emit(W::I32Const(k as i32));
                        self.emit(W::I32Eq);
                        self.emit(W::If(BlockType::Empty));
                        self.ctx.push(Ctx::If);
                        self.do_branch(x, t);
                        self.ctx.pop();
                        self.emit(W::End);
                    }
                } else {
                    let labels: Vec<u32> = targets
                        .iter()
                        .map(|&t| {
                            if self.rpo_num[t as usize] <= self.rpo_num[x as usize] {
                                self.label(Ctx::Loop(t))
                            } else {
                                self.label(Ctx::Block(t))
                            }
                        })
                        .collect();
                    self.get(index);
                    let default = labels[0];
                    self.emit(W::BrTable(Cow::Owned(labels), default));
                }
                self.ctx.pop();
                self.emit(W::End);
                self.sync(dirty);
                self.get_code(fallback);
                let t = self.tmp_code();
                self.emit(W::LocalSet(t));
                self.call_lookup(t, true);
            }
            Term::Call { target, ret, cont } => {
                self.sync(dirty);
                match target {
                    CallTarget::Direct(addr) => match self.m.direct_target(addr) {
                        Some(fi) => {
                            self.emit(W::LocalGet(0));
                            self.emit(W::Call(fi));
                        }
                        None => {
                            self.code_const(addr);
                            let t = self.tmp_code();
                            self.emit(W::LocalSet(t));
                            self.call_lookup(t, false);
                        }
                    },
                    CallTarget::Indirect(v) => {
                        self.get_code(v);
                        let t = self.tmp_code();
                        self.emit(W::LocalSet(t));
                        self.call_lookup(t, false);
                    }
                }
                // Continue inline only if the callee returned to us.
                let t = self.tmp_code();
                self.emit(W::LocalTee(t));
                self.code_const(ret);
                self.emit(if self.code64() { W::I64Ne } else { W::I32Ne });
                self.emit(W::If(BlockType::Empty));
                self.emit(W::LocalGet(t));
                self.emit(W::Return);
                self.emit(W::End);
                let live = self.a.state_live_in(cont) & ALL_STATE;
                self.reload(live);
                self.do_branch(x, cont);
            }
            Term::Exit(addr) => {
                self.sync(dirty);
                match self.m.direct_target(addr) {
                    Some(fi) => {
                        self.emit(W::LocalGet(0));
                        self.emit(W::ReturnCall(fi));
                    }
                    None => {
                        self.code_const(addr);
                        self.emit(W::Return);
                    }
                }
            }
            Term::Ret(v) => {
                self.sync(dirty);
                self.get_code(v);
                self.emit(W::Return);
            }
            Term::JmpInd(v) => {
                self.sync(dirty);
                self.get_code(v);
                let t = self.tmp_code();
                self.emit(W::LocalSet(t));
                self.call_lookup(t, true);
            }
            Term::Fault { code, eip } => {
                self.emit(W::I32Const(0));
                self.set_fault_info(Ty::I32);
                self.raise(dirty, code, eip);
            }
            Term::None => self.emit(W::Unreachable),
        }
    }

    /// Fallback for irreducible control flow: a loop around a `br_table` on
    /// a label variable.
    fn dispatch_loop(&mut self, order: &[BlockId]) {
        let n = order.len();
        self.emit(W::I32Const(0));
        self.emit(W::LocalSet(self.label_local));
        self.emit(W::Loop(BlockType::Empty));
        self.ctx.push(Ctx::Dispatch);
        // Map block ids to dense case numbers.
        let mut case_of = vec![u32::MAX; self.f.blocks.len()];
        for (k, &b) in order.iter().enumerate() {
            case_of[b as usize] = k as u32;
        }
        for _ in 0..n {
            self.emit(W::Block(BlockType::Empty));
            self.ctx.push(Ctx::If);
        }
        // label -> case: br_table indexes by block id.
        let nb = self.f.blocks.len();
        let table: Vec<u32> = (0..nb)
            .map(|b| {
                let c = case_of[b];
                if c == u32::MAX {
                    0
                } else {
                    c
                }
            })
            .collect();
        self.emit(W::LocalGet(self.label_local));
        self.emit(W::BrTable(Cow::Owned(table), 0));
        for &b in order {
            self.ctx.pop();
            self.emit(W::End);
            self.emitted[b as usize] = true;
            self.block_body(b);
            self.terminator(b);
        }
        self.ctx.pop();
        self.emit(W::End);
    }

    // ---- Instructions ----------------------------------------------------------

    fn block_body(&mut self, b: BlockId) {
        let blk = &self.f.blocks[b as usize];
        let trace = opt::dirty_trace(blk, self.a.dirty_in[b as usize]);
        for (k, inst) in blk.insts.iter().enumerate() {
            self.inst(inst, trace[k]);
        }
    }

    fn mem64(&self) -> bool {
        self.m.cfg.mem64
    }

    /// Converts the value on the stack from `from` to the memory's address
    /// type.
    fn conv_ma(&mut self, from: Ty) {
        match (from, self.mem64()) {
            (Ty::I32, true) => self.emit(W::I64ExtendI32U),
            (Ty::I64, false) => self.emit(W::I32WrapI64),
            _ => {}
        }
    }

    /// Pushes vreg `v` as an address in the memory's address type.
    fn get_ma(&mut self, v: V) {
        self.get(v);
        let t = self.f.ty(v);
        self.conv_ma(t);
    }

    /// A local holding the address in vreg `v` in the memory's address type.
    fn addr_local(&mut self, v: V) -> u32 {
        let t = self.f.ty(v);
        let la = self.local_of[v as usize];
        if (t == Ty::I64) == self.mem64() {
            return la;
        }
        self.emit(W::LocalGet(la));
        self.conv_ma(t);
        self.emit(W::LocalSet(self.tmp_ma));
        self.tmp_ma
    }

    /// Pushes the guest-check global as an i64.
    fn guest_check_i64(&mut self) {
        self.emit(W::GlobalGet(G_GUEST_CHECK));
        if !self.mem64() {
            self.emit(W::I64ExtendI32U);
        }
    }

    /// Pushes `(addr + off) & (size - 1)` (non-zero when misaligned) for the
    /// memory-typed address in local `ma`.
    fn misaligned(&mut self, ma: u32, off: u32, size: u8) {
        self.emit(W::LocalGet(ma));
        if self.mem64() {
            self.emit(W::I32WrapI64);
        }
        if off != 0 {
            self.emit(W::I32Const(off as i32));
            self.emit(W::I32Add);
        }
        self.emit(W::I32Const(size as i32 - 1));
        self.emit(W::I32And);
    }

    /// Emits an address check for the access at vreg `v` + `off`.
    fn check_addr(&mut self, v: V, off: u32, dirty: StateMask, eip: u64) {
        let addr = self.local_of[v as usize];
        if self.f.ty(v) == Ty::I64 || self.mem64() {
            self.emit(W::LocalGet(addr));
            if self.f.ty(v) == Ty::I64 {
                if off != 0 {
                    self.emit(W::I64Const(off as i64));
                    self.emit(W::I64Add);
                }
            } else {
                if off != 0 {
                    self.emit(W::I32Const(off as i32));
                    self.emit(W::I32Add);
                }
                self.emit(W::I64ExtendI32U);
            }
            self.emit(W::LocalTee(self.tmp_i64b));
            self.emit(W::I64Const(NULL_LIMIT as i64));
            self.emit(W::I64Sub);
            self.guest_check_i64();
            self.emit(W::I64GtU);
            self.emit(W::If(BlockType::Empty));
            self.emit(W::LocalGet(self.tmp_i64b));
            self.emit(W::I32WrapI64);
            self.emit(W::LocalSet(self.tmp_i32b));
            self.raise(dirty & FAULT_SYNC, fault::ACCESS_VIOLATION, eip);
            self.emit(W::End);
            return;
        }
        self.emit(W::LocalGet(addr));
        if off != 0 {
            self.emit(W::I32Const(off as i32));
            self.emit(W::I32Add);
        }
        self.emit(W::LocalTee(self.tmp_i32b));
        self.emit(W::I32Const(NULL_LIMIT as i32));
        self.emit(W::I32Sub);
        self.emit(W::GlobalGet(G_GUEST_CHECK));
        self.emit(W::I32GtU);
        self.emit(W::If(BlockType::Empty));
        self.raise(dirty & FAULT_SYNC, fault::ACCESS_VIOLATION, eip);
        self.emit(W::End);
    }

    /// Notifies the host when a store hits a page holding translated code.
    /// `ma` is a local holding the address in the memory's address type.
    fn check_code_write(&mut self, ma: u32, off: u32) {
        if self.mem64() {
            let a = self.tmp_i64b;
            self.emit(W::LocalGet(ma));
            if off != 0 {
                self.emit(W::I64Const(off as i64));
                self.emit(W::I64Add);
            }
            self.emit(W::LocalTee(a));
            self.emit(W::I64Const(15));
            self.emit(W::I64ShrU);
            self.emit(W::GlobalGet(G_CODE_BITMAP));
            self.emit(W::I64Add);
            self.emit(W::I32Load8U(memarg(0, 0)));
            self.emit(W::LocalGet(a));
            self.emit(W::I32WrapI64);
            self.emit(W::I32Const(12));
            self.emit(W::I32ShrU);
            self.emit(W::I32Const(7));
            self.emit(W::I32And);
            self.emit(W::I32ShrU);
            self.emit(W::I32Const(1));
            self.emit(W::I32And);
            self.emit(W::If(BlockType::Empty));
            self.emit(W::LocalGet(0));
            self.emit(W::LocalGet(a));
            self.emit(W::Call(F_CODE_WRITE));
            self.emit(W::End);
            return;
        }
        let a = self.tmp_i32b;
        self.emit(W::LocalGet(ma));
        if off != 0 {
            self.emit(W::I32Const(off as i32));
            self.emit(W::I32Add);
        }
        self.emit(W::LocalTee(a));
        self.emit(W::I32Const(15));
        self.emit(W::I32ShrU);
        self.emit(W::GlobalGet(G_CODE_BITMAP));
        self.emit(W::I32Add);
        self.emit(W::I32Load8U(memarg(0, 0)));
        self.emit(W::LocalGet(a));
        self.emit(W::I32Const(12));
        self.emit(W::I32ShrU);
        self.emit(W::I32Const(7));
        self.emit(W::I32And);
        self.emit(W::I32ShrU);
        self.emit(W::I32Const(1));
        self.emit(W::I32And);
        self.emit(W::If(BlockType::Empty));
        self.emit(W::LocalGet(0));
        self.emit(W::LocalGet(a));
        self.emit(W::Call(F_CODE_WRITE));
        self.emit(W::End);
    }

    fn needs_check(&self, mem: &Mem) -> bool {
        mem.space == Space::Guest && self.m.cfg.mem_checks
    }

    fn inst(&mut self, inst: &Inst, dirty: StateMask) {
        let ty = inst.dst.map(|d| self.f.ty(d));
        match &inst.op {
            Op::Const(c) => {
                let c = *c;
                match ty.unwrap_or(Ty::I32) {
                    Ty::I32 => self.emit(W::I32Const(c as u32 as i32)),
                    Ty::I64 => self.emit(W::I64Const(c as i64)),
                    Ty::F32 => self.emit(W::F32Const(f32::from_bits(c as u32).into())),
                    Ty::F64 => self.emit(W::F64Const(f64::from_bits(c).into())),
                    Ty::V128 => self.emit(W::V128Const(c as i128)),
                }
            }
            Op::Copy(a) => self.get(*a),
            Op::Bin(op, a, b) => {
                self.get(*a);
                self.get(*b);
                self.emit(bin_instr(*op));
            }
            Op::Un(op, a) => {
                self.get(*a);
                self.emit(un_instr(*op));
            }
            Op::Select { cond, t, f } => {
                self.get(*t);
                self.get(*f);
                self.get(*cond);
                self.emit(W::Select);
            }
            Op::Load { addr, mem } => {
                if self.needs_check(mem) {
                    self.check_addr(*addr, mem.offset, dirty, inst.eip);
                }
                let la = self.addr_local(*addr);
                let ty = ty.unwrap();
                if mem.atomic && mem.size > 1 {
                    // Atomics trap when misaligned; fall back to a plain load.
                    self.misaligned(la, mem.offset, mem.size);
                    self.emit(W::If(BlockType::Result(val_type(ty))));
                    self.emit(W::LocalGet(la));
                    self.emit(load_instr(ty, mem, false));
                    self.emit(W::Else);
                    self.emit(W::LocalGet(la));
                    self.emit(load_instr(ty, mem, true));
                    self.emit(W::End);
                } else {
                    self.emit(W::LocalGet(la));
                    self.emit(load_instr(ty, mem, mem.atomic));
                }
            }
            Op::Store { addr, val, mem } => {
                if self.needs_check(mem) {
                    self.check_addr(*addr, mem.offset, dirty, inst.eip);
                }
                let la = self.addr_local(*addr);
                let vty = self.f.ty(*val);
                if mem.atomic && mem.size > 1 {
                    self.misaligned(la, mem.offset, mem.size);
                    self.emit(W::If(BlockType::Empty));
                    self.emit(W::LocalGet(la));
                    self.get(*val);
                    self.emit(store_instr(vty, mem, false));
                    self.emit(W::Else);
                    self.emit(W::LocalGet(la));
                    self.get(*val);
                    self.emit(store_instr(vty, mem, true));
                    self.emit(W::End);
                } else {
                    self.emit(W::LocalGet(la));
                    self.get(*val);
                    self.emit(store_instr(vty, mem, mem.atomic));
                }
                if mem.space == Space::Guest && self.m.cfg.smc_checks {
                    self.check_code_write(la, mem.offset);
                }
            }
            Op::AtomicRmw { op, addr, val, mem } => {
                if mem.space == Space::Guest && self.m.cfg.mem_checks {
                    self.check_addr(*addr, mem.offset, dirty, inst.eip);
                }
                let mut la = self.addr_local(*addr);
                // The result may take over the address's local (the address
                // dies here), but the code-write check below still needs the
                // address. Only code with 64-bit addresses has the spare local
                // (32-bit code would need a new one, changing its output).
                if mem.space == Space::Guest
                    && self.m.cfg.smc_checks
                    && self.tmp_ma != UNASSIGNED
                    && inst.dst.is_some_and(|d| self.local_of[d as usize] == la)
                {
                    self.emit(W::LocalGet(la));
                    self.emit(W::LocalSet(self.tmp_ma));
                    la = self.tmp_ma;
                }
                let ty = ty.unwrap_or(Ty::I32);
                let wide = ty == Ty::I64;
                let tmp = if wide { self.tmp_i64 } else { self.tmp_i32 };
                // Misaligned: plain load + op + store (not atomic).
                self.misaligned(la, mem.offset, mem.size);
                self.emit(W::If(BlockType::Result(val_type(ty))));
                {
                    self.emit(W::LocalGet(la));
                    self.emit(load_instr(ty, mem, false));
                    self.emit(W::LocalSet(tmp));
                    self.emit(W::LocalGet(la));
                    match op {
                        RmwOp::Xchg => self.get(*val),
                        _ => {
                            self.emit(W::LocalGet(tmp));
                            self.get(*val);
                            self.emit(match (op, wide) {
                                (RmwOp::Add, false) => W::I32Add,
                                (RmwOp::Sub, false) => W::I32Sub,
                                (RmwOp::And, false) => W::I32And,
                                (RmwOp::Or, false) => W::I32Or,
                                (_, false) => W::I32Xor,
                                (RmwOp::Add, true) => W::I64Add,
                                (RmwOp::Sub, true) => W::I64Sub,
                                (RmwOp::And, true) => W::I64And,
                                (RmwOp::Or, true) => W::I64Or,
                                (_, true) => W::I64Xor,
                            });
                        }
                    }
                    self.emit(store_instr(ty, mem, false));
                    self.emit(W::LocalGet(tmp));
                }
                self.emit(W::Else);
                self.emit(W::LocalGet(la));
                self.get(*val);
                self.emit(rmw_instr(*op, mem, wide));
                self.emit(W::End);
                if mem.space == Space::Guest && self.m.cfg.smc_checks {
                    if let Some(d) = inst.dst {
                        self.set(d);
                    } else {
                        self.emit(W::Drop);
                    }
                    self.check_code_write(la, mem.offset);
                    return;
                }
            }
            Op::AtomicCmpxchg {
                addr,
                expected,
                new,
                mem,
            } => {
                if mem.space == Space::Guest && self.m.cfg.mem_checks {
                    self.check_addr(*addr, mem.offset, dirty, inst.eip);
                }
                let la = self.addr_local(*addr);
                let ty = ty.unwrap_or(Ty::I32);
                let (tmp, eq) = if ty == Ty::I64 {
                    (self.tmp_i64, W::I64Eq)
                } else {
                    (self.tmp_i32, W::I32Eq)
                };
                self.misaligned(la, mem.offset, mem.size);
                self.emit(W::If(BlockType::Result(val_type(ty))));
                {
                    // old = [a]; [a] = old == expected ? new : old
                    self.emit(W::LocalGet(la));
                    self.emit(load_instr(ty, mem, false));
                    self.emit(W::LocalSet(tmp));
                    self.emit(W::LocalGet(la));
                    self.get(*new);
                    self.emit(W::LocalGet(tmp));
                    self.emit(W::LocalGet(tmp));
                    self.get(*expected);
                    self.emit(eq);
                    self.emit(W::Select);
                    self.emit(store_instr(ty, mem, false));
                    self.emit(W::LocalGet(tmp));
                }
                self.emit(W::Else);
                self.emit(W::LocalGet(la));
                self.get(*expected);
                self.get(*new);
                self.emit(cmpxchg_instr(ty, mem));
                self.emit(W::End);
            }
            Op::MemCopy { dst, src, len } => {
                if self.m.cfg.mem_checks {
                    for v in [*dst, *src] {
                        self.check_range(v, *len, dirty, inst.eip);
                    }
                }
                self.get_ma(*dst);
                self.get_ma(*src);
                self.get_ma(*len);
                self.emit(W::MemoryCopy {
                    src_mem: 0,
                    dst_mem: 0,
                });
                if self.m.cfg.smc_checks {
                    self.check_code_write_range(*dst, *len);
                }
            }
            Op::MemFill { dst, val, len } => {
                if self.m.cfg.mem_checks {
                    self.check_range(*dst, *len, dirty, inst.eip);
                }
                self.get_ma(*dst);
                self.get(*val);
                self.get_ma(*len);
                self.emit(W::MemoryFill(0));
                if self.m.cfg.smc_checks {
                    self.check_code_write_range(*dst, *len);
                }
            }
            Op::CallHelper(h, args) => {
                for &a in args {
                    self.get(a);
                }
                let fi = self.m.helper_func_index(*h);
                self.emit(W::Call(fi));
            }
            Op::Math { op, a, b } => {
                self.emit(W::I32Const(*op as i32));
                self.get(*a);
                self.get(*b);
                self.emit(W::Call(F_MATH));
            }
            Op::Vec(op, args) => {
                if matches!(op, VecOp::Zero) {
                    self.emit(W::V128Const(0));
                } else {
                    for &a in args {
                        self.get(a);
                    }
                    self.emit(vec_instr(op));
                }
            }
            Op::FaultIf { cond, code, info } => {
                self.get(*cond);
                self.emit(W::If(BlockType::Empty));
                self.get(*info);
                let t = self.f.ty(*info);
                self.set_fault_info(t);
                self.raise(dirty & FAULT_SYNC, *code, inst.eip);
                self.emit(W::End);
            }
            Op::Cond(_) | Op::Eflags => panic!("flag reads must be lowered before codegen"),
        }
        match inst.dst {
            Some(d) => self.set(d),
            None => {
                if matches!(inst.op, Op::AtomicRmw { .. } | Op::AtomicCmpxchg { .. }) {
                    self.emit(W::Drop);
                }
            }
        }
    }

    /// Checks that [addr, addr+len) lies in the guest region.
    fn check_range(&mut self, addr: V, len: V, dirty: StateMask, eip: u64) {
        if self.f.ty(addr) == Ty::I64 || self.mem64() {
            // In i64: fault if addr < NULL_LIMIT || addr + len - NULL_LIMIT > check
            let wide = self.f.ty(addr) == Ty::I64;
            self.get(addr);
            if !wide {
                self.emit(W::I64ExtendI32U);
            }
            self.emit(W::LocalTee(self.tmp_i64b));
            self.emit(W::I32WrapI64);
            self.emit(W::LocalSet(self.tmp_i32b));
            self.emit(W::LocalGet(self.tmp_i64b));
            self.emit(W::I64Const(NULL_LIMIT as i64));
            self.emit(W::I64LtU);
            self.emit(W::LocalGet(self.tmp_i64b));
            self.get(len);
            if !wide {
                self.emit(W::I64ExtendI32U);
            }
            self.emit(W::I64Add);
            self.emit(W::I64Const(NULL_LIMIT as i64));
            self.emit(W::I64Sub);
            self.guest_check_i64();
            self.emit(W::I64GtU);
            self.emit(W::I32Or);
            self.emit(W::If(BlockType::Empty));
            self.raise(dirty & FAULT_SYNC, fault::ACCESS_VIOLATION, eip);
            self.emit(W::End);
            return;
        }
        // fault if addr < NULL_LIMIT || addr + len > guest_limit || overflow
        self.get(addr);
        self.emit(W::LocalSet(self.tmp_i32b));
        self.get(addr);
        self.emit(W::I32Const(NULL_LIMIT as i32));
        self.emit(W::I32LtU);
        self.get(addr);
        self.get(len);
        self.emit(W::I32Add);
        self.emit(W::I32Const(NULL_LIMIT as i32));
        self.emit(W::I32Sub);
        self.emit(W::GlobalGet(G_GUEST_CHECK));
        self.emit(W::I32GtU);
        self.emit(W::I32Or);
        self.emit(W::If(BlockType::Empty));
        self.raise(dirty & FAULT_SYNC, fault::ACCESS_VIOLATION, eip);
        self.emit(W::End);
    }

    /// Notifies the host for each code page in [addr, addr+len).
    fn check_code_write_range(&mut self, addr: V, len: V) {
        // for (p = addr & ~0xfff; p < addr + len; p += 0x1000) check(p)
        let wide = self.mem64();
        let p = if wide { self.tmp_i64 } else { self.tmp_i32 };
        let (and, add, ge, k_mask, k_page) = if wide {
            (
                W::I64And,
                W::I64Add,
                W::I64GeU,
                W::I64Const(!0xfff),
                W::I64Const(0x1000),
            )
        } else {
            (
                W::I32And,
                W::I32Add,
                W::I32GeU,
                W::I32Const(!0xfff),
                W::I32Const(0x1000),
            )
        };
        self.get_ma(addr);
        self.emit(k_mask);
        self.emit(and.clone());
        self.emit(W::LocalSet(p));
        self.emit(W::Block(BlockType::Empty));
        self.emit(W::Loop(BlockType::Empty));
        self.emit(W::LocalGet(p));
        self.get_ma(addr);
        self.get_ma(len);
        self.emit(add.clone());
        self.emit(ge);
        self.emit(W::BrIf(1));
        self.check_code_write(p, 0);
        self.emit(W::LocalGet(p));
        self.emit(k_page);
        self.emit(add);
        self.emit(W::LocalSet(p));
        self.emit(W::Br(0));
        self.emit(W::End);
        self.emit(W::End);
    }
}

fn load_instr(ty: Ty, mem: &Mem, atomic: bool) -> W<'static> {
    let size = mem.size as u32;
    let ma = memarg(mem.offset, if atomic { size.trailing_zeros() } else { 0 });
    match (ty, size, mem.signed, atomic) {
        (Ty::I32, 1, false, false) => W::I32Load8U(ma),
        (Ty::I32, 1, true, false) => W::I32Load8S(ma),
        (Ty::I32, 2, false, false) => W::I32Load16U(ma),
        (Ty::I32, 2, true, false) => W::I32Load16S(ma),
        (Ty::I32, 4, _, false) => W::I32Load(ma),
        (Ty::I32, 1, _, true) => W::I32AtomicLoad8U(ma),
        (Ty::I32, 2, _, true) => W::I32AtomicLoad16U(ma),
        (Ty::I32, 4, _, true) => W::I32AtomicLoad(ma),
        (Ty::I64, 8, _, false) => W::I64Load(ma),
        (Ty::I64, 8, _, true) => W::I64AtomicLoad(ma),
        (Ty::I64, 4, false, _) => W::I64Load32U(ma),
        (Ty::I64, 4, true, _) => W::I64Load32S(ma),
        (Ty::I64, 2, false, _) => W::I64Load16U(ma),
        (Ty::I64, 2, true, _) => W::I64Load16S(ma),
        (Ty::I64, 1, false, _) => W::I64Load8U(ma),
        (Ty::I64, 1, true, _) => W::I64Load8S(ma),
        (Ty::F64, 8, _, _) => W::F64Load(ma),
        (Ty::F32, 4, _, _) => W::F32Load(ma),
        (Ty::V128, 16, _, _) => W::V128Load(ma),
        (Ty::V128, 8, _, _) => W::V128Load64Zero(ma),
        (Ty::V128, 4, _, _) => W::V128Load32Zero(ma),
        other => panic!("unsupported load {other:?}"),
    }
}

fn store_instr(ty: Ty, mem: &Mem, atomic: bool) -> W<'static> {
    let size = mem.size as u32;
    let ma = memarg(mem.offset, if atomic { size.trailing_zeros() } else { 0 });
    match (ty, size, atomic) {
        (Ty::I32, 1, false) => W::I32Store8(ma),
        (Ty::I32, 2, false) => W::I32Store16(ma),
        (Ty::I32, 4, false) => W::I32Store(ma),
        (Ty::I32, 1, true) => W::I32AtomicStore8(ma),
        (Ty::I32, 2, true) => W::I32AtomicStore16(ma),
        (Ty::I32, 4, true) => W::I32AtomicStore(ma),
        (Ty::I64, 8, false) => W::I64Store(ma),
        (Ty::I64, 8, true) => W::I64AtomicStore(ma),
        (Ty::I64, 4, _) => W::I64Store32(ma),
        (Ty::I64, 2, _) => W::I64Store16(ma),
        (Ty::I64, 1, _) => W::I64Store8(ma),
        (Ty::F64, 8, _) => W::F64Store(ma),
        (Ty::F32, 4, _) => W::F32Store(ma),
        (Ty::V128, 16, _) => W::V128Store(ma),
        other => panic!("unsupported store {other:?}"),
    }
}

fn rmw_instr(op: RmwOp, mem: &Mem, wide: bool) -> W<'static> {
    let size = mem.size as u32;
    let ma = memarg(mem.offset, size.trailing_zeros());
    if wide {
        return match op {
            RmwOp::Add => W::I64AtomicRmwAdd(ma),
            RmwOp::Sub => W::I64AtomicRmwSub(ma),
            RmwOp::And => W::I64AtomicRmwAnd(ma),
            RmwOp::Or => W::I64AtomicRmwOr(ma),
            RmwOp::Xor => W::I64AtomicRmwXor(ma),
            RmwOp::Xchg => W::I64AtomicRmwXchg(ma),
        };
    }
    match (op, size) {
        (RmwOp::Add, 1) => W::I32AtomicRmw8AddU(ma),
        (RmwOp::Add, 2) => W::I32AtomicRmw16AddU(ma),
        (RmwOp::Add, _) => W::I32AtomicRmwAdd(ma),
        (RmwOp::Sub, 1) => W::I32AtomicRmw8SubU(ma),
        (RmwOp::Sub, 2) => W::I32AtomicRmw16SubU(ma),
        (RmwOp::Sub, _) => W::I32AtomicRmwSub(ma),
        (RmwOp::And, 1) => W::I32AtomicRmw8AndU(ma),
        (RmwOp::And, 2) => W::I32AtomicRmw16AndU(ma),
        (RmwOp::And, _) => W::I32AtomicRmwAnd(ma),
        (RmwOp::Or, 1) => W::I32AtomicRmw8OrU(ma),
        (RmwOp::Or, 2) => W::I32AtomicRmw16OrU(ma),
        (RmwOp::Or, _) => W::I32AtomicRmwOr(ma),
        (RmwOp::Xor, 1) => W::I32AtomicRmw8XorU(ma),
        (RmwOp::Xor, 2) => W::I32AtomicRmw16XorU(ma),
        (RmwOp::Xor, _) => W::I32AtomicRmwXor(ma),
        (RmwOp::Xchg, 1) => W::I32AtomicRmw8XchgU(ma),
        (RmwOp::Xchg, 2) => W::I32AtomicRmw16XchgU(ma),
        (RmwOp::Xchg, _) => W::I32AtomicRmwXchg(ma),
    }
}

fn cmpxchg_instr(ty: Ty, mem: &Mem) -> W<'static> {
    let size = mem.size as u32;
    let ma = memarg(mem.offset, size.trailing_zeros());
    match (ty, size) {
        (Ty::I64, _) => W::I64AtomicRmwCmpxchg(ma),
        (_, 1) => W::I32AtomicRmw8CmpxchgU(ma),
        (_, 2) => W::I32AtomicRmw16CmpxchgU(ma),
        _ => W::I32AtomicRmwCmpxchg(ma),
    }
}

fn vec_instr(op: &VecOp) -> W<'static> {
    use Lane as L;
    match op {
        VecOp::Bin(b) => vbin_instr(*b),
        VecOp::Un(u) => vun_instr(*u),
        VecOp::Shl(l) => match l {
            L::I8 => W::I8x16Shl,
            L::I16 => W::I16x8Shl,
            L::I32 => W::I32x4Shl,
            _ => W::I64x2Shl,
        },
        VecOp::ShrS(l) => match l {
            L::I8 => W::I8x16ShrS,
            L::I16 => W::I16x8ShrS,
            L::I32 => W::I32x4ShrS,
            _ => W::I64x2ShrS,
        },
        VecOp::ShrU(l) => match l {
            L::I8 => W::I8x16ShrU,
            L::I16 => W::I16x8ShrU,
            L::I32 => W::I32x4ShrU,
            _ => W::I64x2ShrU,
        },
        VecOp::Shuffle(lanes) => W::I8x16Shuffle(*lanes),
        VecOp::Splat(l) => match l {
            L::I8 => W::I8x16Splat,
            L::I16 => W::I16x8Splat,
            L::I32 => W::I32x4Splat,
            L::I64 => W::I64x2Splat,
            L::F32 => W::F32x4Splat,
            L::F64 => W::F64x2Splat,
        },
        VecOp::Extract(l, i) => match l {
            L::I8 => W::I8x16ExtractLaneU(*i),
            L::I16 => W::I16x8ExtractLaneU(*i),
            L::I32 => W::I32x4ExtractLane(*i),
            L::I64 => W::I64x2ExtractLane(*i),
            L::F32 => W::F32x4ExtractLane(*i),
            L::F64 => W::F64x2ExtractLane(*i),
        },
        VecOp::Replace(l, i) => match l {
            L::I8 => W::I8x16ReplaceLane(*i),
            L::I16 => W::I16x8ReplaceLane(*i),
            L::I32 => W::I32x4ReplaceLane(*i),
            L::I64 => W::I64x2ReplaceLane(*i),
            L::F32 => W::F32x4ReplaceLane(*i),
            L::F64 => W::F64x2ReplaceLane(*i),
        },
        VecOp::Bitmask(l) => match l {
            L::I8 => W::I8x16Bitmask,
            L::I16 => W::I16x8Bitmask,
            L::I32 | L::F32 => W::I32x4Bitmask,
            _ => W::I64x2Bitmask,
        },
        VecOp::Zero => W::V128Const(0),
    }
}

fn vbin_instr(op: VBin) -> W<'static> {
    use VBin::*;
    match op {
        I8x16Add => W::I8x16Add,
        I8x16Sub => W::I8x16Sub,
        I8x16AddSatS => W::I8x16AddSatS,
        I8x16AddSatU => W::I8x16AddSatU,
        I8x16SubSatS => W::I8x16SubSatS,
        I8x16SubSatU => W::I8x16SubSatU,
        I8x16Eq => W::I8x16Eq,
        I8x16GtS => W::I8x16GtS,
        I8x16MinU => W::I8x16MinU,
        I8x16MaxU => W::I8x16MaxU,
        I8x16AvgrU => W::I8x16AvgrU,
        I8x16NarrowI16x8S => W::I8x16NarrowI16x8S,
        I8x16NarrowI16x8U => W::I8x16NarrowI16x8U,
        I16x8Add => W::I16x8Add,
        I16x8Sub => W::I16x8Sub,
        I16x8AddSatS => W::I16x8AddSatS,
        I16x8AddSatU => W::I16x8AddSatU,
        I16x8SubSatS => W::I16x8SubSatS,
        I16x8SubSatU => W::I16x8SubSatU,
        I16x8Mul => W::I16x8Mul,
        I16x8Eq => W::I16x8Eq,
        I16x8GtS => W::I16x8GtS,
        I16x8MinS => W::I16x8MinS,
        I16x8MaxS => W::I16x8MaxS,
        I16x8AvgrU => W::I16x8AvgrU,
        I16x8NarrowI32x4S => W::I16x8NarrowI32x4S,
        I16x8NarrowI32x4U => W::I16x8NarrowI32x4U,
        I32x4Add => W::I32x4Add,
        I32x4Sub => W::I32x4Sub,
        I32x4Mul => W::I32x4Mul,
        I32x4Eq => W::I32x4Eq,
        I32x4GtS => W::I32x4GtS,
        I32x4DotI16x8S => W::I32x4DotI16x8S,
        I32x4ExtMulLowI16x8S => W::I32x4ExtMulLowI16x8S,
        I32x4ExtMulHighI16x8S => W::I32x4ExtMulHighI16x8S,
        I32x4ExtMulLowI16x8U => W::I32x4ExtMulLowI16x8U,
        I32x4ExtMulHighI16x8U => W::I32x4ExtMulHighI16x8U,
        I64x2Add => W::I64x2Add,
        I64x2Sub => W::I64x2Sub,
        I64x2Eq => W::I64x2Eq,
        I64x2ExtMulLowI32x4U => W::I64x2ExtMulLowI32x4U,
        F32x4Add => W::F32x4Add,
        F32x4Sub => W::F32x4Sub,
        F32x4Mul => W::F32x4Mul,
        F32x4Div => W::F32x4Div,
        F32x4Eq => W::F32x4Eq,
        F32x4Ne => W::F32x4Ne,
        F32x4Lt => W::F32x4Lt,
        F32x4Le => W::F32x4Le,
        F32x4Pmin => W::F32x4PMin,
        F32x4Pmax => W::F32x4PMax,
        F64x2Add => W::F64x2Add,
        F64x2Sub => W::F64x2Sub,
        F64x2Mul => W::F64x2Mul,
        F64x2Div => W::F64x2Div,
        F64x2Eq => W::F64x2Eq,
        F64x2Ne => W::F64x2Ne,
        F64x2Lt => W::F64x2Lt,
        F64x2Le => W::F64x2Le,
        F64x2Pmin => W::F64x2PMin,
        F64x2Pmax => W::F64x2PMax,
        V128And => W::V128And,
        V128Or => W::V128Or,
        V128Xor => W::V128Xor,
        V128AndNot => W::V128AndNot,
    }
}

fn vun_instr(op: VUn) -> W<'static> {
    use VUn::*;
    match op {
        V128Not => W::V128Not,
        F32x4Sqrt => W::F32x4Sqrt,
        F64x2Sqrt => W::F64x2Sqrt,
        F32x4Nearest => W::F32x4Nearest,
        F32x4Floor => W::F32x4Floor,
        F32x4Ceil => W::F32x4Ceil,
        F32x4Trunc => W::F32x4Trunc,
        F64x2Nearest => W::F64x2Nearest,
        F64x2Floor => W::F64x2Floor,
        F64x2Ceil => W::F64x2Ceil,
        F64x2Trunc => W::F64x2Trunc,
        F32x4ConvertI32x4S => W::F32x4ConvertI32x4S,
        I32x4TruncSatF32x4S => W::I32x4TruncSatF32x4S,
        F64x2ConvertLowI32x4S => W::F64x2ConvertLowI32x4S,
        I32x4TruncSatF64x2SZero => W::I32x4TruncSatF64x2SZero,
        F32x4DemoteF64x2Zero => W::F32x4DemoteF64x2Zero,
        F64x2PromoteLowF32x4 => W::F64x2PromoteLowF32x4,
        I16x8ExtAddPairwiseI8x16U => W::I16x8ExtAddPairwiseI8x16U,
        I32x4ExtAddPairwiseI16x8U => W::I32x4ExtAddPairwiseI16x8U,
    }
}

fn bin_instr(op: BinOp) -> W<'static> {
    use BinOp::*;
    match op {
        I32Add => W::I32Add,
        I32Sub => W::I32Sub,
        I32Mul => W::I32Mul,
        I32DivS => W::I32DivS,
        I32DivU => W::I32DivU,
        I32RemS => W::I32RemS,
        I32RemU => W::I32RemU,
        I32And => W::I32And,
        I32Or => W::I32Or,
        I32Xor => W::I32Xor,
        I32Shl => W::I32Shl,
        I32ShrS => W::I32ShrS,
        I32ShrU => W::I32ShrU,
        I32Rotl => W::I32Rotl,
        I32Rotr => W::I32Rotr,
        I32Eq => W::I32Eq,
        I32Ne => W::I32Ne,
        I32LtS => W::I32LtS,
        I32LtU => W::I32LtU,
        I32GtS => W::I32GtS,
        I32GtU => W::I32GtU,
        I32LeS => W::I32LeS,
        I32LeU => W::I32LeU,
        I32GeS => W::I32GeS,
        I32GeU => W::I32GeU,
        I64Add => W::I64Add,
        I64Sub => W::I64Sub,
        I64Mul => W::I64Mul,
        I64DivS => W::I64DivS,
        I64DivU => W::I64DivU,
        I64RemS => W::I64RemS,
        I64RemU => W::I64RemU,
        I64And => W::I64And,
        I64Or => W::I64Or,
        I64Xor => W::I64Xor,
        I64Shl => W::I64Shl,
        I64ShrS => W::I64ShrS,
        I64ShrU => W::I64ShrU,
        I64Eq => W::I64Eq,
        I64Ne => W::I64Ne,
        I64LtS => W::I64LtS,
        I64LtU => W::I64LtU,
        I64GtS => W::I64GtS,
        I64GtU => W::I64GtU,
        I64LeS => W::I64LeS,
        I64LeU => W::I64LeU,
        I64GeS => W::I64GeS,
        I64GeU => W::I64GeU,
        I64Rotl => W::I64Rotl,
        I64Rotr => W::I64Rotr,
        F64Add => W::F64Add,
        F64Sub => W::F64Sub,
        F64Mul => W::F64Mul,
        F64Div => W::F64Div,
        F64Min => W::F64Min,
        F64Max => W::F64Max,
        F64Copysign => W::F64Copysign,
        F64Eq => W::F64Eq,
        F64Ne => W::F64Ne,
        F64Lt => W::F64Lt,
        F64Gt => W::F64Gt,
        F64Le => W::F64Le,
        F64Ge => W::F64Ge,
        F32Add => W::F32Add,
        F32Sub => W::F32Sub,
        F32Mul => W::F32Mul,
        F32Div => W::F32Div,
        F32Min => W::F32Min,
        F32Max => W::F32Max,
        F32Eq => W::F32Eq,
        F32Lt => W::F32Lt,
        F32Le => W::F32Le,
    }
}

fn un_instr(op: UnOp) -> W<'static> {
    use UnOp::*;
    match op {
        I32Eqz => W::I32Eqz,
        I32Clz => W::I32Clz,
        I32Ctz => W::I32Ctz,
        I32Popcnt => W::I32Popcnt,
        I32Extend8S => W::I32Extend8S,
        I32Extend16S => W::I32Extend16S,
        I64Eqz => W::I64Eqz,
        I64ExtendI32S => W::I64ExtendI32S,
        I64ExtendI32U => W::I64ExtendI32U,
        I32WrapI64 => W::I32WrapI64,
        I64Clz => W::I64Clz,
        I64Ctz => W::I64Ctz,
        I64Popcnt => W::I64Popcnt,
        I64Extend8S => W::I64Extend8S,
        I64Extend16S => W::I64Extend16S,
        I64Extend32S => W::I64Extend32S,
        F64ConvertI64U => W::F64ConvertI64U,
        F32ConvertI64S => W::F32ConvertI64S,
        I64TruncSatF32S => W::I64TruncSatF32S,
        F64Neg => W::F64Neg,
        F64Abs => W::F64Abs,
        F64Sqrt => W::F64Sqrt,
        F64Ceil => W::F64Ceil,
        F64Floor => W::F64Floor,
        F64Trunc => W::F64Trunc,
        F64Nearest => W::F64Nearest,
        F64ConvertI32S => W::F64ConvertI32S,
        F64ConvertI64S => W::F64ConvertI64S,
        F64PromoteF32 => W::F64PromoteF32,
        F32DemoteF64 => W::F32DemoteF64,
        F32ConvertI32S => W::F32ConvertI32S,
        I32TruncSatF64S => W::I32TruncSatF64S,
        I64TruncSatF64S => W::I64TruncSatF64S,
        I32TruncSatF32S => W::I32TruncSatF32S,
        F64ReinterpretI64 => W::F64ReinterpretI64,
        I64ReinterpretF64 => W::I64ReinterpretF64,
        F32ReinterpretI32 => W::F32ReinterpretI32,
        I32ReinterpretF32 => W::I32ReinterpretF32,
    }
}

// ---- Helpers --------------------------------------------------------------------

/// Emits an expression tree; `map` gives the locals for FR, FA, FB, FC.
fn emit_e(out: &mut Vec<W<'static>>, e: &E, map: [u32; 4]) {
    match e {
        E::Fr => out.push(W::LocalGet(map[0])),
        E::Fa => out.push(W::LocalGet(map[1])),
        E::Fb => out.push(W::LocalGet(map[2])),
        E::Fc => out.push(W::LocalGet(map[3])),
        E::K(c) => out.push(W::I32Const(*c as i32)),
        E::K64(c) => out.push(W::I64Const(*c as i64)),
        E::Bin(op, a, b) => {
            emit_e(out, a, map);
            emit_e(out, b, map);
            out.push(bin_instr(*op));
        }
        E::Un(op, a) => {
            emit_e(out, a, map);
            out.push(un_instr(*op));
        }
    }
}

fn gen_helper(h: Helper, eflags_index: u32) -> wasm_encoder::Function {
    let mut out: Vec<W<'static>> = vec![];
    let mut locals: Vec<ValType> = vec![];
    match h {
        Helper::Eflags => {
            // params: fk fr fa fb fc
            let kinds: Vec<u32> = flags::all_kinds().collect();
            let n = kinds.len() as u32;
            out.push(W::Block(BlockType::Empty)); // default
            for _ in 0..n {
                out.push(W::Block(BlockType::Empty));
            }
            // idx = op * 3 + width_code, or n when op is out of range.
            out.push(W::LocalGet(0));
            out.push(W::I32Const(0xff));
            out.push(W::I32And);
            out.push(W::I32Const(3));
            out.push(W::I32Mul);
            out.push(W::LocalGet(0));
            out.push(W::I32Const(8));
            out.push(W::I32ShrU);
            out.push(W::I32Const(3));
            out.push(W::I32And);
            out.push(W::I32Add);
            out.push(W::BrTable(Cow::Owned((0..n).collect()), n));
            for &k in &kinds {
                out.push(W::End);
                emit_e(&mut out, &flags::eflags(k), [1, 2, 3, 4]);
                out.push(W::Return);
            }
            out.push(W::End);
            out.push(W::LocalGet(1));
            out.push(W::I32Const(crate::abi::flags::ARITH as i32));
            out.push(W::I32And);
        }
        Helper::EvalCond => {
            // params: cc fk fr fa fb fc; local 6 = eflags
            locals.push(ValType::I32);
            for p in 1..=5 {
                out.push(W::LocalGet(p));
            }
            out.push(W::Call(eflags_index));
            out.push(W::LocalSet(6));
            out.push(W::Block(BlockType::Empty));
            for _ in 0..16 {
                out.push(W::Block(BlockType::Empty));
            }
            out.push(W::LocalGet(0));
            out.push(W::I32Const(15));
            out.push(W::I32And);
            out.push(W::BrTable(Cow::Owned((0..16).collect()), 16));
            for cc in 0..16u8 {
                out.push(W::End);
                let e = flags::cond_from_eflags(Cc::from_u8(cc), E::Fr);
                emit_e(&mut out, &e, [6, 6, 6, 6]);
                out.push(W::Return);
            }
            out.push(W::End);
            out.push(W::I32Const(0));
        }
        Helper::Eflags64 => {
            // params: fk (i32), fr fa fb fc (i64)
            let kinds: Vec<u32> = flags::all_kinds64().collect();
            let n = kinds.len() as u32;
            out.push(W::Block(BlockType::Empty)); // default
            for _ in 0..n {
                out.push(W::Block(BlockType::Empty));
            }
            // idx = op * 4 + width_code, or n when op is out of range.
            out.push(W::LocalGet(0));
            out.push(W::I32Const(0xff));
            out.push(W::I32And);
            out.push(W::I32Const(2));
            out.push(W::I32Shl);
            out.push(W::LocalGet(0));
            out.push(W::I32Const(8));
            out.push(W::I32ShrU);
            out.push(W::I32Const(3));
            out.push(W::I32And);
            out.push(W::I32Add);
            out.push(W::BrTable(Cow::Owned((0..n).collect()), n));
            for &k in &kinds {
                out.push(W::End);
                emit_e(&mut out, &flags::for_x64(flags::eflags(k), k), [1, 2, 3, 4]);
                out.push(W::Return);
            }
            out.push(W::End);
            out.push(W::LocalGet(1));
            out.push(W::I32WrapI64);
            out.push(W::I32Const(crate::abi::flags::ARITH as i32));
            out.push(W::I32And);
        }
        Helper::EvalCond64 => {
            // params: cc fk (i32) fr fa fb fc (i64); local 6 = eflags
            locals.push(ValType::I32);
            for p in 1..=5 {
                out.push(W::LocalGet(p));
            }
            out.push(W::Call(eflags_index));
            out.push(W::LocalSet(6));
            out.push(W::Block(BlockType::Empty));
            for _ in 0..16 {
                out.push(W::Block(BlockType::Empty));
            }
            out.push(W::LocalGet(0));
            out.push(W::I32Const(15));
            out.push(W::I32And);
            out.push(W::BrTable(Cow::Owned((0..16).collect()), 16));
            for cc in 0..16u8 {
                out.push(W::End);
                let e = flags::cond_from_eflags(Cc::from_u8(cc), E::Fr);
                emit_e(&mut out, &e, [6, 6, 6, 6]);
                out.push(W::Return);
            }
            out.push(W::End);
            out.push(W::I32Const(0));
        }
        Helper::DivU128 => {
            // params: hi lo d (i64); locals: 3 = i (i32), 4 = top bit (i64).
            // Restoring division, one quotient bit per round; hi < d keeps
            // the remainder in 64 bits (with the bit shifted out in local 4).
            locals.push(ValType::I32);
            locals.push(ValType::I64);
            out.push(W::I32Const(64));
            out.push(W::LocalSet(3));
            out.push(W::Loop(BlockType::Empty));
            // top = hi >> 63; hi = hi << 1 | lo >> 63; lo <<= 1
            out.push(W::LocalGet(0));
            out.push(W::I64Const(63));
            out.push(W::I64ShrU);
            out.push(W::LocalSet(4));
            out.push(W::LocalGet(0));
            out.push(W::I64Const(1));
            out.push(W::I64Shl);
            out.push(W::LocalGet(1));
            out.push(W::I64Const(63));
            out.push(W::I64ShrU);
            out.push(W::I64Or);
            out.push(W::LocalSet(0));
            out.push(W::LocalGet(1));
            out.push(W::I64Const(1));
            out.push(W::I64Shl);
            out.push(W::LocalSet(1));
            // if (top || hi >= d) { hi -= d; lo |= 1 }
            out.push(W::LocalGet(4));
            out.push(W::I32WrapI64);
            out.push(W::LocalGet(0));
            out.push(W::LocalGet(2));
            out.push(W::I64GeU);
            out.push(W::I32Or);
            out.push(W::If(BlockType::Empty));
            out.push(W::LocalGet(0));
            out.push(W::LocalGet(2));
            out.push(W::I64Sub);
            out.push(W::LocalSet(0));
            out.push(W::LocalGet(1));
            out.push(W::I64Const(1));
            out.push(W::I64Or);
            out.push(W::LocalSet(1));
            out.push(W::End);
            out.push(W::LocalGet(3));
            out.push(W::I32Const(1));
            out.push(W::I32Sub);
            out.push(W::LocalTee(3));
            out.push(W::BrIf(0));
            out.push(W::End);
            out.push(W::LocalGet(1));
        }
        _ => crate::fpu_helpers::gen(h, &mut out, &mut locals),
    }
    out.push(W::End);
    let mut f = wasm_encoder::Function::new_with_locals_types(locals);
    for i in &out {
        f.instruction(i);
    }
    f
}

#[cfg(test)]
mod tests {
    #[test]
    fn eflags_helper_first() {
        let cfg = super::CodegenConfig::default();
        let g = super::ModuleGen::new(&cfg, &[]);
        assert_eq!(
            g.helper_func_index(crate::ir::Helper::Eflags),
            super::NUM_FUNC_IMPORTS
        );
    }
}
