//! Shader model 1–3 → WGSL.
//!
//! Translation is statement by statement: D3D9's structured control flow
//! (`if`, `rep`, `loop`, `call`) nests the way WGSL's does, so registers
//! become `var<private>` variables and every instruction becomes a block
//! that computes a `vec4<f32>` and writes the masked components. No control
//! flow graph, no SSA.
//!
//! Derivatives and implicit-LOD sampling inside non-uniform branches are
//! common in D3D9 shaders and rejected by WGSL's uniformity analysis, so the
//! pixel shader turns that diagnostic off (`diagnostic(off,
//! derivative_uniformity)`). Hoisting coordinates above the branch and using
//! `textureSampleGrad` is the precise fix and is still to do.

#![allow(clippy::needless_range_loop)] // component loops index masks and names together

use std::collections::HashMap;
use std::fmt::Write;

use crate::bytecode::*;
use crate::key::*;
use crate::reflect::{ps_linkage, Reflection};
use crate::Error;

/// A translated shader.
#[derive(Clone, Debug)]
pub struct Translation {
    pub wgsl: String,
    /// Entry point name.
    pub entry: &'static str,
    /// Textures the shader reads, for the bind group layout.
    pub textures: Vec<TextureBinding>,
    /// Number of varyings (vertex outputs or pixel inputs), including clip
    /// varyings.
    pub varyings: u32,
    /// Pixel shaders: colour targets written (bit per target).
    pub color_outputs: u8,
}

/// One texture/sampler pair in bind group 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TextureBinding {
    /// Sampler index (0..16 pixel, 16..20 vertex).
    pub sampler: u32,
    pub dim: SamplerDim,
    /// `texture_depth_*`.
    pub depth: bool,
    /// `sampler_comparison`.
    pub compare: bool,
}

pub fn translate_vertex(shader: &Shader, refl: &Reflection, key: &VertexKey) -> Result<Translation, Error> {
    if shader.stage != Stage::Vertex {
        return Err(Error::Unsupported("not a vertex shader".into()));
    }
    let mut g = Gen::new(shader, refl, None);
    g.vkey = Some(key);
    g.run()
}

pub fn translate_pixel(shader: &Shader, refl: &Reflection, key: &PixelKey) -> Result<Translation, Error> {
    if shader.stage != Stage::Pixel {
        return Err(Error::Unsupported("not a pixel shader".into()));
    }
    let g = Gen::new(shader, refl, Some(key));
    g.run()
}

#[derive(Clone, Copy, PartialEq)]
enum Block {
    If,
    Rep,
    Loop(usize),
}

struct Gen<'a> {
    sh: &'a Shader,
    refl: &'a Reflection,
    pkey: Option<&'a PixelKey>,
    vkey: Option<&'a VertexKey>,
    ps: bool,
    ps1: bool,
    ps14: bool,
    defs: HashMap<u32, [f32; 4]>,
    defi: HashMap<u32, [i32; 4]>,
    defb: HashMap<u32, bool>,
    /// Functions: main body first, then labels.
    funcs: Vec<(String, String)>,
    cur: String,
    indent: usize,
    blocks: Vec<Block>,
    loops: Vec<String>,
    tmp: usize,
    temps: u32,
    texregs: u32,
    has_a0: bool,
    has_p0: bool,
    uses_rel_const: bool,
    sampler_dims: HashMap<u32, TextureType>,
}

fn fmt_f32(v: f32) -> String {
    // Infinities and NaNs (in `def` constants) are made at run time: WGSL
    // rejects them in constant expressions, and Direct3D passes them on.
    if !v.is_finite() {
        return format!("bitcast<f32>({:#x}u | d3d_zero)", v.to_bits());
    }
    let s = format!("{v:?}");
    if s.contains('e') && !s.contains('.') {
        // 1e30 -> 1.0e30
        s.replacen('e', ".0e", 1)
    } else {
        s
    }
}

fn vec4f(v: [f32; 4]) -> String {
    format!("vec4<f32>({}, {}, {}, {})", fmt_f32(v[0]), fmt_f32(v[1]), fmt_f32(v[2]), fmt_f32(v[3]))
}

const COMP: [char; 4] = ['x', 'y', 'z', 'w'];

fn cmp_op(c: Comparison) -> &'static str {
    match c {
        Comparison::Gt => ">",
        Comparison::Eq => "==",
        Comparison::Ge => ">=",
        Comparison::Lt => "<",
        Comparison::Ne => "!=",
        Comparison::Le => "<=",
    }
}

impl<'a> Gen<'a> {
    fn new(sh: &'a Shader, refl: &'a Reflection, pkey: Option<&'a PixelKey>) -> Gen<'a> {
        let ps = sh.stage == Stage::Pixel;
        let mut defs = HashMap::new();
        let mut defi = HashMap::new();
        let mut defb = HashMap::new();
        let mut temps = 0;
        let mut texregs = 0;
        let mut has_a0 = false;
        let mut has_p0 = false;
        let mut uses_rel_const = false;
        let ps1 = ps && sh.version.major == 1;
        let ps14 = ps1 && sh.version.minor >= 4;
        for ins in &sh.instructions {
            match ins.opcode {
                Opcode::Def => {
                    let mut v = ins.imm.iter().map(|b| f32::from_bits(*b)).collect::<Vec<_>>();
                    if ps1 {
                        v.iter_mut().for_each(|x| *x = x.clamp(-1.0, 1.0));
                    }
                    defs.insert(ins.dst.unwrap().reg.num, [v[0], v[1], v[2], v[3]]);
                }
                Opcode::DefI => {
                    let v = ins.imm.iter().map(|b| *b as i32).collect::<Vec<_>>();
                    defi.insert(ins.dst.unwrap().reg.num, [v[0], v[1], v[2], v[3]]);
                }
                Opcode::DefB => {
                    defb.insert(ins.dst.unwrap().reg.num, ins.imm[0] != 0);
                }
                _ => {}
            }
            let regs = ins.dst.iter().map(|d| (d.reg, d.rel)).chain(ins.src.iter().map(|s| (s.reg, s.rel)));
            for (r, rel) in regs.chain(ins.predicate.iter().map(|s| (s.reg, s.rel))) {
                match r.ty {
                    RegType::Temp => temps = temps.max(r.num + 1),
                    RegType::Texture => texregs = texregs.max(r.num + 1),
                    RegType::Addr => has_a0 = true,
                    RegType::Predicate => has_p0 = true,
                    RegType::Const if rel.is_some() => uses_rel_const = true,
                    _ => {}
                }
                if let Some(rel) = rel {
                    if rel.reg.ty == RegType::Addr {
                        has_a0 = true;
                    }
                }
            }
        }
        if ps1 {
            temps = temps.max(1);
        }
        if ps && sh.version.major < 3 {
            texregs = texregs.max(8);
        }
        let sampler_dims = refl.samplers.iter().copied().collect();
        Gen {
            sh,
            refl,
            pkey,
            vkey: None,
            ps,
            ps1,
            ps14,
            defs,
            defi,
            defb,
            funcs: Vec::new(),
            cur: String::new(),
            indent: 1,
            blocks: Vec::new(),
            loops: Vec::new(),
            tmp: 0,
            temps,
            texregs,
            has_a0,
            has_p0,
            uses_rel_const,
            sampler_dims,
        }
    }

    fn line(&mut self, s: &str) {
        for _ in 0..self.indent {
            self.cur.push_str("    ");
        }
        self.cur.push_str(s);
        self.cur.push('\n');
    }

    fn fresh(&mut self, base: &str) -> String {
        self.tmp += 1;
        format!("{base}{}", self.tmp)
    }

    // ---- Operands ----

    /// Reads a register as `vec4<f32>` before swizzle and modifier.
    fn reg_read(&mut self, s: &Src) -> Result<String, Error> {
        let n = s.reg.num;
        Ok(match s.reg.ty {
            RegType::Temp => format!("r{n}"),
            RegType::Input => match s.rel {
                Some(rel) => format!("v[{}]", self.rel_index(&rel, n)),
                None => format!("v[{n}]"),
            },
            RegType::Texture => format!("t{n}"),
            RegType::Const => self.const_read(s),
            RegType::ConstInt => match self.defi.get(&n) {
                Some(v) => format!("vec4<f32>({}.0, {}.0, {}.0, {}.0)", v[0], v[1], v[2], v[3]),
                None => format!("vec4<f32>(k.i[{n}])"),
            },
            RegType::ConstBool => format!("vec4<f32>(f32({}))", self.bool_const(n)),
            RegType::Loop => "vec4<f32>(f32(aL))".into(),
            RegType::MiscType => {
                if n == 0 {
                    "v_pos".into()
                } else {
                    "v_face".into()
                }
            }
            RegType::Addr => "vec4<f32>(a0)".into(),
            RegType::Predicate => "vec4<f32>(p0)".into(),
            RegType::Output => format!("o[{n}]"),
            RegType::ColorOut => format!("o_c{n}"),
            RegType::AttrOut => format!("o_d{n}"),
            RegType::TexCrdOut => format!("o_t{n}"),
            RegType::RastOut => ["o_pos", "o_fog", "o_pts"][n.min(2) as usize].into(),
            RegType::DepthOut => "o_depth".into(),
            t => return Err(Error::Unsupported(format!("reading {t:?} registers"))),
        })
    }

    fn rel_index(&self, rel: &RelAddr, base: u32) -> String {
        let a = match rel.reg.ty {
            RegType::Loop => "aL".to_string(),
            _ => format!("a0.{}", COMP[rel.component as usize]),
        };
        if base == 0 {
            a
        } else {
            format!("({a} + {base})")
        }
    }

    fn const_read(&mut self, s: &Src) -> String {
        let n = s.reg.num;
        if let Some(rel) = s.rel {
            return format!("cf({})", self.rel_index(&rel, n));
        }
        if let Some(v) = self.defs.get(&n) {
            return vec4f(*v);
        }
        if self.ps1 {
            format!("clamp(k.f[{n}], vec4<f32>(-1.0), vec4<f32>(1.0))")
        } else {
            format!("k.f[{n}]")
        }
    }

    fn bool_const(&self, n: u32) -> String {
        match self.defb.get(&n) {
            Some(v) => v.to_string(),
            None => format!("(k.b[{}][{}] != 0u)", n / 4, n % 4),
        }
    }

    fn int_const(&self, n: u32) -> String {
        match self.defi.get(&n) {
            Some(v) => format!("vec4<i32>({}, {}, {}, {})", v[0], v[1], v[2], v[3]),
            None => format!("k.i[{n}]"),
        }
    }

    /// A source operand as a `vec4<f32>` expression.
    fn src(&mut self, s: &Src) -> Result<String, Error> {
        let base = self.reg_read(s)?;
        let sw = if s.swizzle == Src::IDENTITY {
            base
        } else {
            let sw: String = s.swizzle.iter().map(|c| COMP[*c as usize]).collect();
            format!("{base}.{sw}")
        };
        Ok(match s.modifier {
            SrcMod::None => sw,
            SrcMod::Neg => format!("(-{sw})"),
            SrcMod::Bias => format!("({sw} - vec4<f32>(0.5))"),
            SrcMod::BiasNeg => format!("(vec4<f32>(0.5) - {sw})"),
            SrcMod::Sign => format!("({sw} * 2.0 - vec4<f32>(1.0))"),
            SrcMod::SignNeg => format!("(vec4<f32>(1.0) - {sw} * 2.0)"),
            SrcMod::Comp => format!("(vec4<f32>(1.0) - {sw})"),
            SrcMod::X2 => format!("({sw} * 2.0)"),
            SrcMod::X2Neg => format!("({sw} * -2.0)"),
            SrcMod::Dz => format!("d3d_div({sw}, {sw}.z)"),
            SrcMod::Dw => format!("d3d_div({sw}, {sw}.w)"),
            SrcMod::Abs => format!("abs({sw})"),
            SrcMod::AbsNeg => format!("(-abs({sw}))"),
            SrcMod::Not => format!("(vec4<f32>(1.0) - {sw})"),
        })
    }

    /// A source operand with its register number offset (matrix rows).
    fn src_row(&mut self, s: &Src, k: u32) -> Result<String, Error> {
        let mut s = *s;
        s.reg.num += k;
        self.src(&s)
    }

    /// The boolean component `c` of a predicate source.
    fn pred_comp(&self, p: &Src, c: usize) -> String {
        let e = format!("p0.{}", COMP[p.swizzle[c] as usize]);
        if p.modifier == SrcMod::Not {
            format!("!{e}")
        } else {
            e
        }
    }

    /// A boolean condition from the source of `if`/`callnz`/`breakp`.
    fn cond(&mut self, s: &Src) -> Result<String, Error> {
        Ok(match s.reg.ty {
            RegType::ConstBool => self.bool_const(s.reg.num),
            RegType::Predicate => self.pred_comp(s, 0),
            _ => format!("({}.x != 0.0)", self.src(s)?),
        })
    }

    fn dst_name(&self, d: &Dst) -> Result<String, Error> {
        let n = d.reg.num;
        Ok(match d.reg.ty {
            RegType::Temp => format!("r{n}"),
            RegType::Texture => format!("t{n}"),
            RegType::RastOut => ["o_pos", "o_fog", "o_pts"][n.min(2) as usize].into(),
            RegType::AttrOut => format!("o_d{n}"),
            RegType::TexCrdOut => format!("o_t{n}"),
            RegType::Output => match d.rel {
                Some(rel) => format!("o[{}]", self.rel_index(&rel, n)),
                None => format!("o[{n}]"),
            },
            RegType::ColorOut => format!("o_c{n}"),
            RegType::DepthOut => "o_depth".into(),
            RegType::Input => format!("v[{n}]"),
            t => return Err(Error::Unsupported(format!("writing {t:?} registers"))),
        })
    }

    /// Writes a `vec4<f32>` value to a destination: shift, saturate, mask,
    /// predicate.
    fn write(&mut self, d: &Dst, pred: Option<&Src>, value: &str) -> Result<(), Error> {
        let mut v = value.to_string();
        if d.shift != 0 {
            let f = if d.shift > 0 { (1 << d.shift) as f32 } else { 1.0 / (1 << -d.shift) as f32 };
            v = format!("({v} * {})", fmt_f32(f));
        }
        if d.saturate {
            v = format!("clamp({v}, vec4<f32>(0.0), vec4<f32>(1.0))");
        }
        match d.reg.ty {
            RegType::Addr => {
                let t = self.fresh("w");
                self.line(&format!("let {t} = {v};"));
                for c in 0..4 {
                    if d.mask & (1 << c) != 0 {
                        self.masked(pred, c, &format!("a0.{} = i32(floor({t}.{}));", COMP[c], COMP[c]));
                    }
                }
                return Ok(());
            }
            RegType::Predicate => unreachable!("setp writes the predicate itself"),
            _ => {}
        }
        let name = self.dst_name(d)?;
        if d.mask == MASK_ALL && pred.is_none() {
            self.line(&format!("{name} = {v};"));
            return Ok(());
        }
        let t = self.fresh("w");
        self.line(&format!("let {t} = {v};"));
        for c in 0..4 {
            if d.mask & (1 << c) != 0 {
                self.masked(pred, c, &format!("{name}.{} = {t}.{};", COMP[c], COMP[c]));
            }
        }
        Ok(())
    }

    fn masked(&mut self, pred: Option<&Src>, c: usize, stmt: &str) {
        match pred {
            Some(p) => {
                let cond = self.pred_comp(p, c);
                self.line(&format!("if ({cond}) {{ {stmt} }}"));
            }
            None => self.line(stmt),
        }
    }

    // ---- Instructions ----

    fn run(mut self) -> Result<Translation, Error> {
        let mut main_done = false;
        let mut label_open = false;
        let sh = self.sh;
        let ins_list = &sh.instructions;
        let mut i = 0;
        while i < ins_list.len() {
            let ins = &ins_list[i];
            match ins.opcode {
                Opcode::Label => {
                    let name = format!("label{}", ins.src[0].reg.num);
                    if !main_done {
                        self.close_func("shader_main".into());
                        main_done = true;
                    } else if label_open {
                        return Err(Error::parse(i, "label inside a label"));
                    }
                    self.cur.clear();
                    self.funcs.push((name, String::new()));
                    label_open = true;
                    i += 1;
                    continue;
                }
                Opcode::Ret if self.blocks.is_empty() => {
                    if !main_done {
                        self.close_func("shader_main".into());
                        main_done = true;
                    } else if label_open {
                        let name = self.funcs.pop().unwrap().0;
                        self.close_func(name);
                        label_open = false;
                    }
                    i += 1;
                    continue;
                }
                _ => {}
            }
            if main_done && !label_open {
                // Code after the main `ret` that isn't in a label is dead.
                i += 1;
                continue;
            }
            // Pixel shader 1.x co-issue: the next instruction reads its
            // sources before this one writes.
            if i + 1 < ins_list.len() && ins_list[i + 1].coissue {
                let a = ins;
                let b = &ins_list[i + 1];
                let va = self.value(a)?;
                let vb = self.value(b)?;
                if let (Some(va), Some(vb)) = (va, vb) {
                    let (ta, tb) = (self.fresh("ci"), self.fresh("ci"));
                    self.line(&format!("let {ta} = {va};"));
                    self.line(&format!("let {tb} = {vb};"));
                    self.write(&a.dst.unwrap(), a.predicate.as_ref(), &ta)?;
                    self.write(&b.dst.unwrap(), b.predicate.as_ref(), &tb)?;
                    i += 2;
                    continue;
                }
            }
            self.instruction(ins)?;
            i += 1;
        }
        if !self.blocks.is_empty() {
            return Err(Error::Unsupported("unbalanced control flow".into()));
        }
        if !main_done {
            self.close_func("shader_main".into());
        } else if label_open {
            let name = self.funcs.pop().unwrap().0;
            self.close_func(name);
        }
        self.finish()
    }

    fn close_func(&mut self, name: String) {
        let body = std::mem::take(&mut self.cur);
        if let Some(f) = self.funcs.iter_mut().find(|(n, _)| *n == name) {
            f.1 = body;
        } else {
            self.funcs.push((name, body));
        }
    }

    /// The value of a pure arithmetic instruction, or `None`.
    fn value(&mut self, ins: &Instruction) -> Result<Option<String>, Error> {
        let s = |g: &mut Self, i: usize| -> Result<String, Error> {
            let src = ins.src.get(i).ok_or_else(|| Error::Unsupported(format!("{:?} missing operand", ins.opcode)))?;
            g.src(src)
        };
        let v = match ins.opcode {
            Opcode::Mov | Opcode::MovA => s(self, 0)?,
            Opcode::Add => format!("({} + {})", s(self, 0)?, s(self, 1)?),
            Opcode::Sub => format!("({} - {})", s(self, 0)?, s(self, 1)?),
            Opcode::Mul => format!("({} * {})", s(self, 0)?, s(self, 1)?),
            Opcode::Mad => format!("({} * {} + {})", s(self, 0)?, s(self, 1)?, s(self, 2)?),
            Opcode::Rcp => format!("vec4<f32>(d3d_rcp({}.x))", s(self, 0)?),
            Opcode::Rsq => format!("vec4<f32>(d3d_rsq({}.x))", s(self, 0)?),
            Opcode::Dp3 => format!("vec4<f32>(dot({}.xyz, {}.xyz))", s(self, 0)?, s(self, 1)?),
            Opcode::Dp4 => format!("vec4<f32>(dot({}, {}))", s(self, 0)?, s(self, 1)?),
            Opcode::Min => format!("min({}, {})", s(self, 0)?, s(self, 1)?),
            Opcode::Max => format!("max({}, {})", s(self, 0)?, s(self, 1)?),
            Opcode::Slt => format!("select(vec4<f32>(0.0), vec4<f32>(1.0), {} < {})", s(self, 0)?, s(self, 1)?),
            Opcode::Sge => format!("select(vec4<f32>(0.0), vec4<f32>(1.0), {} >= {})", s(self, 0)?, s(self, 1)?),
            Opcode::Exp => format!("vec4<f32>(exp2({}.x))", s(self, 0)?),
            Opcode::ExpP if self.sh.version.major < 2 => format!("d3d_expp({}.x)", s(self, 0)?),
            Opcode::ExpP => format!("vec4<f32>(exp2({}.x))", s(self, 0)?),
            Opcode::Log => format!("vec4<f32>(d3d_log({}.x))", s(self, 0)?),
            Opcode::LogP if self.sh.version.major < 2 => format!("d3d_logp({}.x)", s(self, 0)?),
            Opcode::LogP => format!("vec4<f32>(d3d_log({}.x))", s(self, 0)?),
            Opcode::Lit => format!("d3d_lit({})", s(self, 0)?),
            Opcode::Dst => {
                let (a, b) = (s(self, 0)?, s(self, 1)?);
                format!("vec4<f32>(1.0, {a}.y * {b}.y, {a}.z, {b}.w)")
            }
            Opcode::Lrp => {
                let (a, b, c) = (s(self, 0)?, s(self, 1)?, s(self, 2)?);
                format!("({a} * ({b} - {c}) + {c})")
            }
            Opcode::Frc => format!("fract({})", s(self, 0)?),
            Opcode::M4x4 | Opcode::M4x3 | Opcode::M3x4 | Opcode::M3x3 | Opcode::M3x2 => {
                let (cols, rows) = match ins.opcode {
                    Opcode::M4x4 => (4, 4),
                    Opcode::M4x3 => (4, 3),
                    Opcode::M3x4 => (3, 4),
                    Opcode::M3x3 => (3, 3),
                    _ => (3, 2),
                };
                let a = s(self, 0)?;
                let m = ins.src[1];
                let mut parts = Vec::new();
                for k in 0..4 {
                    if k < rows {
                        let row = self.src_row(&m, k as u32)?;
                        parts.push(if cols == 4 {
                            format!("dot({a}, {row})")
                        } else {
                            format!("dot({a}.xyz, {row}.xyz)")
                        });
                    } else {
                        parts.push("0.0".into());
                    }
                }
                format!("vec4<f32>({})", parts.join(", "))
            }
            Opcode::Pow => format!("vec4<f32>(d3d_pow({}.x, {}.x))", s(self, 0)?, s(self, 1)?),
            Opcode::Crs => format!("vec4<f32>(cross({}.xyz, {}.xyz), 0.0)", s(self, 0)?, s(self, 1)?),
            Opcode::Sgn => format!("sign({})", s(self, 0)?),
            Opcode::Abs => format!("abs({})", s(self, 0)?),
            Opcode::Nrm => format!("d3d_nrm({})", s(self, 0)?),
            Opcode::SinCos => {
                let a = s(self, 0)?;
                format!("vec4<f32>(cos({a}.x), sin({a}.x), 0.0, 0.0)")
            }
            Opcode::Cmp => {
                let (a, b, c) = (s(self, 0)?, s(self, 1)?, s(self, 2)?);
                format!("select({c}, {b}, {a} >= vec4<f32>(0.0))")
            }
            Opcode::Cnd => {
                let (a, b, c) = (s(self, 0)?, s(self, 1)?, s(self, 2)?);
                format!("select({c}, {b}, {a} > vec4<f32>(0.5))")
            }
            Opcode::Dp2Add => {
                let (a, b, c) = (s(self, 0)?, s(self, 1)?, s(self, 2)?);
                format!("vec4<f32>(dot({a}.xy, {b}.xy) + {c}.x)")
            }
            Opcode::Dsx if self.ps => format!("dpdx({})", s(self, 0)?),
            Opcode::Dsy if self.ps => format!("dpdy({})", s(self, 0)?),
            Opcode::Bem => {
                let (a, b) = (s(self, 0)?, s(self, 1)?);
                let m = format!("drv.bump_env[{}]", ins.dst.unwrap().reg.num);
                format!(
                    "vec4<f32>({a}.x + {m}.x * {b}.x + {m}.z * {b}.y, {a}.y + {m}.y * {b}.x + {m}.w * {b}.y, 0.0, 0.0)"
                )
            }
            Opcode::TexCoord if self.ps14 => s(self, 0)?,
            _ => return Ok(None),
        };
        Ok(Some(v))
    }

    fn instruction(&mut self, ins: &Instruction) -> Result<(), Error> {
        if matches!(ins.opcode, Opcode::Dcl | Opcode::Def | Opcode::DefI | Opcode::DefB | Opcode::Nop | Opcode::Phase) {
            return Ok(());
        }
        if let Some(v) = self.value(ins)? {
            let d = ins.dst.unwrap();
            if ins.opcode == Opcode::MovA || (d.reg.ty == RegType::Addr && self.sh.version.major >= 2) {
                // mova rounds to nearest; vs_1_1's mov to a0 floors.
                let t = self.fresh("w");
                self.line(&format!("let {t} = {v};"));
                for c in 0..4 {
                    if d.mask & (1 << c) != 0 {
                        let stmt = format!("a0.{} = i32(floor({t}.{} + 0.5));", COMP[c], COMP[c]);
                        self.masked(ins.predicate.as_ref(), c, &stmt);
                    }
                }
                return Ok(());
            }
            return self.write(&d, ins.predicate.as_ref(), &v);
        }
        let s = |g: &mut Self, i: usize| -> Result<String, Error> {
            let src = ins.src.get(i).ok_or_else(|| Error::Unsupported(format!("{:?} missing operand", ins.opcode)))?;
            g.src(src)
        };
        match ins.opcode {
            Opcode::SetP => {
                let d = ins.dst.unwrap();
                let (a, b) = (s(self, 0)?, s(self, 1)?);
                let op = cmp_op(ins.comparison().ok_or_else(|| Error::Unsupported("setp comparison".into()))?);
                let t = self.fresh("w");
                self.line(&format!("let {t} = {a} {op} {b};"));
                for c in 0..4 {
                    if d.mask & (1 << c) != 0 {
                        let stmt = format!("p0.{} = {t}.{};", COMP[c], COMP[c]);
                        self.masked(ins.predicate.as_ref(), c, &stmt);
                    }
                }
            }
            Opcode::If => {
                let c = self.cond(&ins.src[0])?;
                self.line(&format!("if ({c}) {{"));
                self.indent += 1;
                self.blocks.push(Block::If);
            }
            Opcode::IfC => {
                let (a, b) = (s(self, 0)?, s(self, 1)?);
                let op = cmp_op(ins.comparison().ok_or_else(|| Error::Unsupported("if comparison".into()))?);
                self.line(&format!("if ({a}.x {op} {b}.x) {{"));
                self.indent += 1;
                self.blocks.push(Block::If);
            }
            Opcode::Else => {
                self.indent -= 1;
                self.line("} else {");
                self.indent += 1;
            }
            Opcode::EndIf => {
                self.pop_block(Block::If)?;
                self.line("}");
            }
            Opcode::Rep => {
                let n = self.int_const(ins.src[0].reg.num);
                let c = self.fresh("rep");
                self.line(&format!("for (var {c}: i32 = 0; {c} < clamp({n}.x, 0, 255); {c}++) {{"));
                self.indent += 1;
                self.blocks.push(Block::Rep);
            }
            Opcode::EndRep => {
                self.pop_block(Block::Rep)?;
                self.line("}");
            }
            Opcode::Loop => {
                let n = self.int_const(ins.src[1].reg.num);
                let saved = self.fresh("saved_aL");
                let c = self.fresh("lp");
                self.line("{");
                self.indent += 1;
                self.line(&format!("let {saved} = aL;"));
                self.line(&format!("aL = {n}.y;"));
                self.line(&format!("for (var {c}: i32 = 0; {c} < clamp({n}.x, 0, 255); {c}++) {{"));
                self.indent += 1;
                self.loops.push(format!("{n}.z|{saved}"));
                self.blocks.push(Block::Loop(self.loops.len() - 1));
            }
            Opcode::EndLoop => {
                let Some(Block::Loop(idx)) = self.blocks.last().copied() else {
                    return Err(Error::Unsupported("endloop without loop".into()));
                };
                self.blocks.pop();
                let info = self.loops[idx].clone();
                let (step, saved) = info.split_once('|').unwrap();
                self.line(&format!("aL += {step};"));
                self.indent -= 1;
                self.line("}");
                self.line(&format!("aL = {saved};"));
                self.indent -= 1;
                self.line("}");
            }
            Opcode::Break => self.line("break;"),
            Opcode::BreakC => {
                let (a, b) = (s(self, 0)?, s(self, 1)?);
                let op = cmp_op(ins.comparison().ok_or_else(|| Error::Unsupported("break comparison".into()))?);
                self.line(&format!("if ({a}.x {op} {b}.x) {{ break; }}"));
            }
            Opcode::BreakP => {
                let c = self.cond(&ins.src[0])?;
                self.line(&format!("if ({c}) {{ break; }}"));
            }
            Opcode::Call => self.line(&format!("label{}();", ins.src[0].reg.num)),
            Opcode::CallNz => {
                let c = self.cond(&ins.src[1])?;
                self.line(&format!("if ({c}) {{ label{}(); }}", ins.src[0].reg.num));
            }
            Opcode::Ret => self.line("return;"),
            Opcode::TexKill => {
                let d = ins.dst.unwrap();
                let r = self.dst_name(&d)?;
                let comps: String = if self.sh.version.major >= 2 {
                    (0..4).filter(|c| d.mask & (1 << c) != 0).map(|c| COMP[c]).collect()
                } else {
                    "xyz".into()
                };
                let cond = match comps.len() {
                    0 => "false".to_string(),
                    1 => format!("{r}.{comps} < 0.0"),
                    n => format!("any({r}.{comps} < vec{n}<f32>(0.0))"),
                };
                self.line(&format!("if ({cond}) {{ discard; }}"));
            }
            Opcode::Tex => self.tex(ins)?,
            Opcode::TexLdl => {
                let coord = s(self, 0)?;
                let n = self.sampler_index(ins.src[1].reg.num);
                let v = self.sample(n, &coord, Lod::Level(format!("{coord}.w")), false)?;
                self.write(&ins.dst.unwrap(), ins.predicate.as_ref(), &v)?;
            }
            Opcode::TexLdd => {
                let coord = s(self, 0)?;
                let n = self.sampler_index(ins.src[1].reg.num);
                let (dx, dy) = (s(self, 2)?, s(self, 3)?);
                let v = self.sample(n, &coord, Lod::Grad(dx, dy), false)?;
                self.write(&ins.dst.unwrap(), ins.predicate.as_ref(), &v)?;
            }
            Opcode::TexCoord => {
                // ps_1_1..1_3: t# = saturate(texcoord) with w = 1.
                let d = ins.dst.unwrap();
                let n = d.reg.num;
                self.write(&d, None, &format!("vec4<f32>(clamp(tc{n}.xyz, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0)"))?;
            }
            _ if self.ps1 => self.tex_ps1(ins)?,
            op => {
                return Err(Error::Unsupported(format!(
                    "{} in {:?} shader model {}.{}",
                    op.mnemonic(),
                    self.sh.stage,
                    self.sh.version.major,
                    self.sh.version.minor
                )));
            }
        }
        Ok(())
    }

    fn pop_block(&mut self, want: Block) -> Result<(), Error> {
        match self.blocks.pop() {
            Some(b) if b == want => {
                self.indent -= 1;
                Ok(())
            }
            _ => Err(Error::Unsupported("unbalanced control flow".into())),
        }
    }

    fn sampler_index(&self, reg: u32) -> u32 {
        if self.ps {
            reg
        } else {
            reg + 16
        }
    }

    fn tex(&mut self, ins: &Instruction) -> Result<(), Error> {
        let d = ins.dst.unwrap();
        if self.sh.version.major >= 2 {
            // texld dst, coord, s#
            let coord = self.src(&ins.src[0])?;
            let n = self.sampler_index(ins.src[1].reg.num);
            let (lod, proj) = match ins.controls {
                TEXLD_PROJECT => (Lod::Implicit, true),
                TEXLD_BIAS => (Lod::Bias(format!("{coord}.w")), false),
                _ => (Lod::Implicit, false),
            };
            let v = self.sample(n, &coord, lod, proj)?;
            return self.write(&d, ins.predicate.as_ref(), &v);
        }
        if self.ps14 {
            // texld r#, src: sampler = destination register number.
            let coord = self.src(&ins.src[0])?;
            let v = self.sample(d.reg.num, &coord, Lod::Implicit, false)?;
            return self.write(&d, None, &v);
        }
        // tex t#: sample stage # at texture coordinate #.
        let n = d.reg.num;
        let proj = self.pkey.map(|k| k.samplers[n as usize].projected).unwrap_or(false);
        let v = self.sample(n, &format!("tc{n}"), Lod::Implicit, proj)?;
        self.write(&d, None, &v)
    }

    /// Texture-addressing instructions of pixel shader 1.0–1.3.
    fn tex_ps1(&mut self, ins: &Instruction) -> Result<(), Error> {
        let d = ins.dst.unwrap();
        let n = d.reg.num;
        let tc = format!("tc{n}");
        let src0 = |g: &mut Self| -> Result<String, Error> { g.src(&ins.src[0]) };
        match ins.opcode {
            Opcode::TexBem | Opcode::TexBemL => {
                let s = src0(self)?;
                let m = format!("drv.bump_env[{n}]");
                // A projected stage divides the texture coordinate, not the
                // displacement.
                let proj = self.pkey.map(|k| k.samplers[n as usize].projected).unwrap_or(false);
                let base = if proj { format!("({tc}.xy / {tc}.w)") } else { format!("{tc}.xy") };
                let coord = format!(
                    "vec4<f32>({base}.x + {m}.x * {s}.x + {m}.z * {s}.y, {base}.y + {m}.y * {s}.x + {m}.w * {s}.y, 0.0, 1.0)"
                );
                let mut v = self.sample(n, &coord, Lod::Implicit, false)?;
                if ins.opcode == Opcode::TexBemL {
                    let l = format!("drv.bump_lum[{n}]");
                    v = format!("vec4<f32>(({v}).xyz * clamp({s}.z * {l}.x + {l}.y, 0.0, 1.0), ({v}).w)");
                }
                self.write(&d, None, &v)
            }
            Opcode::TexReg2Ar | Opcode::TexReg2Gb | Opcode::TexReg2Rgb => {
                let s = src0(self)?;
                let coord = match ins.opcode {
                    Opcode::TexReg2Ar => format!("vec4<f32>({s}.w, {s}.x, 0.0, 1.0)"),
                    Opcode::TexReg2Gb => format!("vec4<f32>({s}.y, {s}.z, 0.0, 1.0)"),
                    _ => format!("vec4<f32>({s}.xyz, 1.0)"),
                };
                let v = self.sample(n, &coord, Lod::Implicit, false)?;
                self.write(&d, None, &v)
            }
            Opcode::TexM3x2Pad | Opcode::TexM3x3Pad => {
                let s = src0(self)?;
                // Row k of the matrix lands in tmat component k.
                let k = self.pad_row(ins);
                self.line(&format!("tmat.{} = dot({tc}.xyz, {s}.xyz);", COMP[k]));
                self.line(&format!("teye.{} = {tc}.w;", COMP[k]));
                Ok(())
            }
            Opcode::TexM3x2Tex | Opcode::TexM3x2Depth => {
                let s = src0(self)?;
                self.line(&format!("tmat.y = dot({tc}.xyz, {s}.xyz);"));
                if ins.opcode == Opcode::TexM3x2Depth {
                    self.line("o_depth = vec4<f32>(select(tmat.y / tmat.x, 1.0, tmat.x == 0.0));");
                    self.write(&d, None, "vec4<f32>(tmat.x, tmat.y, 0.0, 1.0)")
                } else {
                    let v = self.sample(n, "vec4<f32>(tmat.xy, 0.0, 1.0)", Lod::Implicit, false)?;
                    self.write(&d, None, &v)
                }
            }
            Opcode::TexM3x3Tex | Opcode::TexM3x3 | Opcode::TexM3x3Spec | Opcode::TexM3x3VSpec => {
                let s = src0(self)?;
                self.line(&format!("tmat.z = dot({tc}.xyz, {s}.xyz);"));
                self.line(&format!("teye.z = {tc}.w;"));
                let v = match ins.opcode {
                    Opcode::TexM3x3 => "vec4<f32>(tmat.xyz, 1.0)".to_string(),
                    Opcode::TexM3x3Tex => self.sample(n, "vec4<f32>(tmat.xyz, 1.0)", Lod::Implicit, false)?,
                    _ => {
                        let eye = if ins.opcode == Opcode::TexM3x3Spec {
                            format!("{}.xyz", self.src(&ins.src[1])?)
                        } else {
                            "teye.xyz".to_string()
                        };
                        let r = self.fresh("refl");
                        self.line(&format!(
                            "let {r} = 2.0 * tmat.xyz * dot(tmat.xyz, {eye}) / max(dot(tmat.xyz, tmat.xyz), 1e-30) - {eye};"
                        ));
                        self.sample(n, &format!("vec4<f32>({r}, 1.0)"), Lod::Implicit, false)?
                    }
                };
                self.write(&d, None, &v)
            }
            Opcode::TexDp3 | Opcode::TexDp3Tex => {
                let s = src0(self)?;
                let dp = format!("dot({tc}.xyz, {s}.xyz)");
                let v = if ins.opcode == Opcode::TexDp3 {
                    format!("vec4<f32>({dp})")
                } else {
                    self.sample(n, &format!("vec4<f32>({dp}, 0.0, 0.0, 1.0)"), Lod::Implicit, false)?
                };
                self.write(&d, None, &v)
            }
            Opcode::TexDepth => {
                let r = format!("r{n}");
                self.line(&format!("o_depth = vec4<f32>(select({r}.x / {r}.y, 1.0, {r}.y == 0.0));"));
                Ok(())
            }
            op => Err(Error::Unsupported(format!("{} in pixel shader 1.x", op.mnemonic()))),
        }
    }

    /// Which matrix row a `texm3x*pad` fills: the first pad of a group is
    /// row 0, the second row 1.
    fn pad_row(&self, ins: &Instruction) -> usize {
        let idx = self.sh.instructions.iter().position(|i| std::ptr::eq(i, ins)).unwrap_or(0);
        if idx > 0 && matches!(self.sh.instructions[idx - 1].opcode, Opcode::TexM3x3Pad | Opcode::TexM3x2Pad) {
            1
        } else {
            0
        }
    }

    fn sampler_dim(&self, n: u32) -> SamplerDim {
        match self.sampler_dims.get(&n) {
            Some(TextureType::D2) => SamplerDim::D2,
            Some(TextureType::Cube) => SamplerDim::Cube,
            Some(TextureType::Volume) => SamplerDim::Volume,
            _ => self.pkey.and_then(|k| k.samplers.get(n as usize)).map(|s| s.dim).unwrap_or(SamplerDim::D2),
        }
    }

    fn sampler_key(&self, n: u32) -> SamplerKey {
        if n < 16 {
            if let Some(k) = self.pkey {
                return k.samplers[n as usize];
            }
        }
        SamplerKey::d2()
    }

    /// Emits a sample of sampler `n` at `coord` (a `vec4<f32>` expression)
    /// and returns the `vec4<f32>` result expression after the key's
    /// swizzle.
    fn sample(&mut self, n: u32, coord: &str, lod: Lod, project: bool) -> Result<String, Error> {
        let key = self.sampler_key(n);
        let dim = self.sampler_dim(n);
        let c = self.fresh("tc_");
        self.line(&format!("let {c} = {coord};"));
        let (ncomp, swz) = match dim {
            SamplerDim::D2 => (2, "xy"),
            SamplerDim::Cube | SamplerDim::Volume => (3, "xyz"),
        };
        let uv = if project { format!("({c}.{swz} / {c}.w)") } else { format!("{c}.{swz}") };
        let _ = ncomp;
        let (t, s) = (format!("tex{n}"), format!("smp{n}"));
        let r = self.fresh("smp_");
        let in_vs = !self.ps;
        let expr = if key.depth && key.compare {
            // WGSL has no textureSampleCompareGrad; level 0 also sidesteps
            // the uniformity rules.
            let reference = if project { format!("({c}.z / {c}.w)") } else { format!("{c}.z") };
            if dim == SamplerDim::Volume {
                return Err(Error::Unsupported("depth compare on a volume texture".into()));
            }
            format!("vec4<f32>(textureSampleCompareLevel({t}, {s}, {uv}, {reference}))")
        } else if key.depth {
            let call = match (&lod, in_vs) {
                (Lod::Implicit, false) | (Lod::Bias(_), false) => format!("textureSample({t}, {s}, {uv})"),
                (Lod::Level(l), _) => format!("textureSampleLevel({t}, {s}, {uv}, i32({l}))"),
                _ => format!("textureSampleLevel({t}, {s}, {uv}, 0)"),
            };
            format!("vec4<f32>({call})")
        } else {
            match (&lod, in_vs) {
                (Lod::Implicit, false) => format!("textureSample({t}, {s}, {uv})"),
                (Lod::Bias(b), false) => format!("textureSampleBias({t}, {s}, {uv}, {b})"),
                (Lod::Level(l), _) => format!("textureSampleLevel({t}, {s}, {uv}, {l})"),
                (Lod::Grad(dx, dy), false) => format!("textureSampleGrad({t}, {s}, {uv}, {dx}.{swz}, {dy}.{swz})"),
                _ => format!("textureSampleLevel({t}, {s}, {uv}, 0.0)"),
            }
        };
        self.line(&format!("let {r} = {expr};"));
        if key.swizzle == SamplerKey::IDENTITY || key.depth {
            return Ok(r);
        }
        let parts: Vec<String> = key
            .swizzle
            .iter()
            .map(|c| match c {
                0..=3 => format!("{r}.{}", COMP[*c as usize]),
                4 => "0.0".into(),
                _ => "1.0".into(),
            })
            .collect();
        Ok(format!("vec4<f32>({})", parts.join(", ")))
    }

    // ---- Module assembly ----

    fn finish(self) -> Result<Translation, Error> {
        let mut m = String::new();
        let sh = self.sh;
        let layout = ConstLayout::for_stage(sh.stage);
        if matches!(self.vkey.map(|k| k.clip), Some(ClipMode::Builtin(_))) {
            m += "enable clip_distances;\n\n";
        }
        if self.ps {
            m += "diagnostic(off, derivative_uniformity);\n\n";
        }
        let _ = writeln!(
            m,
            "struct Consts {{\n    f: array<vec4<f32>, {}>,\n    i: array<vec4<i32>, 16>,\n    b: array<vec4<u32>, 4>,\n}}",
            layout.float_count
        );
        let binding = if self.ps { 1 } else { 0 };
        let _ = writeln!(m, "@group(0) @binding({binding}) var<uniform> k: Consts;");
        m += DRIVER_WGSL;
        m.push('\n');

        // Textures.
        let mut textures = Vec::new();
        for (n, _) in &self.refl.samplers {
            let n = *n;
            let key = self.sampler_key(n);
            let dim = self.sampler_dim(n);
            let ty = match (dim, key.depth) {
                (SamplerDim::D2, false) => "texture_2d<f32>",
                (SamplerDim::Cube, false) => "texture_cube<f32>",
                (SamplerDim::Volume, false) => "texture_3d<f32>",
                (SamplerDim::D2, true) => "texture_depth_2d",
                (SamplerDim::Cube, true) => "texture_depth_cube",
                (SamplerDim::Volume, true) => "texture_3d<f32>",
            };
            let compare = key.depth && key.compare;
            let st = if compare { "sampler_comparison" } else { "sampler" };
            let _ = writeln!(m, "@group(1) @binding({}) var tex{n}: {ty};", texture_binding(n));
            let _ = writeln!(m, "@group(1) @binding({}) var smp{n}: {st};", sampler_binding(n));
            textures.push(TextureBinding { sampler: n, dim, depth: key.depth, compare });
        }
        m.push('\n');

        // Registers.
        for i in 0..self.temps {
            let _ = writeln!(m, "var<private> r{i}: vec4<f32>;");
        }
        m += "var<private> v: array<vec4<f32>, 16>;\n";
        if self.ps && sh.version.major < 3 {
            for i in 0..self.texregs {
                let _ = writeln!(m, "var<private> t{i}: vec4<f32>;");
            }
            if self.ps1 {
                for i in 0..8 {
                    let _ = writeln!(m, "var<private> tc{i}: vec4<f32>;");
                }
                m += "var<private> tmat: vec4<f32>;\nvar<private> teye: vec4<f32>;\n";
            }
        }
        if self.has_a0 {
            m += "var<private> a0: vec4<i32>;\n";
        }
        m += "var<private> aL: i32;\n";
        if self.has_p0 {
            m += "var<private> p0: vec4<bool>;\n";
        }
        if self.ps {
            m += "var<private> v_pos: vec4<f32>;\nvar<private> v_face: vec4<f32>;\n";
            for i in 0..4 {
                let _ = writeln!(m, "var<private> o_c{i}: vec4<f32>;");
            }
            m += "var<private> o_depth: vec4<f32>;\n";
        } else if sh.version.major >= 3 {
            m += "var<private> o: array<vec4<f32>, 12>;\n";
        } else {
            m += "var<private> o_pos: vec4<f32>;\nvar<private> o_fog: vec4<f32>;\nvar<private> o_pts: vec4<f32>;\n";
            // Unwritten diffuse components are 1, specular ones 0 (as
            // wined3d's GLSL backend has them; test_default_diffuse).
            m += "var<private> o_d0: vec4<f32> = vec4<f32>(1.0);\nvar<private> o_d1: vec4<f32>;\n";
            for i in 0..8 {
                let _ = writeln!(m, "var<private> o_t{i}: vec4<f32>;");
            }
        }
        m.push('\n');
        m += HELPERS;
        if self.uses_rel_const {
            m += "fn cf(i: i32) -> vec4<f32> {\n    switch i {\n";
            let mut defs: Vec<_> = self.defs.iter().collect();
            defs.sort_by_key(|(n, _)| **n);
            for (n, v) in defs {
                let _ = writeln!(m, "        case {n}: {{ return {}; }}", vec4f(*v));
            }
            let _ = writeln!(
                m,
                "        default: {{}}\n    }}\n    if (i < 0 || i >= {}) {{ return vec4<f32>(0.0); }}\n    return k.f[i];\n}}\n",
                layout.float_count
            );
        }
        for (name, body) in &self.funcs {
            let _ = write!(m, "fn {name}() {{\n{body}}}\n\n");
        }
        if !self.funcs.iter().any(|(n, _)| n == "shader_main") {
            m += "fn shader_main() {\n}\n\n";
        }

        let (varyings, color_outputs) = if self.ps { self.ps_entry(&mut m)? } else { self.vs_entry(&mut m)? };
        Ok(Translation { wgsl: m, entry: "main", textures, varyings, color_outputs })
    }

    fn vs_entry(&self, m: &mut String) -> Result<(u32, u8), Error> {
        let key = self.vkey.expect("vertex key");
        let sh = self.sh;
        let mut params = Vec::new();
        let mut prologue = String::new();
        for (reg, _) in &self.refl.vs_inputs {
            let r = *reg as usize;
            let kind = key.inputs.get(r).copied().unwrap_or_default();
            let (ty, conv) = match kind {
                InputKind::Float => ("vec4<f32>", format!("a{r}")),
                InputKind::FloatBgra => ("vec4<f32>", format!("a{r}.zyxw")),
                InputKind::Uint => ("vec4<u32>", format!("vec4<f32>(a{r})")),
                InputKind::Sint => ("vec4<i32>", format!("vec4<f32>(a{r})")),
                InputKind::Udec3 => ("u32", format!("d3d_udec3(a{r})")),
                InputKind::Dec3n => ("u32", format!("d3d_dec3n(a{r})")),
            };
            params.push(format!("@location({r}) a{r}: {ty}"));
            let _ = writeln!(prologue, "    v[{r}] = {conv};");
        }
        // Outputs.
        let _ = writeln!(m, "struct VsOut {{\n    @builtin(position) pos: vec4<f32>,");
        let mut loc = 0u32;
        let mut epilogue = String::new();
        for var in &key.outputs {
            let interp = match var.interp {
                Interp::Perspective => "",
                Interp::Centroid => " @interpolate(perspective, centroid)",
                Interp::Flat => " @interpolate(flat, first)",
            };
            let _ = writeln!(m, "    @location({loc}){interp} out{loc}: vec4<f32>,");
            let src = self.vs_output_source(var.semantic);
            let _ = writeln!(epilogue, "    out.out{loc} = {src};");
            loc += 1;
        }
        let pos = if sh.version.major >= 3 {
            match self.refl.vs_outputs.iter().position(|s| s.usage == usage::POSITION && s.index == 0) {
                Some(_) => self.vs_output_source(Semantic::new(usage::POSITION, 0)),
                None => "vec4<f32>(0.0, 0.0, 0.0, 1.0)".into(),
            }
        } else {
            "o_pos".into()
        };
        match key.clip {
            ClipMode::None => {}
            ClipMode::Builtin(mask) => {
                let _ = writeln!(m, "    @builtin(clip_distances) clip: array<f32, 6>,");
                for i in 0..6 {
                    let v = if mask & (1 << i) != 0 { format!("dot(pos, drv.clip_planes[{i}])") } else { "1.0".into() };
                    let _ = writeln!(epilogue, "    out.clip[{i}] = {v};");
                }
            }
            ClipMode::Varying(mask) => {
                let _ =
                    writeln!(m, "    @location({loc}) clip0: vec4<f32>,\n    @location({}) clip1: vec4<f32>,", loc + 1);
                let d = |i: usize| {
                    if mask & (1 << i) != 0 {
                        format!("dot(pos, drv.clip_planes[{i}])")
                    } else {
                        "1.0".into()
                    }
                };
                let _ = writeln!(epilogue, "    out.clip0 = vec4<f32>({}, {}, {}, {});", d(0), d(1), d(2), d(3));
                let _ = writeln!(epilogue, "    out.clip1 = vec4<f32>({}, {}, 1.0, 1.0);", d(4), d(5));
                loc += CLIP_VARYINGS;
            }
        }
        m.push_str("}\n\n");
        let _ = writeln!(m, "@vertex\nfn main({}) -> VsOut {{", params.join(", "));
        m.push_str(&prologue);
        m.push_str("    shader_main();\n    var out: VsOut;\n");
        let _ = writeln!(m, "    let pos = {pos};");
        if key.pos_fixup {
            m.push_str("    out.pos = vec4<f32>(pos.xy * drv.pos_fixup.xy + pos.w * drv.pos_fixup.zw, pos.zw);\n");
        } else {
            m.push_str("    out.pos = pos;\n");
        }
        m.push_str(&epilogue);
        m.push_str("    return out;\n}\n");
        Ok((loc, 0))
    }

    /// The expression a vertex shader writes for `sem`.
    fn vs_output_source(&self, sem: Semantic) -> String {
        if self.sh.version.major >= 3 {
            // Find the o# register declared with this semantic.
            for ins in &self.sh.instructions {
                if let (Opcode::Dcl, Some(Decl::Usage { usage, index }), Some(d)) = (ins.opcode, ins.decl, ins.dst) {
                    if d.reg.ty == RegType::Output && usage == sem.usage && index == sem.index {
                        let full = format!("o[{}]", d.reg.num);
                        // A partial declaration (dcl_fog o1.x) packs
                        // several semantics into one register.
                        if d.mask != MASK_ALL && d.mask != 0 {
                            let comps: Vec<String> = (0..4)
                                .filter(|c| d.mask & (1 << c) != 0)
                                .map(|c| format!("{full}.{}", COMP[c]))
                                .collect();
                            let mut v = comps.clone();
                            while v.len() < 4 {
                                v.push(if v.len() == 3 { "1.0".into() } else { "0.0".into() });
                            }
                            return format!("vec4<f32>({})", v.join(", "));
                        }
                        return full;
                    }
                }
            }
            return "vec4<f32>(0.0)".into();
        }
        let written = self.refl.vs_outputs.contains(&sem);
        if !written && sem.usage == usage::FOG {
            // No oFog: the fog factor is the specular alpha, as fixed
            // function's FOGVERTEXMODE NONE has it (wined3d's vertex
            // shaders leave the fog output to it); unwritten, it is 0 like
            // any output, which fogs fully (fog_with_shader_test).
            return if self.refl.vs_outputs.contains(&Semantic::new(usage::COLOR, 1)) {
                "vec4<f32>(clamp(o_d1.w, 0.0, 1.0), 0.0, 0.0, 1.0)".into()
            } else {
                "vec4<f32>(0.0, 0.0, 0.0, 1.0)".into()
            };
        }
        if !written {
            return if sem == Semantic::new(usage::COLOR, 0) { "vec4<f32>(1.0)" } else { "vec4<f32>(0.0)" }.into();
        }
        match sem.usage {
            usage::POSITION => "o_pos".into(),
            // Vertex shaders before 3.0 have their colour outputs clamped
            // to 0..1 per vertex, before interpolation.
            usage::COLOR => format!("clamp(o_d{}, vec4<f32>(0.0), vec4<f32>(1.0))", sem.index),
            usage::TEXCOORD => format!("o_t{}", sem.index),
            usage::FOG => "vec4<f32>(o_fog.x, 0.0, 0.0, 1.0)".into(),
            usage::PSIZE => "o_pts".into(),
            _ => "vec4<f32>(0.0)".into(),
        }
    }

    fn ps_entry(&self, m: &mut String) -> Result<(u32, u8), Error> {
        let key = self.pkey.expect("pixel key");
        let sh = self.sh;
        let linkage = ps_linkage(self.refl, key);
        let mut params = vec!["@builtin(position) frag_pos: vec4<f32>".to_string()];
        if self.refl.uses_vface {
            params.push("@builtin(front_facing) front: bool".into());
        }
        let mut prologue = String::new();
        let mut loc = 0u32;
        for var in &linkage {
            let interp = match var.interp {
                Interp::Perspective => "",
                Interp::Centroid => " @interpolate(perspective, centroid)",
                Interp::Flat => " @interpolate(flat, first)",
            };
            params.push(format!("@location({loc}){interp} in{loc}: vec4<f32>"));
            let sem = var.semantic;
            if sh.version.major >= 3 {
                for ins in &sh.instructions {
                    if let (Opcode::Dcl, Some(Decl::Usage { usage, index }), Some(d)) = (ins.opcode, ins.decl, ins.dst)
                    {
                        if d.reg.ty == RegType::Input && usage == sem.usage && index == sem.index {
                            let _ = writeln!(prologue, "    v[{}] = in{loc};", d.reg.num);
                        }
                    }
                }
            } else if sem.usage == usage::COLOR {
                let _ = writeln!(prologue, "    v[{}] = in{loc};", sem.index);
            } else if sem.usage == usage::TEXCOORD {
                let _ = writeln!(prologue, "    t{0} = in{loc};", sem.index);
                if self.ps1 {
                    let _ = writeln!(prologue, "    tc{0} = in{loc};", sem.index);
                }
            }
            if sem.usage == usage::FOG && key.fog.uses_varying() {
                let _ = writeln!(prologue, "    let fog_in = in{loc}.x;");
            }
            loc += 1;
        }
        if let ClipMode::Varying(_) = key.clip {
            params.push(format!("@location({loc}) clip0: vec4<f32>"));
            params.push(format!("@location({}) clip1: vec4<f32>", loc + 1));
            loc += CLIP_VARYINGS;
        }
        let mut outputs = self.refl.color_outputs;
        if self.ps1 {
            outputs = 1;
        }
        // Output struct.
        let shader_depth = self.refl.writes_depth || self.ps1_writes_depth();
        // Depth bias applies to the rasterized depth, not a shader's own.
        let bias = key.depth_bias && !shader_depth;
        let has_out = outputs != 0 || shader_depth || bias;
        if has_out {
            m.push_str("struct PsOut {\n");
            for i in 0..4 {
                if outputs & (1 << i) != 0 {
                    let _ = writeln!(m, "    @location({i}) c{i}: vec4<f32>,");
                }
            }
            if shader_depth || bias {
                m.push_str("    @builtin(frag_depth) depth: f32,\n");
            }
            m.push_str("}\n\n");
        }
        let ret = if has_out { " -> PsOut" } else { "" };
        let _ = writeln!(m, "@fragment\nfn main({}){ret} {{", params.join(", "));
        // The biased depth (Direct3D: DEPTHBIAS + SLOPESCALEDEPTHBIAS times
        // the depth's largest slope), before any discard. Table fog reads it
        // as is; the depth buffer gets it clamped.
        if bias {
            m.push_str("    let z_slope = max(abs(dpdx(frag_pos.z)), abs(dpdy(frag_pos.z)));\n");
            m.push_str("    let z_biased = frag_pos.z + drv.depth_bias.x + drv.depth_bias.y * z_slope;\n");
        }
        let z = if bias { "z_biased" } else { "frag_pos.z" };
        if let ClipMode::Varying(mask) = key.clip {
            for i in 0..6 {
                if mask & (1 << i) != 0 {
                    let c = if i < 4 { format!("clip0.{}", COMP[i]) } else { format!("clip1.{}", COMP[i - 4]) };
                    let _ = writeln!(m, "    if ({c} < 0.0) {{ discard; }}");
                }
            }
        }
        m.push_str("    v_pos = vec4<f32>(frag_pos.xy - vec2<f32>(0.5), 0.0, 0.0);\n");
        if self.refl.uses_vface {
            m.push_str("    v_face = vec4<f32>(select(-1.0, 1.0, front));\n");
        }
        if self.ps1 {
            // Pixel shader 1.x output is r0; its depth defaults to the
            // rasterized depth.
            m.push_str("    o_depth = vec4<f32>(frag_pos.z);\n");
        }
        m.push_str(&prologue);
        m.push_str("    shader_main();\n");
        if self.ps1 {
            m.push_str("    o_c0 = r0;\n");
        }
        // Fog after shaders before 3.0, as fixed function does.
        if sh.version.major < 3 && outputs & 1 != 0 {
            let f = match key.fog {
                Fog::None => None,
                Fog::Vertex => Some(
                    if linkage.iter().any(|v| v.semantic.usage == usage::FOG) { "fog_in" } else { "1.0" }.to_string(),
                ),
                // Table fog: eye depth (W; the fragment position's w is
                // its reciprocal) or pixel Z.
                Fog::Linear | Fog::Exp | Fog::Exp2 => {
                    let d = if key.fog_w { "(1.0 / frag_pos.w)" } else { z };
                    Some(match key.fog {
                        Fog::Linear => format!("(drv.fog_params.y - {d}) * drv.fog_params.w"),
                        Fog::Exp => format!("exp(-drv.fog_params.z * {d})"),
                        _ => format!("exp(-(drv.fog_params.z * {d}) * (drv.fog_params.z * {d}))"),
                    })
                }
                Fog::VertexLinear | Fog::VertexExp | Fog::VertexExp2 => {
                    let c = if linkage.iter().any(|v| v.semantic.usage == usage::FOG) { "fog_in" } else { "0.0" };
                    Some(match key.fog {
                        Fog::VertexLinear => format!("(drv.fog_params.y - {c}) * drv.fog_params.w"),
                        Fog::VertexExp => format!("exp(-drv.fog_params.z * {c})"),
                        _ => format!("exp(-(drv.fog_params.z * {c}) * (drv.fog_params.z * {c}))"),
                    })
                }
            };
            if let Some(f) = f {
                let _ = writeln!(m, "    let fog = clamp({f}, 0.0, 1.0);");
                m.push_str("    o_c0 = vec4<f32>(mix(drv.fog_color.xyz, o_c0.xyz, fog), o_c0.w);\n");
            }
        }
        if key.srgb_write && outputs & 1 != 0 {
            m.push_str("    let srgb_c = clamp(o_c0.xyz, vec3<f32>(0.0), vec3<f32>(1.0));\n");
            m.push_str("    o_c0 = vec4<f32>(select(1.055 * pow(srgb_c, vec3<f32>(1.0 / 2.4)) - 0.055, srgb_c * 12.92, srgb_c <= vec3<f32>(0.0031308)), o_c0.w);\n");
        }
        if key.alpha_test != cmp::ALWAYS && outputs & 1 != 0 {
            // Direct3D compares the 8-bit alpha with the 8-bit reference.
            let a = "round(clamp(o_c0.w, 0.0, 1.0) * 255.0)";
            let pass = match key.alpha_test {
                cmp::NEVER => "false".to_string(),
                cmp::LESS => format!("{a} < drv.alpha_ref.x"),
                cmp::EQUAL => format!("{a} == drv.alpha_ref.x"),
                cmp::LESS_EQUAL => format!("{a} <= drv.alpha_ref.x"),
                cmp::GREATER => format!("{a} > drv.alpha_ref.x"),
                cmp::NOT_EQUAL => format!("{a} != drv.alpha_ref.x"),
                cmp::GREATER_EQUAL => format!("{a} >= drv.alpha_ref.x"),
                _ => "true".to_string(),
            };
            let _ = writeln!(m, "    if (!({pass})) {{ discard; }}");
        }
        if has_out {
            m.push_str("    var out: PsOut;\n");
            for i in 0..4 {
                if outputs & (1 << i) != 0 {
                    let _ = writeln!(m, "    out.c{i} = o_c{i};");
                }
            }
            if shader_depth {
                m.push_str("    out.depth = clamp(o_depth.x, 0.0, 1.0);\n");
            } else if bias {
                m.push_str("    out.depth = clamp(z_biased, 0.0, 1.0);\n");
            }
            m.push_str("    return out;\n");
        }
        m.push_str("}\n");
        Ok((loc, outputs))
    }

    fn ps1_writes_depth(&self) -> bool {
        self.ps1 && self.sh.instructions.iter().any(|i| matches!(i.opcode, Opcode::TexDepth | Opcode::TexM3x2Depth))
    }
}

enum Lod {
    Implicit,
    Bias(String),
    Level(String),
    Grad(String, String),
}

const HELPERS: &str = "var<private> d3d_zero: u32 = 0u;
fn d3d_rcp(x: f32) -> f32 {
    return select(1.0 / x, 3.402823466e+38, x == 0.0);
}
fn d3d_rsq(x: f32) -> f32 {
    let a = abs(x);
    return select(inverseSqrt(a), 3.402823466e+38, a == 0.0);
}
fn d3d_log(x: f32) -> f32 {
    let a = abs(x);
    return select(log2(a), -3.402823466e+38, a == 0.0);
}
fn d3d_pow(x: f32, y: f32) -> f32 {
    let a = abs(x);
    if (a == 0.0) {
        return select(select(0.0, 3.402823466e+38, y < 0.0), 1.0, y == 0.0);
    }
    return exp2(y * log2(a));
}
fn d3d_expp(x: f32) -> vec4<f32> {
    let f = floor(x);
    return vec4<f32>(exp2(f), x - f, exp2(x), 1.0);
}
fn d3d_logp(x: f32) -> vec4<f32> {
    let a = abs(x);
    if (a == 0.0) {
        return vec4<f32>(-3.402823466e+38, 1.0, -3.402823466e+38, 1.0);
    }
    let e = floor(log2(a));
    return vec4<f32>(e, a / exp2(e), log2(a), 1.0);
}
fn d3d_lit(s: vec4<f32>) -> vec4<f32> {
    var r = vec4<f32>(1.0, 0.0, 0.0, 1.0);
    if (s.x > 0.0) {
        r.y = s.x;
        if (s.y > 0.0) {
            r.z = d3d_pow(s.y, clamp(s.w, -127.9961, 127.9961));
        }
    }
    return r;
}
fn d3d_nrm(v: vec4<f32>) -> vec4<f32> {
    let l = dot(v.xyz, v.xyz);
    return select(v * inverseSqrt(l), vec4<f32>(0.0), l == 0.0);
}
fn d3d_div(v: vec4<f32>, d: f32) -> vec4<f32> {
    return select(v / d, v, d == 0.0);
}
fn d3d_udec3(p: u32) -> vec4<f32> {
    return vec4<f32>(f32(p & 1023u), f32((p >> 10u) & 1023u), f32((p >> 20u) & 1023u), 1.0);
}
fn d3d_dec3n(p: u32) -> vec4<f32> {
    let x = (i32(p << 22u) >> 22u);
    let y = (i32(p << 12u) >> 22u);
    let z = (i32(p << 2u) >> 22u);
    return vec4<f32>(max(vec3<f32>(f32(x), f32(y), f32(z)) / 511.0, vec3<f32>(-1.0)), 1.0);
}

";
