//! Round trips, malformed input, and agreement with the C header.

use d3dgpu_proto::d3d9::*;
use d3dgpu_proto::*;

const HEADER: &str = include_str!("../include/d3dgpu_proto.h");

fn snake_upper(name: &str) -> String {
    let mut s = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_ascii_uppercase() && i > 0 {
            s.push('_');
        }
        s.push(c.to_ascii_uppercase());
    }
    s
}

fn header_value(name: &str) -> Option<u32> {
    for line in HEADER.lines() {
        let line = line.trim();
        let rest = line.strip_prefix(name).filter(|r| r.starts_with(' ') || r.starts_with('=')).or_else(|| {
            line.strip_prefix("#define ").and_then(|l| l.strip_prefix(name)).filter(|r| r.starts_with(' '))
        });
        if let Some(rest) = rest {
            let v = rest.trim_start_matches([' ', '=']).split([',', ' ', '/']).next()?.trim_end_matches('u');
            return if let Some(hex) = v.strip_prefix("0x") {
                u32::from_str_radix(hex, 16).ok()
            } else {
                v.parse().ok()
            };
        }
    }
    None
}

#[test]
fn header_opcodes_match() {
    for op in Op::ALL {
        let name = format!("D3DGPU_OP_{}", snake_upper(op.name()));
        assert_eq!(header_value(&name), Some(*op as u32), "{name}");
    }
    let count = HEADER.lines().filter(|l| l.trim_start().starts_with("D3DGPU_OP_")).count();
    assert_eq!(count, Op::ALL.len(), "header has opcodes the crate lacks");
}

#[test]
fn header_constants_match() {
    assert_eq!(header_value("D3DGPU_MAGIC"), Some(MAGIC));
    assert_eq!(header_value("D3DGPU_VERSION"), Some(VERSION));
    assert_eq!(header_value("D3DGPU_DATA_INLINE"), Some(DATA_INLINE));
    assert_eq!(header_value("D3DGPU_DATA_SHARED"), Some(DATA_SHARED));
    assert_eq!(header_value("D3DGPU_BUFFER_VERTEX"), Some(buffer_usage::VERTEX));
    assert_eq!(header_value("D3DGPU_BUFFER_INDEX"), Some(buffer_usage::INDEX));
    assert_eq!(header_value("D3DGPU_BUFFER_DYNAMIC"), Some(buffer_usage::DYNAMIC));
    assert_eq!(header_value("D3DGPU_TEXTURE_RENDER_TARGET"), Some(texture_usage::RENDER_TARGET));
    assert_eq!(header_value("D3DGPU_TEXTURE_DEPTH_STENCIL"), Some(texture_usage::DEPTH_STENCIL));
    assert_eq!(header_value("D3DGPU_VERTEX_SAMPLER_BASE"), Some(VERTEX_SAMPLER_BASE));
    assert_eq!(header_value("D3DGPU_PRESENT_VSYNC"), Some(present::VSYNC));
}

/// Encodes one command of every kind, for the round trip and the C layout
/// check. Returns (C struct name or "" for variable-size, batch).
fn every_command() -> Vec<(&'static str, Vec<u8>)> {
    let region =
        TextureRegion { texture: Handle(3), face: 1, level: 2, x: 4, y: 5, z: 0, width: 6, height: 7, depth: 1 };
    let mut out = Vec::new();
    let mut one = |name: &'static str, f: &dyn Fn(&mut Writer)| {
        let mut w = Writer::new();
        f(&mut w);
        out.push((name, w.finish()));
    };
    one("d3dgpu_cmd_create_buffer", &|w| w.create_buffer(Handle(1), 4096, buffer_usage::VERTEX));
    one("d3dgpu_cmd_destroy", &|w| w.destroy(Handle(1)));
    one("", &|w| w.write_buffer(Handle(1), 16, &[1u8, 2, 3, 4, 5]));
    one("", &|w| w.write_buffer(Handle(1), 16, DataSrc::Shared { offset: 64, len: 128 }));
    one("d3dgpu_cmd_create_texture", &|w| {
        w.create_texture(Handle(2), &TextureDesc::d2(Format::A8R8G8B8, 64, 32, 1, texture_usage::RENDER_TARGET))
    });
    one("", &|w| w.write_texture(&region, 24, 0, &[9u8; 12]));
    one("", &|w| w.create_shader(Handle(4), Stage::Pixel, 0x1234_5678_9abc_def0, &[0u8, 2, 0xff, 0xff]));
    one("", &|w| {
        w.create_vertex_decl(
            Handle(5),
            &[
                VertexElement::new(0, 0, DeclType::Float3, DeclUsage::Position, 0),
                VertexElement::new(0, 12, DeclType::D3dColor, DeclUsage::Color, 0),
            ],
        )
    });
    one("d3dgpu_cmd_set_palette", &|w| w.set_palette(0, &[[1, 2, 3, 4]; 256]));
    one("d3dgpu_cmd_set_render_target", &|w| w.set_render_target(1, Handle(2), 0, 0));
    one("d3dgpu_cmd_set_depth_stencil", &|w| w.set_depth_stencil(Handle(6), 0, 0));
    one("d3dgpu_cmd_set_viewport", &|w| {
        w.set_viewport(&Viewport { x: 1, y: 2, width: 3, height: 4, min_z: 0.25, max_z: 0.75 })
    });
    one("d3dgpu_cmd_set_scissor", &|w| w.set_scissor(Rect::new(1, 2, 30, 40)));
    one("d3dgpu_cmd_set_render_state", &|w| w.set_render_state(RenderState::CullMode, Cull::None.0));
    one("d3dgpu_cmd_set_sampler_state", &|w| w.set_sampler_state(17, SamplerState::MinFilter, 2));
    one("d3dgpu_cmd_set_texture", &|w| w.set_texture(0, Handle(2)));
    one("d3dgpu_cmd_set_texture_stage_state", &|w| w.set_texture_stage_state(1, TextureStageState::ColorOp, 4));
    one("d3dgpu_cmd_set_object", &|w| w.set_vertex_shader(Handle(7)));
    one("d3dgpu_cmd_set_object", &|w| w.set_pixel_shader(Handle(8)));
    one("d3dgpu_cmd_set_object", &|w| w.set_vertex_decl(Handle(5)));
    one("d3dgpu_cmd_set_stream_source", &|w| w.set_stream_source(1, Handle(1), 32, 28));
    one("d3dgpu_cmd_set_stream_freq", &|w| w.set_stream_freq(0, 0x4000_0004));
    one("d3dgpu_cmd_set_indices", &|w| w.set_indices(Handle(9), Format::Index16));
    one("", &|w| w.set_shader_const_f(Stage::Vertex, 4, &[[1.0, 2.0, 3.0, 4.0], [5.0, 6.0, 7.0, 8.0]]));
    one("", &|w| w.set_shader_const_i(Stage::Pixel, 0, &[[1, -2, 3, 4]]));
    one("", &|w| w.set_shader_const_b(Stage::Pixel, 3, &[true, false, true]));
    one("d3dgpu_cmd_set_clip_plane", &|w| w.set_clip_plane(2, [0.0, 1.0, 0.0, -0.5]));
    one("d3dgpu_cmd_clear", &|w| w.clear(clear::TARGET | clear::ZBUFFER, 0xff10_2030, 1.0, 0, &[]));
    one("", &|w| w.clear(clear::TARGET, 0, 0.5, 7, &[Rect::new(0, 0, 4, 4), Rect::new(8, 8, 9, 9)]));
    one("d3dgpu_cmd_draw", &|w| w.draw(PrimitiveType::TriangleFan, 3, 4));
    one("d3dgpu_cmd_draw_indexed", &|w| {
        w.draw_indexed(
            PrimitiveType::TriangleList,
            &IndexedDraw { base_vertex: -4, min_index: 1, num_vertices: 9, start_index: 6, prim_count: 2 },
        )
    });
    one("", &|w| w.draw_up(PrimitiveType::PointList, 1, 16, &[0u8; 16]));
    one("", &|w| {
        w.draw_indexed_up(PrimitiveType::TriangleList, 0, 3, 1, Format::Index16, &[0u8, 0, 1, 0, 2, 0], 12, &[0u8; 36])
    });
    one("d3dgpu_cmd_stretch_rect", &|w| {
        w.stretch_rect(
            Handle(2),
            0,
            0,
            Rect::new(0, 0, 8, 8),
            Handle(3),
            0,
            1,
            Rect::new(0, 0, 4, 4),
            TextureFilter::Linear,
        )
    });
    one("d3dgpu_cmd_present", &|w| w.present(Handle(2), 1, 0));
    one("d3dgpu_cmd_set_gamma_ramp", &|w| w.set_gamma_ramp(1, &[[0x1234; 256]; 3]));
    one("d3dgpu_cmd_read_texture", &|w| w.read_texture(&region, 1024, 32, 0, 0x1_0000_0002));
    one("d3dgpu_cmd_signal", &|w| w.signal(u64::MAX - 1));
    one("", &|w| w.marker("frame 1"));
    out
}

#[test]
fn every_opcode_is_exercised() {
    let mut seen = std::collections::HashSet::new();
    for (_, b) in every_command() {
        let op = u32::from_le_bytes(b[BATCH_HEADER_SIZE..BATCH_HEADER_SIZE + 4].try_into().unwrap());
        seen.insert(op);
    }
    for op in Op::ALL {
        assert!(seen.contains(&(*op as u32)), "{op:?} not covered");
    }
}

#[test]
fn round_trip_values() {
    let mut w = Writer::new();
    w.create_texture(Handle(2), &TextureDesc::d2(Format::Dxt5, 128, 64, 8, 0));
    w.set_shader_const_f(Stage::Vertex, 4, &[[1.0, 2.0, 3.0, 4.0], [5.0, 6.0, 7.0, 8.0]]);
    w.write_buffer(Handle(1), 16, &[1u8, 2, 3, 4, 5]);
    w.draw_indexed(
        PrimitiveType::TriangleList,
        &IndexedDraw { base_vertex: -4, min_index: 1, num_vertices: 9, start_index: 6, prim_count: 2 },
    );
    w.signal(0xdead_beef_0000_0001);
    w.marker("hello");
    let b = w.finish();
    let cmds: Vec<_> = Reader::new(&b).unwrap().collect::<Result<_, _>>().unwrap();
    assert_eq!(cmds[0], Command::CreateTexture { id: Handle(2), desc: TextureDesc::d2(Format::Dxt5, 128, 64, 8, 0) });
    match &cmds[1] {
        Command::SetShaderConstF { stage: Stage::Vertex, start: 4, count: 2, data } => {
            assert_eq!(f32_at(data, 5), 6.0);
        }
        c => panic!("{c:?}"),
    }
    assert_eq!(cmds[2], Command::WriteBuffer { id: Handle(1), offset: 16, data: Data::Inline(&[1, 2, 3, 4, 5]) });
    assert_eq!(
        cmds[3],
        Command::DrawIndexed {
            prim: PrimitiveType::TriangleList,
            draw: IndexedDraw { base_vertex: -4, min_index: 1, num_vertices: 9, start_index: 6, prim_count: 2 }
        }
    );
    assert_eq!(cmds[4], Command::Signal { fence: 0xdead_beef_0000_0001 });
    assert_eq!(cmds[5], Command::Marker("hello"));
}

#[test]
fn every_command_decodes_and_reencodes_identically() {
    for (_, b) in every_command() {
        let cmds: Vec<_> = Reader::new(&b).unwrap().collect::<Result<_, _>>().unwrap();
        assert_eq!(cmds.len(), 1);
        let again = reencode(&cmds[0]);
        assert_eq!(again, b, "{:?}", cmds[0]);
    }
}

fn data_src<'a>(d: &Data<'a>) -> DataSrc<'a> {
    match *d {
        Data::Inline(b) => DataSrc::Inline(b),
        Data::Shared { offset, len } => DataSrc::Shared { offset, len },
    }
}

fn reencode(c: &Command) -> Vec<u8> {
    let mut w = Writer::new();
    match c {
        Command::CreateBuffer { id, size, usage } => w.create_buffer(*id, *size, *usage),
        Command::Destroy { id } => w.destroy(*id),
        Command::WriteBuffer { id, offset, data } => w.write_buffer(*id, *offset, data_src(data)),
        Command::CreateTexture { id, desc } => w.create_texture(*id, desc),
        Command::WriteTexture { region, row_pitch, slice_pitch, data } => {
            w.write_texture(region, *row_pitch, *slice_pitch, data_src(data))
        }
        Command::CreateShader { id, stage, hash, bytecode } => w.create_shader(*id, *stage, *hash, data_src(bytecode)),
        Command::CreateVertexDecl { id, elements } => w.create_vertex_decl(*id, elements),
        Command::SetPalette { index, entries } => {
            let mut e = [[0u8; 4]; 256];
            for (i, c) in entries.as_chunks::<4>().0.iter().enumerate() {
                e[i].copy_from_slice(c);
            }
            w.set_palette(*index, &e)
        }
        Command::SetRenderTarget { index, texture, face, level } => {
            w.set_render_target(*index, *texture, *face, *level)
        }
        Command::SetDepthStencil { texture, face, level } => w.set_depth_stencil(*texture, *face, *level),
        Command::SetViewport(v) => w.set_viewport(v),
        Command::SetScissor(r) => w.set_scissor(*r),
        Command::SetRenderState { state, value } => w.set_render_state(*state, *value),
        Command::SetSamplerState { sampler, state, value } => w.set_sampler_state(*sampler, *state, *value),
        Command::SetTexture { sampler, texture } => w.set_texture(*sampler, *texture),
        Command::SetTextureStageState { stage, state, value } => w.set_texture_stage_state(*stage, *state, *value),
        Command::SetVertexShader(h) => w.set_vertex_shader(*h),
        Command::SetPixelShader(h) => w.set_pixel_shader(*h),
        Command::SetVertexDecl(h) => w.set_vertex_decl(*h),
        Command::SetStreamSource { stream, buffer, offset, stride } => {
            w.set_stream_source(*stream, *buffer, *offset, *stride)
        }
        Command::SetStreamFreq { stream, value } => w.set_stream_freq(*stream, *value),
        Command::SetIndices { buffer, format } => w.set_indices(*buffer, *format),
        Command::SetShaderConstF { stage, start, count, data } => {
            let v: Vec<[f32; 4]> =
                (0..*count as usize).map(|i| std::array::from_fn(|j| f32_at(data, i * 4 + j))).collect();
            w.set_shader_const_f(*stage, *start, &v)
        }
        Command::SetShaderConstI { stage, start, count, data } => {
            let v: Vec<[i32; 4]> =
                (0..*count as usize).map(|i| std::array::from_fn(|j| u32_at(data, i * 4 + j) as i32)).collect();
            w.set_shader_const_i(*stage, *start, &v)
        }
        Command::SetShaderConstB { stage, start, count, data } => {
            let v: Vec<bool> = (0..*count as usize).map(|i| u32_at(data, i) != 0).collect();
            w.set_shader_const_b(*stage, *start, &v)
        }
        Command::SetClipPlane { index, plane } => w.set_clip_plane(*index, *plane),
        Command::Clear { flags, color, z, stencil, rects } => w.clear(*flags, *color, *z, *stencil, rects),
        Command::Draw { prim, start_vertex, prim_count } => w.draw(*prim, *start_vertex, *prim_count),
        Command::DrawIndexed { prim, draw } => w.draw_indexed(*prim, draw),
        Command::DrawUp { prim, prim_count, stride, vertices } => {
            w.draw_up(*prim, *prim_count, *stride, data_src(vertices))
        }
        Command::DrawIndexedUp {
            prim,
            min_index,
            num_vertices,
            prim_count,
            index_format,
            stride,
            indices,
            vertices,
        } => w.draw_indexed_up(
            *prim,
            *min_index,
            *num_vertices,
            *prim_count,
            *index_format,
            data_src(indices),
            *stride,
            data_src(vertices),
        ),
        Command::StretchRect { src, src_face, src_level, src_rect, dst, dst_face, dst_level, dst_rect, filter } => {
            w.stretch_rect(*src, *src_face, *src_level, *src_rect, *dst, *dst_face, *dst_level, *dst_rect, *filter)
        }
        Command::Present { texture, window, flags } => w.present(*texture, *window, *flags),
        Command::SetGammaRamp { window, ramp } => {
            let mut r = [[0u16; 256]; 3];
            for (i, c) in ramp.as_chunks::<2>().0.iter().enumerate() {
                r[i / 256][i % 256] = u16::from_le_bytes([c[0], c[1]]);
            }
            w.set_gamma_ramp(*window, &r)
        }
        Command::ReadTexture { region, dest_offset, row_pitch, slice_pitch, fence } => {
            w.read_texture(region, *dest_offset, *row_pitch, *slice_pitch, *fence)
        }
        Command::Signal { fence } => w.signal(*fence),
        Command::Marker(s) => w.marker(s),
    }
    w.finish()
}

#[test]
fn rejects_bad_batches() {
    let mut w = Writer::new();
    w.draw(PrimitiveType::TriangleList, 0, 1);
    let good = w.finish();

    let mut bad = good.clone();
    bad[0] ^= 1;
    assert!(matches!(Reader::new(&bad), Err(DecodeError::BadMagic(_))));

    let mut bad = good.clone();
    bad[4] = 99;
    assert!(matches!(Reader::new(&bad), Err(DecodeError::UnsupportedVersion(99))));

    assert!(matches!(Reader::new(&good[..good.len() - 4]), Err(DecodeError::BadLength)));

    // A command whose size runs past the batch.
    let mut bad = good.clone();
    bad[BATCH_HEADER_SIZE + 4] = 200;
    let r: Vec<_> = Reader::new(&bad).unwrap().collect();
    assert!(matches!(r[0], Err(DecodeError::Malformed { .. })));

    // Unknown opcode.
    let mut bad = good.clone();
    bad[BATCH_HEADER_SIZE] = 0xee;
    let r: Vec<_> = Reader::new(&bad).unwrap().collect();
    assert!(matches!(r[0], Err(DecodeError::UnknownOp { op: 0xee, .. })));

    // A truncated payload (size says 8: header only).
    let mut bad = good.clone();
    bad[BATCH_HEADER_SIZE + 4] = 8;
    let r: Vec<_> = Reader::new(&bad).unwrap().collect();
    assert!(matches!(r[0], Err(DecodeError::Malformed { .. })));

    // Inline data that claims more bytes than the command holds.
    let mut w = Writer::new();
    w.write_buffer(Handle(1), 0, &[1u8, 2, 3, 4]);
    let mut bad = w.finish();
    let len_at = BATCH_HEADER_SIZE + 8 + 12;
    bad[len_at] = 64;
    let r: Vec<_> = Reader::new(&bad).unwrap().collect();
    assert!(matches!(r[0], Err(DecodeError::Malformed { .. })));
}

#[test]
fn shared_data_resolves() {
    let shared = [0u8, 1, 2, 3, 4, 5, 6, 7];
    assert_eq!(Data::Shared { offset: 2, len: 3 }.resolve(&shared), Some(&shared[2..5]));
    assert_eq!(Data::Shared { offset: 6, len: 3 }.resolve(&shared), None);
}

#[test]
fn default_states_match_direct3d() {
    let rs = default_render_states();
    assert_eq!(rs[RenderState::CullMode.0 as usize], Cull::Ccw.0);
    assert_eq!(rs[RenderState::ColorWriteEnable.0 as usize], 0xf);
    assert_eq!(f32::from_bits(rs[RenderState::PointSizeMax.0 as usize]), 64.0);
    let ts1 = default_texture_stage_states(1);
    assert_eq!(ts1[TextureStageState::ColorOp.0 as usize], 1); // D3DTOP_DISABLE
}

/// Compiles a C program against the header and compares every fixed
/// command struct's size with what the writer emits. Skipped when no C
/// compiler is installed.
#[test]
fn c_struct_layouts_match_writer() {
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".into());
    let dir = std::env::temp_dir().join(format!("d3dgpu-proto-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let cmds = every_command();
    let mut src = String::from("#include <stdio.h>\n#include \"d3dgpu_proto.h\"\nint main(void){\n");
    for (name, _) in &cmds {
        if !name.is_empty() {
            src += &format!("printf(\"{name} %u\\n\", (unsigned)sizeof(struct {name}));\n");
        }
    }
    src += "printf(\"d3dgpu_batch_header %u\\n\", (unsigned)sizeof(struct d3dgpu_batch_header));\nreturn 0;}\n";
    std::fs::write(dir.join("t.c"), src).unwrap();
    let include = concat!(env!("CARGO_MANIFEST_DIR"), "/include");
    let exe = dir.join("t");
    let status = std::process::Command::new(&cc)
        .args(["-std=c99", "-Wall", "-Werror", "-I", include, "-o"])
        .arg(&exe)
        .arg(dir.join("t.c"))
        .status();
    let Ok(status) = status else {
        eprintln!("no C compiler ({cc}); skipping");
        return;
    };
    assert!(status.success(), "header does not compile");
    let out = String::from_utf8(std::process::Command::new(&exe).output().unwrap().stdout).unwrap();
    let sizes: std::collections::HashMap<&str, usize> = out
        .lines()
        .map(|l| {
            let (n, s) = l.split_once(' ').unwrap();
            (n, s.parse().unwrap())
        })
        .collect();
    assert_eq!(sizes["d3dgpu_batch_header"], BATCH_HEADER_SIZE);
    for (name, batch) in &cmds {
        if name.is_empty() {
            continue;
        }
        assert_eq!(sizes[name], batch.len() - BATCH_HEADER_SIZE, "{name}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
