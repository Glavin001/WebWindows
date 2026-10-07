//! `wwt`: the local command-line toolkit that turns a Windows .exe into a
//! web app.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use wwt::pe::PeFile;
use wwt::Config;

#[derive(Parser)]
#[command(
    name = "wwt",
    version,
    about = "Translate 32-bit and 64-bit Windows programs to WebAssembly"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(clap::Args, Clone)]
struct TranslateOpts {
    /// Optimization level (0 = fast mode, 1 = full).
    #[arg(short = 'O', long, default_value_t = 1)]
    opt: u32,
    /// Disable null-region and guest-limit checks.
    #[arg(long)]
    no_mem_checks: bool,
    /// Disable self-modifying code checks.
    #[arg(long)]
    no_smc_checks: bool,
    /// Make all guest memory accesses atomic (strict ordering).
    #[arg(long)]
    strict_ordering: bool,
    /// Extra function entry points (hex), e.g. from a run-time profile.
    #[arg(long, value_delimiter = ',')]
    seed: Vec<String>,
    /// Profile file with one hex address per line (written by the runtime).
    #[arg(long)]
    profile: Option<PathBuf>,
    /// Target a 64-bit (memory64) WebAssembly memory.
    #[arg(long)]
    mem64: bool,
    /// Load (and translate) the image at this base (hex) instead of its
    /// preferred one. Without `--mem64`, 64-bit images preferred above 4 GB
    /// move below it.
    #[arg(long)]
    base: Option<String>,
}

impl TranslateOpts {
    fn config(&self) -> Config {
        let mut c = Config::default();
        c.opt_level = self.opt;
        c.lift.mem_checks = !self.no_mem_checks;
        c.codegen.mem_checks = !self.no_mem_checks;
        c.lift.smc_checks = !self.no_smc_checks;
        c.codegen.smc_checks = !self.no_smc_checks;
        c.lift.strict_ordering = self.strict_ordering;
        c.with_mem64(self.mem64)
    }

    /// Loads a PE file, moved to `--base` when given.
    fn load(&self, file: &Path) -> Result<PeFile> {
        let mut pe = load(file)?;
        if let Some(b) = &self.base {
            pe.rebase(parse_hex(b)?)?;
        }
        Ok(pe)
    }

    fn seeds(&self) -> Result<Vec<u64>> {
        let mut out = vec![];
        for s in &self.seed {
            out.push(parse_hex(s)?);
        }
        if let Some(p) = &self.profile {
            if p.exists() {
                for line in std::fs::read_to_string(p)?.lines() {
                    let l = line.trim();
                    if !l.is_empty() && !l.starts_with('#') {
                        out.push(parse_hex(l)?);
                    }
                }
            }
        }
        Ok(out)
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// Show the PE file's sections, imports, exports and entry point.
    Info { file: PathBuf },
    /// Translate a PE file to a WebAssembly module.
    Translate {
        file: PathBuf,
        /// Output path (default: <file>.wasm next to the input).
        #[arg(short, long)]
        out: Option<PathBuf>,
        #[command(flatten)]
        opts: TranslateOpts,
        /// Print the translation report as JSON.
        #[arg(long)]
        report: bool,
        /// Run Binaryen's wasm-opt over the result if it is installed.
        #[arg(long)]
        wasm_opt: bool,
    },
    /// Print the optimized IR of one function (or all).
    Ir {
        file: PathBuf,
        /// Function address (hex); all functions when omitted.
        #[arg(long)]
        func: Option<String>,
        #[command(flatten)]
        opts: TranslateOpts,
    },
    /// Print the generated WebAssembly as text.
    Wat {
        file: PathBuf,
        #[command(flatten)]
        opts: TranslateOpts,
    },
    /// Disassemble discovered functions.
    Disasm {
        file: PathBuf,
        #[arg(long)]
        func: Option<String>,
    },
    /// Write the runtime kernel module.
    Kernel {
        #[arg(short, long)]
        out: PathBuf,
        /// The kernel for a 64-bit (memory64) memory.
        #[arg(long)]
        mem64: bool,
        /// The kernel for x86-64 code on a 64-bit memory (64-bit code
        /// addresses; implies --mem64).
        #[arg(long)]
        code64: bool,
    },
    /// Print the runtime ABI (CPU struct layout and constants) as JSON.
    Abi,
    /// Package an .exe as a static web app directory: translated module,
    /// runtime kernel, ABI and the JavaScript runtime.
    Pack {
        file: PathBuf,
        #[arg(short, long)]
        out: PathBuf,
        #[command(flatten)]
        opts: TranslateOpts,
    },
}

fn parse_hex(s: &str) -> Result<u64> {
    let t = s.trim().trim_start_matches("0x").trim_start_matches("0X");
    u64::from_str_radix(t, 16).with_context(|| format!("bad hex address {s:?}"))
}

fn load(file: &Path) -> Result<PeFile> {
    let data = std::fs::read(file).with_context(|| format!("reading {}", file.display()))?;
    PeFile::parse(data).with_context(|| format!("parsing {}", file.display()))
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Info { file } => {
            let pe = load(&file)?;
            println!("machine      {:#06x} ({:?})", pe.machine, pe.mode);
            if pe.preferred_base != pe.image_base as u64 {
                println!("preferred    {:#x} (loads below 4 GB)", pe.preferred_base);
            }
            println!("image base   {:#010x}", pe.image_base);
            println!("entry point  {:#010x?}", pe.entry_point());
            println!("size         {:#x}", pe.size_of_image);
            println!("dll          {}", pe.is_dll());
            println!("subsystem    {}", pe.subsystem);
            println!(
                "relocations  {}",
                pe.relocations
                    .as_ref()
                    .map_or("none".into(), |r| r.len().to_string())
            );
            println!("sections:");
            for s in &pe.sections {
                println!(
                    "  {:8} rva {:#08x} size {:#08x} raw {:#08x} flags {:#010x}",
                    s.name,
                    s.virtual_address,
                    s.mem_size(),
                    s.raw_size,
                    s.characteristics
                );
            }
            println!("imports:");
            for i in &pe.imports {
                println!(
                    "  {}!{} @ iat {:#x}",
                    i.dll,
                    i.name,
                    pe.image_base + i.iat_rva as u64
                );
            }
            if !pe.exports.is_empty() {
                println!("exports:");
                for e in &pe.exports {
                    println!("  #{} {:?} {:?}", e.ordinal, e.name, e.target);
                }
            }
            if let Some(tls) = &pe.tls {
                println!("tls callbacks: {:x?}", tls.callbacks);
            }
            if !pe.pdata.is_empty() {
                println!(".pdata functions: {}", pe.pdata.len());
            }
        }
        Cmd::Translate {
            file,
            out,
            opts,
            report,
            wasm_opt,
        } => {
            let pe = opts.load(&file)?;
            let t = wwt::translate_pe(&pe, &opts.config(), &opts.seeds()?)?;
            let out = out.unwrap_or_else(|| file.with_extension("wasm"));
            std::fs::write(&out, &t.wasm)?;
            if wasm_opt {
                run_wasm_opt(&out)?;
            }
            eprintln!(
                "{}: {} functions, {} x86 instructions, {} bytes of wasm, {} unsupported",
                out.display(),
                t.report.functions,
                t.report.x86_instructions,
                std::fs::metadata(&out)?.len(),
                t.report.unsupported.len()
            );
            if report {
                println!("{}", serde_json::to_string_pretty(&t.report)?);
            } else {
                for (a, s) in t.report.unsupported.iter().take(20) {
                    eprintln!("  unsupported at {a:#x}: {s}");
                }
            }
        }
        Cmd::Ir { file, func, opts } => {
            let pe = opts.load(&file)?;
            let t = wwt::translate_pe(&pe, &opts.config(), &opts.seeds()?)?;
            let want = func.map(|f| parse_hex(&f)).transpose()?;
            for f in &t.ir {
                if want.is_none_or(|w| w == f.entry) {
                    println!("{f}");
                }
            }
        }
        Cmd::Wat { file, opts } => {
            let pe = opts.load(&file)?;
            let t = wwt::translate_pe(&pe, &opts.config(), &opts.seeds()?)?;
            println!("{}", wasmprinter::print_bytes(&t.wasm)?);
        }
        Cmd::Disasm { file, func } => {
            let pe = load(&file)?;
            let img = pe.image()?;
            let d = wwt::translate::discover_pe(&pe, &img, &[], &Config::default());
            let want = func.map(|f| parse_hex(&f)).transpose()?;
            let mut addrs: Vec<u64> = d.insts.keys().copied().collect();
            addrs.sort_unstable();
            let mut fmt = iced_x86::IntelFormatter::new();
            use iced_x86::Formatter;
            for a in addrs {
                if let Some(w) = want {
                    if a < w {
                        continue;
                    }
                }
                if d.functions.contains(&a) {
                    println!("\nfunction {a:#010x}:");
                } else if d.leaders.contains(&a) {
                    println!("  {a:#010x}:");
                }
                let mut s = String::new();
                fmt.format(&d.insts[&a], &mut s);
                println!("    {a:08x}  {s}");
            }
        }
        Cmd::Kernel { out, mem64, code64 } => std::fs::write(
            out,
            if code64 {
                wwt::kernel::kernel_code64_wasm()
            } else if mem64 {
                wwt::kernel::kernel64_wasm()
            } else {
                wwt::kernel::kernel_wasm()
            },
        )?,
        Cmd::Abi => println!("{}", wwt::abi::abi_json()),
        Cmd::Pack { file, out, opts } => pack(&file, &out, &opts)?,
    }
    Ok(())
}

fn run_wasm_opt(path: &Path) -> Result<()> {
    let st = std::process::Command::new("wasm-opt")
        .args([
            "-O2",
            "--enable-threads",
            "--enable-tail-call",
            "--enable-bulk-memory",
            "--enable-simd",
            "--enable-sign-ext",
            "--enable-nontrapping-float-to-int",
            "--enable-mutable-globals",
        ])
        .arg(path)
        .arg("-o")
        .arg(path)
        .status()
        .context("running wasm-opt")?;
    if !st.success() {
        bail!("wasm-opt failed");
    }
    Ok(())
}

/// Writes a static web app: index.html, the runtime, kernel, ABI and the
/// translated module, plus the original .exe (needed for its data).
fn pack(file: &Path, out: &Path, opts: &TranslateOpts) -> Result<()> {
    std::fs::create_dir_all(out)?;
    let pe = opts.load(file)?;
    let t = wwt::translate_pe(&pe, &opts.config(), &opts.seeds()?)?;
    let name = file
        .file_name()
        .context("input has no file name")?
        .to_string_lossy()
        .into_owned();
    std::fs::write(out.join(format!("{name}.wasm")), &t.wasm)?;
    std::fs::copy(file, out.join(&name))?;
    let kernel = if opts.mem64 && pe.mode == wwt::ir::Mode::X64 {
        wwt::kernel::kernel_code64_wasm()
    } else if opts.mem64 {
        wwt::kernel::kernel64_wasm()
    } else {
        wwt::kernel::kernel_wasm()
    };
    std::fs::write(out.join("kernel.wasm"), kernel)?;
    std::fs::write(out.join("abi.json"), wwt::abi::abi_json())?;
    // Copy the JavaScript runtime that ships next to the CLI.
    let runtime = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../runtime");
    for f in [
        "runtime.mjs",
        "win32.mjs",
        "pe.mjs",
        "index.html",
        "worker.mjs",
    ] {
        let src = runtime.join(f);
        if src.exists() {
            std::fs::copy(&src, out.join(f))?;
        }
    }
    std::fs::write(
        out.join("app.json"),
        serde_json::to_string_pretty(&serde_json::json!({ "exe": name }))?,
    )?;
    eprintln!("packed {} into {}", name, out.display());
    Ok(())
}
