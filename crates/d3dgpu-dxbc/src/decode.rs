//! Shader model 4/5 token stream → [`Program`].

use crate::Error;

/// Shader stage (`D3D10_SB_TOKENIZED_PROGRAM_TYPE`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProgramType {
    Pixel,
    Vertex,
    Geometry,
    Hull,
    Domain,
    Compute,
}

macro_rules! opcodes {
    ($($name:ident = $n:expr, $text:expr;)*) => {
        /// Instruction and declaration opcodes.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Opcode { $($name,)* }
        impl Opcode {
            pub fn from_u32(v: u32) -> Option<Opcode> {
                match v { $($n => Some(Opcode::$name),)* _ => None }
            }
            pub fn code(self) -> u32 {
                match self { $(Opcode::$name => $n,)* }
            }
            pub fn name(self) -> &'static str {
                match self { $(Opcode::$name => $text,)* }
            }
            pub const ALL: &'static [Opcode] = &[$(Opcode::$name,)*];
        }
    };
}

opcodes! {
    Add = 0x00, "add";
    And = 0x01, "and";
    Break = 0x02, "break";
    BreakC = 0x03, "breakc";
    Call = 0x04, "call";
    CallC = 0x05, "callc";
    Case = 0x06, "case";
    Continue = 0x07, "continue";
    ContinueC = 0x08, "continuec";
    Cut = 0x09, "cut";
    Default = 0x0a, "default";
    DerivRtx = 0x0b, "deriv_rtx";
    DerivRty = 0x0c, "deriv_rty";
    Discard = 0x0d, "discard";
    Div = 0x0e, "div";
    Dp2 = 0x0f, "dp2";
    Dp3 = 0x10, "dp3";
    Dp4 = 0x11, "dp4";
    Else = 0x12, "else";
    Emit = 0x13, "emit";
    EmitThenCut = 0x14, "emitthencut";
    EndIf = 0x15, "endif";
    EndLoop = 0x16, "endloop";
    EndSwitch = 0x17, "endswitch";
    Eq = 0x18, "eq";
    Exp = 0x19, "exp";
    Frc = 0x1a, "frc";
    Ftoi = 0x1b, "ftoi";
    Ftou = 0x1c, "ftou";
    Ge = 0x1d, "ge";
    IAdd = 0x1e, "iadd";
    If = 0x1f, "if";
    IEq = 0x20, "ieq";
    IGe = 0x21, "ige";
    ILt = 0x22, "ilt";
    IMad = 0x23, "imad";
    IMax = 0x24, "imax";
    IMin = 0x25, "imin";
    IMul = 0x26, "imul";
    INe = 0x27, "ine";
    INeg = 0x28, "ineg";
    IShl = 0x29, "ishl";
    IShr = 0x2a, "ishr";
    Itof = 0x2b, "itof";
    Label = 0x2c, "label";
    Ld = 0x2d, "ld";
    LdMs = 0x2e, "ld2dms";
    Log = 0x2f, "log";
    Loop = 0x30, "loop";
    Lt = 0x31, "lt";
    Mad = 0x32, "mad";
    Min = 0x33, "min";
    Max = 0x34, "max";
    CustomData = 0x35, "customdata";
    Mov = 0x36, "mov";
    Movc = 0x37, "movc";
    Mul = 0x38, "mul";
    Ne = 0x39, "ne";
    Nop = 0x3a, "nop";
    Not = 0x3b, "not";
    Or = 0x3c, "or";
    ResInfo = 0x3d, "resinfo";
    Ret = 0x3e, "ret";
    RetC = 0x3f, "retc";
    RoundNe = 0x40, "round_ne";
    RoundNi = 0x41, "round_ni";
    RoundPi = 0x42, "round_pi";
    RoundZ = 0x43, "round_z";
    Rsq = 0x44, "rsq";
    Sample = 0x45, "sample";
    SampleC = 0x46, "sample_c";
    SampleCLz = 0x47, "sample_c_lz";
    SampleL = 0x48, "sample_l";
    SampleD = 0x49, "sample_d";
    SampleB = 0x4a, "sample_b";
    Sqrt = 0x4b, "sqrt";
    Switch = 0x4c, "switch";
    SinCos = 0x4d, "sincos";
    UDiv = 0x4e, "udiv";
    ULt = 0x4f, "ult";
    UGe = 0x50, "uge";
    UMul = 0x51, "umul";
    UMad = 0x52, "umad";
    UMax = 0x53, "umax";
    UMin = 0x54, "umin";
    UShr = 0x55, "ushr";
    Utof = 0x56, "utof";
    Xor = 0x57, "xor";
    DclResource = 0x58, "dcl_resource";
    DclConstantBuffer = 0x59, "dcl_constantbuffer";
    DclSampler = 0x5a, "dcl_sampler";
    DclIndexRange = 0x5b, "dcl_indexrange";
    DclGsOutputTopology = 0x5c, "dcl_outputtopology";
    DclGsInputPrimitive = 0x5d, "dcl_inputprimitive";
    DclMaxOutputVertexCount = 0x5e, "dcl_maxout";
    DclInput = 0x5f, "dcl_input";
    DclInputSgv = 0x60, "dcl_input_sgv";
    DclInputSiv = 0x61, "dcl_input_siv";
    DclInputPs = 0x62, "dcl_input_ps";
    DclInputPsSgv = 0x63, "dcl_input_ps_sgv";
    DclInputPsSiv = 0x64, "dcl_input_ps_siv";
    DclOutput = 0x65, "dcl_output";
    DclOutputSgv = 0x66, "dcl_output_sgv";
    DclOutputSiv = 0x67, "dcl_output_siv";
    DclTemps = 0x68, "dcl_temps";
    DclIndexableTemp = 0x69, "dcl_indexabletemp";
    DclGlobalFlags = 0x6a, "dcl_globalflags";
    Lod = 0x6c, "lod";
    Gather4 = 0x6d, "gather4";
    SamplePos = 0x6e, "samplepos";
    SampleInfo = 0x6f, "sampleinfo";
    HsDecls = 0x71, "hs_decls";
    HsControlPointPhase = 0x72, "hs_control_point_phase";
    HsForkPhase = 0x73, "hs_fork_phase";
    HsJoinPhase = 0x74, "hs_join_phase";
    EmitStream = 0x75, "emit_stream";
    CutStream = 0x76, "cut_stream";
    EmitThenCutStream = 0x77, "emitthencut_stream";
    InterfaceCall = 0x78, "fcall";
    BufInfo = 0x79, "bufinfo";
    DerivRtxCoarse = 0x7a, "deriv_rtx_coarse";
    DerivRtxFine = 0x7b, "deriv_rtx_fine";
    DerivRtyCoarse = 0x7c, "deriv_rty_coarse";
    DerivRtyFine = 0x7d, "deriv_rty_fine";
    Gather4C = 0x7e, "gather4_c";
    Gather4Po = 0x7f, "gather4_po";
    Gather4PoC = 0x80, "gather4_po_c";
    Rcp = 0x81, "rcp";
    F32ToF16 = 0x82, "f32tof16";
    F16ToF32 = 0x83, "f16tof32";
    UAddC = 0x84, "uaddc";
    USubB = 0x85, "usubb";
    CountBits = 0x86, "countbits";
    FirstBitHi = 0x87, "firstbit_hi";
    FirstBitLo = 0x88, "firstbit_lo";
    FirstBitShi = 0x89, "firstbit_shi";
    UBfe = 0x8a, "ubfe";
    IBfe = 0x8b, "ibfe";
    Bfi = 0x8c, "bfi";
    BfRev = 0x8d, "bfrev";
    SwapC = 0x8e, "swapc";
    DclStream = 0x8f, "dcl_stream";
    DclFunctionBody = 0x90, "dcl_function_body";
    DclFunctionTable = 0x91, "dcl_function_table";
    DclInterface = 0x92, "dcl_interface";
    DclInputControlPointCount = 0x93, "dcl_input_control_point_count";
    DclOutputControlPointCount = 0x94, "dcl_output_control_point_count";
    DclTessDomain = 0x95, "dcl_tessellator_domain";
    DclTessPartitioning = 0x96, "dcl_tessellator_partitioning";
    DclTessOutputPrimitive = 0x97, "dcl_tessellator_output_primitive";
    DclHsMaxTessFactor = 0x98, "dcl_hs_max_tessfactor";
    DclHsForkPhaseInstanceCount = 0x99, "dcl_hs_fork_phase_instance_count";
    DclHsJoinPhaseInstanceCount = 0x9a, "dcl_hs_join_phase_instance_count";
    DclThreadGroup = 0x9b, "dcl_thread_group";
    DclUavTyped = 0x9c, "dcl_uav_typed";
    DclUavRaw = 0x9d, "dcl_uav_raw";
    DclUavStructured = 0x9e, "dcl_uav_structured";
    DclTgsmRaw = 0x9f, "dcl_tgsm_raw";
    DclTgsmStructured = 0xa0, "dcl_tgsm_structured";
    DclResourceRaw = 0xa1, "dcl_resource_raw";
    DclResourceStructured = 0xa2, "dcl_resource_structured";
    LdUavTyped = 0xa3, "ld_uav_typed";
    StoreUavTyped = 0xa4, "store_uav_typed";
    LdRaw = 0xa5, "ld_raw";
    StoreRaw = 0xa6, "store_raw";
    LdStructured = 0xa7, "ld_structured";
    StoreStructured = 0xa8, "store_structured";
    AtomicAnd = 0xa9, "atomic_and";
    AtomicOr = 0xaa, "atomic_or";
    AtomicXor = 0xab, "atomic_xor";
    AtomicCmpStore = 0xac, "atomic_cmp_store";
    AtomicIAdd = 0xad, "atomic_iadd";
    AtomicIMax = 0xae, "atomic_imax";
    AtomicIMin = 0xaf, "atomic_imin";
    AtomicUMax = 0xb0, "atomic_umax";
    AtomicUMin = 0xb1, "atomic_umin";
    ImmAtomicAlloc = 0xb2, "imm_atomic_alloc";
    ImmAtomicConsume = 0xb3, "imm_atomic_consume";
    ImmAtomicIAdd = 0xb4, "imm_atomic_iadd";
    ImmAtomicAnd = 0xb5, "imm_atomic_and";
    ImmAtomicOr = 0xb6, "imm_atomic_or";
    ImmAtomicXor = 0xb7, "imm_atomic_xor";
    ImmAtomicExch = 0xb8, "imm_atomic_exch";
    ImmAtomicCmpExch = 0xb9, "imm_atomic_cmp_exch";
    ImmAtomicIMax = 0xba, "imm_atomic_imax";
    ImmAtomicIMin = 0xbb, "imm_atomic_imin";
    ImmAtomicUMax = 0xbc, "imm_atomic_umax";
    ImmAtomicUMin = 0xbd, "imm_atomic_umin";
    Sync = 0xbe, "sync";
    DAdd = 0xbf, "dadd";
    DMax = 0xc0, "dmax";
    DMin = 0xc1, "dmin";
    DMul = 0xc2, "dmul";
    DEq = 0xc3, "deq";
    DGe = 0xc4, "dge";
    DLt = 0xc5, "dlt";
    DNe = 0xc6, "dne";
    DMov = 0xc7, "dmov";
    DMovc = 0xc8, "dmovc";
    Dtof = 0xc9, "dtof";
    Ftod = 0xca, "ftod";
    EvalSnapped = 0xcb, "eval_snapped";
    EvalSampleIndex = 0xcc, "eval_sample_index";
    EvalCentroid = 0xcd, "eval_centroid";
    DclGsInstanceCount = 0xce, "dcl_gs_instances";
    Abort = 0xcf, "abort";
    DebugBreak = 0xd0, "debug_break";
    DDiv = 0xd2, "ddiv";
    DFma = 0xd3, "dfma";
    DRcp = 0xd4, "drcp";
    Msad = 0xd5, "msad";
    Dtoi = 0xd6, "dtoi";
    Dtou = 0xd7, "dtou";
    Itod = 0xd8, "itod";
    Utod = 0xd9, "utod";
}

/// Register files (`D3D10_SB_OPERAND_TYPE`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RegType {
    Temp,
    Input,
    Output,
    IndexableTemp,
    Immediate32,
    Immediate64,
    Sampler,
    Resource,
    ConstantBuffer,
    ImmediateConstantBuffer,
    Label,
    InputPrimitiveId,
    OutputDepth,
    Null,
    Rasterizer,
    OutputCoverageMask,
    Stream,
    FunctionBody,
    FunctionTable,
    Interface,
    FunctionInput,
    FunctionOutput,
    OutputControlPointId,
    InputForkInstanceId,
    InputJoinInstanceId,
    InputControlPoint,
    OutputControlPoint,
    InputPatchConstant,
    InputDomainPoint,
    ThisPointer,
    Uav,
    ThreadGroupSharedMemory,
    InputThreadId,
    InputThreadGroupId,
    InputThreadIdInGroup,
    InputCoverageMask,
    InputThreadIdInGroupFlattened,
    InputGsInstanceId,
    OutputDepthGreaterEqual,
    OutputDepthLessEqual,
    CycleCounter,
    OutputStencilRef,
    InnerCoverage,
}

impl RegType {
    fn from_u32(v: u32) -> Option<RegType> {
        use RegType::*;
        Some(match v {
            0 => Temp,
            1 => Input,
            2 => Output,
            3 => IndexableTemp,
            4 => Immediate32,
            5 => Immediate64,
            6 => Sampler,
            7 => Resource,
            8 => ConstantBuffer,
            9 => ImmediateConstantBuffer,
            10 => Label,
            11 => InputPrimitiveId,
            12 => OutputDepth,
            13 => Null,
            14 => Rasterizer,
            15 => OutputCoverageMask,
            16 => Stream,
            17 => FunctionBody,
            18 => FunctionTable,
            19 => Interface,
            20 => FunctionInput,
            21 => FunctionOutput,
            22 => OutputControlPointId,
            23 => InputForkInstanceId,
            24 => InputJoinInstanceId,
            25 => InputControlPoint,
            26 => OutputControlPoint,
            27 => InputPatchConstant,
            28 => InputDomainPoint,
            29 => ThisPointer,
            30 => Uav,
            31 => ThreadGroupSharedMemory,
            32 => InputThreadId,
            33 => InputThreadGroupId,
            34 => InputThreadIdInGroup,
            35 => InputCoverageMask,
            36 => InputThreadIdInGroupFlattened,
            37 => InputGsInstanceId,
            38 => OutputDepthGreaterEqual,
            39 => OutputDepthLessEqual,
            40 => CycleCounter,
            41 => OutputStencilRef,
            42 => InnerCoverage,
            _ => return None,
        })
    }
}

/// How an operand selects components.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Select {
    /// No components (labels, null, some declarations).
    None,
    /// Destination write mask (bits x, y, z, w).
    Mask(u8),
    /// Source swizzle.
    Swizzle([u8; 4]),
    /// One component replicated.
    Scalar(u8),
}

impl Select {
    /// The component read for output channel `i` (sources).
    pub fn comp(&self, i: usize) -> u8 {
        match self {
            Select::Swizzle(s) => s[i],
            Select::Scalar(c) => *c,
            Select::Mask(_) | Select::None => i as u8,
        }
    }
    /// The write mask (destinations); all four for other selections.
    pub fn mask(&self) -> u8 {
        match self {
            Select::Mask(m) => *m,
            _ => 0xf,
        }
    }
}

/// Operand modifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Modifier {
    #[default]
    None,
    Neg,
    Abs,
    AbsNeg,
}

/// One index of a register reference: an immediate, a relative operand,
/// or both added.
#[derive(Clone, Debug, PartialEq)]
pub struct Index {
    pub imm: u32,
    pub rel: Option<Box<Operand>>,
}

/// An operand.
#[derive(Clone, Debug, PartialEq)]
pub struct Operand {
    pub ty: RegType,
    /// Number of components (0, 1 or 4).
    pub comps: u8,
    pub select: Select,
    pub indices: Vec<Index>,
    /// Immediate values (`l(...)`), one or four.
    pub imm: Vec<u32>,
    pub modifier: Modifier,
    pub nonuniform: bool,
}

impl Operand {
    /// The first index as an immediate (the register number).
    pub fn reg(&self) -> u32 {
        self.indices.first().map(|i| i.imm).unwrap_or(0)
    }
}

/// Resource dimensions (`D3D10_SB_RESOURCE_DIMENSION`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ResourceDim {
    Unknown,
    Buffer,
    Texture1D,
    Texture2D,
    Texture2DMs,
    Texture3D,
    TextureCube,
    Texture1DArray,
    Texture2DArray,
    Texture2DMsArray,
    TextureCubeArray,
    RawBuffer,
    StructuredBuffer,
}

impl ResourceDim {
    pub fn from_u32(v: u32) -> ResourceDim {
        use ResourceDim::*;
        match v {
            1 => Buffer,
            2 => Texture1D,
            3 => Texture2D,
            4 => Texture2DMs,
            5 => Texture3D,
            6 => TextureCube,
            7 => Texture1DArray,
            8 => Texture2DArray,
            9 => Texture2DMsArray,
            10 => TextureCubeArray,
            11 => RawBuffer,
            12 => StructuredBuffer,
            _ => Unknown,
        }
    }
    /// Coordinates a sample/load needs (without array index).
    pub fn coord_count(self) -> usize {
        use ResourceDim::*;
        match self {
            Buffer | Texture1D | Texture1DArray | RawBuffer | StructuredBuffer => 1,
            Texture2D | Texture2DMs | Texture2DArray | Texture2DMsArray => 2,
            Texture3D | TextureCube | TextureCubeArray => 3,
            Unknown => 2,
        }
    }
    pub fn is_array(self) -> bool {
        matches!(
            self,
            ResourceDim::Texture1DArray
                | ResourceDim::Texture2DArray
                | ResourceDim::Texture2DMsArray
                | ResourceDim::TextureCubeArray
        )
    }
    pub fn is_ms(self) -> bool {
        matches!(self, ResourceDim::Texture2DMs | ResourceDim::Texture2DMsArray)
    }
}

/// Component return types of resources (`D3D10_SB_RESOURCE_RETURN_TYPE`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ReturnType {
    Unorm,
    Snorm,
    Sint,
    Uint,
    Float,
    Mixed,
    Double,
    Unknown,
}

impl ReturnType {
    pub fn from_u32(v: u32) -> ReturnType {
        match v {
            1 => ReturnType::Unorm,
            2 => ReturnType::Snorm,
            3 => ReturnType::Sint,
            4 => ReturnType::Uint,
            5 => ReturnType::Float,
            6 => ReturnType::Mixed,
            7 => ReturnType::Double,
            _ => ReturnType::Unknown,
        }
    }
}

/// One decoded instruction or declaration.
#[derive(Clone, Debug, PartialEq)]
pub struct Instruction {
    pub op: Opcode,
    /// The opcode-specific control bits 11..24 of the opcode token.
    pub controls: u32,
    pub saturate: bool,
    /// `_nz` (true) or `_z` for conditional instructions.
    pub test_nz: bool,
    /// Immediate texel offsets (`aoffimmi`).
    pub offsets: [i8; 3],
    /// Extended-token resource dimension and return types (SM5 `_indexable`).
    pub ext_dim: Option<ResourceDim>,
    pub ext_ret: Option<[ReturnType; 4]>,
    pub operands: Vec<Operand>,
    /// Raw dwords after the operands (counts, strides, return types,
    /// system value names, thread group size).
    pub extra: Vec<u32>,
    /// `customdata` payload (immediate constant buffers).
    pub data: Vec<u32>,
}

/// A decoded program.
#[derive(Clone, Debug, PartialEq)]
pub struct Program {
    pub ty: ProgramType,
    pub major: u8,
    pub minor: u8,
    pub instructions: Vec<Instruction>,
}

/// Raw dwords that follow the operands of a declaration.
fn trailing_dwords(op: Opcode) -> (usize, usize) {
    // (operands, trailing dwords)
    use Opcode::*;
    match op {
        DclTemps
        | DclMaxOutputVertexCount
        | DclGsInstanceCount
        | DclHsMaxTessFactor
        | DclHsForkPhaseInstanceCount
        | DclHsJoinPhaseInstanceCount => (0, 1),
        DclIndexableTemp => (0, 3),
        DclThreadGroup => (0, 3),
        DclResource | DclUavTyped => (1, 1),
        DclResourceStructured | DclUavStructured | DclTgsmRaw | DclIndexRange => (1, 1),
        DclTgsmStructured => (1, 2),
        DclInputSgv | DclInputSiv | DclInputPsSgv | DclInputPsSiv | DclOutputSgv | DclOutputSiv => (1, 1),
        _ => (usize::MAX, 0),
    }
}

pub fn decode(tokens: &[u32]) -> Result<Program, Error> {
    let version = *tokens.first().ok_or_else(|| Error::decode(0, "empty program"))?;
    let len = *tokens.get(1).ok_or_else(|| Error::decode(0, "missing length"))? as usize;
    let ty = match version >> 16 {
        0 => ProgramType::Pixel,
        1 => ProgramType::Vertex,
        2 => ProgramType::Geometry,
        3 => ProgramType::Hull,
        4 => ProgramType::Domain,
        5 => ProgramType::Compute,
        t => return Err(Error::decode(0, format!("unknown program type {t}"))),
    };
    let (major, minor) = (((version >> 4) & 0xf) as u8, (version & 0xf) as u8);
    if !(4..=5).contains(&major) {
        return Err(Error::Unsupported(format!("shader model {major}.{minor}")));
    }
    let end = len.min(tokens.len());
    let mut pos = 2;
    let mut instructions = Vec::new();
    while pos < end {
        let at = pos;
        let tok = tokens[pos];
        let raw_op = tok & 0x7ff;
        let op = Opcode::from_u32(raw_op).ok_or_else(|| Error::decode(at, format!("unknown opcode {raw_op:#x}")))?;
        if op == Opcode::CustomData {
            let n = *tokens.get(pos + 1).ok_or_else(|| Error::decode(at, "truncated customdata"))? as usize;
            if n < 2 || pos + n > end {
                return Err(Error::decode(at, "bad customdata length"));
            }
            let class = tok >> 11;
            let data = tokens[pos + 2..pos + n].to_vec();
            instructions.push(Instruction {
                op,
                controls: class,
                saturate: false,
                test_nz: false,
                offsets: [0; 3],
                ext_dim: None,
                ext_ret: None,
                operands: Vec::new(),
                extra: Vec::new(),
                data,
            });
            pos += n;
            continue;
        }
        let ilen = ((tok >> 24) & 0x7f) as usize;
        // dcl_interface and friends may use the next token as the length.
        let ilen =
            if ilen == 0 { *tokens.get(pos + 1).ok_or_else(|| Error::decode(at, "truncated"))? as usize } else { ilen };
        let iend = pos + ilen;
        if ilen == 0 || iend > end {
            return Err(Error::decode(at, format!("{} runs past the end", op.name())));
        }
        let mut ins = Instruction {
            op,
            controls: (tok >> 11) & 0x1fff,
            saturate: tok & (1 << 13) != 0,
            test_nz: tok & (1 << 18) != 0,
            offsets: [0; 3],
            ext_dim: None,
            ext_ret: None,
            operands: Vec::new(),
            extra: Vec::new(),
            data: Vec::new(),
        };
        let mut p = pos + 1;
        // Extended opcode tokens.
        let mut ext = tok & 0x8000_0000 != 0;
        while ext {
            let e = *tokens.get(p).ok_or_else(|| Error::decode(at, "truncated extended opcode"))?;
            p += 1;
            ext = e & 0x8000_0000 != 0;
            match e & 0x3f {
                1 => {
                    let s4 = |v: u32| (((v & 0xf) as i8) << 4) >> 4;
                    ins.offsets = [s4(e >> 9), s4(e >> 13), s4(e >> 17)];
                }
                2 => ins.ext_dim = Some(ResourceDim::from_u32((e >> 6) & 0x1f)),
                3 => {
                    ins.ext_ret = Some(std::array::from_fn(|i| ReturnType::from_u32((e >> (6 + 4 * i)) & 0xf)));
                }
                _ => {}
            }
        }
        let (n_operands, n_extra) = trailing_dwords(op);
        let mut count = 0;
        let limit = if n_operands == usize::MAX { iend } else { iend.saturating_sub(n_extra) };
        while p < limit && count < n_operands {
            let (o, np) = operand(tokens, p, iend, at)?;
            ins.operands.push(o);
            p = np;
            count += 1;
        }
        ins.extra = tokens[p..iend].to_vec();
        instructions.push(ins);
        pos = iend;
    }
    Ok(Program { ty, major, minor, instructions })
}

fn operand(t: &[u32], mut p: usize, end: usize, at: usize) -> Result<(Operand, usize), Error> {
    let tok = *t.get(p).filter(|_| p < end).ok_or_else(|| Error::decode(at, "missing operand"))?;
    p += 1;
    let comps = match tok & 3 {
        0 => 0,
        1 => 1,
        2 => 4,
        _ => return Err(Error::Unsupported("N-component operands".into())),
    };
    let select = if comps == 4 {
        match (tok >> 2) & 3 {
            0 => Select::Mask(((tok >> 4) & 0xf) as u8),
            1 => Select::Swizzle(std::array::from_fn(|i| ((tok >> (4 + 2 * i)) & 3) as u8)),
            2 => Select::Scalar(((tok >> 4) & 3) as u8),
            _ => return Err(Error::decode(at, "bad component selection")),
        }
    } else if comps == 1 {
        Select::Scalar(0)
    } else {
        Select::None
    };
    let raw_ty = (tok >> 12) & 0xff;
    let ty = RegType::from_u32(raw_ty).ok_or_else(|| Error::decode(at, format!("unknown operand type {raw_ty}")))?;
    let dims = ((tok >> 20) & 3) as usize;
    let mut o = Operand {
        ty,
        comps,
        select,
        indices: Vec::new(),
        imm: Vec::new(),
        modifier: Modifier::None,
        nonuniform: false,
    };
    if tok & 0x8000_0000 != 0 {
        let e = *t.get(p).ok_or_else(|| Error::decode(at, "truncated operand"))?;
        p += 1;
        if e & 0x3f == 1 {
            o.modifier = match (e >> 6) & 0xff {
                1 => Modifier::Neg,
                2 => Modifier::Abs,
                3 => Modifier::AbsNeg,
                _ => Modifier::None,
            };
            o.nonuniform = e & (1 << 17) != 0;
        }
    }
    if ty == RegType::Immediate32 || ty == RegType::Immediate64 {
        let n = if comps == 4 { 4 } else { 1 } * if ty == RegType::Immediate64 { 2 } else { 1 };
        o.imm = t.get(p..p + n).ok_or_else(|| Error::decode(at, "truncated immediate"))?.to_vec();
        p += n;
    }
    for d in 0..dims {
        let rep = (tok >> (22 + 3 * d)) & 7;
        let mut idx = Index { imm: 0, rel: None };
        match rep {
            0 => {
                idx.imm = *t.get(p).ok_or_else(|| Error::decode(at, "truncated index"))?;
                p += 1;
            }
            1 => {
                // 64-bit immediate index: the high dword is not used.
                idx.imm = *t.get(p + 1).ok_or_else(|| Error::decode(at, "truncated index"))?;
                p += 2;
            }
            2..=4 => {
                if rep == 3 {
                    idx.imm = *t.get(p).ok_or_else(|| Error::decode(at, "truncated index"))?;
                    p += 1;
                } else if rep == 4 {
                    idx.imm = *t.get(p + 1).ok_or_else(|| Error::decode(at, "truncated index"))?;
                    p += 2;
                }
                let (r, np) = operand(t, p, end, at)?;
                idx.rel = Some(Box::new(r));
                p = np;
            }
            _ => return Err(Error::decode(at, "bad index representation")),
        }
        o.indices.push(idx);
    }
    Ok((o, p))
}
