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
use crate::discover::{scan_data_for_code_pointers, CodeSource, Discovery, SeedKind};
use crate::ir::{BlockId, CallAbi, CallTarget, FlagFree, FlagFreeCallees, Function, Op, Term};
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
}

#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub functions: usize,
    pub blocks: usize,
    pub x86_instructions: usize,
    pub wasm_bytes: usize,
    pub seeds: std::collections::BTreeMap<String, usize>,
    /// Instructions the lifter could not translate: (address, text).
    pub unsupported: Vec<(u32, String)>,
    /// Indirect jumps recognized as jump tables.
    pub jump_tables: usize,
    /// Functions shown to return without their callers reading the flags
    /// (`prove_flags_abi`).
    pub flag_free_returns: usize,
}

pub struct Translation {
    pub wasm: Vec<u8>,
    /// Translated function entry points, in table order.
    pub entries: Vec<u32>,
    pub report: Report,
    /// The IR of each function after optimization (for inspection).
    pub ir: Vec<Function>,
}

/// Image description stored in the module so the runtime can load the PE
/// without parsing it again.
#[derive(Serialize)]
pub struct ImageMeta {
    pub image_base: u32,
    pub size_of_image: u32,
    pub size_of_headers: u32,
    pub entry: Option<u32>,
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
        image_base: pe.image_base,
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

/// Discovers code in a PE image from all static seeds plus `profile` (code
/// found at run time on earlier launches).
pub fn discover_pe(pe: &PeFile, img: &dyn CodeSource, profile: &[u32], cfg: &Config) -> Discovery {
    let mut d = Discovery::new();
    if let Some(e) = pe.entry_point() {
        d.add_function_seed(e, SeedKind::Entry);
    }
    for ex in &pe.exports {
        if let ExportTarget::Rva(rva) = ex.target {
            let va = pe.image_base + rva;
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
    d.explore(img);
    // Code pointers in data: relocation targets (always present in DLLs),
    // otherwise a scan of data sections. Addresses already known as
    // instruction starts are reached some other way (jump tables, calls).
    let mut candidates: BTreeSet<(u32, SeedKind)> = BTreeSet::new();
    if let Some(relocs) = &pe.relocations {
        for &r in relocs {
            if let Some(v) = img.read_u32(pe.image_base + r) {
                // Skip values inside code that are operands of instructions in
                // code (e.g. `push offset label`), unless they point at code.
                if img.is_code(v) {
                    candidates.insert((v, SeedKind::Relocation));
                }
            }
        }
    }
    // Executables often carry few or no relocations: also scan their data
    // for code pointers (each confirmed by decoding).
    if cfg.scan_data && !pe.is_dll() {
        for s in &pe.sections {
            if s.is_executable() || s.characteristics & 0xC0 == 0x80 {
                continue;
            }
            let start = pe.image_base + s.virtual_address;
            let end = start + s.raw_size.min(s.mem_size());
            for v in scan_data_for_code_pointers(img, start, end) {
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
    let setjmp_slots: Vec<u32> = pe
        .imports
        .iter()
        .filter(|i| {
            let n = i.name.to_string().to_ascii_lowercase();
            n.contains("setjmp")
        })
        .map(|i| pe.image_base + i.iat_rva)
        .collect();
    if !setjmp_slots.is_empty() {
        use iced_x86::{FlowControl, OpKind, Register};
        let via_slot = |i: &iced_x86::Instruction| {
            i.op0_kind() == OpKind::Memory
                && i.memory_base() == Register::None
                && i.memory_index() == Register::None
                && setjmp_slots.contains(&i.memory_displacement32())
        };
        // Import stubs: `jmp [slot]` (how MinGW calls undeclared imports).
        let stubs: Vec<u32> = d
            .insts
            .values()
            .filter(|i| i.flow_control() == FlowControl::IndirectBranch && via_slot(i))
            .map(|i| i.ip32())
            .collect();
        let returns: Vec<u32> = d
            .insts
            .values()
            .filter(|i| {
                (i.flow_control() == FlowControl::IndirectCall && via_slot(i))
                    || (i.flow_control() == FlowControl::Call
                        && i.op0_kind() == OpKind::NearBranch32
                        && stubs.contains(&i.near_branch32()))
            })
            .map(|i| i.next_ip32())
            .collect();
        for r in returns {
            d.add_function_seed(r, SeedKind::Call);
        }
        d.explore(img);
    }
    d
}

/// Translates a PE file ahead of time.
pub fn translate_pe(pe: &PeFile, cfg: &Config, profile: &[u32]) -> Result<Translation> {
    let mut cfg = cfg.clone();
    match cfg.c_abi {
        Some(true) => cfg.lift.abi = CallAbi::c(),
        None if pe.is_wine_builtin() => cfg.lift.abi = CallAbi::c(),
        None => cfg.prove_flags_abi = !pe.is_dll(),
        Some(false) => {}
    }
    cfg.lift.define_all_flags = cfg.lift.abi.flags_dead_at_ret || cfg.prove_flags_abi;
    let cfg = &cfg;
    let img = pe.image()?;
    let mut d = discover_pe(pe, &img, profile, cfg);
    let meta = image_meta(pe);
    let names: HashMap<u32, String> = pe.code_names().into_iter().collect();
    translate_discovered(&img, &mut d, cfg, Some(&meta), &names)
}

/// Fast mode: translates code reachable from `entries` in `src`.
pub fn translate_region(
    src: &dyn CodeSource,
    entries: &[u32],
    cfg: &Config,
) -> Result<Translation> {
    translate_region_with_known(src, entries, &[], cfg)
}

/// Fast mode with a set of function entries already translated elsewhere,
/// which are called through the lookup rather than translated again.
pub fn translate_region_with_known(
    src: &dyn CodeSource,
    entries: &[u32],
    known: &[u32],
    cfg: &Config,
) -> Result<Translation> {
    let mut d = Discovery::new();
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
    translate_discovered(src, &mut d, cfg, None, &HashMap::new())
}

/// Lifts, optimizes and generates code for every discovered function.
/// `names` (symbols by address) only label functions in the module's name
/// section, for profilers and debuggers.
pub fn translate_discovered(
    src: &dyn CodeSource,
    d: &mut Discovery,
    cfg: &Config,
    meta: Option<&ImageMeta>,
    names: &HashMap<u32, String>,
) -> Result<Translation> {
    let mut report = Report::default();
    for k in d.seed_kinds.values() {
        *report.seeds.entry(format!("{k:?}")).or_default() += 1;
    }
    report.jump_tables = d.jump_tables.len();
    report.x86_instructions = d.insts.len();
    let mut funcs: Vec<Function> = vec![];
    let mut done: BTreeSet<u32> = BTreeSet::new();
    loop {
        let todo: Vec<u32> = d
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
            let builtin = names
                .get(&entry)
                .and_then(|n| crate::builtin::Builtin::by_name(n))
                .filter(|_| cfg.builtins);
            let f = match builtin {
                Some(b) => crate::builtin::body(entry, b),
                None => {
                    let lifted = lift_function(src, d, entry, &cfg.lift);
                    report.unsupported.extend(lifted.unsupported);
                    for e in lifted.extra_entries {
                        d.add_function_seed(e, SeedKind::Call);
                    }
                    lifted.func
                }
            };
            funcs.push(f);
        }
    }
    if cfg.prove_flags_abi {
        // Imports: Wine's exports never read the flags on entry, except
        // `_chkesp` (MSVC debug builds' stack check after calls).
        let reading = meta
            .map(|m| {
                m.imports
                    .iter()
                    .filter(|i| i.name.to_string().contains("chkesp"))
                    .map(|i| m.image_base + i.iat_rva)
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
    let entries: Vec<u32> = funcs.iter().map(|f| f.entry).collect();
    let meta_json = serde_json::to_string(&Meta {
        abi_version: abi::ABI_VERSION,
        guest_limit: cfg.codegen.guest_limit,
        image: meta,
        report: &report,
    })?;
    let gen = ModuleGen::new(&cfg.codegen, &funcs).with_names(names);
    let wasm = gen.build(&funcs, &meta_json);
    report.wasm_bytes = wasm.len();
    Ok(Translation {
        wasm,
        entries,
        report,
        ir: funcs,
    })
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
pub fn prove_flags_abi(funcs: &mut [Function], reading_slots: HashSet<u32>) -> usize {
    let index: HashMap<u32, usize> = funcs
        .iter()
        .enumerate()
        .map(|(i, f)| (f.entry, i))
        .collect();
    let mut dead = vec![true; funcs.len()];
    loop {
        for (f, &d) in funcs.iter_mut().zip(&dead) {
            f.abi.flags_dead_at_ret = d;
        }
        flag_free_callees(funcs, &index, &reading_slots);
        // Whose returns are observed: callees of calls whose continuation
        // needs the flags, and (through tail jumps) the callees of those.
        let mut observed = vec![false; funcs.len()];
        let mut any_indirect = false;
        // (Calls and jumps through import slots reach other modules, whose
        // returns are not decided here.)
        for (i, f) in funcs.iter().enumerate() {
            let need = flags_needed(f);
            for (b, blk) in f.blocks.iter().enumerate() {
                let b = b as BlockId;
                match &blk.term {
                    Term::Call { target, cont, .. } if need[*cont as usize] != 0 => match target {
                        CallTarget::Direct(t) => {
                            if let Some(&j) = index.get(t) {
                                observed[j] = true;
                            }
                        }
                        CallTarget::Indirect(v) => {
                            any_indirect |= opt::slot_of(f, b, *v).is_none();
                        }
                    },
                    Term::Exit(t) if !dead[i] => {
                        if let Some(&j) = index.get(t) {
                            observed[j] = true;
                        }
                    }
                    Term::JmpInd(v) if !dead[i] => {
                        any_indirect |= opt::slot_of(f, b, *v).is_none();
                    }
                    _ => {}
                }
            }
        }
        let next: Vec<bool> = observed.iter().map(|&o| !o && !any_indirect).collect();
        if next == dead {
            return dead.iter().filter(|&&d| d).count();
        }
        dead = next;
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

/// Finds the callees that do not need the flags on entry, given each
/// function's `flags_dead_at_ret` (see `prove_flags_abi`).
fn flag_free_callees(funcs: &mut [Function], index: &HashMap<u32, usize>, slots: &HashSet<u32>) {
    // Whether a function needs the flags on entry depends on which of the
    // functions it calls or jumps to do.
    let mut callers: Vec<Vec<usize>> = vec![vec![]; funcs.len()];
    for (i, f) in funcs.iter().enumerate() {
        for b in &f.blocks {
            if let Term::Call {
                target: CallTarget::Direct(t),
                ..
            }
            | Term::Exit(t) = b.term
            {
                if let Some(&j) = index.get(&t) {
                    callers[j].push(i);
                }
            }
        }
    }
    let mut free: HashSet<u32> = index.keys().copied().collect();
    let mut todo: Vec<usize> = (0..funcs.len()).collect();
    while !todo.is_empty() {
        let set = std::sync::Arc::new(FlagFree {
            entries: free.clone(),
            reading_slots: slots.clone(),
        });
        let mut removed = vec![];
        for &i in &todo {
            let f = &mut funcs[i];
            f.abi.flag_free_callees = FlagFreeCallees::Known(set.clone());
            if flags_needed(f)[0] != 0 {
                removed.push(i);
            }
        }
        for &i in &removed {
            free.remove(&funcs[i].entry);
        }
        // Only the callers of what was removed can change.
        let mut next: Vec<usize> = removed
            .iter()
            .flat_map(|&i| callers[i].iter().copied())
            .filter(|&c| free.contains(&funcs[c].entry))
            .collect();
        next.sort_unstable();
        next.dedup();
        todo = next;
    }
    let set = std::sync::Arc::new(FlagFree {
        entries: free,
        reading_slots: slots.clone(),
    });
    for f in funcs.iter_mut() {
        f.abi.flag_free_callees = FlagFreeCallees::Known(set.clone());
    }
}

/// Translates a single code snippet at `base` (used by the instruction test
/// suite): everything outside the snippet is an exit.
pub fn translate_snippet(code: &[u8], base: u32, cfg: &Config) -> (Function, Vec<(u32, String)>) {
    let src = crate::discover::FlatCode {
        base,
        bytes: code.to_vec(),
    };
    let mut d = Discovery::new();
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
