//! Direct3D 10/11 shader bytecode (DXBC, shader model 4.0–5.0) to WGSL.
//!
//! The pipeline is the same shape as the shader model 1–3 translator in
//! `d3dgpu-shader`: decode the token stream, reflect what the shader
//! declares, and emit WGSL statement by statement. SM4/5 control flow is
//! structured (`if`, `loop`, `switch`), so no control-flow graph is
//! needed. Registers are typeless 32-bit values, so every register is a
//! `vec4<u32>` and each instruction bitcasts its sources to the type it
//! operates on (as DXVK does in SPIR-V).
//!
//! What the bytecode doesn't decide (whether a bound texture is a depth
//! texture, the element format of typed buffers and storage textures, the
//! varying layout of the next stage) is the [`Key`].

pub mod container;
pub mod decode;
pub mod disasm;
pub mod reflect;
pub mod wgsl;

pub use container::{ComponentType, Container, SigElement};
pub use decode::{Opcode, Program, ProgramType, RegType, ResourceDim, ReturnType};
pub use reflect::Reflection;
pub use wgsl::{
    Binding, BindingType, BufferFormat, Clip, Interp, Key, LinkElement, LinkSlot, SrvKind, StorageFormat, TexDim,
    TexSample, Translation, UavKind,
};

/// Translation errors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Container(String),
    Decode { offset: usize, message: String },
    Unsupported(String),
}

impl Error {
    pub(crate) fn decode(offset: usize, message: impl Into<String>) -> Error {
        Error::Decode { offset, message: message.into() }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Container(m) => write!(f, "DXBC container: {m}"),
            Error::Decode { offset, message } => write!(f, "token {offset}: {message}"),
            Error::Unsupported(m) => write!(f, "unsupported: {m}"),
        }
    }
}

impl std::error::Error for Error {}

/// A parsed shader: program, signatures and reflection.
#[derive(Clone, Debug)]
pub struct Shader {
    pub program: Program,
    pub reflection: Reflection,
}

impl Shader {
    /// Parses a DXBC container.
    pub fn from_dxbc(bytes: &[u8]) -> Result<Shader, Error> {
        let c = Container::parse(bytes)?;
        let program = decode::decode(&c.program()?)?;
        let reflection = Reflection::new(&program, c.input_signature()?, c.output_signature()?);
        Ok(Shader { program, reflection })
    }

    pub fn ty(&self) -> ProgramType {
        self.program.ty
    }

    /// Translates with a variant key.
    pub fn translate(&self, key: &Key) -> Result<Translation, Error> {
        wgsl::translate(self, key)
    }

    /// The varyings this pixel shader reads, in location order: pass them
    /// as the vertex shader's [`Key::outputs`].
    pub fn linkage(&self) -> Vec<LinkSlot> {
        wgsl::ps_linkage(&self.reflection)
    }

    pub fn disassemble(&self) -> String {
        disasm::disassemble(&self.program)
    }
}
