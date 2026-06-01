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
