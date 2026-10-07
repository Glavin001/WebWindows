//! Performance scenes shaped like a 2005 game: many small draws with
//! per-draw vertex constants, a handful of textures and blend states, and
//! a dynamic vertex buffer rewritten every frame.

use crate::*;

/// Objects created once by [`setup`].
pub struct Assets {
    pub textures: Vec<Handle>,
    pub vb: Handle,
}

/// Shaders, declaration, textures and the dynamic vertex buffer.
pub fn setup(b: &mut Builder) -> Assets {
    b.shaders(
        "vs_2_0
        dcl_position v0
        dcl_texcoord v1
        add oPos.xy, v0, c0
        mov oPos.zw, v0
        mov oT0, v1
        mov oD0, c1",
        "ps_2_0
        dcl t0
        dcl v0
        dcl_2d s0
        texld r0, t0, s0
        mul r0, r0, v0
        mov oC0, r0",
    );
    b.decl(&DECL_PT);
    b.w.set_render_state(RenderState::SrcBlend, Blend::SrcAlpha.0);
    b.w.set_render_state(RenderState::DestBlend, Blend::InvSrcAlpha.0);
    let textures = (0..4u8).map(|i| b.texture(Format::A8R8G8B8, 2, 2, &[i * 60; 16])).collect();
    let vb = b.handle();
    b.w.create_buffer(vb, 80, buffer_usage::VERTEX | buffer_usage::DYNAMIC);
    Assets { textures, vb }
}

/// One frame of `draws` draws, then a present.
pub fn frame(b: &mut Builder, a: &Assets, draws: u32, frame_no: u32) {
    b.w.clear(clear::TARGET | clear::ZBUFFER, argb(0, 0, 0, 255), 1.0, 0, &[]);
    let quad: Vec<u8> = [
        pt(-0.05, 0.05, 0.5, 0.0, 0.0),
        pt(0.05, 0.05, 0.5, 1.0, 0.0),
        pt(-0.05, -0.05, 0.5, 0.0, 1.0),
        pt(0.05, -0.05, 0.5, 1.0, 1.0),
    ]
    .concat();
    b.w.write_buffer(a.vb, 0, &quad);
    b.w.set_stream_source(0, a.vb, 0, 20);
    for i in 0..draws {
        let x = (i % 15) as f32 / 7.5 - 0.95;
        let y = (i / 15 % 15) as f32 / 7.5 - 0.95;
        b.w.set_shader_const_f(Stage::Vertex, 0, &[[x, y, 0.0, 0.0], [1.0, (frame_no % 7) as f32 / 7.0, 0.5, 1.0]]);
        b.w.set_texture(0, a.textures[i as usize % a.textures.len()]);
        b.w.set_render_state(RenderState::AlphaBlendEnable, (i % 3 == 0) as u32);
        b.w.draw(PrimitiveType::TriangleStrip, 0, 2);
    }
    b.present();
}

/// Objects created once by [`setup11`].
pub struct Assets11 {
    pub srvs: Vec<Handle>,
    pub vb: Handle,
    pub cb: Handle,
    pub blend: Handle,
}

/// The Direct3D 11 version of [`setup`]: the same frame shape, with a
/// constant buffer rewritten before every draw (`Map(WRITE_DISCARD)`).
pub fn setup11(b: &mut Builder) -> Assets11 {
    use d3d11::*;
    b.shaders11(
        include_bytes!("../fixtures/perf.vs_perf.vs_4_0.dxbc"),
        include_bytes!("../fixtures/perf.ps_perf.ps_4_0.dxbc"),
    );
    let el = |semantic: &str, format, offset| InputElement {
        semantic: semantic.into(),
        semantic_index: 0,
        format,
        slot: 0,
        offset,
        per_instance: false,
        step_rate: 0,
    };
    b.input_layout(&[el("POSITION", DxgiFormat::R32G32B32Float, 0), el("TEXCOORD", DxgiFormat::R32G32Float, 12)]);
    b.w.set_primitive_topology(5); // triangle strip
    let srvs = (0..4u8).map(|i| b.texture_srv(DxgiFormat::R8G8B8A8Unorm, 2, 2, &[i * 60; 16]).1).collect();
    let s = b.handle();
    b.w.create_sampler(s, &SamplerDesc11 { filter: filter::MIN_MAG_MIP_POINT, ..Default::default() });
    b.w.set_samplers(Stage11::Pixel, 0, &[s]);
    let vb = b.handle();
    b.w.create_buffer11(vb, 80, bind::VERTEX_BUFFER, 0, 0);
    let cb = b.handle();
    b.w.create_buffer11(cb, 32, bind::CONSTANT_BUFFER, 0, 0);
    b.set_cb(Stage11::Vertex, 0, cb);
    let blend = b.handle();
    let mut d = BlendDesc11::default();
    d.targets[0] =
        RtBlend { enable: true, src: 5, dst: 6, op: 1, src_alpha: 5, dst_alpha: 6, op_alpha: 1, write_mask: 0xf };
    b.w.create_blend_state(blend, &d);
    Assets11 { srvs, vb, cb, blend }
}

/// One Direct3D 11 frame of `draws` draws, then a present.
pub fn frame11(b: &mut Builder, a: &Assets11, draws: u32, frame_no: u32) {
    use d3d11::*;
    b.w.clear_render_target_view(b.rtv, [0.0, 0.0, 0.0, 1.0]);
    b.w.clear_depth_stencil_view(b.dsv, clear11::DEPTH, 1.0, 0);
    let quad: Vec<u8> = [
        pt(-0.05, 0.05, 0.5, 0.0, 0.0),
        pt(0.05, 0.05, 0.5, 1.0, 0.0),
        pt(-0.05, -0.05, 0.5, 0.0, 1.0),
        pt(0.05, -0.05, 0.5, 1.0, 1.0),
    ]
    .concat();
    b.w.update_subresource(a.vb, 0, None, 0, 0, &quad);
    b.vb(0, a.vb, 20);
    for i in 0..draws {
        let x = (i % 15) as f32 / 7.5 - 0.95;
        let y = (i / 15 % 15) as f32 / 7.5 - 0.95;
        let c: Vec<u8> =
            [x, y, 0.0, 0.0, 1.0, (frame_no % 7) as f32 / 7.0, 0.5, 1.0].iter().flat_map(|f| f.to_le_bytes()).collect();
        b.w.update_subresource(a.cb, 0, None, 0, 0, &c);
        b.w.set_shader_resources(Stage11::Pixel, 0, &[a.srvs[i as usize % a.srvs.len()]]);
        let blend = if i % 3 == 0 { a.blend } else { Handle::NONE };
        b.w.set_blend_state(blend, [1.0; 4], !0);
        b.w.draw11(4, 0, 1, 0);
    }
    b.present();
}
