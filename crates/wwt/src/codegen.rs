//! Layer 6 — Generate WebAssembly.
//!
//! One WebAssembly function per x86 function, with type `(cpu: i32) -> i32`:
//! the result is the x86 address to continue at. With 64-bit memory
//! ([`CodegenConfig::mem64`]) the CPU pointer, native addresses and the
//! runtime-table globals are i64, and x86-64 code addresses are i64 too
//! (`(cpu) -> i64`, see [`ModuleGen::code64`]). Branches and loops are
//! rebuilt as structured control flow from the dominator tree (Ramsey,
//! "Beyond Relooper", 2022), after [`reducible`] gives every loop a single
//! entry; a graph that is still irreducible falls back to a dispatch loop.
//! Calls between functions in the module are direct; everything else goes
//! through the two-level address lookup into the shared function table.

use std::borrow::Cow;
use std::collections::HashMap;

use wasm_encoder::{
    BlockType, CodeSection, ConstExpr, CustomSection, ElementSection, Elements, EntityType,
    ExportKind, ExportSection, FunctionSection, GlobalType, ImportSection, Instruction as W,
    MemArg, MemoryType, Module, NameMap, NameSection, RefType, TableType, TypeSection, ValType,
};

use crate::abi::{self, addr::NULL_LIMIT, cpu, cpu64, fault, imports, native_layout, store_map};
use crate::discover::CodeSource;
use crate::flags::{self, E};
use crate::ir::*;
use crate::opt::{self, Analysis, StateMask, ALL_STATE, FAULT_SYNC};
use crate::reducible;

/// How many of the most recent facts about one vreg `check_covered` looks
/// at.
const MAX_FACTS: usize = 64;

/// Whether an operation writes guest memory that may hold code, so its
/// translation checks the store map for it (`FnGen::check_code_write`).
fn may_write_code(op: &Op) -> bool {
    match op {
        Op::Store { mem, .. } | Op::AtomicRmw { mem, .. } | Op::AtomicCmpxchg { mem, .. } => mem.space == Space::Guest,
        Op::MemCopy { .. } | Op::MemFill { .. } => true,
        _ => false,
    }
}

/// Values by vreg in the order they were added, which can be undone back to
/// an earlier length: facts and aliases scoped to the dominator subtree
/// being emitted (`FnGen::do_tree`).
#[derive(Default)]
struct Scoped<T> {
    map: HashMap<V, Vec<T>>,
    log: Vec<V>,
}

impl<T> Scoped<T> {
    fn push(&mut self, v: V, x: T) {
        self.map.entry(v).or_default().push(x);
        self.log.push(v);
    }

    /// The values about `v`, oldest first.
    fn get(&self, v: V) -> &[T] {
        self.map.get(&v).map_or(&[], |x| &x[..])
    }

    fn len(&self) -> usize {
        self.log.len()
    }

    /// Removes the values added after the first `len`.
    fn truncate(&mut self, len: usize) {
        while self.log.len() > len {
            let v = self.log.pop().unwrap();
            if let Some(x) = self.map.get_mut(&v) {
                x.pop();
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct CodegenConfig {
    pub mem_checks: bool,
    /// Fast mode: plain guest loads and stores have no explicit check and
    /// rely on the engine's bounds trap instead (see `Fun::trap_access`).
    /// The runtime turns the trap into an access violation at the right
    /// instruction, with the registers as last written back.
    pub mem_traps: bool,
    pub smc_checks: bool,
    /// Import a 64-bit (memory64) memory. Guest addresses narrower than the
    /// memory are zero-extended; with a 32-bit memory, x86-64 addresses are
    /// checked against the guest limit and wrapped.
    pub mem64: bool,
    /// The guest limit the module will run under, when the host knows it at
    /// translation time: checks then compare against a constant, which
    /// frees a register in V8's code. `None` reads the `guest_limit` import,
    /// so the module runs under any limit. Recorded in the module metadata;
    /// the runtime refuses a module made for another limit.
    pub guest_limit: Option<u32>,
    /// Let long-running loops re-enter their function (see `crate::osr`).
    pub osr: bool,
    /// Lower `lock`-prefixed read-modify-writes and `cmpxchg` to WebAssembly
    /// atomics. Without, they are a plain load and store: equivalent while
    /// guest threads never run in parallel (the runtimes run one at a time,
    /// and never in the middle of an instruction), and far cheaper than
    /// sequentially consistent atomics on shared memory.
    pub atomics: bool,
}

impl Default for CodegenConfig {
    fn default() -> Self {
        CodegenConfig {
            mem_checks: true,
            mem_traps: false,
            smc_checks: true,
            mem64: false,
            guest_limit: None,
            osr: true,
            atomics: false,
        }
    }
}

/// Largest memory64 size browsers allow (16 GB) in 64 KB pages.
pub const MAX_PAGES_64: u64 = 1 << 18;

// Type indices.
const T_FN: u32 = 0;
const T_FAULT: u32 = 1;
const T_MATH: u32 = 2;
const T_F64_F64: u32 = 3;
const T_PREEMPT: u32 = 4;
const T_FIRST_HELPER: u32 = 5;

// Imported function indices. Native implementations (`Term::Native`,
// `Term::NativeTry`) follow, one import each, in the order of
// `ModuleGen::natives`.
const F_FAULT: u32 = 0;
const F_MATH: u32 = 1;
/// Math.sin and Math.cos themselves, which engines call without going
/// through JavaScript (the other operations go through `math`).
const F_SIN: u32 = 2;
const F_COS: u32 = 3;
const F_PREEMPT: u32 = 4;
const F_FIRST_NATIVE: u32 = 5;

// Imported global indices.
const G_TABLE_BASE: u32 = 0;
const G_LOOKUP_L1: u32 = 1;
const G_GUEST_CHECK: u32 = 2;
const G_STORE_MAP: u32 = 3;
const G_ZERO_L2: u32 = 4;
const G_TICK: u32 = 5;
/// 64-bit code only.
const G_CODE_PAGES: u32 = 6;

/// Translated calls nested in WebAssembly calls before one returns to the
/// dispatch loop instead (`call_depth_enter`). Browsers give a worker
/// about 1 MB of stack; translated frames take up to a few hundred bytes.
const MAX_CALL_DEPTH: u32 = 1000;

/// How far (bytes) a load may be from an address that passed a check and
/// still skip its own (see `check_covered`).
const CHECK_WINDOW: u32 = 0x8000 - 16;
/// Largest index range `root + [0, r]` tracked for check elimination.
const MAX_INDEX: u32 = CHECK_WINDOW / 2;

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
    /// Names of the native implementations the functions leave through or
    /// try, with the number of constant arguments they take after the CPU
    /// state.
    natives: Vec<(&'static str, usize)>,
    /// Symbol names by x86 address, for the name section.
    names: HashMap<u64, String>,
    /// The x86 code, to describe trapping accesses' memory operands in
    /// `wwt.traps`.
    code: Option<&'a dyn CodeSource>,
}

impl<'a> ModuleGen<'a> {
    pub fn new(cfg: &'a CodegenConfig, funcs: &[Function]) -> ModuleGen<'a> {
        let mut func_index = HashMap::new();
        for (i, f) in funcs.iter().enumerate() {
            func_index.insert(f.entry, i as u32);
        }
        // The flag helpers come first, in every module.
        let mut helpers = vec![Helper::Eflags, Helper::EvalCond];
        let mut natives = vec![];
        for f in funcs {
            for b in &f.blocks {
                let native = match &b.term {
                    Term::Native(n) => Some((*n, 0)),
                    Term::NativeTry { name, args, .. } => Some((*name, args.len())),
                    _ => None,
                };
                if let Some(n) = native {
                    if !natives.contains(&n) {
                        natives.push(n);
                    }
                }
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
            natives,
            names: HashMap::new(),
            code: None,
        }
    }

    /// The x86 code the functions come from (see `ModuleGen::code`).
    pub fn with_code(mut self, code: &'a dyn CodeSource) -> Self {
        self.code = Some(code);
        self
    }

    /// Sets whether code addresses are 64-bit (by default, whether the
    /// first function is x86-64 code on a 64-bit memory).
    pub fn with_code64(mut self, code64: bool) -> Self {
        self.code64 = code64;
        self
    }

    /// Labels translated functions with these symbols in the name section
    /// (functions without one are named by address).
    pub fn with_names(mut self, names: &HashMap<u64, String>) -> Self {
        self.names = names.clone();
        self
    }

    /// Disables direct calls between the module's functions (for modules
    /// whose functions share entry addresses, like instruction tests).
    pub fn without_direct_calls(mut self) -> Self {
        self.func_index.clear();
        self
    }

    fn num_func_imports(&self) -> u32 {
        F_FIRST_NATIVE + self.natives.len() as u32
    }

    fn native_func_index(&self, name: &str) -> u32 {
        F_FIRST_NATIVE + self.natives.iter().position(|n| n.0 == name).unwrap() as u32
    }

    fn helper_func_index(&self, h: Helper) -> u32 {
        self.num_func_imports() + self.helpers.iter().position(|x| *x == h).unwrap() as u32
    }

    fn translated_func_index(&self, i: u32) -> u32 {
        self.num_func_imports() + self.helpers.len() as u32 + i
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
        // fault(cpu, code, eip, info) -> where to continue
        types.ty().function([at, ValType::I32, ct, ct], [ct]);
        types
            .ty()
            .function([ValType::I32, ValType::F64, ValType::F64], [ValType::F64]);
        types.ty().function([ValType::F64], [ValType::F64]);
        // preempt(cpu, eip) -> yield?
        types.ty().function([at, ct], [ValType::I32]);
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
        // Natives with constant arguments: (cpu, args...) -> next eip.
        let mut native_type_idx = vec![];
        for &(_, nargs) in &self.natives {
            if nargs == 0 {
                native_type_idx.push(T_FN);
                continue;
            }
            let key = (vec![Ty::I32; 1 + nargs], Ty::I32);
            let idx = match self.helper_types.iter().position(|k| *k == key) {
                Some(i) => i,
                None => {
                    types
                        .ty()
                        .function(vec![ValType::I32; 1 + nargs], [ValType::I32]);
                    self.helper_types.push(key);
                    self.helper_types.len() - 1
                }
            };
            native_type_idx.push(T_FIRST_HELPER + idx as u32);
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
            imports::SIN,
            EntityType::Function(T_F64_F64),
        );
        imp.import(
            imports::MODULE,
            imports::COS,
            EntityType::Function(T_F64_F64),
        );
        imp.import(
            imports::MODULE,
            imports::PREEMPT,
            EntityType::Function(T_PREEMPT),
        );
        // Native implementations follow (F_FIRST_NATIVE on).
        for (&(n, _), &t) in self.natives.iter().zip(&native_type_idx) {
            imp.import(imports::MODULE, n, EntityType::Function(t));
        }
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
            imports::STORE_MAP,
            imports::ZERO_L2,
            imports::TICK,
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
        // The lazy-flags evaluator, for the host to build CONTEXT records.
        let mut exports = ExportSection::new();
        exports.export(
            abi::EFLAGS_EXPORT,
            ExportKind::Func,
            self.helper_func_index(Helper::Eflags),
        );
        module.section(&exports);

        module.section(&elems);

        // Code.
        let mut code = CodeSection::new();
        let eflags = self.helper_func_index(Helper::Eflags);
        for h in self.helpers.clone() {
            let eflags = match h {
                Helper::EvalCond64 => self.helper_func_index(Helper::Eflags64),
                _ => eflags,
            };
            code.function(&gen_helper(h, eflags));
        }
        let mut residue = Vec::with_capacity(funcs.len());
        // Trapping accesses, at first by their offset in the code section's
        // entries.
        let mut traps = Vec::new();
        for f in funcs {
            let (body, r, t) = self.gen_function_full(f);
            let start = code.byte_len() + leb_len(body.byte_len());
            traps.extend(t.into_iter().map(|t| TrapSite {
                at: start as u32 + t.at,
                ..t
            }));
            code.function(&body);
            residue.push(r);
        }
        // The entries start after the section's id, size and count.
        let count = (self.helpers.len() + funcs.len()) as u32;
        let entries = module.len()
            + 1
            + leb_len(leb_len(count as usize) + code.byte_len())
            + leb_len(count as usize);
        module.section(&code);

        // Names, for profilers and debuggers: `symbol@address` or
        // `x86_address`.
        let mut fnames = NameMap::new();
        for (i, h) in self.helpers.iter().enumerate() {
            fnames.append(self.num_func_imports() + i as u32, &format!("helper_{h:?}"));
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
        // Per function, in table order (see `Residue`).
        module.section(&CustomSection {
            name: Cow::Borrowed(abi::RESIDUE_SECTION),
            data: Cow::Owned(serde_json::to_vec(&residue).unwrap()),
        });
        if !traps.is_empty() {
            let mut data = Vec::with_capacity(4 + traps.len() * 16);
            data.extend_from_slice(&(traps.len() as u32).to_le_bytes());
            for t in &traps {
                let (operand, disp) = match self.code.and_then(|c| mem_operand(c, t.eip)) {
                    Some((o, d)) => (abi::trap::OPERAND | o, d),
                    None => (0, 0),
                };
                let flags = operand | if t.write { abi::trap::WRITE } else { 0 };
                data.extend_from_slice(&(entries as u32 + t.at).to_le_bytes());
                data.extend_from_slice(&(t.eip as u32).to_le_bytes());
                data.extend_from_slice(&flags.to_le_bytes());
                data.extend_from_slice(&disp.to_le_bytes());
            }
            module.section(&CustomSection {
                name: Cow::Borrowed(abi::TRAPS_SECTION),
                data: Cow::Owned(data),
            });
        }
        module.finish()
    }

    pub fn gen_function(&self, f: &Function) -> wasm_encoder::Function {
        self.gen_function_counted(f).0
    }

    /// The function and the emulation left in it.
    pub fn gen_function_counted(&self, f: &Function) -> (wasm_encoder::Function, Residue) {
        let (body, r, _) = self.gen_function_full(f);
        (body, r)
    }

    /// The function, the emulation left in it and its trapping accesses.
    fn gen_function_full(&self, f: &Function) -> (wasm_encoder::Function, Residue, Vec<TrapSite>) {
        let fixed;
        // Blocks from here on are made by the passes below, not lifted.
        let lifted = f.blocks.len();
        // Where the optimizer kept the state a preemption check writes back.
        let preempt = opt::preempt_points(f);
        let irreducible = !reducible::is_reducible(f);
        let f = if reducible::is_reducible(f) && !self.cfg.osr {
            f
        } else {
            let mut c = f.clone();
            if !reducible::is_reducible(&c) {
                reducible::merge_switches(&mut c);
            }
            reducible::make_reducible(&mut c);
            if self.cfg.osr {
                crate::osr::add_reentry(&mut c);
            }
            fixed = c;
            &fixed
        };
        let a = opt::analyze(f);
        let mut g = FnGen::new(self, f, &a);
        g.lifted = lifted;
        g.preempt = preempt;
        g.irreducible = irreducible;
        g.run();
        let r = g.residue;
        let (body, traps) = g.finish();
        (body, r, traps)
    }
}

/// A guest access that relies on the engine's bounds trap (fast mode):
/// where its load or store instruction starts (from the start of the
/// function's body, then of the module) and the x86 instruction it belongs
/// to.
pub struct TrapSite {
    pub at: u32,
    pub eip: u64,
    pub write: bool,
}

/// The memory operand of the 32-bit instruction at `eip`, for `wwt.traps`
/// (see `abi::trap`): its registers, scale and segment, and displacement.
fn mem_operand(code: &dyn CodeSource, eip: u64) -> Option<(u32, u32)> {
    use iced_x86::{Decoder, DecoderOptions, OpKind, Register};
    let i = Decoder::with_ip(32, code.bytes(eip), eip, DecoderOptions::NONE).decode();
    if i.is_invalid() || !(0..i.op_count()).any(|k| i.op_kind(k) == OpKind::Memory) {
        return None;
    }
    let reg = |r: Register| match r {
        Register::None => Some(0),
        // 16-bit addressing wraps at 64 KB: not described.
        r if r.is_gpr32() => Some(r.number() as u32 + 1),
        _ => None,
    };
    let seg = match i.memory_segment() {
        Register::FS => 1,
        Register::GS => 2,
        _ => 0,
    };
    let scale = i.memory_index_scale().trailing_zeros();
    let packed = reg(i.memory_base())? << abi::trap::BASE_SHIFT
        | reg(i.memory_index())? << abi::trap::INDEX_SHIFT
        | scale << abi::trap::SCALE_SHIFT
        | seg << abi::trap::SEG_SHIFT;
    Some((packed, i.memory_displacement32()))
}

/// The length of `n` as an unsigned LEB128.
fn leb_len(n: usize) -> usize {
    (usize::BITS - (n | 1).leading_zeros()).div_ceil(7) as usize
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

/// Emulation left in a translated function, counted as emitted (outside
/// the shared fault blocks, which run only on a fault): what an optimizer
/// would remove, listed per function in the `wwt.residue` section.
#[derive(Default, Clone, Copy, Debug, serde::Serialize)]
pub struct Residue {
    /// x86 registers written back to the CPU state (at calls, exits, ...).
    pub state_stores: u32,
    /// ... of which lazy-flag state.
    pub flag_stores: u32,
    /// x86 registers loaded from the CPU state (at entry, after calls).
    pub state_loads: u32,
    /// Guest-limit checks on memory accesses.
    pub checks: u32,
    /// Accesses relying on the engine's bounds trap instead (fast mode).
    #[serde(skip_serializing_if = "is_zero")]
    pub traps: u32,
    /// Store-map lookups (taken while code is writable).
    pub store_maps: u32,
    /// Calls and jumps through the address lookup (indirect, or to code
    /// outside the module).
    pub lookups: u32,
    /// Direct calls between translated functions.
    pub calls: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

struct FnGen<'g, 'a> {
    m: &'g ModuleGen<'a>,
    residue: Residue,
    /// Trapping accesses (fast mode): the index in `out` of the load or
    /// store, its x86 address and whether it writes.
    trap_sites: Vec<(usize, u64, bool)>,
    /// Emitting a fault block: not counted in `residue`.
    in_fault: bool,
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
    /// The thread's call depth before a call (see `call_depth_enter`).
    depth_local: u32,
    /// Labels open at the current point of the body (blocks, loops, ifs).
    open_labels: u32,
    /// Shared fault blocks: (state mask, fault code), in creation order.
    fault_blocks: Vec<(StateMask, u32)>,
    /// The faulting instruction's address, for the shared fault blocks.
    fault_eip: u32,
    /// Uses of each vreg in the function (for stackifying).
    use_count: Vec<u32>,
    /// Stackified values not emitted yet: vreg -> (instruction, dirty mask).
    /// They are emitted where their one use reads them (see `stackify`).
    pending: Vec<(V, Inst, StateMask)>,
    /// Scratch for the store-map byte in code-write checks.
    tmp_map: u32,
    /// Set (to 1) by a store that hit a page with translated code
    /// (`check_code_write`): the rest of this function was translated from
    /// the bytes before that store, so it returns to the dispatcher at the
    /// next instruction boundary, which then translates the bytes as they
    /// are now (`smc_exit`). x86 runs the instructions it just wrote, also
    /// the next ones in line; protectors and packers decrypt code that way.
    /// UNASSIGNED in functions without stores that check for code writes.
    smc_local: u32,
    /// Whether the instruction being emitted checked a store for code writes.
    smc_checked: bool,
    /// Blocks entered from a block whose last instruction may write code:
    /// they check `smc_local` on entry.
    smc_entry: Vec<bool>,
    tmp_i32b: u32,
    tmp_i64: u32,
    tmp_i64b: u32,
    /// Scratch address in the memory's address type.
    tmp_ma: u32,
    label_local: u32,
    /// Blocks numbered from here on were added by code generation's own
    /// passes (`reducible`, `osr`), not lifted from x86 code.
    lifted: usize,
    /// `opt::preempt_points` of the lifted function, and whether it was
    /// irreducible: where its back edges' state survived the optimizer.
    preempt: Vec<bool>,
    irreducible: bool,
    emitted: Vec<bool>,
    /// Addresses whose guest-limit check passed, as (vreg, lo, hi): some
    /// address in `vreg + [lo, hi]` passed (lo = hi: that one did). Loads
    /// near them skip the check (see `check_covered`). `facts_local` holds
    /// any vreg and lasts until the end of the block or the vreg's next
    /// definition; `facts_tree` holds single-definition temporaries and is
    /// scoped to the dominator subtree being emitted.
    /// Both are keyed by the vreg, so a lookup costs the facts about that
    /// vreg only (obfuscated code makes functions of tens of thousands of
    /// accesses, which a list scanned per access made quadratic).
    facts_local: HashMap<V, Vec<(u32, u32)>>,
    facts_tree: Scoped<(u32, u32)>,
    /// Vregs with one definition, in a block that dominates their uses.
    def_block: Vec<u32>,
    structured: bool,
    cur_block: BlockId,
    /// Within the current block, temporaries known to equal `root + off +
    /// x` for some x in [0, r] (r = 0: exactly `root + off`), as
    /// (temporary, root, off, r).
    alias: HashMap<V, (V, u32, u32)>,
    /// The temporaries in `alias` by their root, to forget them when the
    /// root is redefined.
    alias_by_root: HashMap<V, Vec<V>>,
    /// The same, between single-definition temporaries, scoped like
    /// `facts_tree`.
    alias_tree: Scoped<(V, u32, u32)>,
    /// Constant value of single-definition temporaries defined by a constant.
    const_of: Vec<Option<u32>>,
    /// Upper bound of single-definition temporaries' values, where known
    /// (masks, shifts, zero-extending loads): u32::MAX otherwise.
    max_of: Vec<u32>,
}

const UNASSIGNED: u32 = u32::MAX;

impl<'g, 'a> FnGen<'g, 'a> {
    fn new(m: &'g ModuleGen<'a>, f: &'g Function, a: &'g Analysis) -> Self {
        let n = f.blocks.len();
        let mut g = FnGen {
            m,
            residue: Residue::default(),
            trap_sites: Vec::new(),
            in_fault: false,
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
            depth_local: 0,
            open_labels: 0,
            fault_blocks: vec![],
            fault_eip: 0,
            use_count: vec![],
            pending: vec![],
            tmp_map: 0,
            smc_local: UNASSIGNED,
            smc_checked: false,
            smc_entry: vec![],
            tmp_i32b: 0,
            tmp_i64: 0,
            tmp_i64b: 0,
            tmp_ma: 0,
            label_local: 0,
            lifted: n,
            preempt: vec![],
            irreducible: false,
            emitted: vec![false; n],
            facts_local: HashMap::new(),
            facts_tree: Scoped::default(),
            def_block: vec![],
            structured: true,
            cur_block: 0,
            alias: HashMap::new(),
            alias_by_root: HashMap::new(),
            alias_tree: Scoped::default(),
            const_of: vec![],
            max_of: vec![],
        };
        g.local_of[CPU as usize] = 0;
        g.assign_locals();
        g.tmp_i32 = g.new_local(ValType::I32);
        if f.blocks.iter().any(|b| b.term.clobbers_state()) {
            g.depth_local = g.new_local(ValType::I32);
        }
        g.fault_eip = g.new_local(m.code_type());
        g.tmp_map = g.new_local(ValType::I32);
        if m.cfg.smc_checks && f.blocks.iter().any(|b| b.insts.iter().any(|i| may_write_code(&i.op))) {
            g.smc_local = g.new_local(ValType::I32);
            g.smc_entry = vec![false; f.blocks.len()];
            for b in &f.blocks {
                // The last instruction's IR: those with the last eip.
                let last = b.insts.last().map(|i| i.eip);
                if b.insts.iter().rev().take_while(|i| Some(i.eip) == last).any(|i| may_write_code(&i.op)) {
                    for s in b.term.successors() {
                        g.smc_entry[s as usize] = true;
                    }
                }
            }
        }
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

    fn finish(self) -> (wasm_encoder::Function, Vec<TrapSite>) {
        let mut func =
            wasm_encoder::Function::new_with_locals_types(self.local_types.iter().copied());
        let mut traps = Vec::with_capacity(self.trap_sites.len());
        let mut sites = self.trap_sites.iter().peekable();
        for (k, i) in self.out.iter().enumerate() {
            while let Some(&(_, eip, write)) = sites.next_if(|s| s.0 == k) {
                let at = func.byte_len() as u32;
                traps.push(TrapSite { at, eip, write });
            }
            func.instruction(i);
        }
        func.instruction(&W::End);
        (func, traps)
    }

    fn emit(&mut self, i: W<'static>) {
        match i {
            W::Block(_) | W::Loop(_) | W::If(_) => self.open_labels += 1,
            W::End => self.open_labels -= 1,
            _ => {}
        }
        self.out.push(i);
    }

    fn get(&mut self, v: V) {
        if let Some(i) = self.pending.iter().position(|&(p, _, _)| p == v) {
            let (_, inst, dirty) = self.pending.remove(i);
            self.inst_value(&inst, dirty, true);
            return;
        }
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
            if !self.in_fault {
                self.residue.state_stores += 1;
                if FLAG_STATE.contains(&v) {
                    self.residue.flag_stores += 1;
                }
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
            if !self.in_fault {
                self.residue.state_loads += 1;
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

    /// The local holding the fault information for `raise`: `tmp_i32b`, or
    /// `tmp_i64b` for 64-bit code.
    fn fault_info(&self) -> u32 {
        if self.code64() {
            self.tmp_i64b
        } else {
            self.tmp_i32b
        }
    }

    /// Sets the fault information from the value of type `t` on the stack.
    fn set_fault_info(&mut self, t: Ty) {
        if self.code64() {
            if t == Ty::I32 {
                self.emit(W::I64ExtendI32U);
            }
        } else if t == Ty::I64 {
            self.emit(W::I32WrapI64);
        }
        self.emit(W::LocalSet(self.fault_info()));
    }

    /// Raises a fault: branches to the function's shared fault block for
    /// this state mask and code, which writes back `mask`, records eip and
    /// calls the host. Sharing these blocks (one per distinct mask and code,
    /// instead of a copy at every check) makes modules 10-15% smaller,
    /// which shortens compilation. The fault blocks wrap the body
    /// (see `run`); block k is `open_labels + k` labels out. The
    /// address/info value must already be in `fault_info()`.
    fn raise(&mut self, mask: StateMask, code: u32, eip: u64) {
        let k = match self
            .fault_blocks
            .iter()
            .position(|&(m, c)| m == mask && c == code)
        {
            Some(k) => k,
            None => {
                self.fault_blocks.push((mask, code));
                self.fault_blocks.len() - 1
            }
        } as u32;
        self.code_const(eip);
        self.emit(W::LocalSet(self.fault_eip));
        self.emit(W::Br(self.open_labels + k));
    }

    /// The code of a shared fault block: writes back `mask`, records eip
    /// and calls the host, which returns where to continue (a Windows
    /// exception dispatcher, with the guest stack set up for it).
    fn fault_block(&mut self, mask: StateMask, code: u32) {
        self.in_fault = true;
        self.sync(mask);
        self.emit(W::LocalGet(0));
        self.emit(W::LocalGet(self.fault_eip));
        self.store_eip();
        self.emit(W::LocalGet(0));
        self.emit(W::I32Const(code as i32));
        self.emit(W::LocalGet(self.fault_eip));
        self.emit(W::LocalGet(self.fault_info()));
        self.emit(W::Call(F_FAULT));
        self.emit(W::Return);
    }

    // ---- Lookup --------------------------------------------------------------

    /// With the value on the stack as an index into one of the runtime's
    /// tables (`global` names its base), returns the memory offset to
    /// access it with: the table's constant address when the guest limit is
    /// known at translation time, else 0 after adding the imported base.
    fn native_table(&mut self, global: u32) -> u32 {
        match self.m.cfg.guest_limit {
            Some(l) if !self.mem64() => {
                let off = match global {
                    G_LOOKUP_L1 => native_layout::LOOKUP_L1,
                    G_ZERO_L2 => native_layout::ZERO_L2,
                    _ => native_layout::STORE_MAP,
                };
                l + off
            }
            _ => {
                self.emit(W::GlobalGet(global));
                self.emit(if self.mem64() { W::I64Add } else { W::I32Add });
                0
            }
        }
    }

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
        if self.mem64() {
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
    /// Before a call that nests a WebAssembly call: past `MAX_CALL_DEPTH`
    /// nested calls, leaves with the callee's address instead, so the
    /// frames unwind to the dispatch loop, which calls it from there (the
    /// state is written back and the return address is on the guest
    /// stack). Deep guest recursion then fits the WebAssembly stack.
    fn call_depth_enter(&mut self, target: CallTarget) {
        let d = self.depth_local;
        self.emit(W::LocalGet(0));
        self.emit(W::I32Load(memarg(cpu::CALL_DEPTH, 2)));
        self.emit(W::LocalTee(d));
        self.emit(W::I32Const(MAX_CALL_DEPTH as i32));
        self.emit(W::I32GeU);
        self.emit(W::If(BlockType::Empty));
        match target {
            CallTarget::Direct(addr) => self.code_const(addr),
            CallTarget::Indirect(v) => self.get_code(v),
        }
        self.emit(W::Return);
        self.emit(W::End);
        self.emit(W::LocalGet(0));
        self.emit(W::LocalGet(d));
        self.emit(W::I32Const(1));
        self.emit(W::I32Add);
        self.emit(W::I32Store(memarg(cpu::CALL_DEPTH, 2)));
    }

    /// After the call: the depth from before it (also right when frames
    /// below unwound without restoring theirs).
    fn call_depth_leave(&mut self) {
        self.emit(W::LocalGet(0));
        self.emit(W::LocalGet(self.depth_local));
        self.emit(W::I32Store(memarg(cpu::CALL_DEPTH, 2)));
    }

    fn call_lookup(&mut self, t: u32, tail: bool) {
        self.residue.lookups += 1;
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
        self.use_count = vec![0; f.vtypes.len()];
        for b in &f.blocks {
            for inst in &b.insts {
                for u in inst.op.uses() {
                    self.use_count[u as usize] += 1;
                }
            }
            for u in b.term.uses() {
                self.use_count[u as usize] += 1;
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
        self.compute_max_of(&order);
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
        // Wrap the body in one block per fault block, innermost first, with
        // each fault block's code after its block's end.
        let body = std::mem::take(&mut self.out);
        let n = self.fault_blocks.len();
        for s in &mut self.trap_sites {
            s.0 += n;
        }
        self.out = Vec::with_capacity(body.len() + n * 32);
        for _ in 0..n {
            self.out.push(W::Block(BlockType::Empty));
        }
        self.out.extend(body);
        for (mask, code) in self.fault_blocks.clone() {
            self.out.push(W::End);
            self.fault_block(mask, code);
        }
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
        let scope = (self.facts_tree.len(), self.alias_tree.len());
        self.do_tree_inner(x);
        self.facts_tree.truncate(scope.0);
        self.alias_tree.truncate(scope.1);
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

    /// At a loop's back edge: once the thread's slice deadline has passed,
    /// writes the state back and asks the host whether to switch threads;
    /// if so, the loop's frames return the yield address and the thread
    /// resumes at the loop header later. Two loads and a compare otherwise.
    fn preempt_check(&mut self, src: BlockId, tgt: BlockId) {
        let Some(resume) = self.resume_addr(src, tgt) else {
            return;
        };
        self.emit(W::GlobalGet(G_TICK));
        self.emit(W::I32Load(memarg(0, 2)));
        self.emit(W::LocalGet(0));
        self.emit(W::I32Load(memarg(cpu::PREEMPT_AT, 2)));
        self.emit(W::I32GeU);
        self.emit(W::If(BlockType::Empty));
        self.ctx.push(Ctx::If);
        self.sync(self.a.dirty_end[src as usize]);
        self.emit(W::LocalGet(0));
        self.code_const(resume);
        self.emit(W::Call(F_PREEMPT));
        self.emit(W::If(BlockType::Empty));
        self.code_const(if self.code64() {
            abi::addr::YIELD64
        } else {
            abi::addr::YIELD as u64
        });
        self.emit(W::Return);
        self.emit(W::End);
        self.ctx.pop();
        self.emit(W::End);
    }

    /// The x86 address a back edge continues at, where a preempted thread
    /// resumes. A block made by restructuring has none of its own: a
    /// multi-entry loop's dispatch header carries its first entry's address
    /// whichever entry the label selects, and a merged jump table's block
    /// that of one of the table jumps. An edge into one resumes at the entry
    /// it was meant for when that is known (the label setter in front of a
    /// dispatch header carries its entry's address); otherwise it gets no
    /// check.
    ///
    /// The edge must also leave a block whose dirty state the optimizer kept
    /// for it (`opt::preempt_points`): a lifted block it marked, or a label
    /// setter, whose predecessors' edges into the loop entry it marked.
    fn resume_addr(&self, src: BlockId, tgt: BlockId) -> Option<u64> {
        let s = &self.f.blocks[src as usize];
        if (src as usize) < self.lifted {
            if !self.preempt.get(src as usize).copied().unwrap_or(false)
                || tgt as usize >= self.lifted
            {
                return None;
            }
            return Some(self.f.blocks[tgt as usize].addr);
        }
        let setter = self.irreducible
            && matches!(s.term, Term::Jump(t) if t == tgt)
            && matches!(
                s.insts.as_slice(),
                [Inst {
                    dst: Some(_),
                    op: Op::Const(_),
                    ..
                }]
            );
        setter.then_some(s.addr)
    }

    fn do_branch(&mut self, src: BlockId, tgt: BlockId) {
        if self.ctx.contains(&Ctx::Dispatch) {
            if self.rpo_num[tgt as usize] <= self.rpo_num[src as usize] {
                self.preempt_check(src, tgt);
            }
            self.emit(W::I32Const(tgt as i32));
            self.emit(W::LocalSet(self.label_local));
            let l = self.label(Ctx::Dispatch);
            self.emit(W::Br(l));
            return;
        }
        if self.rpo_num[tgt as usize] <= self.rpo_num[src as usize] {
            self.preempt_check(src, tgt);
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
        let dirty = self.a.dirty_end[x as usize] & opt::term_sync(self.f, x);
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
                self.call_depth_enter(target.clone());
                match target {
                    CallTarget::Direct(addr) => match self.m.direct_target(addr) {
                        Some(fi) => {
                            self.residue.calls += 1;
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
                self.emit(W::LocalSet(t));
                self.call_depth_leave();
                self.emit(W::LocalGet(t));
                self.code_const(ret);
                self.emit(if self.code64() { W::I64Ne } else { W::I32Ne });
                self.emit(W::If(BlockType::Empty));
                self.emit(W::LocalGet(t));
                self.emit(W::Return);
                self.emit(W::End);
                let live = self.a.state_live_in(cont) & ALL_STATE & !opt::call_preserved(self.f);
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
            Term::Native(n) => {
                self.sync(dirty);
                self.emit(W::LocalGet(0));
                self.emit(W::ReturnCall(self.m.native_func_index(n)));
            }
            Term::NativeTry {
                name,
                args,
                fallback,
            } => {
                // Leave with the next address, or continue in the translated
                // body when the native declined (0): it changed nothing, so
                // the state in locals is still current.
                self.sync(dirty);
                self.emit(W::LocalGet(0));
                for a in args {
                    self.emit(W::I32Const(a as i32));
                }
                self.emit(W::Call(self.m.native_func_index(name)));
                self.emit(W::LocalTee(self.tmp_i32));
                self.emit(W::If(BlockType::Empty));
                self.emit(W::LocalGet(self.tmp_i32));
                self.emit(W::Return);
                self.emit(W::End);
                self.do_branch(x, fallback);
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
        self.alias_by_root.clear();
        self.cur_block = b;
        let deferred = self.stackify(blk);
        // Entered after a block whose last instruction may have written code
        // (a lifted block has the address it starts at).
        if self.smc_entry.get(b as usize).copied().unwrap_or(false) && (b as usize) < self.lifted {
            self.smc_exit(self.a.dirty_in[b as usize], blk.addr);
        }
        let mut smc_pending = false;
        let mut prev_eip = None;
        for (k, inst) in blk.insts.iter().enumerate() {
            // The first instruction boundary after one that may have
            // written code.
            if smc_pending && prev_eip != Some(inst.eip) {
                self.smc_exit(trace[k], inst.eip);
                smc_pending = false;
            }
            prev_eip = Some(inst.eip);
            self.smc_checked = false;
            if deferred[k] {
                self.pending
                    .push((inst.dst.unwrap(), inst.clone(), trace[k]));
            } else {
                self.inst(inst, trace[k]);
            }
            smc_pending |= self.smc_checked;
            if let Some(d) = inst.dst {
                // Facts and aliases about the old value of `d` are void.
                self.facts_local.remove(&d);
                if let Some((root, _, _)) = self.alias.remove(&d) {
                    if let Some(ts) = self.alias_by_root.get_mut(&root) {
                        ts.retain(|&t| t != d);
                    }
                }
                for t in self.alias_by_root.remove(&d).unwrap_or_default() {
                    if self.alias.get(&t).is_some_and(|&(root, _, _)| root == d) {
                        self.alias.remove(&t);
                    }
                }
                if let Op::Bin(BinOp::I32Add, x, y) = inst.op {
                    let c = (self.const_of[y as usize], self.const_of[x as usize]);
                    let (mx, my) = (self.max_of[x as usize], self.max_of[y as usize]);
                    let (root, off, r) = match c {
                        (Some(c), _) => self.resolve(x, c),
                        (None, Some(c)) => self.resolve(y, c),
                        // A base plus a small index (an interpreter's
                        // operand field, a byte table lookup).
                        _ if my <= MAX_INDEX && mx > MAX_INDEX => {
                            let (root, off, r) = self.resolve(x, 0);
                            (root, off, r.saturating_add(my))
                        }
                        _ if mx <= MAX_INDEX && my > MAX_INDEX => {
                            let (root, off, r) = self.resolve(y, 0);
                            (root, off, r.saturating_add(mx))
                        }
                        _ => (d, 0, 0),
                    };
                    if root != d && r <= MAX_INDEX {
                        let temp = |v: V| self.def_block[v as usize] != u32::MAX;
                        if self.structured && temp(d) && temp(root) {
                            self.alias_tree.push(d, (root, off, r));
                        } else {
                            self.alias.insert(d, (root, off, r));
                            self.alias_by_root.entry(root).or_default().push(d);
                        }
                    }
                }
            }
        }
    }

    /// Upper bounds of single-definition temporaries (see `max_of`), in
    /// one pass over the blocks in reverse postorder (definitions come
    /// before their uses).
    fn compute_max_of(&mut self, order: &[BlockId]) {
        let f = self.f;
        let mut m = vec![u32::MAX; f.vtypes.len()];
        for &b in order {
            for inst in &f.blocks[b as usize].insts {
                let Some(d) = inst.dst else { continue };
                if self.def_block[d as usize] == u32::MAX || f.vtypes[d as usize] != Ty::I32 {
                    continue;
                }
                let k = |v: V| self.const_of[v as usize];
                m[d as usize] = match inst.op {
                    Op::Const(c) => c as u32,
                    Op::Copy(a) => m[a as usize],
                    Op::Bin(BinOp::I32And, a, b) => m[a as usize].min(m[b as usize]),
                    Op::Bin(BinOp::I32ShrU, a, b) => match k(b) {
                        Some(s) => m[a as usize] >> (s & 31),
                        None => u32::MAX,
                    },
                    Op::Bin(BinOp::I32Shl, a, b) => match k(b) {
                        Some(s) if m[a as usize].leading_zeros() >= (s & 31) => {
                            m[a as usize] << (s & 31)
                        }
                        _ => u32::MAX,
                    },
                    Op::Bin(BinOp::I32Mul, a, b) => {
                        m[a as usize].checked_mul(m[b as usize]).unwrap_or(u32::MAX)
                    }
                    Op::Bin(BinOp::I32Add, a, b) => {
                        m[a as usize].checked_add(m[b as usize]).unwrap_or(u32::MAX)
                    }
                    Op::Load { mem, .. } if !mem.signed && mem.size < 4 => {
                        (1u32 << (8 * mem.size as u32)) - 1
                    }
                    _ => u32::MAX,
                };
            }
        }
        self.max_of = m;
    }

    // ---- Check elimination -----------------------------------------------------
    //
    // A passed guest-limit check puts `addr + off` in [NULL_LIMIT,
    // guest limit - 16]. A later load at `addr + off2`, with the same `addr`
    // value and |off2 - off| up to CHECK_WINDOW, then lies within 32 KB of
    // that range: in the guest's 64 KB null region or in the lookup's first
    // level (4 MB) just above the guest limit, so reading it cannot harm
    // the runtime. Such loads skip their check. Addresses are tracked as a
    // root plus a constant and, through aliases, a bounded index (an
    // interpreter's `base + (insn >> 3 & 0xff0)`), so a check of one such
    // address covers the others within the window. The cost is precision:
    // a load there reads memory instead of faulting, which only a pointer
    // within 32 KB of those boundaries can notice. Stores share checks the
    // same way: the 64 KB above the guest limit hold nothing
    // (`native_layout::GUARD`). While code may be written they go through
    // the store map regardless (it has to see every page they touch).

    /// `v + off` as `root + off' + x` for some x in [0, r], following this
    /// block's aliases.
    fn resolve(&self, v: V, off: u32) -> (V, u32, u32) {
        match self.alias.get(&v).or_else(|| self.alias_tree.get(v).first()) {
            Some(&(root, o, r)) => (root, o.wrapping_add(off), r),
            None => (v, off, 0),
        }
    }

    /// Whether a passed check covers a load at `v + off`: every address the
    /// load may access is within CHECK_WINDOW of every address the check
    /// may have passed.
    fn check_covered(&self, v: V, off: u32) -> bool {
        let (v, lo2, r) = self.resolve(v, off);
        let hi2 = lo2.wrapping_add(r);
        let near = |a: u32, b: u32| (a.wrapping_sub(b) as i32).unsigned_abs() <= CHECK_WINDOW;
        // The most recent facts about `v` only: older ones are rarely the
        // nearest, and forgetting one costs a check, never correctness.
        let local = self.facts_local.get(&v).map_or(&[][..], |f| &f[..]);
        local
            .iter()
            .rev()
            .take(MAX_FACTS)
            .chain(self.facts_tree.get(v).iter().rev().take(MAX_FACTS))
            .any(|&(lo, hi)| near(hi2, lo) && near(hi, lo2))
    }

    /// Records that the address `v + off` passed a check.
    fn checked(&mut self, v: V, off: u32) {
        let (v, lo, r) = self.resolve(v, off);
        let hi = lo.wrapping_add(r);
        let d = self.def_block.get(v as usize).copied().unwrap_or(u32::MAX);
        if self.structured && d != u32::MAX && self.dominates(d, self.cur_block) {
            self.facts_tree.push(v, (lo, hi));
        } else {
            self.facts_local.entry(v).or_default().push((lo, hi));
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
        match self.m.cfg.guest_limit {
            Some(l) if !self.mem64() => {
                self.emit(W::I64Const(l.wrapping_sub(NULL_LIMIT + 16) as i64))
            }
            _ => {
                self.emit(W::GlobalGet(G_GUEST_CHECK));
                if !self.mem64() {
                    self.emit(W::I64ExtendI32U);
                }
            }
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

    /// The access violation code for a read or a write.
    fn av_code(write: bool) -> u32 {
        if write {
            fault::ACCESS_VIOLATION_WRITE
        } else {
            fault::ACCESS_VIOLATION
        }
    }

    /// Emits an address check for the access at vreg `v` + `off`.
    fn check_addr(&mut self, v: V, off: u32, write: bool, dirty: StateMask, eip: u64) {
        self.residue.checks += 1;
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
            self.raise(dirty & FAULT_SYNC, Self::av_code(write), eip);
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
        self.guest_check_value();
        self.emit(W::I32GtU);
        self.emit(W::If(BlockType::Empty));
        self.raise(dirty & FAULT_SYNC, Self::av_code(write), eip);
        self.emit(W::End);
    }

    /// Pushes the store-map byte for local `a` (an i32 address).
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
    /// `ma` is a local holding the address in the memory's address type;
    /// with 64-bit memory it must have passed the address check (the store
    /// map covers the guest region).
    fn check_code_write(&mut self, ma: u32, off: u32) {
        self.smc_checked = true;
        if self.mem64() {
            // page = (ma + off) >> 12; L1 entries are 8 bytes. A page with
            // code is below the guest limit, so within the lookup (64-bit
            // code's included).
            let a = self.tmp_i64b;
            self.emit(W::LocalGet(ma));
            if off != 0 {
                self.emit(W::I64Const(off as i64));
                self.emit(W::I64Add);
            }
            self.emit(W::I64Const(12));
            self.emit(W::I64ShrU);
            self.emit(W::LocalTee(a));
            self.emit(W::GlobalGet(G_STORE_MAP));
            self.emit(W::I64Add);
            self.emit(W::I32Load8U(memarg(0, 0)));
            self.emit(W::LocalTee(self.tmp_map));
            self.emit(W::I32Const(store_map::CODE as i32));
            self.emit(W::I32And);
            self.emit(W::If(BlockType::Empty));
            // l1[page] = zero_l2
            self.emit(W::GlobalGet(G_LOOKUP_L1));
            self.emit(W::LocalGet(a));
            self.emit(W::I64Const(3));
            self.emit(W::I64Shl);
            self.emit(W::I64Add);
            self.emit(W::GlobalGet(G_ZERO_L2));
            self.emit(W::I64Store(memarg(0, 3)));
            // store_map[page] &= ~CODE
            self.emit(W::GlobalGet(G_STORE_MAP));
            self.emit(W::LocalGet(a));
            self.emit(W::I64Add);
            self.emit(W::LocalGet(self.tmp_map));
            self.emit(W::I32Const(!(store_map::CODE as i32)));
            self.emit(W::I32And);
            self.emit(W::I32Store8(memarg(0, 0)));
            self.smc_mark();
            self.emit(W::End);
            return;
        }
        let a = self.tmp_i32b;
        self.emit(W::LocalGet(ma));
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
        self.smc_mark();
        self.emit(W::End);
    }

    /// In a store's code-write branch: the function must stop after this
    /// instruction (see `smc_local`).
    fn smc_mark(&mut self) {
        if self.smc_local != UNASSIGNED {
            self.emit(W::I32Const(1));
            self.emit(W::LocalSet(self.smc_local));
        }
    }

    /// Returns to the dispatcher at `addr`, with the state in `dirty`
    /// written back, if a store since the last check hit translated code.
    fn smc_exit(&mut self, dirty: StateMask, addr: u64) {
        if self.smc_local == UNASSIGNED {
            return;
        }
        self.emit(W::LocalGet(self.smc_local));
        self.emit(W::If(BlockType::Empty));
        self.ctx.push(Ctx::If);
        self.sync(dirty);
        self.code_const(addr);
        self.emit(W::Return);
        self.ctx.pop();
        self.emit(W::End);
    }

    /// The checks before a plain store: one store-map lookup on the fast
    /// path; pages that need more (code, the null region, the guest limit)
    /// get the precise address check and the code-write notification.
    /// The host hears about a code write before the store happens, which is
    /// equivalent: it only drops translations, which are redone on demand.
    ///
    /// The store map is consulted only while the runtime says code may be
    /// written (`cpu::CODE_WRITABLE`); otherwise a store needs just the
    /// guest-limit check, skipped when a nearby check `covered` it.
    /// 64-bit addresses (x86-64 code, or a 64-bit memory) have no fast
    /// path: the map covers only the guest region, so the address is
    /// checked first, then the map's CODE bit.
    fn check_store(&mut self, v: V, off: u32, covered: bool, dirty: StateMask, eip: u64) {
        let (mem, smc) = (self.m.cfg.mem_checks, self.m.cfg.smc_checks);
        let plain = mem && !covered;
        if !smc {
            if plain {
                self.check_addr(v, off, true, dirty, eip);
            }
            return;
        }
        if self.f.ty(v) == Ty::I64 || self.mem64() {
            if mem || self.mem64() {
                self.check_addr(v, off, true, dirty, eip);
            }
            let la = self.addr_local(v);
            self.check_code_write(la, off);
            return;
        }
        self.code_writable();
        self.emit(W::If(BlockType::Empty));
        self.check_store_map(v, off, dirty, eip);
        if plain {
            self.emit(W::Else);
            self.check_addr(v, off, true, dirty, eip);
        }
        self.emit(W::End);
    }

    /// Pushes `cpu.CODE_WRITABLE`.
    fn code_writable(&mut self) {
        self.emit(W::LocalGet(0));
        self.emit(W::I32Load(memarg(cpu::CODE_WRITABLE, 2)));
    }

    /// The store-map path of a 32-bit store: one lookup; pages that need
    /// more get the precise address check and the code-write notification.
    fn check_store_map(&mut self, v: V, off: u32, dirty: StateMask, eip: u64) {
        self.residue.store_maps += 1;
        let mem = self.m.cfg.mem_checks;
        let addr = self.local_of[v as usize];
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
            self.check_addr(v, off, true, dirty, eip);
            // It runs only on the slow path: not a check.
            self.residue.checks -= 1;
        }
        self.check_code_write(addr, off);
        self.emit(W::End);
    }

    /// Whether a guest access goes without an explicit check, relying on
    /// the engine's bounds trap (`CodegenConfig::mem_traps`). The address
    /// it uses is `addr + off - NULL_LIMIT`, wrapping as x86's does, plus a
    /// memarg offset of `NULL_LIMIT`: a valid address lands on itself, and
    /// one in the null region wraps to 4 GB or more, beyond any 32-bit
    /// memory. So do addresses above the top of memory; those between the
    /// guest limit and the top (the native region) do not trap.
    fn trap_access(&self, v: V, mem: &Mem) -> bool {
        self.m.cfg.mem_traps
            && self.m.cfg.mem_checks
            && mem.space == Space::Guest
            && !mem.atomic
            && !self.mem64()
            && self.f.ty(v) == Ty::I32
    }

    /// Records the access emitted next, which a nearby trapping access
    /// covered, as a trap site too: its address is not rotated, so the
    /// engine traps only above the top of memory (or past 4 GB, where x86
    /// would wrap), which then still raises an access violation there.
    fn covered_trap_site(&mut self, v: V, mem: &Mem, eip: u64, write: bool) {
        if self.trap_access(v, mem) {
            self.trap_sites.push((self.out.len(), eip, write));
        }
    }

    /// Pushes the address of a trapping access (see `trap_access`).
    fn trap_addr(&mut self, la: u32, off: u32) {
        self.residue.traps += 1;
        self.emit(W::LocalGet(la));
        self.emit(W::I32Const(off.wrapping_sub(NULL_LIMIT) as i32));
        self.emit(W::I32Add);
    }

    fn needs_check(&self, mem: &Mem) -> bool {
        mem.space == Space::Guest && self.m.cfg.mem_checks
    }

    fn inst(&mut self, inst: &Inst, dirty: StateMask) {
        self.inst_value(inst, dirty, false);
    }

    /// Emits `inst`; with `keep`, its value stays on the stack instead of
    /// going to its local (a stackified value, emitted at its use).
    fn inst_value(&mut self, inst: &Inst, dirty: StateMask, keep: bool) {
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
            Op::Load { addr, mem }
                if self.trap_access(*addr, mem) && !self.check_covered(*addr, mem.offset) =>
            {
                let la = self.addr_local(*addr);
                self.trap_addr(la, mem.offset);
                self.trap_sites.push((self.out.len(), inst.eip, false));
                self.emit(load_instr(ty.unwrap(), &trap_mem(mem), false));
                self.checked(*addr, mem.offset);
            }
            Op::Load { addr, mem } => {
                if self.needs_check(mem) && !self.check_covered(*addr, mem.offset) {
                    self.check_addr(*addr, mem.offset, false, dirty, inst.eip);
                    self.checked(*addr, mem.offset);
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
                    self.covered_trap_site(*addr, mem, inst.eip, false);
                    self.emit(load_instr(ty, mem, mem.atomic));
                }
            }
            Op::Store { addr, val, mem }
                if self.trap_access(*addr, mem) && !self.check_covered(*addr, mem.offset) =>
            {
                self.check_store(*addr, mem.offset, true, dirty, inst.eip);
                let la = self.addr_local(*addr);
                self.trap_addr(la, mem.offset);
                self.get(*val);
                self.trap_sites.push((self.out.len(), inst.eip, true));
                self.emit(store_instr(self.f.ty(*val), &trap_mem(mem), false));
                self.checked(*addr, mem.offset);
            }
            Op::Store { addr, val, mem } => {
                if mem.space == Space::Guest {
                    let covered = self.check_covered(*addr, mem.offset);
                    self.check_store(*addr, mem.offset, covered, dirty, inst.eip);
                    if self.m.cfg.mem_checks {
                        self.checked(*addr, mem.offset);
                    }
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
                    self.covered_trap_site(*addr, mem, inst.eip, true);
                    self.emit(store_instr(vty, mem, mem.atomic));
                }
            }
            Op::AtomicRmw { op, addr, val, mem } => {
                if mem.space == Space::Guest && self.m.cfg.mem_checks {
                    self.check_addr(*addr, mem.offset, true, dirty, inst.eip);
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
                // Misaligned, or no atomics wanted: plain load + op + store.
                let atomics = self.m.cfg.atomics;
                if atomics {
                    self.misaligned(la, mem.offset, mem.size);
                    self.emit(W::If(BlockType::Result(val_type(ty))));
                }
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
                if atomics {
                    self.emit(W::Else);
                    self.emit(W::LocalGet(la));
                    self.get(*val);
                    self.emit(rmw_instr(*op, mem, wide));
                    self.emit(W::End);
                }
                if mem.space == Space::Guest && self.m.cfg.smc_checks {
                    if let Some(d) = inst.dst {
                        self.set(d);
                    } else {
                        self.emit(W::Drop);
                    }
                    self.code_writable();
                    self.emit(W::If(BlockType::Empty));
                    self.check_code_write(la, mem.offset);
                    self.emit(W::End);
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
                    self.check_addr(*addr, mem.offset, true, dirty, inst.eip);
                }
                let la = self.addr_local(*addr);
                let ty = ty.unwrap_or(Ty::I32);
                let (tmp, eq) = if ty == Ty::I64 {
                    (self.tmp_i64, W::I64Eq)
                } else {
                    (self.tmp_i32, W::I32Eq)
                };
                let atomics = self.m.cfg.atomics;
                if atomics {
                    self.misaligned(la, mem.offset, mem.size);
                    self.emit(W::If(BlockType::Result(val_type(ty))));
                }
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
                if atomics {
                    self.emit(W::Else);
                    self.emit(W::LocalGet(la));
                    self.get(*expected);
                    self.get(*new);
                    self.emit(cmpxchg_instr(ty, mem));
                    self.emit(W::End);
                }
            }
            Op::MemCopy { dst, src, len } => {
                // With 64-bit memory the code-write check needs the range
                // checked (the store map covers the guest region only).
                if self.m.cfg.mem_checks || self.mem64() && self.m.cfg.smc_checks {
                    // The source is read before the destination is written.
                    self.check_range(*src, *len, false, dirty, inst.eip);
                    self.check_range(*dst, *len, true, dirty, inst.eip);
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
                if self.m.cfg.mem_checks || self.mem64() && self.m.cfg.smc_checks {
                    self.check_range(*dst, *len, true, dirty, inst.eip);
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
            Op::Math {
                op: MathOp::Sin, a, ..
            } => {
                self.get(*a);
                self.emit(W::Call(F_SIN));
            }
            Op::Math {
                op: MathOp::Cos, a, ..
            } => {
                self.get(*a);
                self.emit(W::Call(F_COS));
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
            Some(_) if keep => {}
            Some(d) => self.set(d),
            None => {
                if matches!(inst.op, Op::AtomicRmw { .. } | Op::AtomicCmpxchg { .. }) {
                    self.emit(W::Drop);
                }
            }
        }
    }

    // ---- Stackifying -----------------------------------------------------------
    //
    // A temporary used once, in the same block, by an instruction that reads
    // its operands straight from locals in order, can stay on the
    // WebAssembly stack: its instruction is emitted where the use reads it
    // instead of before, saving a local.set/local.get pair. Over half of a
    // module's instructions were local.get/set; fewer instructions compile
    // faster and run faster before the engine optimizes them. The producers
    // of a use's operands must be the instructions right before it, in
    // operand order (so nothing is reordered), and a producer with effects
    // (a load, which may fault) only when the use emits nothing before
    // reading it.

    /// Operands of `op` that its emission reads once each, in order, with
    /// nothing emitted before them; and whether producers with effects may
    /// supply them.
    fn stack_operands(op: &Op) -> (Vec<V>, bool) {
        match op {
            Op::Copy(a) | Op::Un(_, a) => (vec![*a], true),
            Op::Bin(_, a, b) => (vec![*a, *b], true),
            Op::Select { cond, t, f } => (vec![*t, *f, *cond], true),
            // The value is read after the address checks.
            Op::Store { val, mem, .. } if !mem.atomic || mem.size == 1 => (vec![*val], false),
            _ => (vec![], false),
        }
    }

    /// Whether `inst` can be emitted at its use: pure, or (with `effects`) a
    /// plain load.
    fn stackable(inst: &Inst, effects: bool) -> bool {
        match &inst.op {
            Op::Const(_) | Op::Copy(_) | Op::Un(..) | Op::Select { .. } => true,
            Op::Bin(op, ..) => effects || !op.can_trap(),
            Op::Load { mem, .. } => effects && !mem.atomic,
            _ => false,
        }
    }

    /// Marks the instructions of a block that are emitted at their use.
    fn stackify(&self, blk: &Block) -> Vec<bool> {
        let n = blk.insts.len();
        let mut deferred = vec![false; n];
        let one_use = |v: V| {
            v >= NUM_STATE
                && self.use_count[v as usize] == 1
                && self.a.dense[v as usize] == u32::MAX
        };
        // Claims producers for `operands`, last operand first, from the
        // instructions just before `cursor`; returns where they start.
        fn claim(
            blk: &Block,
            deferred: &mut [bool],
            one_use: &dyn Fn(V) -> bool,
            operands: &[V],
            effects: bool,
            mut cursor: usize,
        ) -> usize {
            for &o in operands.iter().rev() {
                if cursor == 0 {
                    break;
                }
                let j = cursor - 1;
                let p = &blk.insts[j];
                if p.dst != Some(o) || !one_use(o) || deferred[j] || !FnGen::stackable(p, effects) {
                    break;
                }
                deferred[j] = true;
                let (sub, sub_effects) = FnGen::stack_operands(&p.op);
                cursor = claim(blk, deferred, one_use, &sub, sub_effects, j);
            }
            cursor
        }
        if let Term::Branch { cond, .. } = blk.term {
            claim(blk, &mut deferred, &one_use, &[cond], true, n);
        }
        for c in (0..n).rev() {
            if deferred[c] {
                continue;
            }
            let (ops, effects) = FnGen::stack_operands(&blk.insts[c].op);
            claim(blk, &mut deferred, &one_use, &ops, effects, c);
        }
        deferred
    }

    /// Checks that [addr, addr+len) lies in the guest region.
    fn check_range(&mut self, addr: V, len: V, write: bool, dirty: StateMask, eip: u64) {
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
            self.raise(dirty & FAULT_SYNC, Self::av_code(write), eip);
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
        self.guest_check_value();
        self.emit(W::I32GtU);
        self.emit(W::I32Or);
        self.emit(W::If(BlockType::Empty));
        self.raise(dirty & FAULT_SYNC, Self::av_code(write), eip);
        self.emit(W::End);
    }

    /// Notifies the host for each code page in [addr, addr+len).
    fn check_code_write_range(&mut self, addr: V, len: V) {
        // if (code_writable) for (p = addr & ~0xfff; p < addr + len; p += 0x1000) check(p)
        self.code_writable();
        self.emit(W::If(BlockType::Empty));
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
        self.emit(W::End);
    }
}

/// `mem` as accessed by a trapping access (see `FnGen::trap_access`).
fn trap_mem(mem: &Mem) -> Mem {
    Mem {
        offset: NULL_LIMIT,
        ..*mem
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

/// `eflags`: the function index of the Eflags helper.
fn gen_helper(h: Helper, eflags: u32) -> wasm_encoder::Function {
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
            out.push(W::Call(eflags));
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
            out.push(W::Call(eflags));
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
        Helper::Tsc => {
            // param: previous (i64); local 1: the clock's value.
            locals.push(ValType::I64);
            out.push(W::GlobalGet(G_TICK));
            out.push(W::I32Load(MemArg {
                offset: 0,
                align: 2,
                memory_index: 0,
            }));
            out.push(W::I64ExtendI32U);
            out.push(W::I64Const(3_000_000));
            out.push(W::I64Mul);
            out.push(W::LocalSet(1));
            out.push(W::LocalGet(0));
            out.push(W::I64Const(64));
            out.push(W::I64Add);
            out.push(W::LocalTee(0));
            out.push(W::LocalGet(1));
            out.push(W::LocalGet(0));
            out.push(W::LocalGet(1));
            out.push(W::I64GtU);
            out.push(W::Select);
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
            super::F_FIRST_NATIVE
        );
    }
}
