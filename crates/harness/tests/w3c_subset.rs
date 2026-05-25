//! End-to-end Stage-1 test: load the real `harness/selected.toml`,
//! run each selected case against the real OWL 2 RL engine, assert
//! every selected case passes.
//!
//! The current Stage-1 selection is the three-case hand-rolled OWL 2 RL
//! fixture set plus one SPARQL 1.1 smoke ASK. The aspirational
//! ≥50-case W3C OWL 2 RL subset referenced in earlier specs ships when
//! `crates/harness/scripts/fetch-w3c-suites.sh` is wired into
//! `selected.toml` — see SPEC-01 / TASKS.md MEDIUM SPEC-01 follow-up.
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
    let expected: usize = sel.suites.values().map(|s| s.include.len()).sum();
    let mut engine = horndb_owlrl::Engine::new();
    let report = run_selected(&mut engine, &sel, &workspace(), &|p, s: Suite| {
        manifest::parse(p, s)
    })
    .unwrap();

    assert_eq!(
        report.outcomes.len(),
        expected,
        "expected one outcome per selected test ({expected}), got {}",
        report.outcomes.len(),
    );
    let failing: Vec<String> = report
        .outcomes
        .iter()
        .filter(|o| matches!(o.status, horndb_harness::Status::Failed))
        .map(|o| {
            format!(
                "{} ({})",
                o.test_id,
                o.reason.as_deref().unwrap_or("no reason")
            )
        })
        .collect();
    assert!(
        failing.is_empty(),
        "real engine failed selected cases: {failing:#?}"
    );
}
