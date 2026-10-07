//! Test layer 5: the translator's output on fixed binaries, as reviewed
//! snapshots. A change in generated code shows up as a snapshot diff
//! (`cargo insta review`); behavior is checked by the other layers.

use std::path::Path;

use wwt::pe::PeFile;

fn translate(name: &str) -> wwt::Translation {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/binaries")
        .join(name);
    let pe = PeFile::parse(std::fs::read(path).unwrap()).unwrap();
    wwt::translate_pe(&pe, &wwt::Config::default(), &[]).unwrap()
}

fn ir_of(t: &wwt::Translation, entry: u64) -> String {
    t.ir.iter()
        .find(|f| f.entry == entry)
        .expect("function")
        .to_string()
}

#[test]
fn hello_ir() {
    let t = translate("hello.exe");
    let all: String =
        t.ir.iter()
            .map(|f| f.to_string())
            .collect::<Vec<_>>()
            .join("\n");
    insta::assert_snapshot!(all);
}

#[test]
fn hello_wat() {
    let t = translate("hello.exe");
    insta::assert_snapshot!(wasmprinter::print_bytes(&t.wasm).unwrap());
}

#[test]
fn switch_report() {
    let t = translate("switch-O2.exe");
    let r = &t.report;
    insta::assert_snapshot!(format!(
        "functions: {}\nblocks: {}\nx86 instructions: {}\njump tables: {}\nseeds: {:?}\nunsupported: {}",
        r.functions,
        r.blocks,
        r.x86_instructions,
        r.jump_tables,
        r.seeds,
        r.unsupported.len()
    ));
}

#[test]
fn switch_classify_ir() {
    // `classify` is a jump table; its IR shows the switch lowering.
    let t = translate("switch-O2.exe");
    let f =
        t.ir.iter()
            .find(|f| {
                f.blocks
                    .iter()
                    .any(|b| matches!(b.term, wwt::ir::Term::Switch { .. }))
            })
            .expect("a function with a jump table");
    insta::assert_snapshot!(ir_of(&t, f.entry));
}

#[test]
fn floats_x87_ir() {
    let t = translate("floats-O1.exe");
    // The first function using the FPU stack top.
    let f =
        t.ir.iter()
            .find(|f| {
                f.blocks
                    .iter()
                    .any(|b| b.insts.iter().any(|i| i.dst == Some(wwt::ir::FPU_TOP)))
            })
            .expect("an x87 function");
    insta::assert_snapshot!(ir_of(&t, f.entry));
}
