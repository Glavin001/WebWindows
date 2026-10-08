//! The Direct3D shader model 1–3 token stream: opcodes, register types and
//! the parsed instruction form.

/// Shader stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stage {
    Vertex,
    Pixel,
}

/// Shader model version, e.g. 2.0 is `Version { major: 2, minor: 0 }`.
/// `ps_2_x` and `vs_2_x` are encoded as minor 1, `_sw` variants as minor 0xff.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Version {
    pub major: u8,
    pub minor: u8,
}

impl Version {
    pub const fn new(major: u8, minor: u8) -> Version {
        Version { major, minor }
    }
}

macro_rules! opcodes {
    ($($name:ident = $n:expr, $text:expr;)*) => {
        /// Instruction opcodes (`D3DSIO_*`).
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Opcode { $($name,)* }
        impl Opcode {
            pub fn from_u16(v: u16) -> Option<Opcode> {
                match v { $($n => Some(Opcode::$name),)* _ => None }
            }
            pub fn code(self) -> u16 {
                match self { $(Opcode::$name => $n,)* }
            }
            /// Assembly mnemonic (for shader model 2+ spellings: `texld`,
            /// `texcrd` and `tex` share an opcode and are told apart by
            /// version).
            pub fn mnemonic(self) -> &'static str {
                match self { $(Opcode::$name => $text,)* }
            }
            pub const ALL: &'static [Opcode] = &[$(Opcode::$name,)*];
        }
    };
}

opcodes! {
    Nop = 0, "nop";
    Mov = 1, "mov";
    Add = 2, "add";
    Sub = 3, "sub";
    Mad = 4, "mad";
    Mul = 5, "mul";
    Rcp = 6, "rcp";
    Rsq = 7, "rsq";
    Dp3 = 8, "dp3";
    Dp4 = 9, "dp4";
    Min = 10, "min";
    Max = 11, "max";
    Slt = 12, "slt";
    Sge = 13, "sge";
    Exp = 14, "exp";
    Log = 15, "log";
    Lit = 16, "lit";
    Dst = 17, "dst";
    Lrp = 18, "lrp";
    Frc = 19, "frc";
    M4x4 = 20, "m4x4";
    M4x3 = 21, "m4x3";
    M3x4 = 22, "m3x4";
    M3x3 = 23, "m3x3";
    M3x2 = 24, "m3x2";
    Call = 25, "call";
    CallNz = 26, "callnz";
    Loop = 27, "loop";
    Ret = 28, "ret";
    EndLoop = 29, "endloop";
    Label = 30, "label";
    Dcl = 31, "dcl";
    Pow = 32, "pow";
    Crs = 33, "crs";
    Sgn = 34, "sgn";
    Abs = 35, "abs";
    Nrm = 36, "nrm";
    SinCos = 37, "sincos";
    Rep = 38, "rep";
    EndRep = 39, "endrep";
    If = 40, "if";
    IfC = 41, "if";
    Else = 42, "else";
    EndIf = 43, "endif";
    Break = 44, "break";
    BreakC = 45, "break";
    MovA = 46, "mova";
    DefB = 47, "defb";
    DefI = 48, "defi";
    TexCoord = 64, "texcoord";
    TexKill = 65, "texkill";
    Tex = 66, "tex";
    TexBem = 67, "texbem";
    TexBemL = 68, "texbeml";
    TexReg2Ar = 69, "texreg2ar";
    TexReg2Gb = 70, "texreg2gb";
    TexM3x2Pad = 71, "texm3x2pad";
    TexM3x2Tex = 72, "texm3x2tex";
    TexM3x3Pad = 73, "texm3x3pad";
    TexM3x3Tex = 74, "texm3x3tex";
    TexM3x3Spec = 76, "texm3x3spec";
    TexM3x3VSpec = 77, "texm3x3vspec";
    ExpP = 78, "expp";
    LogP = 79, "logp";
    Cnd = 80, "cnd";
    Def = 81, "def";
    TexReg2Rgb = 82, "texreg2rgb";
    TexDp3Tex = 83, "texdp3tex";
    TexM3x2Depth = 84, "texm3x2depth";
    TexDp3 = 85, "texdp3";
    TexM3x3 = 86, "texm3x3";
    TexDepth = 87, "texdepth";
    Cmp = 88, "cmp";
    Bem = 89, "bem";
    Dp2Add = 90, "dp2add";
    Dsx = 91, "dsx";
    Dsy = 92, "dsy";
    TexLdd = 93, "texldd";
    SetP = 94, "setp";
    TexLdl = 95, "texldl";
    BreakP = 96, "breakp";
    Phase = 0xfffd, "phase";
}

/// Register files (`D3DSPR_*`). Some numbers mean different things per
/// stage; the parser resolves them into distinct variants.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RegType {
    /// `r#`
    Temp,
    /// `v#`
    Input,
    /// `c#` (all four constant banks folded into one index space)
    Const,
    /// `a0` (vertex shaders)
    Addr,
    /// `t#` (pixel shaders): texture coordinate inputs (1.x/2.x) or
    /// texture registers (1.x).
    Texture,
    /// `oPos` (0), `oFog` (1), `oPts` (2)
    RastOut,
    /// `oD#`
    AttrOut,
    /// `oT#` (vertex shaders before 3.0)
    TexCrdOut,
    /// `o#` (vertex shader 3.0)
    Output,
    /// `i#`
    ConstInt,
    /// `oC#`
    ColorOut,
    /// `oDepth`
    DepthOut,
    /// `s#`
    Sampler,
    /// `b#`
    ConstBool,
    /// `aL`
    Loop,
    /// `vPos` (0), `vFace` (1)
    MiscType,
    /// `l#`
    Label,
    /// `p0`
    Predicate,
}

impl RegType {
    /// Decodes the 5-bit register type field for a stage and version.
    pub fn decode(raw: u32, stage: Stage, version: Version) -> Option<RegType> {
        Some(match raw {
            0 => RegType::Temp,
            1 => RegType::Input,
            2 | 11 | 12 | 13 => RegType::Const,
            3 => match stage {
                Stage::Vertex => RegType::Addr,
                Stage::Pixel => RegType::Texture,
            },
            4 => RegType::RastOut,
            5 => RegType::AttrOut,
            6 => {
                if stage == Stage::Vertex && version.major >= 3 {
                    RegType::Output
                } else {
                    RegType::TexCrdOut
                }
            }
            7 => RegType::ConstInt,
            8 => RegType::ColorOut,
            9 => RegType::DepthOut,
            10 => RegType::Sampler,
            14 => RegType::ConstBool,
            15 => RegType::Loop,
            16 => RegType::Temp, // TEMPFLOAT16: half-precision temps, treated as full
            17 => RegType::MiscType,
            18 => RegType::Label,
            19 => RegType::Predicate,
            _ => return None,
        })
    }

    /// The raw type field and register-number bias for encoding.
    pub fn encode(self) -> u32 {
        match self {
            RegType::Temp => 0,
            RegType::Input => 1,
            RegType::Const => 2,
            RegType::Addr | RegType::Texture => 3,
            RegType::RastOut => 4,
            RegType::AttrOut => 5,
            RegType::TexCrdOut | RegType::Output => 6,
            RegType::ConstInt => 7,
            RegType::ColorOut => 8,
            RegType::DepthOut => 9,
            RegType::Sampler => 10,
            RegType::ConstBool => 14,
            RegType::Loop => 15,
            RegType::MiscType => 17,
            RegType::Label => 18,
            RegType::Predicate => 19,
        }
    }
}

/// A register reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Reg {
    pub ty: RegType,
    pub num: u32,
}

/// Relative addressing: `c[a0.x + n]` or `o[aL + n]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RelAddr {
    /// `a0` or `aL`.
    pub reg: Reg,
    /// Component of the address register (0..4).
    pub component: u8,
}

/// Source modifiers (`D3DSPSM_*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SrcMod {
    None,
    Neg,
    Bias,
    BiasNeg,
    Sign,
    SignNeg,
    Comp,
    X2,
    X2Neg,
    Dz,
    Dw,
    Abs,
    AbsNeg,
    Not,
}

impl SrcMod {
    pub fn decode(v: u32) -> Option<SrcMod> {
        Some(match v {
            0 => SrcMod::None,
            1 => SrcMod::Neg,
            2 => SrcMod::Bias,
            3 => SrcMod::BiasNeg,
            4 => SrcMod::Sign,
            5 => SrcMod::SignNeg,
            6 => SrcMod::Comp,
            7 => SrcMod::X2,
            8 => SrcMod::X2Neg,
            9 => SrcMod::Dz,
            10 => SrcMod::Dw,
            11 => SrcMod::Abs,
            12 => SrcMod::AbsNeg,
            13 => SrcMod::Not,
            _ => return None,
        })
    }
    pub fn encode(self) -> u32 {
        self as u32
    }
}

/// A source operand.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Src {
    pub reg: Reg,
    /// Component selected for each of x, y, z, w (0..4).
    pub swizzle: [u8; 4],
    pub modifier: SrcMod,
    pub rel: Option<RelAddr>,
}

impl Src {
    pub const IDENTITY: [u8; 4] = [0, 1, 2, 3];
    pub fn new(reg: Reg) -> Src {
        Src { reg, swizzle: Src::IDENTITY, modifier: SrcMod::None, rel: None }
    }
}

/// Write mask bits.
pub const MASK_X: u8 = 1;
pub const MASK_Y: u8 = 2;
pub const MASK_Z: u8 = 4;
pub const MASK_W: u8 = 8;
pub const MASK_ALL: u8 = 15;

/// A destination operand.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Dst {
    pub reg: Reg,
    pub mask: u8,
    pub saturate: bool,
    pub partial_precision: bool,
    pub centroid: bool,
    /// Result shift for pixel shader 1.x: 1 = x2, 2 = x4, 3 = x8, -1 = d2,
    /// -2 = d4, -3 = d8.
    pub shift: i8,
    /// Output relative addressing (`o[aL]` in vs_3_0).
    pub rel: Option<RelAddr>,
}

impl Dst {
    pub fn new(reg: Reg, mask: u8) -> Dst {
        Dst { reg, mask, saturate: false, partial_precision: false, centroid: false, shift: 0, rel: None }
    }
}

/// Texture types in sampler declarations (`D3DSTT_*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextureType {
    Unknown,
    D2,
    Cube,
    Volume,
}

impl TextureType {
    pub fn decode(v: u32) -> TextureType {
        match v {
            2 => TextureType::D2,
            3 => TextureType::Cube,
            4 => TextureType::Volume,
            _ => TextureType::Unknown,
        }
    }
    pub fn encode(self) -> u32 {
        match self {
            TextureType::Unknown => 0,
            TextureType::D2 => 2,
            TextureType::Cube => 3,
            TextureType::Volume => 4,
        }
    }
}

/// The usage half of a `dcl` instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Decl {
    /// `dcl_texcoord1 v3` and friends: usage (`D3DDECLUSAGE`) and index.
    Usage { usage: u8, index: u8 },
    /// `dcl_2d s0` and friends.
    Sampler(TextureType),
}

/// Comparison in `ifc`, `breakc` and `setp` (`D3DSPC_*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Comparison {
    Gt = 1,
    Eq = 2,
    Ge = 3,
    Lt = 4,
    Ne = 5,
    Le = 6,
}

impl Comparison {
    pub fn decode(v: u32) -> Option<Comparison> {
        Some(match v {
            1 => Comparison::Gt,
            2 => Comparison::Eq,
            3 => Comparison::Ge,
            4 => Comparison::Lt,
            5 => Comparison::Ne,
            6 => Comparison::Le,
            _ => return None,
        })
    }
    pub fn suffix(self) -> &'static str {
        match self {
            Comparison::Gt => "gt",
            Comparison::Eq => "eq",
            Comparison::Ge => "ge",
            Comparison::Lt => "lt",
            Comparison::Ne => "ne",
            Comparison::Le => "le",
        }
    }
}

/// `texld` variants in shader model 2+ (`D3DSI_TEXLD_*` in the control bits).
pub const TEXLD_PROJECT: u8 = 1;
pub const TEXLD_BIAS: u8 = 2;

/// One parsed instruction.
#[derive(Clone, Debug, PartialEq)]
pub struct Instruction {
    pub opcode: Opcode,
    /// Opcode-specific control bits (comparison, `texld` variant).
    pub controls: u8,
    pub coissue: bool,
    pub dst: Option<Dst>,
    /// Predicate source of a predicated instruction (`(p0.x) add ...`).
    pub predicate: Option<Src>,
    pub src: Vec<Src>,
    pub decl: Option<Decl>,
    /// Immediate values of `def` (4 floats as bits), `defi` (4 ints) and
    /// `defb` (1 bool).
    pub imm: Vec<u32>,
}

impl Instruction {
    pub fn comparison(&self) -> Option<Comparison> {
        Comparison::decode(self.controls as u32)
    }
}

/// A parsed shader.
#[derive(Clone, Debug, PartialEq)]
pub struct Shader {
    pub stage: Stage,
    pub version: Version,
    pub instructions: Vec<Instruction>,
}

/// D3DDECLUSAGE values used by the translator.
pub mod usage {
    pub const POSITION: u8 = 0;
    pub const BLENDWEIGHT: u8 = 1;
    pub const BLENDINDICES: u8 = 2;
    pub const NORMAL: u8 = 3;
    pub const PSIZE: u8 = 4;
    pub const TEXCOORD: u8 = 5;
    pub const TANGENT: u8 = 6;
    pub const BINORMAL: u8 = 7;
    pub const TESSFACTOR: u8 = 8;
    pub const POSITIONT: u8 = 9;
    pub const COLOR: u8 = 10;
    pub const FOG: u8 = 11;
    pub const DEPTH: u8 = 12;
    pub const SAMPLE: u8 = 13;

    pub const NAMES: [&str; 14] = [
        "position",
        "blendweight",
        "blendindices",
        "normal",
        "psize",
        "texcoord",
        "tangent",
        "binormal",
        "tessfactor",
        "positiont",
        "color",
        "fog",
        "depth",
        "sample",
    ];
}
