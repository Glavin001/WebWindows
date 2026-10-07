//! Translates a shader from assembly text or bytecode and prints WGSL.
//!
//! cargo run -p d3dgpu-shader --example translate -- shader.asm
//! cargo run -p d3dgpu-shader --example translate -- shader.bin

use d3dgpu_shader::*;

fn main() {
    let path = std::env::args().nth(1).expect("usage: translate <file.asm|file.bin>");
    let data = std::fs::read(&path).expect("read");
    let module = match std::str::from_utf8(&data) {
        Ok(text) if text.trim_start().starts_with(['v', 'p']) => {
            ShaderModule::new(&asm::assemble(text).unwrap_or_else(|e| panic!("{e}"))).unwrap()
        }
        _ => ShaderModule::from_bytes(&data).unwrap_or_else(|e| panic!("{e}")),
    };
    eprintln!("{}", asm::disassemble(&module.shader));
    eprintln!("{:#?}", module.reflection);
    let out = match module.stage() {
        Stage::Vertex => module.vertex(&VertexKey { pos_fixup: true, ..Default::default() }),
        Stage::Pixel => module.pixel(&PixelKey::default()),
    };
    print!("{}", out.unwrap_or_else(|e| panic!("{e}")).wgsl);
}
