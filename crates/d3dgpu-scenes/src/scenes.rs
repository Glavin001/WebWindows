//! The scenes. All are built at 64 x 64 by the tests; coordinates in the
//! comments assume that size.

use crate::*;

const RED: u32 = argb(255, 0, 0, 255);
const GREEN: u32 = argb(0, 255, 0, 255);
const BLUE: u32 = argb(0, 0, 255, 255);
const WHITE: u32 = argb(255, 255, 255, 255);
const BLACK: u32 = argb(0, 0, 0, 255);
const YELLOW: u32 = argb(255, 255, 0, 255);

const R: [u8; 4] = [255, 0, 0, 255];
const G: [u8; 4] = [0, 255, 0, 255];
const B: [u8; 4] = [0, 0, 255, 255];
const W: [u8; 4] = [255, 255, 255, 255];
const K: [u8; 4] = [0, 0, 0, 255];
const Y: [u8; 4] = [255, 255, 0, 255];

pub const ALL: &[Scene] = &[
    Scene { name: "clear", build: clear },
    Scene { name: "triangle", build: triangle },
    Scene { name: "cull", build: cull },
    Scene { name: "texture_ps11", build: texture_ps11 },
    Scene { name: "texture_formats", build: texture_formats },
    Scene { name: "triangle_fan", build: triangle_fan },
    Scene { name: "triangle_fan_indexed", build: triangle_fan_indexed },
    Scene { name: "wireframe", build: wireframe },
    Scene { name: "alpha_test", build: alpha_test },
    Scene { name: "alpha_blend", build: alpha_blend },
    Scene { name: "depth_test", build: depth_test },
    Scene { name: "stencil", build: stencil },
    Scene { name: "scissor_clear", build: scissor_clear },
    Scene { name: "fog_table", build: fog_table },
    Scene { name: "fog_vertex", build: fog_vertex },
    Scene { name: "clip_plane", build: clip_plane },
    Scene { name: "half_pixel", build: half_pixel },
    Scene { name: "viewport_clamp", build: viewport_clamp },
    Scene { name: "dynamic_buffer", build: dynamic_buffer },
    Scene { name: "readback", build: readback },
    Scene { name: "ps30_vpos_vface", build: ps30_vpos_vface },
    Scene { name: "vertex_formats", build: vertex_formats },
    Scene { name: "relative_constants", build: relative_constants },
    Scene { name: "draw_up", build: draw_up },
    Scene { name: "instancing", build: instancing },
    Scene { name: "stretch_rect", build: stretch_rect },
    Scene { name: "gamma_ramp", build: gamma_ramp },
    Scene { name: "lines_points", build: lines_points },
    Scene { name: "cube_texture", build: cube_texture },
    Scene { name: "shadow_compare", build: shadow_compare },
    Scene { name: "ps14", build: ps14 },
    Scene { name: "palette_p8", build: palette_p8 },
    Scene { name: "volume_texture", build: volume_texture },
    Scene { name: "flat_shading", build: flat_shading },
    Scene { name: "triangle_strip_indexed", build: triangle_strip_indexed },
    Scene { name: "flow_control", build: flow_control },
];

fn color_setup(b: &mut Builder) {
    b.shaders(VS_COLOR, PS_COLOR);
    b.decl(&DECL_PC);
}

fn pixel_setup(b: &mut Builder) {
    b.shaders(VS_PIXEL, PS_COLOR);
    b.decl(&DECL_PC);
    b.pixel_space();
}

fn draw_list(b: &mut Builder, verts: &[u8]) {
    let n = verts.len() as u32 / 16;
    b.w.draw_up(PrimitiveType::TriangleList, n / 3, 16, verts);
}

/// A clear to a D3DCOLOR shows that colour.
fn clear(b: &mut Builder) {
    b.w.clear(clear::TARGET, argb(0x33, 0x66, 0x99, 0xff), 1.0, 0, &[]);
    b.present();
    b.expect_rect(Rect::new(0, 0, 64, 64), [0x33, 0x66, 0x99, 0xff]);
}

/// A clockwise triangle survives the default `D3DCULL_CCW`.
fn triangle(b: &mut Builder) {
    b.w.clear(clear::TARGET | clear::ZBUFFER, BLACK, 1.0, 0, &[]);
    color_setup(b);
    let v: Vec<u8> = [(-0.5, -0.5), (0.0, 0.5), (0.5, -0.5)].iter().flat_map(|(x, y)| pc(*x, *y, 0.5, RED)).collect();
    let vb = b.buffer(&v, buffer_usage::VERTEX);
    b.w.set_stream_source(0, vb, 0, 16);
    b.w.draw(PrimitiveType::TriangleList, 0, 1);
    b.present();
    b.expect(32, 32, R);
    b.expect(32, 40, R);
    b.expect(5, 5, K);
    b.expect(60, 60, K);
}

/// Counter-clockwise triangles are culled by default and drawn with
/// `D3DCULL_NONE`.
fn cull(b: &mut Builder) {
    b.w.clear(clear::TARGET, BLACK, 1.0, 0, &[]);
    color_setup(b);
    let ccw = |x0: f32, x1: f32, c: u32| -> Vec<u8> {
        [(x0, -0.5), (x1, -0.5), ((x0 + x1) / 2.0, 0.5)].iter().flat_map(|(x, y)| pc(*x, *y, 0.5, c)).collect()
    };
    draw_list(b, &ccw(-0.9, -0.1, GREEN));
    b.w.set_render_state(RenderState::CullMode, Cull::None.0);
    draw_list(b, &ccw(0.1, 0.9, BLUE));
    // D3DCULL_CW culls the clockwise one.
    b.w.set_render_state(RenderState::CullMode, Cull::Cw.0);
    let cw: Vec<u8> = [(-0.2, -0.9), (0.0, -0.6), (0.2, -0.9)].iter().flat_map(|(x, y)| pc(*x, *y, 0.5, RED)).collect();
    draw_list(b, &cw);
    b.present();
    b.expect(16, 32, K);
    b.expect(48, 32, B);
    b.expect(32, 59, K);
}

/// `vs_1_1` / `ps_1_1` with `tex`, point sampling a 2 x 2 texture.
fn texture_ps11(b: &mut Builder) {
    b.shaders(
        "vs_1_1
        dcl_position v0
        dcl_texcoord v1
        mov oPos, v0
        mov oT0, v1",
        "ps_1_1
        tex t0
        mov r0, t0",
    );
    b.decl(&DECL_PT);
    // A8R8G8B8 texels are B, G, R, A in memory.
    let texels = [[0, 0, 255, 255], [0, 255, 0, 255], [255, 0, 0, 255], [255, 255, 255, 255]].concat();
    let t = b.texture(Format::A8R8G8B8, 2, 2, &texels);
    b.w.set_texture(0, t);
    let v = [
        pt(-1.0, 1.0, 0.5, 0.0, 0.0),
        pt(1.0, 1.0, 0.5, 1.0, 0.0),
        pt(-1.0, -1.0, 0.5, 0.0, 1.0),
        pt(1.0, -1.0, 0.5, 1.0, 1.0),
    ]
    .concat();
    b.w.draw_up(PrimitiveType::TriangleStrip, 2, 20, &v);
    b.present();
    b.expect(16, 16, R);
    b.expect(48, 16, G);
    b.expect(16, 48, B);
    b.expect(48, 48, W);
}

/// Packed, luminance, alpha-only, X8 and DXT1 textures sampled in bands of
/// 8 pixels.
fn texture_formats(b: &mut Builder) {
    b.w.clear(clear::TARGET, BLACK, 1.0, 0, &[]);
    b.shaders(
        "vs_2_0
        dcl_position v0
        dcl_texcoord v1
        mad oPos.xy, v0, c0, c0.zwzw
        mov oPos.zw, v0
        mov oT0, v1",
        PS_TEX,
    );
    b.decl(&DECL_PT);
    b.pixel_space();
    let dxt1 = {
        // One block: colour 0 = pure green (0x07e0), colour 1 = black, all
        // indices 0.
        let mut blk = vec![0xe0, 0x07, 0x00, 0x00];
        blk.extend_from_slice(&[0; 4]);
        blk
    };
    let cases: Vec<(Format, u32, Vec<u8>, [u8; 4])> = vec![
        (Format::R5G6B5, 1, 0xf800u16.to_le_bytes().to_vec(), R),
        (Format::A1R5G5B5, 1, 0x83e0u16.to_le_bytes().to_vec(), G),
        (Format::A4R4G4B4, 1, 0xf00fu16.to_le_bytes().to_vec(), B),
        (Format::L8, 1, vec![0x80], [128, 128, 128, 255]),
        (Format::A8L8, 1, vec![0x40, 0xff], [64, 64, 64, 255]),
        (Format::X8R8G8B8, 1, vec![0x10, 0x20, 0x30, 0x00], [0x30, 0x20, 0x10, 255]),
        (Format::Dxt1, 4, [dxt1.clone(); 1].concat(), G),
        (Format::A8, 1, vec![0x80], [0, 0, 0, 128]),
    ];
    for (i, (format, size, data, want)) in cases.into_iter().enumerate() {
        let t = b.texture(format, size, size, &data);
        b.w.set_texture(0, t);
        let x0 = i as f32 * 8.0 - 0.5;
        let x1 = x0 + 8.0;
        let v = [
            pt(x0, -0.5, 0.5, 0.5, 0.5),
            pt(x1, -0.5, 0.5, 0.5, 0.5),
            pt(x0, 63.5, 0.5, 0.5, 0.5),
            pt(x1, 63.5, 0.5, 0.5, 0.5),
        ]
        .concat();
        b.w.draw_up(PrimitiveType::TriangleStrip, 2, 20, &v);
        b.expect(i as i32 * 8 + 4, 32, want);
    }
    b.present();
}

fn diamond_fan() -> Vec<u8> {
    [(0.0, 0.0), (0.0, 0.8), (0.8, 0.0), (0.0, -0.8), (-0.8, 0.0), (0.0, 0.8)]
        .iter()
        .flat_map(|(x, y)| pc(*x, *y, 0.5, YELLOW))
        .collect()
}

/// `D3DPT_TRIANGLEFAN` (WebGPU has no fans): a diamond around the centre.
fn triangle_fan(b: &mut Builder) {
    b.w.clear(clear::TARGET, BLACK, 1.0, 0, &[]);
    color_setup(b);
    let vb = b.buffer(&diamond_fan(), buffer_usage::VERTEX);
    b.w.set_stream_source(0, vb, 0, 16);
    b.w.draw(PrimitiveType::TriangleFan, 0, 4);
    b.present();
    b.expect(32, 32, Y);
    b.expect(50, 32, Y);
    b.expect(32, 14, Y);
    b.expect(2, 2, K);
    b.expect(60, 4, K);
}

/// An indexed fan with a base vertex: two unused vertices first.
fn triangle_fan_indexed(b: &mut Builder) {
    b.w.clear(clear::TARGET, BLACK, 1.0, 0, &[]);
    color_setup(b);
    let mut v = pc(0.9, 0.9, 0.5, RED);
    v.extend(pc(0.9, 0.8, 0.5, RED));
    v.extend(&diamond_fan()[..5 * 16]);
    let vb = b.buffer(&v, buffer_usage::VERTEX);
    let ib: Vec<u8> = [0u16, 1, 2, 3, 4, 1].iter().flat_map(|i| i.to_le_bytes()).collect();
    let ib = b.buffer(&ib, buffer_usage::INDEX);
    b.w.set_stream_source(0, vb, 0, 16);
    b.w.set_indices(ib, Format::Index16);
    b.w.draw_indexed(
        PrimitiveType::TriangleFan,
        &IndexedDraw { base_vertex: 2, min_index: 0, num_vertices: 5, start_index: 0, prim_count: 4 },
    );
    b.present();
    b.expect(32, 32, Y);
    b.expect(50, 32, Y);
    b.expect(2, 2, K);
}

/// `D3DFILL_WIREFRAME`: edges only.
fn wireframe(b: &mut Builder) {
    b.w.clear(clear::TARGET, BLACK, 1.0, 0, &[]);
    color_setup(b);
    b.w.set_render_state(RenderState::FillMode, FillMode::Wireframe.0);
    let v: Vec<u8> = [(-0.8, -0.8), (0.0, 0.8), (0.8, -0.8)].iter().flat_map(|(x, y)| pc(*x, *y, 0.5, WHITE)).collect();
    draw_list(b, &v);
    b.present();
    // Interior stays clear; the bottom edge (y = -0.8, pixel row ~57) is lit.
    b.expect(32, 40, K);
    b.expect_any(Rect::new(30, 55, 34, 60), W);
}

/// Alpha test `GREATER 0x80` keeps alpha 0xc0 and drops alpha 0x40.
fn alpha_test(b: &mut Builder) {
    b.w.clear(clear::TARGET, BLUE, 1.0, 0, &[]);
    color_setup(b);
    b.w.set_render_state(RenderState::AlphaTestEnable, 1);
    b.w.set_render_state(RenderState::AlphaRef, 0x80);
    b.w.set_render_state(RenderState::AlphaFunc, CmpFunc::Greater.0);
    draw_list(b, &rect_clip(-1.0, -1.0, 0.0, 1.0, 0.5, argb(255, 0, 0, 0x40)));
    draw_list(b, &rect_clip(0.0, -1.0, 1.0, 1.0, 0.5, argb(0, 255, 0, 0xc0)));
    // Exactly at the reference fails GREATER.
    b.w.set_render_state(RenderState::AlphaFunc, CmpFunc::Greater.0);
    draw_list(b, &rect_clip(-0.25, -0.25, 0.25, 0.25, 0.5, argb(255, 255, 255, 0x80)));
    b.present();
    b.expect(16, 32, B);
    b.expect(48, 10, [0, 255, 0, 0xc0]);
    b.expect(30, 32, B);
}

/// `SRCALPHA` / `INVSRCALPHA` blending of half-transparent red over blue.
fn alpha_blend(b: &mut Builder) {
    b.w.clear(clear::TARGET, BLUE, 1.0, 0, &[]);
    color_setup(b);
    b.w.set_render_state(RenderState::AlphaBlendEnable, 1);
    b.w.set_render_state(RenderState::SrcBlend, Blend::SrcAlpha.0);
    b.w.set_render_state(RenderState::DestBlend, Blend::InvSrcAlpha.0);
    draw_list(b, &rect_clip(-1.0, -1.0, 1.0, 1.0, 0.5, argb(255, 0, 0, 0x80)));
    b.present();
    // a = 128/255: r = 255a = 128, b = 255(1-a) = 127, alpha = a*a + (1-a) = 0.75.
    b.expect(32, 32, [128, 0, 127, 191]);
}

/// Depth test with `LESSEQUAL`, then `GREATER`.
fn depth_test(b: &mut Builder) {
    b.w.clear(clear::TARGET | clear::ZBUFFER, BLACK, 1.0, 0, &[]);
    color_setup(b);
    draw_list(b, &rect_clip(-1.0, -1.0, 1.0, 1.0, 0.3, RED));
    draw_list(b, &rect_clip(-1.0, -1.0, 1.0, 1.0, 0.6, BLUE));
    b.w.set_render_state(RenderState::ZFunc, CmpFunc::Greater.0);
    draw_list(b, &rect_clip(0.0, -1.0, 1.0, 1.0, 0.6, GREEN));
    // With ZENABLE off everything passes.
    b.w.set_render_state(RenderState::ZEnable, 0);
    draw_list(b, &rect_clip(-1.0, 0.5, -0.5, 1.0, 0.9, WHITE));
    b.present();
    b.expect(16, 32, R);
    b.expect(48, 32, G);
    b.expect(4, 4, W);
}

/// Stencil written by an invisible quad, then used as a mask.
fn stencil(b: &mut Builder) {
    b.w.clear(clear::TARGET | clear::ZBUFFER | clear::STENCIL, BLACK, 1.0, 0, &[]);
    color_setup(b);
    b.w.set_render_state(RenderState::StencilEnable, 1);
    b.w.set_render_state(RenderState::StencilFunc, CmpFunc::Always.0);
    b.w.set_render_state(RenderState::StencilPass, StencilOp::Replace.0);
    b.w.set_render_state(RenderState::StencilRef, 1);
    b.w.set_render_state(RenderState::ColorWriteEnable, 0);
    draw_list(b, &rect_clip(-1.0, -1.0, 0.0, 1.0, 0.5, RED));
    b.w.set_render_state(RenderState::ColorWriteEnable, 0xf);
    b.w.set_render_state(RenderState::StencilFunc, CmpFunc::Equal.0);
    b.w.set_render_state(RenderState::StencilPass, StencilOp::Keep.0);
    b.w.set_render_state(RenderState::ZFunc, CmpFunc::Always.0);
    draw_list(b, &rect_clip(-1.0, -1.0, 1.0, 1.0, 0.5, GREEN));
    b.present();
    b.expect(16, 32, G);
    b.expect(48, 32, K);
}

/// `Clear` honours the scissor rectangle and explicit rects.
fn scissor_clear(b: &mut Builder) {
    b.w.clear(clear::TARGET, BLACK, 1.0, 0, &[]);
    b.w.set_scissor(Rect::new(16, 16, 48, 48));
    b.w.set_render_state(RenderState::ScissorTestEnable, 1);
    b.w.clear(clear::TARGET, RED, 1.0, 0, &[]);
    b.w.set_render_state(RenderState::ScissorTestEnable, 0);
    b.w.clear(clear::TARGET, GREEN, 1.0, 0, &[Rect::new(0, 0, 8, 8)]);
    // Scissored draws too.
    b.w.set_render_state(RenderState::ScissorTestEnable, 1);
    b.w.set_scissor(Rect::new(56, 0, 64, 8));
    color_setup(b);
    draw_list(b, &rect_clip(-1.0, -1.0, 1.0, 1.0, 0.5, BLUE));
    b.present();
    b.expect_rect(Rect::new(16, 16, 48, 48), R);
    b.expect(8, 32, K);
    b.expect(15, 15, K);
    b.expect(48, 48, K);
    b.expect_rect(Rect::new(0, 0, 8, 8), G);
    b.expect_rect(Rect::new(56, 0, 64, 8), B);
    b.expect(55, 4, K);
}

/// Linear table fog from depth: z = 0.5 between start 0 and end 1 is half
/// fogged.
fn fog_table(b: &mut Builder) {
    b.w.clear(clear::TARGET, BLACK, 1.0, 0, &[]);
    color_setup(b);
    b.w.set_render_state(RenderState::FogEnable, 1);
    b.w.set_render_state(RenderState::FogTableMode, FogMode::Linear.0);
    b.w.set_render_state(RenderState::FogStart, 0.0f32.to_bits());
    b.w.set_render_state(RenderState::FogEnd, 1.0f32.to_bits());
    b.w.set_render_state(RenderState::FogColor, WHITE);
    draw_list(b, &rect_clip(-1.0, -1.0, 1.0, 1.0, 0.5, RED));
    b.present();
    b.expect_tol(32, 32, [255, 128, 128, 255], 3);
}

/// Vertex fog from `oFog`: factor 0.25 keeps a quarter of the colour.
fn fog_vertex(b: &mut Builder) {
    b.w.clear(clear::TARGET, BLACK, 1.0, 0, &[]);
    b.shaders(
        "vs_1_1
        dcl_position v0
        dcl_color v1
        def c4, 0.25, 0, 0, 0
        mov oPos, v0
        mov oD0, v1
        mov oFog, c4.x",
        "ps_1_1
        mov r0, v0",
    );
    b.decl(&DECL_PC);
    b.w.set_render_state(RenderState::FogEnable, 1);
    b.w.set_render_state(RenderState::FogColor, WHITE);
    draw_list(b, &rect_clip(-1.0, -1.0, 1.0, 1.0, 0.5, RED));
    b.present();
    b.expect_tol(32, 32, [255, 191, 191, 255], 3);
}

/// A user clip plane keeps x >= 0 in clip space.
fn clip_plane(b: &mut Builder) {
    b.w.clear(clear::TARGET, BLACK, 1.0, 0, &[]);
    color_setup(b);
    b.w.set_clip_plane(0, [1.0, 0.0, 0.0, 0.0]);
    b.w.set_render_state(RenderState::ClipPlaneEnable, 1);
    draw_list(b, &rect_clip(-1.0, -1.0, 1.0, 1.0, 0.5, RED));
    // Plane 2, y <= 0.5: removes the top quarter of this green quad.
    b.w.set_clip_plane(2, [0.0, -1.0, 0.0, 0.5]);
    b.w.set_render_state(RenderState::ClipPlaneEnable, 0b100);
    draw_list(b, &rect_clip(-1.0, -1.0, -0.5, 1.0, 0.5, GREEN));
    b.present();
    b.expect(24, 32, K);
    b.expect(48, 32, R);
    b.expect(8, 32, G);
    b.expect(8, 4, K);
}

/// Direct3D 9 pixel centres are at integers: a quad from 9.5 to 19.5
/// covers exactly pixels 10..=19.
fn half_pixel(b: &mut Builder) {
    b.w.clear(clear::TARGET, BLACK, 1.0, 0, &[]);
    pixel_setup(b);
    draw_list(b, &rect_px(9.5, 9.5, 19.5, 19.5, 0.5, WHITE));
    b.present();
    b.expect_rect(Rect::new(10, 10, 20, 20), W);
    b.expect_rect(Rect::new(9, 9, 21, 10), K);
    b.expect_rect(Rect::new(9, 20, 21, 21), K);
    b.expect_rect(Rect::new(9, 10, 10, 20), K);
    b.expect_rect(Rect::new(20, 10, 21, 20), K);
}

/// A viewport running past the right edge (x = 32, width 128): WebGPU
/// rejects it, so the core clamps it and fixes up positions.
fn viewport_clamp(b: &mut Builder) {
    b.w.clear(clear::TARGET, BLACK, 1.0, 0, &[]);
    color_setup(b);
    b.w.set_viewport(&Viewport { x: 32, y: 0, width: 128, height: 64, min_z: 0.0, max_z: 1.0 });
    // NDC x -1..-0.5 maps to pixels 32..64.
    draw_list(b, &rect_clip(-1.0, -1.0, -0.5, 1.0, 0.5, RED));
    b.present();
    b.expect(34, 32, R);
    b.expect(62, 32, R);
    b.expect(29, 32, K);
}

/// Rewriting a vertex buffer between two draws of one batch: each draw
/// must see its own contents.
fn dynamic_buffer(b: &mut Builder) {
    b.w.clear(clear::TARGET, BLACK, 1.0, 0, &[]);
    color_setup(b);
    let left = rect_clip(-1.0, -1.0, 0.0, 1.0, 0.5, RED);
    let right = rect_clip(0.0, -1.0, 1.0, 1.0, 0.5, BLUE);
    let vb = b.buffer(&left, buffer_usage::VERTEX | buffer_usage::DYNAMIC);
    b.w.set_stream_source(0, vb, 0, 16);
    b.w.draw(PrimitiveType::TriangleList, 0, 2);
    b.w.write_buffer(vb, 0, &right);
    b.w.draw(PrimitiveType::TriangleList, 0, 2);
    // A texture rewritten between draws, too.
    b.shaders(VS_TEX, PS_TEX);
    b.decl(&DECL_PT);
    let t = b.texture(Format::A8R8G8B8, 1, 1, &[0, 255, 0, 255]);
    b.w.set_texture(0, t);
    let quad = |x0: f32, x1: f32| {
        [pt(x0, 1.0, 0.5, 0.5, 0.5), pt(x1, 1.0, 0.5, 0.5, 0.5), pt(x0, 0.5, 0.5, 0.5, 0.5), pt(x1, 0.5, 0.5, 0.5, 0.5)]
            .concat()
    };
    b.w.draw_up(PrimitiveType::TriangleStrip, 2, 20, &quad(-1.0, -0.5));
    b.w.write_texture(
        &TextureRegion { texture: t, face: 0, level: 0, x: 0, y: 0, z: 0, width: 1, height: 1, depth: 1 },
        4,
        0,
        &[255, 255, 255, 255],
    );
    b.w.draw_up(PrimitiveType::TriangleStrip, 2, 20, &quad(0.5, 1.0));
    b.present();
    b.expect(16, 32, R);
    b.expect(48, 32, B);
    b.expect(4, 4, G);
    b.expect(60, 4, W);
}

/// Render-target readback into shared memory, in the Direct3D format.
fn readback(b: &mut Builder) {
    let rt = b.render_target(Format::A8R8G8B8, 8, 8);
    let rt565 = b.render_target(Format::R5G6B5, 4, 4);
    b.w.set_depth_stencil(Handle::NONE, 0, 0);
    b.w.set_render_target(0, rt, 0, 0);
    b.w.set_viewport(&Viewport { x: 0, y: 0, width: 8, height: 8, min_z: 0.0, max_z: 1.0 });
    b.w.clear(clear::TARGET, argb(0x30, 0x20, 0x10, 0xff), 1.0, 0, &[]);
    b.w.clear(clear::TARGET, argb(0xff, 0, 0, 0x80), 1.0, 0, &[Rect::new(0, 0, 1, 1)]);
    let region =
        |t, w, h| TextureRegion { texture: t, face: 0, level: 0, x: 0, y: 0, z: 0, width: w, height: h, depth: 1 };
    let mut want = Vec::new();
    for y in 0..2 {
        for x in 0..2 {
            want.extend_from_slice(if x == 0 && y == 0 { &[0, 0, 0xff, 0x80] } else { &[0x10, 0x20, 0x30, 0xff] });
        }
    }
    b.read_back(region(rt, 2, 2), 8, want);
    b.w.set_render_target(0, rt565, 0, 0);
    b.w.set_viewport(&Viewport { x: 0, y: 0, width: 4, height: 4, min_z: 0.0, max_z: 1.0 });
    b.w.clear(clear::TARGET, argb(0xff, 0, 0, 0xff), 1.0, 0, &[]);
    b.read_back(region(rt565, 2, 1), 4, vec![0x00, 0xf8, 0x00, 0xf8]);
    // The scene's frame: back to the back buffer.
    b.w.set_render_target(0, b.backbuffer, 0, 0);
    b.w.set_viewport(&Viewport { x: 0, y: 0, width: 64, height: 64, min_z: 0.0, max_z: 1.0 });
    b.w.clear(clear::TARGET, GREEN, 1.0, 0, &[]);
    b.present();
    b.expect(4, 4, G);
}

/// `ps_3_0` with `vPos` (integer pixel coordinates) and `vFace`.
fn ps30_vpos_vface(b: &mut Builder) {
    b.w.clear(clear::TARGET, BLACK, 1.0, 0, &[]);
    b.shaders(
        "vs_3_0
        dcl_position v0
        dcl_position o0
        mov o0, v0",
        "ps_3_0
        dcl vPos.xy
        dcl vFace
        def c0, 0.015625, 0, 1, 0
        mul r0.x, vPos.x, c0.x
        cmp r0.y, vFace, c0.z, c0.y
        mul r0.z, vPos.y, c0.x
        mov r0.w, c0.z
        mov oC0, r0",
    );
    b.decl(&DECL_PC);
    b.w.set_render_state(RenderState::CullMode, Cull::None.0);
    draw_list(b, &rect_clip(-1.0, -1.0, 1.0, 1.0, 0.5, WHITE));
    b.present();
    // vPos at pixel 10 is 10.0: 10 / 64 * 255 = 39.8.
    b.expect_tol(10, 20, [40, 255, 80, 255], 1);
    b.expect_tol(48, 0, [191, 255, 0, 255], 1);
}

/// Vertex formats: `SHORT2` positions, `D3DCOLOR`, `UBYTE4`, `DEC3N`.
fn vertex_formats(b: &mut Builder) {
    b.w.clear(clear::TARGET, BLACK, 1.0, 0, &[]);
    b.shaders(
        "vs_2_0
        dcl_position v0
        dcl_color v1
        dcl_texcoord v2
        dcl_normal v3
        def c1, 0.00392156863, 0, 0, 1
        mad oPos.xy, v0, c0, c0.zwzw
        mov oPos.zw, c1.zw
        mov oD0, v1
        mul oT0, v2, c1.x
        mov oT1, v3",
        "ps_2_0
        dcl v0
        dcl t0
        dcl t1
        add r0, v0, t0
        mov r0.x, t1.x
        mov oC0, r0",
    );
    b.decl(&[
        VertexElement::new(0, 0, DeclType::Short2, DeclUsage::Position, 0),
        VertexElement::new(0, 4, DeclType::D3dColor, DeclUsage::Color, 0),
        VertexElement::new(0, 8, DeclType::UByte4, DeclUsage::TexCoord, 0),
        VertexElement::new(0, 12, DeclType::Dec3N, DeclUsage::Normal, 0),
    ]);
    b.pixel_space();
    // DEC3N x = 511 is +1.0.
    let v = |x: i16, y: i16| {
        let mut v = Vec::new();
        v.extend_from_slice(&x.to_le_bytes());
        v.extend_from_slice(&y.to_le_bytes());
        v.extend_from_slice(&argb(0, 255, 0, 255).to_le_bytes());
        v.extend_from_slice(&[0, 0, 255, 0]);
        v.extend_from_slice(&511u32.to_le_bytes());
        v
    };
    // Integer positions -1..65 cover every pixel centre.
    let verts = [v(-1, -1), v(65, -1), v(-1, 65), v(65, 65)].concat();
    b.w.draw_up(PrimitiveType::TriangleStrip, 2, 16, &verts);
    b.present();
    b.expect(32, 32, [255, 255, 255, 255]);
}

/// `mova` and relative constant addressing pick a colour by index.
fn relative_constants(b: &mut Builder) {
    b.w.clear(clear::TARGET, BLACK, 1.0, 0, &[]);
    b.shaders(
        "vs_2_0
        dcl_position v0
        dcl_blendindices v1
        mova a0.x, v1.x
        mov oPos, v0
        mov oD0, c[a0.x + 10]",
        PS_COLOR,
    );
    b.decl(&[
        VertexElement::new(0, 0, DeclType::Float3, DeclUsage::Position, 0),
        VertexElement::new(0, 12, DeclType::UByte4, DeclUsage::BlendIndices, 0),
    ]);
    b.w.set_shader_const_f(Stage::Vertex, 10, &[[1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0]]);
    // Index 2 -> c12 (blue) on the left, index 1 -> c11 (green) on the right.
    let quad = |x0: f32, x1: f32, idx: u8| -> Vec<u8> {
        rect_clip(x0, -1.0, x1, 1.0, 0.5, 0)
            .chunks(16)
            .flat_map(|c| {
                let mut v = c[..12].to_vec();
                v.extend_from_slice(&[idx, 0, 0, 0]);
                v
            })
            .collect()
    };
    b.w.draw_up(PrimitiveType::TriangleList, 2, 16, &quad(-1.0, 0.0, 2));
    b.w.draw_up(PrimitiveType::TriangleList, 2, 16, &quad(0.0, 1.0, 1));
    b.present();
    b.expect(16, 32, B);
    b.expect(48, 32, G);
}

/// `DrawPrimitiveUP` and `DrawIndexedPrimitiveUP`.
fn draw_up(b: &mut Builder) {
    b.w.clear(clear::TARGET, BLACK, 1.0, 0, &[]);
    color_setup(b);
    draw_list(b, &rect_clip(-1.0, -1.0, 0.0, 1.0, 0.5, RED));
    let v =
        [pc(0.0, 1.0, 0.5, BLUE), pc(1.0, 1.0, 0.5, BLUE), pc(1.0, -1.0, 0.5, BLUE), pc(0.0, -1.0, 0.5, BLUE)].concat();
    let i: Vec<u8> = [0u16, 1, 2, 0, 2, 3].iter().flat_map(|i| i.to_le_bytes()).collect();
    b.w.draw_indexed_up(PrimitiveType::TriangleList, 0, 4, 2, Format::Index16, &i, 16, &v);
    b.present();
    b.expect(16, 32, R);
    b.expect(48, 32, B);
}

/// Geometry instancing: stream 0 repeated three times, stream 1 per
/// instance.
fn instancing(b: &mut Builder) {
    b.w.clear(clear::TARGET, BLACK, 1.0, 0, &[]);
    b.shaders(
        "vs_2_0
        dcl_position v0
        dcl_texcoord v1
        dcl_color v2
        add oPos.xy, v0, v1
        mov oPos.zw, v0
        mov oD0, v2",
        PS_COLOR,
    );
    b.decl(&[
        VertexElement::new(0, 0, DeclType::Float3, DeclUsage::Position, 0),
        VertexElement::new(1, 0, DeclType::Float2, DeclUsage::TexCoord, 0),
        VertexElement::new(1, 8, DeclType::D3dColor, DeclUsage::Color, 0),
    ]);
    let quad: Vec<u8> = [(-0.2, 0.2), (0.2, 0.2), (0.2, -0.2), (-0.2, -0.2)]
        .iter()
        .flat_map(|(x, y)| [*x as f32, *y, 0.5].iter().flat_map(|f: &f32| f.to_le_bytes()).collect::<Vec<u8>>())
        .collect();
    let inst: Vec<u8> = [(-0.6f32, RED), (0.0, GREEN), (0.6, BLUE)]
        .iter()
        .flat_map(|(x, c)| {
            let mut v = x.to_le_bytes().to_vec();
            v.extend_from_slice(&0f32.to_le_bytes());
            v.extend_from_slice(&c.to_le_bytes());
            v
        })
        .collect();
    let vb0 = b.buffer(&quad, buffer_usage::VERTEX);
    let vb1 = b.buffer(&inst, buffer_usage::VERTEX);
    let ib: Vec<u8> = [0u16, 1, 2, 0, 2, 3].iter().flat_map(|i| i.to_le_bytes()).collect();
    let ib = b.buffer(&ib, buffer_usage::INDEX);
    b.w.set_stream_source(0, vb0, 0, 12);
    b.w.set_stream_source(1, vb1, 0, 12);
    b.w.set_stream_freq(0, (1 << 30) | 3);
    b.w.set_stream_freq(1, (1 << 31) | 1);
    b.w.set_indices(ib, Format::Index16);
    b.w.draw_indexed(
        PrimitiveType::TriangleList,
        &IndexedDraw { base_vertex: 0, min_index: 0, num_vertices: 4, start_index: 0, prim_count: 2 },
    );
    b.present();
    // Instance centres at NDC x = -0.6, 0, 0.6 -> pixels 12.8, 32, 51.2.
    b.expect(13, 32, R);
    b.expect(32, 32, G);
    b.expect(51, 32, B);
    b.expect(32, 10, K);
}

/// `StretchRect` from a 16 x 16 render target to the whole back buffer.
fn stretch_rect(b: &mut Builder) {
    let rt = b.render_target(Format::X8R8G8B8, 16, 16);
    b.w.set_depth_stencil(Handle::NONE, 0, 0);
    b.w.set_render_target(0, rt, 0, 0);
    b.w.set_viewport(&Viewport { x: 0, y: 0, width: 16, height: 16, min_z: 0.0, max_z: 1.0 });
    b.w.clear(clear::TARGET, RED, 1.0, 0, &[]);
    b.w.clear(clear::TARGET, GREEN, 1.0, 0, &[Rect::new(0, 0, 8, 8)]);
    b.w.stretch_rect(rt, 0, 0, Rect::default(), b.backbuffer, 0, 0, Rect::default(), TextureFilter::Point);
    // And a sub-rectangle: the green corner into the bottom-right 16 x 16.
    b.w.stretch_rect(
        rt,
        0,
        0,
        Rect::new(0, 0, 8, 8),
        b.backbuffer,
        0,
        0,
        Rect::new(48, 48, 64, 64),
        TextureFilter::Point,
    );
    b.present();
    b.expect(16, 16, G);
    b.expect(48, 16, R);
    b.expect(40, 40, R);
    b.expect(56, 56, G);
}

/// The gamma ramp applies when presenting.
fn gamma_ramp(b: &mut Builder) {
    b.w.clear(clear::TARGET, argb(0x20, 0x40, 0x60, 0xff), 1.0, 0, &[]);
    let ramp: [[u16; 256]; 3] = std::array::from_fn(|_| std::array::from_fn(|i| ((255 - i) * 257) as u16));
    b.w.set_gamma_ramp(WINDOW, &ramp);
    b.present();
    b.expect(32, 32, [0xdf, 0xbf, 0x9f, 0xff]);
}

/// Line lists and point lists (one pixel wide).
fn lines_points(b: &mut Builder) {
    b.w.clear(clear::TARGET, BLACK, 1.0, 0, &[]);
    pixel_setup(b);
    let line = [pc(4.0, 20.0, 0.5, RED), pc(60.0, 20.0, 0.5, RED)].concat();
    b.w.draw_up(PrimitiveType::LineList, 1, 16, &line);
    let points = [pc(10.0, 40.0, 0.5, GREEN), pc(50.0, 40.0, 0.5, BLUE)].concat();
    b.w.draw_up(PrimitiveType::PointList, 2, 16, &points);
    let strip = [pc(4.0, 50.0, 0.5, WHITE), pc(30.0, 50.0, 0.5, WHITE), pc(30.0, 60.0, 0.5, WHITE)].concat();
    b.w.draw_up(PrimitiveType::LineStrip, 2, 16, &strip);
    b.present();
    b.expect_any(Rect::new(30, 19, 34, 22), R);
    b.expect_any(Rect::new(9, 39, 12, 42), G);
    b.expect_any(Rect::new(49, 39, 52, 42), B);
    b.expect_any(Rect::new(15, 49, 18, 52), W);
    b.expect_any(Rect::new(29, 54, 32, 57), W);
    b.expect(32, 32, K);
}

/// Cube map sampled by direction.
fn cube_texture(b: &mut Builder) {
    b.w.clear(clear::TARGET, BLACK, 1.0, 0, &[]);
    b.shaders(
        "vs_2_0
        dcl_position v0
        dcl_texcoord v1
        mov oPos, v0
        mov oT0, v1",
        "ps_2_0
        dcl t0
        dcl_cube s0
        texld r0, t0, s0
        mov oC0, r0",
    );
    b.decl(&[
        VertexElement::new(0, 0, DeclType::Float3, DeclUsage::Position, 0),
        VertexElement::new(0, 12, DeclType::Float3, DeclUsage::TexCoord, 0),
    ]);
    let cube = b.handle();
    b.w.create_texture(
        cube,
        &TextureDesc {
            kind: TextureKind::Cube,
            format: Format::A8R8G8B8,
            width: 1,
            height: 1,
            depth: 1,
            levels: 1,
            usage: 0,
        },
    );
    // Faces +X, -X, +Y, -Y, +Z, -Z.
    let colors: [[u8; 4]; 6] = [
        [0, 0, 255, 255],
        [255, 0, 255, 255],
        [0, 255, 0, 255],
        [0, 255, 255, 255],
        [255, 0, 0, 255],
        [255, 255, 255, 255],
    ];
    for (face, c) in colors.iter().enumerate() {
        b.w.write_texture(
            &TextureRegion {
                texture: cube,
                face: face as u32,
                level: 0,
                x: 0,
                y: 0,
                z: 0,
                width: 1,
                height: 1,
                depth: 1,
            },
            4,
            0,
            c,
        );
    }
    b.w.set_texture(0, cube);
    let quad = |x0: f32, x1: f32, dir: [f32; 3]| -> Vec<u8> {
        [(x0, 1.0), (x1, 1.0), (x1, -1.0), (x0, 1.0), (x1, -1.0), (x0, -1.0)]
            .iter()
            .flat_map(|(x, y)| {
                [*x, *y, 0.5, dir[0], dir[1], dir[2]].iter().flat_map(|f| f.to_le_bytes()).collect::<Vec<u8>>()
            })
            .collect()
    };
    b.w.draw_up(PrimitiveType::TriangleList, 2, 24, &quad(-1.0, 0.0, [1.0, 0.0, 0.0]));
    b.w.draw_up(PrimitiveType::TriangleList, 2, 24, &quad(0.0, 1.0, [0.0, 1.0, 0.0]));
    b.present();
    b.expect(16, 32, R);
    b.expect(48, 32, G);
}

/// A D24S8 texture rendered as depth, then sampled as a shadow map: the
/// comparison is `reference <= stored`.
fn shadow_compare(b: &mut Builder) {
    let shadow = b.handle();
    b.w.create_texture(shadow, &TextureDesc::d2(Format::D24S8, 16, 16, 1, texture_usage::DEPTH_STENCIL));
    let small = b.render_target(Format::A8R8G8B8, 16, 16);
    b.w.set_render_target(0, small, 0, 0);
    b.w.set_depth_stencil(shadow, 0, 0);
    b.w.set_viewport(&Viewport { x: 0, y: 0, width: 16, height: 16, min_z: 0.0, max_z: 1.0 });
    b.w.clear(clear::TARGET | clear::ZBUFFER | clear::STENCIL, BLACK, 0.5, 0, &[]);
    b.w.set_render_target(0, b.backbuffer, 0, 0);
    b.w.set_depth_stencil(b.depth, 0, 0);
    b.w.set_viewport(&Viewport { x: 0, y: 0, width: 64, height: 64, min_z: 0.0, max_z: 1.0 });
    b.w.clear(clear::TARGET | clear::ZBUFFER, BLUE, 1.0, 0, &[]);
    b.shaders(
        "vs_2_0
        dcl_position v0
        dcl_texcoord v1
        mov oPos, v0
        mov oT0, v1",
        "ps_2_0
        dcl t0
        dcl_2d s0
        texldp r0, t0, s0
        mov oC0, r0",
    );
    b.decl(&[
        VertexElement::new(0, 0, DeclType::Float3, DeclUsage::Position, 0),
        VertexElement::new(0, 12, DeclType::Float4, DeclUsage::TexCoord, 0),
    ]);
    b.w.set_texture(0, shadow);
    let quad = |x0: f32, x1: f32, refz: f32| -> Vec<u8> {
        [(x0, 1.0), (x1, 1.0), (x1, -1.0), (x0, 1.0), (x1, -1.0), (x0, -1.0)]
            .iter()
            .flat_map(|(x, y)| {
                [*x, *y, 0.5, 0.5, 0.5, refz, 1.0].iter().flat_map(|f| f.to_le_bytes()).collect::<Vec<u8>>()
            })
            .collect()
    };
    b.w.draw_up(PrimitiveType::TriangleList, 2, 28, &quad(-1.0, 0.0, 0.3));
    b.w.draw_up(PrimitiveType::TriangleList, 2, 28, &quad(0.0, 1.0, 0.7));
    b.present();
    b.expect(16, 32, W);
    b.expect(48, 32, [0, 0, 0, 0]);
}

/// `ps_1_4`: `texld` into a temp and `texcrd`, with a constant clamped to
/// [-1, 1] as pixel shader 1.x constants are.
fn ps14(b: &mut Builder) {
    b.w.clear(clear::TARGET, BLACK, 1.0, 0, &[]);
    b.shaders(
        "vs_1_1
        dcl_position v0
        dcl_texcoord v1
        mov oPos, v0
        mov oT0, v1
        mov oT1, v1",
        "ps_1_4
        def c1, 2, 2, 2, 1
        texld r0, t0
        texcrd r1.rgb, t1
        mul r0.rgb, r0, c0
        mul r0.rgb, r0, c1
        +mov r0.a, c1.a",
    );
    b.decl(&DECL_PT);
    b.w.set_shader_const_f(Stage::Pixel, 0, &[[0.5, 0.25, 1.0, 1.0]]);
    let t = b.texture(Format::A8R8G8B8, 1, 1, &[255, 255, 255, 255]);
    b.w.set_texture(0, t);
    let v = [
        pt(-1.0, 1.0, 0.5, 0.0, 0.0),
        pt(1.0, 1.0, 0.5, 1.0, 0.0),
        pt(-1.0, -1.0, 0.5, 0.0, 1.0),
        pt(1.0, -1.0, 0.5, 1.0, 1.0),
    ]
    .concat();
    b.w.draw_up(PrimitiveType::TriangleStrip, 2, 20, &v);
    b.present();
    // c1 = 2 clamps to 1: white * (0.5, 0.25, 1) * 1.
    b.expect(32, 32, [128, 64, 255, 255]);
}

/// P8 textures go through palette 0.
fn palette_p8(b: &mut Builder) {
    let mut pal = [[0u8; 4]; 256];
    pal[1] = [0, 255, 0, 255];
    pal[2] = [255, 0, 255, 255];
    b.w.set_palette(0, &pal);
    b.shaders(VS_TEX, PS_TEX);
    b.decl(&DECL_PT);
    let t = b.texture(Format::P8, 2, 1, &[1, 2]);
    b.w.set_texture(0, t);
    let v = [
        pt(-1.0, 1.0, 0.5, 0.0, 0.0),
        pt(1.0, 1.0, 0.5, 1.0, 0.0),
        pt(-1.0, -1.0, 0.5, 0.0, 1.0),
        pt(1.0, -1.0, 0.5, 1.0, 1.0),
    ]
    .concat();
    b.w.draw_up(PrimitiveType::TriangleStrip, 2, 20, &v);
    b.present();
    b.expect(16, 32, G);
    b.expect(48, 32, [255, 0, 255, 255]);
}

/// A 1 x 1 x 2 volume texture sampled in each slice.
fn volume_texture(b: &mut Builder) {
    b.shaders(
        "vs_2_0
        dcl_position v0
        dcl_texcoord v1
        mov oPos, v0
        mov oT0, v1",
        "ps_2_0
        dcl t0
        dcl_volume s0
        texld r0, t0, s0
        mov oC0, r0",
    );
    b.decl(&[
        VertexElement::new(0, 0, DeclType::Float3, DeclUsage::Position, 0),
        VertexElement::new(0, 12, DeclType::Float3, DeclUsage::TexCoord, 0),
    ]);
    let vol = b.handle();
    b.w.create_texture(
        vol,
        &TextureDesc {
            kind: TextureKind::Volume,
            format: Format::A8R8G8B8,
            width: 1,
            height: 1,
            depth: 2,
            levels: 1,
            usage: 0,
        },
    );
    b.w.write_texture(
        &TextureRegion { texture: vol, face: 0, level: 0, x: 0, y: 0, z: 0, width: 1, height: 1, depth: 2 },
        4,
        4,
        &[[0, 0, 255, 255], [255, 0, 0, 255]].concat(),
    );
    b.w.set_texture(0, vol);
    let quad = |x0: f32, x1: f32, w: f32| -> Vec<u8> {
        [(x0, 1.0), (x1, 1.0), (x1, -1.0), (x0, 1.0), (x1, -1.0), (x0, -1.0)]
            .iter()
            .flat_map(|(x, y)| [*x, *y, 0.5, 0.5, 0.5, w].iter().flat_map(|f| f.to_le_bytes()).collect::<Vec<u8>>())
            .collect()
    };
    b.w.draw_up(PrimitiveType::TriangleList, 2, 24, &quad(-1.0, 0.0, 0.25));
    b.w.draw_up(PrimitiveType::TriangleList, 2, 24, &quad(0.0, 1.0, 0.75));
    b.present();
    b.expect(16, 32, R);
    b.expect(48, 32, B);
}

/// `D3DSHADE_FLAT` takes the colour of the first vertex of each triangle.
fn flat_shading(b: &mut Builder) {
    b.w.clear(clear::TARGET, BLACK, 1.0, 0, &[]);
    color_setup(b);
    b.w.set_render_state(RenderState::ShadeMode, 1);
    let v = [pc(-1.0, -1.0, 0.5, RED), pc(-1.0, 1.0, 0.5, GREEN), pc(1.0, -1.0, 0.5, BLUE)].concat();
    draw_list(b, &v);
    // A fan: the provoking vertex of fan triangle i is vertex i + 1.
    let fan = [pc(1.0, 1.0, 0.5, WHITE), pc(1.0, -1.0, 0.5, YELLOW), pc(-1.0, 1.0, 0.5, BLUE)].concat();
    b.w.draw_up(PrimitiveType::TriangleFan, 1, 16, &fan);
    b.present();
    b.expect(10, 40, R);
    b.expect(54, 24, Y);
}

/// An indexed strip with 32-bit indices, a base vertex and a start index.
fn triangle_strip_indexed(b: &mut Builder) {
    b.w.clear(clear::TARGET, BLACK, 1.0, 0, &[]);
    color_setup(b);
    let mut v = vec![0u8; 16 * 3];
    v.extend(
        [pc(-1.0, 1.0, 0.5, GREEN), pc(1.0, 1.0, 0.5, GREEN), pc(-1.0, -1.0, 0.5, GREEN), pc(1.0, -1.0, 0.5, GREEN)]
            .concat(),
    );
    let vb = b.buffer(&v, buffer_usage::VERTEX);
    let ib: Vec<u8> = [9u32, 9, 0, 1, 2, 3].iter().flat_map(|i| i.to_le_bytes()).collect();
    let ib = b.buffer(&ib, buffer_usage::INDEX);
    b.w.set_stream_source(0, vb, 0, 16);
    b.w.set_indices(ib, Format::Index32);
    b.w.draw_indexed(
        PrimitiveType::TriangleStrip,
        &IndexedDraw { base_vertex: 3, min_index: 0, num_vertices: 4, start_index: 2, prim_count: 2 },
    );
    b.present();
    b.expect(8, 8, G);
    b.expect(56, 56, G);
}

/// Loops, `rep`, calls and predication in a `vs_3_0` / `ps_3_0` pair.
fn flow_control(b: &mut Builder) {
    b.w.clear(clear::TARGET, BLACK, 1.0, 0, &[]);
    b.shaders(
        "vs_3_0
        dcl_position v0
        dcl_position o0
        dcl_texcoord0 o1
        defi i0, 4, 0, 1, 0
        def c0, 0, 0.0625, 1, 0
        mov o0, v0
        mov r0, c0.x
        loop aL, i0
            add r0.x, r0.x, c0.y
        endloop
        call l0
        mov o1, r0
        ret
        label l0
        add r0.y, r0.y, c0.z
        ret",
        "ps_3_0
        dcl_texcoord0 v0
        defi i0, 3, 0, 0, 0
        def c0, 0.25, 0, 1, 0.5
        mov r1, c0.y
        rep i0
            add r1.z, r1.z, c0.x
        endrep
        setp_gt p0.x, v0.y, c0.w
        (p0.x) mov r1.y, c0.z
        mov r1.x, v0.x
        mov r1.w, c0.z
        mov oC0, r1",
    );
    b.decl(&DECL_PC);
    draw_list(b, &rect_clip(-1.0, -1.0, 1.0, 1.0, 0.5, WHITE));
    b.present();
    // r = 4 * 0.0625 = 0.25 -> 64; g = 1 (call ran, predicate set); b = 0.75 -> 191.
    b.expect(32, 32, [64, 255, 191, 255]);
}
