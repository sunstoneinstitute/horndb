//! End-to-end Stage-1 test: load the real `harness/selected.toml`,
//! run the OWL 2 RL 50-case subset against the real engine, assert
//! every selected case passes.
//!
//! Gated behind the `real-engine` feature so the default test run
//! (and the Stage-0 PR job) does not depend on the SPEC-04 engine.

#![cfg(feature = "real-engine")]

use std::path::PathBuf;

use horndb_harness::{manifest, runner::run_selected, selected::Selected, testcase::Suite};

fn workspace() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p.pop();
    p
}

#[test]
fn real_engine_passes_full_stage1_selection() {
    let sel = Selected::load(&workspace().join("harness/selected.toml")).unwrap();
    let mut engine = horndb_owlrl::Engine::new();
    let report = run_selected(&mut engine, &sel, &workspace(), &|p, s: Suite| {
        manifest::parse(p, s)
    })
    .unwrap();

    assert!(
        report.outcomes.len() >= 50,
        "expected >=50 selected tests, got {}",
        report.outcomes.len(),
    );
    let failing: Vec<&str> = report
        .outcomes
        .iter()
        .filter(|o| matches!(o.status, horndb_harness::Status::Failed))
        .map(|o| o.test_id.as_str())
        .collect();
    assert!(
        failing.is_empty(),
        "real engine failed selected cases: {failing:?}"
    );
}
