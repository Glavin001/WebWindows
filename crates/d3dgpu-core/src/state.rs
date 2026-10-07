//! The Direct3D 9 device state the command stream sets.

use d3dgpu_proto::d3d9::*;
use d3dgpu_proto::{Handle, Rect, Viewport};
use d3dgpu_shader::ConstLayout;

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Stream {
    pub buffer: Handle,
    pub offset: u32,
    pub stride: u32,
    pub freq: u32,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug, Hash)]
pub struct Target {
    pub texture: Handle,
    pub face: u32,
    pub level: u32,
}

/// Shader constants of one stage, kept in the uniform layout the
/// translator declares, with a dirty flag so unchanged constants aren't
/// uploaded again.
pub struct Consts {
    pub layout: ConstLayout,
    pub bytes: Vec<u8>,
    pub dirty: bool,
    /// Ring offset of the last upload, valid in `epoch`.
    pub offset: u64,
    pub epoch: u64,
}

impl Consts {
    pub fn new(layout: ConstLayout) -> Consts {
        Consts { layout, bytes: vec![0; layout.size() as usize], dirty: true, offset: 0, epoch: u64::MAX }
    }

    pub fn set_f(&mut self, start: u32, data: &[u8]) {
        let begin = start as usize * 16;
        let end = (begin + data.len()).min(self.layout.float_count as usize * 16);
        if begin < end {
            self.bytes[begin..end].copy_from_slice(&data[..end - begin]);
            self.dirty = true;
        }
    }

    pub fn set_i(&mut self, start: u32, data: &[u8]) {
        let base = self.layout.int_offset() as usize;
        let begin = base + start as usize * 16;
        let end = (begin + data.len()).min(base + 256);
        if begin < end {
            self.bytes[begin..end].copy_from_slice(&data[..end - begin]);
            self.dirty = true;
        }
    }

    pub fn set_b(&mut self, start: u32, data: &[u8]) {
        let base = self.layout.bool_offset() as usize;
        for (i, v) in data.as_chunks::<4>().0.iter().enumerate() {
            let n = start as usize + i;
            if n < 16 {
                let b = u32::from_le_bytes([v[0], v[1], v[2], v[3]]) != 0;
                self.bytes[base + n * 4..base + n * 4 + 4].copy_from_slice(&(b as u32).to_le_bytes());
                self.dirty = true;
            }
        }
    }
}

pub struct State {
    pub rs: [u32; RENDER_STATE_COUNT],
    pub samp: [[u32; SAMPLER_STATE_COUNT]; MAX_SAMPLERS],
    pub tss: [[u32; TEXTURE_STAGE_STATE_COUNT]; MAX_TEXTURE_STAGES],
    pub textures: [Handle; MAX_SAMPLERS],
    pub streams: [Stream; MAX_STREAMS],
    pub indices: Handle,
    pub index_format: Format,
    pub vs: Handle,
    pub ps: Handle,
    pub decl: Handle,
    pub viewport: Viewport,
    pub scissor: Rect,
    pub rts: [Target; MAX_RENDER_TARGETS],
    pub ds: Target,
    pub vs_consts: Consts,
    pub ps_consts: Consts,
    pub clip_planes: [[f32; 4]; MAX_CLIP_PLANES],
    pub palettes: std::collections::HashMap<u32, [[u8; 4]; 256]>,
}

impl State {
    pub fn new() -> State {
        State {
            rs: default_render_states(),
            samp: [default_sampler_states(); MAX_SAMPLERS],
            tss: std::array::from_fn(|i| default_texture_stage_states(i as u32)),
            textures: [Handle::NONE; MAX_SAMPLERS],
            streams: [Stream::default(); MAX_STREAMS],
            indices: Handle::NONE,
            index_format: Format::Index16,
            vs: Handle::NONE,
            ps: Handle::NONE,
            decl: Handle::NONE,
            viewport: Viewport { x: 0, y: 0, width: 0, height: 0, min_z: 0.0, max_z: 1.0 },
            scissor: Rect::default(),
            rts: [Target::default(); MAX_RENDER_TARGETS],
            ds: Target::default(),
            vs_consts: Consts::new(ConstLayout::VERTEX),
            ps_consts: Consts::new(ConstLayout::PIXEL),
            clip_planes: [[0.0; 4]; MAX_CLIP_PLANES],
            palettes: Default::default(),
        }
    }

    pub fn r(&self, s: RenderState) -> u32 {
        self.rs[s.0 as usize]
    }

    pub fn rf(&self, s: RenderState) -> f32 {
        f32::from_bits(self.rs[s.0 as usize])
    }
}
