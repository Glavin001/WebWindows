//! Translator tests: every shader in the corpus must assemble, survive a
//! disassemble/assemble round trip unchanged, and translate under several
//! variant keys into WGSL that Naga accepts with only the capabilities
//! browsers ship.

use d3dgpu_shader::asm::{assemble, disassemble};
use d3dgpu_shader::*;

/// Shaders written the way fxc and D3DXAssembleShader accept them. Between
/// them they use every opcode the translator handles.
const CORPUS: &[(&str, &str)] = &[
    (
        "vs_1_1 transform and light",
        "vs_1_1
        dcl_position v0
        dcl_normal v1
        dcl_texcoord v2
        dcl_color v3
        def c90, 0, 0.5, 1, 2
        m4x4 oPos, v0, c0
        dp3 r0.x, v1, c4
        max r0.x, r0.x, c90.x
        mul oD0, v3, r0.x
        mov oD1, c90.x
        mov oT0.xy, v2
        mov oT1, v2.yxzw
        dp4 r1.x, v0, c2
        mov oFog, r1.x
        mov oPts, c90.z",
    ),
    (
        "vs_1_1 every arithmetic",
        "vs_1_1
        dcl_position v0
        def c95, 1, 2, 3, 4
        mov r0, v0
        add r1, r0, c95
        sub r1, r1, -r0
        mad r2, r0, c95.x, r1
        mul r2, r2, r2
        rcp r3.x, r2.x
        rsq r3.y, r2.y
        dp3 r4, r0, r1
        dp4 r4.w, r0, r1
        min r5, r0, r1
        max r5, r5, r2
        slt r6, r0, r1
        sge r6, r0, r1
        exp r7, r0.x
        expp r7, r0.y
        log r8, r0.z
        logp r8, r0.w
        lit r9, r0
        dst r10, r0, r1
        frc r11, r0
        m4x3 r1.xyz, r0, c10
        m3x4 r2, r0, c10
        m3x3 r3.xyz, r0, c10
        m3x2 r4.xy, r0, c10
        mov a0.x, r0.x
        mov r5, c[a0.x + 20]
        mov r6, c[a0.x]
        add oPos, r5, r6",
    ),
    (
        "vs_2_0 relative and flow",
        "vs_2_0
        dcl_position v0
        dcl_blendindices v1
        defi i0, 4, 0, 1, 0
        defb b0, true
        def c200, 3, 0, 0, 0
        mova a0.x, v1.x
        mov r0, c[a0.x + 10]
        mov r1, c0
        rep i0
            add r1, r1, r0
        endrep
        if b0
            mul r1, r1, c200.x
        else
            mov r1, -r1
        endif
        if b1
            mov r1, c1
        endif
        call l0
        callnz l1, b2
        add oPos, r1, v0
        ret
        label l0
        mov r0, c5
        ret
        label l1
        pow r0, r0.x, r1.y
        crs r2.xyz, r0, r1
        sgn r3, r0, r1, r2
        abs r4, r0
        nrm r5, r0
        sincos r6.xy, r0.x, c6, c7
        lrp r7, r0, r1, r2
        ret",
    ),
    (
        "vs_3_0 outputs, loop, predicate, texldl",
        "vs_3_0
        dcl_position v0
        dcl_texcoord0 v1
        dcl_texcoord1 v2
        dcl_position o0
        dcl_texcoord0 o1
        dcl_color0 o2
        dcl_fog o3.x
        dcl_2d s0
        defi i0, 3, 1, 2, 0
        def c250, 0, 1, 0, 0
        mov r0, c250.x
        loop aL, i0
            add r0, r0, c[aL]
            break_gt r0.x, c250.y
        endloop
        setp_lt p0, r0, c250
        (p0) mov r0, c250.y
        (!p0.x) add r0.x, r0.x, r0.y
        if p0.y
            mov r0.y, c250.x
        endif
        if_ge r0.x, c250.y
            mov r0.z, c250.y
        endif
        rep i0
            breakp p0.z
            break_ne r0.x, r0.y
            break
        endrep
        texldl r1, v2, s0
        mov o0, v0
        mov o1, v1
        mov o2, r1
        mov o3.x, r0.x",
    ),
    (
        "ps_1_1 texture blending",
        "ps_1_1
        def c0, 0.5, 0.5, 0.5, 1
        tex t0
        tex t1
        texcoord t2
        mul r0, t0, v0
        mad r0.rgb, t1, c0, r0
        +mov r0.a, t0.a
        add_sat r1, r0, -v1
        lrp r0, c0.a, r0, r1
        dp3_x2 r1, t0_bx2, t1_bx2
        sub r0, r0, r1_bias
        cnd r0, r0.a, t0, 1-t1
        mul_x4 r0, r0, c0
        mul_d2 r0, r0, r0",
    ),
    (
        "ps_1_1 kill",
        "ps_1_1
        texkill t0
        mov r0, v0",
    ),
    (
        "ps_1_3 texture addressing",
        "ps_1_3
        tex t0
        texbem t1, t0
        texbeml t2, t0
        texreg2ar t3, t2
        mov r0, t3",
    ),
    (
        "ps_1_3 matrix addressing",
        "ps_1_3
        def c0, 0, 0, 1, 0
        tex t0
        texm3x3pad t1, t0_bx2
        texm3x3pad t2, t0_bx2
        texm3x3spec t3, t0_bx2, c0
        mov r0, t3",
    ),
    (
        "ps_1_2 vspec and dp3",
        "ps_1_2
        tex t0
        texm3x3pad t1, t0
        texm3x3pad t2, t0
        texm3x3vspec t3, t0
        mul r0, t3, v0",
    ),
    (
        "ps_1_3 m3x2 and depth",
        "ps_1_3
        tex t0
        texm3x2pad t1, t0
        texm3x2tex t2, t0
        texdp3tex t3, t0
        mul r0, t2, t3",
    ),
    (
        "ps_1_3 m3x2depth",
        "ps_1_3
        tex t0
        texm3x2pad t1, t0
        texm3x2depth t2, t0
        mov r0, t0",
    ),
    (
        "ps_1_2 texreg and m3x3",
        "ps_1_2
        tex t0
        texreg2gb t1, t0
        texreg2rgb t2, t0
        texdp3 t3, t0
        mul r0, t1, t2
        add r0, r0, t3",
    ),
    (
        "ps_1_2 m3x3 plain",
        "ps_1_2
        tex t0
        texm3x3pad t1, t0
        texm3x3pad t2, t0
        texm3x3 t3, t0
        mov r0, t3",
    ),
    (
        "ps_1_1 cube environment",
        "ps_1_1
        tex t0
        texm3x3pad t1, t0_bx2
        texm3x3pad t2, t0_bx2
        texm3x3tex t3, t0_bx2
        nop
        mov r0, t3",
    ),
    (
        "ps_1_4 phases",
        "ps_1_4
        def c0, 1, 0, 0, 1
        texld r0, t0
        texcrd r1.rgb, t1
        texld r2, t2_dz
        bem r3.rg, r0, r1
        phase
        texld r3, r3
        texdepth r5
        cmp r0, r0, r2, r3
        cnd r0, r1, r0, c0
        mul r0, r0, v0",
    ),
    (
        "ps_2_0 sampling",
        "ps_2_0
        dcl t0.xy
        dcl t1
        dcl_centroid t2
        dcl v0
        dcl_2d s0
        dcl_cube s1
        dcl_volume s2
        def c0, 0.5, 0.5, 0, 0
        texld r0, t0, s0
        texldp r1, t1, s0
        texldb r2, t1, s0
        texld r3, t1, s1
        texld r4, t1, s2
        dp2add r5, r0, r1, c0.x
        cmp r6, r0, r1, r2
        mad r0, r0, v0, r3
        add r0, r0, r4
        rcp r1.x, r5.x
        mul r0, r0, r1.x
        texkill t1
        mov oC0, r0
        mov oC1, r6
        mov oDepth, r5.x",
    ),
    (
        "ps_2_x flow and derivatives",
        "ps_2_x
        dcl t0
        dcl_2d s0
        defb b0, false
        defi i0, 2, 0, 0, 0
        def c0, 0, 0.25, 0, 0
        mov r0, c0.x
        rep i0
            if b0
                texld r1, t0, s0
            endif
            add r0, r0, c0.y
        endrep
        dsx r2, t0
        dsy r3, t0
        texldd r4, t0, s0, r2, r3
        if_lt r0.x, c0.y
            mov r0, r4
        endif
        mov oC0, r0",
    ),
    (
        "ps_3_0 inputs, vPos, vFace, predication",
        "ps_3_0
        dcl_texcoord0 v0
        dcl_texcoord1_centroid v1.xy
        dcl_color0 v2
        dcl vPos.xy
        dcl vFace
        dcl_2d s0
        def c0, 0, 1, 0.5, 0
        texld r0, v0, s0
        setp_gt p0, vFace, c0.x
        (p0) mul r0, r0, v2
        mov r1.xy, vPos
        mul r1.xy, r1, c0.z
        loop aL, i0
            add r0, r0, v[aL]
        endloop
        callnz l2, !p0.x
        mov_sat oC0, r0
        mov oC3, r1
        ret
        label l2
        mov r0, c0.y
        ret",
    ),
];

fn validate(src: &str, caps: naga::valid::Capabilities) -> naga::Module {
    let module = match naga::front::wgsl::parse_str(src) {
        Ok(m) => m,
        Err(e) => panic!("WGSL parse error:\n{}\n{}", e.emit_to_string(src), numbered(src)),
    };
    if let Err(e) = naga::valid::Validator::new(naga::valid::ValidationFlags::all(), caps).validate(&module) {
        panic!("WGSL validation error:\n{}\n{}", e.emit_to_string(src), numbered(src));
    }
    module
}

fn numbered(src: &str) -> String {
    src.lines().enumerate().map(|(i, l)| format!("{:4} {l}\n", i + 1)).collect()
}

fn module_of(text: &str) -> ShaderModule {
    let tokens = assemble(text).unwrap_or_else(|e| panic!("{e}\n{text}"));
    ShaderModule::new(&tokens).unwrap()
}

#[test]
fn corpus_round_trips_through_text() {
    for (name, text) in CORPUS {
        let tokens = assemble(text).unwrap_or_else(|e| panic!("{name}: {e}"));
        let shader = parse(&tokens).unwrap_or_else(|e| panic!("{name}: {e}"));
        let text2 = disassemble(&shader);
        let tokens2 = assemble(&text2).unwrap_or_else(|e| panic!("{name}: re-assembling: {e}\n{text2}"));
        assert_eq!(tokens, tokens2, "{name}:\n{text2}");
        assert_eq!(encode(&shader), tokens, "{name}: encode");
    }
}

#[test]
fn corpus_translates_and_validates() {
    let caps = naga::valid::Capabilities::empty();
    for (name, text) in CORPUS {
        let m = module_of(text);
        let out = match m.stage() {
            Stage::Vertex => {
                // Feed every output the vertex shader writes, as a pixel
                // shader reading all of them would.
                let mut key = VertexKey { pos_fixup: true, ..Default::default() };
                key.outputs = m
                    .reflection
                    .vs_outputs
                    .iter()
                    .filter(|s| s.usage != bytecode::usage::POSITION)
                    .map(|s| Varying { semantic: *s, interp: Interp::Perspective })
                    .collect();
                m.vertex(&key)
            }
            Stage::Pixel => m.pixel(&PixelKey::default()),
        };
        let out = out.unwrap_or_else(|e| panic!("{name}: {e}"));
        validate(&out.wgsl, caps);
    }
}

#[test]
fn pixel_variants_validate() {
    let caps = naga::valid::Capabilities::empty();
    let keys = [
        PixelKey { alpha_test: cmp::GREATER_EQUAL, fog: Fog::Vertex, ..Default::default() },
        PixelKey { alpha_test: cmp::NEVER, fog: Fog::Linear, flat_shading: true, ..Default::default() },
        PixelKey { fog: Fog::Exp, clip: ClipMode::Varying(0b101011), ..Default::default() },
        PixelKey { fog: Fog::Exp2, ..Default::default() },
        {
            let mut k = PixelKey::default();
            k.samplers[0] = SamplerKey { depth: true, compare: true, ..SamplerKey::d2() };
            k.samplers[1] = SamplerKey { swizzle: [0, 0, 0, 5], projected: true, ..SamplerKey::d2() };
            k.samplers[2] = SamplerKey { dim: SamplerDim::Cube, swizzle: [4, 4, 4, 0], ..SamplerKey::d2() };
            k.samplers[3] = SamplerKey { dim: SamplerDim::Volume, ..SamplerKey::d2() };
            k
        },
        {
            let mut k = PixelKey::default();
            k.samplers[0] = SamplerKey { depth: true, compare: false, ..SamplerKey::d2() };
            k
        },
    ];
    for (name, text) in CORPUS.iter().filter(|(_, t)| t.trim_start().starts_with("ps")) {
        let m = module_of(text);
        for key in &keys {
            let out = m.pixel(key).unwrap_or_else(|e| panic!("{name}: {e}"));
            validate(&out.wgsl, caps);
        }
    }
}

#[test]
fn vertex_variants_validate() {
    let text = "vs_3_0
        dcl_position v0
        dcl_blendindices v1
        dcl_color v2
        dcl_normal v3
        dcl_tangent v4
        dcl_position o0
        dcl_texcoord0 o1
        mov o0, v0
        add o1, v1, v2
        add o1, o1, v3
        add o1, o1, v4";
    let m = module_of(text);
    let mut inputs = [InputKind::Float; 16];
    inputs[1] = InputKind::Uint;
    inputs[2] = InputKind::FloatBgra;
    inputs[3] = InputKind::Dec3n;
    inputs[4] = InputKind::Udec3;
    let tex0 = Varying { semantic: Semantic::new(bytecode::usage::TEXCOORD, 0), interp: Interp::Centroid };
    let col0 = Varying { semantic: Semantic::new(bytecode::usage::COLOR, 0), interp: Interp::Flat };
    let base = VertexKey { inputs, outputs: vec![tex0, col0], pos_fixup: true, ..Default::default() };
    validate(&m.vertex(&base).unwrap().wgsl, naga::valid::Capabilities::empty());
    let varying = VertexKey { clip: ClipMode::Varying(0b111111), ..base.clone() };
    validate(&m.vertex(&varying).unwrap().wgsl, naga::valid::Capabilities::empty());
    let builtin = VertexKey { clip: ClipMode::Builtin(0b11), ..base.clone() };
    let out = m.vertex(&builtin).unwrap();
    assert!(out.wgsl.starts_with("enable clip_distances;"));
    validate(&out.wgsl, naga::valid::Capabilities::CLIP_DISTANCES);
    let mut sint = base.clone();
    sint.inputs[1] = InputKind::Sint;
    validate(&m.vertex(&sint).unwrap().wgsl, naga::valid::Capabilities::empty());
}

/// Encodings checked against fxc / D3DXAssembleShader output as quoted in
/// public test suites.
#[test]
fn known_encodings() {
    assert_eq!(
        assemble("vs_2_0\ndcl_position v0\nm4x4 oPos, v0, c0").unwrap(),
        vec![
            0xfffe0200, 0x0200001f, 0x80000000, 0x900f0000, 0x03000014, 0xc00f0000, 0x90e40000, 0xa0e40000, 0x0000ffff
        ]
    );
    assert_eq!(
        assemble("ps_2_0\ndcl t0\ndcl_2d s0\ntexld r0, t0, s0\nmov oC0, r0").unwrap(),
        vec![
            0xffff0200, 0x0200001f, 0x80000000, 0xb00f0000, 0x0200001f, 0x90000000, 0xa00f0800, 0x03000042, 0x800f0000,
            0xb0e40000, 0xa0e40800, 0x02000001, 0x800f0800, 0x80e40000, 0x0000ffff
        ]
    );
    // ps_1_1: no length field; def is followed by raw floats.
    assert_eq!(
        assemble("ps_1_1\ndef c0, 1.0, 0.0, 0.0, 1.0\ntex t0\nmul r0, t0, c0").unwrap(),
        vec![
            0xffff0101, 0x00000051, 0xa00f0000, 0x3f800000, 0x00000000, 0x00000000, 0x3f800000, 0x00000042, 0xb00f0000,
            0x00000005, 0x800f0000, 0xb0e40000, 0xa0e40000, 0x0000ffff
        ]
    );
    // vs_1_1 relative addressing has no address token.
    let rel = assemble("vs_1_1\nmov r0, c[a0.x + 3]").unwrap();
    assert_eq!(rel, vec![0xfffe0101, 0x00000001, 0x800f0000, 0xa0e42003, 0x0000ffff]);
    // vs_2_0 relative addressing does.
    let rel = assemble("vs_2_0\nmov r0, c[a0.x + 3]").unwrap();
    assert_eq!(rel, vec![0xfffe0200, 0x03000001, 0x800f0000, 0xa0e42003, 0xb0000000, 0x0000ffff]);
}

#[test]
fn comments_are_skipped() {
    let mut tokens = assemble("ps_2_0\nmov oC0, c0").unwrap();
    // A two-token comment (as fxc writes for the constant table).
    tokens.splice(1..1, [0x0002fffe, 0x42415443, 0x0000001c]);
    let s = parse(&tokens).unwrap();
    assert_eq!(s.instructions.len(), 1);
}

#[test]
fn bad_bytecode_is_rejected() {
    assert!(parse(&[]).is_err());
    assert!(parse(&[0x12345678, 0xffff]).is_err());
    assert!(parse(&[0xfffe0400, 0xffff]).is_err(), "shader model 4 is not SM1-3");
    assert!(parse(&[0xffff0200, 0x03000002, 0x800f0000]).is_err(), "truncated");
    assert!(parse(&[0xffff0200, 0x00000077, 0x0000ffff]).is_err(), "unknown opcode");
    assert!(parse(&[0xffff0200]).is_err(), "no end token");
}

#[test]
fn reflection() {
    let vs = module_of(CORPUS[0].1);
    let r = &vs.reflection;
    assert_eq!(r.vs_inputs.len(), 4);
    assert!(r.vs_outputs.contains(&Semantic::new(bytecode::usage::COLOR, 0)));
    assert!(r.vs_outputs.contains(&Semantic::new(bytecode::usage::TEXCOORD, 1)));
    assert!(r.vs_outputs.contains(&Semantic::new(bytecode::usage::FOG, 0)));
    assert_eq!(r.float_consts, 91);

    let ps = module_of(CORPUS.iter().find(|(n, _)| n.starts_with("ps_2_0")).unwrap().1);
    let r = &ps.reflection;
    assert_eq!(r.samplers.len(), 3);
    assert_eq!(r.color_outputs, 0b11);
    assert!(r.writes_depth && r.uses_discard);
    let sems: Vec<_> = r.ps_inputs.iter().map(|v| (v.semantic.usage, v.semantic.index, v.interp)).collect();
    assert_eq!(
        sems,
        vec![
            (bytecode::usage::COLOR, 0, Interp::Perspective),
            (bytecode::usage::TEXCOORD, 0, Interp::Perspective),
            (bytecode::usage::TEXCOORD, 1, Interp::Perspective),
            (bytecode::usage::TEXCOORD, 2, Interp::Centroid),
        ]
    );

    let ps11 = module_of(CORPUS.iter().find(|(n, _)| *n == "ps_1_1 texture blending").unwrap().1);
    let sems: Vec<_> = ps11.reflection.ps_inputs.iter().map(|v| (v.semantic.usage, v.semantic.index)).collect();
    assert_eq!(
        sems,
        vec![
            (bytecode::usage::COLOR, 0),
            (bytecode::usage::COLOR, 1),
            (bytecode::usage::TEXCOORD, 0),
            (bytecode::usage::TEXCOORD, 1),
            (bytecode::usage::TEXCOORD, 2),
        ]
    );
    assert_eq!(ps11.reflection.samplers.iter().map(|s| s.0).collect::<Vec<_>>(), vec![0, 1]);

    // Vertex fog adds a FOG varying after the shader's own.
    let key = PixelKey { fog: Fog::Vertex, flat_shading: true, ..Default::default() };
    let link = ps11.linkage(&key);
    assert_eq!(link.last().unwrap().semantic, Semantic::new(bytecode::usage::FOG, 0));
    assert_eq!(link[0].interp, Interp::Flat);
}

#[test]
fn every_opcode_is_covered_by_the_corpus() {
    let mut seen = std::collections::HashSet::new();
    for (_, text) in CORPUS {
        for ins in parse(&assemble(text).unwrap()).unwrap().instructions {
            seen.insert(ins.opcode);
        }
    }
    let missing: Vec<_> = Opcode::ALL.iter().filter(|o| !seen.contains(o)).collect();
    assert!(missing.is_empty(), "opcodes without a corpus shader: {missing:?}");
}
