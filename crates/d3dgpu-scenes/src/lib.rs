//! Test scenes for the d3dgpu render core.
//!
//! Each scene is written against the protocol's builder API only, so the
//! same scene runs in `cargo test` on native wgpu and on the browser demo
//! page. A scene records its commands and what the result must look like:
//! pixel probes on the presented frame and bytes expected in shared memory
//! after readbacks. Expected values come from Direct3D 9's rules (half-pixel
//! centres, D3DCOLOR byte order, fog and blend formulas), worked out by hand
//! in the comments.

use d3dgpu_proto::d3d9::*;
use d3dgpu_proto::*;

pub mod perf;
mod scenes;

pub use scenes::ALL;

/// The window id scenes present to.
pub const WINDOW: u32 = 1;

/// A named scene.
pub struct Scene {
    pub name: &'static str,
    pub build: fn(&mut Builder),
}

/// What to check after running a scene.
#[derive(Clone, Debug, PartialEq)]
pub enum Expect {
    /// Every pixel of `rect` in the presented frame is `rgba` (± tolerance).
    Pixels { rect: Rect, rgba: [u8; 4], tolerance: u8 },
    /// At least one pixel of `rect` is `rgba` (± tolerance): for lines and
    /// points, whose exact pixels vary between rasterizers.
    AnyPixel { rect: Rect, rgba: [u8; 4], tolerance: u8 },
    /// Shared memory at `offset` holds `bytes` once all fences completed.
    Shared { offset: u32, bytes: Vec<u8> },
}

/// A built scene.
pub struct Built {
    pub name: &'static str,
    pub width: u32,
    pub height: u32,
    /// Command batches, executed in order.
    pub batches: Vec<Vec<u8>>,
    /// Size of the shared memory region the scene reads back into.
    pub shared_size: usize,
    pub expect: Vec<Expect>,
}

/// Records a scene: commands plus expectations. Starts with an
/// `A8R8G8B8` back buffer and a cleared `D24S8` depth buffer bound, a full
/// viewport, and Direct3D's default state.
pub struct Builder {
    pub w: Writer,
    next: u32,
    pub width: u32,
    pub height: u32,
    pub backbuffer: Handle,
    pub depth: Handle,
    batches: Vec<Vec<u8>>,
    expect: Vec<Expect>,
    pub shared_size: usize,
    fence: u64,
}

impl Builder {
    pub fn new(width: u32, height: u32) -> Builder {
        let mut b = Builder {
            w: Writer::new(),
            next: 1,
            width,
            height,
            backbuffer: Handle::NONE,
            depth: Handle::NONE,
            batches: Vec::new(),
            expect: Vec::new(),
            shared_size: 0,
            fence: 0,
        };
        b.backbuffer = b.render_target(Format::A8R8G8B8, width, height);
        b.depth = b.handle();
        b.w.create_texture(b.depth, &TextureDesc::d2(Format::D24S8, width, height, 1, texture_usage::DEPTH_STENCIL));
        b.w.set_render_target(0, b.backbuffer, 0, 0);
        b.w.set_depth_stencil(b.depth, 0, 0);
        b.w.set_viewport(&Viewport { x: 0, y: 0, width, height, min_z: 0.0, max_z: 1.0 });
        // Depth buffers start undefined in Direct3D; applications clear them.
        b.w.clear(clear::ZBUFFER | clear::STENCIL, 0, 1.0, 0, &[]);
        b
    }

    pub fn handle(&mut self) -> Handle {
        self.next += 1;
        Handle(self.next)
    }

    pub fn render_target(&mut self, format: Format, width: u32, height: u32) -> Handle {
        let h = self.handle();
        self.w.create_texture(h, &TextureDesc::d2(format, width, height, 1, texture_usage::RENDER_TARGET));
        h
    }

    /// Assembles and registers a shader; the stage comes from its version line.
    pub fn shader(&mut self, text: &str) -> Handle {
        let tokens = d3dgpu_shader::asm::assemble(text).unwrap_or_else(|e| panic!("{e}\n{text}"));
        let stage = if text.trim_start().starts_with("vs") { Stage::Vertex } else { Stage::Pixel };
        let bytes: Vec<u8> = tokens.iter().flat_map(|t| t.to_le_bytes()).collect();
        let h = self.handle();
        let hash = bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ *b as u64).wrapping_mul(0x100_0000_01b3));
        self.w.create_shader(h, stage, hash, &bytes);
        h
    }

    /// Assembles and binds a vertex and pixel shader pair.
    pub fn shaders(&mut self, vs: &str, ps: &str) {
        let (v, p) = (self.shader(vs), self.shader(ps));
        self.w.set_vertex_shader(v);
        self.w.set_pixel_shader(p);
    }

    pub fn decl(&mut self, elements: &[VertexElement]) -> Handle {
        let h = self.handle();
        self.w.create_vertex_decl(h, elements);
        self.w.set_vertex_decl(h);
        h
    }

    pub fn buffer(&mut self, data: &[u8], usage: u32) -> Handle {
        let h = self.handle();
        self.w.create_buffer(h, data.len() as u32, usage);
        self.w.write_buffer(h, 0, data);
        h
    }

    /// A 2D texture with one level of `data` (rows tightly packed).
    pub fn texture(&mut self, format: Format, width: u32, height: u32, data: &[u8]) -> Handle {
        let h = self.handle();
        self.w.create_texture(h, &TextureDesc::d2(format, width, height, 1, 0));
        let pitch = format.row_bytes(width).unwrap();
        self.w.write_texture(
            &TextureRegion { texture: h, face: 0, level: 0, x: 0, y: 0, z: 0, width, height, depth: 1 },
            pitch,
            0,
            data,
        );
        h
    }

    /// Sets `c0` to the pixel-to-clip transform [`VS_PIXEL`] uses.
    pub fn pixel_space(&mut self) {
        let (w, h) = (self.width as f32, self.height as f32);
        self.w.set_shader_const_f(Stage::Vertex, 0, &[[2.0 / w, -2.0 / h, -1.0, 1.0]]);
    }

    /// Ends the current batch; later commands go into a new one.
    pub fn split(&mut self) {
        let w = std::mem::take(&mut self.w);
        self.batches.push(w.finish());
    }

    pub fn present(&mut self) {
        self.w.present(self.backbuffer, WINDOW, 0);
    }

    /// Reads a region back into shared memory and expects `bytes` there.
    pub fn read_back(&mut self, region: TextureRegion, row_pitch: u32, bytes: Vec<u8>) {
        let offset = self.shared_size.next_multiple_of(16) as u32;
        self.shared_size = offset as usize + bytes.len();
        self.fence += 1;
        self.w.read_texture(&region, offset, row_pitch, 0, self.fence);
        self.expect.push(Expect::Shared { offset, bytes });
    }

    pub fn expect(&mut self, x: i32, y: i32, rgba: [u8; 4]) {
        self.expect.push(Expect::Pixels { rect: Rect::new(x, y, x + 1, y + 1), rgba, tolerance: 2 });
    }

    pub fn expect_tol(&mut self, x: i32, y: i32, rgba: [u8; 4], tolerance: u8) {
        self.expect.push(Expect::Pixels { rect: Rect::new(x, y, x + 1, y + 1), rgba, tolerance });
    }

    pub fn expect_rect(&mut self, rect: Rect, rgba: [u8; 4]) {
        self.expect.push(Expect::Pixels { rect, rgba, tolerance: 2 });
    }

    pub fn expect_any(&mut self, rect: Rect, rgba: [u8; 4]) {
        self.expect.push(Expect::AnyPixel { rect, rgba, tolerance: 2 });
    }

    pub fn finish(mut self, name: &'static str) -> Built {
        self.split();
        Built {
            name,
            width: self.width,
            height: self.height,
            batches: self.batches,
            shared_size: self.shared_size,
            expect: self.expect,
        }
    }
}

impl Built {
    /// Checks a presented frame (tightly packed RGBA8, `width` wide) and the
    /// shared memory after all fences against the expectations; returns the
    /// failures.
    pub fn check(&self, width: u32, pixels: &[u8], shared: &[u8]) -> Vec<String> {
        let mut fails = Vec::new();
        let px = |x: i32, y: i32| -> Option<[u8; 4]> {
            let i = ((y as u32 * width + x as u32) * 4) as usize;
            pixels.get(i..i + 4).map(|p| [p[0], p[1], p[2], p[3]])
        };
        let close = |a: Option<[u8; 4]>, b: [u8; 4], tol: u8| {
            a.is_some_and(|a| a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= tol))
        };
        for e in &self.expect {
            match e {
                Expect::Pixels { rect, rgba, tolerance } => {
                    'outer: for y in rect.y1..rect.y2 {
                        for x in rect.x1..rect.x2 {
                            if !close(px(x, y), *rgba, *tolerance) {
                                fails.push(format!("({x}, {y}) is {:?}, want {rgba:?}", px(x, y)));
                                break 'outer;
                            }
                        }
                    }
                }
                Expect::AnyPixel { rect, rgba, tolerance } => {
                    let found =
                        (rect.y1..rect.y2).any(|y| (rect.x1..rect.x2).any(|x| close(px(x, y), *rgba, *tolerance)));
                    if !found {
                        fails.push(format!("no pixel of {rect:?} is {rgba:?}"));
                    }
                }
                Expect::Shared { offset, bytes } => {
                    let got = shared.get(*offset as usize..*offset as usize + bytes.len());
                    if got != Some(bytes.as_slice()) {
                        fails.push(format!("shared memory at {offset} is {got:02x?}, want {bytes:02x?}"));
                    }
                }
            }
        }
        fails
    }
}

impl Scene {
    pub fn build(&self, width: u32, height: u32) -> Built {
        let mut b = Builder::new(width, height);
        (self.build)(&mut b);
        b.finish(self.name)
    }
}

/// Vertex shader: position in clip space and a colour.
pub const VS_COLOR: &str = "vs_2_0
    dcl_position v0
    dcl_color v1
    mov oPos, v0
    mov oD0, v1";

/// Vertex shader: position in Direct3D pixel coordinates (pixel centres at
/// integers) via `c0` from [`Builder::pixel_space`], and a colour.
pub const VS_PIXEL: &str = "vs_2_0
    dcl_position v0
    dcl_color v1
    mad oPos.xy, v0, c0, c0.zwzw
    mov oPos.zw, v0
    mov oD0, v1";

/// Pixel shader: the interpolated colour.
pub const PS_COLOR: &str = "ps_2_0
    dcl v0
    mov oC0, v0";

/// Vertex shader: clip-space position and a texture coordinate.
pub const VS_TEX: &str = "vs_2_0
    dcl_position v0
    dcl_texcoord v1
    mov oPos, v0
    mov oT0, v1";

/// Pixel shader: sampler 0 at texture coordinate 0.
pub const PS_TEX: &str = "ps_2_0
    dcl t0
    dcl_2d s0
    texld r0, t0, s0
    mov oC0, r0";

/// Position (float3) and D3DCOLOR.
pub const DECL_PC: [VertexElement; 2] = [
    VertexElement::new(0, 0, DeclType::Float3, DeclUsage::Position, 0),
    VertexElement::new(0, 12, DeclType::D3dColor, DeclUsage::Color, 0),
];

/// Position (float3) and a 2D texture coordinate.
pub const DECL_PT: [VertexElement; 2] = [
    VertexElement::new(0, 0, DeclType::Float3, DeclUsage::Position, 0),
    VertexElement::new(0, 12, DeclType::Float2, DeclUsage::TexCoord, 0),
];

/// A position + D3DCOLOR vertex.
pub fn pc(x: f32, y: f32, z: f32, color: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity(16);
    for f in [x, y, z] {
        v.extend_from_slice(&f.to_le_bytes());
    }
    v.extend_from_slice(&color.to_le_bytes());
    v
}

/// A position + texcoord vertex.
pub fn pt(x: f32, y: f32, z: f32, u: f32, v: f32) -> Vec<u8> {
    [x, y, z, u, v].iter().flat_map(|f| f.to_le_bytes()).collect()
}

/// Two clockwise triangles (6 position + colour vertices) for a rectangle
/// in clip space (y up): (x0, y0) is the bottom-left corner.
pub fn rect_clip(x0: f32, y0: f32, x1: f32, y1: f32, z: f32, color: u32) -> Vec<u8> {
    quad(&[(x0, y1), (x1, y1), (x1, y0), (x0, y0)], z, color)
}

/// Two clockwise triangles for a rectangle in Direct3D pixel coordinates
/// (y down): (x0, y0) is the top-left corner. For [`VS_PIXEL`].
pub fn rect_px(x0: f32, y0: f32, x1: f32, y1: f32, z: f32, color: u32) -> Vec<u8> {
    quad(&[(x0, y0), (x1, y0), (x1, y1), (x0, y1)], z, color)
}

/// Corners in on-screen order top-left, top-right, bottom-right,
/// bottom-left.
fn quad(c: &[(f32, f32); 4], z: f32, color: u32) -> Vec<u8> {
    [0, 1, 2, 0, 2, 3].iter().flat_map(|i| pc(c[*i].0, c[*i].1, z, color)).collect()
}

/// D3DCOLOR from RGBA bytes.
pub const fn argb(r: u8, g: u8, b: u8, a: u8) -> u32 {
    (a as u32) << 24 | (r as u32) << 16 | (g as u32) << 8 | b as u32
}
