//! Layer 1: every instruction form against results recorded on real CPUs.

use wwt_testkit::case::read_fixtures;
use wwt_testkit::suite::{fixture_path, run_fixtures};

fn run_group(group: &str) {
    let path = fixture_path(group);
    if !path.exists() {
        eprintln!("no fixtures for {group}; record them with `cargo run -p wwt-testkit --bin record`");
        return;
    }
    let fixtures = read_fixtures(&path).unwrap();
    for (name, cfg) in [("optimized", wwt::Config::default()), ("fast mode", wwt::Config::fast())] {
        let summary = run_fixtures(&fixtures, &cfg).unwrap();
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
