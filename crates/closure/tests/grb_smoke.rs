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

use reasoner_closure::grb::{init_once, BoolMatrix};

#[test]
fn init_and_build_bool_matrix() {
    init_once().expect("GrB_init");

    // Build a 3x3 boolean matrix with edges (0,1) and (1,2).
    let mat = BoolMatrix::from_edges(3, &[(0, 1), (1, 2)]).expect("build matrix");
    assert_eq!(mat.nvals().unwrap(), 2);
    assert_eq!(mat.nrows(), 3);
    assert_eq!(mat.ncols(), 3);
}

#[test]
fn boolean_mxm_one_step() {
    init_once().expect("GrB_init");

    // A: edges (0,1),(1,2). A*A should produce edge (0,2).
    let a = BoolMatrix::from_edges(3, &[(0, 1), (1, 2)]).unwrap();
    let c = a.mxm_lor_land(&a).unwrap();
    let edges = c.extract_edges().unwrap();
    assert_eq!(edges, vec![(0u64, 2u64)]);
}
