//! Shader model 4/5 → WGSL.
//!
//! Every register (`r#`, `v#`, `o#`, `x#[]`, constant buffers) is
//! `vec4<u32>`; instructions bitcast their sources to the type they
//! operate on and their results back. Control flow is structured and maps
//! one to one. Resource bindings follow a fixed scheme per stage:
//!
//! | Group | Contents |
//! | --- | --- |
//! | 0 | vertex or compute shader resources |
//! | 1 | pixel shader resources |
//! | 2 | driver uniforms (binding 0) |
//!
//! Within a group, constant buffer `n` is binding `n`, shader resource `n`
//! is `16 + n`, sampler `n` is `144 + n` and UAV `n` is `160 + n`.

#![allow(clippy::needless_range_loop)]

use std::fmt::Write;

use crate::container::{sv, ComponentType, SigElement};
use crate::decode::*;
use crate::reflect::{InterpMode, Reflection};
use crate::{Error, Shader};

pub const CB_BASE: u32 = 0;
pub const SRV_BASE: u32 = 16;
pub const SAMPLER_BASE: u32 = 144;
pub const UAV_BASE: u32 = 160;
pub const DRIVER_GROUP: u32 = 2;

/// WGSL declaration of the driver uniforms.
pub const DRIVER_WGSL: &str = "struct Driver {
    // Viewport clamp fixup: pos.xy = pos.xy * pos_fixup.xy + pos.w * pos_fixup.zw.
    pos_fixup: vec4<f32>,
    // x: StartInstanceLocation (SV_InstanceID excludes it, WebGPU's
    // instance_index doesn't); y: the base vertex to subtract from
    // vertex_index for SV_VertexID.
    misc: vec4<u32>,
}
@group(2) @binding(0) var<uniform> drv: Driver;
";

/// Size of the driver uniform block in bytes.
pub const DRIVER_SIZE: u64 = 32;

/// Element formats of typed buffers (`Buffer<T>`, `RWBuffer<T>`), which
/// WebGPU doesn't have: they become storage buffers decoded in the shader.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum BufferFormat {
    #[default]
    R32Uint,
    R32Sint,
    R32Float,
    Rg32Uint,
    Rg32Sint,
    Rg32Float,
    Rgb32Uint,
    Rgb32Sint,
    Rgb32Float,
    Rgba32Uint,
    Rgba32Sint,
    Rgba32Float,
    Rgba8Unorm,
    Rgba8Uint,
    Rgba16Float,
    R16Float,
    Rg16Float,
}

impl BufferFormat {
    /// 32-bit words per element.
    pub fn words(self) -> u32 {
        use BufferFormat::*;
        match self {
            R32Uint | R32Sint | R32Float | Rgba8Unorm | Rgba8Uint | R16Float | Rg16Float => 1,
            Rg32Uint | Rg32Sint | Rg32Float | Rgba16Float => 2,
            Rgb32Uint | Rgb32Sint | Rgb32Float => 3,
            Rgba32Uint | Rgba32Sint | Rgba32Float => 4,
        }
    }
}

/// Storage texture formats WebGPU offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum StorageFormat {
    #[default]
    Rgba8Unorm,
    Rgba8Snorm,
    Rgba8Uint,
    Rgba8Sint,
    Rgba16Uint,
    Rgba16Sint,
    Rgba16Float,
    R32Uint,
    R32Sint,
    R32Float,
    Rg32Uint,
    Rg32Sint,
    Rg32Float,
    Rgba32Uint,
    Rgba32Sint,
    Rgba32Float,
    Bgra8Unorm,
}

impl StorageFormat {
    pub fn wgsl(self) -> &'static str {
        use StorageFormat::*;
        match self {
            Rgba8Unorm => "rgba8unorm",
            Rgba8Snorm => "rgba8snorm",
            Rgba8Uint => "rgba8uint",
            Rgba8Sint => "rgba8sint",
            Rgba16Uint => "rgba16uint",
            Rgba16Sint => "rgba16sint",
            Rgba16Float => "rgba16float",
            R32Uint => "r32uint",
            R32Sint => "r32sint",
            R32Float => "r32float",
            Rg32Uint => "rg32uint",
            Rg32Sint => "rg32sint",
            Rg32Float => "rg32float",
            Rgba32Uint => "rgba32uint",
            Rgba32Sint => "rgba32sint",
            Rgba32Float => "rgba32float",
            Bgra8Unorm => "bgra8unorm",
        }
    }
    fn ty(self) -> Ty {
        use StorageFormat::*;
        match self {
            Rgba8Uint | Rgba16Uint | R32Uint | Rg32Uint | Rgba32Uint => Ty::U,
            Rgba8Sint | Rgba16Sint | R32Sint | Rg32Sint | Rgba32Sint => Ty::I,
            _ => Ty::F,
        }
    }
    /// Read-write access is only allowed on single-channel 32-bit formats.
    pub fn read_write_ok(self) -> bool {
        matches!(self, StorageFormat::R32Uint | StorageFormat::R32Sint | StorageFormat::R32Float)
    }
}

/// What the runtime binds to a shader resource slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SrvKind {
    /// A texture; `depth` when it has a depth format (sampled as a depth
    /// texture; needed for comparison sampling).
    Texture { depth: bool },
    /// A typed buffer.
    Buffer(BufferFormat),
}

/// What the runtime binds to a UAV slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UavKind {
    Texture(StorageFormat),
    Buffer(BufferFormat),
}

/// Interpolation of a varying (must match between stages).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Interp {
    Perspective,
    PerspectiveCentroid,
    PerspectiveSample,
    Linear,
    LinearCentroid,
    LinearSample,
    /// Constant interpolation; the varying carries raw bits as `vec4<u32>`.
    Flat,
}

impl Interp {
    fn attr(self) -> &'static str {
        match self {
            Interp::Perspective => "",
            Interp::PerspectiveCentroid => " @interpolate(perspective, centroid)",
            Interp::PerspectiveSample => " @interpolate(perspective, sample)",
            Interp::Linear => " @interpolate(linear)",
            Interp::LinearCentroid => " @interpolate(linear, centroid)",
            Interp::LinearSample => " @interpolate(linear, sample)",
            Interp::Flat => " @interpolate(flat)",
        }
    }
    fn ty(self) -> &'static str {
        if self == Interp::Flat {
            "vec4<u32>"
        } else {
            "vec4<f32>"
        }
    }
}

/// One semantic packed into a varying.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LinkElement {
    pub name: String,
    pub index: u32,
    /// Components it occupies in the varying.
    pub mask: u8,
}

/// One inter-stage varying (a pixel shader input register).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LinkSlot {
    pub interp: Interp,
    pub elements: Vec<LinkElement>,
}

/// User clip/cull distances.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Clip {
    #[default]
    None,
    /// `@builtin(clip_distances)` with this many distances.
    Builtin(u8),
    /// Two extra varyings and a `discard` in the pixel shader.
    Varying(u8),
}

/// Everything a translation depends on besides the bytecode.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct Key {
    /// Bound shader resource kinds by slot (unlisted: non-depth texture or
    /// `R32Uint` buffer).
    pub srvs: Vec<(u32, SrvKind)>,
    /// Bound UAV kinds by slot (unlisted: `R32Uint` buffer or `rgba8unorm`
    /// storage texture).
    pub uavs: Vec<(u32, UavKind)>,
    /// Vertex shaders: the varyings to write, in location order (the pixel
    /// shader's [`Shader::linkage`]). `None` writes every output register
    /// in register order.
    pub outputs: Option<Vec<LinkSlot>>,
    pub clip: Clip,
    /// Apply `Driver::pos_fixup` to the output position.
    pub pos_fixup: bool,
}

/// A texture's dimension in WGSL terms.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TexDim {
    D2,
    D2Array,
    D3,
    Cube,
    CubeArray,
}

/// Sample type of a texture binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TexSample {
    Float,
    Depth,
    Uint,
    Sint,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BindingType {
    /// A constant buffer of `size` bytes, bound with a dynamic offset.
    ConstantBuffer {
        size: u32,
    },
    Texture {
        dim: TexDim,
        sample: TexSample,
        multisampled: bool,
    },
    Sampler {
        comparison: bool,
    },
    StorageBuffer {
        read_only: bool,
    },
    StorageTexture {
        format: StorageFormat,
        dim: TexDim,
        read: bool,
        write: bool,
    },
}

/// One binding the translated shader declares.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Binding {
    pub group: u32,
    pub binding: u32,
    /// The Direct3D slot (`b#`, `t#`, `s#`, `u#`).
    pub slot: u32,
    pub ty: BindingType,
}

/// A vertex shader input the input layout must feed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VertexInput {
    pub location: u32,
    pub name: String,
    pub index: u32,
    pub ty: ComponentType,
}

#[derive(Clone, Debug)]
pub struct Translation {
    pub wgsl: String,
    pub entry: &'static str,
    pub bindings: Vec<Binding>,
    pub vertex_inputs: Vec<VertexInput>,
    /// Varyings written (vertex) or read (pixel), including clip varyings.
    pub varyings: u32,
    /// Pixel shaders: render targets written (bit per target) and their types.
    pub targets: Vec<(u32, ComponentType)>,
    pub workgroup_size: [u32; 3],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ty {
    F,
    I,
    U,
}

impl Ty {
    fn s(self) -> &'static str {
        match self {
            Ty::F => "f32",
            Ty::I => "i32",
            Ty::U => "u32",
        }
    }
    fn v(self) -> String {
        format!("vec4<{}>", self.s())
    }
}

const C: [char; 4] = ['x', 'y', 'z', 'w'];

/// The varyings a pixel shader reads, in location order.
pub fn ps_linkage(r: &Reflection) -> Vec<LinkSlot> {
    let mut slots: Vec<(u32, LinkSlot)> = Vec::new();
    for d in &r.input_decls {
        if d.sv.is_some() {
            continue;
        }
        let elems: Vec<&SigElement> = r.input_elements(d.reg).filter(|e| e.system_value == sv::UNDEFINED).collect();
        if elems.is_empty() || slots.iter().any(|(reg, _)| *reg == d.reg) {
            continue;
        }
        let int = elems.iter().any(|e| e.component_type != ComponentType::Float);
        let interp = if int {
            Interp::Flat
        } else {
            match d.interp {
                InterpMode::Constant => Interp::Flat,
                InterpMode::LinearCentroid => Interp::PerspectiveCentroid,
                InterpMode::LinearSample => Interp::PerspectiveSample,
                InterpMode::LinearNoPerspective => Interp::Linear,
                InterpMode::LinearNoPerspectiveCentroid => Interp::LinearCentroid,
                InterpMode::LinearNoPerspectiveSample => Interp::LinearSample,
                _ => Interp::Perspective,
            }
        };
        let elements =
            elems.iter().map(|e| LinkElement { name: e.name.clone(), index: e.index, mask: e.mask }).collect();
        slots.push((d.reg, LinkSlot { interp, elements }));
    }
    slots.sort_by_key(|(r, _)| *r);
    slots.into_iter().map(|(_, s)| s).collect()
}

pub fn translate(sh: &Shader, key: &Key) -> Result<Translation, Error> {
    let r = &sh.reflection;
    let p = &sh.program;
    if !matches!(p.ty, ProgramType::Vertex | ProgramType::Pixel | ProgramType::Compute) {
        return Err(Error::Unsupported(format!("{:?} shaders (WebGPU has no such stage)", p.ty)));
    }
    if let Some(op) = r.unsupported.first() {
        return Err(Error::Unsupported(format!("instruction {}", op.name())));
    }
    let g = Gen {
        sh,
        key,
        ps: p.ty == ProgramType::Pixel,
        cs: p.ty == ProgramType::Compute,
        group: if p.ty == ProgramType::Pixel { 1 } else { 0 },
        body: String::new(),
        funcs: Vec::new(),
        indent: 1,
        tmp: 0,
        switch: Vec::new(),
        labels_started: false,
        bindings: Vec::new(),
    };
    g.run()
}

struct SwitchState {
    pending: Vec<String>,
    open: bool,
    has_default: bool,
}

struct Gen<'a> {
    sh: &'a Shader,
    key: &'a Key,
    ps: bool,
    cs: bool,
    group: u32,
    body: String,
    funcs: Vec<(String, String)>,
    indent: usize,
    tmp: usize,
    switch: Vec<SwitchState>,
    labels_started: bool,
    bindings: Vec<Binding>,
}

fn f32_lit(bits: u32) -> String {
    let f = f32::from_bits(bits);
    if f.is_finite() && (f == 0.0 && bits == 0 || f.is_normal()) {
        let s = format!("{f:?}");
        if s.parse::<f32>().ok().map(f32::to_bits) == Some(bits) {
            return if s.contains('e') && !s.contains('.') { s.replacen('e', ".0e", 1) } else { s };
        }
    }
    format!("bitcast<f32>({bits}u)")
}

fn lit(bits: u32, ty: Ty) -> String {
    match ty {
        Ty::F => f32_lit(bits),
        Ty::I => {
            let v = bits as i32;
            if v == i32::MIN {
                "i32(-2147483648)".into()
            } else {
                format!("{v}i")
            }
        }
        Ty::U => format!("{bits}u"),
    }
}

impl<'a> Gen<'a> {
    fn line(&mut self, s: &str) {
        for _ in 0..self.indent {
            self.body.push_str("    ");
        }
        self.body.push_str(s);
        self.body.push('\n');
    }

    /// A fresh local name; the underscore keeps it apart from register
    /// and resource names (`t1`, `s0`, `r3`).
    fn fresh(&mut self, b: &str) -> String {
        self.tmp += 1;
        format!("{b}_{}", self.tmp)
    }

    fn srv_kind(&self, slot: u32) -> SrvKind {
        self.key.srvs.iter().find(|(s, _)| *s == slot).map(|(_, k)| *k).unwrap_or(SrvKind::Texture { depth: false })
    }

    fn uav_kind(&self, slot: u32) -> UavKind {
        let d = self.sh.reflection.uav(slot);
        let default = if d.is_some_and(|d| {
            d.dim != ResourceDim::Buffer && d.dim != ResourceDim::RawBuffer && d.dim != ResourceDim::StructuredBuffer
        }) {
            UavKind::Texture(StorageFormat::Rgba8Unorm)
        } else {
            UavKind::Buffer(BufferFormat::R32Uint)
        };
        self.key.uavs.iter().find(|(s, _)| *s == slot).map(|(_, k)| *k).unwrap_or(default)
    }

    fn depth_srv(&self, slot: u32) -> bool {
        let compared = self.sh.reflection.resource(slot).is_some_and(|d| d.compared);
        compared || matches!(self.srv_kind(slot), SrvKind::Texture { depth: true })
    }

    // ---- Operands ----

    fn index(&mut self, i: &Index) -> Result<String, Error> {
        Ok(match &i.rel {
            None => format!("{}u", i.imm),
            Some(r) => {
                let v = self.src(r, Ty::U)?;
                if i.imm == 0 {
                    format!("({v}).x")
                } else {
                    format!("(({v}).x + {}u)", i.imm)
                }
            }
        })
    }

    /// The `vec4<u32>` storage an operand names.
    fn base(&mut self, o: &Operand) -> Result<String, Error> {
        let idx0 = || o.indices.first().cloned().unwrap_or(Index { imm: 0, rel: None });
        Ok(match o.ty {
            RegType::Temp => format!("r{}", o.reg()),
            RegType::Input => format!("v[{}]", self.index(&idx0())?),
            RegType::Output => format!("o[{}]", self.index(&idx0())?),
            RegType::IndexableTemp => {
                let i = o.indices.get(1).cloned().ok_or_else(|| Error::Unsupported("x# without index".into()))?;
                format!("x{}[{}]", o.reg(), self.index(&i)?)
            }
            RegType::ConstantBuffer => {
                let i = o.indices.get(1).cloned().ok_or_else(|| Error::Unsupported("cb# without index".into()))?;
                let idx = self.index(&i)?;
                format!("cb{}[min({idx}, {}u)]", o.reg(), self.cb_size(o.reg()) - 1)
            }
            RegType::ImmediateConstantBuffer => {
                let n = self.sh.reflection.icb.len().max(1);
                format!("icb[min({}, {}u)]", self.index(&idx0())?, n - 1)
            }
            RegType::InputThreadId => "vtid".into(),
            RegType::InputThreadGroupId => "vgid".into(),
            RegType::InputThreadIdInGroup => "vlid".into(),
            RegType::InputThreadIdInGroupFlattened => "vlidx".into(),
            RegType::InputCoverageMask => "vcov".into(),
            RegType::InputPrimitiveId => "vprim".into(),
            RegType::OutputDepth | RegType::OutputDepthGreaterEqual | RegType::OutputDepthLessEqual => "odepth".into(),
            RegType::OutputCoverageMask => "omask".into(),
            t => return Err(Error::Unsupported(format!("operand {t:?}"))),
        })
    }

    fn cb_size(&self, slot: u32) -> u32 {
        self.sh.reflection.cbuffers.iter().find(|(s, _)| *s == slot).map(|(_, n)| *n).unwrap_or(1).max(1)
    }

    fn swizzle(o: &Operand) -> String {
        let s: String = (0..4).map(|i| C[o.select.comp(i) as usize]).collect();
        if s == "xyzw" {
            String::new()
        } else {
            format!(".{s}")
        }
    }

    /// A source operand as `vec4<ty>`.
    fn src(&mut self, o: &Operand, ty: Ty) -> Result<String, Error> {
        let v = if o.ty == RegType::Immediate32 {
            let comps: Vec<String> = (0..4)
                .map(|i| {
                    let c = if o.imm.len() == 1 { 0 } else { o.select.comp(i) as usize };
                    lit(o.imm[c.min(o.imm.len() - 1)], ty)
                })
                .collect();
            format!("{}({})", ty.v(), comps.join(", "))
        } else {
            let b = self.base(o)?;
            let sw = Self::swizzle(o);
            match ty {
                Ty::U => format!("{b}{sw}"),
                _ => format!("bitcast<{}>({b}{sw})", ty.v()),
            }
        };
        Ok(match (o.modifier, ty) {
            (Modifier::None, _) => v,
            (Modifier::Neg, Ty::U) => format!("(vec4<u32>(0u) - {v})"),
            (Modifier::Neg, _) => format!("(-{v})"),
            (Modifier::Abs, Ty::F) | (Modifier::Abs, Ty::I) => format!("abs({v})"),
            (Modifier::AbsNeg, Ty::F) | (Modifier::AbsNeg, Ty::I) => format!("(-abs({v}))"),
            (_, Ty::U) => v,
        })
    }

    /// Writes a `vec4<ty>` value to a destination operand.
    fn write(&mut self, o: &Operand, value: &str, ty: Ty, saturate: bool) -> Result<(), Error> {
        if o.ty == RegType::Null {
            return Ok(());
        }
        let v = if saturate && ty == Ty::F {
            format!("clamp({value}, vec4<f32>(0.0), vec4<f32>(1.0))")
        } else {
            value.to_string()
        };
        let bits = if ty == Ty::U { v } else { format!("bitcast<vec4<u32>>({v})") };
        let lhs = self.base(o)?;
        let mask = if o.comps == 1 { 1 } else { o.select.mask() };
        if mask == 0xf {
            self.line(&format!("{lhs} = {bits};"));
            return Ok(());
        }
        let t = self.fresh("t");
        self.line(&format!("let {t} = {bits};"));
        for c in 0..4 {
            if mask & (1 << c) != 0 {
                self.line(&format!("{lhs}.{} = {t}.{};", C[c], C[c]));
            }
        }
        Ok(())
    }

    fn cond(&mut self, ins: &Instruction, o: &Operand) -> Result<String, Error> {
        let v = self.src(o, Ty::U)?;
        Ok(if ins.test_nz { format!("({v}).x != 0u") } else { format!("({v}).x == 0u") })
    }

    // ---- Instructions ----

    fn run(mut self) -> Result<Translation, Error> {
        let p = &self.sh.program;
        for ins in &p.instructions {
            self.instruction(ins)?;
        }
        if let Some(s) = self.switch.last() {
            let _ = s;
            return Err(Error::Unsupported("unterminated switch".into()));
        }
        self.finish_function();
        self.module()
    }

    fn finish_function(&mut self) {
        let body = std::mem::take(&mut self.body);
        let name = if self.labels_started {
            self.funcs.last().map(|f| f.0.clone()).unwrap_or_default()
        } else {
            "shader_main".into()
        };
        if let Some(f) = self.funcs.iter_mut().find(|f| f.0 == name) {
            f.1 = body;
        } else {
            self.funcs.push((name, body));
        }
    }

    fn instruction(&mut self, ins: &Instruction) -> Result<(), Error> {
        use Opcode::*;
        let o = &ins.operands;
        // Switch case labels gather until the first statement of the body.
        if let Some(sw) = self.switch.last_mut() {
            match ins.op {
                Case | Default | EndSwitch => {}
                _ if !sw.pending.is_empty() => {
                    let labels = std::mem::take(&mut sw.pending).join(", ");
                    sw.open = true;
                    self.line(&format!("case {labels}: {{"));
                    self.indent += 1;
                }
                _ => {}
            }
        }
        if ins.op.name().starts_with("dcl_") || matches!(ins.op, CustomData | Nop | HsDecls) {
            return Ok(());
        }
        let sat = ins.saturate;
        macro_rules! s {
            ($i:expr, $t:expr) => {
                self.src(&o[$i], $t)?
            };
        }
        // Per-component ALU operations: (dst type, expression).
        let alu: Option<(Ty, String)> = match ins.op {
            Add => Some((Ty::F, format!("({} + {})", s!(1, Ty::F), s!(2, Ty::F)))),
            Mul => Some((Ty::F, format!("({} * {})", s!(1, Ty::F), s!(2, Ty::F)))),
            Div => Some((Ty::F, format!("({} / {})", s!(1, Ty::F), s!(2, Ty::F)))),
            Mad => Some((Ty::F, format!("({} * {} + {})", s!(1, Ty::F), s!(2, Ty::F), s!(3, Ty::F)))),
            Min => Some((Ty::F, format!("min({}, {})", s!(1, Ty::F), s!(2, Ty::F)))),
            Max => Some((Ty::F, format!("max({}, {})", s!(1, Ty::F), s!(2, Ty::F)))),
            Dp2 => Some((Ty::F, format!("vec4<f32>(dot(({}).xy, ({}).xy))", s!(1, Ty::F), s!(2, Ty::F)))),
            Dp3 => Some((Ty::F, format!("vec4<f32>(dot(({}).xyz, ({}).xyz))", s!(1, Ty::F), s!(2, Ty::F)))),
            Dp4 => Some((Ty::F, format!("vec4<f32>(dot({}, {}))", s!(1, Ty::F), s!(2, Ty::F)))),
            Exp => Some((Ty::F, format!("exp2({})", s!(1, Ty::F)))),
            Log => Some((Ty::F, format!("log2({})", s!(1, Ty::F)))),
            Sqrt => Some((Ty::F, format!("sqrt({})", s!(1, Ty::F)))),
            Rsq => Some((Ty::F, format!("inverseSqrt({})", s!(1, Ty::F)))),
            Rcp => Some((Ty::F, format!("(vec4<f32>(1.0) / {})", s!(1, Ty::F)))),
            Frc => Some((Ty::F, format!("fract({})", s!(1, Ty::F)))),
            RoundNe => Some((Ty::F, format!("round({})", s!(1, Ty::F)))),
            RoundNi => Some((Ty::F, format!("floor({})", s!(1, Ty::F)))),
            RoundPi => Some((Ty::F, format!("ceil({})", s!(1, Ty::F)))),
            RoundZ => Some((Ty::F, format!("trunc({})", s!(1, Ty::F)))),
            DerivRtx | DerivRtxCoarse | DerivRtxFine | DerivRty | DerivRtyCoarse | DerivRtyFine => {
                if !self.ps {
                    Some((Ty::F, "vec4<f32>(0.0)".into()))
                } else {
                    let f = match ins.op {
                        DerivRtx => "dpdx",
                        DerivRtxCoarse => "dpdxCoarse",
                        DerivRtxFine => "dpdxFine",
                        DerivRty => "dpdy",
                        DerivRtyCoarse => "dpdyCoarse",
                        _ => "dpdyFine",
                    };
                    Some((Ty::F, format!("{f}({})", s!(1, Ty::F))))
                }
            }
            Eq | Ne | Lt | Ge => {
                let op = match ins.op {
                    Eq => "==",
                    Ne => "!=",
                    Lt => "<",
                    _ => ">=",
                };
                Some((
                    Ty::U,
                    format!("select(vec4<u32>(0u), vec4<u32>(0xffffffffu), {} {op} {})", s!(1, Ty::F), s!(2, Ty::F)),
                ))
            }
            IEq | INe | ILt | IGe | ULt | UGe => {
                let (op, t) = match ins.op {
                    IEq => ("==", Ty::U),
                    INe => ("!=", Ty::U),
                    ILt => ("<", Ty::I),
                    IGe => (">=", Ty::I),
                    ULt => ("<", Ty::U),
                    _ => (">=", Ty::U),
                };
                Some((Ty::U, format!("select(vec4<u32>(0u), vec4<u32>(0xffffffffu), {} {op} {})", s!(1, t), s!(2, t))))
            }
            IAdd => Some((Ty::U, format!("({} + {})", s!(1, Ty::U), s!(2, Ty::U)))),
            IMad | UMad => Some((Ty::U, format!("({} * {} + {})", s!(1, Ty::U), s!(2, Ty::U), s!(3, Ty::U)))),
            IMax => Some((Ty::I, format!("max({}, {})", s!(1, Ty::I), s!(2, Ty::I)))),
            IMin => Some((Ty::I, format!("min({}, {})", s!(1, Ty::I), s!(2, Ty::I)))),
            UMax => Some((Ty::U, format!("max({}, {})", s!(1, Ty::U), s!(2, Ty::U)))),
            UMin => Some((Ty::U, format!("min({}, {})", s!(1, Ty::U), s!(2, Ty::U)))),
            INeg => Some((Ty::I, format!("(-{})", s!(1, Ty::I)))),
            IShl => Some((Ty::U, format!("({} << ({} & vec4<u32>(31u)))", s!(1, Ty::U), s!(2, Ty::U)))),
            IShr => Some((Ty::I, format!("({} >> ({} & vec4<u32>(31u)))", s!(1, Ty::I), s!(2, Ty::U)))),
            UShr => Some((Ty::U, format!("({} >> ({} & vec4<u32>(31u)))", s!(1, Ty::U), s!(2, Ty::U)))),
            And => Some((Ty::U, format!("({} & {})", s!(1, Ty::U), s!(2, Ty::U)))),
            Or => Some((Ty::U, format!("({} | {})", s!(1, Ty::U), s!(2, Ty::U)))),
            Xor => Some((Ty::U, format!("({} ^ {})", s!(1, Ty::U), s!(2, Ty::U)))),
            Not => Some((Ty::U, format!("(~{})", s!(1, Ty::U)))),
            Itof => Some((Ty::F, format!("vec4<f32>({})", s!(1, Ty::I)))),
            Utof => Some((Ty::F, format!("vec4<f32>({})", s!(1, Ty::U)))),
            Ftoi => {
                let a = s!(1, Ty::F);
                Some((Ty::I, format!("d3d_ftoi({a})")))
            }
            Ftou => {
                let a = s!(1, Ty::F);
                Some((Ty::U, format!("d3d_ftou({a})")))
            }
            Mov => {
                let t = if sat || o[1].modifier != Modifier::None { Ty::F } else { Ty::U };
                Some((t, s!(1, t)))
            }
            Movc => {
                let t = if sat { Ty::F } else { Ty::U };
                let (c, a, b) = (s!(1, Ty::U), s!(2, t), s!(3, t));
                Some((t, format!("select({b}, {a}, {c} != vec4<u32>(0u))")))
            }
            CountBits => Some((Ty::U, format!("countOneBits({})", s!(1, Ty::U)))),
            FirstBitLo => Some((Ty::U, format!("firstTrailingBit({})", s!(1, Ty::U)))),
            FirstBitHi => {
                let a = s!(1, Ty::U);
                Some((Ty::U, format!("d3d_firstbit_hi({a})")))
            }
            FirstBitShi => {
                let a = s!(1, Ty::I);
                Some((Ty::I, format!("d3d_firstbit_shi({a})")))
            }
            BfRev => Some((Ty::U, format!("reverseBits({})", s!(1, Ty::U)))),
            UBfe | IBfe => {
                let t = if ins.op == UBfe { Ty::U } else { Ty::I };
                let (w, off, v) = (s!(1, Ty::U), s!(2, Ty::U), s!(3, t));
                let (tw, toff, tv) = (self.fresh("w"), self.fresh("o"), self.fresh("v"));
                self.line(&format!("let {tw} = {w} & vec4<u32>(31u);"));
                self.line(&format!("let {toff} = {off} & vec4<u32>(31u);"));
                self.line(&format!("let {tv} = {v};"));
                let comps: Vec<String> =
                    (0..4).map(|i| format!("extractBits({tv}.{c}, {toff}.{c}, {tw}.{c})", c = C[i])).collect();
                Some((t, format!("{}({})", t.v(), comps.join(", "))))
            }
            Bfi => {
                let (w, off, ins_v, base) = (s!(1, Ty::U), s!(2, Ty::U), s!(3, Ty::U), s!(4, Ty::U));
                let (tw, toff, ti, tb) = (self.fresh("w"), self.fresh("o"), self.fresh("i"), self.fresh("b"));
                self.line(&format!("let {tw} = {w} & vec4<u32>(31u);"));
                self.line(&format!("let {toff} = {off} & vec4<u32>(31u);"));
                self.line(&format!("let {ti} = {ins_v};"));
                self.line(&format!("let {tb} = {base};"));
                let comps: Vec<String> =
                    (0..4).map(|i| format!("insertBits({tb}.{c}, {ti}.{c}, {toff}.{c}, {tw}.{c})", c = C[i])).collect();
                Some((Ty::U, format!("vec4<u32>({})", comps.join(", "))))
            }
            F32ToF16 => {
                let a = self.fresh("h");
                let v = s!(1, Ty::F);
                self.line(&format!("let {a} = {v};"));
                let comps: Vec<String> =
                    (0..4).map(|i| format!("(pack2x16float(vec2<f32>({a}.{}, 0.0)) & 0xffffu)", C[i])).collect();
                Some((Ty::U, format!("vec4<u32>({})", comps.join(", "))))
            }
            F16ToF32 => {
                let a = self.fresh("h");
                let v = s!(1, Ty::U);
                self.line(&format!("let {a} = {v};"));
                let comps: Vec<String> = (0..4).map(|i| format!("unpack2x16float({a}.{}).x", C[i])).collect();
                Some((Ty::F, format!("vec4<f32>({})", comps.join(", "))))
            }
            _ => None,
        };
        if let Some((ty, v)) = alu {
            return self.write(&o[0], &v, ty, sat);
        }
        match ins.op {
            SinCos => {
                let a = self.fresh("a");
                let v = s!(2, Ty::F);
                self.line(&format!("let {a} = {v};"));
                self.write(&o[0], &format!("sin({a})"), Ty::F, sat)?;
                self.write(&o[1], &format!("cos({a})"), Ty::F, sat)?;
            }
            UDiv => {
                let (a, b) = (self.fresh("a"), self.fresh("b"));
                let (va, vb) = (s!(2, Ty::U), s!(3, Ty::U));
                self.line(&format!("let {a} = {va};"));
                self.line(&format!("let {b} = {vb};"));
                let z = format!("{b} == vec4<u32>(0u)");
                self.write(&o[0], &format!("select({a} / {b}, vec4<u32>(0xffffffffu), {z})"), Ty::U, false)?;
                self.write(&o[1], &format!("select({a} % {b}, vec4<u32>(0xffffffffu), {z})"), Ty::U, false)?;
            }
            IMul | UMul => {
                let (a, b) = (self.fresh("a"), self.fresh("b"));
                let t = if ins.op == IMul { Ty::I } else { Ty::U };
                let (va, vb) = (s!(2, t), s!(3, t));
                self.line(&format!("let {a} = {va};"));
                self.line(&format!("let {b} = {vb};"));
                if o[0].ty != RegType::Null {
                    let f = if ins.op == IMul { "d3d_imul_hi" } else { "d3d_umul_hi" };
                    self.write(&o[0], &format!("{f}({a}, {b})"), Ty::U, false)?;
                }
                self.write(&o[1], &format!("bitcast<vec4<u32>>({a} * {b})"), Ty::U, false)?;
            }
            UAddC | USubB => {
                let (a, b) = (self.fresh("a"), self.fresh("b"));
                let (va, vb) = (s!(2, Ty::U), s!(3, Ty::U));
                self.line(&format!("let {a} = {va};"));
                self.line(&format!("let {b} = {vb};"));
                if ins.op == UAddC {
                    self.write(&o[0], &format!("({a} + {b})"), Ty::U, false)?;
                    self.write(
                        &o[1],
                        &format!("select(vec4<u32>(0u), vec4<u32>(1u), ({a} + {b}) < {a})"),
                        Ty::U,
                        false,
                    )?;
                } else {
                    self.write(&o[0], &format!("({a} - {b})"), Ty::U, false)?;
                    self.write(&o[1], &format!("select(vec4<u32>(0u), vec4<u32>(1u), {a} < {b})"), Ty::U, false)?;
                }
            }
            SwapC => {
                let (c, a, b) = (self.fresh("c"), self.fresh("a"), self.fresh("b"));
                let (vc, va, vb) = (s!(2, Ty::U), s!(3, Ty::U), s!(4, Ty::U));
                self.line(&format!("let {c} = {vc} != vec4<u32>(0u);"));
                self.line(&format!("let {a} = {va};"));
                self.line(&format!("let {b} = {vb};"));
                self.write(&o[0], &format!("select({a}, {b}, {c})"), Ty::U, false)?;
                self.write(&o[1], &format!("select({b}, {a}, {c})"), Ty::U, false)?;
            }
            // Control flow.
            If => {
                let c = self.cond(ins, &o[0])?;
                self.line(&format!("if ({c}) {{"));
                self.indent += 1;
            }
            Else => {
                self.indent -= 1;
                self.line("} else {");
                self.indent += 1;
            }
            EndIf | EndLoop => {
                self.indent -= 1;
                self.line("}");
            }
            Loop => {
                self.line("loop {");
                self.indent += 1;
            }
            Break => self.line("break;"),
            Continue => self.line("continue;"),
            BreakC | ContinueC | RetC | Discard => {
                let c = self.cond(ins, &o[0])?;
                let stmt = match ins.op {
                    BreakC => "break;",
                    ContinueC => "continue;",
                    RetC => "return;",
                    _ => "discard;",
                };
                self.line(&format!("if ({c}) {{ {stmt} }}"));
            }
            Switch => {
                let v = s!(0, Ty::I);
                self.line(&format!("switch (({v}).x) {{"));
                self.indent += 1;
                self.switch.push(SwitchState { pending: Vec::new(), open: false, has_default: false });
            }
            Case | Default => {
                let label = if ins.op == Case {
                    let v = o[0].imm.first().copied().unwrap_or(0) as i32;
                    format!("{v}i")
                } else {
                    "default".to_string()
                };
                let close = {
                    let sw = self.switch.last_mut().ok_or_else(|| Error::Unsupported("case outside switch".into()))?;
                    let close = sw.open && sw.pending.is_empty();
                    if close {
                        sw.open = false;
                    }
                    if ins.op == Default {
                        sw.has_default = true;
                    }
                    sw.pending.push(label);
                    close
                };
                if close {
                    self.indent -= 1;
                    self.line("}");
                }
            }
            EndSwitch => {
                let sw = self.switch.pop().ok_or_else(|| Error::Unsupported("endswitch outside switch".into()))?;
                if sw.open {
                    self.indent -= 1;
                    self.line("}");
                }
                if !sw.pending.is_empty() {
                    self.line(&format!("case {}: {{}}", sw.pending.join(", ")));
                }
                if !sw.has_default {
                    self.line("default: {}");
                }
                self.indent -= 1;
                self.line("}");
            }
            Ret => {
                if self.indent == 1 && self.switch.is_empty() {
                    // End of a function body: later code is a label or dead.
                    self.line("return;");
                } else {
                    self.line("return;");
                }
            }
            Label => {
                self.finish_function();
                self.labels_started = true;
                self.funcs.push((format!("l{}", o[0].reg()), String::new()));
                self.indent = 1;
            }
            Call => self.line(&format!("l{}();", o[0].reg())),
            CallC => {
                let c = self.cond(ins, &o[0])?;
                self.line(&format!("if ({c}) {{ l{}(); }}", o[1].reg()));
            }
            Sync => {
                let f = ins.controls & 0xf;
                if f & 0x3 != 0 {
                    self.line("workgroupBarrier();");
                }
                if f & 0xc != 0 {
                    self.line("storageBarrier();");
                }
            }
            Sample | SampleB | SampleL | SampleD | SampleC | SampleCLz | Gather4 | Gather4C => self.sample(ins)?,
            Ld | LdMs => self.load(ins)?,
            ResInfo => self.resinfo(ins)?,
            BufInfo => self.bufinfo(ins)?,
            SampleInfo => {
                let n = if o[1].ty == RegType::Resource {
                    if self.sh.reflection.resource(o[1].reg()).is_some_and(|d| d.dim.is_ms()) {
                        format!("textureNumSamples(t{})", o[1].reg())
                    } else {
                        "1u".into()
                    }
                } else {
                    "1u".into()
                };
                let uint = ins.controls & 0x3 == 2;
                let v = if uint {
                    format!("vec4<u32>({n}, 0u, 0u, 0u)")
                } else {
                    format!("bitcast<vec4<u32>>(vec4<f32>(f32({n}), 0.0, 0.0, 0.0))")
                };
                self.write(&o[0], &v, Ty::U, false)?;
            }
            SamplePos => self.write(&o[0], "vec4<f32>(0.0)", Ty::F, false)?,
            EvalCentroid | EvalSampleIndex => {
                // WGSL has no interpolateAt*: use the interpolated value.
                let v = s!(1, Ty::F);
                self.write(&o[0], &v, Ty::F, sat)?;
            }
            LdRaw | LdStructured | LdUavTyped => self.mem_load(ins)?,
            StoreRaw | StoreStructured | StoreUavTyped => self.mem_store(ins)?,
            AtomicAnd | AtomicOr | AtomicXor | AtomicCmpStore | AtomicIAdd | AtomicIMax | AtomicIMin | AtomicUMax
            | AtomicUMin | ImmAtomicIAdd | ImmAtomicAnd | ImmAtomicOr | ImmAtomicXor | ImmAtomicExch
            | ImmAtomicCmpExch | ImmAtomicIMax | ImmAtomicIMin | ImmAtomicUMax | ImmAtomicUMin => self.atomic(ins)?,
            op => return Err(Error::Unsupported(format!("instruction {}", op.name()))),
        }
        Ok(())
    }

    // ---- Textures ----

    fn res_decl(&self, o: &Operand) -> Result<crate::reflect::ResourceDecl, Error> {
        self.sh
            .reflection
            .resource(o.reg())
            .cloned()
            .ok_or_else(|| Error::Unsupported(format!("t{} is not declared", o.reg())))
    }

    fn ret_ty(ret: &[ReturnType; 4]) -> Ty {
        match ret[0] {
            ReturnType::Sint => Ty::I,
            ReturnType::Uint => Ty::U,
            _ => Ty::F,
        }
    }

    /// Coordinates for sampling: (coords, array index) for a dimension.
    fn coords(c: &str, dim: ResourceDim, ty: &str) -> (String, Option<String>) {
        use ResourceDim::*;
        match dim {
            Texture1D => (format!("vec2<{ty}>({c}.x, {})", if ty == "f32" { "0.5" } else { "0i" }), None),
            Texture1DArray => {
                (format!("vec2<{ty}>({c}.x, {})", if ty == "f32" { "0.5" } else { "0i" }), Some(format!("{c}.y")))
            }
            Texture2D | Texture2DMs => (format!("{c}.xy"), None),
            Texture2DArray | Texture2DMsArray => (format!("{c}.xy"), Some(format!("{c}.z"))),
            Texture3D | TextureCube => (format!("{c}.xyz"), None),
            TextureCubeArray => (format!("{c}.xyz"), Some(format!("{c}.w"))),
            _ => (format!("{c}.x"), None),
        }
    }

    fn offset_arg(ins: &Instruction, dim: ResourceDim) -> Option<String> {
        if ins.offsets == [0; 3] {
            return None;
        }
        let [u, v, w] = ins.offsets;
        use ResourceDim::*;
        match dim {
            Texture1D | Texture1DArray => Some(format!("vec2<i32>({u}, 0)")),
            Texture2D | Texture2DArray | Texture2DMs | Texture2DMsArray => Some(format!("vec2<i32>({u}, {v})")),
            Texture3D => Some(format!("vec3<i32>({u}, {v}, {w})")),
            _ => None,
        }
    }

    fn sample(&mut self, ins: &Instruction) -> Result<(), Error> {
        use Opcode::*;
        let o = &ins.operands;
        let t = &o[2];
        let s = &o[3];
        let d = self.res_decl(t)?;
        let depth = self.depth_srv(t.reg());
        let c = self.fresh("c");
        let cv = self.src(&o[1], Ty::F)?;
        self.line(&format!("let {c} = {cv};"));
        let (coords, array) = Self::coords(&c, d.dim, "f32");
        let arr = array.map(|a| format!(", i32(round({a}))")).unwrap_or_default();
        let off = Self::offset_arg(ins, d.dim).map(|o| format!(", {o}")).unwrap_or_default();
        let (tn, sn) = (format!("t{}", t.reg()), format!("s{}", s.reg()));
        let implicit = self.ps;
        let lvl_zero = if depth { "0i" } else { "0.0" };
        let expr = match ins.op {
            Sample if implicit => format!("textureSample({tn}, {sn}, {coords}{arr}{off})"),
            Sample => format!("textureSampleLevel({tn}, {sn}, {coords}{arr}, {lvl_zero}{off})"),
            SampleB if implicit => {
                let b = self.src(&o[4], Ty::F)?;
                format!("textureSampleBias({tn}, {sn}, {coords}{arr}, ({b}).x{off})")
            }
            SampleB => format!("textureSampleLevel({tn}, {sn}, {coords}{arr}, {lvl_zero}{off})"),
            SampleL => {
                let l = self.src(&o[4], Ty::F)?;
                let l = if depth { format!("i32(({l}).x)") } else { format!("({l}).x") };
                format!("textureSampleLevel({tn}, {sn}, {coords}{arr}, {l}{off})")
            }
            SampleD => {
                let (dx, dy) = (self.src(&o[4], Ty::F)?, self.src(&o[5], Ty::F)?);
                let sw = match d.dim.coord_count() {
                    1 => ".xx",
                    2 => ".xy",
                    _ => ".xyz",
                };
                let sw = if matches!(d.dim, ResourceDim::Texture1D | ResourceDim::Texture1DArray) {
                    format!("vec2<f32>(({dx}).x, 0.0), vec2<f32>(({dy}).x, 0.0)")
                } else {
                    format!("({dx}){sw}, ({dy}){sw}")
                };
                format!("textureSampleGrad({tn}, {sn}, {coords}{arr}, {sw}{off})")
            }
            SampleC | SampleCLz => {
                let r = self.src(&o[4], Ty::F)?;
                if ins.op == SampleC && implicit {
                    format!("textureSampleCompare({tn}, {sn}, {coords}{arr}, ({r}).x{off})")
                } else {
                    format!("textureSampleCompareLevel({tn}, {sn}, {coords}{arr}, ({r}).x{off})")
                }
            }
            Gather4 => {
                if depth {
                    format!("textureGather({tn}, {sn}, {coords}{arr}{off})")
                } else {
                    let comp = s.select.comp(0);
                    format!("textureGather({comp}, {tn}, {sn}, {coords}{arr}{off})")
                }
            }
            Gather4C => {
                let r = self.src(&o[4], Ty::F)?;
                format!("textureGatherCompare({tn}, {sn}, {coords}{arr}, ({r}).x{off})")
            }
            _ => unreachable!(),
        };
        let ret = Self::ret_ty(&d.ret);
        let res = self.fresh("smp");
        self.line(&format!("let {res} = {expr};"));
        let full = if depth && matches!(ins.op, Sample | SampleB | SampleL | SampleD) {
            format!("vec4<f32>({res}, 0.0, 0.0, 1.0)")
        } else if matches!(ins.op, SampleC | SampleCLz) {
            format!("vec4<f32>({res})")
        } else {
            res
        };
        let sw = Self::swizzle(t);
        let ty = if matches!(ins.op, SampleC | SampleCLz | Gather4C) || depth { Ty::F } else { ret };
        self.write(&o[0], &format!("({full}){sw}"), ty, ins.saturate)
    }

    fn load(&mut self, ins: &Instruction) -> Result<(), Error> {
        let o = &ins.operands;
        let t = &o[2];
        let d = self.res_decl(t)?;
        if d.dim == ResourceDim::Buffer {
            let i = self.src(&o[1], Ty::U)?;
            let v = self.typed_buffer_load(&format!("t{}", t.reg()), t.reg(), true, &format!("({i}).x"), &d.ret)?;
            let sw = Self::swizzle(t);
            return self.write(&o[0], &format!("({v}){sw}"), Ty::U, false);
        }
        let depth = self.depth_srv(t.reg());
        let c = self.fresh("c");
        let cv = self.src(&o[1], Ty::I)?;
        self.line(&format!("let {c} = {cv};"));
        let (coords, array) = Self::coords(&c, d.dim, "i32");
        let mut coords = coords;
        if let Some(off) = Self::offset_arg(ins, d.dim) {
            coords = format!("({coords} + {off})");
        }
        let arr = array.map(|a| format!(", {a}")).unwrap_or_default();
        let tn = format!("t{}", t.reg());
        let last = if d.dim.is_ms() {
            let s = self.src(&o[3], Ty::I)?;
            format!("({s}).x")
        } else {
            format!("{c}.w")
        };
        let res = self.fresh("ld");
        self.line(&format!("let {res} = textureLoad({tn}, {coords}{arr}, {last});"));
        let ret = Self::ret_ty(&d.ret);
        let full = if depth { format!("vec4<f32>({res}, 0.0, 0.0, 1.0)") } else { res };
        let sw = Self::swizzle(t);
        self.write(&o[0], &format!("({full}){sw}"), if depth { Ty::F } else { ret }, ins.saturate)
    }

    fn resinfo(&mut self, ins: &Instruction) -> Result<(), Error> {
        let o = &ins.operands;
        let t = &o[2];
        let (name, dim) = if t.ty == RegType::Uav {
            let d = self.sh.reflection.uav(t.reg()).ok_or_else(|| Error::Unsupported("undeclared u#".into()))?;
            (format!("u{}", t.reg()), d.dim)
        } else {
            (format!("t{}", t.reg()), self.res_decl(t)?.dim)
        };
        let mip = self.src(&o[1], Ty::U)?;
        let lvl = if dim.is_ms() || t.ty == RegType::Uav { String::new() } else { format!(", ({mip}).x") };
        use ResourceDim::*;
        let dims = format!("textureDimensions({name}{lvl})");
        let levels =
            if dim.is_ms() || t.ty == RegType::Uav { "1u".to_string() } else { format!("textureNumLevels({name})") };
        let v = match dim {
            Texture1D => format!("vec4<u32>({dims}.x, 0u, 0u, {levels})"),
            Texture1DArray => format!("vec4<u32>({dims}.x, textureNumLayers({name}), 0u, {levels})"),
            Texture2D | Texture2DMs | TextureCube => format!("vec4<u32>({dims}, 0u, {levels})"),
            Texture2DArray | Texture2DMsArray | TextureCubeArray => {
                format!("vec4<u32>({dims}, textureNumLayers({name}), {levels})")
            }
            Texture3D => format!("vec4<u32>({dims}, {levels})"),
            _ => return Err(Error::Unsupported("resinfo on a buffer".into())),
        };
        let r = self.fresh("ri");
        self.line(&format!("let {r} = {v};"));
        let sw = Self::swizzle(t);
        match ins.controls & 0x3 {
            2 => self.write(&o[0], &format!("({r}){sw}"), Ty::U, false),
            1 => self.write(
                &o[0],
                &format!("(vec4<f32>(vec3<f32>(1.0) / vec3<f32>({r}.xyz), f32({r}.w))){sw}"),
                Ty::F,
                ins.saturate,
            ),
            _ => self.write(&o[0], &format!("(vec4<f32>({r})){sw}"), Ty::F, ins.saturate),
        }
    }

    fn bufinfo(&mut self, ins: &Instruction) -> Result<(), Error> {
        let o = &ins.operands;
        let t = &o[1];
        let (name, dim, stride, slot, srv) = if t.ty == RegType::Uav {
            let d = self.sh.reflection.uav(t.reg()).ok_or_else(|| Error::Unsupported("undeclared u#".into()))?;
            (format!("u{}", t.reg()), d.dim, d.stride, t.reg(), false)
        } else {
            let d = self.res_decl(t)?;
            (format!("t{}", t.reg()), d.dim, d.stride, t.reg(), true)
        };
        let words = format!("arrayLength(&{name})");
        let n = match dim {
            ResourceDim::RawBuffer => format!("({words} * 4u)"),
            ResourceDim::StructuredBuffer => format!("({words} * 4u / {}u)", stride.max(1)),
            _ => {
                let f = if srv {
                    match self.srv_kind(slot) {
                        SrvKind::Buffer(f) => f,
                        _ => BufferFormat::R32Uint,
                    }
                } else {
                    match self.uav_kind(slot) {
                        UavKind::Buffer(f) => f,
                        _ => BufferFormat::R32Uint,
                    }
                };
                format!("({words} / {}u)", f.words())
            }
        };
        self.write(&o[0], &format!("vec4<u32>({n})"), Ty::U, false)
    }

    // ---- Buffers, UAVs, groupshared memory ----

    /// (name, is atomic array) of a raw/structured/typed buffer operand.
    fn buffer_target(&self, o: &Operand) -> Result<(String, bool, ResourceDim, u32), Error> {
        Ok(match o.ty {
            RegType::Resource => {
                let d = self.res_decl(o)?;
                (format!("t{}", o.reg()), false, d.dim, d.stride)
            }
            RegType::Uav => {
                let d = self.sh.reflection.uav(o.reg()).ok_or_else(|| Error::Unsupported("undeclared u#".into()))?;
                (format!("u{}", o.reg()), d.atomic, d.dim, d.stride)
            }
            RegType::ThreadGroupSharedMemory => {
                let d = self
                    .sh
                    .reflection
                    .tgsm
                    .iter()
                    .find(|g| g.reg == o.reg())
                    .ok_or_else(|| Error::Unsupported("undeclared g#".into()))?;
                let dim = if d.stride == 0 { ResourceDim::RawBuffer } else { ResourceDim::StructuredBuffer };
                (format!("g{}", o.reg()), d.atomic, dim, d.stride)
            }
            t => return Err(Error::Unsupported(format!("memory operand {t:?}"))),
        })
    }

    fn word_read(name: &str, atomic: bool, idx: &str) -> String {
        if atomic {
            format!("atomicLoad(&{name}[{idx}])")
        } else {
            format!("{name}[{idx}]")
        }
    }

    fn mem_load(&mut self, ins: &Instruction) -> Result<(), Error> {
        let o = &ins.operands;
        let res = o.last().unwrap();
        let (name, atomic, dim, stride) = self.buffer_target(res)?;
        let base = self.fresh("w");
        match ins.op {
            Opcode::LdRaw => {
                let a = self.src(&o[1], Ty::U)?;
                self.line(&format!("let {base} = ({a}).x >> 2u;"));
            }
            Opcode::LdStructured => {
                let (i, off) = (self.src(&o[1], Ty::U)?, self.src(&o[2], Ty::U)?);
                self.line(&format!("let {base} = (({i}).x * {stride}u + ({off}).x) >> 2u;"));
            }
            _ => {
                // ld_uav_typed
                if res.ty == RegType::Uav {
                    if let UavKind::Texture(_) = self.uav_kind(res.reg()) {
                        let c = self.src(&o[1], Ty::I)?;
                        let (coords, arr) = Self::coords(&format!("({c})"), dim, "i32");
                        let arr = arr.map(|a| format!(", {a}")).unwrap_or_default();
                        let v = format!("textureLoad({name}, {coords}{arr})");
                        let sw = Self::swizzle(res);
                        let ty = match self.uav_kind(res.reg()) {
                            UavKind::Texture(f) => f.ty(),
                            _ => Ty::U,
                        };
                        return self.write(&o[0], &format!("({v}){sw}"), ty, false);
                    }
                }
                let i = self.src(&o[1], Ty::U)?;
                let d = self.sh.reflection.uav(res.reg()).map(|d| d.ret).unwrap_or([ReturnType::Uint; 4]);
                let v = self.typed_buffer_load(&name, res.reg(), false, &format!("({i}).x"), &d)?;
                let sw = Self::swizzle(res);
                return self.write(&o[0], &format!("({v}){sw}"), Ty::U, false);
            }
        }
        let _ = dim;
        let comps: Vec<String> =
            (0..4).map(|c| Self::word_read(&name, atomic, &format!("{base} + {}u", res.select.comp(c)))).collect();
        self.write(&o[0], &format!("vec4<u32>({})", comps.join(", ")), Ty::U, false)
    }

    fn mem_store(&mut self, ins: &Instruction) -> Result<(), Error> {
        let o = &ins.operands;
        let dst = &o[0];
        let (name, atomic, dim, stride) = self.buffer_target(dst)?;
        let mask = dst.select.mask();
        let base = self.fresh("w");
        let value_i = match ins.op {
            Opcode::StoreRaw => {
                let a = self.src(&o[1], Ty::U)?;
                self.line(&format!("let {base} = ({a}).x >> 2u;"));
                2
            }
            Opcode::StoreStructured => {
                let (i, off) = (self.src(&o[1], Ty::U)?, self.src(&o[2], Ty::U)?);
                self.line(&format!("let {base} = (({i}).x * {stride}u + ({off}).x) >> 2u;"));
                3
            }
            _ => {
                if let UavKind::Texture(f) = self.uav_kind(dst.reg()) {
                    let c = self.src(&o[1], Ty::I)?;
                    let (coords, arr) = Self::coords(&format!("({c})"), dim, "i32");
                    let arr = arr.map(|a| format!(", {a}")).unwrap_or_default();
                    let v = self.src(&o[2], f.ty())?;
                    self.line(&format!("textureStore({name}, {coords}{arr}, {v});"));
                    return Ok(());
                }
                let i = self.src(&o[1], Ty::U)?;
                let v = self.src(&o[2], Ty::U)?;
                return self.typed_buffer_store(&name, dst.reg(), &format!("({i}).x"), &v);
            }
        };
        let v = self.fresh("sv");
        let vv = self.src(&o[value_i], Ty::U)?;
        self.line(&format!("let {v} = {vv};"));
        // Components written are consecutive words starting at the address.
        let mut k = 0;
        for c in 0..4 {
            if mask & (1 << c) != 0 {
                let idx = format!("{base} + {k}u");
                if atomic {
                    self.line(&format!("atomicStore(&{name}[{idx}], {v}.{});", C[c]));
                } else {
                    self.line(&format!("{name}[{idx}] = {v}.{};", C[c]));
                }
                k += 1;
            }
        }
        Ok(())
    }

    fn typed_buffer_format(&self, slot: u32, srv: bool) -> BufferFormat {
        if srv {
            match self.srv_kind(slot) {
                SrvKind::Buffer(f) => f,
                _ => BufferFormat::R32Uint,
            }
        } else {
            match self.uav_kind(slot) {
                UavKind::Buffer(f) => f,
                _ => BufferFormat::R32Uint,
            }
        }
    }

    /// Element `index` of a typed buffer as the bits of its natural type.
    fn typed_buffer_load(
        &mut self,
        name: &str,
        slot: u32,
        srv: bool,
        index: &str,
        _ret: &[ReturnType; 4],
    ) -> Result<String, Error> {
        let f = self.typed_buffer_format(slot, srv);
        let atomic = !srv && self.sh.reflection.uav(slot).is_some_and(|d| d.atomic);
        let w = self.fresh("e");
        self.line(&format!("let {w} = {index} * {}u;", f.words()));
        let rd = |k: u32| Self::word_read(name, atomic, &format!("{w} + {k}u"));
        let one_f = "0x3f800000u";
        use BufferFormat::*;
        Ok(match f {
            R32Uint | R32Sint => format!("vec4<u32>({}, 0u, 0u, 1u)", rd(0)),
            R32Float => format!("vec4<u32>({}, 0u, 0u, {one_f})", rd(0)),
            Rg32Uint | Rg32Sint => format!("vec4<u32>({}, {}, 0u, 1u)", rd(0), rd(1)),
            Rg32Float => format!("vec4<u32>({}, {}, 0u, {one_f})", rd(0), rd(1)),
            Rgb32Uint | Rgb32Sint => format!("vec4<u32>({}, {}, {}, 1u)", rd(0), rd(1), rd(2)),
            Rgb32Float => format!("vec4<u32>({}, {}, {}, {one_f})", rd(0), rd(1), rd(2)),
            Rgba32Uint | Rgba32Sint | Rgba32Float => format!("vec4<u32>({}, {}, {}, {})", rd(0), rd(1), rd(2), rd(3)),
            Rgba8Unorm => format!("bitcast<vec4<u32>>(unpack4x8unorm({}))", rd(0)),
            Rgba8Uint => format!("((vec4<u32>({0}) >> vec4<u32>(0u, 8u, 16u, 24u)) & vec4<u32>(255u))", rd(0)),
            R16Float => format!("bitcast<vec4<u32>>(vec4<f32>(unpack2x16float({}).x, 0.0, 0.0, 1.0))", rd(0)),
            Rg16Float => format!("bitcast<vec4<u32>>(vec4<f32>(unpack2x16float({}), 0.0, 1.0))", rd(0)),
            Rgba16Float => {
                format!("bitcast<vec4<u32>>(vec4<f32>(unpack2x16float({}), unpack2x16float({})))", rd(0), rd(1))
            }
        })
    }

    fn typed_buffer_store(&mut self, name: &str, slot: u32, index: &str, v: &str) -> Result<(), Error> {
        let f = self.typed_buffer_format(slot, false);
        let atomic = self.sh.reflection.uav(slot).is_some_and(|d| d.atomic);
        let w = self.fresh("e");
        let val = self.fresh("v");
        self.line(&format!("let {w} = {index} * {}u;", f.words()));
        self.line(&format!("let {val} = {v};"));
        use BufferFormat::*;
        let words: Vec<String> = match f {
            Rgba8Unorm => vec![format!("pack4x8unorm(bitcast<vec4<f32>>({val}))")],
            Rgba8Uint => {
                vec![format!("dot(min({val}, vec4<u32>(255u)) << vec4<u32>(0u, 8u, 16u, 24u), vec4<u32>(1u))")]
            }
            R16Float => vec![format!("pack2x16float(vec2<f32>(bitcast<f32>({val}.x), 0.0))")],
            Rg16Float => vec![format!("pack2x16float(bitcast<vec2<f32>>({val}.xy))")],
            Rgba16Float => vec![
                format!("pack2x16float(bitcast<vec2<f32>>({val}.xy))"),
                format!("pack2x16float(bitcast<vec2<f32>>({val}.zw))"),
            ],
            _ => (0..f.words() as usize).map(|i| format!("{val}.{}", C[i])).collect(),
        };
        for (k, word) in words.iter().enumerate() {
            if atomic {
                self.line(&format!("atomicStore(&{name}[{w} + {k}u], {word});"));
            } else {
                self.line(&format!("{name}[{w} + {k}u] = {word};"));
            }
        }
        Ok(())
    }

    fn atomic(&mut self, ins: &Instruction) -> Result<(), Error> {
        use Opcode::*;
        let o = &ins.operands;
        let imm = ins.op.name().starts_with("imm_");
        let (dst, target, rest) = if imm { (Some(&o[0]), &o[1], &o[2..]) } else { (None, &o[0], &o[1..]) };
        let (name, _atomic, dim, stride) = self.buffer_target(target)?;
        if target.ty == RegType::Uav {
            if let UavKind::Texture(_) = self.uav_kind(target.reg()) {
                return Err(Error::Unsupported("atomics on typed UAV textures".into()));
            }
        }
        let addr = self.src(&rest[0], Ty::U)?;
        let idx = self.fresh("ai");
        match dim {
            ResourceDim::RawBuffer => self.line(&format!("let {idx} = ({addr}).x >> 2u;")),
            ResourceDim::StructuredBuffer => {
                self.line(&format!("let {idx} = (({addr}).x * {stride}u + ({addr}).y) >> 2u;"))
            }
            _ => {
                let f = self.typed_buffer_format(target.reg(), false);
                self.line(&format!("let {idx} = ({addr}).x * {}u;", f.words()));
            }
        }
        let p = format!("&{name}[{idx}]");
        let old = self.fresh("old");
        match ins.op {
            AtomicCmpStore | ImmAtomicCmpExch => {
                let (cmp, val) = (self.src(&rest[1], Ty::U)?, self.src(&rest[2], Ty::U)?);
                // WGSL's compare-exchange is weak; Direct3D's is strong.
                self.line(&format!("var {old}: u32;"));
                self.line("loop {");
                self.line(&format!("    let r = atomicCompareExchangeWeak({p}, ({cmp}).x, ({val}).x);"));
                self.line(&format!("    {old} = r.old_value;"));
                self.line("    if (r.exchanged || r.old_value != (".to_string().as_str());
                // (completed below to keep the expression readable)
                self.body.pop();
                self.body.push_str(&format!("{cmp}).x) {{ break; }}\n"));
                self.line("}");
            }
            AtomicIMax | AtomicIMin | ImmAtomicIMax | ImmAtomicIMin => {
                let v = self.src(&rest[1], Ty::I)?;
                let f = if matches!(ins.op, AtomicIMax | ImmAtomicIMax) { "max" } else { "min" };
                self.line(&format!("var {old}: u32;"));
                self.line("loop {");
                self.line(&format!("    {old} = atomicLoad({p});"));
                self.line(&format!("    let n = bitcast<u32>({f}(bitcast<i32>({old}), ({v}).x));"));
                self.line(&format!("    if (atomicCompareExchangeWeak({p}, {old}, n).exchanged) {{ break; }}"));
                self.line("}");
            }
            _ => {
                let v = self.src(&rest[1], Ty::U)?;
                let f = match ins.op {
                    AtomicAnd | ImmAtomicAnd => "atomicAnd",
                    AtomicOr | ImmAtomicOr => "atomicOr",
                    AtomicXor | ImmAtomicXor => "atomicXor",
                    AtomicIAdd | ImmAtomicIAdd => "atomicAdd",
                    AtomicUMax | ImmAtomicUMax => "atomicMax",
                    AtomicUMin | ImmAtomicUMin => "atomicMin",
                    _ => "atomicExchange",
                };
                self.line(&format!("let {old} = {f}({p}, ({v}).x);"));
            }
        }
        if let Some(d) = dst {
            self.write(d, &format!("vec4<u32>({old})"), Ty::U, false)?;
        }
        Ok(())
    }

    // ---- Module ----

    fn bind(&mut self, binding: u32, slot: u32, ty: BindingType) {
        self.bindings.push(Binding { group: self.group, binding, slot, ty });
    }

    fn module(mut self) -> Result<Translation, Error> {
        let r = &self.sh.reflection;
        let mut m = String::new();
        let clip_builtin = matches!(self.key.clip, Clip::Builtin(_)) && !self.ps && !self.cs;
        if clip_builtin {
            m += "enable clip_distances;\n";
        }
        if self.ps {
            m += "diagnostic(off, derivative_uniformity);\n";
        }
        m += DRIVER_WGSL;
        let grp = self.group;
        // Constant buffers.
        for (slot, size) in r.cbuffers.clone() {
            let _ = writeln!(
                m,
                "@group({grp}) @binding({}) var<uniform> cb{slot}: array<vec4<u32>, {size}>;",
                CB_BASE + slot
            );
            self.bind(CB_BASE + slot, slot, BindingType::ConstantBuffer { size: size * 16 });
        }
        // Shader resources.
        for d in r.resources.clone() {
            let b = SRV_BASE + d.slot;
            match d.dim {
                ResourceDim::Buffer | ResourceDim::RawBuffer | ResourceDim::StructuredBuffer => {
                    let _ = writeln!(m, "@group({grp}) @binding({b}) var<storage, read> t{}: array<u32>;", d.slot);
                    self.bind(b, d.slot, BindingType::StorageBuffer { read_only: true });
                }
                dim => {
                    let depth = self.depth_srv(d.slot);
                    let ret = Self::ret_ty(&d.ret);
                    let (tdim, wdim) = match dim {
                        ResourceDim::Texture1D | ResourceDim::Texture2D | ResourceDim::Texture2DMs => {
                            (TexDim::D2, "2d")
                        }
                        ResourceDim::Texture1DArray | ResourceDim::Texture2DArray | ResourceDim::Texture2DMsArray => {
                            (TexDim::D2Array, "2d_array")
                        }
                        ResourceDim::Texture3D => (TexDim::D3, "3d"),
                        ResourceDim::TextureCube => (TexDim::Cube, "cube"),
                        _ => (TexDim::CubeArray, "cube_array"),
                    };
                    let ms = dim.is_ms();
                    if ms && dim == ResourceDim::Texture2DMsArray {
                        return Err(Error::Unsupported("multisampled texture arrays".into()));
                    }
                    let ty = if depth {
                        if ms {
                            "texture_depth_multisampled_2d".to_string()
                        } else {
                            format!("texture_depth_{wdim}")
                        }
                    } else if ms {
                        format!("texture_multisampled_2d<{}>", ret.s())
                    } else {
                        format!("texture_{wdim}<{}>", ret.s())
                    };
                    let _ = writeln!(m, "@group({grp}) @binding({b}) var t{}: {ty};", d.slot);
                    let sample = if depth {
                        TexSample::Depth
                    } else {
                        match ret {
                            Ty::F => TexSample::Float,
                            Ty::U => TexSample::Uint,
                            Ty::I => TexSample::Sint,
                        }
                    };
                    self.bind(b, d.slot, BindingType::Texture { dim: tdim, sample, multisampled: ms });
                }
            }
        }
        for (slot, cmp) in r.samplers.clone() {
            let b = SAMPLER_BASE + slot;
            let _ = writeln!(
                m,
                "@group({grp}) @binding({b}) var s{slot}: {};",
                if cmp { "sampler_comparison" } else { "sampler" }
            );
            self.bind(b, slot, BindingType::Sampler { comparison: cmp });
        }
        for d in r.uavs.clone() {
            let b = UAV_BASE + d.slot;
            match (d.dim, self.uav_kind(d.slot)) {
                (ResourceDim::RawBuffer | ResourceDim::StructuredBuffer | ResourceDim::Buffer, _)
                | (_, UavKind::Buffer(_)) => {
                    let el = if d.atomic { "atomic<u32>" } else { "u32" };
                    let _ =
                        writeln!(m, "@group({grp}) @binding({b}) var<storage, read_write> u{}: array<{el}>;", d.slot);
                    self.bind(b, d.slot, BindingType::StorageBuffer { read_only: false });
                }
                (dim, UavKind::Texture(f)) => {
                    let (tdim, wdim) = match dim {
                        ResourceDim::Texture1D | ResourceDim::Texture2D => (TexDim::D2, "2d"),
                        ResourceDim::Texture1DArray | ResourceDim::Texture2DArray => (TexDim::D2Array, "2d_array"),
                        ResourceDim::Texture3D => (TexDim::D3, "3d"),
                        _ => return Err(Error::Unsupported(format!("UAV dimension {dim:?}"))),
                    };
                    let (read, write) = (d.read, d.written || !d.read);
                    let access = match (read, write) {
                        (true, true) => {
                            if !f.read_write_ok() {
                                return Err(Error::Unsupported(format!("read-write {} storage texture", f.wgsl())));
                            }
                            "read_write"
                        }
                        (true, false) => "read",
                        _ => "write",
                    };
                    let _ = writeln!(
                        m,
                        "@group({grp}) @binding({b}) var u{}: texture_storage_{wdim}<{}, {access}>;",
                        d.slot,
                        f.wgsl()
                    );
                    self.bind(b, d.slot, BindingType::StorageTexture { format: f, dim: tdim, read, write });
                }
            }
        }
        for g in r.tgsm.clone() {
            let el = if g.atomic { "atomic<u32>" } else { "u32" };
            let _ = writeln!(m, "var<workgroup> g{}: array<{el}, {}>;", g.reg, g.words.max(1));
        }
        m.push('\n');
        // Registers.
        for i in 0..r.temps {
            let _ = writeln!(m, "var<private> r{i}: vec4<u32>;");
        }
        for (reg, size, _) in &r.indexable_temps {
            let _ = writeln!(m, "var<private> x{reg}: array<vec4<u32>, {}>;", size.max(&1));
        }
        let regs = |e: &SigElement| if e.register < 64 { e.register + 1 } else { 1 };
        let nin = r.input_decls.iter().map(|d| d.reg + 1).chain(r.inputs.iter().map(regs)).max().unwrap_or(1);
        let nout = r.output_decls.iter().map(|d| d.reg + 1).chain(r.outputs.iter().map(regs)).max().unwrap_or(1);
        let _ = writeln!(m, "var<private> v: array<vec4<u32>, {}>;", nin.max(1));
        let _ = writeln!(m, "var<private> o: array<vec4<u32>, {}>;", nout.max(1));
        m += "var<private> vtid: vec4<u32>;\nvar<private> vgid: vec4<u32>;\nvar<private> vlid: vec4<u32>;\nvar<private> vlidx: vec4<u32>;\n";
        m += "var<private> vcov: vec4<u32>;\nvar<private> vprim: vec4<u32>;\nvar<private> odepth: vec4<u32>;\nvar<private> omask: vec4<u32>;\n";
        if !r.icb.is_empty() {
            let vals: Vec<String> =
                r.icb.iter().map(|v| format!("vec4<u32>({}u, {}u, {}u, {}u)", v[0], v[1], v[2], v[3])).collect();
            let _ = writeln!(
                m,
                "var<private> icb: array<vec4<u32>, {}> = array<vec4<u32>, {}>({});",
                vals.len(),
                vals.len(),
                vals.join(", ")
            );
        }
        m += HELPERS;
        for (name, body) in &self.funcs {
            let _ = write!(m, "fn {name}() {{\n{body}}}\n\n");
        }
        let mut t = Translation {
            wgsl: String::new(),
            entry: "main",
            bindings: std::mem::take(&mut self.bindings),
            vertex_inputs: Vec::new(),
            varyings: 0,
            targets: Vec::new(),
            workgroup_size: r.thread_group,
        };
        match self.sh.program.ty {
            ProgramType::Vertex => self.vs_entry(&mut m, &mut t)?,
            ProgramType::Pixel => self.ps_entry(&mut m, &mut t)?,
            _ => self.cs_entry(&mut m)?,
        }
        t.wgsl = m;
        Ok(t)
    }

    fn vs_entry(&self, m: &mut String, t: &mut Translation) -> Result<(), Error> {
        let r = &self.sh.reflection;
        let mut params = Vec::new();
        let mut pro = String::new();
        for d in &r.input_decls {
            let e = r.input_elements(d.reg).next();
            match d.sv.or(e.map(|e| e.system_value)) {
                Some(sv::VERTEX_ID) => {
                    params.push("@builtin(vertex_index) vid: u32".to_string());
                    let _ = writeln!(pro, "    v[{}] = vec4<u32>(vid - drv.misc.y);", d.reg);
                }
                Some(sv::INSTANCE_ID) => {
                    params.push("@builtin(instance_index) iid: u32".to_string());
                    let _ = writeln!(pro, "    v[{}] = vec4<u32>(iid - drv.misc.x);", d.reg);
                }
                _ => {
                    let Some(e) = e else { continue };
                    let ty = match e.component_type {
                        ComponentType::Uint => "vec4<u32>",
                        ComponentType::Sint => "vec4<i32>",
                        _ => "vec4<f32>",
                    };
                    if params.iter().any(|p: &String| p.starts_with(&format!("@location({}) ", d.reg))) {
                        continue;
                    }
                    params.push(format!("@location({}) a{}: {ty}", d.reg, d.reg));
                    let _ = writeln!(pro, "    v[{}] = bitcast<vec4<u32>>(a{});", d.reg, d.reg);
                    t.vertex_inputs.push(VertexInput {
                        location: d.reg,
                        name: e.name.clone(),
                        index: e.index,
                        ty: e.component_type,
                    });
                }
            }
        }
        params.sort();
        params.dedup();
        // Outputs.
        let slots: Vec<LinkSlot> = match &self.key.outputs {
            Some(s) => s.clone(),
            None => {
                let mut regs: Vec<u32> =
                    r.outputs.iter().filter(|e| e.system_value == sv::UNDEFINED).map(|e| e.register).collect();
                regs.sort();
                regs.dedup();
                regs.iter()
                    .map(|reg| LinkSlot {
                        interp: Interp::Perspective,
                        elements: r
                            .output_elements(*reg)
                            .filter(|e| e.system_value == sv::UNDEFINED)
                            .map(|e| LinkElement { name: e.name.clone(), index: e.index, mask: e.mask })
                            .collect(),
                    })
                    .collect()
            }
        };
        m.push_str("struct VsOut {\n    @builtin(position) pos: vec4<f32>,\n");
        let mut epi = String::new();
        for (k, slot) in slots.iter().enumerate() {
            let _ = writeln!(m, "    @location({k}){} l{k}: {},", slot.interp.attr(), slot.interp.ty());
            let mut comps = ["0u".to_string(), "0u".to_string(), "0u".to_string(), "0u".to_string()];
            for el in &slot.elements {
                if let Some(src) = r.outputs.iter().find(|e| e.same_semantic(&el.name, el.index)) {
                    let dst_c: Vec<usize> = (0..4).filter(|c| el.mask & (1 << c) != 0).collect();
                    let src_c: Vec<usize> = (0..4).filter(|c| src.mask & (1 << c) != 0).collect();
                    for (d, s) in dst_c.iter().zip(&src_c) {
                        comps[*d] = format!("o[{}].{}", src.register, C[*s]);
                    }
                }
            }
            let v = format!("vec4<u32>({})", comps.join(", "));
            let v = if slot.interp == Interp::Flat { v } else { format!("bitcast<vec4<f32>>({v})") };
            let _ = writeln!(epi, "    out.l{k} = {v};");
        }
        let mut loc = slots.len() as u32;
        // Clip and cull distances, in element order.
        let mut clip = Vec::new();
        let mut clip_elems: Vec<&SigElement> = r
            .outputs
            .iter()
            .filter(|e| e.system_value == sv::CLIP_DISTANCE || e.system_value == sv::CULL_DISTANCE)
            .collect();
        clip_elems.sort_by_key(|e| (e.system_value, e.index));
        for e in clip_elems {
            for c in 0..4 {
                if e.mask & (1 << c) != 0 {
                    clip.push(format!("bitcast<f32>(o[{}].{})", e.register, C[c]));
                }
            }
        }
        match self.key.clip {
            Clip::Builtin(n) if n > 0 => {
                let _ = writeln!(m, "    @builtin(clip_distances) clip: array<f32, {n}>,");
                for i in 0..n as usize {
                    let _ = writeln!(epi, "    out.clip[{i}] = {};", clip.get(i).cloned().unwrap_or("1.0".into()));
                }
            }
            Clip::Varying(n) if n > 0 => {
                let _ =
                    writeln!(m, "    @location({loc}) clip0: vec4<f32>,\n    @location({}) clip1: vec4<f32>,", loc + 1);
                let c: Vec<String> = (0..8).map(|i| clip.get(i).cloned().unwrap_or("1.0".into())).collect();
                let _ = writeln!(epi, "    out.clip0 = vec4<f32>({}, {}, {}, {});", c[0], c[1], c[2], c[3]);
                let _ = writeln!(epi, "    out.clip1 = vec4<f32>({}, {}, {}, {});", c[4], c[5], c[6], c[7]);
                loc += 2;
            }
            _ => {}
        }
        m.push_str("}\n\n");
        t.varyings = loc;
        let pos = r
            .outputs
            .iter()
            .find(|e| e.system_value == sv::POSITION)
            .map(|e| format!("bitcast<vec4<f32>>(o[{}])", e.register))
            .unwrap_or_else(|| "vec4<f32>(0.0, 0.0, 0.0, 1.0)".into());
        let _ = writeln!(m, "@vertex\nfn main({}) -> VsOut {{", params.join(", "));
        m.push_str(&pro);
        m.push_str("    shader_main();\n    var out: VsOut;\n");
        let _ = writeln!(m, "    let pos = {pos};");
        if self.key.pos_fixup {
            m.push_str("    out.pos = vec4<f32>(pos.xy * drv.pos_fixup.xy + pos.w * drv.pos_fixup.zw, pos.zw);\n");
        } else {
            m.push_str("    out.pos = pos;\n");
        }
        m.push_str(&epi);
        m.push_str("    return out;\n}\n");
        Ok(())
    }

    fn ps_entry(&self, m: &mut String, t: &mut Translation) -> Result<(), Error> {
        let r = &self.sh.reflection;
        let slots = ps_linkage(r);
        let mut params = vec!["@builtin(position) fpos: vec4<f32>".to_string()];
        let mut pro = String::new();
        let mut loc = 0u32;
        let mut linked_regs = Vec::new();
        for d in &r.input_decls {
            if d.sv.is_some() {
                continue;
            }
            if r.input_elements(d.reg).any(|e| e.system_value == sv::UNDEFINED) && !linked_regs.contains(&d.reg) {
                linked_regs.push(d.reg);
            }
        }
        linked_regs.sort();
        for (k, slot) in slots.iter().enumerate() {
            let reg = linked_regs[k];
            params.push(format!("@location({k}){} l{k}: {}", slot.interp.attr(), slot.interp.ty()));
            let _ = writeln!(pro, "    v[{reg}] = bitcast<vec4<u32>>(l{k});");
            loc += 1;
        }
        for d in &r.input_decls {
            let e = r.input_elements(d.reg).find(|e| e.system_value != sv::UNDEFINED);
            match d.sv.or(e.map(|e| e.system_value)) {
                Some(sv::POSITION) => {
                    // Direct3D's SV_Position.w is the clip-space w.
                    let _ = writeln!(pro, "    v[{}] = bitcast<vec4<u32>>(vec4<f32>(fpos.xyz, 1.0 / fpos.w));", d.reg);
                }
                Some(sv::IS_FRONT_FACE) => {
                    params.push("@builtin(front_facing) ff: bool".into());
                    let _ = writeln!(pro, "    v[{}] = vec4<u32>(select(0u, 0xffffffffu, ff));", d.reg);
                }
                Some(sv::SAMPLE_INDEX) => {
                    params.push("@builtin(sample_index) si: u32".into());
                    let _ = writeln!(pro, "    v[{}] = vec4<u32>(si);", d.reg);
                }
                Some(sv::CLIP_DISTANCE)
                | Some(sv::CULL_DISTANCE)
                | Some(sv::PRIMITIVE_ID)
                | Some(sv::RENDER_TARGET_ARRAY_INDEX)
                | Some(sv::VIEWPORT_ARRAY_INDEX) => {}
                _ => {}
            }
        }
        if r.special_inputs.contains(&RegType::InputCoverageMask) {
            params.push("@builtin(sample_mask) cov: u32".into());
            pro += "    vcov = vec4<u32>(cov);\n";
        }
        params.sort_by_key(|p| !p.starts_with("@builtin(position)"));
        if let Clip::Varying(n) = self.key.clip {
            if n > 0 {
                params.push(format!("@location({loc}) clip0: vec4<f32>"));
                params.push(format!("@location({}) clip1: vec4<f32>", loc + 1));
                for i in 0..n as usize {
                    let c = if i < 4 { format!("clip0.{}", C[i]) } else { format!("clip1.{}", C[i - 4]) };
                    let _ = writeln!(pro, "    if ({c} < 0.0) {{ discard; }}");
                }
                loc += 2;
            }
        }
        t.varyings = loc;
        // Outputs.
        let mut targets: Vec<(u32, ComponentType)> = r
            .outputs
            .iter()
            .filter(|e| {
                e.system_value == sv::TARGET
                    || (e.system_value == sv::UNDEFINED && e.name.eq_ignore_ascii_case("SV_Target"))
            })
            .map(|e| (e.register, e.component_type))
            .collect();
        targets.sort_by_key(|t| t.0);
        targets.dedup_by_key(|t| t.0);
        let has_out = !targets.is_empty() || r.writes_depth || r.writes_coverage;
        if has_out {
            m.push_str("struct PsOut {\n");
            for (reg, ct) in &targets {
                let ty = match ct {
                    ComponentType::Uint => "vec4<u32>",
                    ComponentType::Sint => "vec4<i32>",
                    _ => "vec4<f32>",
                };
                let _ = writeln!(m, "    @location({reg}) c{reg}: {ty},");
            }
            if r.writes_depth {
                m.push_str("    @builtin(frag_depth) depth: f32,\n");
            }
            if r.writes_coverage {
                m.push_str("    @builtin(sample_mask) mask: u32,\n");
            }
            m.push_str("}\n\n");
        }
        let _ = writeln!(m, "@fragment\nfn main({}){} {{", params.join(", "), if has_out { " -> PsOut" } else { "" });
        m.push_str(&pro);
        m.push_str("    shader_main();\n");
        if has_out {
            m.push_str("    var out: PsOut;\n");
            for (reg, ct) in &targets {
                let v = match ct {
                    ComponentType::Uint => format!("o[{reg}]"),
                    ComponentType::Sint => format!("bitcast<vec4<i32>>(o[{reg}])"),
                    _ => format!("bitcast<vec4<f32>>(o[{reg}])"),
                };
                let _ = writeln!(m, "    out.c{reg} = {v};");
            }
            if r.writes_depth {
                m.push_str("    out.depth = bitcast<f32>(odepth.x);\n");
            }
            if r.writes_coverage {
                m.push_str("    out.mask = omask.x;\n");
            }
            m.push_str("    return out;\n");
        }
        m.push_str("}\n");
        t.targets = targets;
        Ok(())
    }

    fn cs_entry(&self, m: &mut String) -> Result<(), Error> {
        let [x, y, z] = self.sh.reflection.thread_group;
        let _ = writeln!(
            m,
            "@compute @workgroup_size({}, {}, {})\nfn main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(workgroup_id) wid: vec3<u32>, @builtin(local_invocation_id) lid: vec3<u32>, @builtin(local_invocation_index) lidx: u32) {{",
            x.max(1),
            y.max(1),
            z.max(1)
        );
        m.push_str("    vtid = vec4<u32>(gid, 0u);\n    vgid = vec4<u32>(wid, 0u);\n    vlid = vec4<u32>(lid, 0u);\n    vlidx = vec4<u32>(lidx);\n");
        m.push_str("    shader_main();\n}\n");
        Ok(())
    }
}

const HELPERS: &str = "fn d3d_ftoi(a: vec4<f32>) -> vec4<i32> {
    return select(vec4<i32>(a), vec4<i32>(0), a != a);
}
fn d3d_ftou(a: vec4<f32>) -> vec4<u32> {
    return select(vec4<u32>(a), vec4<u32>(0u), a != a);
}
fn d3d_firstbit_hi(a: vec4<u32>) -> vec4<u32> {
    return select(vec4<u32>(31u) - firstLeadingBit(a), vec4<u32>(0xffffffffu), a == vec4<u32>(0u));
}
fn d3d_firstbit_shi(a: vec4<i32>) -> vec4<i32> {
    let f = firstLeadingBit(a);
    return select(vec4<i32>(31) - f, vec4<i32>(-1), f == vec4<i32>(-1));
}
fn d3d_umul_hi1(a: u32, b: u32) -> u32 {
    let al = a & 0xffffu;
    let ah = a >> 16u;
    let bl = b & 0xffffu;
    let bh = b >> 16u;
    let lo = al * bl;
    let m1 = ah * bl;
    let m2 = al * bh;
    let mid = (lo >> 16u) + (m1 & 0xffffu) + (m2 & 0xffffu);
    return ah * bh + (m1 >> 16u) + (m2 >> 16u) + (mid >> 16u);
}
fn d3d_umul_hi(a: vec4<u32>, b: vec4<u32>) -> vec4<u32> {
    return vec4<u32>(d3d_umul_hi1(a.x, b.x), d3d_umul_hi1(a.y, b.y), d3d_umul_hi1(a.z, b.z), d3d_umul_hi1(a.w, b.w));
}
fn d3d_imul_hi1(a: i32, b: i32) -> u32 {
    var hi = d3d_umul_hi1(bitcast<u32>(a), bitcast<u32>(b));
    if (a < 0) { hi = hi - bitcast<u32>(b); }
    if (b < 0) { hi = hi - bitcast<u32>(a); }
    return hi;
}
fn d3d_imul_hi(a: vec4<i32>, b: vec4<i32>) -> vec4<u32> {
    return vec4<u32>(d3d_imul_hi1(a.x, b.x), d3d_imul_hi1(a.y, b.y), d3d_imul_hi1(a.z, b.z), d3d_imul_hi1(a.w, b.w));
}

";
