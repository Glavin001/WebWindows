//! Layer 7 — Orchestration, metadata and caching.
//!
//! [`translate_pe`] runs the whole pipeline over a PE file ahead of time;
//! [`translate_region`] is the fast mode used for code discovered while the
//! program runs. Both produce one WebAssembly module whose functions are
//! registered under their x86 addresses at load time.

use std::collections::BTreeSet;

use anyhow::Result;
use serde::Serialize;

use crate::abi;
use crate::codegen::{CodegenConfig, ModuleGen};
use crate::discover::{scan_data_for_code_pointers_in, CodeSource, Discovery, SeedKind};
use crate::ir::{Function, Mode};
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
}

impl Default for Config {
    fn default() -> Self {
        Config {
            lift: LiftConfig::default(),
            codegen: CodegenConfig::default(),
            opt_level: 1,
            scan_data: true,
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
    pub unsupported: Vec<(u32, String)>,
    /// Indirect jumps recognized as jump tables.
    pub jump_tables: usize,
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
    /// x86 or x86-64; the runtime picks the CPU layout from it.
    pub mode: Mode,
    pub machine: u16,
    /// The base the translation was made for (where the image must load).
    pub image_base: u32,
    /// The base in the file (differs for 64-bit images moved below 4 GB).
    pub preferred_base: u64,
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

/// Discovers code in a PE image from all static seeds plus `profile` (code
/// found at run time on earlier launches).
pub fn discover_pe(pe: &PeFile, img: &dyn CodeSource, profile: &[u32], cfg: &Config) -> Discovery {
    let mut d = Discovery::new_in(pe.mode);
    if let Some(e) = pe.entry_point() {
        d.add_function_seed(e, SeedKind::Entry);
    }
    // x86-64: every function that calls or allocates stack has a `.pdata`
    // entry.
    for &rva in &pe.pdata {
        let va = pe.image_base + rva;
        if img.is_code(va) {
            d.add_function_seed(va, SeedKind::Pdata);
        }
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
        for (k, &r) in relocs.iter().enumerate() {
            let site = pe.image_base + r;
            let v = if pe.reloc_types.get(k) == Some(&crate::pe::REL_DIR64) {
                img.read_u64(site)
                    .filter(|v| v >> 32 == 0)
                    .map(|v| v as u32)
            } else {
                img.read_u32(site)
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
    // Executables often carry few or no relocations: also scan their data
    // for code pointers (each confirmed by decoding).
    if cfg.scan_data && !pe.is_dll() {
        for s in &pe.sections {
            if s.is_executable() || s.characteristics & 0xC0 == 0x80 {
                continue;
            }
            let start = pe.image_base + s.virtual_address;
            let end = start + s.raw_size.min(s.mem_size());
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
        // `[slot]`, or `[rip+slot]` in 64-bit code.
        let via_slot = |i: &iced_x86::Instruction| {
            i.op0_kind() == OpKind::Memory
                && matches!(i.memory_base(), Register::None | Register::RIP)
                && i.memory_index() == Register::None
                && i.memory_displacement64() >> 32 == 0
                && setjmp_slots.contains(&(i.memory_displacement64() as u32))
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
                        && crate::lift::branch_target(i, 0).is_some_and(|t| stubs.contains(&t)))
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
/// Translates a PE file ahead of time. The mode (x86 or x86-64) comes from
/// the file.
pub fn translate_pe(pe: &PeFile, cfg: &Config, profile: &[u32]) -> Result<Translation> {
    let cfg = &cfg.clone().with_mode(pe.mode);
    let img = pe.image()?;
    let mut d = discover_pe(pe, &img, profile, cfg);
    let meta = image_meta(pe);
    translate_discovered(&img, &mut d, cfg, Some(&meta))
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
    translate_discovered(src, &mut d, cfg, None)
}

/// Lifts, optimizes and generates code for every discovered function.
pub fn translate_discovered(
    src: &dyn CodeSource,
    d: &mut Discovery,
    cfg: &Config,
    meta: Option<&ImageMeta>,
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
            let lifted = lift_function(src, d, entry, &cfg.lift);
            report.unsupported.extend(lifted.unsupported);
            for e in lifted.extra_entries {
                d.add_function_seed(e, SeedKind::Call);
            }
            let mut f = lifted.func;
            opt::optimize(&mut f, cfg.opt_level);
            report.blocks += f.blocks.len();
            funcs.push(f);
        }
    }
    funcs.sort_by_key(|f| f.entry);
    report.functions = funcs.len();
    report.unsupported.sort();
    report.unsupported.dedup();
    let entries: Vec<u32> = funcs.iter().map(|f| f.entry).collect();
    let meta_json = serde_json::to_string(&Meta {
        abi_version: abi::ABI_VERSION,
        image: meta,
        report: &report,
    })?;
    let gen = ModuleGen::new(&cfg.codegen, &funcs);
    let wasm = gen.build(&funcs, &meta_json);
    report.wasm_bytes = wasm.len();
    Ok(Translation {
        wasm,
        entries,
        report,
        ir: funcs,
    })
}

/// Translates a single code snippet at `base` (used by the instruction test
/// suite): everything outside the snippet is an exit.
pub fn translate_snippet(code: &[u8], base: u32, cfg: &Config) -> (Function, Vec<(u32, String)>) {
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
