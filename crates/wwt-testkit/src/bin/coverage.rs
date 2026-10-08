//! Instruction-form coverage of real binaries.
//!
//!   cargo run -p wwt-testkit --bin coverage -- a.exe b.exe ...
//!
//! Lists every instruction form (iced-x86 `Code`) the translator discovers
//! in the given binaries and whether the instruction suite covers it with
//! recorded cases. Exits non-zero when a form is not covered. Instructions
//! that only ever fault (`wwt::lift::only_faults`: privileged ones such as
//! `in`, software interrupts, `ud2`) or that the translator refuses
//! (far-pointer loads, ...) are listed apart: the instruction suite has nothing to test there. They mostly come
//! from data decoded as code. So are forms that move or push segment
//! registers without recorded cases: the selectors they read are Windows'
//! (what the translator gives a program), which the CPU the suite records
//! on, under Linux, does not have.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use wwt::pe::PeFile;
use wwt_testkit::case::read_fixtures;
use wwt_testkit::gen::Group;
use wwt_testkit::suite::fixture_path;

fn main() -> Result<()> {
    let files: Vec<String> = std::env::args().skip(1).collect();
    let mut tested: BTreeSet<String> = BTreeSet::new();
    for g in Group::all() {
        let p = fixture_path(g.name());
        if p.exists() {
            for f in read_fixtures(&p)? {
                for part in f.case.form.split('+') {
                    tested.insert(part.to_string());
                }
            }
        }
    }
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut unsupported: BTreeMap<String, usize> = BTreeMap::new();
    let mut selectors: BTreeMap<String, usize> = BTreeMap::new();
    for f in &files {
        let pe = PeFile::parse(std::fs::read(f)?)?;
        let img = pe.image()?;
        let cfg = wwt::Config::default();
        let mut d = wwt::translate::discover_pe(&pe, &img, &[], &cfg);
        let t = wwt::translate::translate_discovered(
            &img,
            &mut d,
            &cfg,
            None,
            &Default::default(),
            &Default::default(),
        )?;
        let refused: BTreeSet<u32> = t.report.unsupported.iter().map(|(va, _)| *va).collect();
        for (va, i) in &d.insts {
            let form = format!("{:?}", i.code());
            if refused.contains(va) || wwt::lift::only_faults(i) {
                *unsupported.entry(form).or_default() += 1;
            } else if !tested.contains(&form) && uses_segment_register(i) {
                *selectors.entry(form).or_default() += 1;
            } else {
                *seen.entry(form).or_default() += 1;
            }
        }
    }
    for (c, n) in &unsupported {
        println!("  unsupported (a fault when reached): {c} ({n} occurrences)");
    }
    for (c, n) in &selectors {
        println!("  segment registers (Windows' selectors, not recorded): {c} ({n} occurrences)");
    }
    let missing: Vec<_> = seen.iter().filter(|(c, _)| !tested.contains(*c)).collect();
    println!(
        "{} binaries: {} instruction forms, {} covered by the suite, {} not covered",
        files.len(),
        seen.len(),
        seen.len() - missing.len(),
        missing.len()
    );
    for (c, n) in &missing {
        println!("  not covered: {c} ({n} occurrences)");
    }
    std::process::exit(if missing.is_empty() { 0 } else { 1 });
}

/// An explicit segment register operand (`mov r/m16, sreg`, `push es`, ...).
fn uses_segment_register(i: &iced_x86::Instruction) -> bool {
    (0..i.op_count()).any(|n| {
        i.op_kind(n) == iced_x86::OpKind::Register && i.op_register(n).is_segment_register()
    })
}
