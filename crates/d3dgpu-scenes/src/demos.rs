//! Animated demos for the browser page and benchmarks: real-looking
//! frames (lit, textured, depth-tested 3D, compute particles, multi-pass
//! post-processing, shadow maps, sprite batches) through both APIs.
//!
//! Shaders are HLSL in `hlsl/demos.hlsl` (shader model 4/5) and
//! `hlsl/demos9.hlsl` (shader model 2/3), compiled by
//! `tools/dxbc/compile.sh crates/d3dgpu-scenes/hlsl`. A demo records a
//! setup batch when created and one batch per frame; frames depend only on
//! the time, so the same demo can be replayed natively and in the browser.

use std::f32::consts::PI;

use d3dgpu_proto::d3d11::*;

use crate::scenes11::view_desc;
use crate::*;

macro_rules! fixture {
    ($name:literal) => {
        include_bytes!(concat!("../fixtures/", $name)).as_slice()
    };
}

/// The tunable size of a demo (cubes, particles, draws, …).
#[derive(Clone, Copy, Debug)]
pub struct Param {
    pub label: &'static str,
    pub default: u32,
    pub min: u32,
    pub max: u32,
}

pub trait Demo {
    /// Records one frame at time `t` (seconds), ending with a present.
    fn frame(&mut self, b: &mut Builder, t: f32);
}

pub struct DemoInfo {
    pub name: &'static str,
    pub api: Api,
    pub about: &'static str,
    pub param: Option<Param>,
    create: fn(&mut Builder, u32) -> Box<dyn Demo>,
}

impl DemoInfo {
    /// A builder for the demo's API with the setup recorded (take it with
    /// [`Builder::take_batch`]), and the demo.
    pub fn start(&self, width: u32, height: u32, param: Option<u32>) -> (Builder, Box<dyn Demo>) {
        let mut b = match self.api {
            Api::D3D9 => Builder::new(width, height),
            Api::D3D11 => Builder::new_d3d11(width, height),
        };
        let p = match self.param {
            Some(spec) => param.unwrap_or(spec.default).clamp(spec.min, spec.max),
            None => 0,
        };
        let demo = (self.create)(&mut b, p);
        (b, demo)
    }
}

const fn param(label: &'static str, default: u32, min: u32, max: u32) -> Option<Param> {
    Some(Param { label, default, min, max })
}

pub const DEMOS: &[DemoInfo] = &[
    DemoInfo {
        name: "cubes11",
        api: Api::D3D11,
        about: "Lit, textured, spinning cubes; one DrawIndexed per cube with its constant buffer rewritten (the Map(WRITE_DISCARD) pattern)",
        param: param("cubes", 500, 1, 20000),
        create: |b, n| Box::new(Cubes11::new(b, n)),
    },
    DemoInfo {
        name: "cubes9",
        api: Api::D3D9,
        about: "The same cubes through Direct3D 9: vs_3_0/ps_3_0 from HLSL, SetVertexShaderConstantF and one DrawIndexedPrimitive per cube",
        param: param("cubes", 500, 1, 20000),
        create: |b, n| Box::new(Cubes9::new(b, n)),
    },
    DemoInfo {
        name: "instanced11",
        api: Api::D3D11,
        about: "One DrawIndexedInstanced of many cubes placed and spun by SV_InstanceID (GPU-bound vertex work)",
        param: param("instances", 20000, 1, 1_000_000),
        create: |b, n| Box::new(Instanced11::new(b, n)),
    },
    DemoInfo {
        name: "particles11",
        api: Api::D3D11,
        about: "A compute shader integrates particles in structured buffers; a vertex shader expands them into additive quads",
        param: param("particles", 65536, 64, 4_194_304),
        create: |b, n| Box::new(Particles11::new(b, n)),
    },
    DemoInfo {
        name: "bloom11",
        api: Api::D3D11,
        about: "HDR (RGBA16F) scene, bright pass and separable blur at half resolution, tone-mapped composite: five passes per frame",
        param: param("cubes", 64, 1, 5000),
        create: |b, n| Box::new(Bloom11::new(b, n)),
    },
    DemoInfo {
        name: "shadows11",
        api: Api::D3D11,
        about: "A depth-only pass into a 1024² typeless depth texture, then 3x3 PCF with a comparison sampler",
        param: param("cubes", 25, 1, 2500),
        create: |b, n| Box::new(Shadows11::new(b, n)),
    },
    DemoInfo {
        name: "sprites9",
        api: Api::D3D9,
        about: "Alpha-blended sprites from DrawPrimitiveUP batches of 100 (2D games, HUDs, particle systems)",
        param: param("sprites", 4000, 1, 200_000),
        create: |b, n| Box::new(Sprites9::new(b, n)),
    },
    DemoInfo {
        name: "perf9",
        api: Api::D3D9,
        about: "The synthetic perf frame: small quads with per-draw constants, texture and blend changes",
        param: param("draws", 2000, 1, 50000),
        create: |b, n| Box::new(Perf9 { assets: perf::setup(b), draws: n, frame: 0 }),
    },
    DemoInfo {
        name: "perf11",
        api: Api::D3D11,
        about: "The synthetic perf frame through Direct3D 11 (constant buffer rewritten per draw)",
        param: param("draws", 2000, 1, 50000),
        create: |b, n| Box::new(Perf11 { assets: perf::setup11(b), draws: n, frame: 0 }),
    },
];

pub fn find(name: &str) -> Option<&'static DemoInfo> {
    DEMOS.iter().find(|d| d.name == name)
}

// ---- Math (row vectors, left-handed, depth 0..1, as Direct3D) ----

pub type Mat4 = [[f32; 4]; 4];
type V3 = [f32; 3];

pub fn mul(a: &Mat4, b: &Mat4) -> Mat4 {
    let mut r = [[0.0; 4]; 4];
    for i in 0..4 {
        for j in 0..4 {
            r[i][j] = (0..4).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    r
}

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn normalize(a: V3) -> V3 {
    let l = dot(a, a).sqrt();
    [a[0] / l, a[1] / l, a[2] / l]
}

pub fn look_at(eye: V3, at: V3, up: V3) -> Mat4 {
    let z = normalize(sub(at, eye));
    let x = normalize(cross(up, z));
    let y = cross(z, x);
    [
        [x[0], y[0], z[0], 0.0],
        [x[1], y[1], z[1], 0.0],
        [x[2], y[2], z[2], 0.0],
        [-dot(x, eye), -dot(y, eye), -dot(z, eye), 1.0],
    ]
}

pub fn perspective(fov_y: f32, aspect: f32, near: f32, far: f32) -> Mat4 {
    let ys = 1.0 / (fov_y / 2.0).tan();
    let q = far / (far - near);
    [[ys / aspect, 0.0, 0.0, 0.0], [0.0, ys, 0.0, 0.0], [0.0, 0.0, q, 1.0], [0.0, 0.0, -near * q, 0.0]]
}

pub fn ortho(w: f32, h: f32, near: f32, far: f32) -> Mat4 {
    let q = 1.0 / (far - near);
    [[2.0 / w, 0.0, 0.0, 0.0], [0.0, 2.0 / h, 0.0, 0.0], [0.0, 0.0, q, 0.0], [0.0, 0.0, -near * q, 1.0]]
}

pub fn rotation_y(a: f32) -> Mat4 {
    let (s, c) = a.sin_cos();
    [[c, 0.0, -s, 0.0], [0.0, 1.0, 0.0, 0.0], [s, 0.0, c, 0.0], [0.0, 0.0, 0.0, 1.0]]
}

pub fn rotation_x(a: f32) -> Mat4 {
    let (s, c) = a.sin_cos();
    [[1.0, 0.0, 0.0, 0.0], [0.0, c, s, 0.0], [0.0, -s, c, 0.0], [0.0, 0.0, 0.0, 1.0]]
}

pub fn scale_translate(s: V3, t: V3) -> Mat4 {
    [[s[0], 0.0, 0.0, 0.0], [0.0, s[1], 0.0, 0.0], [0.0, 0.0, s[2], 0.0], [t[0], t[1], t[2], 1.0]]
}

fn mat_bytes(m: &Mat4) -> Vec<u8> {
    m.iter().flatten().flat_map(|f| f.to_le_bytes()).collect()
}

fn f4(v: [f32; 4]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

/// A camera orbiting the origin.
fn orbit(t: f32, radius: f32, height: f32, aspect: f32) -> (Mat4, V3) {
    let a = t * 0.25;
    let eye = [a.sin() * radius, height, a.cos() * radius];
    let view = look_at(eye, [0.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
    let far = radius * 4.0 + 50.0;
    (mul(&view, &perspective(PI / 3.0, aspect, 0.1, far)), eye)
}

/// A colour from a hue in [0, 1).
fn hue(h: f32) -> [f32; 4] {
    let c = |o: f32| (((h + o).fract() * 6.0 - 3.0).abs() - 1.0).clamp(0.0, 1.0);
    [c(0.0), c(2.0 / 3.0), c(1.0 / 3.0), 1.0]
}

// ---- Geometry ----

const VERTEX_STRIDE: u32 = 32;

/// A unit cube (-1..1): 24 vertices of position, normal and uv; 36
/// 16-bit indices, clockwise from outside (Direct3D's front faces).
pub fn cube() -> (Vec<u8>, Vec<u8>) {
    let faces: [(V3, V3); 6] = [
        ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
        ([-1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
        ([0.0, 0.0, 1.0], [0.0, 1.0, 0.0]),
        ([0.0, 0.0, -1.0], [0.0, 1.0, 0.0]),
        ([0.0, 1.0, 0.0], [0.0, 0.0, 1.0]),
        ([0.0, -1.0, 0.0], [0.0, 0.0, -1.0]),
    ];
    let mut v = Vec::new();
    let mut idx: Vec<u16> = Vec::new();
    for (f, (n, up)) in faces.iter().enumerate() {
        let r = cross(*n, *up);
        let corners = [(-1.0, 1.0, 0.0, 0.0), (1.0, 1.0, 1.0, 0.0), (1.0, -1.0, 1.0, 1.0), (-1.0, -1.0, 0.0, 1.0)];
        for (cr, cu, u, w) in corners {
            let p = [n[0] + r[0] * cr + up[0] * cu, n[1] + r[1] * cr + up[1] * cu, n[2] + r[2] * cr + up[2] * cu];
            for x in p.iter().chain(n.iter()).chain([u, w].iter()) {
                v.extend_from_slice(&x.to_le_bytes());
            }
        }
        let base = (f * 4) as u16;
        idx.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    (v, idx.iter().flat_map(|i| i.to_le_bytes()).collect())
}

/// A light/dark checker with a border, RGBA8, and its mip chain.
fn checker_mips(size: u32) -> Vec<Vec<u8>> {
    let mut level: Vec<u8> = Vec::new();
    for y in 0..size {
        for x in 0..size {
            let edge = x % 16 == 0 || y % 16 == 0;
            let on = ((x / 16) + (y / 16)) % 2 == 0;
            let v = if edge {
                90
            } else if on {
                245
            } else {
                190
            };
            level.extend_from_slice(&[v, v, v, 255]);
        }
    }
    let mut out = vec![level];
    let mut s = size;
    while s > 1 {
        let prev = out.last().unwrap();
        let n = s / 2;
        let mut next = Vec::with_capacity((n * n * 4) as usize);
        for y in 0..n {
            for x in 0..n {
                for c in 0..4 {
                    let at = |xx: u32, yy: u32| prev[((yy * s + xx) * 4 + c) as usize] as u32;
                    let sum = at(2 * x, 2 * y) + at(2 * x + 1, 2 * y) + at(2 * x, 2 * y + 1) + at(2 * x + 1, 2 * y + 1);
                    next.push((sum / 4) as u8);
                }
            }
        }
        out.push(next);
        s = n;
    }
    out
}

fn mesh_layout() -> Vec<InputElement> {
    let el = |semantic: &str, format, offset| InputElement {
        semantic: semantic.into(),
        semantic_index: 0,
        format,
        slot: 0,
        offset,
        per_instance: false,
        step_rate: 0,
    };
    vec![
        el("POSITION", DxgiFormat::R32G32B32Float, 0),
        el("NORMAL", DxgiFormat::R32G32B32Float, 12),
        el("TEXCOORD", DxgiFormat::R32G32Float, 24),
    ]
}

/// What most Direct3D 11 demos share: the cube, the checker texture, a
/// linear sampler and the frame/object constant buffers.
struct Common11 {
    vb: Handle,
    ib: Handle,
    checker: Handle,
    frame_cb: Handle,
    object_cb: Handle,
}

const FRAME_CB_SIZE: usize = 2 * 64 + 3 * 16;

impl Common11 {
    fn new(b: &mut Builder) -> Common11 {
        let (verts, indices) = cube();
        let vb = b.buffer11(bind::VERTEX_BUFFER, &verts);
        let ib = b.buffer11(bind::INDEX_BUFFER, &indices);
        let mips = checker_mips(64);
        let t = b.texture11(&Texture11Desc::d2(
            DxgiFormat::R8G8B8A8Unorm,
            64,
            64,
            mips.len() as u32,
            bind::SHADER_RESOURCE,
        ));
        for (i, m) in mips.iter().enumerate() {
            b.w.update_subresource(t, i as u32, None, (64 >> i).max(1) * 4, 0, m.as_slice());
        }
        let checker = b.view(ViewKind::ShaderResource, t, DxgiFormat::R8G8B8A8Unorm);
        let s = b.handle();
        b.w.create_sampler(
            s,
            &SamplerDesc11 { filter: filter::ANISOTROPIC, max_anisotropy: 8, address: [1; 3], ..Default::default() },
        );
        for stage in [Stage11::Pixel, Stage11::Vertex] {
            b.w.set_samplers(stage, 0, &[s]);
        }
        let frame_cb = b.handle();
        b.w.create_buffer11(frame_cb, FRAME_CB_SIZE as u32, bind::CONSTANT_BUFFER, 0, 0);
        let object_cb = b.handle();
        b.w.create_buffer11(object_cb, 80, bind::CONSTANT_BUFFER, 0, 0);
        for stage in [Stage11::Vertex, Stage11::Pixel] {
            b.set_cb(stage, 0, frame_cb);
            b.set_cb(stage, 1, object_cb);
        }
        Common11 { vb, ib, checker, frame_cb, object_cb }
    }

    fn bind_mesh(&self, b: &mut Builder) {
        b.input_layout(&mesh_layout());
        b.vb(0, self.vb, VERTEX_STRIDE);
        b.w.set_index_buffer(self.ib, DxgiFormat::R16Uint, 0);
        b.w.set_primitive_topology(4);
        b.w.set_shader_resources(Stage11::Pixel, 0, &[self.checker]);
    }

    fn set_frame(&self, b: &mut Builder, view_proj: &Mat4, light_view_proj: &Mat4, light: V3, eye: V3, p: [f32; 4]) {
        let mut d = mat_bytes(view_proj);
        d.extend(mat_bytes(light_view_proj));
        d.extend(f4([light[0], light[1], light[2], 0.0]));
        d.extend(f4([eye[0], eye[1], eye[2], 1.0]));
        d.extend(f4(p));
        b.w.update_subresource(self.frame_cb, 0, None, 0, 0, d.as_slice());
    }

    fn draw_object(&self, b: &mut Builder, world: &Mat4, color: [f32; 4]) {
        let mut d = mat_bytes(world);
        d.extend(f4(color));
        b.w.update_subresource(self.object_cb, 0, None, 0, 0, d.as_slice());
        b.w.draw_indexed11(36, 0, 0, 1, 0);
    }
}

fn aspect(b: &Builder) -> f32 {
    b.width as f32 / b.height as f32
}

const LIGHT: V3 = [-0.4, -1.0, 0.3];

/// Cube `i` of a spinning grid of `n`.
fn grid_cube(i: u32, n: u32, t: f32) -> (Mat4, [f32; 4]) {
    let side = (n as f32).sqrt().ceil() as u32;
    let (gx, gz) = (i % side, i / side);
    let off = (side as f32 - 1.0) * 0.5;
    let pos = [(gx as f32 - off) * 2.6, ((t * 1.3 + i as f32 * 0.7).sin()) * 0.3, (gz as f32 - off) * 2.6];
    let spin = t * (0.6 + (i % 5) as f32 * 0.25) + i as f32;
    let world = mul(&mul(&rotation_x(spin * 0.7), &rotation_y(spin)), &scale_translate([0.8, 0.8, 0.8], pos));
    (world, hue(i as f32 * 0.618034))
}

fn grid_radius(n: u32) -> f32 {
    (n as f32).sqrt().ceil() * 2.6 * 0.75 + 4.0
}

// ---- Demos ----

struct Cubes11 {
    c: Common11,
    n: u32,
}

impl Cubes11 {
    fn new(b: &mut Builder, n: u32) -> Cubes11 {
        let c = Common11::new(b);
        b.shaders11(fixture!("demos.vs_mesh.vs_4_0.dxbc"), fixture!("demos.ps_mesh.ps_4_0.dxbc"));
        c.bind_mesh(b);
        Cubes11 { c, n }
    }
}

impl Demo for Cubes11 {
    fn frame(&mut self, b: &mut Builder, t: f32) {
        b.w.clear_render_target_view(b.rtv, [0.05, 0.06, 0.09, 1.0]);
        b.w.clear_depth_stencil_view(b.dsv, clear11::DEPTH, 1.0, 0);
        let r = grid_radius(self.n);
        let (vp, eye) = orbit(t, r, r * 0.6, aspect(b));
        self.c.set_frame(b, &vp, &vp, LIGHT, eye, [t, 0.0, aspect(b), 0.0]);
        for i in 0..self.n {
            let (world, color) = grid_cube(i, self.n, t);
            self.c.draw_object(b, &world, color);
        }
        b.present();
    }
}

struct Cubes9 {
    n: u32,
}

impl Cubes9 {
    fn new(b: &mut Builder, n: u32) -> Cubes9 {
        use d3dgpu_proto::d3d9::*;
        let vs = b.shader_bytes(Stage::Vertex, fixture!("demos9.vs9_mesh.vs_3_0.d3dbc"));
        let ps = b.shader_bytes(Stage::Pixel, fixture!("demos9.ps9_mesh.ps_3_0.d3dbc"));
        b.w.set_vertex_shader(vs);
        b.w.set_pixel_shader(ps);
        b.decl(&[
            VertexElement::new(0, 0, DeclType::Float3, DeclUsage::Position, 0),
            VertexElement::new(0, 12, DeclType::Float3, DeclUsage::Normal, 0),
            VertexElement::new(0, 24, DeclType::Float2, DeclUsage::TexCoord, 0),
        ]);
        let (verts, indices) = cube();
        let vb = b.buffer(&verts, buffer_usage::VERTEX);
        let ib = b.buffer(&indices, buffer_usage::INDEX);
        b.w.set_stream_source(0, vb, 0, VERTEX_STRIDE);
        b.w.set_indices(ib, Format::Index16);
        let mips = checker_mips(64);
        let tex = b.texture(Format::A8B8G8R8, 64, 64, &mips[0]);
        b.w.set_texture(0, tex);
        for (s, v) in
            [(SamplerState::MagFilter, TextureFilter::Linear.0), (SamplerState::MinFilter, TextureFilter::Linear.0)]
        {
            b.w.set_sampler_state(0, s, v);
        }
        Cubes9 { n }
    }
}

impl Demo for Cubes9 {
    fn frame(&mut self, b: &mut Builder, t: f32) {
        use d3dgpu_proto::d3d9::*;
        b.w.clear(clear::TARGET | clear::ZBUFFER, argb(13, 15, 23, 255), 1.0, 0, &[]);
        let r = grid_radius(self.n);
        let (vp, eye) = orbit(t, r, r * 0.6, aspect(b));
        b.w.set_shader_const_f(Stage::Vertex, 0, &vp);
        b.w.set_shader_const_f(Stage::Pixel, 8, &[[LIGHT[0], LIGHT[1], LIGHT[2], 0.0], [eye[0], eye[1], eye[2], 1.0]]);
        for i in 0..self.n {
            let (world, color) = grid_cube(i, self.n, t);
            b.w.set_shader_const_f(Stage::Vertex, 4, &world);
            b.w.set_shader_const_f(Stage::Pixel, 10, &[color]);
            b.w.draw_indexed(
                PrimitiveType::TriangleList,
                &IndexedDraw { base_vertex: 0, min_index: 0, num_vertices: 24, start_index: 0, prim_count: 12 },
            );
        }
        b.present();
    }
}

struct Instanced11 {
    c: Common11,
    n: u32,
}

impl Instanced11 {
    fn new(b: &mut Builder, n: u32) -> Instanced11 {
        let c = Common11::new(b);
        b.shaders11(fixture!("demos.vs_inst.vs_4_0.dxbc"), fixture!("demos.ps_mesh.ps_4_0.dxbc"));
        c.bind_mesh(b);
        Instanced11 { c, n }
    }
}

impl Demo for Instanced11 {
    fn frame(&mut self, b: &mut Builder, t: f32) {
        b.w.clear_render_target_view(b.rtv, [0.04, 0.04, 0.06, 1.0]);
        b.w.clear_depth_stencil_view(b.dsv, clear11::DEPTH, 1.0, 0);
        let side = (self.n as f32).cbrt().ceil();
        let r = side * 2.0 * 1.1 + 3.0;
        let (vp, eye) = orbit(t, r, r * 0.5, aspect(b));
        self.c.set_frame(b, &vp, &vp, LIGHT, eye, [t, side, aspect(b), 0.0]);
        b.w.draw_indexed11(36, 0, 0, self.n, 0);
        b.present();
    }
}

struct Particles11 {
    c: Common11,
    n: u32,
    pos_uav: Handle,
    vel_uav: Handle,
    pos_srv: Handle,
    sim_cb: Handle,
    cs: Handle,
    vs_mesh: Handle,
    ps_mesh: Handle,
    vs_part: Handle,
    ps_part: Handle,
    additive: Handle,
    no_depth: Handle,
    no_cull: Handle,
    last: Option<f32>,
}

impl Particles11 {
    fn new(b: &mut Builder, n: u32) -> Particles11 {
        let c = Common11::new(b);
        // WebGPU zero-initializes buffers: every particle starts dead and
        // the first dispatch respawns it (no megabytes of initial data in
        // the setup batch).
        let bytes = n * 16;
        let mk = |b: &mut Builder| {
            let h = b.handle();
            b.w.create_buffer11(h, bytes, bind::UNORDERED_ACCESS | bind::SHADER_RESOURCE, misc::BUFFER_STRUCTURED, 16);
            h
        };
        let pos_buf = mk(b);
        let vel_buf = mk(b);
        let mut d = view_desc(DxgiFormat::Unknown, ViewDim::Buffer);
        d.num_elements = n;
        let pos_uav = b.view_with(ViewKind::UnorderedAccess, pos_buf, &d);
        let vel_uav = b.view_with(ViewKind::UnorderedAccess, vel_buf, &d);
        let pos_srv = b.view_with(ViewKind::ShaderResource, pos_buf, &d);
        let sim_cb = b.handle();
        b.w.create_buffer11(sim_cb, 16, bind::CONSTANT_BUFFER, 0, 0);
        let cs = b.shader11(Stage11::Compute, fixture!("demos.cs_particles.cs_5_0.dxbc"));
        let vs_mesh = b.shader11(Stage11::Vertex, fixture!("demos.vs_mesh.vs_4_0.dxbc"));
        let ps_mesh = b.shader11(Stage11::Pixel, fixture!("demos.ps_mesh.ps_4_0.dxbc"));
        let vs_part = b.shader11(Stage11::Vertex, fixture!("demos.vs_particles.vs_5_0.dxbc"));
        let ps_part = b.shader11(Stage11::Pixel, fixture!("demos.ps_particles.ps_4_0.dxbc"));
        let additive = b.handle();
        let mut bd = BlendDesc11::default();
        bd.targets[0] =
            RtBlend { enable: true, src: 2, dst: 2, op: 1, src_alpha: 2, dst_alpha: 2, op_alpha: 1, write_mask: 0xf };
        b.w.create_blend_state(additive, &bd);
        let no_depth = b.handle();
        b.w.create_depth_stencil_state(no_depth, &DepthStencilDesc11 { depth_write: false, ..Default::default() });
        let no_cull = b.handle();
        b.w.create_rasterizer_state(no_cull, &RasterizerDesc11 { cull: cull::NONE, ..Default::default() });
        Particles11 {
            c,
            n,
            pos_uav,
            vel_uav,
            pos_srv,
            sim_cb,
            cs,
            vs_mesh,
            ps_mesh,
            vs_part,
            ps_part,
            additive,
            no_depth,
            no_cull,
            last: None,
        }
    }
}

impl Demo for Particles11 {
    fn frame(&mut self, b: &mut Builder, t: f32) {
        let dt = self.last.map(|l| (t - l).clamp(0.0, 0.05)).unwrap_or(0.016);
        self.last = Some(t);
        // Simulate.
        b.w.update_subresource(self.sim_cb, 0, None, 0, 0, f4([dt, t, self.n as f32, 0.0]).as_slice());
        b.w.set_shader11(Stage11::Compute, self.cs);
        b.set_cb(Stage11::Compute, 0, self.sim_cb);
        b.w.set_unordered_access_views(Stage11::Compute, 0, &[self.pos_uav, self.vel_uav]);
        b.w.dispatch(self.n.div_ceil(64), 1, 1);
        b.w.set_unordered_access_views(Stage11::Compute, 0, &[Handle::NONE, Handle::NONE]);
        // A floor of cubes, then the particles.
        b.w.clear_render_target_view(b.rtv, [0.02, 0.02, 0.04, 1.0]);
        b.w.clear_depth_stencil_view(b.dsv, clear11::DEPTH, 1.0, 0);
        let (vp, eye) = orbit(t, 9.0, 3.0, aspect(b));
        self.c.set_frame(b, &vp, &vp, LIGHT, eye, [t, 0.0, aspect(b), 0.0]);
        b.w.set_shader11(Stage11::Vertex, self.vs_mesh);
        b.w.set_shader11(Stage11::Pixel, self.ps_mesh);
        b.w.set_blend_state(Handle::NONE, [1.0; 4], !0);
        b.w.set_depth_stencil_state(Handle::NONE, 0);
        b.w.set_rasterizer_state(Handle::NONE);
        self.c.bind_mesh(b);
        for i in 0..9 {
            let (x, z) = ((i % 3) as f32 - 1.0, (i / 3) as f32 - 1.0);
            let world = scale_translate([1.4, 0.15, 1.4], [x * 3.0, -3.2, z * 3.0]);
            self.c.draw_object(b, &world, [0.35, 0.35, 0.4, 1.0]);
        }
        b.w.set_shader11(Stage11::Vertex, self.vs_part);
        b.w.set_shader11(Stage11::Pixel, self.ps_part);
        b.w.set_input_layout(Handle::NONE);
        b.w.set_shader_resources(Stage11::Vertex, 0, &[self.pos_srv]);
        b.w.set_blend_state(self.additive, [1.0; 4], !0);
        b.w.set_depth_stencil_state(self.no_depth, 0);
        b.w.set_rasterizer_state(self.no_cull);
        b.w.draw11(self.n * 6, 0, 1, 0);
        b.w.set_shader_resources(Stage11::Vertex, 0, &[Handle::NONE]);
        b.present();
    }
}

struct Bloom11 {
    c: Common11,
    n: u32,
    hdr_rtv: Handle,
    hdr_srv: Handle,
    half: [(Handle, Handle); 2],
    post_cb: Handle,
    vs_mesh: Handle,
    ps_mesh: Handle,
    vs_post: Handle,
    ps_bright: Handle,
    ps_blur: Handle,
    ps_composite: Handle,
    linear: Handle,
}

impl Bloom11 {
    fn new(b: &mut Builder, n: u32) -> Bloom11 {
        let c = Common11::new(b);
        let rt = |b: &mut Builder, w: u32, h: u32| {
            let t = b.texture11(&Texture11Desc::d2(
                DxgiFormat::R16G16B16A16Float,
                w,
                h,
                1,
                bind::RENDER_TARGET | bind::SHADER_RESOURCE,
            ));
            (
                b.view(ViewKind::RenderTarget, t, DxgiFormat::R16G16B16A16Float),
                b.view(ViewKind::ShaderResource, t, DxgiFormat::R16G16B16A16Float),
            )
        };
        let (w, h) = (b.width, b.height);
        let (hdr_rtv, hdr_srv) = rt(b, w, h);
        let half = [rt(b, w / 2, h / 2), rt(b, w / 2, h / 2)];
        let post_cb = b.handle();
        b.w.create_buffer11(post_cb, 16, bind::CONSTANT_BUFFER, 0, 0);
        let linear = b.handle();
        b.w.create_sampler(linear, &SamplerDesc11 { filter: filter::MIN_MAG_MIP_LINEAR, ..Default::default() });
        Bloom11 {
            c,
            n,
            hdr_rtv,
            hdr_srv,
            half,
            post_cb,
            vs_mesh: b.shader11(Stage11::Vertex, fixture!("demos.vs_mesh.vs_4_0.dxbc")),
            ps_mesh: b.shader11(Stage11::Pixel, fixture!("demos.ps_mesh.ps_4_0.dxbc")),
            vs_post: b.shader11(Stage11::Vertex, fixture!("demos.vs_post.vs_4_0.dxbc")),
            ps_bright: b.shader11(Stage11::Pixel, fixture!("demos.ps_bright.ps_4_0.dxbc")),
            ps_blur: b.shader11(Stage11::Pixel, fixture!("demos.ps_blur.ps_4_0.dxbc")),
            ps_composite: b.shader11(Stage11::Pixel, fixture!("demos.ps_composite.ps_4_0.dxbc")),
            linear,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn post(&self, b: &mut Builder, rtv: Handle, w: u32, h: u32, ps: Handle, srvs: &[Handle], params: [f32; 4]) {
        b.w.set_render_targets11(&[rtv], Handle::NONE);
        b.w.set_viewports(&[Viewport11 {
            x: 0.0,
            y: 0.0,
            width: w as f32,
            height: h as f32,
            min_depth: 0.0,
            max_depth: 1.0,
        }]);
        b.w.update_subresource(self.post_cb, 0, None, 0, 0, f4(params).as_slice());
        b.w.set_shader11(Stage11::Pixel, ps);
        b.w.set_shader_resources(Stage11::Pixel, 0, srvs);
        b.w.draw11(3, 0, 1, 0);
        b.w.set_shader_resources(Stage11::Pixel, 0, &[Handle::NONE, Handle::NONE]);
    }
}

impl Demo for Bloom11 {
    fn frame(&mut self, b: &mut Builder, t: f32) {
        let (w, h) = (b.width, b.height);
        // The scene, in HDR: a ring of cubes, some glowing.
        b.w.set_render_targets11(&[self.hdr_rtv], b.dsv);
        b.full_viewport();
        b.w.clear_render_target_view(self.hdr_rtv, [0.01, 0.01, 0.02, 1.0]);
        b.w.clear_depth_stencil_view(b.dsv, clear11::DEPTH, 1.0, 0);
        let (vp, eye) = orbit(t * 0.6, 14.0, 6.0, aspect(b));
        self.c.set_frame(b, &vp, &vp, LIGHT, eye, [t, 0.0, aspect(b), 0.0]);
        b.w.set_shader11(Stage11::Vertex, self.vs_mesh);
        b.w.set_shader11(Stage11::Pixel, self.ps_mesh);
        for stage in [Stage11::Vertex, Stage11::Pixel] {
            b.set_cb(stage, 0, self.c.frame_cb);
        }
        self.c.bind_mesh(b);
        b.w.set_samplers(Stage11::Pixel, 0, &[self.linear]);
        for i in 0..self.n {
            let a = i as f32 / self.n as f32 * 2.0 * PI + t * 0.3;
            let ring = 3.0 + (i % 4) as f32 * 2.0;
            let pos = [a.cos() * ring, (t * 2.0 + i as f32).sin() * 1.5, a.sin() * ring];
            let world = mul(&rotation_y(t + i as f32), &scale_translate([0.5, 0.5, 0.5], pos));
            let glow = if i % 5 == 0 { 6.0 } else { 1.0 };
            let c = hue(i as f32 * 0.13);
            self.c.draw_object(b, &world, [c[0] * glow, c[1] * glow, c[2] * glow, 1.0]);
        }
        // Bright pass and blur at half resolution, then the composite.
        b.w.set_shader11(Stage11::Vertex, self.vs_post);
        b.w.set_input_layout(Handle::NONE);
        b.set_cb(Stage11::Pixel, 0, self.post_cb);
        let (hw, hh) = (w / 2, h / 2);
        self.post(b, self.half[0].0, hw, hh, self.ps_bright, &[self.hdr_srv], [0.0; 4]);
        self.post(b, self.half[1].0, hw, hh, self.ps_blur, &[self.half[0].1], [1.5 / hw as f32, 0.0, 0.0, 0.0]);
        self.post(b, self.half[0].0, hw, hh, self.ps_blur, &[self.half[1].1], [0.0, 1.5 / hh as f32, 0.0, 0.0]);
        self.post(b, b.rtv, w, h, self.ps_composite, &[self.hdr_srv, self.half[0].1], [0.0, 0.0, 0.9, 0.0]);
        b.w.set_samplers(Stage11::Pixel, 0, &[Handle::NONE]);
        b.present();
    }
}

struct Shadows11 {
    c: Common11,
    n: u32,
    shadow_dsv: Handle,
    shadow_srv: Handle,
    vs_shadow: Handle,
    vs_lit: Handle,
    ps_lit: Handle,
    biased: Handle,
}

const SHADOW_SIZE: u32 = 1024;

impl Shadows11 {
    fn new(b: &mut Builder, n: u32) -> Shadows11 {
        let c = Common11::new(b);
        let t = b.texture11(&Texture11Desc::d2(
            DxgiFormat::R32Typeless,
            SHADOW_SIZE,
            SHADOW_SIZE,
            1,
            bind::DEPTH_STENCIL | bind::SHADER_RESOURCE,
        ));
        let shadow_dsv = b.view(ViewKind::DepthStencil, t, DxgiFormat::D32Float);
        let shadow_srv = b.view(ViewKind::ShaderResource, t, DxgiFormat::R32Float);
        let cmp = b.handle();
        b.w.create_sampler(cmp, &SamplerDesc11 { filter: 0x95, comparison: 4, address: [3; 3], ..Default::default() });
        b.w.set_samplers(Stage11::Pixel, 1, &[cmp]);
        let biased = b.handle();
        b.w.create_rasterizer_state(
            biased,
            &RasterizerDesc11 { depth_bias: 64, slope_scaled_depth_bias: 1.5, ..Default::default() },
        );
        Shadows11 {
            c,
            n,
            shadow_dsv,
            shadow_srv,
            vs_shadow: b.shader11(Stage11::Vertex, fixture!("demos.vs_shadow.vs_4_0.dxbc")),
            vs_lit: b.shader11(Stage11::Vertex, fixture!("demos.vs_lit.vs_4_0.dxbc")),
            ps_lit: b.shader11(Stage11::Pixel, fixture!("demos.ps_lit.ps_4_0.dxbc")),
            biased,
        }
    }

    fn objects(&self, b: &mut Builder, t: f32) {
        self.c.draw_object(b, &scale_translate([12.0, 0.2, 12.0], [0.0, -1.2, 0.0]), [0.8, 0.8, 0.85, 1.0]);
        let side = (self.n as f32).sqrt().ceil() as u32;
        for i in 0..self.n {
            let (gx, gz) = (i % side, i / side);
            let off = (side as f32 - 1.0) * 0.5;
            let spacing = 9.0 / side as f32;
            let pos = [
                (gx as f32 - off) * spacing,
                0.4 + (t * 1.7 + i as f32).sin().abs() * 1.2,
                (gz as f32 - off) * spacing,
            ];
            let s = (spacing * 0.3).min(0.6);
            let world = mul(&rotation_y(t * 0.8 + i as f32), &scale_translate([s, s, s], pos));
            self.c.draw_object(b, &world, hue(i as f32 * 0.618034));
        }
    }
}

impl Demo for Shadows11 {
    fn frame(&mut self, b: &mut Builder, t: f32) {
        let la = t * 0.2;
        let light = normalize([la.cos() * 0.6, -1.0, la.sin() * 0.6]);
        let lpos = [-light[0] * 20.0, -light[1] * 20.0, -light[2] * 20.0];
        let lvp = mul(&look_at(lpos, [0.0; 3], [0.0, 0.0, 1.0]), &ortho(28.0, 28.0, 1.0, 45.0));
        let (vp, eye) = orbit(t * 0.5, 15.0, 9.0, aspect(b));
        self.c.set_frame(b, &vp, &lvp, light, eye, [t, 0.0, aspect(b), 0.0]);
        self.c.bind_mesh(b);
        // Depth from the light: no render target, no pixel shader.
        b.w.set_shader_resources(Stage11::Pixel, 1, &[Handle::NONE]);
        b.w.set_render_targets11(&[], self.shadow_dsv);
        let s = SHADOW_SIZE as f32;
        b.w.set_viewports(&[Viewport11 { x: 0.0, y: 0.0, width: s, height: s, min_depth: 0.0, max_depth: 1.0 }]);
        b.w.clear_depth_stencil_view(self.shadow_dsv, clear11::DEPTH, 1.0, 0);
        b.w.set_shader11(Stage11::Vertex, self.vs_shadow);
        b.w.set_shader11(Stage11::Pixel, Handle::NONE);
        b.w.set_rasterizer_state(self.biased);
        self.objects(b, t);
        // The lit pass.
        b.w.set_rasterizer_state(Handle::NONE);
        b.w.set_render_targets11(&[b.rtv], b.dsv);
        b.full_viewport();
        b.w.clear_render_target_view(b.rtv, [0.45, 0.6, 0.8, 1.0]);
        b.w.clear_depth_stencil_view(b.dsv, clear11::DEPTH, 1.0, 0);
        b.w.set_shader11(Stage11::Vertex, self.vs_lit);
        b.w.set_shader11(Stage11::Pixel, self.ps_lit);
        b.w.set_shader_resources(Stage11::Pixel, 1, &[self.shadow_srv]);
        self.objects(b, t);
        b.present();
    }
}

struct Sprites9 {
    n: u32,
}

impl Sprites9 {
    fn new(b: &mut Builder, n: u32) -> Sprites9 {
        use d3dgpu_proto::d3d9::*;
        let vs = b.shader_bytes(Stage::Vertex, fixture!("demos9.vs9_sprite.vs_2_0.d3dbc"));
        let ps = b.shader_bytes(Stage::Pixel, fixture!("demos9.ps9_sprite.ps_2_0.d3dbc"));
        b.w.set_vertex_shader(vs);
        b.w.set_pixel_shader(ps);
        b.decl(&[
            VertexElement::new(0, 0, DeclType::Float2, DeclUsage::Position, 0),
            VertexElement::new(0, 8, DeclType::D3dColor, DeclUsage::Color, 0),
            VertexElement::new(0, 12, DeclType::Float2, DeclUsage::TexCoord, 0),
        ]);
        // A soft round sprite.
        let mut tex = Vec::with_capacity(64 * 64 * 4);
        for y in 0..64 {
            for x in 0..64 {
                let (dx, dy) = ((x as f32 - 31.5) / 32.0, (y as f32 - 31.5) / 32.0);
                let a = (1.0 - (dx * dx + dy * dy)).max(0.0);
                tex.extend_from_slice(&[255, 255, 255, (a * a * 255.0) as u8]);
            }
        }
        let t = b.texture(Format::A8B8G8R8, 64, 64, &tex);
        b.w.set_texture(0, t);
        for (s, v) in [
            (SamplerState::MagFilter, TextureFilter::Linear.0),
            (SamplerState::MinFilter, TextureFilter::Linear.0),
            (SamplerState::AddressU, TextureAddress::Clamp.0),
            (SamplerState::AddressV, TextureAddress::Clamp.0),
        ] {
            b.w.set_sampler_state(0, s, v);
        }
        b.w.set_render_state(RenderState::ZEnable, 0);
        b.w.set_render_state(RenderState::CullMode, Cull::None.0);
        b.w.set_render_state(RenderState::AlphaBlendEnable, 1);
        b.w.set_render_state(RenderState::SrcBlend, Blend::SrcAlpha.0);
        b.w.set_render_state(RenderState::DestBlend, Blend::One.0);
        let (w, h) = (b.width as f32, b.height as f32);
        b.w.set_shader_const_f(Stage::Vertex, 11, &[[2.0 / w, -2.0 / h, 0.0, 0.0]]);
        Sprites9 { n }
    }
}

impl Demo for Sprites9 {
    fn frame(&mut self, b: &mut Builder, t: f32) {
        use d3dgpu_proto::d3d9::*;
        b.w.clear(clear::TARGET, argb(4, 4, 10, 255), 1.0, 0, &[]);
        let (w, h) = (b.width as f32, b.height as f32);
        let size = (w.min(h) * 0.05).max(4.0);
        let mut batch = Vec::with_capacity(100 * 6 * 20);
        let flush = |b: &mut Builder, batch: &mut Vec<u8>| {
            if !batch.is_empty() {
                b.w.draw_up(PrimitiveType::TriangleList, batch.len() as u32 / 60, 20, batch.as_slice());
                batch.clear();
            }
        };
        for i in 0..self.n {
            let f = i as f32;
            let a = t * (0.3 + (i % 13) as f32 * 0.05) + f * 2.399;
            let r = 0.15 + 0.85 * ((f * 0.618034).fract());
            let x = w * 0.5 + a.cos() * r * w * 0.45 * (1.0 + 0.2 * (t + f).sin());
            let y = h * 0.5 + (a * 1.3).sin() * r * h * 0.45;
            let c = hue(f * 0.07 + t * 0.05);
            let col = argb((c[0] * 255.0) as u8, (c[1] * 255.0) as u8, (c[2] * 255.0) as u8, 160);
            let corners = [(-1.0, -1.0, 0.0, 0.0), (1.0, -1.0, 1.0, 0.0), (1.0, 1.0, 1.0, 1.0), (-1.0, 1.0, 0.0, 1.0)];
            for k in [0, 1, 2, 0, 2, 3] {
                let (cx, cy, u, v) = corners[k];
                for val in [x + cx * size, y + cy * size] {
                    batch.extend_from_slice(&f32::to_le_bytes(val));
                }
                batch.extend_from_slice(&col.to_le_bytes());
                for val in [u, v] {
                    batch.extend_from_slice(&f32::to_le_bytes(val));
                }
            }
            if batch.len() >= 100 * 6 * 20 {
                flush(b, &mut batch);
            }
        }
        flush(b, &mut batch);
        b.present();
    }
}

struct Perf9 {
    assets: perf::Assets,
    draws: u32,
    frame: u32,
}

impl Demo for Perf9 {
    fn frame(&mut self, b: &mut Builder, _t: f32) {
        perf::frame(b, &self.assets, self.draws, self.frame);
        self.frame += 1;
    }
}

struct Perf11 {
    assets: perf::Assets11,
    draws: u32,
    frame: u32,
}

impl Demo for Perf11 {
    fn frame(&mut self, b: &mut Builder, _t: f32) {
        perf::frame11(b, &self.assets, self.draws, self.frame);
        self.frame += 1;
    }
}
