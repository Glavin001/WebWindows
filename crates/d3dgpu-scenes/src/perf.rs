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
