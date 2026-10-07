//! Layer 1: every instruction form against results recorded on real CPUs.

use wwt_testkit::case::read_fixtures;
use wwt_testkit::suite::{fixture_path, run_fixtures};

fn run_group(group: &str) {
    run_group_with(group, &[]);
}

/// Runs a group optimized and in fast mode, plus the `extra` configurations.
fn run_group_with(group: &str, extra: &[(&str, wwt::Config)]) {
    let path = fixture_path(group);
    if !path.exists() {
        eprintln!(
            "no fixtures for {group}; record them with `cargo run -p wwt-testkit --bin record`"
        );
        return;
    }
    let fixtures = read_fixtures(&path).unwrap();
    let configs = [
        ("optimized", wwt::Config::default()),
        ("fast mode", wwt::Config::fast()),
    ];
    for (name, cfg) in configs.iter().chain(extra) {
        let summary = run_fixtures(&fixtures, cfg).unwrap();
        let report = summary.report();
        eprintln!("{group} ({name}): {report}");
        let (_, fail, _) = summary.totals();
        assert_eq!(fail, 0, "{group} ({name}) has failing cases:\n{report}");
    }
}

#[test]
fn integer() {
    run_group("integer");
}

#[test]
fn fusion() {
    run_group("fusion");
}

#[test]
fn x87() {
    run_group("x87");
}

#[test]
fn sse() {
    run_group("sse");
}

// x86-64: the executor runs these in the translator's 64-bit mode, also
// with a 64-bit WebAssembly memory.

fn mem64() -> [(&'static str, wwt::Config); 1] {
    [("memory64", wwt::Config::default().with_mem64(true))]
}

#[test]
fn integer64() {
    run_group_with("integer64", &mem64());
}

#[test]
fn fusion64() {
    run_group_with("fusion64", &mem64());
}

#[test]
fn sse64() {
    run_group_with("sse64", &mem64());
}
