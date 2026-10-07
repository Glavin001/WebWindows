//! Layer 6 — Generate WebAssembly.
//!
//! One WebAssembly function per x86 function, with type `(cpu: i32) -> i32`:
//! the result is the x86 address to continue at. Branches and loops are
//! rebuilt as structured control flow from the dominator tree (Ramsey,
//! "Beyond Relooper", 2022), after [`reducible`] gives every loop a single
//! entry; a graph that is still irreducible falls back to a dispatch loop.
//! Calls between functions in the module are direct; everything else goes
//! through the two-level address lookup into the shared function table.

use std::borrow::Cow;
use std::collections::HashMap;

use wasm_encoder::{
    BlockType, CodeSection, ConstExpr, CustomSection, ElementSection, Elements, EntityType,
    FunctionSection, GlobalType, ImportSection, Instruction as W, MemArg, MemoryType, Module,
    NameMap, NameSection, RefType, TableType, TypeSection, ValType,
};

use crate::abi::{self, addr::NULL_LIMIT, cpu, fault, imports, native_layout, store_map};
use crate::flags::{self, E};
use crate::ir::*;
use crate::opt::{self, Analysis, StateMask, ALL_STATE, FAULT_SYNC};
use crate::reducible;

#[derive(Debug, Clone)]
pub struct CodegenConfig {
    pub mem_checks: bool,
    pub smc_checks: bool,
    /// The guest limit the module will run under, when the host knows it at
    /// translation time: checks then compare against a constant, which
    /// frees a register in V8's code. `None` reads the `guest_limit` import,
    /// so the module runs under any limit. Recorded in the module metadata;
    /// the runtime refuses a module made for another limit.
    pub guest_limit: Option<u32>,
}

impl Default for CodegenConfig {
    fn default() -> Self {
        CodegenConfig {
            mem_checks: true,
            smc_checks: true,
            guest_limit: None,
        }
    }
}

// Type indices.
const T_FN: u32 = 0;
const T_FAULT: u32 = 1;
const T_MATH: u32 = 2;
const T_FIRST_HELPER: u32 = 3;

// Imported function indices.
const F_FAULT: u32 = 0;
const F_MATH: u32 = 1;
const NUM_FUNC_IMPORTS: u32 = 2;

// Imported global indices.
const G_TABLE_BASE: u32 = 0;
const G_LOOKUP_L1: u32 = 1;
const G_GUEST_CHECK: u32 = 2;
const G_STORE_MAP: u32 = 3;
const G_ZERO_L2: u32 = 4;

/// How far (bytes) a load may be from an address that passed a check and
/// still skip its own (see `check_covered`).
const CHECK_WINDOW: u32 = 0x1000 - 16;

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
    func_index: HashMap<u32, u32>,
    helpers: Vec<Helper>,
    helper_types: Vec<(Vec<Ty>, Ty)>,
    /// Symbol names by x86 address, for the name section.
    names: HashMap<u32, String>,
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
                        if !helpers.contains(h) {
                            helpers.push(*h);
                        }
                    }
                }
            }
        }
        ModuleGen {
            cfg,
            func_index,
            helpers,
            helper_types: vec![],
            names: HashMap::new(),
        }
    }

    /// Labels translated functions with these symbols in the name section
    /// (functions without one are named by address).
    pub fn with_names(mut self, names: &HashMap<u32, String>) -> Self {
        self.names = names.clone();
        self
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

    pub fn direct_target(&self, addr: u32) -> Option<u32> {
        self.func_index
            .get(&addr)
            .map(|&i| self.translated_func_index(i))
    }

    /// Generates the module bytes.
    pub fn build(mut self, funcs: &[Function], meta_json: &str) -> Vec<u8> {
        let mut module = Module::new();

        // Types.
        let mut types = TypeSection::new();
        types.ty().function([ValType::I32], [ValType::I32]);
        types
            .ty()
            .function([ValType::I32, ValType::I32, ValType::I32, ValType::I32], []);
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
        imp.import(imports::MODULE, imports::MATH, EntityType::Function(T_MATH));
        imp.import(
            imports::MODULE,
            imports::MEMORY,
            MemoryType {
                minimum: 1,
                maximum: Some(65536),
                memory64: false,
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
        for name in [
            imports::TABLE_BASE,
            imports::LOOKUP_L1,
            imports::GUEST_LIMIT,
            imports::STORE_MAP,
            imports::ZERO_L2,
        ] {
            imp.import(
                imports::MODULE,
                name,
                GlobalType {
                    val_type: ValType::I32,
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
            code.function(&gen_helper(h));
        }
        for f in funcs {
            code.function(&self.gen_function(f));
        }
        module.section(&code);

        // Names, for profilers and debuggers: `symbol@address` or
        // `x86_address`.
        let mut fnames = NameMap::new();
        for (i, h) in self.helpers.iter().enumerate() {
            fnames.append(NUM_FUNC_IMPORTS + i as u32, &format!("helper_{h:?}"));
        }
        for (i, f) in funcs.iter().enumerate() {
            let name = match self.names.get(&f.entry) {
                Some(n) => format!("{n}@{:x}", f.entry),
                None => format!("x86_{:x}", f.entry),
            };
            fnames.append(self.translated_func_index(i as u32), &name);
        }
        let mut names = NameSection::new();
        names.functions(&fnames);
        module.section(&names);

        // Function address map and metadata.
        let mut map = Vec::with_capacity(4 + funcs.len() * 4);
        map.extend_from_slice(&(funcs.len() as u32).to_le_bytes());
        for f in funcs {
            map.extend_from_slice(&f.entry.to_le_bytes());
        }
        module.section(&CustomSection {
            name: Cow::Borrowed(abi::FUNCS_SECTION),
            data: Cow::Owned(map),
        });
        module.section(&CustomSection {
            name: Cow::Borrowed(abi::META_SECTION),
            data: Cow::Borrowed(meta_json.as_bytes()),
        });
        module.finish()
    }

    pub fn gen_function(&self, f: &Function) -> wasm_encoder::Function {
        let fixed;
        let f = if reducible::is_reducible(f) {
            f
        } else {
            let mut c = f.clone();
            reducible::make_reducible(&mut c);
            fixed = c;
            &fixed
        };
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
    /// Scratch for the store-map byte in code-write checks.
    tmp_map: u32,
    tmp_i32b: u32,
    tmp_i64: u32,
    label_local: u32,
    emitted: Vec<bool>,
    /// Addresses whose guest-limit check passed, as (vreg, offset): loads
    /// near them skip the check (see `check_covered`). `facts_local` holds
    /// any vreg and lasts until the end of the block or the vreg's next
    /// definition; `facts_tree` holds single-definition temporaries and is
    /// scoped to the dominator subtree being emitted.
    facts_local: Vec<(V, u32)>,
    facts_tree: Vec<(V, u32)>,
    /// Vregs with one definition, in a block that dominates their uses.
    def_block: Vec<u32>,
    structured: bool,
    cur_block: BlockId,
    /// Within the current block, temporaries known to equal `root + off`.
    alias: Vec<(V, V, u32)>,
    /// Constant value of single-definition temporaries defined by a constant.
    const_of: Vec<Option<u32>>,
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
            tmp_map: 0,
            tmp_i32b: 0,
            tmp_i64: 0,
            label_local: 0,
            emitted: vec![false; n],
            facts_local: vec![],
            facts_tree: vec![],
            def_block: vec![],
            structured: true,
            cur_block: 0,
            alias: vec![],
            const_of: vec![],
        };
        g.local_of[CPU as usize] = 0;
        g.assign_locals();
        g.tmp_i32 = g.new_local(ValType::I32);
        g.tmp_map = g.new_local(ValType::I32);
        g.tmp_i32b = g.new_local(ValType::I32);
        g.tmp_i64 = g.new_local(ValType::I64);
        g.label_local = g.new_local(ValType::I32);
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
            let (off, ty) = state_home(v).unwrap();
            self.emit(W::LocalGet(0));
            self.get(v);
            match (ty, state_home_size(v)) {
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
            let (off, ty) = state_home(v).unwrap();
            self.emit(W::LocalGet(0));
            match (ty, state_home_size(v)) {
                (Ty::V128, _) => self.emit(W::V128Load(memarg(off, 4))),
                (Ty::I64, _) => self.emit(W::I64Load(memarg(off, 3))),
                (_, 2) => self.emit(W::I32Load16U(memarg(off, 1))),
                _ => self.emit(W::I32Load(memarg(off, 2))),
            }
            self.set(v);
        }
    }

    fn store_eip_const(&mut self, eip: u32) {
        self.emit(W::LocalGet(0));
        self.emit(W::I32Const(eip as i32));
        self.emit(W::I32Store(memarg(cpu::EIP, 2)));
    }

    /// Raises a fault: writes back `mask`, records eip and calls the host.
    /// The address/info value must already be in `tmp_i32b`.
    fn raise(&mut self, mask: StateMask, code: u32, eip: u32) {
        self.sync(mask);
        self.store_eip_const(eip);
        self.emit(W::LocalGet(0));
        self.emit(W::I32Const(code as i32));
        self.emit(W::I32Const(eip as i32));
        self.emit(W::LocalGet(self.tmp_i32b));
        self.emit(W::Call(F_FAULT));
        self.emit(W::Unreachable);
    }

    // ---- Lookup --------------------------------------------------------------

    /// With the value on the stack as an index into one of the runtime's
    /// tables (`global` names its base), returns the memory offset to
    /// access it with: the table's constant address when the guest limit is
    /// known at translation time, else 0 after adding the imported base.
    fn native_table(&mut self, global: u32) -> u32 {
        match self.m.cfg.guest_limit {
            Some(l) => {
                let off = match global {
                    G_LOOKUP_L1 => native_layout::LOOKUP_L1,
                    G_ZERO_L2 => native_layout::ZERO_L2,
                    _ => native_layout::STORE_MAP,
                };
                l + off
            }
            None => {
                self.emit(W::GlobalGet(global));
                self.emit(W::I32Add);
                0
            }
        }
    }

    /// Pushes the table index for the address in local `t`.
    fn lookup_index(&mut self, t: u32) {
        self.emit(W::LocalGet(t));
        self.emit(W::I32Const(12));
        self.emit(W::I32ShrU);
        self.emit(W::I32Const(2));
        self.emit(W::I32Shl);
        let o = self.native_table(G_LOOKUP_L1);
        self.emit(W::I32Load(memarg(o, 2)));
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
        self.emit(W::I32Store(memarg(cpu::EIP, 2)));
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
        // Temporaries with a single definition (state vregs have many).
        let mut defs = vec![0u32; f.vtypes.len()];
        self.def_block = vec![u32::MAX; f.vtypes.len()];
        for (bi, b) in f.blocks.iter().enumerate() {
            for inst in &b.insts {
                if let Some(d) = inst.dst {
                    defs[d as usize] += 1;
                    self.def_block[d as usize] = bi as u32;
                }
            }
        }
        for v in 0..f.vtypes.len() {
            if v < NUM_STATE as usize || defs[v] != 1 {
                self.def_block[v] = u32::MAX;
            }
        }
        self.const_of = vec![None; f.vtypes.len()];
        for b in &f.blocks {
            for inst in &b.insts {
                if let (Some(d), Op::Const(c)) = (inst.dst, &inst.op) {
                    if self.def_block[d as usize] != u32::MAX && f.vtypes[d as usize] == Ty::I32 {
                        self.const_of[d as usize] = Some(*c as u32);
                    }
                }
            }
        }
        // Entry: load live state.
        let live = self.a.state_live_in(0) & ALL_STATE;
        self.reload(live);
        if reducible {
            self.do_tree(0);
        } else {
            self.structured = false;
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
        // Everything emitted from here on until this returns is dominated
        // by `x`, so facts established in `x` hold there.
        let scope = self.facts_tree.len();
        self.do_tree_inner(x);
        self.facts_tree.truncate(scope);
    }

    fn do_tree_inner(&mut self, x: BlockId) {
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
                self.get(fallback);
                self.emit(W::LocalSet(self.tmp_i32));
                self.call_lookup(self.tmp_i32, true);
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
                            self.emit(W::I32Const(addr as i32));
                            self.emit(W::LocalSet(self.tmp_i32));
                            self.call_lookup(self.tmp_i32, false);
                        }
                    },
                    CallTarget::Indirect(v) => {
                        self.get(v);
                        self.emit(W::LocalSet(self.tmp_i32));
                        self.call_lookup(self.tmp_i32, false);
                    }
                }
                // Continue inline only if the callee returned to us.
                self.emit(W::LocalTee(self.tmp_i32));
                self.emit(W::I32Const(ret as i32));
                self.emit(W::I32Ne);
                self.emit(W::If(BlockType::Empty));
                self.emit(W::LocalGet(self.tmp_i32));
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
                        self.emit(W::I32Const(addr as i32));
                        self.emit(W::Return);
                    }
                }
            }
            Term::Ret(v) => {
                self.sync(dirty);
                self.get(v);
                self.emit(W::Return);
            }
            Term::JmpInd(v) => {
                self.sync(dirty);
                self.get(v);
                self.emit(W::LocalSet(self.tmp_i32));
                self.call_lookup(self.tmp_i32, true);
            }
            Term::Fault { code, eip } => {
                self.emit(W::I32Const(0));
                self.emit(W::LocalSet(self.tmp_i32b));
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
        self.facts_local.clear();
        self.alias.clear();
        self.cur_block = b;
        for (k, inst) in blk.insts.iter().enumerate() {
            self.inst(inst, trace[k]);
            if let Some(d) = inst.dst {
                // Facts and aliases about the old value of `d` are void.
                self.facts_local.retain(|&(v, _)| v != d);
                self.alias.retain(|&(t, r, _)| t != d && r != d);
                if let Op::Bin(BinOp::I32Add, x, y) = inst.op {
                    let c = (self.const_of[y as usize], self.const_of[x as usize]);
                    let (root, off) = match c {
                        (Some(c), _) => self.resolve(x, c),
                        (None, Some(c)) => self.resolve(y, c),
                        _ => (d, 0),
                    };
                    if root != d {
                        self.alias.push((d, root, off));
                    }
                }
            }
        }
    }

    // ---- Check elimination -----------------------------------------------------
    //
    // A passed guest-limit check puts `addr + off` in [NULL_LIMIT,
    // guest limit - 16]. A later load at `addr + off2`, with the same `addr`
    // value and |off2 - off| below CHECK_WINDOW, then lies within 4 KB of
    // that range: in the guest's null region or in the first page above the
    // guest limit (the lookup's first level), so reading it cannot harm
    // the runtime. Such loads skip their check. The cost is precision: a
    // load there reads memory instead of faulting, which only a pointer
    // within 4 KB of those boundaries can notice. Stores are never skipped
    // (the store map has to see every page they touch).

    /// `v + off` as `root + off'`, following this block's aliases.
    fn resolve(&self, v: V, off: u32) -> (V, u32) {
        match self.alias.iter().find(|&&(t, _, _)| t == v) {
            Some(&(_, r, o)) => (r, o.wrapping_add(off)),
            None => (v, off),
        }
    }

    /// Whether a passed check covers a load at `v + off`.
    fn check_covered(&self, v: V, off: u32) -> bool {
        let (v, off) = self.resolve(v, off);
        self.facts_local
            .iter()
            .chain(self.facts_tree.iter())
            .any(|&(fv, fo)| {
                fv == v && (fo.wrapping_sub(off) as i32).unsigned_abs() <= CHECK_WINDOW
            })
    }

    /// Records that the address `v + off` passed a check.
    fn checked(&mut self, v: V, off: u32) {
        let (v, off) = self.resolve(v, off);
        let d = self.def_block.get(v as usize).copied().unwrap_or(u32::MAX);
        if self.structured && d != u32::MAX && self.dominates(d, self.cur_block) {
            self.facts_tree.push((v, off));
        } else {
            self.facts_local.push((v, off));
        }
    }

    /// Pushes the largest valid `address - NULL_LIMIT` for an access of up
    /// to 16 bytes.
    fn guest_check_value(&mut self) {
        match self.m.cfg.guest_limit {
            Some(l) => self.emit(W::I32Const(l.wrapping_sub(NULL_LIMIT + 16) as i32)),
            None => self.emit(W::GlobalGet(G_GUEST_CHECK)),
        }
    }

    /// Emits an address check for `size` bytes at local `addr` + `off`.
    fn check_addr(&mut self, addr: u32, off: u32, dirty: StateMask, eip: u32) {
        self.emit(W::LocalGet(addr));
        if off != 0 {
            self.emit(W::I32Const(off as i32));
            self.emit(W::I32Add);
        }
        self.emit(W::LocalTee(self.tmp_i32b));
        self.emit(W::I32Const(NULL_LIMIT as i32));
        self.emit(W::I32Sub);
        self.guest_check_value();
        self.emit(W::I32GtU);
        self.emit(W::If(BlockType::Empty));
        self.raise(dirty & FAULT_SYNC, fault::ACCESS_VIOLATION, eip);
        self.emit(W::End);
    }

    /// Pushes the store-map byte for local `a`.
    fn store_map_byte(&mut self, a: u32) {
        self.emit(W::LocalGet(a));
        self.emit(W::I32Const(12));
        self.emit(W::I32ShrU);
        let o = self.native_table(G_STORE_MAP);
        self.emit(W::I32Load8U(memarg(o, 0)));
    }

    /// Invalidates the translations of the page a store hits if it holds
    /// translated code: points the page's lookup entry at the empty second
    /// level and clears its CODE bit, so the next jump there misses and is
    /// translated again. Done inline rather than by calling the host, because
    /// a call on this path, even untaken, makes V8 spill around every store.
    fn check_code_write(&mut self, addr: u32, off: u32) {
        let a = self.tmp_i32b;
        self.emit(W::LocalGet(addr));
        if off != 0 {
            self.emit(W::I32Const(off as i32));
            self.emit(W::I32Add);
        }
        self.emit(W::I32Const(12));
        self.emit(W::I32ShrU);
        self.emit(W::LocalTee(a));
        let o = self.native_table(G_STORE_MAP);
        self.emit(W::I32Load8U(memarg(o, 0)));
        self.emit(W::LocalTee(self.tmp_map));
        self.emit(W::I32Const(store_map::CODE as i32));
        self.emit(W::I32And);
        self.emit(W::If(BlockType::Empty));
        // l1[page] = zero_l2
        self.emit(W::LocalGet(a));
        self.emit(W::I32Const(2));
        self.emit(W::I32Shl);
        let o = self.native_table(G_LOOKUP_L1);
        match self.m.cfg.guest_limit {
            Some(l) => self.emit(W::I32Const((l + native_layout::ZERO_L2) as i32)),
            None => self.emit(W::GlobalGet(G_ZERO_L2)),
        }
        self.emit(W::I32Store(memarg(o, 2)));
        // store_map[page] &= ~CODE
        self.emit(W::LocalGet(a));
        let o = self.native_table(G_STORE_MAP);
        self.emit(W::LocalGet(self.tmp_map));
        self.emit(W::I32Const(!(store_map::CODE as i32)));
        self.emit(W::I32And);
        self.emit(W::I32Store8(memarg(o, 0)));
        self.emit(W::End);
    }

    /// The checks before a plain store: one store-map lookup on the fast
    /// path; pages that need more (code, the null region, the guest limit)
    /// get the precise address check and the code-write notification.
    /// The host hears about a code write before the store happens, which is
    /// equivalent: it only drops translations, which are redone on demand.
    fn check_store(&mut self, addr: u32, off: u32, dirty: StateMask, eip: u32) {
        let (mem, smc) = (self.m.cfg.mem_checks, self.m.cfg.smc_checks);
        if !smc {
            if mem {
                self.check_addr(addr, off, dirty, eip);
            }
            return;
        }
        let a = self.tmp_i32b;
        self.emit(W::LocalGet(addr));
        if off != 0 {
            self.emit(W::I32Const(off as i32));
            self.emit(W::I32Add);
        }
        self.emit(W::LocalSet(a));
        self.store_map_byte(a);
        self.emit(W::If(BlockType::Empty));
        if mem {
            self.check_addr(addr, off, dirty, eip);
        }
        self.check_code_write(addr, off);
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
                let la = self.local_of[*addr as usize];
                if self.needs_check(mem) && !self.check_covered(*addr, mem.offset) {
                    self.check_addr(la, mem.offset, dirty, inst.eip);
                    self.checked(*addr, mem.offset);
                }
                let ty = ty.unwrap();
                if mem.atomic && mem.size > 1 {
                    // Atomics trap when misaligned; fall back to a plain load.
                    self.emit(W::LocalGet(la));
                    if mem.offset != 0 {
                        self.emit(W::I32Const(mem.offset as i32));
                        self.emit(W::I32Add);
                    }
                    self.emit(W::I32Const(mem.size as i32 - 1));
                    self.emit(W::I32And);
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
                let la = self.local_of[*addr as usize];
                if mem.space == Space::Guest {
                    self.check_store(la, mem.offset, dirty, inst.eip);
                    if self.m.cfg.mem_checks {
                        self.checked(*addr, mem.offset);
                    }
                }
                let vty = self.f.ty(*val);
                if mem.atomic && mem.size > 1 {
                    self.emit(W::LocalGet(la));
                    if mem.offset != 0 {
                        self.emit(W::I32Const(mem.offset as i32));
                        self.emit(W::I32Add);
                    }
                    self.emit(W::I32Const(mem.size as i32 - 1));
                    self.emit(W::I32And);
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
            }
            Op::AtomicRmw { op, addr, val, mem } => {
                let la = self.local_of[*addr as usize];
                if self.needs_check(mem) || mem.space == Space::Guest && self.m.cfg.mem_checks {
                    self.check_addr(la, mem.offset, dirty, inst.eip);
                }
                let ty = ty.unwrap_or(Ty::I32);
                // Misaligned: plain load + op + store (not atomic).
                self.emit(W::LocalGet(la));
                self.emit(W::I32Const(mem.size as i32 - 1));
                self.emit(W::I32And);
                self.emit(W::If(BlockType::Result(val_type(ty))));
                {
                    self.emit(W::LocalGet(la));
                    self.emit(load_instr(ty, mem, false));
                    self.emit(W::LocalSet(self.tmp_i32));
                    self.emit(W::LocalGet(la));
                    match op {
                        RmwOp::Xchg => self.get(*val),
                        _ => {
                            self.emit(W::LocalGet(self.tmp_i32));
                            self.get(*val);
                            self.emit(match op {
                                RmwOp::Add => W::I32Add,
                                RmwOp::Sub => W::I32Sub,
                                RmwOp::And => W::I32And,
                                RmwOp::Or => W::I32Or,
                                _ => W::I32Xor,
                            });
                        }
                    }
                    self.emit(store_instr(Ty::I32, mem, false));
                    self.emit(W::LocalGet(self.tmp_i32));
                }
                self.emit(W::Else);
                self.emit(W::LocalGet(la));
                self.get(*val);
                self.emit(rmw_instr(*op, mem));
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
                let la = self.local_of[*addr as usize];
                if mem.space == Space::Guest && self.m.cfg.mem_checks {
                    self.check_addr(la, mem.offset, dirty, inst.eip);
                }
                let ty = ty.unwrap_or(Ty::I32);
                let (tmp, eq, sel) = if ty == Ty::I64 {
                    (self.tmp_i64, W::I64Eq, ())
                } else {
                    (self.tmp_i32, W::I32Eq, ())
                };
                let _ = sel;
                self.emit(W::LocalGet(la));
                self.emit(W::I32Const(mem.size as i32 - 1));
                self.emit(W::I32And);
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
                self.get(*dst);
                self.get(*src);
                self.get(*len);
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
                self.get(*dst);
                self.get(*val);
                self.get(*len);
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
                self.emit(W::LocalSet(self.tmp_i32b));
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
    fn check_range(&mut self, addr: V, len: V, dirty: StateMask, eip: u32) {
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
        self.guest_check_value();
        self.emit(W::I32GtU);
        self.emit(W::I32Or);
        self.emit(W::If(BlockType::Empty));
        self.raise(dirty & FAULT_SYNC, fault::ACCESS_VIOLATION, eip);
        self.emit(W::End);
    }

    /// Notifies the host for each code page in [addr, addr+len).
    fn check_code_write_range(&mut self, addr: V, len: V) {
        // for (p = addr & ~0xfff; p < addr + len; p += 0x1000) check(p)
        let p = self.tmp_i32;
        let end = self.label_local;
        let _ = end;
        self.get(addr);
        self.emit(W::I32Const(!0xfff));
        self.emit(W::I32And);
        self.emit(W::LocalSet(p));
        self.emit(W::Block(BlockType::Empty));
        self.emit(W::Loop(BlockType::Empty));
        self.emit(W::LocalGet(p));
        self.get(addr);
        self.get(len);
        self.emit(W::I32Add);
        self.emit(W::I32GeU);
        self.emit(W::BrIf(1));
        self.check_code_write(p, 0);
        self.emit(W::LocalGet(p));
        self.emit(W::I32Const(0x1000));
        self.emit(W::I32Add);
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
        (Ty::F64, 8, _) => W::F64Store(ma),
        (Ty::F32, 4, _) => W::F32Store(ma),
        (Ty::V128, 16, _) => W::V128Store(ma),
        other => panic!("unsupported store {other:?}"),
    }
}

fn rmw_instr(op: RmwOp, mem: &Mem) -> W<'static> {
    let size = mem.size as u32;
    let ma = memarg(mem.offset, size.trailing_zeros());
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

fn gen_helper(h: Helper) -> wasm_encoder::Function {
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
            out.push(W::Call(NUM_FUNC_IMPORTS)); // Eflags is helper 0
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
