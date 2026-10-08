//! The DXBC container: chunks and input/output signatures.

use crate::Error;

/// A parsed DXBC container (borrowing the bytes).
pub struct Container<'a> {
    pub chunks: Vec<([u8; 4], &'a [u8])>,
}

fn u32_at(b: &[u8], off: usize) -> Option<u32> {
    b.get(off..off + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

impl<'a> Container<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Container<'a>, Error> {
        if bytes.len() < 32 || &bytes[0..4] != b"DXBC" {
            return Err(Error::Container("not a DXBC container".into()));
        }
        // 16 bytes of checksum, then version (1) and total size.
        let total = u32_at(bytes, 24).unwrap() as usize;
        if total > bytes.len() {
            return Err(Error::Container(format!("container claims {total} bytes, has {}", bytes.len())));
        }
        let count = u32_at(bytes, 28).unwrap() as usize;
        let mut chunks = Vec::with_capacity(count);
        for i in 0..count {
            let off =
                u32_at(bytes, 32 + i * 4).ok_or_else(|| Error::Container("truncated chunk table".into()))? as usize;
            let fourcc: [u8; 4] = bytes
                .get(off..off + 4)
                .ok_or_else(|| Error::Container("chunk outside the container".into()))?
                .try_into()
                .unwrap();
            let size = u32_at(bytes, off + 4).ok_or_else(|| Error::Container("truncated chunk".into()))? as usize;
            let data =
                bytes.get(off + 8..off + 8 + size).ok_or_else(|| Error::Container("chunk runs past the end".into()))?;
            chunks.push((fourcc, data));
        }
        Ok(Container { chunks })
    }

    pub fn chunk(&self, fourcc: &[u8; 4]) -> Option<&'a [u8]> {
        self.chunks.iter().find(|(f, _)| f == fourcc).map(|(_, d)| *d)
    }

    /// The shader program tokens (`SHEX` or `SHDR`).
    pub fn program(&self) -> Result<Vec<u32>, Error> {
        let data = self
            .chunk(b"SHEX")
            .or_else(|| self.chunk(b"SHDR"))
            .ok_or_else(|| Error::Container("no SHDR/SHEX chunk".into()))?;
        Ok(data.as_chunks::<4>().0.iter().map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect())
    }

    /// Input signature (`ISGN`/`ISG1`).
    pub fn input_signature(&self) -> Result<Vec<SigElement>, Error> {
        match self.chunk(b"ISG1") {
            Some(d) => parse_signature(d, SigLayout::V1),
            None => self.chunk(b"ISGN").map(|d| parse_signature(d, SigLayout::V0)).unwrap_or(Ok(Vec::new())),
        }
    }

    /// Output signature (`OSGN`/`OSG5`/`OSG1`).
    pub fn output_signature(&self) -> Result<Vec<SigElement>, Error> {
        if let Some(d) = self.chunk(b"OSG1") {
            return parse_signature(d, SigLayout::V1);
        }
        if let Some(d) = self.chunk(b"OSG5") {
            return parse_signature(d, SigLayout::V5);
        }
        self.chunk(b"OSGN").map(|d| parse_signature(d, SigLayout::V0)).unwrap_or(Ok(Vec::new()))
    }
}

/// Component types in signatures (`D3D_REGISTER_COMPONENT_TYPE`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ComponentType {
    Unknown,
    Uint,
    Sint,
    Float,
}

/// System values in signatures (`D3D_NAME`).
pub mod sv {
    pub const UNDEFINED: u32 = 0;
    pub const POSITION: u32 = 1;
    pub const CLIP_DISTANCE: u32 = 2;
    pub const CULL_DISTANCE: u32 = 3;
    pub const RENDER_TARGET_ARRAY_INDEX: u32 = 4;
    pub const VIEWPORT_ARRAY_INDEX: u32 = 5;
    pub const VERTEX_ID: u32 = 6;
    pub const PRIMITIVE_ID: u32 = 7;
    pub const INSTANCE_ID: u32 = 8;
    pub const IS_FRONT_FACE: u32 = 9;
    pub const SAMPLE_INDEX: u32 = 10;
    pub const TARGET: u32 = 64;
    pub const DEPTH: u32 = 65;
    pub const COVERAGE: u32 = 66;
    pub const DEPTH_GREATER_EQUAL: u32 = 67;
    pub const DEPTH_LESS_EQUAL: u32 = 68;
    pub const STENCIL_REF: u32 = 69;
}

/// One element of an input or output signature.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SigElement {
    pub name: String,
    pub index: u32,
    pub system_value: u32,
    pub component_type: ComponentType,
    pub register: u32,
    /// Components present.
    pub mask: u8,
    /// Components the shader reads (inputs) or never writes (outputs).
    pub rw_mask: u8,
    pub stream: u32,
}

impl SigElement {
    /// Semantic match as Direct3D does it (names are case-insensitive).
    pub fn same_semantic(&self, name: &str, index: u32) -> bool {
        self.index == index && self.name.eq_ignore_ascii_case(name)
    }
}

#[derive(Clone, Copy)]
enum SigLayout {
    V0,
    V5,
    V1,
}

fn parse_signature(d: &[u8], layout: SigLayout) -> Result<Vec<SigElement>, Error> {
    let bad = || Error::Container("malformed signature".into());
    let count = u32_at(d, 0).ok_or_else(bad)? as usize;
    let (stride, base) = match layout {
        SigLayout::V0 => (24, 0),
        SigLayout::V5 => (28, 4),
        SigLayout::V1 => (32, 4),
    };
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let e = 8 + i * stride;
        let stream = if base == 4 { u32_at(d, e).ok_or_else(bad)? } else { 0 };
        let name_off = u32_at(d, e + base).ok_or_else(bad)? as usize;
        let index = u32_at(d, e + base + 4).ok_or_else(bad)?;
        let system_value = u32_at(d, e + base + 8).ok_or_else(bad)?;
        let ct = u32_at(d, e + base + 12).ok_or_else(bad)?;
        let register = u32_at(d, e + base + 16).ok_or_else(bad)?;
        let mask = *d.get(e + base + 20).ok_or_else(bad)?;
        let rw_mask = *d.get(e + base + 21).ok_or_else(bad)?;
        let name_bytes = d.get(name_off..).ok_or_else(bad)?;
        let end = name_bytes.iter().position(|b| *b == 0).ok_or_else(bad)?;
        let name = String::from_utf8_lossy(&name_bytes[..end]).into_owned();
        let component_type = match ct {
            1 => ComponentType::Uint,
            2 => ComponentType::Sint,
            3 => ComponentType::Float,
            _ => ComponentType::Unknown,
        };
        out.push(SigElement { name, index, system_value, component_type, register, mask, rw_mask, stream });
    }
    Ok(out)
}
