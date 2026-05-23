//! Smoke test: prove FFI surface compiles and we can convert error codes.

use reasoner_closure::error::GrbError;

#[test]
fn grb_success_is_ok() {
    // GrB_SUCCESS == 0; converting it to a GrbError result should be Ok(()).
    assert!(GrbError::check(0).is_ok());
}

#[test]
fn grb_nonzero_is_err() {
    // Any non-zero code is an error. Pick GrB_NULL_POINTER (typically -3 in v8.x, -103 in v10).
    let err = GrbError::check(-3);
    assert!(err.is_err());
}
