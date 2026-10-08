//! Runs cases on the real CPU through `tools/oracle/oracle.c` (32-bit
//! cases) and `tools/oracle/oracle64.c` (x86-64 cases).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

use crate::case::{hex, mem_diff, Case, Outcome};
use crate::layout::*;

const CASE_SIZE: usize = 16 + 4 + 32 + 4 + 4 + MEM_SIZE + 512;
const RESULT_SIZE: usize = 32 + 4 + 4 + 4 + 4 + MEM_SIZE + 512; // regs, eflags, marker, sig, fault eip
/// x86-64: code, length, pad, regs, rflags, window, fx.
const CASE_SIZE64: usize = 16 + 4 + 4 + 128 + 8 + MEM_SIZE + 512;
/// x86-64: regs, rflags, marker, sig, fault rip, window, fx.
const RESULT_SIZE64: usize = 128 + 8 + 4 + 4 + 8 + MEM_SIZE + 512;

pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Builds the oracle binary (needs `gcc -m32`) and returns its path.
pub fn build_oracle() -> Result<PathBuf> {
    build("oracle", &["-m32"])
}

/// Builds the x86-64 oracle binary and returns its path.
pub fn build_oracle64() -> Result<PathBuf> {
    build("oracle64", &[])
}

fn build(name: &str, flags: &[&str]) -> Result<PathBuf> {
    let root = repo_root();
    let src = root.join(format!("tools/oracle/{name}.c"));
    let out_dir = root.join("target/oracle");
    std::fs::create_dir_all(&out_dir)?;
    let bin = out_dir.join(name);
    let stale = match (std::fs::metadata(&bin), std::fs::metadata(&src)) {
        (Ok(b), Ok(s)) => b.modified()? < s.modified()?,
        _ => true,
    };
    if stale {
        let st = Command::new("gcc")
            .args(flags)
            .args(["-O1", "-no-pie", "-fno-pie", "-o"])
            .arg(&bin)
            .arg(&src)
            .status()
            .context("running gcc (install gcc-multilib)")?;
        if !st.success() {
            bail!("building the oracle failed");
        }
    }
    Ok(bin)
}

/// Whether this machine can run the oracle (x86 with 32-bit support).
pub fn oracle_available() -> bool {
    cfg!(any(target_arch = "x86_64", target_arch = "x86")) && build_oracle().is_ok()
}

fn sig_to_fault(sig: u32) -> &'static str {
    match sig {
        8 => "DE",      // SIGFPE
        4 => "UD",      // SIGILL
        5 => "BP",      // SIGTRAP
        11 | 7 => "AV", // SIGSEGV, SIGBUS
        _ => "??",
    }
}

/// Runs cases natively and returns their outcomes.
pub fn run(cases: &[Case]) -> Result<Vec<Outcome>> {
    if cases.iter().any(|c| c.x64 != cases[0].x64) {
        bail!("a batch mixes 32-bit and 64-bit cases");
    }
    if cases.first().is_some_and(|c| c.x64) {
        return run64(cases);
    }
    let bin = build_oracle()?;
    let mut input = Vec::with_capacity(cases.len() * CASE_SIZE);
    for c in cases {
        let code = c.code_bytes();
        let mut buf = [0u8; 16];
        buf[..code.len()].copy_from_slice(&code);
        input.extend_from_slice(&buf);
        input.extend_from_slice(&(code.len() as u32).to_le_bytes());
        for &r in &c.regs {
            input.extend_from_slice(&(r as u32).to_le_bytes());
        }
        input.extend_from_slice(&c.eflags.to_le_bytes());
        input.extend_from_slice(&0u32.to_le_bytes());
        input.extend_from_slice(&c.window());
        input.extend_from_slice(&c.fx_bytes());
    }
    let stdout = run_binary(&bin, input, cases.len() * RESULT_SIZE)?;
    let mut res = vec![];
    for (i, c) in cases.iter().enumerate() {
        let r = &stdout[i * RESULT_SIZE..(i + 1) * RESULT_SIZE];
        let u = |o: usize| u32::from_le_bytes(r[o..o + 4].try_into().unwrap());
        let regs = (0..8).map(|k| u(k * 4) as u64).collect();
        res.push(outcome(i, c, regs, u(32), u(36), u(40), &r[48..])?);
    }
    Ok(res)
}

/// Runs x86-64 cases natively.
fn run64(cases: &[Case]) -> Result<Vec<Outcome>> {
    let bin = build_oracle64()?;
    let mut input = Vec::with_capacity(cases.len() * CASE_SIZE64);
    for c in cases {
        let code = c.code_bytes();
        let mut buf = [0u8; 16];
        buf[..code.len()].copy_from_slice(&code);
        input.extend_from_slice(&buf);
        input.extend_from_slice(&(code.len() as u32).to_le_bytes());
        input.extend_from_slice(&0u32.to_le_bytes());
        for &r in &c.regs {
            input.extend_from_slice(&r.to_le_bytes());
        }
        input.extend_from_slice(&(c.eflags as u64).to_le_bytes());
        input.extend_from_slice(&c.window());
        input.extend_from_slice(&c.fx_bytes());
    }
    let stdout = run_binary(&bin, input, cases.len() * RESULT_SIZE64)?;
    let mut res = vec![];
    for (i, c) in cases.iter().enumerate() {
        let r = &stdout[i * RESULT_SIZE64..(i + 1) * RESULT_SIZE64];
        let u = |o: usize| u32::from_le_bytes(r[o..o + 4].try_into().unwrap());
        let u64_at = |o: usize| u64::from_le_bytes(r[o..o + 8].try_into().unwrap());
        let regs = (0..16).map(|k| u64_at(k * 8)).collect();
        res.push(outcome(i, c, regs, u(128), u(136), u(140), &r[152..])?);
    }
    Ok(res)
}

/// Feeds `input` to an oracle binary and returns its output.
fn run_binary(bin: &Path, input: Vec<u8>, expect: usize) -> Result<Vec<u8>> {
    let mut child = Command::new(bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("starting oracle")?;
    let mut stdin = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let out = child.wait_with_output()?;
    writer.join().unwrap()?;
    if out.stdout.len() != expect {
        bail!(
            "oracle returned {} bytes, expected {expect} (status {})",
            out.stdout.len(),
            out.status
        );
    }
    Ok(out.stdout)
}

/// Builds an outcome from an oracle result; `tail` holds the memory window
/// followed by the FXSAVE image.
fn outcome(
    i: usize,
    c: &Case,
    regs: Vec<u64>,
    eflags: u32,
    marker: u32,
    sig: u32,
    tail: &[u8],
) -> Result<Outcome> {
    let code_len = c.code_bytes().len() as u32;
    let (eip, fault) = match marker {
        0 => (INS + code_len, String::new()),
        1 => (TGT, String::new()),
        2 => (RET_TGT, String::new()),
        0xff => (0, sig_to_fault(sig).to_string()),
        m => bail!("case {i} ({}): unexpected marker {m:#x}", c.form),
    };
    let mem = &tail[..MEM_SIZE];
    let fx = &tail[MEM_SIZE..MEM_SIZE + 512];
    Ok(Outcome {
        regs,
        eflags,
        eip,
        fault,
        mem: mem_diff(&c.window(), mem),
        fx: c.fx.as_ref().map(|_| hex(fx)),
    })
}
