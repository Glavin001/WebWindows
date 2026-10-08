//! CPU-side emulation of Direct3D 9 features WebGPU lacks.
//!
//! The render core calls into this crate wherever Direct3D 9 and WebGPU disagree and the gap has to be closed on
//! the CPU: primitive types WebGPU cannot draw, fill modes it has no rasterizer state for, texture and vertex
//! formats it cannot store or fetch, and viewports it rejects. Everything is a pure function over slices, with no
//! GPU and no state, so each rule can be tested against Direct3D's documented behaviour on its own.
//!
//! - [`index`]: triangle fans, strips, wireframe and point fill as index lists; base vertex and restart fixes.
//! - [`format`](mod@format): `D3DFORMAT` to WebGPU texture format, upload/readback conversion, sampling swizzles, and the
//!   BC1-BC5 decoders ([`format::dxt`]) for devices without `texture-compression-bc`.
//! - [`vertex`]: `D3DDECLTYPE` to WebGPU vertex formats, and repacking of vertex buffers WebGPU cannot address.
//! - [`viewport`]: viewports that extend past the render target, clamped with a clip-space fixup.
//! - [`half`]: float16 conversion used by the format code.

pub mod format;
pub mod half;
pub mod index;
pub mod vertex;
pub mod viewport;
