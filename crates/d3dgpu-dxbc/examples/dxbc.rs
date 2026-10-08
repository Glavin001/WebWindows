//! Prints the disassembly, reflection and WGSL of a DXBC file.
//!
//! cargo run -p d3dgpu-dxbc --example dxbc -- shader.dxbc
fn main() {
    let path = std::env::args().nth(1).expect("usage: dxbc <file.dxbc>");
    let sh = d3dgpu_dxbc::Shader::from_dxbc(&std::fs::read(path).unwrap()).unwrap_or_else(|e| panic!("{e}"));
    eprintln!("{}", sh.disassemble());
    eprintln!("{:#?}", sh.reflection);
    match sh.translate(&d3dgpu_dxbc::Key { pos_fixup: true, ..Default::default() }) {
        Ok(t) => print!("{}", t.wgsl),
        Err(e) => eprintln!("error: {e}"),
    }
}
