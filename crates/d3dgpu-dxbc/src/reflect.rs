//! What a shader declares and uses, gathered from its declarations,
//! signatures and instructions.

use crate::container::SigElement;
use crate::decode::*;

/// A shader resource (`t#`) declaration.
#[derive(Clone, Debug, PartialEq)]
pub struct ResourceDecl {
    pub slot: u32,
    pub dim: ResourceDim,
    pub ret: [ReturnType; 4],
    /// Structured buffers: bytes per element.
    pub stride: u32,
    /// Read with `sample_c`/`gather4_c` (needs a comparison-capable depth binding).
    pub compared: bool,
    /// Read with filtering instructions (`sample*`, `gather4*`, `lod`).
    pub sampled: bool,
}

/// An unordered access view (`u#`) declaration.
#[derive(Clone, Debug, PartialEq)]
pub struct UavDecl {
    pub slot: u32,
    pub dim: ResourceDim,
    pub ret: [ReturnType; 4],
    pub stride: u32,
    pub read: bool,
    pub written: bool,
    pub atomic: bool,
    /// Uses a signed atomic (min/max), which WGSL can't mix with unsigned
    /// ones on one buffer.
    pub signed_atomic: bool,
}

/// Thread group shared memory (`g#`).
#[derive(Clone, Debug, PartialEq)]
pub struct TgsmDecl {
    pub reg: u32,
    /// Size in 32-bit words.
    pub words: u32,
    /// 0 for raw.
    pub stride: u32,
    pub atomic: bool,
}

/// Interpolation of a pixel shader input register (`dcl_input_ps`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum InterpMode {
    #[default]
    Undefined,
    Constant,
    Linear,
    LinearCentroid,
    LinearNoPerspective,
    LinearNoPerspectiveCentroid,
    LinearSample,
    LinearNoPerspectiveSample,
}

impl InterpMode {
    fn from_u32(v: u32) -> InterpMode {
        use InterpMode::*;
        match v {
            1 => Constant,
            2 => Linear,
            3 => LinearCentroid,
            4 => LinearNoPerspective,
            5 => LinearNoPerspectiveCentroid,
            6 => LinearSample,
            7 => LinearNoPerspectiveSample,
            _ => Undefined,
        }
    }
}

/// An input or output register declaration.
#[derive(Clone, Debug, PartialEq)]
pub struct RegDecl {
    pub ty: RegType,
    pub reg: u32,
    pub mask: u8,
    pub interp: InterpMode,
    /// System value name from `_siv`/`_sgv` declarations.
    pub sv: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Reflection {
    pub inputs: Vec<SigElement>,
    pub outputs: Vec<SigElement>,
    pub input_decls: Vec<RegDecl>,
    pub output_decls: Vec<RegDecl>,
    pub temps: u32,
    /// `x#[size]`, components.
    pub indexable_temps: Vec<(u32, u32, u32)>,
    /// Constant buffer slot and size in vec4s.
    pub cbuffers: Vec<(u32, u32)>,
    pub resources: Vec<ResourceDecl>,
    /// Sampler slot and comparison mode.
    pub samplers: Vec<(u32, bool)>,
    /// (resource, sampler) slot pairs used together by sampling
    /// instructions.
    pub pairs: Vec<(u32, u32)>,
    pub uavs: Vec<UavDecl>,
    pub tgsm: Vec<TgsmDecl>,
    /// Immediate constant buffer contents (vec4s of u32).
    pub icb: Vec<[u32; 4]>,
    pub thread_group: [u32; 3],
    /// Special input registers read (thread ids, coverage, primitive id).
    pub special_inputs: Vec<RegType>,
    pub writes_depth: bool,
    pub writes_coverage: bool,
    pub uses_discard: bool,
    pub uses_derivatives: bool,
    /// Instructions the translator can't express on WebGPU.
    pub unsupported: Vec<Opcode>,
}

impl Reflection {
    pub fn new(p: &Program, inputs: Vec<SigElement>, outputs: Vec<SigElement>) -> Reflection {
        let mut r = Reflection { inputs, outputs, ..Default::default() };
        for ins in &p.instructions {
            let op0 = ins.operands.first();
            let ret4 =
                |w: u32| -> [ReturnType; 4] { std::array::from_fn(|i| ReturnType::from_u32((w >> (4 * i)) & 0xf)) };
            match ins.op {
                Opcode::DclTemps => r.temps = ins.extra[0],
                Opcode::DclIndexableTemp => r.indexable_temps.push((ins.extra[0], ins.extra[1], ins.extra[2])),
                Opcode::DclConstantBuffer => {
                    let o = op0.unwrap();
                    let size = o.indices.get(1).map(|i| i.imm).unwrap_or(4096);
                    r.cbuffers.push((o.reg(), size.clamp(1, 4096)));
                }
                Opcode::DclSampler => r.samplers.push((op0.unwrap().reg(), (ins.controls & 0xf) == 1)),
                Opcode::DclResource => r.resources.push(ResourceDecl {
                    slot: op0.unwrap().reg(),
                    dim: ResourceDim::from_u32(ins.controls & 0x1f),
                    ret: ret4(ins.extra[0]),
                    stride: 0,
                    compared: false,
                    sampled: false,
                }),
                Opcode::DclResourceRaw | Opcode::DclResourceStructured => r.resources.push(ResourceDecl {
                    slot: op0.unwrap().reg(),
                    dim: if ins.op == Opcode::DclResourceRaw {
                        ResourceDim::RawBuffer
                    } else {
                        ResourceDim::StructuredBuffer
                    },
                    ret: [ReturnType::Uint; 4],
                    stride: ins.extra.first().copied().unwrap_or(0),
                    compared: false,
                    sampled: false,
                }),
                Opcode::DclUavTyped | Opcode::DclUavRaw | Opcode::DclUavStructured => {
                    let (dim, ret, stride) = match ins.op {
                        Opcode::DclUavTyped => (ResourceDim::from_u32(ins.controls & 0x1f), ret4(ins.extra[0]), 0),
                        Opcode::DclUavRaw => (ResourceDim::RawBuffer, [ReturnType::Uint; 4], 0),
                        _ => (ResourceDim::StructuredBuffer, [ReturnType::Uint; 4], ins.extra[0]),
                    };
                    r.uavs.push(UavDecl {
                        slot: op0.unwrap().reg(),
                        dim,
                        ret,
                        stride,
                        read: false,
                        written: false,
                        atomic: false,
                        signed_atomic: false,
                    });
                }
                Opcode::DclTgsmRaw => r.tgsm.push(TgsmDecl {
                    reg: op0.unwrap().reg(),
                    words: ins.extra[0].div_ceil(4),
                    stride: 0,
                    atomic: false,
                }),
                Opcode::DclTgsmStructured => r.tgsm.push(TgsmDecl {
                    reg: op0.unwrap().reg(),
                    words: (ins.extra[0] * ins.extra[1]).div_ceil(4),
                    stride: ins.extra[0],
                    atomic: false,
                }),
                Opcode::DclThreadGroup => r.thread_group = [ins.extra[0], ins.extra[1], ins.extra[2]],
                Opcode::CustomData if ins.controls == 3 => {
                    r.icb =
                        ins.data.chunks(4).map(|c| std::array::from_fn(|i| c.get(i).copied().unwrap_or(0))).collect();
                }
                Opcode::DclInput
                | Opcode::DclInputSgv
                | Opcode::DclInputSiv
                | Opcode::DclInputPs
                | Opcode::DclInputPsSgv
                | Opcode::DclInputPsSiv => {
                    let o = op0.unwrap();
                    let interp = if matches!(ins.op, Opcode::DclInputPs | Opcode::DclInputPsSiv | Opcode::DclInputPsSgv)
                    {
                        InterpMode::from_u32(ins.controls & 0xf)
                    } else {
                        InterpMode::Undefined
                    };
                    let sv = ins.extra.first().copied();
                    if matches!(o.ty, RegType::Input) {
                        r.input_decls.push(RegDecl { ty: o.ty, reg: o.reg(), mask: o.select.mask(), interp, sv });
                    } else if !r.special_inputs.contains(&o.ty) {
                        r.special_inputs.push(o.ty);
                    }
                }
                Opcode::DclOutput | Opcode::DclOutputSgv | Opcode::DclOutputSiv => {
                    let o = op0.unwrap();
                    match o.ty {
                        RegType::OutputDepth | RegType::OutputDepthGreaterEqual | RegType::OutputDepthLessEqual => {
                            r.writes_depth = true
                        }
                        RegType::OutputCoverageMask => r.writes_coverage = true,
                        _ => r.output_decls.push(RegDecl {
                            ty: o.ty,
                            reg: o.reg(),
                            mask: o.select.mask(),
                            interp: InterpMode::Undefined,
                            sv: ins.extra.first().copied(),
                        }),
                    }
                }
                _ => {}
            }
            // Usage.
            let res = |r: &mut Reflection, slot: u32, f: &dyn Fn(&mut ResourceDecl)| {
                if let Some(d) = r.resources.iter_mut().find(|d| d.slot == slot) {
                    f(d)
                }
            };
            if matches!(
                ins.op,
                Opcode::Sample
                    | Opcode::SampleB
                    | Opcode::SampleL
                    | Opcode::SampleD
                    | Opcode::Gather4
                    | Opcode::Gather4Po
                    | Opcode::Lod
                    | Opcode::SampleC
                    | Opcode::SampleCLz
                    | Opcode::Gather4C
                    | Opcode::Gather4PoC
            ) {
                let i = if matches!(ins.op, Opcode::Gather4Po | Opcode::Gather4PoC) { 3 } else { 2 };
                if let (Some(t), Some(s)) = (ins.operands.get(i), ins.operands.get(i + 1)) {
                    let pair = (t.reg(), s.reg());
                    if !r.pairs.contains(&pair) {
                        r.pairs.push(pair);
                    }
                }
            }
            match ins.op {
                Opcode::Sample
                | Opcode::SampleB
                | Opcode::SampleL
                | Opcode::SampleD
                | Opcode::Gather4
                | Opcode::Gather4Po
                | Opcode::Lod => {
                    if let Some(t) = ins.operands.get(2) {
                        res(&mut r, t.reg(), &|d| d.sampled = true);
                    }
                }
                Opcode::SampleC | Opcode::SampleCLz | Opcode::Gather4C | Opcode::Gather4PoC => {
                    let i = if matches!(ins.op, Opcode::Gather4PoC) { 3 } else { 2 };
                    if let Some(t) = ins.operands.get(i) {
                        res(&mut r, t.reg(), &|d| d.compared = true);
                    }
                }
                Opcode::DerivRtx
                | Opcode::DerivRty
                | Opcode::DerivRtxCoarse
                | Opcode::DerivRtxFine
                | Opcode::DerivRtyCoarse
                | Opcode::DerivRtyFine => r.uses_derivatives = true,
                Opcode::Discard => r.uses_discard = true,
                _ => {}
            }
            let uav = |r: &mut Reflection, o: Option<&Operand>, f: &dyn Fn(&mut UavDecl)| {
                if let Some(o) = o.filter(|o| o.ty == RegType::Uav) {
                    if let Some(d) = r.uavs.iter_mut().find(|d| d.slot == o.reg()) {
                        f(d)
                    }
                }
            };
            let tgsm_atomic = |r: &mut Reflection, o: Option<&Operand>| {
                if let Some(o) = o.filter(|o| o.ty == RegType::ThreadGroupSharedMemory) {
                    if let Some(d) = r.tgsm.iter_mut().find(|d| d.reg == o.reg()) {
                        d.atomic = true;
                    }
                }
            };
            match ins.op {
                Opcode::LdUavTyped | Opcode::LdRaw | Opcode::LdStructured => {
                    uav(&mut r, ins.operands.last(), &|d| d.read = true);
                }
                Opcode::StoreUavTyped | Opcode::StoreRaw | Opcode::StoreStructured => {
                    uav(&mut r, ins.operands.first(), &|d| d.written = true);
                }
                Opcode::AtomicAnd
                | Opcode::AtomicOr
                | Opcode::AtomicXor
                | Opcode::AtomicCmpStore
                | Opcode::AtomicIAdd
                | Opcode::AtomicUMax
                | Opcode::AtomicUMin => {
                    uav(&mut r, ins.operands.first(), &|d| d.atomic = true);
                    tgsm_atomic(&mut r, ins.operands.first());
                }
                Opcode::AtomicIMax | Opcode::AtomicIMin => {
                    uav(&mut r, ins.operands.first(), &|d| {
                        d.atomic = true;
                        d.signed_atomic = true
                    });
                    tgsm_atomic(&mut r, ins.operands.first());
                }
                Opcode::ImmAtomicIAdd
                | Opcode::ImmAtomicAnd
                | Opcode::ImmAtomicOr
                | Opcode::ImmAtomicXor
                | Opcode::ImmAtomicExch
                | Opcode::ImmAtomicCmpExch
                | Opcode::ImmAtomicUMax
                | Opcode::ImmAtomicUMin
                | Opcode::ImmAtomicIMax
                | Opcode::ImmAtomicIMin => {
                    let signed = matches!(ins.op, Opcode::ImmAtomicIMax | Opcode::ImmAtomicIMin);
                    uav(&mut r, ins.operands.get(1), &|d| {
                        d.atomic = true;
                        d.signed_atomic |= signed
                    });
                    tgsm_atomic(&mut r, ins.operands.get(1));
                }
                Opcode::ImmAtomicAlloc
                | Opcode::ImmAtomicConsume
                | Opcode::Lod
                | Opcode::Msad
                | Opcode::Gather4Po
                | Opcode::Gather4PoC
                | Opcode::EvalSnapped
                | Opcode::InterfaceCall
                | Opcode::Emit
                | Opcode::Cut
                | Opcode::EmitThenCut
                | Opcode::EmitStream
                | Opcode::CutStream
                | Opcode::EmitThenCutStream => r.unsupported.push(ins.op),
                op if op.name().starts_with('d')
                    && op.code() >= Opcode::DAdd.code()
                    && !op.name().starts_with("dcl")
                    // Double precision: WebGPU has no f64.
                    && !matches!(op, Opcode::DebugBreak) =>
                {
                    r.unsupported.push(op)
                }
                _ => {}
            }
            for o in &ins.operands {
                if matches!(
                    o.ty,
                    RegType::InputThreadId
                        | RegType::InputThreadGroupId
                        | RegType::InputThreadIdInGroup
                        | RegType::InputThreadIdInGroupFlattened
                        | RegType::InputCoverageMask
                        | RegType::InputPrimitiveId
                        | RegType::InputGsInstanceId
                ) && !r.special_inputs.contains(&o.ty)
                {
                    r.special_inputs.push(o.ty);
                }
            }
        }
        r.unsupported.sort_by_key(|o| o.code());
        r.unsupported.dedup();
        r
    }

    pub fn resource(&self, slot: u32) -> Option<&ResourceDecl> {
        self.resources.iter().find(|d| d.slot == slot)
    }

    pub fn uav(&self, slot: u32) -> Option<&UavDecl> {
        self.uavs.iter().find(|d| d.slot == slot)
    }

    /// The input signature element(s) at register `reg`.
    pub fn input_elements(&self, reg: u32) -> impl Iterator<Item = &SigElement> {
        self.inputs.iter().filter(move |e| e.register == reg)
    }

    pub fn output_elements(&self, reg: u32) -> impl Iterator<Item = &SigElement> {
        self.outputs.iter().filter(move |e| e.register == reg)
    }
}
