//! WebWindows translator: x86 (32-bit Windows) machine code to WebAssembly.
//!
//! The pipeline has seven layers, each in its own module:
//!
//! 1. [`pe`] — load the PE file.
//! 2. [`discover`] — find code from entry points, exports, TLS callbacks,
//!    relocations and run-time profiles.
//! 3. Decode (inside [`discover`], with `iced-x86`).
//! 4. [`lift`] — x86 semantics as explicit operations in [`ir`].
//! 5. [`opt`] — flag elimination, folding, dead-code elimination.
//! 6. [`codegen`] — structured WebAssembly with `wasm-encoder`.
//! 7. [`translate`] — orchestration, module metadata and caching.

pub mod abi;
pub mod builtin;
pub mod codegen;
pub mod discover;
pub mod flags;
pub mod fpu;
mod fpu_helpers;
pub mod inline;
pub mod ir;
pub mod kernel;
pub mod lift;
pub mod opt;
pub mod osr;
pub mod pe;
pub mod reducible;
pub mod translate;

pub use translate::{translate_pe, translate_region, Config, Translation};
