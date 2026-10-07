//! What the open render pass has set, so draws skip commands that would
//! set the same thing again. In the browser every pass command is a call
//! into JavaScript and a Dawn validation step, so a stream of draws that
//! share most of their state is dominated by these calls otherwise.
//!
//! Objects are identified by ids the core hands out when it creates them
//! (never reused), not by handles, which the front end can recycle.

/// Pass state set so far. Reset when a pass begins and after anything
/// records pass commands behind its back (quad clears).
pub struct PassCache {
    pipeline: u64,
    groups: [(u64, Vec<u32>); 4],
    vbs: [(u64, u64); 8],
    ib: (u64, u64, Option<wgpu::IndexFormat>),
    viewport: [u32; 6],
    scissor: [u32; 4],
    blend: [u64; 4],
    stencil: u32,
}

const UNSET: u64 = u64::MAX;

impl PassCache {
    pub fn new() -> PassCache {
        PassCache {
            pipeline: UNSET,
            groups: std::array::from_fn(|_| (UNSET, Vec::new())),
            vbs: [(UNSET, 0); 8],
            ib: (UNSET, 0, None),
            viewport: [u32::MAX; 6],
            scissor: [u32::MAX; 4],
            blend: [UNSET; 4],
            stencil: u32::MAX,
        }
    }

    pub fn reset(&mut self) {
        *self = PassCache::new();
    }

    pub fn set_pipeline(&mut self, pass: &mut wgpu::RenderPass<'static>, id: u64, p: &wgpu::RenderPipeline) {
        if self.pipeline != id {
            pass.set_pipeline(p);
            self.pipeline = id;
        }
    }

    pub fn set_bind_group(
        &mut self,
        pass: &mut wgpu::RenderPass<'static>,
        index: u32,
        id: u64,
        group: &wgpu::BindGroup,
        offsets: &[u32],
    ) {
        let slot = &mut self.groups[index as usize];
        if slot.0 != id || slot.1 != offsets {
            pass.set_bind_group(index, group, offsets);
            slot.0 = id;
            slot.1.clear();
            slot.1.extend_from_slice(offsets);
        }
    }

    pub fn set_vertex_buffer(
        &mut self,
        pass: &mut wgpu::RenderPass<'static>,
        slot: u32,
        id: u64,
        buffer: &wgpu::Buffer,
        offset: u64,
    ) {
        let s = &mut self.vbs[slot as usize];
        if *s != (id, offset) {
            pass.set_vertex_buffer(slot, buffer.slice(offset..));
            *s = (id, offset);
        }
    }

    pub fn set_index_buffer(
        &mut self,
        pass: &mut wgpu::RenderPass<'static>,
        id: u64,
        buffer: &wgpu::Buffer,
        offset: u64,
        format: wgpu::IndexFormat,
    ) {
        if self.ib != (id, offset, Some(format)) {
            pass.set_index_buffer(buffer.slice(offset..), format);
            self.ib = (id, offset, Some(format));
        }
    }

    pub fn set_viewport(&mut self, pass: &mut wgpu::RenderPass<'static>, v: [f32; 6]) {
        let key = v.map(f32::to_bits);
        if self.viewport != key {
            pass.set_viewport(v[0], v[1], v[2], v[3], v[4], v[5]);
            self.viewport = key;
        }
    }

    pub fn set_scissor(&mut self, pass: &mut wgpu::RenderPass<'static>, r: [u32; 4]) {
        if self.scissor != r {
            pass.set_scissor_rect(r[0], r[1], r[2], r[3]);
            self.scissor = r;
        }
    }

    pub fn set_blend_constant(&mut self, pass: &mut wgpu::RenderPass<'static>, c: wgpu::Color) {
        let key = [c.r, c.g, c.b, c.a].map(f64::to_bits);
        if self.blend != key {
            pass.set_blend_constant(c);
            self.blend = key;
        }
    }

    pub fn set_stencil_reference(&mut self, pass: &mut wgpu::RenderPass<'static>, r: u32) {
        if self.stencil != r {
            pass.set_stencil_reference(r);
            self.stencil = r;
        }
    }
}
