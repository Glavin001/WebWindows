//! Direct3D 11 scenes. Shaders are HLSL in `hlsl/d3d11.hlsl`, compiled to
//! DXBC fixtures (see that file). Coordinates assume 64 x 64. Direct3D 10+
//! puts pixel centres at .5, like WebGPU, so a pixel (x, y) is at
//! NDC (x + 0.5) / 32 - 1, 1 - (y + 0.5) / 32.

use d3dgpu_proto::d3d11::*;

use crate::*;

macro_rules! dxbc {
    ($name:literal) => {
        include_bytes!(concat!("../fixtures/d3d11.", $name, ".dxbc")).as_slice()
    };
}

const R: [u8; 4] = [255, 0, 0, 255];
const G: [u8; 4] = [0, 255, 0, 255];
const B: [u8; 4] = [0, 0, 255, 255];
const W: [u8; 4] = [255, 255, 255, 255];
const K: [u8; 4] = [0, 0, 0, 255];
const Y: [u8; 4] = [255, 255, 0, 255];

const RED: u32 = argb(255, 0, 0, 255);
const GREEN: u32 = argb(0, 255, 0, 255);
const BLUE: u32 = argb(0, 0, 255, 255);
const WHITE: u32 = argb(255, 255, 255, 255);
const YELLOW: u32 = argb(255, 255, 0, 255);

const TRIANGLES: u32 = 4;
const BLACK4: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

/// Position (float3) and a `D3DCOLOR` read as `B8G8R8A8_UNORM` (its bytes
/// are B, G, R, A), matching [`pc`] vertices.
fn layout_pc() -> Vec<InputElement> {
    vec![
        InputElement {
            semantic: "POSITION".into(),
            semantic_index: 0,
            format: DxgiFormat::R32G32B32Float,
            slot: 0,
            offset: 0,
            per_instance: false,
            step_rate: 0,
        },
        InputElement {
            semantic: "COLOR".into(),
            semantic_index: 0,
            format: DxgiFormat::B8G8R8A8Unorm,
            slot: 0,
            offset: APPEND_ALIGNED_ELEMENT,
            per_instance: false,
            step_rate: 0,
        },
    ]
}

fn el(semantic: &str, format: DxgiFormat, slot: u32, offset: u32, per_instance: bool) -> InputElement {
    InputElement {
        semantic: semantic.into(),
        semantic_index: 0,
        format,
        slot,
        offset,
        per_instance,
        step_rate: per_instance as u32,
    }
}

fn floats(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ *b as u64).wrapping_mul(0x100_0000_01b3))
}

/// A whole-resource view description.
pub fn view_desc(format: DxgiFormat, dim: ViewDim) -> ViewDesc {
    ViewDesc {
        format,
        dim,
        first_mip: 0,
        mip_count: u32::MAX,
        first_slice: 0,
        slice_count: u32::MAX,
        first_element: 0,
        num_elements: 0,
        flags: 0,
    }
}

impl Builder {
    pub fn new_d3d11(width: u32, height: u32) -> Builder {
        let mut b = Builder::empty(width, height);
        let bind_rt = bind::RENDER_TARGET | bind::SHADER_RESOURCE;
        b.backbuffer = b.texture11(&Texture11Desc::d2(DxgiFormat::R8G8B8A8Unorm, width, height, 1, bind_rt));
        b.rtv = b.view(ViewKind::RenderTarget, b.backbuffer, DxgiFormat::R8G8B8A8Unorm);
        b.depth = b.texture11(&Texture11Desc::d2(DxgiFormat::D24UnormS8Uint, width, height, 1, bind::DEPTH_STENCIL));
        b.dsv = b.view(ViewKind::DepthStencil, b.depth, DxgiFormat::D24UnormS8Uint);
        b.w.set_render_targets11(&[b.rtv], b.dsv);
        b.full_viewport();
        b.w.clear_depth_stencil_view(b.dsv, clear11::DEPTH | clear11::STENCIL, 1.0, 0);
        b.w.set_primitive_topology(TRIANGLES);
        b
    }

    pub fn full_viewport(&mut self) {
        let (w, h) = (self.width as f32, self.height as f32);
        self.w.set_viewports(&[Viewport11 { x: 0.0, y: 0.0, width: w, height: h, min_depth: 0.0, max_depth: 1.0 }]);
    }

    pub fn texture11(&mut self, desc: &Texture11Desc) -> Handle {
        let h = self.handle();
        self.w.create_texture11(h, desc);
        h
    }

    /// A 2D view of a texture's first slice (all mips for shader resources).
    pub fn view(&mut self, kind: ViewKind, resource: Handle, format: DxgiFormat) -> Handle {
        self.view_with(kind, resource, &view_desc(format, ViewDim::Texture2D))
    }

    pub fn view_with(&mut self, kind: ViewKind, resource: Handle, desc: &ViewDesc) -> Handle {
        let h = self.handle();
        self.w.create_view(h, kind, resource, desc);
        h
    }

    pub fn shader11(&mut self, stage: Stage11, dxbc: &[u8]) -> Handle {
        let h = self.handle();
        self.w.create_shader11(h, stage, fnv(dxbc), dxbc);
        h
    }

    /// Creates and binds a vertex and pixel shader.
    pub fn shaders11(&mut self, vs: &[u8], ps: &[u8]) {
        let (v, p) = (self.shader11(Stage11::Vertex, vs), self.shader11(Stage11::Pixel, ps));
        self.w.set_shader11(Stage11::Vertex, v);
        self.w.set_shader11(Stage11::Pixel, p);
    }

    pub fn buffer11(&mut self, bind_flags: u32, data: &[u8]) -> Handle {
        let h = self.handle();
        self.w.create_buffer11(h, data.len() as u32, bind_flags, 0, 0);
        self.w.update_subresource(h, 0, None, 0, 0, data);
        h
    }

    /// Creates and binds an input layout.
    pub fn input_layout(&mut self, elements: &[InputElement]) -> Handle {
        let h = self.handle();
        self.w.create_input_layout(h, elements);
        self.w.set_input_layout(h);
        h
    }

    /// Binds `buffer` as vertex buffer `slot`.
    pub fn vb(&mut self, slot: u32, buffer: Handle, stride: u32) {
        self.w.set_vertex_buffers(slot, &[VertexBufferBinding { buffer, stride, offset: 0 }]);
    }

    /// Uploads `verts` (position + colour) and draws them as a list.
    pub fn draw_pc(&mut self, verts: &[u8]) {
        let vb = self.buffer11(bind::VERTEX_BUFFER, verts);
        self.vb(0, vb, 16);
        self.w.draw11(verts.len() as u32 / 16, 0, 1, 0);
    }

    /// A 2D texture with one level of tightly packed `data`, and its view.
    pub fn texture_srv(&mut self, format: DxgiFormat, width: u32, height: u32, data: &[u8]) -> (Handle, Handle) {
        let t = self.texture11(&Texture11Desc::d2(format, width, height, 1, bind::SHADER_RESOURCE));
        let pitch = format.row_bytes(width).unwrap();
        self.w.update_subresource(t, 0, None, pitch, 0, data);
        let v = self.view(ViewKind::ShaderResource, t, format);
        (t, v)
    }

    /// A constant buffer holding `data`.
    pub fn cbuffer(&mut self, data: &[f32]) -> Handle {
        self.buffer11(bind::CONSTANT_BUFFER, &floats(data))
    }

    pub fn set_cb(&mut self, stage: Stage11, slot: u32, buffer: Handle) {
        self.w.set_constant_buffers(
            stage,
            slot,
            &[ConstantBufferBinding { buffer, first_constant: 0, num_constants: 0 }],
        );
    }
}

fn color_setup(b: &mut Builder) {
    b.shaders11(dxbc!("vs_color.vs_4_0"), dxbc!("ps_color.ps_4_0"));
    b.input_layout(&layout_pc());
}

fn tex_setup(b: &mut Builder, ps: &[u8]) {
    b.shaders11(dxbc!("vs_tex.vs_4_0"), ps);
    b.input_layout(&[
        el("POSITION", DxgiFormat::R32G32B32Float, 0, 0, false),
        el("TEXCOORD", DxgiFormat::R32G32Float, 0, 12, false),
    ]);
}

/// A clockwise textured rectangle in clip space (y up), uv 0..1 from its
/// top-left corner.
fn rect_tex(x0: f32, y0: f32, x1: f32, y1: f32) -> Vec<u8> {
    let c = [(x0, y1, 0.0, 0.0), (x1, y1, 1.0, 0.0), (x1, y0, 1.0, 1.0), (x0, y0, 0.0, 1.0)];
    [0, 1, 2, 0, 2, 3].iter().flat_map(|i| pt(c[*i].0, c[*i].1, 0.0, c[*i].2, c[*i].3)).collect()
}

fn draw_tex(b: &mut Builder, verts: &[u8]) {
    let vb = b.buffer11(bind::VERTEX_BUFFER, verts);
    b.vb(0, vb, 20);
    b.w.draw11(verts.len() as u32 / 20, 0, 1, 0);
}

fn point_sampler(b: &mut Builder) -> Handle {
    let s = b.handle();
    b.w.create_sampler(s, &SamplerDesc11 { filter: filter::MIN_MAG_MIP_POINT, ..Default::default() });
    b.w.set_samplers(Stage11::Pixel, 0, &[s]);
    s
}

/// A clockwise triangle drawn through an input layout, views and the
/// default state objects.
pub fn triangle(b: &mut Builder) {
    b.w.clear_render_target_view(b.rtv, BLACK4);
    color_setup(b);
    let v: Vec<u8> = [(-0.5, -0.5), (0.0, 0.5), (0.5, -0.5)].iter().flat_map(|(x, y)| pc(*x, *y, 0.5, RED)).collect();
    b.draw_pc(&v);
    b.present();
    b.expect(32, 32, R);
    b.expect(32, 40, R);
    b.expect(5, 5, K);
    b.expect(60, 60, K);
}

/// Constant buffers updated between draws: each draw sees the contents it
/// was recorded with. The same buffer feeds both stages.
pub fn constant_buffers(b: &mut Builder) {
    b.w.clear_render_target_view(b.rtv, BLACK4);
    b.shaders11(dxbc!("vs_cb.vs_4_0"), dxbc!("ps_cb.ps_4_0"));
    b.input_layout(&[el("POSITION", DxgiFormat::R32G32B32Float, 0, 0, false)]);
    let quad = rect_clip(0.0, 0.0, 1.0, 1.0, 0.5, 0);
    let vb = b.buffer11(bind::VERTEX_BUFFER, &quad);
    b.vb(0, vb, 16);
    let object = b.cbuffer(&[0.0; 8]);
    let frame = b.cbuffer(&[1.0, 1.0, 1.0, 1.0]);
    b.set_cb(Stage11::Vertex, 0, object);
    b.set_cb(Stage11::Pixel, 0, object);
    b.set_cb(Stage11::Pixel, 1, frame);
    let colors = [[1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0]];
    for (i, c) in colors.iter().enumerate() {
        let x = -0.9 + 0.6 * i as f32;
        let mut data = vec![x, -0.25, 0.5, 0.5];
        data.extend_from_slice(c);
        b.w.update_subresource(object, 0, None, 0, 0, &floats(&data));
        b.w.draw11(6, 0, 1, 0);
    }
    // A partial update (D3D11_BOX on a buffer) of the colour only.
    b.w.update_subresource(object, 0, None, 0, 0, &floats(&[-0.25, 0.4, 0.5, 0.5, 1.0, 1.0, 1.0, 1.0]));
    let bx = Box3 { left: 0, top: 0, front: 0, right: 16, bottom: 1, back: 1 };
    b.w.update_subresource(object, 0, Some(&bx), 0, 0, &floats(&[-0.25, 0.4, 0.5, 0.5]));
    b.w.update_subresource(frame, 0, None, 0, 0, &floats(&[1.0, 1.0, 0.0, 1.0]));
    b.w.draw11(6, 0, 1, 0);
    b.present();
    b.expect(11, 32, R);
    b.expect(30, 32, G);
    b.expect(49, 32, B);
    b.expect(32, 11, Y);
    b.expect(32, 50, K);
}

/// A vertex buffer rewritten between draws (renamed while in use), and an
/// indexed draw with a base vertex.
pub fn dynamic_buffers(b: &mut Builder) {
    b.w.clear_render_target_view(b.rtv, BLACK4);
    color_setup(b);
    let vb = b.handle();
    b.w.create_buffer11(vb, 96, bind::VERTEX_BUFFER, 0, 0);
    b.vb(0, vb, 16);
    for (i, c) in [RED, GREEN, BLUE].iter().enumerate() {
        let x = -0.9 + 0.6 * i as f32;
        b.w.update_subresource(vb, 0, None, 0, 0, &rect_clip(x, 0.0, x + 0.5, 0.5, 0.5, *c));
        b.w.draw11(6, 0, 1, 0);
    }
    // Vertices 4..8 form a quad; 0..4 would cover the screen.
    let mut verts = Vec::new();
    for (x, y) in
        [(-1.0, 1.0), (1.0, 1.0), (1.0, -1.0), (-1.0, -1.0), (-0.5, -0.2), (0.5, -0.2), (0.5, -0.8), (-0.5, -0.8)]
    {
        verts.extend(pc(x, y, 0.5, YELLOW));
    }
    let vb2 = b.buffer11(bind::VERTEX_BUFFER, &verts);
    let indices: Vec<u8> = [0u16, 1, 2, 0, 2, 3].iter().flat_map(|i| i.to_le_bytes()).collect();
    let ib = b.buffer11(bind::INDEX_BUFFER, &indices);
    b.vb(0, vb2, 16);
    b.w.set_index_buffer(ib, DxgiFormat::R16Uint, 0);
    b.w.draw_indexed11(6, 0, 4, 1, 0);
    b.present();
    b.expect(11, 24, R);
    b.expect(30, 24, G);
    b.expect(49, 24, B);
    b.expect(32, 48, Y);
    b.expect(5, 60, K);
    b.expect(32, 5, K);
}

/// SV_VertexID excludes StartVertexLocation and BaseVertexLocation (as in
/// Wine's test_vertex_id): a draw from vertex 3 still sees ids 0, 1, 2.
pub fn vertex_id(b: &mut Builder) {
    b.w.clear_render_target_view(b.rtv, BLACK4);
    // Both triangles are at z = 0: no depth buffer.
    b.w.set_render_targets11(&[b.rtv], Handle::NONE);
    b.shaders11(dxbc!("vs_fullscreen.vs_4_0"), dxbc!("ps_cb.ps_4_0"));
    b.w.set_input_layout(Handle::NONE);
    let color = b.cbuffer(&[0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0]);
    let tint = b.cbuffer(&[1.0; 4]);
    b.set_cb(Stage11::Pixel, 0, color);
    b.set_cb(Stage11::Pixel, 1, tint);
    // Ids 0, 1, 2: the whole screen.
    b.w.draw11(3, 3, 1, 0);
    // Ids 3, 4, 5 (the index values): the upper-right half.
    b.w.update_subresource(color, 0, None, 0, 0, &floats(&[0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0]));
    let indices: Vec<u8> = [3u16, 4, 5].iter().flat_map(|i| i.to_le_bytes()).collect();
    let ib = b.buffer11(bind::INDEX_BUFFER, &indices);
    b.w.set_index_buffer(ib, DxgiFormat::R16Uint, 0);
    b.w.draw_indexed11(3, 0, 7, 1, 0);
    b.present();
    b.expect(60, 3, G);
    b.expect(3, 60, R);
}

/// SV_Position in the pixel shader: pixel centres at .5, z the depth and
/// w the clip-space w (2 here).
pub fn sv_position(b: &mut Builder) {
    b.w.clear_render_target_view(b.rtv, BLACK4);
    b.shaders11(dxbc!("vs_w2.vs_4_0"), dxbc!("ps_svpos.ps_4_0"));
    b.input_layout(&[el("POSITION", DxgiFormat::R32G32B32Float, 0, 0, false)]);
    b.draw_pc(&rect_clip(-1.0, -1.0, 1.0, 1.0, 0.25, 0));
    b.present();
    // (10.5 / 64, 20.5 / 64, 0.25, 2 / 4) in 8 bits.
    b.expect(10, 20, [42, 82, 64, 128]);
    b.expect(40, 3, [162, 14, 64, 128]);
}

/// Per-instance data from a second vertex buffer, StartInstanceLocation
/// (which selects instance data but not SV_InstanceID).
pub fn instancing(b: &mut Builder) {
    b.w.clear_render_target_view(b.rtv, BLACK4);
    b.shaders11(dxbc!("vs_inst.vs_4_0"), dxbc!("ps_color.ps_4_0"));
    b.input_layout(&[
        el("POSITION", DxgiFormat::R32G32B32Float, 0, 0, false),
        el("OFFSET", DxgiFormat::R32G32Float, 1, 0, true),
        el("COLOR", DxgiFormat::B8G8R8A8Unorm, 1, 8, true),
    ]);
    let quad = rect_clip(-0.15, -0.15, 0.15, 0.15, 0.5, 0);
    let vb = b.buffer11(bind::VERTEX_BUFFER, &quad);
    let mut inst = Vec::new();
    for (x, c) in [(-0.6f32, RED), (0.0, GREEN), (0.6, YELLOW)] {
        inst.extend(floats(&[x, 0.0]));
        inst.extend(c.to_le_bytes());
    }
    let ib = b.buffer11(bind::VERTEX_BUFFER, &inst);
    b.vb(0, vb, 16);
    b.vb(1, ib, 12);
    b.w.draw11(6, 0, 2, 1);
    b.present();
    b.expect(32, 32, G); // instance data 1, SV_InstanceID 0
    b.expect(51, 32, W); // instance data 2, SV_InstanceID 1 -> blue 1
    b.expect(13, 32, K); // instance data 0 not drawn
}

/// Point sampling a 2 x 2 texture uploaded with UpdateSubresource.
pub fn texture(b: &mut Builder) {
    tex_setup(b, dxbc!("ps_tex.ps_4_0"));
    let data = [R, G, B, W].concat();
    let (_, srv) = b.texture_srv(DxgiFormat::R8G8B8A8Unorm, 2, 2, &data);
    b.w.set_shader_resources(Stage11::Pixel, 0, &[srv]);
    point_sampler(b);
    draw_tex(b, &rect_tex(-1.0, -1.0, 1.0, 1.0));
    b.present();
    b.expect(16, 16, R);
    b.expect(48, 16, G);
    b.expect(16, 48, B);
    b.expect(48, 48, W);
}

/// Mip levels written by subresource index and chosen by SampleLevel; a
/// typeless texture viewed as UNORM and as sRGB.
pub fn texture_views(b: &mut Builder) {
    b.w.clear_render_target_view(b.rtv, BLACK4);
    tex_setup(b, dxbc!("ps_tex_lod.ps_4_0"));
    point_sampler(b);
    let t = b.texture11(&Texture11Desc::d2(DxgiFormat::R8G8B8A8Unorm, 4, 4, 2, bind::SHADER_RESOURCE));
    b.w.update_subresource(t, 0, None, 16, 0, &R.repeat(16));
    b.w.update_subresource(t, 1, None, 8, 0, &G.repeat(4));
    let srv = b.view(ViewKind::ShaderResource, t, DxgiFormat::R8G8B8A8Unorm);
    b.w.set_shader_resources(Stage11::Pixel, 0, &[srv]);
    let lod = b.cbuffer(&[1.0, 0.0, 0.0, 0.0]);
    b.set_cb(Stage11::Pixel, 0, lod);
    draw_tex(b, &rect_tex(-1.0, 0.0, 1.0, 1.0));
    // 0x80 grey: 128 through UNORM, 55 through sRGB decoding.
    let typeless = b.texture11(&Texture11Desc::d2(DxgiFormat::R8G8B8A8Typeless, 1, 1, 1, bind::SHADER_RESOURCE));
    b.w.update_subresource(typeless, 0, None, 4, 0, &[0x80, 0x80, 0x80, 0xff]);
    let srgb = b.view(ViewKind::ShaderResource, typeless, DxgiFormat::R8G8B8A8UnormSrgb);
    let unorm = b.view(ViewKind::ShaderResource, typeless, DxgiFormat::R8G8B8A8Unorm);
    b.w.update_subresource(lod, 0, None, 0, 0, &floats(&[0.0; 4]));
    b.w.set_shader_resources(Stage11::Pixel, 0, &[srgb]);
    draw_tex(b, &rect_tex(-1.0, -1.0, 0.0, 0.0));
    b.w.set_shader_resources(Stage11::Pixel, 0, &[unorm]);
    draw_tex(b, &rect_tex(0.0, -1.0, 1.0, 0.0));
    b.present();
    b.expect(16, 16, G);
    b.expect(48, 16, G);
    b.expect(16, 48, [55, 55, 55, 255]);
    b.expect(48, 48, [128, 128, 128, 255]);
}

/// Rendering into a texture (cleared, then drawn) and sampling it.
pub fn render_to_texture(b: &mut Builder) {
    let t = b.texture11(&Texture11Desc::d2(
        DxgiFormat::R8G8B8A8Unorm,
        32,
        32,
        1,
        bind::RENDER_TARGET | bind::SHADER_RESOURCE,
    ));
    let rtv = b.view(ViewKind::RenderTarget, t, DxgiFormat::R8G8B8A8Unorm);
    let srv = b.view(ViewKind::ShaderResource, t, DxgiFormat::R8G8B8A8Unorm);
    b.w.set_render_targets11(&[rtv], Handle::NONE);
    b.w.set_viewports(&[Viewport11 { x: 0.0, y: 0.0, width: 32.0, height: 32.0, min_depth: 0.0, max_depth: 1.0 }]);
    b.w.clear_render_target_view(rtv, [0.0, 0.0, 1.0, 1.0]);
    color_setup(b);
    b.draw_pc(&rect_clip(-1.0, -1.0, 0.0, 1.0, 0.5, RED));
    b.w.set_render_targets11(&[b.rtv], b.dsv);
    b.full_viewport();
    b.w.clear_render_target_view(b.rtv, BLACK4);
    tex_setup(b, dxbc!("ps_tex.ps_4_0"));
    point_sampler(b);
    b.w.set_shader_resources(Stage11::Pixel, 0, &[srv]);
    draw_tex(b, &rect_tex(-1.0, -1.0, 1.0, 1.0));
    b.present();
    b.expect(16, 32, R);
    b.expect(48, 32, B);
}

/// Depth test, then stencil written with colour writes masked and tested.
pub fn depth_stencil(b: &mut Builder) {
    b.w.clear_render_target_view(b.rtv, BLACK4);
    color_setup(b);
    b.draw_pc(&rect_clip(-1.0, -1.0, 1.0, 1.0, 0.5, RED));
    b.draw_pc(&rect_clip(-1.0, -1.0, 0.0, 1.0, 0.7, GREEN)); // behind: fails
    b.draw_pc(&rect_clip(0.0, -1.0, 1.0, 1.0, 0.3, BLUE)); // in front
    let write = b.handle();
    let always_replace = StencilFace { fail: 1, depth_fail: 1, pass: 3, func: 8 };
    b.w.create_depth_stencil_state(
        write,
        &DepthStencilDesc11 {
            depth_enable: false,
            stencil_enable: true,
            front: always_replace,
            back: always_replace,
            ..Default::default()
        },
    );
    let no_color = b.handle();
    let mut nc = BlendDesc11::default();
    nc.targets[0].write_mask = 0;
    b.w.create_blend_state(no_color, &nc);
    b.w.set_depth_stencil_state(write, 1);
    b.w.set_blend_state(no_color, [1.0; 4], !0);
    b.draw_pc(&rect_clip(-1.0, 0.5, 1.0, 1.0, 0.5, WHITE));
    let test = b.handle();
    let equal = StencilFace { fail: 1, depth_fail: 1, pass: 1, func: 3 };
    b.w.create_depth_stencil_state(
        test,
        &DepthStencilDesc11 {
            depth_enable: false,
            stencil_enable: true,
            front: equal,
            back: equal,
            ..Default::default()
        },
    );
    b.w.set_depth_stencil_state(test, 1);
    b.w.set_blend_state(Handle::NONE, [1.0; 4], !0);
    b.draw_pc(&rect_clip(-1.0, -1.0, 1.0, 1.0, 0.5, YELLOW));
    b.present();
    b.expect(16, 8, Y);
    b.expect(48, 8, Y);
    b.expect(16, 40, R);
    b.expect(48, 40, B);
}

/// Alpha blending, and a blend factor.
pub fn blend(b: &mut Builder) {
    b.w.clear_render_target_view(b.rtv, [0.0, 0.0, 1.0, 1.0]);
    color_setup(b);
    let alpha = b.handle();
    let mut d = BlendDesc11::default();
    // Colour: src * srcA + dst * (1 - srcA); alpha: keep the target's.
    d.targets[0] =
        RtBlend { enable: true, src: 5, dst: 6, op: 1, src_alpha: 1, dst_alpha: 2, op_alpha: 1, write_mask: 0xf };
    b.w.create_blend_state(alpha, &d);
    b.w.set_blend_state(alpha, [1.0; 4], !0);
    b.draw_pc(&rect_clip(-1.0, -1.0, 0.0, 1.0, 0.5, argb(255, 0, 0, 0x80)));
    let factor = b.handle();
    d.targets[0] =
        RtBlend { enable: true, src: 14, dst: 1, op: 1, src_alpha: 2, dst_alpha: 1, op_alpha: 1, write_mask: 0xf };
    b.w.create_blend_state(factor, &d);
    b.w.set_blend_state(factor, [0.5, 1.0, 0.0, 1.0], !0);
    b.draw_pc(&rect_clip(0.0, -1.0, 1.0, 1.0, 0.5, WHITE));
    b.present();
    b.expect(16, 32, [128, 0, 127, 255]);
    b.expect(48, 32, [128, 255, 0, 255]);
}

/// Culling and winding from rasterizer states, and the scissor rectangle.
pub fn rasterizer(b: &mut Builder) {
    b.w.clear_render_target_view(b.rtv, BLACK4);
    color_setup(b);
    let ccw = |x0: f32, x1: f32, c: u32| -> Vec<u8> {
        [(x0, -0.5), (x1, -0.5), ((x0 + x1) / 2.0, 0.5)].iter().flat_map(|(x, y)| pc(*x, *y, 0.5, c)).collect()
    };
    b.draw_pc(&ccw(-0.9, -0.1, GREEN)); // back-facing: culled by default
    let none = b.handle();
    b.w.create_rasterizer_state(none, &RasterizerDesc11 { cull: cull::NONE, ..Default::default() });
    b.w.set_rasterizer_state(none);
    b.draw_pc(&ccw(0.1, 0.9, BLUE));
    let ccw_front = b.handle();
    b.w.create_rasterizer_state(ccw_front, &RasterizerDesc11 { front_ccw: true, ..Default::default() });
    b.w.set_rasterizer_state(ccw_front);
    let cw: Vec<u8> =
        [(-0.2, -0.95), (0.0, -0.6), (0.2, -0.95)].iter().flat_map(|(x, y)| pc(*x, *y, 0.5, RED)).collect();
    b.draw_pc(&cw); // clockwise is now back-facing
    let scissor = b.handle();
    b.w.create_rasterizer_state(scissor, &RasterizerDesc11 { cull: cull::NONE, scissor: true, ..Default::default() });
    b.w.set_rasterizer_state(scissor);
    b.w.set_scissor_rects(&[Rect::new(0, 0, 64, 8)]);
    b.draw_pc(&rect_clip(-1.0, -1.0, 1.0, 1.0, 0.5, YELLOW));
    b.present();
    b.expect(32, 4, Y);
    b.expect(32, 9, K);
    b.expect(16, 40, K);
    b.expect(48, 40, B);
    b.expect(32, 58, K);
}

/// Viewports partly outside the target and in a corner.
pub fn viewport(b: &mut Builder) {
    b.w.clear_render_target_view(b.rtv, BLACK4);
    color_setup(b);
    let mut quads = rect_clip(-1.0, 0.0, 0.0, 1.0, 0.5, RED);
    quads.extend(rect_clip(0.0, 0.0, 1.0, 1.0, 0.5, GREEN));
    quads.extend(rect_clip(-1.0, -1.0, 0.0, 0.0, 0.5, BLUE));
    quads.extend(rect_clip(0.0, -1.0, 1.0, 0.0, 0.5, WHITE));
    let vb = b.buffer11(bind::VERTEX_BUFFER, &quads);
    b.vb(0, vb, 16);
    // 64 x 64 at (32, 32): only its top-left quarter (red) is on screen.
    b.w.set_viewports(&[Viewport11 { x: 32.0, y: 32.0, width: 64.0, height: 64.0, min_depth: 0.0, max_depth: 1.0 }]);
    b.w.draw11(24, 0, 1, 0);
    b.w.set_viewports(&[Viewport11 { x: 0.0, y: 0.0, width: 32.0, height: 32.0, min_depth: 0.0, max_depth: 1.0 }]);
    b.w.draw11(24, 0, 1, 0);
    b.present();
    b.expect_rect(Rect::new(32, 32, 64, 64), R);
    b.expect(8, 8, R);
    b.expect(24, 8, G);
    b.expect(8, 24, B);
    b.expect(24, 24, W);
    b.expect(48, 16, K);
    b.expect(16, 48, K);
}

/// A compute shader writing a storage texture and a structured buffer;
/// the texture is then sampled and the buffer read back.
pub fn compute(b: &mut Builder) {
    let t = b.texture11(&Texture11Desc::d2(
        DxgiFormat::R8G8B8A8Unorm,
        16,
        16,
        1,
        bind::UNORDERED_ACCESS | bind::SHADER_RESOURCE,
    ));
    let uav = b.view(ViewKind::UnorderedAccess, t, DxgiFormat::R8G8B8A8Unorm);
    let srv = b.view(ViewKind::ShaderResource, t, DxgiFormat::R8G8B8A8Unorm);
    let buf = b.handle();
    b.w.create_buffer11(buf, 16, bind::UNORDERED_ACCESS, misc::BUFFER_STRUCTURED, 4);
    let mut d = view_desc(DxgiFormat::Unknown, ViewDim::Buffer);
    d.num_elements = 4;
    let buav = b.view_with(ViewKind::UnorderedAccess, buf, &d);
    b.w.clear_unordered_access_view_uint(buav, [0xdead; 4]);
    let cs = b.shader11(Stage11::Compute, dxbc!("cs_fill.cs_5_0"));
    b.w.set_shader11(Stage11::Compute, cs);
    b.w.set_unordered_access_views(Stage11::Compute, 0, &[uav, buav]);
    let params = b.cbuffer(&[0.0, 0.0, 0.5, 0.0]);
    b.set_cb(Stage11::Compute, 0, params);
    b.w.dispatch(2, 2, 1);
    b.w.set_unordered_access_views(Stage11::Compute, 0, &[Handle::NONE, Handle::NONE]);
    tex_setup(b, dxbc!("ps_tex.ps_4_0"));
    point_sampler(b);
    b.w.set_shader_resources(Stage11::Pixel, 0, &[srv]);
    draw_tex(b, &rect_tex(-1.0, -1.0, 1.0, 1.0));
    b.present();
    b.expect(2, 2, [0, 0, 128, 255]);
    b.expect(34, 2, [128, 0, 128, 255]);
    b.expect(62, 62, [239, 239, 128, 255]);
    b.read_back11(buf, 0, None, 0, [1u32, 4, 7, 10].iter().flat_map(|v| v.to_le_bytes()).collect());
}

/// A typed buffer (`Buffer<float4>`) read by the pixel shader.
pub fn typed_buffer(b: &mut Builder) {
    b.shaders11(dxbc!("vs_fullscreen.vs_4_0"), dxbc!("ps_buffer.ps_4_0"));
    b.w.set_input_layout(Handle::NONE);
    let colors = floats(&[1.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0, 0.0, 1.0]);
    let buf = b.buffer11(bind::SHADER_RESOURCE, &colors);
    let mut d = view_desc(DxgiFormat::R32G32B32A32Float, ViewDim::Buffer);
    d.num_elements = 4;
    let srv = b.view_with(ViewKind::ShaderResource, buf, &d);
    b.w.set_shader_resources(Stage11::Pixel, 0, &[srv]);
    b.w.draw11(3, 0, 1, 0);
    b.present();
    b.expect(8, 32, R);
    b.expect(24, 32, G);
    b.expect(40, 32, B);
    b.expect(56, 32, Y);
}

/// A depth-only pass (no pixel shader, no render target) into a typeless
/// depth texture, then comparison sampling it through an R32_FLOAT view.
pub fn shadow(b: &mut Builder) {
    let t = b.texture11(&Texture11Desc::d2(
        DxgiFormat::R32Typeless,
        64,
        64,
        1,
        bind::DEPTH_STENCIL | bind::SHADER_RESOURCE,
    ));
    let dsv = b.view(ViewKind::DepthStencil, t, DxgiFormat::D32Float);
    let srv = b.view(ViewKind::ShaderResource, t, DxgiFormat::R32Float);
    b.w.set_render_targets11(&[], dsv);
    b.w.clear_depth_stencil_view(dsv, clear11::DEPTH, 1.0, 0);
    let vs = b.shader11(Stage11::Vertex, dxbc!("vs_depth.vs_4_0"));
    b.w.set_shader11(Stage11::Vertex, vs);
    b.w.set_shader11(Stage11::Pixel, Handle::NONE);
    b.input_layout(&[el("POSITION", DxgiFormat::R32G32B32Float, 0, 0, false)]);
    b.draw_pc(&rect_clip(-1.0, -1.0, 0.0, 1.0, 0.25, 0));
    b.w.set_render_targets11(&[b.rtv], Handle::NONE);
    tex_setup(b, dxbc!("ps_shadow.ps_4_0"));
    let s = b.handle();
    b.w.create_sampler(s, &SamplerDesc11 { filter: filter::COMPARISON, comparison: 4, ..Default::default() });
    b.w.set_samplers(Stage11::Pixel, 0, &[s]);
    b.w.set_shader_resources(Stage11::Pixel, 0, &[srv]);
    draw_tex(b, &rect_tex(-1.0, -1.0, 1.0, 1.0));
    b.present();
    b.expect(16, 32, K); // 0.5 <= 0.25 fails
    b.expect(48, 32, W); // 0.5 <= 1.0 passes
}

/// Two render targets, then a copy from the second into the first.
pub fn mrt_copy(b: &mut Builder) {
    let bind_rt = bind::RENDER_TARGET | bind::SHADER_RESOURCE;
    let t = b.texture11(&Texture11Desc::d2(DxgiFormat::R8G8B8A8Unorm, 64, 64, 1, bind_rt));
    let rtv = b.view(ViewKind::RenderTarget, t, DxgiFormat::R8G8B8A8Unorm);
    b.w.set_render_targets11(&[b.rtv, rtv], b.dsv);
    b.w.clear_render_target_view(b.rtv, BLACK4);
    b.w.clear_render_target_view(rtv, BLACK4);
    b.shaders11(dxbc!("vs_color.vs_4_0"), dxbc!("ps_mrt.ps_4_0"));
    b.input_layout(&layout_pc());
    b.draw_pc(&rect_clip(-1.0, -1.0, 0.0, 1.0, 0.5, RED));
    let bx = Box3 { left: 0, top: 0, front: 0, right: 32, bottom: 64, back: 1 };
    b.w.copy_subresource_region(b.backbuffer, 0, 32, 0, 0, t, 0, Some(&bx));
    b.present();
    b.expect(16, 32, R);
    b.expect(48, 32, [0, 255, 255, 255]);
}

/// Readback of an integer render target region, and of buffers after a
/// copy and a partial update.
pub fn readback(b: &mut Builder) {
    let t = b.texture11(&Texture11Desc::d2(DxgiFormat::R32Uint, 8, 8, 1, bind::RENDER_TARGET));
    let rtv = b.view(ViewKind::RenderTarget, t, DxgiFormat::R32Uint);
    b.w.set_render_targets11(&[rtv], Handle::NONE);
    b.w.set_viewports(&[Viewport11 { x: 0.0, y: 0.0, width: 8.0, height: 8.0, min_depth: 0.0, max_depth: 1.0 }]);
    b.shaders11(dxbc!("vs_fullscreen.vs_4_0"), dxbc!("ps_uint.ps_4_0"));
    b.w.set_input_layout(Handle::NONE);
    b.w.draw11(3, 0, 1, 0);
    let bx = Box3 { left: 2, top: 3, front: 0, right: 6, bottom: 5, back: 1 };
    let expect: Vec<u8> =
        [3002u32, 3003, 3004, 3005, 4002, 4003, 4004, 4005].iter().flat_map(|v| v.to_le_bytes()).collect();
    b.read_back11(t, 0, Some(&bx), 16, expect);
    let a = b.buffer11(bind::VERTEX_BUFFER, &(1..=16).collect::<Vec<u8>>());
    let c = b.handle();
    b.w.create_buffer11(c, 16, bind::VERTEX_BUFFER, 0, 0);
    b.w.copy_resource(c, a);
    let part = Box3 { left: 4, top: 0, front: 0, right: 8, bottom: 1, back: 1 };
    b.w.update_subresource(c, 0, Some(&part), 0, 0, &[0xaa; 4]);
    let mut want: Vec<u8> = (1..=16).collect();
    want[4..8].fill(0xaa);
    b.read_back11(c, 0, None, 0, want);
    b.w.set_render_targets11(&[b.rtv], b.dsv);
    b.w.clear_render_target_view(b.rtv, [0.2, 0.4, 0.6, 1.0]);
    b.present();
    b.expect(5, 5, [51, 102, 153, 255]);
}
