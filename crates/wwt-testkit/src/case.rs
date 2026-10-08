//! Test cases, outcomes and the fixture format.

use serde::{Deserialize, Serialize};

use crate::layout::*;

/// One instruction with its inputs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Case {
    /// iced-x86 `Code` name (the instruction form).
    pub form: String,
    /// Instruction bytes, hex.
    pub code: String,
    /// x86-64 code: sixteen 64-bit registers (rax..rdi, r8..r15) instead of
    /// eight 32-bit ones.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub x64: bool,
    /// General registers in encoding order.
    pub regs: Vec<u64>,
    pub eflags: u32,
    /// Seed for the memory window contents.
    pub mem_seed: u64,
    /// 32-bit values written into the window after filling: (offset, value).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mem_patch: Vec<(u32, u32)>,
    /// x87/SSE input state (FXSAVE image), hex; default state when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fx: Option<String>,
}

/// What happened after running a case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    pub regs: Vec<u64>,
    pub eflags: u32,
    /// Address execution continued at (0 when a fault was raised).
    pub eip: u32,
    /// Fault kind: "DE" (divide error), "UD", "BP", "AV", or empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fault: String,
    /// Bytes of the window that differ from the input: (offset, byte).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mem: Vec<(u32, u8)>,
    /// x87/SSE output state, hex (only for FPU/SIMD cases).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fx: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fixture {
    #[serde(flatten)]
    pub case: Case,
    pub out: Outcome,
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

/// xorshift64* — the memory window contents derive from the case seed.
pub fn fill_window(seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    let mut out = Vec::with_capacity(MEM_SIZE);
    while out.len() < MEM_SIZE {
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        let v = x.wrapping_mul(0x2545_F491_4F6C_DD1D);
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.truncate(MEM_SIZE);
    out
}

impl Case {
    /// Decoder bitness for the case's code.
    pub fn bitness(&self) -> u32 {
        if self.x64 {
            64
        } else {
            32
        }
    }

    pub fn code_bytes(&self) -> Vec<u8> {
        unhex(&self.code)
    }

    /// The initial memory window.
    pub fn window(&self) -> Vec<u8> {
        let mut w = fill_window(self.mem_seed);
        for &(off, v) in &self.mem_patch {
            w[off as usize..off as usize + 4].copy_from_slice(&v.to_le_bytes());
        }
        w
    }

    pub fn fx_bytes(&self) -> Vec<u8> {
        match &self.fx {
            Some(h) => unhex(h),
            None => default_fx(),
        }
    }
}

/// FXSAVE image after `fninit` with the default MXCSR.
pub fn default_fx() -> Vec<u8> {
    let mut fx = vec![0u8; 512];
    fx[0..2].copy_from_slice(&0x037fu16.to_le_bytes());
    fx[24..28].copy_from_slice(&0x1f80u32.to_le_bytes());
    fx[28..32].copy_from_slice(&0xffffu32.to_le_bytes());
    fx
}

/// Memory differences between an input window and an output window.
pub fn mem_diff(before: &[u8], after: &[u8]) -> Vec<(u32, u8)> {
    before
        .iter()
        .zip(after)
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .map(|(i, (_, b))| (i as u32, *b))
        .collect()
}

/// Reads a fixture file (JSON lines, optionally gzip-compressed).
pub fn read_fixtures(path: &std::path::Path) -> std::io::Result<Vec<Fixture>> {
    use std::io::Read;
    let raw = std::fs::read(path)?;
    let text = if path.extension().is_some_and(|e| e == "gz") {
        let mut s = String::new();
        flate2::read::GzDecoder::new(&raw[..]).read_to_string(&mut s)?;
        s
    } else {
        String::from_utf8(raw)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?
    };
    Ok(text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("valid fixture line"))
        .collect())
}

pub fn write_fixtures(path: &std::path::Path, fx: &[Fixture]) -> std::io::Result<()> {
    use std::io::Write;
    let mut text = String::new();
    for f in fx {
        text.push_str(&serde_json::to_string(f).unwrap());
        text.push('\n');
    }
    if path.extension().is_some_and(|e| e == "gz") {
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        enc.write_all(text.as_bytes())?;
        std::fs::write(path, enc.finish()?)
    } else {
        std::fs::write(path, text)
    }
}
