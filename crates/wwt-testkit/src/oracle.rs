//! Runs cases on the real CPU through `tools/oracle/oracle.c`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

use crate::case::{hex, mem_diff, Case, Outcome};
use crate::layout::*;

const CASE_SIZE: usize = 16 + 4 + 32 + 4 + 4 + MEM_SIZE + 512;
const RESULT_SIZE: usize = 32 + 4 + 4 + 4 + 4 + MEM_SIZE + 512; // regs, eflags, marker, sig, fault eip

pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Builds the oracle binary (needs `gcc -m32`) and returns its path.
pub fn build_oracle() -> Result<PathBuf> {
    let root = repo_root();
    let src = root.join("tools/oracle/oracle.c");
    let out_dir = root.join("target/oracle");
    std::fs::create_dir_all(&out_dir)?;
    let bin = out_dir.join("oracle");
    let stale = match (std::fs::metadata(&bin), std::fs::metadata(&src)) {
        (Ok(b), Ok(s)) => b.modified()? < s.modified()?,
        _ => true,
    };
    if stale {
        let st = Command::new("gcc")
            .args(["-m32", "-O1", "-no-pie", "-fno-pie", "-o"])
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
        8 => "DE",   // SIGFPE
        4 => "UD",   // SIGILL
        5 => "BP",   // SIGTRAP
        11 | 7 => "AV", // SIGSEGV, SIGBUS
        _ => "??",
    }
}

/// Runs cases natively and returns their outcomes.
pub fn run(cases: &[Case]) -> Result<Vec<Outcome>> {
    let bin = build_oracle()?;
    let mut input = Vec::with_capacity(cases.len() * CASE_SIZE);
    for c in cases {
        let code = c.code_bytes();
        let mut buf = [0u8; 16];
        buf[..code.len()].copy_from_slice(&code);
        input.extend_from_slice(&buf);
        input.extend_from_slice(&(code.len() as u32).to_le_bytes());
        for r in c.regs {
            input.extend_from_slice(&r.to_le_bytes());
        }
        input.extend_from_slice(&c.eflags.to_le_bytes());
        input.extend_from_slice(&0u32.to_le_bytes());
        input.extend_from_slice(&c.window());
        input.extend_from_slice(&c.fx_bytes());
    }
    let mut child = Command::new(&bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("starting oracle")?;
    let mut stdin = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let out = child.wait_with_output()?;
    writer.join().unwrap()?;
    if out.stdout.len() != cases.len() * RESULT_SIZE {
        bail!(
            "oracle returned {} bytes for {} cases (status {})",
            out.stdout.len(),
            cases.len(),
            out.status
        );
    }
    let mut res = vec![];
    for (i, c) in cases.iter().enumerate() {
        let r = &out.stdout[i * RESULT_SIZE..(i + 1) * RESULT_SIZE];
        let u = |o: usize| u32::from_le_bytes(r[o..o + 4].try_into().unwrap());
        let mut regs = [0u32; 8];
        for (k, reg) in regs.iter_mut().enumerate() {
            *reg = u(k * 4);
        }
        let eflags = u(32);
        let marker = u(36);
        let sig = u(40);
        let code_len = c.code_bytes().len() as u32;
        let (eip, fault) = match marker {
            0 => (INS + code_len, String::new()),
            1 => (TGT, String::new()),
            2 => (RET_TGT, String::new()),
            0xff => (0, sig_to_fault(sig).to_string()),
            m => bail!("case {i} ({}): unexpected marker {m:#x}", c.form),
        };
        let mem = &r[48..48 + MEM_SIZE];
        let fx = &r[48 + MEM_SIZE..48 + MEM_SIZE + 512];
        res.push(Outcome {
            regs,
            eflags,
            eip,
            fault,
            mem: mem_diff(&c.window(), mem),
            fx: c.fx.as_ref().map(|_| hex(fx)),
        });
    }
    Ok(res)
}
