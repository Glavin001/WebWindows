//! Token stream → [`Shader`].
//!
//! Shader model 2+ instructions carry their length; 1.x ones don't, but
//! every parameter token has bit 31 set and every instruction token has it
//! clear, so the parameters of a 1.x instruction are the tokens up to the
//! next one with bit 31 clear (`def` aside, whose immediates are raw).

use crate::bytecode::*;
use crate::Error;

const END: u32 = 0x0000_ffff;
const COMMENT: u32 = 0xfffe;

pub fn parse(tokens: &[u32]) -> Result<Shader, Error> {
    let version_token = *tokens.first().ok_or_else(|| Error::parse(0, "empty shader"))?;
    let stage = match version_token >> 16 {
        0xfffe => Stage::Vertex,
        0xffff => Stage::Pixel,
        _ => return Err(Error::parse(0, format!("bad version token {version_token:#010x}"))),
    };
    let version = Version::new((version_token >> 8) as u8, version_token as u8);
    let supported = match stage {
        Stage::Vertex => matches!(version.major, 1..=3),
        Stage::Pixel => matches!(version.major, 1..=3),
    };
    if !supported {
        return Err(Error::Unsupported(format!("shader model {}.{}", version.major, version.minor)));
    }
    let mut p = Parser { t: tokens, pos: 1, stage, version };
    let mut instructions = Vec::new();
    loop {
        let at = p.pos;
        let tok = *tokens.get(at).ok_or_else(|| Error::parse(at, "missing end token"))?;
        if tok == END {
            break;
        }
        if tok & 0xffff == COMMENT {
            let len = ((tok >> 16) & 0x7fff) as usize;
            p.pos += 1 + len;
            continue;
        }
        instructions.push(p.instruction()?);
    }
    Ok(Shader { stage, version, instructions })
}

struct Parser<'a> {
    t: &'a [u32],
    pos: usize,
    stage: Stage,
    version: Version,
}

fn has_dst(op: Opcode) -> bool {
    !matches!(
        op,
        Opcode::Nop
            | Opcode::Call
            | Opcode::CallNz
            | Opcode::Loop
            | Opcode::Ret
            | Opcode::EndLoop
            | Opcode::Label
            | Opcode::Rep
            | Opcode::EndRep
            | Opcode::If
            | Opcode::IfC
            | Opcode::Else
            | Opcode::EndIf
            | Opcode::Break
            | Opcode::BreakC
            | Opcode::BreakP
            | Opcode::Phase
    )
}

impl<'a> Parser<'a> {
    fn instruction(&mut self) -> Result<Instruction, Error> {
        let at = self.pos;
        let tok = self.t[at];
        let raw_op = (tok & 0xffff) as u16;
        let opcode = Opcode::from_u16(raw_op).ok_or_else(|| Error::parse(at, format!("unknown opcode {raw_op:#x}")))?;
        let controls = ((tok >> 16) & 0xff) as u8;
        let predicated = tok & (1 << 28) != 0;
        let coissue = tok & (1 << 30) != 0;
        self.pos += 1;

        // The parameter tokens of this instruction.
        let end = if self.version.major >= 2 {
            self.pos + ((tok >> 24) & 0xf) as usize
        } else if opcode == Opcode::Def {
            self.pos + 5
        } else {
            let mut e = self.pos;
            while e < self.t.len() && self.t[e] & 0x8000_0000 != 0 {
                e += 1;
            }
            e
        };
        if end > self.t.len() {
            return Err(Error::parse(at, "instruction runs past the end"));
        }

        let mut ins = Instruction {
            opcode,
            controls,
            coissue,
            dst: None,
            predicate: None,
            src: Vec::new(),
            decl: None,
            imm: Vec::new(),
        };

        match opcode {
            Opcode::Dcl => {
                let d = self.next(end, at)?;
                let dst = self.dst(end, at)?;
                ins.decl = Some(if dst.reg.ty == RegType::Sampler {
                    Decl::Sampler(TextureType::decode((d >> 27) & 0xf))
                } else {
                    Decl::Usage { usage: (d & 0x1f) as u8, index: ((d >> 16) & 0xf) as u8 }
                });
                ins.dst = Some(dst);
            }
            Opcode::Def | Opcode::DefI | Opcode::DefB => {
                ins.dst = Some(self.dst(end, at)?);
                let n = if opcode == Opcode::DefB { 1 } else { 4 };
                for _ in 0..n {
                    ins.imm.push(self.next(end, at)?);
                }
            }
            _ => {
                if has_dst(opcode) {
                    ins.dst = Some(self.dst(end, at)?);
                }
                if predicated {
                    ins.predicate = Some(self.src(end, at)?);
                }
                while self.pos < end {
                    ins.src.push(self.src(end, at)?);
                }
            }
        }
        if self.pos != end {
            return Err(Error::parse(at, format!("{} has {} unused tokens", opcode.mnemonic(), end - self.pos)));
        }
        Ok(ins)
    }

    fn next(&mut self, end: usize, at: usize) -> Result<u32, Error> {
        if self.pos >= end {
            return Err(Error::parse(at, "too few operands"));
        }
        self.pos += 1;
        Ok(self.t[self.pos - 1])
    }

    fn reg(&self, tok: u32, at: usize) -> Result<Reg, Error> {
        let raw_ty = ((tok >> 28) & 7) | ((tok >> 8) & 0x18);
        let ty = RegType::decode(raw_ty, self.stage, self.version)
            .ok_or_else(|| Error::parse(at, format!("unknown register type {raw_ty}")))?;
        let mut num = tok & 0x7ff;
        num += match raw_ty {
            11 => 2048,
            12 => 4096,
            13 => 6144,
            _ => 0,
        };
        Ok(Reg { ty, num })
    }

    fn rel(&mut self, end: usize, at: usize) -> Result<RelAddr, Error> {
        if self.version.major < 2 {
            // vs_1_1: always a0.x, no address token.
            return Ok(RelAddr { reg: Reg { ty: RegType::Addr, num: 0 }, component: 0 });
        }
        let tok = self.next(end, at)?;
        let reg = self.reg(tok, at)?;
        Ok(RelAddr { reg, component: ((tok >> 16) & 3) as u8 })
    }

    fn dst(&mut self, end: usize, at: usize) -> Result<Dst, Error> {
        let tok = self.next(end, at)?;
        let reg = self.reg(tok, at)?;
        let mods = (tok >> 20) & 0xf;
        let shift = ((tok >> 24) & 0xf) as i8;
        let rel = if tok & (1 << 13) != 0 { Some(self.rel(end, at)?) } else { None };
        Ok(Dst {
            reg,
            mask: ((tok >> 16) & 0xf) as u8,
            saturate: mods & 1 != 0,
            partial_precision: mods & 2 != 0,
            centroid: mods & 4 != 0,
            shift: if shift >= 8 { shift - 16 } else { shift },
            rel,
        })
    }

    fn src(&mut self, end: usize, at: usize) -> Result<Src, Error> {
        let tok = self.next(end, at)?;
        let reg = self.reg(tok, at)?;
        let sw = (tok >> 16) & 0xff;
        let modifier = SrcMod::decode((tok >> 24) & 0xf)
            .ok_or_else(|| Error::parse(at, format!("unknown source modifier {}", (tok >> 24) & 0xf)))?;
        let rel = if tok & (1 << 13) != 0 { Some(self.rel(end, at)?) } else { None };
        Ok(Src {
            reg,
            swizzle: [(sw & 3) as u8, ((sw >> 2) & 3) as u8, ((sw >> 4) & 3) as u8, ((sw >> 6) & 3) as u8],
            modifier,
            rel,
        })
    }
}

/// Encodes a shader back into tokens (used by the assembler and by tests
/// that round-trip parsed shaders).
pub fn encode(shader: &Shader) -> Vec<u32> {
    let mut out = Vec::new();
    let hi = match shader.stage {
        Stage::Vertex => 0xfffe_0000,
        Stage::Pixel => 0xffff_0000,
    };
    out.push(hi | (shader.version.major as u32) << 8 | shader.version.minor as u32);
    let sm2 = shader.version.major >= 2;
    for ins in &shader.instructions {
        let start = out.len();
        out.push(0);
        let rel_tok = |out: &mut Vec<u32>, r: &RelAddr| {
            if sm2 {
                out.push(0x8000_0000 | reg_bits(r.reg) | (r.component as u32 * 0x55) << 16);
            }
        };
        if ins.opcode == Opcode::Dcl {
            let d = match ins.decl.expect("dcl without declaration") {
                Decl::Usage { usage, index } => usage as u32 | (index as u32) << 16,
                Decl::Sampler(t) => t.encode() << 27,
            };
            out.push(0x8000_0000 | d);
        }
        if let Some(d) = &ins.dst {
            let mods = d.saturate as u32 | (d.partial_precision as u32) << 1 | (d.centroid as u32) << 2;
            let mut t = 0x8000_0000 | reg_bits(d.reg) | (d.mask as u32) << 16 | mods << 20;
            t |= ((d.shift as u32) & 0xf) << 24;
            if d.rel.is_some() {
                t |= 1 << 13;
            }
            out.push(t);
            if let Some(r) = &d.rel {
                rel_tok(&mut out, r);
            }
        }
        if let Some(p) = &ins.predicate {
            out.push(src_token(p));
        }
        for s in &ins.src {
            out.push(src_token(s));
            if let Some(r) = &s.rel {
                rel_tok(&mut out, r);
            }
        }
        out.extend_from_slice(&ins.imm);
        let len = (out.len() - start - 1) as u32;
        let mut t = ins.opcode.code() as u32 | (ins.controls as u32) << 16;
        if sm2 {
            t |= len << 24;
        }
        if ins.predicate.is_some() {
            t |= 1 << 28;
        }
        if ins.coissue {
            t |= 1 << 30;
        }
        out[start] = t;
    }
    out.push(END);
    out
}

fn reg_bits(r: Reg) -> u32 {
    let (ty, num) = match (r.ty, r.num) {
        (RegType::Const, n) if n >= 6144 => (13, n - 6144),
        (RegType::Const, n) if n >= 4096 => (12, n - 4096),
        (RegType::Const, n) if n >= 2048 => (11, n - 2048),
        (t, n) => (t.encode(), n),
    };
    (num & 0x7ff) | (ty & 7) << 28 | (ty & 0x18) << 8
}

fn src_token(s: &Src) -> u32 {
    let sw = s.swizzle[0] as u32 | (s.swizzle[1] as u32) << 2 | (s.swizzle[2] as u32) << 4 | (s.swizzle[3] as u32) << 6;
    let mut t = 0x8000_0000 | reg_bits(s.reg) | sw << 16 | s.modifier.encode() << 24;
    if s.rel.is_some() {
        t |= 1 << 13;
    }
    t
}
