//! A Direct3D shader assembler and disassembler for the syntax `fxc` and
//! `D3DXAssembleShader` use, so tests and scenes can write shaders as text.
//!
//! ```
//! let tokens = d3dgpu_shader::asm::assemble("
//!     vs_2_0
//!     dcl_position v0
//!     m4x4 oPos, v0, c0
//! ").unwrap();
//! let shader = d3dgpu_shader::parse(&tokens).unwrap();
//! assert!(d3dgpu_shader::asm::disassemble(&shader).contains("m4x4 oPos, v0, c0"));
//! ```

use crate::bytecode::*;
use crate::parse::encode;
use crate::Error;

/// Assembles shader text into tokens.
pub fn assemble(text: &str) -> Result<Vec<u32>, Error> {
    Ok(encode(&assemble_shader(text)?))
}

/// Assembles shader text into a parsed shader.
pub fn assemble_shader(text: &str) -> Result<Shader, Error> {
    let mut lines =
        text.lines().enumerate().map(|(i, l)| (i + 1, strip_comment(l).trim())).filter(|(_, l)| !l.is_empty());
    let (vline, vtext) = lines.next().ok_or_else(|| Error::asm(0, "empty shader"))?;
    let (stage, version) = parse_version(vtext).ok_or_else(|| Error::asm(vline, format!("bad version '{vtext}'")))?;
    let a = Asm { stage, version };
    let mut instructions = Vec::new();
    for (n, line) in lines {
        instructions.push(a.line(line).map_err(|m| Error::asm(n, m))?);
    }
    Ok(Shader { stage, version, instructions })
}

fn strip_comment(l: &str) -> &str {
    let l = l.split("//").next().unwrap();
    l.split(';').next().unwrap()
}

fn parse_version(s: &str) -> Option<(Stage, Version)> {
    let (stage, rest) =
        if let Some(r) = s.strip_prefix("vs_") { (Stage::Vertex, r) } else { (Stage::Pixel, s.strip_prefix("ps_")?) };
    let mut it = rest.split('_');
    let major = it.next()?.parse().ok()?;
    let minor = match it.next()? {
        "x" => 1,
        "sw" => 0xff,
        m => m.parse().ok()?,
    };
    Some((stage, Version::new(major, minor)))
}

struct Asm {
    stage: Stage,
    version: Version,
}

type R<T> = Result<T, String>;

impl Asm {
    fn line(&self, line: &str) -> R<Instruction> {
        let mut line = line;
        let mut coissue = false;
        if let Some(r) = line.strip_prefix('+') {
            coissue = true;
            line = r.trim_start();
        }
        let mut predicate = None;
        if let Some(r) = line.strip_prefix('(') {
            let (p, rest) = r.split_once(')').ok_or("unclosed predicate")?;
            predicate = Some(self.src(p.trim())?);
            line = rest.trim_start();
        }
        let (mnemonic, operands) = match line.find(char::is_whitespace) {
            Some(i) => (&line[..i], line[i..].trim()),
            None => (line, ""),
        };
        let ops: Vec<&str> = if operands.is_empty() {
            Vec::new()
        } else {
            split_operands(operands).into_iter().map(str::trim).collect()
        };
        let mut parts = mnemonic.split('_');
        let name = parts.next().unwrap().to_ascii_lowercase();
        let suffixes: Vec<String> = parts.map(|s| s.to_ascii_lowercase()).collect();

        let mut ins = Instruction {
            opcode: Opcode::Nop,
            controls: 0,
            coissue,
            dst: None,
            predicate,
            src: Vec::new(),
            decl: None,
            imm: Vec::new(),
        };

        if name == "dcl" {
            return self.dcl(ins, &suffixes, &ops);
        }
        if name == "def" || name == "defi" || name == "defb" {
            ins.opcode = match name.as_str() {
                "def" => Opcode::Def,
                "defi" => Opcode::DefI,
                _ => Opcode::DefB,
            };
            ins.dst = Some(self.dst(ops.first().ok_or("missing register")?)?);
            for v in &ops[1..] {
                ins.imm.push(match ins.opcode {
                    Opcode::Def => v.parse::<f32>().map_err(|_| format!("bad float '{v}'"))?.to_bits(),
                    Opcode::DefI => v.parse::<i32>().map_err(|_| format!("bad int '{v}'"))? as u32,
                    _ => match *v {
                        "true" | "1" => 1,
                        "false" | "0" => 0,
                        _ => return Err(format!("bad bool '{v}'")),
                    },
                });
            }
            let want = if ins.opcode == Opcode::DefB { 1 } else { 4 };
            if ins.imm.len() != want {
                return Err(format!("{name} needs {want} values"));
            }
            return Ok(ins);
        }

        // Opcode and the suffixes that are part of its name.
        let mut rest = &suffixes[..];
        let ps1 = self.stage == Stage::Pixel && self.version.major == 1;
        ins.opcode = match name.as_str() {
            "if" | "break" if !rest.is_empty() && cmp_from(&rest[0]).is_some() => {
                ins.controls = cmp_from(&rest[0]).unwrap() as u8;
                rest = &rest[1..];
                if name == "if" {
                    Opcode::IfC
                } else {
                    Opcode::BreakC
                }
            }
            "if" => Opcode::If,
            "break" => Opcode::Break,
            "setp" => {
                let c = rest.first().and_then(|s| cmp_from(s)).ok_or("setp needs a comparison")?;
                ins.controls = c as u8;
                rest = &rest[1..];
                Opcode::SetP
            }
            "texld" | "texldp" | "texldb" => {
                ins.controls = match name.as_str() {
                    "texldp" => TEXLD_PROJECT,
                    "texldb" => TEXLD_BIAS,
                    _ => 0,
                };
                Opcode::Tex
            }
            "texcrd" => Opcode::TexCoord,
            n => Opcode::ALL
                .iter()
                .copied()
                .find(|o| o.mnemonic() == n && !matches!(o, Opcode::IfC | Opcode::BreakC))
                .ok_or_else(|| format!("unknown instruction '{n}'"))?,
        };

        let mut dst_mods = Vec::new();
        for s in rest {
            dst_mods.push(s.as_str());
        }

        let mut ops = ops.into_iter();
        if has_dst(ins.opcode) {
            let mut d = self.dst(ops.next().ok_or("missing destination")?)?;
            for m in dst_mods {
                match m {
                    "sat" => d.saturate = true,
                    "pp" => d.partial_precision = true,
                    "centroid" => d.centroid = true,
                    "x2" => d.shift = 1,
                    "x4" => d.shift = 2,
                    "x8" => d.shift = 3,
                    "d2" => d.shift = -1,
                    "d4" => d.shift = -2,
                    "d8" => d.shift = -3,
                    _ => return Err(format!("unknown modifier '_{m}'")),
                }
            }
            ins.dst = Some(d);
        } else if !dst_mods.is_empty() {
            return Err(format!("unexpected modifier on {name}"));
        }
        for o in ops {
            ins.src.push(self.src(o)?);
        }
        let _ = ps1;
        Ok(ins)
    }

    fn dcl(&self, mut ins: Instruction, suffixes: &[String], ops: &[&str]) -> R<Instruction> {
        ins.opcode = Opcode::Dcl;
        let reg_text = ops.first().ok_or("dcl needs a register")?;
        let is_mod = |s: &str| matches!(s, "centroid" | "pp" | "sat");
        let (name, mods) = match suffixes.first() {
            Some(s) if !is_mod(s) => (s.as_str(), &suffixes[1..]),
            _ => ("", suffixes),
        };
        let mut d = self.dst(reg_text)?;
        ins.decl = Some(match name {
            "2d" => Decl::Sampler(TextureType::D2),
            "cube" => Decl::Sampler(TextureType::Cube),
            "volume" => Decl::Sampler(TextureType::Volume),
            "" => {
                if d.reg.ty == RegType::Sampler {
                    Decl::Sampler(TextureType::Unknown)
                } else {
                    Decl::Usage { usage: 0, index: 0 }
                }
            }
            n => {
                let digits = n.trim_start_matches(|c: char| c.is_ascii_alphabetic());
                let word = &n[..n.len() - digits.len()];
                let usage =
                    usage::NAMES.iter().position(|u| *u == word).ok_or_else(|| format!("unknown usage '{n}'"))?;
                let index = if digits.is_empty() { 0 } else { digits.parse().map_err(|_| "bad usage index")? };
                Decl::Usage { usage: usage as u8, index }
            }
        });
        for m in mods {
            match m.as_str() {
                "centroid" => d.centroid = true,
                "pp" => d.partial_precision = true,
                "sat" => d.saturate = true,
                _ => return Err(format!("unknown dcl modifier '{m}'")),
            }
        }
        ins.dst = Some(d);
        Ok(ins)
    }

    fn reg(&self, s: &str) -> R<(Reg, Option<RelAddr>)> {
        let lower = s.to_ascii_lowercase();
        let named = |ty, num| Ok((Reg { ty, num }, None));
        match lower.as_str() {
            "opos" => return named(RegType::RastOut, 0),
            "ofog" => return named(RegType::RastOut, 1),
            "opts" => return named(RegType::RastOut, 2),
            "odepth" => return named(RegType::DepthOut, 0),
            "vpos" => return named(RegType::MiscType, 0),
            "vface" => return named(RegType::MiscType, 1),
            "al" => return named(RegType::Loop, 0),
            "a0" if self.stage == Stage::Vertex => return named(RegType::Addr, 0),
            "p0" => return named(RegType::Predicate, 0),
            _ => {}
        }
        // Prefix, then a number or [relative].
        let prefixes: &[(&str, RegType)] = &[
            ("od", RegType::AttrOut),
            ("ot", RegType::TexCrdOut),
            ("oc", RegType::ColorOut),
            ("o", RegType::Output),
            ("r", RegType::Temp),
            ("v", RegType::Input),
            ("c", RegType::Const),
            ("i", RegType::ConstInt),
            ("b", RegType::ConstBool),
            ("s", RegType::Sampler),
            ("t", RegType::Texture),
            ("l", RegType::Label),
        ];
        let (prefix, ty) = prefixes
            .iter()
            .find(|(p, _)| {
                lower.starts_with(p) && lower[p.len()..].starts_with(|c: char| c.is_ascii_digit() || c == '[')
            })
            .ok_or_else(|| format!("unknown register '{s}'"))?;
        let mut ty = *ty;
        if ty == RegType::Output && (self.stage != Stage::Vertex || self.version.major < 3) {
            return Err(format!("o# registers need vs_3_0: '{s}'"));
        }
        if ty == RegType::Texture && self.stage == Stage::Vertex {
            ty = RegType::Addr;
        }
        let body = &lower[prefix.len()..];
        if let Some(inner) = body.strip_prefix('[') {
            let inner = inner.strip_suffix(']').ok_or("unclosed [")?;
            let mut num = 0u32;
            let mut rel = None;
            for term in inner.split('+').map(str::trim) {
                if let Ok(n) = term.parse::<u32>() {
                    num += n;
                } else {
                    let (r, comp) = match term.split_once('.') {
                        Some((r, c)) => (r, component(c.chars().next().ok_or("bad component")?)?),
                        None => (term, 0),
                    };
                    let (reg, _) = self.reg(r)?;
                    rel = Some(RelAddr { reg, component: comp });
                }
            }
            return Ok((Reg { ty, num }, rel));
        }
        let num = body.parse().map_err(|_| format!("bad register '{s}'"))?;
        Ok((Reg { ty, num }, None))
    }

    fn dst(&self, s: &str) -> R<Dst> {
        let (r, mask) = match s.rsplit_once('.') {
            Some((r, m)) if !r.ends_with(['+', '[']) && !m.contains(']') => {
                let mut mask = 0;
                for c in m.chars() {
                    mask |= 1 << component(c)?;
                }
                (r, mask)
            }
            _ => (s, MASK_ALL),
        };
        let (reg, rel) = self.reg(r)?;
        let mut d = Dst::new(reg, mask);
        d.rel = rel;
        Ok(d)
    }

    fn src(&self, s: &str) -> R<Src> {
        let mut s = s.trim();
        let mut neg = false;
        let mut not = false;
        let mut comp = false;
        if let Some(r) = s.strip_prefix('-') {
            neg = true;
            s = r.trim_start();
        } else if let Some(r) = s.strip_prefix('!') {
            not = true;
            s = r.trim_start();
        } else if let Some(r) = s.strip_prefix("1-") {
            comp = true;
            s = r.trim_start();
        }
        // Swizzle after the last '.' outside brackets; modifier suffix
        // (_bias, _bx2, _x2, _dz, _dw, _abs) before it.
        let close = s.rfind(']').map(|i| i + 1).unwrap_or(0);
        let (body, swizzle) = match s[close..].rfind('.') {
            Some(i) => (&s[..close + i], Some(&s[close + i + 1..])),
            None => (s, None),
        };
        let (reg_text, suffix) = match body[close.min(body.len())..].rfind('_') {
            Some(i) => (&body[..close + i], Some(&body[close + i + 1..])),
            None => (body, None),
        };
        let (reg, rel) = self.reg(reg_text)?;
        let modifier = match (suffix, neg, not, comp) {
            (None, false, false, false) => SrcMod::None,
            (None, true, _, _) => SrcMod::Neg,
            (None, _, true, _) => SrcMod::Not,
            (None, _, _, true) => SrcMod::Comp,
            (Some("bias"), n, _, false) => {
                if n {
                    SrcMod::BiasNeg
                } else {
                    SrcMod::Bias
                }
            }
            (Some("bx2"), n, _, false) => {
                if n {
                    SrcMod::SignNeg
                } else {
                    SrcMod::Sign
                }
            }
            (Some("x2"), n, _, false) => {
                if n {
                    SrcMod::X2Neg
                } else {
                    SrcMod::X2
                }
            }
            (Some("abs"), n, _, false) => {
                if n {
                    SrcMod::AbsNeg
                } else {
                    SrcMod::Abs
                }
            }
            (Some("dz") | Some("db"), false, _, false) => SrcMod::Dz,
            (Some("dw") | Some("da"), false, _, false) => SrcMod::Dw,
            _ => return Err(format!("bad source modifier in '{s}'")),
        };
        let swizzle = match swizzle {
            None => Src::IDENTITY,
            Some(sw) => {
                let c: Vec<u8> = sw.chars().map(component).collect::<R<_>>()?;
                match c.len() {
                    1 => [c[0]; 4],
                    2..=4 => std::array::from_fn(|i| c[i.min(c.len() - 1)]),
                    _ => return Err(format!("bad swizzle '.{sw}'")),
                }
            }
        };
        Ok(Src { reg, swizzle, modifier, rel })
    }
}

fn split_operands(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => depth -= 1,
            ',' if depth == 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

fn component(c: char) -> R<u8> {
    Ok(match c.to_ascii_lowercase() {
        'x' | 'r' => 0,
        'y' | 'g' => 1,
        'z' | 'b' => 2,
        'w' | 'a' => 3,
        _ => return Err(format!("bad component '{c}'")),
    })
}

fn cmp_from(s: &str) -> Option<Comparison> {
    Some(match s {
        "gt" => Comparison::Gt,
        "eq" => Comparison::Eq,
        "ge" => Comparison::Ge,
        "lt" => Comparison::Lt,
        "ne" => Comparison::Ne,
        "le" => Comparison::Le,
        _ => return None,
    })
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

// ---- Disassembly ----

/// Disassembles a shader into the syntax [`assemble`] accepts.
pub fn disassemble(shader: &Shader) -> String {
    let mut out = String::new();
    let st = if shader.stage == Stage::Vertex { "vs" } else { "ps" };
    let minor = match shader.version.minor {
        1 if shader.version.major == 2 => "x".to_string(),
        0xff => "sw".to_string(),
        m => m.to_string(),
    };
    out += &format!("{st}_{}_{minor}\n", shader.version.major);
    for ins in &shader.instructions {
        out += &disassemble_instruction(shader, ins);
        out.push('\n');
    }
    out
}

/// One instruction as text.
pub fn disassemble_instruction(shader: &Shader, ins: &Instruction) -> String {
    let mut s = String::new();
    if ins.coissue {
        s.push('+');
    }
    if let Some(p) = &ins.predicate {
        s += &format!("({}) ", src_text(shader, p));
    }
    let mut name = match ins.opcode {
        Opcode::Tex if shader.stage == Stage::Pixel && shader.version >= Version::new(1, 4) => match ins.controls {
            TEXLD_PROJECT => "texldp".to_string(),
            TEXLD_BIAS => "texldb".to_string(),
            _ => "texld".to_string(),
        },
        Opcode::TexCoord if shader.version >= Version::new(1, 4) => "texcrd".to_string(),
        Opcode::IfC | Opcode::BreakC | Opcode::SetP => {
            format!("{}_{}", ins.opcode.mnemonic(), ins.comparison().map(|c| c.suffix()).unwrap_or("??"))
        }
        Opcode::Dcl => {
            let mut n = String::from("dcl");
            match ins.decl {
                Some(Decl::Sampler(t)) => {
                    n += match t {
                        TextureType::D2 => "_2d",
                        TextureType::Cube => "_cube",
                        TextureType::Volume => "_volume",
                        TextureType::Unknown => "",
                    }
                }
                Some(Decl::Usage { usage, index }) => {
                    // Pixel shaders before 3.0 and t#/v# declarations carry no usage.
                    let reg = ins.dst.unwrap().reg;
                    let shows_usage =
                        shader.version.major >= 3 || (shader.stage == Stage::Vertex && reg.ty == RegType::Input);
                    if shows_usage {
                        let u = usage::NAMES.get(usage as usize).copied().unwrap_or("unknown");
                        n += &format!("_{u}");
                        if index != 0 || usage == usage::TEXCOORD || usage == usage::COLOR {
                            n += &index.to_string();
                        }
                    }
                }
                None => {}
            }
            n
        }
        op => op.mnemonic().to_string(),
    };
    if let Some(d) = &ins.dst {
        if ins.opcode != Opcode::Dcl {
            if d.saturate {
                name += "_sat";
            }
            if d.partial_precision {
                name += "_pp";
            }
            name += match d.shift {
                1 => "_x2",
                2 => "_x4",
                3 => "_x8",
                -1 => "_d2",
                -2 => "_d4",
                -3 => "_d8",
                _ => "",
            };
        }
        if d.centroid {
            name += "_centroid";
        }
    }
    s += &name;
    let mut ops: Vec<String> = Vec::new();
    if let Some(d) = &ins.dst {
        ops.push(dst_text(shader, d, ins.opcode == Opcode::Dcl));
    }
    for src in &ins.src {
        ops.push(src_text(shader, src));
    }
    match ins.opcode {
        Opcode::Def => ops.extend(ins.imm.iter().map(|v| fmt_float(f32::from_bits(*v)))),
        Opcode::DefI => ops.extend(ins.imm.iter().map(|v| (*v as i32).to_string())),
        Opcode::DefB => ops.push(if ins.imm[0] != 0 { "true".into() } else { "false".into() }),
        _ => {}
    }
    if !ops.is_empty() {
        s.push(' ');
        s += &ops.join(", ");
    }
    s
}

fn fmt_float(f: f32) -> String {
    let s = format!("{f:?}");
    s
}

fn reg_text(shader: &Shader, r: Reg, rel: Option<&RelAddr>) -> String {
    let base = match r.ty {
        RegType::Temp => "r",
        RegType::Input => "v",
        RegType::Const => "c",
        RegType::Addr => "a",
        RegType::Texture => "t",
        RegType::RastOut => {
            return ["oPos", "oFog", "oPts"].get(r.num as usize).copied().unwrap_or("oRast?").to_string();
        }
        RegType::AttrOut => "oD",
        RegType::TexCrdOut => "oT",
        RegType::Output => "o",
        RegType::ConstInt => "i",
        RegType::ColorOut => "oC",
        RegType::DepthOut => return "oDepth".into(),
        RegType::Sampler => "s",
        RegType::ConstBool => "b",
        RegType::Loop => return "aL".into(),
        RegType::MiscType => return if r.num == 0 { "vPos".into() } else { "vFace".into() },
        RegType::Label => "l",
        RegType::Predicate => "p",
    };
    let _ = shader;
    match rel {
        Some(a) => {
            let ar = reg_text(shader, a.reg, None);
            let comp = ["x", "y", "z", "w"][a.component as usize];
            let addr = if a.reg.ty == RegType::Loop { ar } else { format!("{ar}.{comp}") };
            if r.num == 0 {
                format!("{base}[{addr}]")
            } else {
                format!("{base}[{addr} + {}]", r.num)
            }
        }
        None => format!("{base}{}", r.num),
    }
}

fn dst_text(shader: &Shader, d: &Dst, dcl: bool) -> String {
    let mut s = reg_text(shader, d.reg, d.rel.as_ref());
    let show_mask = d.mask != MASK_ALL && !(dcl && d.mask == 0);
    if show_mask {
        s.push('.');
        for (i, c) in "xyzw".chars().enumerate() {
            if d.mask & (1 << i) != 0 {
                s.push(c);
            }
        }
    }
    s
}

fn src_text(shader: &Shader, src: &Src) -> String {
    let r = reg_text(shader, src.reg, src.rel.as_ref());
    let body = match src.modifier {
        SrcMod::None => r,
        SrcMod::Neg => format!("-{r}"),
        SrcMod::Bias => format!("{r}_bias"),
        SrcMod::BiasNeg => format!("-{r}_bias"),
        SrcMod::Sign => format!("{r}_bx2"),
        SrcMod::SignNeg => format!("-{r}_bx2"),
        SrcMod::Comp => format!("1-{r}"),
        SrcMod::X2 => format!("{r}_x2"),
        SrcMod::X2Neg => format!("-{r}_x2"),
        SrcMod::Dz => format!("{r}_dz"),
        SrcMod::Dw => format!("{r}_dw"),
        SrcMod::Abs => format!("{r}_abs"),
        SrcMod::AbsNeg => format!("-{r}_abs"),
        SrcMod::Not => format!("!{r}"),
    };
    if src.swizzle == Src::IDENTITY {
        return body;
    }
    let names = ['x', 'y', 'z', 'w'];
    let sw = src.swizzle;
    let text: String = if sw.iter().all(|c| *c == sw[0]) {
        names[sw[0] as usize].to_string()
    } else {
        sw.iter().map(|c| names[*c as usize]).collect()
    };
    format!("{body}.{text}")
}
