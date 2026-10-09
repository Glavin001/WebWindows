//! What a shader reads and writes, found without translating it.

use crate::bytecode::*;
use crate::key::*;

/// Facts about a shader the render core needs to build pipelines and bind
/// groups, and to pick variant keys.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Reflection {
    /// Vertex shaders: input register and its declared semantic.
    pub vs_inputs: Vec<(u32, Semantic)>,
    /// Vertex shaders: semantics written (including `POSITION`).
    pub vs_outputs: Vec<Semantic>,
    /// Pixel shaders: the varyings read, in location order.
    pub ps_inputs: Vec<Varying>,
    /// Pixel shaders: reads `vPos` / `vFace`.
    pub uses_vpos: bool,
    pub uses_vface: bool,
    /// Samplers read, with the declared texture type (`Unknown` before
    /// shader model 2, where the bound texture decides).
    pub samplers: Vec<(u32, TextureType)>,
    /// Pixel shaders: `oC#` written (bit per target); bit 0 for 1.x (`r0`).
    pub color_outputs: u8,
    pub writes_depth: bool,
    pub uses_discard: bool,
    /// One past the highest float constant read directly.
    pub float_consts: u32,
    /// Float constants are read with relative addressing (the whole bank
    /// may be read).
    pub relative_consts: bool,
    pub int_consts: u16,
    pub bool_consts: u16,
    /// Instructions the translator approximates or cannot translate.
    pub approximated: Vec<Opcode>,
    pub unsupported: Vec<Opcode>,
}

impl Reflection {
    pub fn new(shader: &Shader) -> Reflection {
        let mut r = Reflection::default();
        let ps = shader.stage == Stage::Pixel;
        let v = shader.version;
        let ps1 = ps && v.major == 1;
        let ps14 = ps && v >= Version::new(1, 4) && v.major == 1;
        let mut ps_colors = 0u8;
        let mut ps_texcoords = 0u8;
        let mut ps_centroid = 0u8;
        let mut ps3_inputs: Vec<(u32, Varying)> = Vec::new();
        let mut vs3_outputs: Vec<(u32, Semantic)> = Vec::new();
        let mut written_out = std::collections::BTreeSet::new();

        let note_src = |r: &mut Reflection, s: &Src, ps_colors: &mut u8, ps_texcoords: &mut u8| match s.reg.ty {
            RegType::Const => {
                if s.rel.is_some() {
                    r.relative_consts = true;
                } else {
                    r.float_consts = r.float_consts.max(s.reg.num + 1);
                }
            }
            RegType::ConstInt => r.int_consts |= 1 << (s.reg.num & 15),
            RegType::ConstBool => r.bool_consts |= 1 << (s.reg.num & 15),
            RegType::Input if ps && v.major < 3 => *ps_colors |= 1 << s.reg.num,
            RegType::Texture if ps && (v.major == 2 || ps14) => *ps_texcoords |= 1 << s.reg.num,
            RegType::MiscType => {
                if s.reg.num == 0 {
                    r.uses_vpos = true
                } else {
                    r.uses_vface = true
                }
            }
            _ => {}
        };

        for ins in &shader.instructions {
            for s in ins.src.iter().chain(ins.predicate.iter()) {
                note_src(&mut r, s, &mut ps_colors, &mut ps_texcoords);
                // matrix ops read src1 .. src1 + rows
                if s.reg.ty == RegType::Const && s.rel.is_none() {
                    let extra = match ins.opcode {
                        Opcode::M4x4 | Opcode::M3x4 => 3,
                        Opcode::M4x3 | Opcode::M3x3 => 2,
                        Opcode::M3x2 => 1,
                        _ => 0,
                    };
                    r.float_consts = r.float_consts.max(s.reg.num + 1 + extra);
                }
            }
            match ins.opcode {
                Opcode::Dcl => {
                    let d = ins.dst.unwrap();
                    match (ins.decl.unwrap(), d.reg.ty) {
                        (Decl::Sampler(t), RegType::Sampler) => {
                            let n = if shader.stage == Stage::Vertex { d.reg.num + 16 } else { d.reg.num };
                            r.samplers.push((n, t));
                        }
                        (Decl::Usage { usage, index }, RegType::Input) if !ps => {
                            r.vs_inputs.push((d.reg.num, Semantic::new(usage, index)));
                        }
                        (Decl::Usage { usage, index }, RegType::Input) if ps && v.major >= 3 => {
                            let interp = if d.centroid { Interp::Centroid } else { Interp::Perspective };
                            ps3_inputs.push((d.reg.num, Varying { semantic: Semantic::new(usage, index), interp }));
                        }
                        (Decl::Usage { .. }, RegType::Input) => {
                            ps_colors |= 1 << d.reg.num;
                            if d.centroid {
                                ps_centroid |= 1 << d.reg.num;
                            }
                        }
                        (Decl::Usage { .. }, RegType::Texture) => {
                            ps_texcoords |= 1 << d.reg.num;
                            if d.centroid {
                                ps_centroid |= 1 << (d.reg.num + 2);
                            }
                        }
                        (Decl::Usage { usage, index }, RegType::Output) => {
                            vs3_outputs.push((d.reg.num, Semantic::new(usage, index)));
                        }
                        (Decl::Usage { .. }, RegType::MiscType) => {
                            if d.reg.num == 0 {
                                r.uses_vpos = true
                            } else {
                                r.uses_vface = true
                            }
                        }
                        _ => {}
                    }
                }
                Opcode::Tex if ps && v.major >= 2 => {
                    if let Some(s) = ins.src.get(1) {
                        add_sampler(&mut r, s.reg.num, TextureType::Unknown);
                    }
                }
                Opcode::TexLdd | Opcode::TexLdl => {
                    if let Some(s) = ins.src.get(1) {
                        let n = if ps { s.reg.num } else { s.reg.num + 16 };
                        add_sampler(&mut r, n, TextureType::Unknown);
                    }
                }
                Opcode::Tex if ps14 => add_sampler(&mut r, ins.dst.unwrap().reg.num, TextureType::Unknown),
                Opcode::Tex
                | Opcode::TexBem
                | Opcode::TexBemL
                | Opcode::TexReg2Ar
                | Opcode::TexReg2Gb
                | Opcode::TexReg2Rgb
                | Opcode::TexM3x2Tex
                | Opcode::TexM3x3Tex
                | Opcode::TexM3x3Spec
                | Opcode::TexM3x3VSpec
                | Opcode::TexDp3Tex
                    if ps1 =>
                {
                    let n = ins.dst.unwrap().reg.num;
                    add_sampler(&mut r, n, TextureType::Unknown);
                    ps_texcoords |= 1 << n;
                }
                Opcode::TexCoord
                | Opcode::TexM3x2Pad
                | Opcode::TexM3x3Pad
                | Opcode::TexM3x2Depth
                | Opcode::TexDp3
                | Opcode::TexM3x3
                | Opcode::TexKill
                    if ps1 && !ps14 =>
                {
                    ps_texcoords |= 1 << ins.dst.unwrap().reg.num;
                }
                Opcode::TexKill => r.uses_discard = true,
                _ => {}
            }
            if matches!(ins.opcode, Opcode::TexKill) {
                r.uses_discard = true;
            }
            match ins.opcode {
                Opcode::TexBem
                | Opcode::TexBemL
                | Opcode::Bem
                | Opcode::TexM3x3Spec
                | Opcode::TexM3x3VSpec
                | Opcode::TexDepth
                | Opcode::TexM3x2Depth => r.approximated.push(ins.opcode),
                _ => {}
            }
            if let Some(d) = &ins.dst {
                if ins.opcode == Opcode::Dcl {
                    continue;
                }
                match d.reg.ty {
                    RegType::ColorOut => r.color_outputs |= 1 << d.reg.num,
                    RegType::DepthOut => r.writes_depth = true,
                    RegType::RastOut => {
                        written_out.insert(match d.reg.num {
                            0 => Semantic::new(usage::POSITION, 0),
                            1 => Semantic::new(usage::FOG, 0),
                            _ => Semantic::new(usage::PSIZE, 0),
                        });
                    }
                    RegType::AttrOut => {
                        written_out.insert(Semantic::new(usage::COLOR, d.reg.num as u8));
                    }
                    RegType::TexCrdOut => {
                        written_out.insert(Semantic::new(usage::TEXCOORD, d.reg.num as u8));
                    }
                    _ => {}
                }
                if ps1 && d.reg.ty == RegType::Temp && d.reg.num == 0 {
                    r.color_outputs |= 1;
                }
            }
        }

        // Direct3D 8 vertex shaders declare no inputs: their declaration
        // binds stream elements to v# by number, each number standing for
        // a fixed semantic (D3DVSDE_*, as Wine's d3d8 gives wined3d).
        if !ps && v.major < 2 && r.vs_inputs.is_empty() {
            let used: std::collections::BTreeSet<u32> = shader
                .instructions
                .iter()
                .flat_map(|ins| ins.src.iter())
                .filter(|s| s.reg.ty == RegType::Input)
                .map(|s| s.reg.num)
                .collect();
            r.vs_inputs = used.into_iter().filter_map(|reg| d3d8_input_semantic(reg).map(|sem| (reg, sem))).collect();
        }

        if ps {
            if v.major >= 3 {
                ps3_inputs.sort_by_key(|(reg, _)| *reg);
                r.ps_inputs = ps3_inputs.into_iter().map(|(_, v)| v).collect();
            } else {
                for i in 0..2 {
                    if ps_colors & (1 << i) != 0 {
                        let interp = if ps_centroid & (1 << i) != 0 { Interp::Centroid } else { Interp::Perspective };
                        r.ps_inputs.push(Varying { semantic: Semantic::new(usage::COLOR, i), interp });
                    }
                }
                for i in 0..8 {
                    if ps_texcoords & (1 << i) != 0 {
                        let interp =
                            if ps_centroid & (1 << (i + 2)) != 0 { Interp::Centroid } else { Interp::Perspective };
                        r.ps_inputs.push(Varying { semantic: Semantic::new(usage::TEXCOORD, i), interp });
                    }
                }
            }
            r.samplers.sort_by_key(|(n, _)| *n);
        } else if v.major >= 3 {
            vs3_outputs.sort_by_key(|(reg, _)| *reg);
            r.vs_outputs = vs3_outputs.into_iter().map(|(_, s)| s).collect();
        } else {
            r.vs_outputs = written_out.into_iter().collect();
        }
        r.samplers.dedup_by_key(|(n, _)| *n);
        r.approximated.sort_by_key(|o| o.code());
        r.approximated.dedup();
        r
    }
}

/// The semantic of Direct3D 8's vertex shader input register `reg`
/// (D3DVSDE_POSITION = 0 ... D3DVSDE_NORMAL2 = 16).
fn d3d8_input_semantic(reg: u32) -> Option<Semantic> {
    Some(match reg {
        0 => Semantic::new(usage::POSITION, 0),
        1 => Semantic::new(usage::BLENDWEIGHT, 0),
        2 => Semantic::new(usage::BLENDINDICES, 0),
        3 => Semantic::new(usage::NORMAL, 0),
        4 => Semantic::new(usage::PSIZE, 0),
        5 => Semantic::new(usage::COLOR, 0),
        6 => Semantic::new(usage::COLOR, 1),
        7..=14 => Semantic::new(usage::TEXCOORD, (reg - 7) as u8),
        15 => Semantic::new(usage::POSITION, 1),
        16 => Semantic::new(usage::NORMAL, 1),
        _ => return None,
    })
}

fn add_sampler(r: &mut Reflection, n: u32, t: TextureType) {
    if !r.samplers.iter().any(|(s, _)| *s == n) {
        r.samplers.push((n, t));
    }
}

/// The varyings a pixel shader variant consumes, in location order: its own
/// inputs, plus `FOG` when vertex fog is applied after the shader. Use this
/// list as [`VertexKey::outputs`] so the two stages agree.
pub fn ps_linkage(refl: &Reflection, key: &PixelKey) -> Vec<Varying> {
    let mut out: Vec<Varying> = refl
        .ps_inputs
        .iter()
        .map(|v| {
            let mut v = *v;
            if key.flat_shading && v.semantic.usage == usage::COLOR {
                v.interp = Interp::Flat;
            }
            v
        })
        .collect();
    let fog = Semantic::new(usage::FOG, 0);
    if key.fog.uses_varying() && !out.iter().any(|v| v.semantic == fog) {
        out.push(Varying { semantic: fog, interp: Interp::Perspective });
    }
    out
}
