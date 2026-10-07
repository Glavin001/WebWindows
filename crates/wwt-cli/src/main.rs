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
    about = "Translate 32-bit Windows programs to WebAssembly"
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
    /// Guest limit (MB) the module will run under: memory checks compare
    /// against a constant, which is faster, and the runtime refuses the
    /// module under any other limit. Without it, the module reads the limit
    /// at run time.
    #[arg(long)]
    guest_limit_mb: Option<u32>,
    /// Do not inline small leaf functions into their callers.
    #[arg(long)]
    no_inline: bool,
    /// Translate exported memory functions instead of using built-ins.
    #[arg(long)]
    no_builtins: bool,
    /// No re-entry at loop headers (see `wwt::osr`).
    #[arg(long)]
    no_osr: bool,
    /// Whether the image is compiled C, whose code never reads the flags
    /// across calls and returns, so they need not be written back there:
    /// `auto` (Wine's own modules), `on` or `off`.
    #[arg(long, default_value = "auto", value_parser = ["auto", "on", "off"])]
    c_abi: String,
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
        c.codegen.guest_limit = self.guest_limit_mb.map(|mb| mb << 20);
        c.inline = !self.no_inline;
        c.builtins = !self.no_builtins;
        c.codegen.osr = !self.no_osr;
        c.c_abi = match self.c_abi.as_str() {
            "on" => Some(true),
            "off" => Some(false),
            _ => None,
        };
        c
    }

    fn seeds(&self) -> Result<Vec<u32>> {
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
    /// Print the generated WebAssembly as text (or, given a .wasm file,
    /// that module, e.g. an Emscripten build to compare against).
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

fn parse_hex(s: &str) -> Result<u32> {
    let t = s.trim().trim_start_matches("0x").trim_start_matches("0X");
    u32::from_str_radix(t, 16).with_context(|| format!("bad hex address {s:?}"))
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
                    pe.image_base + i.iat_rva
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
        }
        Cmd::Translate {
            file,
            out,
            opts,
            report,
            wasm_opt,
        } => {
            let pe = load(&file)?;
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
            let pe = load(&file)?;
            let t = wwt::translate_pe(&pe, &opts.config(), &opts.seeds()?)?;
            let want = func.map(|f| parse_hex(&f)).transpose()?;
            for f in &t.ir {
                if want.is_none_or(|w| w == f.entry) {
                    println!("{f}");
                }
            }
        }
        Cmd::Wat { file, opts } => {
            let bytes = std::fs::read(&file)?;
            if bytes.starts_with(b"\0asm") {
                println!("{}", wasmprinter::print_bytes(&bytes)?);
                return Ok(());
            }
            let pe = load(&file)?;
            let t = wwt::translate_pe(&pe, &opts.config(), &opts.seeds()?)?;
            println!("{}", wasmprinter::print_bytes(&t.wasm)?);
        }
        Cmd::Disasm { file, func } => {
            let pe = load(&file)?;
            let img = pe.image()?;
            let d = wwt::translate::discover_pe(&pe, &img, &[], &Config::default());
            let want = func.map(|f| parse_hex(&f)).transpose()?;
            let mut addrs: Vec<u32> = d.insts.keys().copied().collect();
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
        Cmd::Kernel { out } => std::fs::write(out, wwt::kernel::kernel_wasm())?,
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
    let pe = load(file)?;
    let t = wwt::translate_pe(&pe, &opts.config(), &opts.seeds()?)?;
    let name = file
        .file_name()
        .context("input has no file name")?
        .to_string_lossy()
        .into_owned();
    std::fs::write(out.join(format!("{name}.wasm")), &t.wasm)?;
    std::fs::copy(file, out.join(&name))?;
    std::fs::write(out.join("kernel.wasm"), wwt::kernel::kernel_wasm())?;
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
