use std::path::PathBuf;

use assert_cmd::Command;
use predicates::str::contains;
use tempfile::tempdir;

fn workspace_root() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p.pop();
    p
}

#[test]
fn run_with_default_selection_against_stub_fails_red() {
    let tmp = tempdir().unwrap();
    let db = tmp.path().join("h.sqlite");
    let junit = tmp.path().join("results.xml");
    let assertion = Command::cargo_bin("harness")
        .unwrap()
        .args([
            "--workspace", workspace_root().to_str().unwrap(),
            "--db", db.to_str().unwrap(),
            "--engine", "stub",
            "run",
            "--junit", junit.to_str().unwrap(),
        ])
        .assert();
    let output = assertion.get_output().clone();
    let stdout = String::from_utf8(output.stdout.clone()).unwrap();
    assert!(stdout.contains("FAIL"), "stub must fail at least one selected test");
    assert!(stdout.contains("PASS"), "stub must pass at least one selected test");
    assertion.failure();
    let xml = std::fs::read_to_string(&junit).unwrap();
    assert!(xml.contains("<testsuite"));
    assert!(xml.contains("<failure"));
}

#[test]
fn allow_failing_flag_keeps_exit_zero() {
    let tmp = tempdir().unwrap();
    let db = tmp.path().join("h.sqlite");
    Command::cargo_bin("harness")
        .unwrap()
        .args([
            "--workspace", workspace_root().to_str().unwrap(),
            "--db", db.to_str().unwrap(),
            "--engine", "stub",
            "run",
            "--allow-failing",
        ])
        .assert()
        .success()
        .stdout(contains("FAIL"));
}
