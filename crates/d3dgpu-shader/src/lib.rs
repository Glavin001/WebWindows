//! Direct3D 9 shader bytecode (shader models 1.0–3.0) to WGSL.
//!
//! ```
//! use d3dgpu_shader::{asm, ShaderModule, VertexKey};
//! let tokens = asm::assemble("vs_2_0\ndcl_position v0\nm4x4 oPos, v0, c0").unwrap();
//! let module = ShaderModule::new(&tokens).unwrap();
//! let out = module.vertex(&VertexKey::default()).unwrap();
//! assert!(out.wgsl.contains("@vertex"));
//! ```
//!
//! The same translator covers game shaders and Wine's fixed-function
//! pipeline, which wined3d compiles to shader model 2/3 bytecode. Anything
//! the bytecode doesn't decide (vertex attribute types, the bound texture's
//! kind, alpha test, fog, clip planes, the varying layout of the other
//! stage) is part of the variant key ([`VertexKey`], [`PixelKey`]); the
//! render core caches translations by bytecode hash plus key.

pub mod asm;
pub mod bytecode;
pub mod key;
mod parse;
pub mod reflect;
pub mod wgsl;

pub use bytecode::{Opcode, Shader, Stage, Version};
pub use key::*;
pub use parse::encode;
pub use reflect::{ps_linkage, Reflection};
pub use wgsl::{TextureBinding, Translation};

/// Translation errors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// Malformed bytecode at token `offset`.
    Parse { offset: usize, message: String },
    /// Assembly error on `line` (1-based).
    Asm { line: usize, message: String },
    /// Valid bytecode the translator can't handle.
    Unsupported(String),
}

impl Error {
    pub(crate) fn parse(offset: usize, message: impl Into<String>) -> Error {
        Error::Parse { offset, message: message.into() }
    }
    pub(crate) fn asm(line: usize, message: impl Into<String>) -> Error {
        Error::Asm { line, message: message.into() }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Parse { offset, message } => write!(f, "bytecode token {offset}: {message}"),
            Error::Asm { line, message } => write!(f, "line {line}: {message}"),
            Error::Unsupported(m) => write!(f, "unsupported: {m}"),
        }
    }
}

impl std::error::Error for Error {}

/// Parses a token stream.
pub fn parse(tokens: &[u32]) -> Result<Shader, Error> {
    parse::parse(tokens)
}

/// Parses bytecode given as little-endian bytes.
pub fn parse_bytes(bytes: &[u8]) -> Result<Shader, Error> {
    if !bytes.len().is_multiple_of(4) {
        return Err(Error::parse(bytes.len() / 4, "length is not a multiple of 4"));
    }
    let tokens: Vec<u32> =
        bytes.as_chunks::<4>().0.iter().map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
    parse(&tokens)
}

/// A parsed shader and its reflection, ready to translate into variants.
#[derive(Clone, Debug)]
pub struct ShaderModule {
    pub shader: Shader,
    pub reflection: Reflection,
}

impl ShaderModule {
    pub fn new(tokens: &[u32]) -> Result<ShaderModule, Error> {
        Ok(Self::from_shader(parse(tokens)?))
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<ShaderModule, Error> {
        Ok(Self::from_shader(parse_bytes(bytes)?))
    }

    pub fn from_shader(shader: Shader) -> ShaderModule {
        let reflection = Reflection::new(&shader);
        ShaderModule { shader, reflection }
    }

    pub fn stage(&self) -> Stage {
        self.shader.stage
    }

    pub fn vertex(&self, key: &VertexKey) -> Result<Translation, Error> {
        wgsl::translate_vertex(&self.shader, &self.reflection, key)
    }

    pub fn pixel(&self, key: &PixelKey) -> Result<Translation, Error> {
        wgsl::translate_pixel(&self.shader, &self.reflection, key)
    }

    /// The varying layout of a pixel shader variant; pass it as the vertex
    /// shader's [`VertexKey::outputs`].
    pub fn linkage(&self, key: &PixelKey) -> Vec<Varying> {
        ps_linkage(&self.reflection, key)
    }
}
