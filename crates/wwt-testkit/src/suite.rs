//! Running a fixture group and summarizing the results per instruction form.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Result;

use crate::case::Fixture;
use crate::compare::compare;
use crate::exec::{Executor, Run};

pub fn fixture_path(group: &str) -> PathBuf {
    crate::oracle::repo_root().join(format!("tests/fixtures/instructions/{group}.jsonl.gz"))
}

#[derive(Default, Debug)]
pub struct FormResult {
    pub pass: usize,
    pub fail: usize,
    pub unsupported: usize,
    pub first_failure: Option<String>,
}

#[derive(Default, Debug)]
pub struct Summary {
    pub forms: BTreeMap<String, FormResult>,
}

impl Summary {
    pub fn totals(&self) -> (usize, usize, usize) {
        self.forms.values().fold((0, 0, 0), |(p, f, u), r| (p + r.pass, f + r.fail, u + r.unsupported))
    }
    pub fn failing_forms(&self) -> Vec<(&String, &FormResult)> {
        self.forms.iter().filter(|(_, r)| r.fail > 0).collect()
    }
    pub fn unsupported_forms(&self) -> Vec<(&String, &FormResult)> {
        self.forms.iter().filter(|(_, r)| r.unsupported > 0 && r.fail == 0).collect()
    }
    pub fn report(&self) -> String {
        let (p, f, u) = self.totals();
        let mut s = format!(
            "{} forms: {} cases pass, {} fail, {} unsupported\n",
            self.forms.len(),
            p,
            f,
            u
        );
        for (name, r) in self.failing_forms() {
            s += &format!("  FAIL {name} ({}/{}): {}\n", r.fail, r.pass + r.fail, r.first_failure.as_deref().unwrap_or(""));
        }
        for (name, r) in self.unsupported_forms() {
            s += &format!("  UNSUPPORTED {name}: {}\n", r.first_failure.as_deref().unwrap_or(""));
        }
        s
    }
}

pub fn run_fixtures(fixtures: &[Fixture], cfg: &wwt::Config) -> Result<Summary> {
    let exec = Executor::new()?;
    let mut summary = Summary::default();
    for chunk in fixtures.chunks(2000) {
        let cases: Vec<_> = chunk.iter().map(|f| f.case.clone()).collect();
        let runs = exec.run(&cases, cfg)?;
        for (fx, run) in chunk.iter().zip(runs) {
            let e = summary.forms.entry(fx.case.form.clone()).or_default();
            match run {
                Run::Unsupported(why) => {
                    e.unsupported += 1;
                    e.first_failure.get_or_insert(why);
                }
                Run::Ok(got) => {
                    let diffs = compare(&fx.case, &fx.out, &got);
                    if diffs.is_empty() {
                        e.pass += 1;
                    } else {
                        e.fail += 1;
                        e.first_failure.get_or_insert_with(|| {
                            format!("{} regs={:x?} flags={:#x}: {}", fx.case.code, fx.case.regs, fx.case.eflags, diffs.join("; "))
                        });
                    }
                }
            }
        }
    }
    Ok(summary)
}
