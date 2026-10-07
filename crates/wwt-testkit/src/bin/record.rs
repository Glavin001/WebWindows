//! Records instruction fixtures on this machine's CPU.
//!
//!   cargo run -p wwt-testkit --bin record -- [group...] [--per-form N] [--check]
//!
//! With --check, compares the CPU's results against the committed fixtures
//! instead of writing them (used by CI on x86 runners).

use anyhow::{bail, Result};
use std::collections::BTreeSet;
use wwt_testkit::case::{read_fixtures, write_fixtures, Fixture};
use wwt_testkit::compare::compare;
use wwt_testkit::gen::{cases_for, extra_cases, forms, Group, Rng};
use wwt_testkit::{oracle, suite};

fn main() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut per_form = 24;
    let mut check = false;
    if let Some(i) = args.iter().position(|a| a == "--per-form") {
        per_form = args[i + 1].parse()?;
        args.drain(i..i + 2);
    }
    if let Some(i) = args.iter().position(|a| a == "--check") {
        check = true;
        args.remove(i);
    }
    let groups: Vec<Group> = Group::all()
        .into_iter()
        .filter(|g| args.is_empty() || args.iter().any(|a| a == g.name()))
        .collect();
    for g in groups {
        let path = suite::fixture_path(g.name());
        let cases = if check {
            read_fixtures(&path)?.into_iter().map(|f| f.case).collect()
        } else {
            let mut rng = Rng::new(0x5757_5454 ^ g.name().len() as u64);
            let mut cases = vec![];
            for code in forms(g) {
                cases.extend(cases_for(code, per_form, &mut rng));
            }
            cases.extend(extra_cases(g, &mut rng));
            cases
        };
        if cases.is_empty() {
            continue;
        }
        let outs = oracle::run(&cases)?;
        let fixtures: Vec<Fixture> = cases
            .into_iter()
            .zip(outs)
            .map(|(case, out)| Fixture { case, out })
            .collect();
        if check {
            // Compared like the translator's results: undefined flags and
            // results may legitimately differ between CPU models.
            let old = read_fixtures(&path)?;
            let mut bad = 0;
            let mut shown = BTreeSet::new();
            for (a, b) in old.iter().zip(&fixtures) {
                let diffs = compare(&a.case, &a.out, &b.out);
                if diffs.is_empty() {
                    continue;
                }
                bad += 1;
                if shown.insert(a.case.form.clone()) && shown.len() <= 40 {
                    eprintln!("  {} ({}): {}", a.case.form, a.case.code, diffs.join("; "));
                }
            }
            if bad > 0 {
                bail!(
                    "{}: {bad} of {} cases differ on this CPU ({} forms)",
                    g.name(),
                    old.len(),
                    shown.len()
                );
            }
            eprintln!("{}: {} cases match this CPU", g.name(), old.len());
        } else {
            std::fs::create_dir_all(path.parent().unwrap())?;
            write_fixtures(&path, &fixtures)?;
            eprintln!(
                "{}: wrote {} cases to {}",
                g.name(),
                fixtures.len(),
                path.display()
            );
        }
    }
    Ok(())
}
