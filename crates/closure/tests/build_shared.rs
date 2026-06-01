//! Unit tests for the pure helpers in `build/shared.rs`. The helper file is
//! `#[path]`-included here (it is also included by `build.rs`); it is std-only
//! so it compiles in the test target regardless of crate features.
#[path = "../build/shared.rs"]
mod shared;

const VERSION_CMAKE: &str = r#"
# version of SuiteSparse:GraphBLAS
set ( GraphBLAS_DATE "Dec 3, 2025" )
set ( GraphBLAS_VERSION_MAJOR 10 CACHE STRING "" FORCE )
set ( GraphBLAS_VERSION_MINOR 3 CACHE STRING "" FORCE )
set ( GraphBLAS_VERSION_SUB   0 CACHE STRING "" FORCE )

set ( GraphBLAS_API_VERSION_MAJOR 2 )
message ( STATUS "Building SuiteSparse:GraphBLAS version: v"
    ${GraphBLAS_VERSION_MAJOR}.${GraphBLAS_VERSION_MINOR}.${GraphBLAS_VERSION_SUB} )
"#;

#[test]
fn parse_version_nominal() {
    assert_eq!(
        shared::parse_version(VERSION_CMAKE).as_deref(),
        Some("10.3.0")
    );
}

#[test]
fn parse_version_skips_interpolation_line_before_set_lines() {
    // The `message(...)` interpolation line mentions GraphBLAS_VERSION_MAJOR
    // and friends *before* the real `set(...)` lines. A naive "first line
    // containing the field name" parser would read `}` / `.` instead of the
    // number; parse_version must skip lines whose next token isn't numeric.
    let interp_first = "message ( STATUS \"v${GraphBLAS_VERSION_MAJOR}.${GraphBLAS_VERSION_MINOR}.${GraphBLAS_VERSION_SUB}\" )\n\
                        set ( GraphBLAS_VERSION_MAJOR 10 CACHE STRING \"\" FORCE )\n\
                        set ( GraphBLAS_VERSION_MINOR 3 CACHE STRING \"\" FORCE )\n\
                        set ( GraphBLAS_VERSION_SUB   0 CACHE STRING \"\" FORCE )\n";
    assert_eq!(
        shared::parse_version(interp_first).as_deref(),
        Some("10.3.0")
    );
}

#[test]
fn parse_version_missing_field_returns_none() {
    let no_sub = "set ( GraphBLAS_VERSION_MAJOR 10 CACHE STRING \"\" FORCE )\n\
                  set ( GraphBLAS_VERSION_MINOR 3 CACHE STRING \"\" FORCE )\n";
    assert_eq!(shared::parse_version(no_sub), None);
}

use shared::LockStep;

#[test]
fn decide_holding_lock_no_marker_builds() {
    assert_eq!(shared::decide(false, true, false), LockStep::Build);
}

#[test]
fn decide_holding_lock_marker_present_reuses() {
    // Won the lock, but the build completed during the acquire race.
    assert_eq!(shared::decide(true, true, false), LockStep::UseInstall);
}

#[test]
fn decide_lock_held_by_other_marker_present_reuses() {
    assert_eq!(shared::decide(true, false, false), LockStep::UseInstall);
}

#[test]
fn decide_lock_held_by_other_no_marker_waits() {
    assert_eq!(shared::decide(false, false, false), LockStep::Wait);
}

#[test]
fn decide_timed_out_fails() {
    assert_eq!(shared::decide(false, false, true), LockStep::Fail);
}

#[test]
fn parse_pid_reads_first_line() {
    assert_eq!(shared::parse_pid("48213\n"), Some(48213));
}

#[test]
fn parse_pid_trims_whitespace() {
    assert_eq!(shared::parse_pid("  61\n\n"), Some(61));
}

#[test]
fn parse_pid_rejects_garbage_and_empty() {
    assert_eq!(shared::parse_pid(""), None);
    assert_eq!(shared::parse_pid("not-a-pid"), None);
}
