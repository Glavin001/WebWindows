//! Layer 1 — Load: a PE32 parser and image loader.
//!
//! Parses the parts of a 32-bit PE file (.exe or .dll) that the translator and
//! runtime need: sections, imports, exports, base relocations and TLS callbacks.
//! [`PeFile::load_image`] lays the file out as it would appear in memory at its
//! preferred base (or at another base, applying relocations).

use anyhow::{anyhow, bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};

pub const IMAGE_SCN_CNT_CODE: u32 = 0x0000_0020;
pub const IMAGE_SCN_MEM_EXECUTE: u32 = 0x2000_0000;
pub const IMAGE_SCN_MEM_READ: u32 = 0x4000_0000;
pub const IMAGE_SCN_MEM_WRITE: u32 = 0x8000_0000;

pub const IMAGE_FILE_DLL: u16 = 0x2000;
pub const IMAGE_FILE_RELOCS_STRIPPED: u16 = 0x0001;

const DIR_EXPORT: usize = 0;
const DIR_IMPORT: usize = 1;
const DIR_BASERELOC: usize = 5;
const DIR_TLS: usize = 9;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Section {
    pub name: String,
    pub virtual_address: u32,
    pub virtual_size: u32,
    pub raw_offset: u32,
    pub raw_size: u32,
    pub characteristics: u32,
}

impl Section {
    pub fn is_executable(&self) -> bool {
        self.characteristics & (IMAGE_SCN_MEM_EXECUTE | IMAGE_SCN_CNT_CODE) != 0
    }
    pub fn is_writable(&self) -> bool {
        self.characteristics & IMAGE_SCN_MEM_WRITE != 0
    }
    /// Size the section occupies in memory.
    pub fn mem_size(&self) -> u32 {
        if self.virtual_size == 0 {
            self.raw_size
        } else {
            self.virtual_size
        }
    }
    pub fn contains_rva(&self, rva: u32) -> bool {
        rva >= self.virtual_address && rva < self.virtual_address + self.mem_size()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImportName {
    Name { hint: u16, name: String },
    Ordinal(u16),
}

impl std::fmt::Display for ImportName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImportName::Name { name, .. } => f.write_str(name),
            ImportName::Ordinal(o) => write!(f, "#{o}"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Import {
    pub dll: String,
    pub name: ImportName,
    /// RVA of the IAT slot the loader fills with the function's address.
    pub iat_rva: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ExportTarget {
    Rva(u32),
    Forwarder(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Export {
    pub ordinal: u32,
    pub name: Option<String>,
    pub target: ExportTarget,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TlsInfo {
    pub raw_data_start: u32,
    pub raw_data_end: u32,
    pub index_address: u32,
    pub callbacks_address: u32,
    pub zero_fill: u32,
    /// Callback virtual addresses (relative to the preferred image base).
    pub callbacks: Vec<u32>,
}

/// A parsed PE32 file.
#[derive(Debug, Clone)]
pub struct PeFile {
    pub data: Vec<u8>,
    pub machine: u16,
    pub characteristics: u16,
    pub image_base: u32,
    pub entry_rva: u32,
    pub size_of_image: u32,
    pub size_of_headers: u32,
    pub section_alignment: u32,
    pub subsystem: u16,
    pub dll_characteristics: u16,
    pub stack_reserve: u32,
    pub stack_commit: u32,
    pub heap_reserve: u32,
    pub heap_commit: u32,
    pub sections: Vec<Section>,
    pub imports: Vec<Import>,
    pub exports: Vec<Export>,
    pub dll_name: Option<String>,
    /// RVAs of 32-bit absolute (HIGHLOW) relocation sites. `None` when the
    /// file has no relocation directory (typical for old game executables).
    pub relocations: Option<Vec<u32>>,
    pub tls: Option<TlsInfo>,
}

fn rd_u16(d: &[u8], off: usize) -> Result<u16> {
    d.get(off..off + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .ok_or_else(|| anyhow!("read u16 out of bounds at {off:#x}"))
}

fn rd_u32(d: &[u8], off: usize) -> Result<u32> {
    d.get(off..off + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or_else(|| anyhow!("read u32 out of bounds at {off:#x}"))
}

impl PeFile {
    pub fn parse(data: Vec<u8>) -> Result<PeFile> {
        ensure!(data.len() >= 0x40, "file too small for a DOS header");
        ensure!(&data[0..2] == b"MZ", "missing MZ signature");
        let pe_off = rd_u32(&data, 0x3c)? as usize;
        ensure!(
            data.get(pe_off..pe_off + 4) == Some(b"PE\0\0"),
            "missing PE signature"
        );
        let coff = pe_off + 4;
        let machine = rd_u16(&data, coff)?;
        ensure!(
            machine == 0x14c,
            "not an i386 PE file (machine {machine:#x})"
        );
        let num_sections = rd_u16(&data, coff + 2)? as usize;
        let opt_size = rd_u16(&data, coff + 16)? as usize;
        let characteristics = rd_u16(&data, coff + 18)?;
        let opt = coff + 20;
        let magic = rd_u16(&data, opt)?;
        ensure!(
            magic == 0x10b,
            "not a PE32 optional header (magic {magic:#x})"
        );

        let entry_rva = rd_u32(&data, opt + 16)?;
        let image_base = rd_u32(&data, opt + 28)?;
        let section_alignment = rd_u32(&data, opt + 32)?;
        let size_of_image = rd_u32(&data, opt + 56)?;
        let size_of_headers = rd_u32(&data, opt + 60)?;
        let subsystem = rd_u16(&data, opt + 68)?;
        let dll_characteristics = rd_u16(&data, opt + 70)?;
        let stack_reserve = rd_u32(&data, opt + 72)?;
        let stack_commit = rd_u32(&data, opt + 76)?;
        let heap_reserve = rd_u32(&data, opt + 80)?;
        let heap_commit = rd_u32(&data, opt + 84)?;
        let num_dirs = rd_u32(&data, opt + 92)? as usize;
        let mut dirs = [(0u32, 0u32); 16];
        for (i, dir) in dirs.iter_mut().enumerate().take(num_dirs.min(16)) {
            let o = opt + 96 + i * 8;
            *dir = (rd_u32(&data, o)?, rd_u32(&data, o + 4)?);
        }

        let mut sections = Vec::with_capacity(num_sections);
        let sec_table = opt + opt_size;
        for i in 0..num_sections {
            let s = sec_table + i * 40;
            let raw_name = data
                .get(s..s + 8)
                .ok_or_else(|| anyhow!("section table out of bounds"))?;
            let name = String::from_utf8_lossy(raw_name)
                .trim_end_matches('\0')
                .to_string();
            sections.push(Section {
                name,
                virtual_size: rd_u32(&data, s + 8)?,
                virtual_address: rd_u32(&data, s + 12)?,
                raw_size: rd_u32(&data, s + 16)?,
                raw_offset: rd_u32(&data, s + 20)?,
                characteristics: rd_u32(&data, s + 36)?,
            });
        }

        let mut pe = PeFile {
            data,
            machine,
            characteristics,
            image_base,
            entry_rva,
            size_of_image,
            size_of_headers,
            section_alignment,
            subsystem,
            dll_characteristics,
            stack_reserve,
            stack_commit,
            heap_reserve,
            heap_commit,
            sections,
            imports: vec![],
            exports: vec![],
            dll_name: None,
            relocations: None,
            tls: None,
        };
        if dirs[DIR_IMPORT].0 != 0 {
            pe.imports = pe.parse_imports(dirs[DIR_IMPORT].0).context("imports")?;
        }
        if dirs[DIR_EXPORT].0 != 0 {
            pe.parse_exports(dirs[DIR_EXPORT].0, dirs[DIR_EXPORT].1)
                .context("exports")?;
        }
        if dirs[DIR_BASERELOC].0 != 0 && characteristics & IMAGE_FILE_RELOCS_STRIPPED == 0 {
            pe.relocations = Some(
                pe.parse_relocs(dirs[DIR_BASERELOC].0, dirs[DIR_BASERELOC].1)
                    .context("relocations")?,
            );
        }
        if dirs[DIR_TLS].0 != 0 {
            pe.tls = Some(pe.parse_tls(dirs[DIR_TLS].0).context("tls")?);
        }
        Ok(pe)
    }

    pub fn is_dll(&self) -> bool {
        self.characteristics & IMAGE_FILE_DLL != 0
    }

    /// Converts an RVA to a file offset.
    pub fn rva_to_offset(&self, rva: u32) -> Option<usize> {
        if rva < self.size_of_headers {
            return Some(rva as usize);
        }
        for s in &self.sections {
            if rva >= s.virtual_address && rva < s.virtual_address + s.raw_size.max(1) {
                let off = s.raw_offset as usize + (rva - s.virtual_address) as usize;
                return (off < self.data.len()).then_some(off);
            }
        }
        None
    }

    fn u32_at_rva(&self, rva: u32) -> Result<u32> {
        let off = self
            .rva_to_offset(rva)
            .ok_or_else(|| anyhow!("rva {rva:#x} not in file"))?;
        rd_u32(&self.data, off)
    }

    fn u16_at_rva(&self, rva: u32) -> Result<u16> {
        let off = self
            .rva_to_offset(rva)
            .ok_or_else(|| anyhow!("rva {rva:#x} not in file"))?;
        rd_u16(&self.data, off)
    }

    fn cstr_at_rva(&self, rva: u32) -> Result<String> {
        let off = self
            .rva_to_offset(rva)
            .ok_or_else(|| anyhow!("rva {rva:#x} not in file"))?;
        let end = self.data[off..]
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| anyhow!("unterminated string"))?;
        Ok(String::from_utf8_lossy(&self.data[off..off + end]).into_owned())
    }

    fn parse_imports(&self, dir_rva: u32) -> Result<Vec<Import>> {
        let mut out = vec![];
        let mut desc = dir_rva;
        loop {
            let ilt = self.u32_at_rva(desc)?;
            let name_rva = self.u32_at_rva(desc + 12)?;
            let iat = self.u32_at_rva(desc + 16)?;
            if name_rva == 0 && iat == 0 {
                break;
            }
            let dll = self.cstr_at_rva(name_rva)?;
            // Prefer the lookup table; old Borland binaries leave it zero.
            let lookup = if ilt != 0 { ilt } else { iat };
            let mut i = 0;
            loop {
                let entry = self.u32_at_rva(lookup + i * 4)?;
                if entry == 0 {
                    break;
                }
                let name = if entry & 0x8000_0000 != 0 {
                    ImportName::Ordinal((entry & 0xffff) as u16)
                } else {
                    ImportName::Name {
                        hint: self.u16_at_rva(entry)?,
                        name: self.cstr_at_rva(entry + 2)?,
                    }
                };
                out.push(Import {
                    dll: dll.clone(),
                    name,
                    iat_rva: iat + i * 4,
                });
                i += 1;
            }
            desc += 20;
        }
        Ok(out)
    }

    fn parse_exports(&mut self, dir_rva: u32, dir_size: u32) -> Result<()> {
        let name_rva = self.u32_at_rva(dir_rva + 12)?;
        self.dll_name = self.cstr_at_rva(name_rva).ok();
        let base = self.u32_at_rva(dir_rva + 16)?;
        let n_funcs = self.u32_at_rva(dir_rva + 20)?;
        let n_names = self.u32_at_rva(dir_rva + 24)?;
        let funcs = self.u32_at_rva(dir_rva + 28)?;
        let names = self.u32_at_rva(dir_rva + 32)?;
        let ords = self.u32_at_rva(dir_rva + 36)?;
        ensure!(n_funcs < 0x10000, "implausible export count");
        let mut names_by_index: Vec<Option<String>> = vec![None; n_funcs as usize];
        for i in 0..n_names {
            let idx = self.u16_at_rva(ords + i * 2)? as usize;
            let nm = self.cstr_at_rva(self.u32_at_rva(names + i * 4)?)?;
            if let Some(slot) = names_by_index.get_mut(idx) {
                *slot = Some(nm);
            }
        }
        for (i, name) in names_by_index.into_iter().enumerate() {
            let rva = self.u32_at_rva(funcs + i as u32 * 4)?;
            if rva == 0 {
                continue;
            }
            let target = if rva >= dir_rva && rva < dir_rva + dir_size {
                ExportTarget::Forwarder(self.cstr_at_rva(rva)?)
            } else {
                ExportTarget::Rva(rva)
            };
            self.exports.push(Export {
                ordinal: base + i as u32,
                name,
                target,
            });
        }
        Ok(())
    }

    fn parse_relocs(&self, dir_rva: u32, dir_size: u32) -> Result<Vec<u32>> {
        let mut out = vec![];
        let mut off = 0;
        while off + 8 <= dir_size {
            let page = self.u32_at_rva(dir_rva + off)?;
            let size = self.u32_at_rva(dir_rva + off + 4)?;
            if size < 8 {
                break;
            }
            for i in 0..(size - 8) / 2 {
                let e = self.u16_at_rva(dir_rva + off + 8 + i * 2)?;
                match e >> 12 {
                    0 => {}
                    3 => out.push(page + (e & 0xfff) as u32),
                    t => bail!("unsupported relocation type {t}"),
                }
            }
            off += size;
        }
        Ok(out)
    }

    fn parse_tls(&self, dir_rva: u32) -> Result<TlsInfo> {
        let mut tls = TlsInfo {
            raw_data_start: self.u32_at_rva(dir_rva)?,
            raw_data_end: self.u32_at_rva(dir_rva + 4)?,
            index_address: self.u32_at_rva(dir_rva + 8)?,
            callbacks_address: self.u32_at_rva(dir_rva + 12)?,
            zero_fill: self.u32_at_rva(dir_rva + 16)?,
            callbacks: vec![],
        };
        if tls.callbacks_address != 0 {
            let mut rva = tls.callbacks_address.wrapping_sub(self.image_base);
            loop {
                let cb = self.u32_at_rva(rva)?;
                if cb == 0 {
                    break;
                }
                tls.callbacks.push(cb);
                rva += 4;
            }
        }
        Ok(tls)
    }

    pub fn section_for_rva(&self, rva: u32) -> Option<&Section> {
        self.sections.iter().find(|s| s.contains_rva(rva))
    }

    pub fn entry_point(&self) -> Option<u32> {
        (self.entry_rva != 0).then_some(self.image_base + self.entry_rva)
    }

    /// Lays the file out in memory as the Windows loader would, without
    /// resolving imports. Returns `size_of_image` bytes for loading at `base`;
    /// relocations are applied when `base` differs from the preferred base.
    pub fn load_image(&self, base: u32) -> Result<Vec<u8>> {
        let mut img = vec![0u8; self.size_of_image as usize];
        let hdr = (self.size_of_headers as usize)
            .min(self.data.len())
            .min(img.len());
        img[..hdr].copy_from_slice(&self.data[..hdr]);
        for s in &self.sections {
            let n = s.raw_size.min(s.mem_size()) as usize;
            let src = s.raw_offset as usize;
            let dst = s.virtual_address as usize;
            let n = n
                .min(self.data.len().saturating_sub(src))
                .min(img.len().saturating_sub(dst));
            img[dst..dst + n].copy_from_slice(&self.data[src..src + n]);
        }
        if base != self.image_base {
            let relocs = self
                .relocations
                .as_ref()
                .ok_or_else(|| anyhow!("image cannot be rebased: no relocations"))?;
            let delta = base.wrapping_sub(self.image_base);
            for &r in relocs {
                let r = r as usize;
                let v = u32::from_le_bytes(img[r..r + 4].try_into().unwrap());
                img[r..r + 4].copy_from_slice(&v.wrapping_add(delta).to_le_bytes());
            }
        }
        Ok(img)
    }

    /// Bytes of the image as laid out at the preferred base, for analysis.
    pub fn image(&self) -> Result<Image> {
        Ok(Image {
            base: self.image_base,
            bytes: self.load_image(self.image_base)?,
            sections: self.sections.clone(),
        })
    }
}

/// An image laid out in memory, used by code discovery and decoding.
#[derive(Debug, Clone)]
pub struct Image {
    pub base: u32,
    pub bytes: Vec<u8>,
    pub sections: Vec<Section>,
}

impl Image {
    pub fn contains(&self, va: u32) -> bool {
        va >= self.base && ((va - self.base) as usize) < self.bytes.len()
    }
    pub fn is_code(&self, va: u32) -> bool {
        if !self.contains(va) {
            return false;
        }
        let rva = va - self.base;
        self.sections
            .iter()
            .any(|s| s.is_executable() && s.contains_rva(rva))
    }
    /// Bytes from `va` to the end of its section (or image).
    pub fn bytes_from(&self, va: u32) -> &[u8] {
        if !self.contains(va) {
            return &[];
        }
        &self.bytes[(va - self.base) as usize..]
    }
    pub fn read_u32(&self, va: u32) -> Option<u32> {
        let b = self.bytes_from(va);
        (b.len() >= 4).then(|| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    pub fn section_of(&self, va: u32) -> Option<&Section> {
        if !self.contains(va) {
            return None;
        }
        let rva = va - self.base;
        self.sections.iter().find(|s| s.contains_rva(rva))
    }
}
