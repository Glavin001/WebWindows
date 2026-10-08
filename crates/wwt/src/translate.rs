//! Layer 7 — Orchestration, metadata and caching.
//!
//! [`translate_pe`] runs the whole pipeline over a PE file ahead of time;
//! [`translate_region`] is the fast mode used for code discovered while the
//! program runs. Both produce one WebAssembly module whose functions are
//! registered under their x86 addresses at load time.

use std::collections::{BTreeSet, HashMap, HashSet};

use anyhow::Result;
use serde::Serialize;

use crate::abi;
use crate::codegen::{CodegenConfig, ModuleGen};
use crate::discover::{scan_data_for_code_pointers_in, CodeSource, Discovery, SeedKind};
use crate::ir::{
    BlockId, CallAbi, CallTarget, FlagFree, FlagFreeCallees, Function, Inst, Mem, Mode, Op, Space,
    Term, Ty,
};
use crate::lift::{lift_function, LiftConfig};
use crate::opt;
use crate::pe::{ExportTarget, PeFile};

#[derive(Debug, Clone)]
pub struct Config {
    pub lift: LiftConfig,
    pub codegen: CodegenConfig,
    /// 0: only the passes required for correctness; 1: full optimization.
    pub opt_level: u32,
    /// Scan data sections for code pointers when the file has no relocations.
    pub scan_data: bool,
    /// Inline small leaf functions into their callers (opt level 1).
    pub inline: bool,
    /// Give exported memory functions (`memmove`, `memset`, ...) built-in
    /// bodies using WebAssembly's bulk memory operations.
    pub builtins: bool,
    /// Whether the image is compiled C, which follows the C calling
    /// convention (`CallAbi::c`). `None`: decide from the image: Wine's own
    /// modules are; for a program, what its own code shows (see
    /// `prove_flags_abi`).
    pub c_abi: Option<bool>,
    /// Look for evidence that the module's code never reads the flags
    /// across calls and returns, and assume so where it holds (see
    /// `prove_flags_abi`). Set for programs (not DLLs, whose callers are
    /// unknown).
    pub prove_flags_abi: bool,
    /// Replace ntdll's heap functions with the host's native heap
    /// (`builtin::NATIVE_HEAP`); applies to ntdll.dll only.
    pub native_heap: bool,
    /// Try the host's native string and locale functions before the
    /// translated ones (`builtin::NATIVE_TRY`); applies to Wine's own DLLs.
    pub native_strings: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            lift: LiftConfig::default(),
            codegen: CodegenConfig::default(),
            opt_level: 1,
            scan_data: true,
            inline: true,
            builtins: true,
            c_abi: None,
            prove_flags_abi: false,
            native_heap: false,
            native_strings: false,
        }
    }
}

impl Config {
    /// Settings for code discovered while running: quick to produce.
    pub fn fast() -> Config {
        Config {
            opt_level: 0,
            ..Config::default()
        }
    }

    /// The same settings for code of `mode` (32-bit or 64-bit x86).
    pub fn with_mode(mut self, mode: Mode) -> Config {
        self.lift.mode = mode;
        self
    }

    /// Targets a 64-bit (memory64) WebAssembly memory.
    pub fn with_mem64(mut self, mem64: bool) -> Config {
        self.lift.mem64 = mem64;
        self.codegen.mem64 = mem64;
        self
    }

    pub fn mode(&self) -> Mode {
        self.lift.mode
    }
}

#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub functions: usize,
    pub blocks: usize,
    pub x86_instructions: usize,
    pub wasm_bytes: usize,
    pub seeds: std::collections::BTreeMap<String, usize>,
    /// Instructions the lifter could not translate: (address, text).
    pub unsupported: Vec<(u64, String)>,
    /// Indirect jumps recognized as jump tables.
    pub jump_tables: usize,
    /// Functions shown to return without their callers reading the flags
    /// (`prove_flags_abi`).
    pub flag_free_returns: usize,
}

pub struct Translation {
    pub wasm: Vec<u8>,
    /// Translated function entry points, in table order.
    pub entries: Vec<u64>,
    pub report: Report,
    /// The IR of each function after optimization (for inspection).
    pub ir: Vec<Function>,
}

/// Image description stored in the module so the runtime can load the PE
/// without parsing it again.
#[derive(Serialize)]
pub struct ImageMeta {
    /// x86 or x86-64; the runtime picks the CPU layout from it.
    pub mode: Mode,
    pub machine: u16,
    /// The base the translation was made for (where the image must load).
    pub image_base: u64,
    /// The base in the file (differs for 64-bit images moved below 4 GB).
    pub preferred_base: u64,
    pub size_of_image: u32,
    pub size_of_headers: u32,
    pub entry: Option<u64>,
    pub is_dll: bool,
    pub subsystem: u16,
    pub stack_reserve: u32,
    pub stack_commit: u32,
    pub heap_reserve: u32,
    pub sections: Vec<crate::pe::Section>,
    pub imports: Vec<crate::pe::Import>,
    pub exports: Vec<crate::pe::Export>,
    pub tls: Option<crate::pe::TlsInfo>,
    pub has_relocations: bool,
}

#[derive(Serialize)]
struct Meta<'a> {
    abi_version: u32,
    /// Set when checks were compiled against this guest limit.
    #[serde(skip_serializing_if = "Option::is_none")]
    guest_limit: Option<u32>,
    image: Option<&'a ImageMeta>,
    report: &'a Report,
}

pub fn image_meta(pe: &PeFile) -> ImageMeta {
    ImageMeta {
        mode: pe.mode,
        machine: pe.machine,
        image_base: pe.image_base,
        preferred_base: pe.preferred_base,
        size_of_image: pe.size_of_image,
        size_of_headers: pe.size_of_headers,
        entry: pe.entry_point(),
        is_dll: pe.is_dll(),
        subsystem: pe.subsystem,
        stack_reserve: pe.stack_reserve,
        stack_commit: pe.stack_commit,
        heap_reserve: pe.heap_reserve,
        sections: pe.sections.clone(),
        imports: pe.imports.clone(),
        exports: pe.exports.clone(),
        tls: pe.tls.clone(),
        has_relocations: pe.relocations.is_some(),
    }
}

/// Sections the loader may drop after loading (debug information, relocations).
const IMAGE_SCN_MEM_DISCARDABLE: u32 = 0x0200_0000;

/// Discovers code in a PE image from all static seeds plus `profile` (code
/// found at run time on earlier launches).
pub fn discover_pe(pe: &PeFile, img: &dyn CodeSource, profile: &[u64], cfg: &Config) -> Discovery {
    let mut d = Discovery::new_in(pe.mode);
    if let Some(e) = pe.entry_point() {
        d.add_function_seed(e, SeedKind::Entry);
    }
    // x86-64: every function that calls or allocates stack has a `.pdata`
    // entry.
    for &rva in &pe.pdata {
        let va = pe.image_base + rva as u64;
        if img.is_code(va) {
            d.add_function_seed(va, SeedKind::Pdata);
        }
    }
    for ex in &pe.exports {
        if let ExportTarget::Rva(rva) = ex.target {
            let va = pe.image_base + rva as u64;
            if img.is_code(va) {
                d.add_function_seed(va, SeedKind::Export);
            }
        }
    }
    if let Some(tls) = &pe.tls {
        for &cb in &tls.callbacks {
            if img.is_code(cb) {
                d.add_function_seed(cb, SeedKind::TlsCallback);
            }
        }
    }
    for &p in profile {
        if img.is_code(p) {
            d.add_function_seed(p, SeedKind::Profile);
        }
    }
    // Native functions must be functions of their own, also where ntdll
    // only tail-jumps to them (`_heap_thread_detach`).
    if cfg.native_heap {
        for (va, name) in pe.code_names() {
            if crate::builtin::native_heap(&name).is_some() && img.is_code(va) {
                d.add_function_seed(va, SeedKind::Export);
            }
        }
    }
    d.explore(img);
    // Code pointers in data: relocation targets (always present in DLLs),
    // otherwise a scan of data sections. Addresses already known as
    // instruction starts are reached some other way (jump tables, calls).
    let mut candidates: BTreeSet<(u64, SeedKind)> = BTreeSet::new();
    if let Some(relocs) = &pe.relocations {
        for (k, &r) in relocs.iter().enumerate() {
            let site = pe.image_base + r as u64;
            let v = if pe.reloc_types.get(k) == Some(&crate::pe::REL_DIR64) {
                img.read_u64(site)
            } else {
                img.read_u32(site).map(|v| v as u64)
            };
            if let Some(v) = v {
                // Skip values inside code that are operands of instructions in
                // code (e.g. `push offset label`), unless they point at code.
                if img.is_code(v) {
                    candidates.insert((v, SeedKind::Relocation));
                }
            }
        }
    }
    // Executables often carry no relocations: then scan their data for
    // code pointers (each confirmed by decoding). With relocations, those
    // list every pointer (one without would break when the image is
    // rebased), and a scan only finds false ones: in debug information,
    // DWARF line tables hold an address for every statement, and each
    // would split its function. Discardable sections (debug information,
    // `.reloc`) hold no pointers the program uses.
    let relocated = pe.relocations.as_ref().is_some_and(|r| !r.is_empty());
    if cfg.scan_data && !pe.is_dll() && !relocated {
        for s in &pe.sections {
            if s.is_executable()
                || s.characteristics & 0xC0 == 0x80
                || s.characteristics & IMAGE_SCN_MEM_DISCARDABLE != 0
            {
                continue;
            }
            let start = pe.image_base + s.virtual_address as u64;
            let end = start + s.raw_size.min(s.mem_size()) as u64;
            for v in scan_data_for_code_pointers_in(img, start, end, pe.mode) {
                candidates.insert((v, SeedKind::DataScan));
            }
        }
    }
    for (v, kind) in candidates {
        if !d.insts.contains_key(&v) {
            d.add_function_seed(v, kind);
        }
    }
    d.explore(img);
    // setjmp returns a second time through longjmp, after its caller's
    // frame has moved on: its return sites must be entry points.
    let setjmp_slots: Vec<u64> = pe
        .imports
        .iter()
        .filter(|i| {
            let n = i.name.to_string().to_ascii_lowercase();
            n.contains("setjmp")
        })
        .map(|i| pe.image_base + i.iat_rva as u64)
        .collect();
    if !setjmp_slots.is_empty() {
        use iced_x86::{FlowControl, OpKind, Register};
        // `[slot]`, or `[rip+slot]` in 64-bit code.
        let via_slot = |i: &iced_x86::Instruction| {
            i.op0_kind() == OpKind::Memory
                && matches!(i.memory_base(), Register::None | Register::RIP)
                && i.memory_index() == Register::None
                && setjmp_slots.contains(&i.memory_displacement64())
        };
        // Import stubs: `jmp [slot]` (how MinGW calls undeclared imports).
        let stubs: Vec<u64> = d
            .insts
            .values()
            .filter(|i| i.flow_control() == FlowControl::IndirectBranch && via_slot(i))
            .map(|i| i.ip())
            .collect();
        let returns: Vec<u64> = d
            .insts
            .values()
            .filter(|i| {
                (i.flow_control() == FlowControl::IndirectCall && via_slot(i))
                    || (i.flow_control() == FlowControl::Call
                        && crate::lift::branch_target(i, 0).is_some_and(|t| stubs.contains(&t)))
            })
            .map(|i| i.next_ip())
            .collect();
        for r in returns {
            d.add_function_seed(r, SeedKind::Call);
        }
        d.explore(img);
    }
    d
}

/// Translates a PE file ahead of time. The mode (x86 or x86-64) comes from
/// the file. A 64-bit image preferred above 4 GB is folded below it when the
/// target is a 32-bit memory.
pub fn translate_pe(pe: &PeFile, cfg: &Config, profile: &[u64]) -> Result<Translation> {
    let mut cfg = cfg.clone().with_mode(pe.mode);
    if !cfg.codegen.mem64 && pe.image_base >> 32 != 0 {
        let mut low = pe.clone();
        low.fold_below_4gb()?;
        return translate_pe(&low, &cfg, profile);
    }
    match cfg.c_abi {
        Some(true) => cfg.lift.abi = CallAbi::c(),
        None if pe.is_wine_builtin() => cfg.lift.abi = CallAbi::c(),
        None => cfg.prove_flags_abi = !pe.is_dll(),
        Some(false) => {}
    }
    cfg.lift.define_all_flags = cfg.lift.abi.flags_dead_at_ret || cfg.prove_flags_abi;
    let is_ntdll = pe
        .dll_name
        .as_deref()
        .is_some_and(|n| n.eq_ignore_ascii_case("ntdll.dll"));
    cfg.native_heap &= is_ntdll;
    // The built-in and native function bodies read their arguments the
    // 32-bit way (from the stack).
    if pe.mode != Mode::X86 {
        cfg.builtins = false;
        cfg.native_heap = false;
        cfg.native_strings = false;
    }
    let cfg = &cfg;
    let img = pe.image()?;
    let mut d = discover_pe(pe, &img, profile, cfg);
    let meta = image_meta(pe);
    let names: HashMap<u64, String> = pe.code_names().into_iter().collect();
    let tried = if cfg.native_strings && pe.is_wine_builtin() {
        natives_tried(pe, &img)
    } else {
        HashMap::new()
    };
    translate_discovered(&img, &mut d, cfg, Some(&meta), &names, &tried)
}

/// Native implementations to try first, by function entry: those
/// `builtin::NATIVE_TRY` has for this DLL's exports (or COFF symbols), with
/// the addresses of the symbols they need (all must be present).
fn natives_tried(pe: &PeFile, img: &crate::pe::Image) -> HashMap<u64, (&'static str, Vec<u32>)> {
    let mut out = HashMap::new();
    let Some(dll) = pe.dll_name.as_deref().map(str::to_ascii_lowercase) else {
        return out;
    };
    for n in crate::builtin::NATIVE_TRY
        .iter()
        .filter(|n| n.dlls.contains(&dll.as_str()))
    {
        let va = if n.name.starts_with('_') {
            pe.symbol(n.name)
        } else {
            pe.exports
                .iter()
                .find_map(|ex| match (&ex.name, &ex.target) {
                    (Some(name), ExportTarget::Rva(rva)) if name == n.name => {
                        Some(pe.image_base + *rva as u64)
                    }
                    _ => None,
                })
        };
        // 32-bit images only (see `translate_pe`).
        let args: Option<Vec<u32>> = n
            .symbols
            .iter()
            .map(|s| pe.symbol(s).map(|a| a as u32))
            .collect();
        if let (Some(va), Some(args)) = (va, args) {
            if img.is_code(va) {
                out.insert(va, (n.import, args));
            }
        }
    }
    out
}

/// Fast mode: translates code reachable from `entries` in `src`.
pub fn translate_region(
    src: &dyn CodeSource,
    entries: &[u64],
    cfg: &Config,
) -> Result<Translation> {
    translate_region_with_known(src, entries, &[], cfg)
}

/// Fast mode with a set of function entries already translated elsewhere,
/// which are called through the lookup rather than translated again.
pub fn translate_region_with_known(
    src: &dyn CodeSource,
    entries: &[u64],
    known: &[u64],
    cfg: &Config,
) -> Result<Translation> {
    let mut d = Discovery::new_in(cfg.mode());
    for &k in known {
        if !entries.contains(&k) {
            d.functions.insert(k);
            d.external.insert(k);
        }
    }
    for &e in entries {
        d.add_function_seed(e, SeedKind::Profile);
    }
    d.explore(src);
    translate_discovered(src, &mut d, cfg, None, &HashMap::new(), &HashMap::new())
}

/// Lifts, optimizes and generates code for every discovered function.
/// `names` (symbols by address) label functions in the module's name
/// section, for profilers and debuggers, and select native heap functions;
/// `tried` gives functions a native implementation to try first.
pub fn translate_discovered(
    src: &dyn CodeSource,
    d: &mut Discovery,
    cfg: &Config,
    meta: Option<&ImageMeta>,
    names: &HashMap<u64, String>,
    tried: &HashMap<u64, (&'static str, Vec<u32>)>,
) -> Result<Translation> {
    let mut report = Report::default();
    for k in d.seed_kinds.values() {
        *report.seeds.entry(format!("{k:?}")).or_default() += 1;
    }
    report.jump_tables = d.jump_tables.len();
    report.x86_instructions = d.insts.len();
    let mut funcs: Vec<Function> = vec![];
    let mut done: BTreeSet<u64> = BTreeSet::new();
    loop {
        let todo: Vec<u64> = d
            .functions
            .iter()
            .copied()
            .filter(|f| !done.contains(f))
            .collect();
        if todo.is_empty() {
            break;
        }
        for entry in todo {
            done.insert(entry);
            // Entries outside the decodable region (calls out of a fast-mode
            // window) are reached through the lookup instead.
            if !d.insts.contains_key(&entry) {
                continue;
            }
            let name = names.get(&entry);
            let native = name
                .and_then(|n| crate::builtin::native_heap(n))
                .filter(|_| cfg.native_heap);
            let builtin = name
                .and_then(|n| crate::builtin::Builtin::by_name(n))
                .filter(|_| cfg.builtins);
            let f = match (native, builtin) {
                (Some(n), _) => crate::builtin::native_body(entry, n, cfg.lift.mem64),
                (None, Some(b)) => crate::builtin::body(entry, b, cfg.lift.mem64),
                (None, None) => {
                    let lifted = lift_function(src, d, entry, &cfg.lift);
                    report.unsupported.extend(lifted.unsupported);
                    for e in lifted.extra_entries {
                        d.add_function_seed(e, SeedKind::Call);
                    }
                    match tried.get(&entry) {
                        Some((n, args)) => {
                            crate::builtin::with_native_try(lifted.func, n, args.clone())
                        }
                        None => lifted.func,
                    }
                }
            };
            funcs.push(f);
        }
    }
    call_through_import_stubs(&mut funcs);
    if cfg.prove_flags_abi {
        // Imports: Wine's exports never read the flags on entry, except
        // `_chkesp` (MSVC debug builds' stack check after calls).
        let reading = meta
            .map(|m| {
                m.imports
                    .iter()
                    .filter(|i| i.name.to_string().contains("chkesp"))
                    .map(|i| m.image_base + i.iat_rva as u64)
                    .collect()
            })
            .unwrap_or_default();
        // Condition reads name the flag operands they use once lowered
        // (`jp` after `sahf` reads only the flags `sahf` set).
        for f in &mut funcs {
            opt::lower_flags(f);
        }
        report.flag_free_returns = prove_flags_abi(&mut funcs, reading);
    }
    for f in &mut funcs {
        opt::optimize(f, cfg.opt_level);
        report.blocks += f.blocks.len();
    }
    if cfg.opt_level > 0 && cfg.inline {
        for i in crate::inline::inline_leaves(&mut funcs) {
            opt::optimize(&mut funcs[i], cfg.opt_level);
        }
    }
    funcs.sort_by_key(|f| f.entry);
    report.functions = funcs.len();
    report.unsupported.sort();
    report.unsupported.dedup();
    let entries: Vec<u64> = funcs.iter().map(|f| f.entry).collect();
    let meta_json = serde_json::to_string(&Meta {
        abi_version: abi::ABI_VERSION,
        guest_limit: cfg.codegen.guest_limit,
        image: meta,
        report: &report,
    })?;
    // The code width follows the mode even with no functions (a DLL that
    // only forwards its exports).
    let gen = ModuleGen::new(&cfg.codegen, &funcs)
        .with_code64(cfg.mode() == Mode::X64 && cfg.codegen.mem64)
        .with_names(names)
        .with_code(src);
    let wasm = gen.build(&funcs, &meta_json);
    report.wasm_bytes = wasm.len();
    Ok(Translation {
        wasm,
        entries,
        report,
        ir: funcs,
    })
}

/// A direct call to an import stub (`jmp [slot]`, how MinGW programs
/// call their imports) calls through the slot instead: one call instead of
/// two, and the lookup resolves the slot's target (which may itself be a
/// thunk the runtime resolved, see runtime/wine/thunks.mjs). Tail jumps
/// to stubs likewise. In 64-bit code the slot holds 8 bytes.
fn call_through_import_stubs(funcs: &mut [Function]) {
    let stubs: HashMap<u64, (u64, u8, Ty, Ty)> = funcs
        .iter()
        .filter_map(|f| {
            let [b] = f.blocks.as_slice() else {
                return None;
            };
            let Term::JmpInd(v) = b.term else { return None };
            match b.insts.as_slice() {
                [Inst {
                    dst: Some(c),
                    op: Op::Const(slot),
                    ..
                }, Inst {
                    dst: Some(l),
                    op: Op::Load { addr, mem },
                    ..
                }] if *l == v
                    && addr == c
                    && mem.offset == 0
                    && (mem.size == 4 || f.mode == Mode::X64 && mem.size == 8) =>
                {
                    Some((f.entry, (*slot, mem.size, f.ty(*c), f.ty(*l))))
                }
                _ => None,
            }
        })
        .collect();
    if stubs.is_empty() {
        return;
    }
    for f in funcs.iter_mut() {
        for b in 0..f.blocks.len() {
            let target = match f.blocks[b].term {
                Term::Call {
                    target: CallTarget::Direct(t),
                    ..
                }
                | Term::Exit(t) => t,
                _ => continue,
            };
            let Some(&(slot, size, slot_ty, ty)) = stubs.get(&target) else {
                continue;
            };
            if target == f.entry {
                continue;
            }
            let eip = f.blocks[b].insts.last().map_or(f.blocks[b].addr, |i| i.eip);
            let c = f.new_vreg(slot_ty);
            let v = f.new_vreg(ty);
            let blk = &mut f.blocks[b];
            blk.insts.push(Inst {
                dst: Some(c),
                op: Op::Const(slot),
                eip,
            });
            blk.insts.push(Inst {
                dst: Some(v),
                op: Op::Load {
                    addr: c,
                    mem: Mem {
                        size,
                        signed: false,
                        space: Space::Trusted,
                        offset: 0,
                        atomic: false,
                    },
                },
                eip,
            });
            blk.term = match blk.term {
                Term::Call { ret, cont, .. } => Term::Call {
                    target: CallTarget::Indirect(v),
                    ret,
                    cont,
                },
                _ => Term::JmpInd(v),
            };
        }
    }
}

/// Lazy flag state, which the C calling convention leaves dead across calls.
const FLAGS: [u32; 5] = crate::ir::FLAG_STATE;

/// For a program's functions (lifted, not yet optimized): finds the callees
/// that never need the flags on entry, so calls to them need not write the
/// flags back, and the functions whose callers never read the flags they
/// return with (as Delphi's runtime has its callers do), whose returns then
/// skip writing them back too. That holds of almost all compiled C, where
/// it saves the flag write-backs and often the flag computations
/// themselves. A program's functions are called by the program itself or by
/// system code, which never reads the flags after a call. Returns how many
/// functions return without writing the flags back.
///
/// Callees are found from the optimistic start (all of them) by removing
/// those that need the flags on entry until none does, so a function that
/// needs them only through its own callees is removed as well; returns
/// likewise, from all of them flag-free, until no caller reads the flags
/// after a call to a function assumed not to write them back.
pub fn prove_flags_abi(funcs: &mut [Function], reading_slots: HashSet<u64>) -> usize {
    let n = funcs.len();
    let index: HashMap<u64, usize> = funcs
        .iter()
        .enumerate()
        .map(|(i, f)| (f.entry, i))
        .collect();
    // Who calls or jumps to whom, within the module; who leaves through an
    // indirect jump that is not an import stub.
    let mut callers: Vec<Vec<usize>> = vec![vec![]; n];
    let mut exits: Vec<Vec<usize>> = vec![vec![]; n];
    let mut jumps_anywhere = vec![false; n];
    for (i, f) in funcs.iter().enumerate() {
        for (b, blk) in f.blocks.iter().enumerate() {
            match blk.term {
                Term::Call {
                    target: CallTarget::Direct(t),
                    ..
                } => {
                    if let Some(&j) = index.get(&t) {
                        callers[j].push(i);
                    }
                }
                Term::Exit(t) => {
                    if let Some(&j) = index.get(&t) {
                        callers[j].push(i);
                        exits[i].push(j);
                    }
                }
                Term::JmpInd(v) => jumps_anywhere[i] |= opt::slot_of(f, b as BlockId, v).is_none(),
                _ => {}
            }
        }
    }
    let shared = std::sync::Arc::new(FlagFree {
        entries: index.keys().copied().collect(),
        needing: Default::default(),
        reading_slots,
    });
    for f in funcs.iter_mut() {
        f.abi.flags_dead_at_ret = true;
        f.abi.flag_free_callees = FlagFreeCallees::Known(shared.clone());
    }
    let mut need: Vec<Vec<u8>> = vec![vec![]; n];
    let mut queued = vec![true; n];
    let mut work: Vec<usize> = (0..n).collect();
    loop {
        // Callees that need the flags on entry, until none is added.
        while let Some(i) = work.pop() {
            queued[i] = false;
            need[i] = flags_needed(&funcs[i]);
            if need[i][0] != 0 && shared.needing.write().unwrap().insert(funcs[i].entry) {
                for &c in &callers[i] {
                    if !queued[c] {
                        queued[c] = true;
                        work.push(c);
                    }
                }
            }
        }
        // Whose returns are observed: callees of calls whose continuation
        // needs the flags, and, through tail jumps, the functions those
        // leave through. (Calls and jumps through import slots reach other
        // modules, whose returns are not decided here.)
        let mut observed = vec![false; n];
        let mut any = false;
        for (i, f) in funcs.iter().enumerate() {
            for (b, blk) in f.blocks.iter().enumerate() {
                if let Term::Call { target, cont, .. } = &blk.term {
                    if need[i][*cont as usize] == 0 {
                        continue;
                    }
                    match target {
                        CallTarget::Direct(t) => {
                            if let Some(&j) = index.get(t) {
                                observed[j] = true;
                            }
                        }
                        CallTarget::Indirect(v) => {
                            any |= opt::slot_of(f, b as BlockId, *v).is_none();
                        }
                    }
                }
            }
        }
        let mut stack: Vec<usize> = (0..n).filter(|&i| observed[i]).collect();
        while let Some(i) = stack.pop() {
            any |= jumps_anywhere[i];
            for &j in &exits[i] {
                if !observed[j] {
                    observed[j] = true;
                    stack.push(j);
                }
            }
        }
        // A function whose returns now keep the flags may pass its
        // caller's on to its callees: look at it again.
        for (i, f) in funcs.iter_mut().enumerate() {
            if (observed[i] || any) && f.abi.flags_dead_at_ret {
                f.abi.flags_dead_at_ret = false;
                queued[i] = true;
                work.push(i);
            }
        }
        if work.is_empty() {
            return funcs.iter().filter(|f| f.abi.flags_dead_at_ret).count();
        }
    }
}

/// For each block of `f`: which of the lazy flag vregs (bit k for
/// `FLAGS[k]`) the code from its start on reads before writing, itself or
/// by passing them on, unwritten, to code that reads them: a callee or
/// jump target that is not flag-free, or a caller when returns keep the
/// flags (`term_sync`). Faults do not count: the flags are not precise
/// there.
fn flags_needed(f: &Function) -> Vec<u8> {
    let flag_bit = |v: u32| FLAGS.iter().position(|&x| x == v).map(|k| 1u8 << k);
    let all = (1u8 << FLAGS.len()) - 1;
    let flag_mask: opt::StateMask = FLAGS.iter().map(|&v| 1u64 << v).sum();
    let at_term: Vec<u8> = (0..f.blocks.len())
        .map(|b| {
            let t = &f.blocks[b].term;
            let passes = t.syncs()
                && !matches!(t, Term::Fault { .. })
                && opt::term_sync(f, b as BlockId) & flag_mask != 0;
            if passes {
                all
            } else {
                0
            }
        })
        .collect();
    let mut need = vec![0u8; f.blocks.len()];
    let post: Vec<BlockId> = f.rpo().into_iter().rev().collect();
    loop {
        let mut changed = false;
        for &b in &post {
            let blk = &f.blocks[b as usize];
            let mut n = at_term[b as usize];
            // After a call the flags are the callee's.
            if !matches!(blk.term, Term::Call { .. }) {
                for s in blk.term.successors() {
                    n |= need[s as usize];
                }
            }
            for u in blk.term.uses() {
                n |= flag_bit(u).unwrap_or(0);
            }
            for inst in blk.insts.iter().rev() {
                if let Some(bit) = inst.dst.and_then(flag_bit) {
                    let used = n & bit != 0;
                    n &= !bit;
                    // A dead flag computation reads nothing.
                    if !used && !inst.op.has_side_effects() && !inst.op.may_fault() {
                        continue;
                    }
                }
                // Before optimization, condition and EFLAGS reads name the
                // flag state implicitly.
                if matches!(inst.op, Op::Cond(_) | Op::Eflags) {
                    n |= all;
                }
                for u in inst.op.uses() {
                    n |= flag_bit(u).unwrap_or(0);
                }
            }
            if n != need[b as usize] {
                need[b as usize] = n;
                changed = true;
            }
        }
        if !changed {
            return need;
        }
    }
}

/// Translates a single code snippet at `base` (used by the instruction test
/// suite): everything outside the snippet is an exit.
pub fn translate_snippet(code: &[u8], base: u64, cfg: &Config) -> (Function, Vec<(u64, String)>) {
    let src = crate::discover::FlatCode {
        base,
        bytes: code.to_vec(),
    };
    let mut d = Discovery::new_in(cfg.mode());
    d.add_function_seed(base, SeedKind::Profile);
    d.explore(&src);
    let lifted = lift_function(&src, &d, base, &cfg.lift);
    let mut f = lifted.func;
    opt::optimize(&mut f, cfg.opt_level);
    (f, lifted.unsupported)
}

/// Builds a module from already-optimized functions without direct calls
/// between them (their entries may coincide).
pub fn build_module(funcs: &[Function], cfg: &Config) -> Vec<u8> {
    let gen = ModuleGen::new(&cfg.codegen, funcs).without_direct_calls();
    gen.build(funcs, "{}")
}
