//! Safe RAII wrapper over the small slice of SuiteSparse:GraphBLAS we use.
//!
//! Only Boolean matrices are exposed. The Boolean `(∨,∧)` semiring
//! (`GxB_LOR_LAND_BOOL` or `GrB_LOR_LAND_SEMIRING_BOOL` depending on
//! the SuiteSparse version) is the only semiring we expose in Stage 1.

use std::sync::Once;

use crate::error::GrbError;
use crate::ffi;

static GRB_INIT: Once = Once::new();
static mut GRB_INIT_RESULT: i32 = 0;

/// Initialise GraphBLAS exactly once per process. Idempotent across threads.
///
/// SuiteSparse:GraphBLAS requires `GrB_init` before any other call and
/// `GrB_finalize` only at process shutdown (we deliberately do not call
/// `GrB_finalize` — the OS reclaims memory on exit and calling it from
/// a `Drop` of a static is unsound).
pub fn init_once() -> Result<(), GrbError> {
    // Safety: GrB_init is thread-safe via Once; mode = GrB_NONBLOCKING (0).
    unsafe {
        GRB_INIT.call_once(|| {
            GRB_INIT_RESULT = ffi::GrB_init(ffi::GrB_Mode_GrB_NONBLOCKING as i32);
        });
        GrbError::check(GRB_INIT_RESULT)
    }
}

/// Owned Boolean GraphBLAS matrix. Frees the underlying `GrB_Matrix` on drop.
pub struct BoolMatrix {
    inner: ffi::GrB_Matrix,
    nrows: u64,
    ncols: u64,
}

// Safety: GrB_Matrix handles are independent allocations; SuiteSparse documents
// that distinct matrix handles may be used concurrently from different threads
// in GrB_NONBLOCKING mode (it serialises internally).
unsafe impl Send for BoolMatrix {}

impl BoolMatrix {
    /// Construct an `n x n` Boolean matrix populated from `edges`.
    pub fn from_edges(n: u64, edges: &[(u64, u64)]) -> Result<Self, GrbError> {
        let mut handle: ffi::GrB_Matrix = std::ptr::null_mut();
        unsafe {
            GrbError::check(ffi::GrB_Matrix_new(
                &mut handle,
                ffi::GrB_BOOL,
                n,
                n,
            ))?;
        }

        if !edges.is_empty() {
            let rows: Vec<u64> = edges.iter().map(|(s, _)| *s).collect();
            let cols: Vec<u64> = edges.iter().map(|(_, o)| *o).collect();
            let vals: Vec<bool> = vec![true; edges.len()];
            unsafe {
                // GrB_Matrix_build_BOOL(C, I, J, X, nvals, dup)
                // dup = GrB_LOR (any associative bool combiner; LOR is canonical).
                GrbError::check(ffi::GrB_Matrix_build_BOOL(
                    handle,
                    rows.as_ptr(),
                    cols.as_ptr(),
                    vals.as_ptr(),
                    edges.len() as u64,
                    ffi::GrB_LOR,
                ))?;
            }
        }

        Ok(Self { inner: handle, nrows: n, ncols: n })
    }

    /// Construct a fresh empty `n x n` Boolean matrix.
    pub fn new(n: u64) -> Result<Self, GrbError> {
        Self::from_edges(n, &[])
    }

    pub fn nrows(&self) -> u64 { self.nrows }
    pub fn ncols(&self) -> u64 { self.ncols }

    pub fn nvals(&self) -> Result<u64, GrbError> {
        let mut n: u64 = 0;
        unsafe { GrbError::check(ffi::GrB_Matrix_nvals(&mut n, self.inner))?; }
        Ok(n)
    }

    /// `C = A * B` over the `(∨, ∧)` Boolean semiring, replacing C.
    ///
    /// Returns a freshly allocated matrix; does not modify self or other.
    pub fn mxm_lor_land(&self, other: &BoolMatrix) -> Result<BoolMatrix, GrbError> {
        assert_eq!(self.ncols, other.nrows, "shape mismatch for MxM");
        let c = BoolMatrix::new(self.nrows)?;
        unsafe {
            // GrB_mxm(C, Mask=NULL, accum=NULL, semiring, A, B, descriptor=NULL)
            GrbError::check(ffi::GrB_mxm(
                c.inner,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                ffi::GxB_LOR_LAND_BOOL,
                self.inner,
                other.inner,
                std::ptr::null_mut(),
            ))?;
        }
        Ok(c)
    }

    /// `self ∨= other` — element-wise Boolean OR with `self` accumulating.
    /// Used to fold powers of `M` into the closure accumulator.
    pub fn or_assign(&mut self, other: &BoolMatrix) -> Result<(), GrbError> {
        assert_eq!(self.nrows, other.nrows);
        assert_eq!(self.ncols, other.ncols);
        unsafe {
            // GrB_Matrix_eWiseAdd_Monoid(C, Mask, accum, monoid, A, B, desc)
            // C = A | B using the LOR monoid.
            GrbError::check(ffi::GrB_Matrix_eWiseAdd_Monoid(
                self.inner,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                ffi::GrB_LOR_MONOID_BOOL,
                self.inner,
                other.inner,
                std::ptr::null_mut(),
            ))?;
        }
        Ok(())
    }

    /// Force any pending GraphBLAS computation on this matrix to complete.
    ///
    /// Required between an `or_assign` and a subsequent `nvals` read because
    /// `GrB_init` was called in `GrB_NONBLOCKING` mode (operations are lazy).
    pub fn wait(&self) -> Result<(), GrbError> {
        unsafe {
            GrbError::check(ffi::GrB_Matrix_wait(
                self.inner,
                ffi::GrB_WaitMode_GrB_MATERIALIZE as i32,
            ))
        }
    }

    /// Extract all `true` entries as `(row, col)` pairs in row-major order.
    pub fn extract_edges(&self) -> Result<Vec<(u64, u64)>, GrbError> {
        let nvals = self.nvals()?;
        let mut rows = vec![0u64; nvals as usize];
        let mut cols = vec![0u64; nvals as usize];
        let mut vals = vec![false; nvals as usize];
        let mut n_out: u64 = nvals;
        unsafe {
            GrbError::check(ffi::GrB_Matrix_extractTuples_BOOL(
                rows.as_mut_ptr(),
                cols.as_mut_ptr(),
                vals.as_mut_ptr(),
                &mut n_out,
                self.inner,
            ))?;
        }
        rows.truncate(n_out as usize);
        cols.truncate(n_out as usize);
        let mut out: Vec<(u64, u64)> = rows.into_iter().zip(cols).collect();
        out.sort_unstable();
        Ok(out)
    }
}

impl Drop for BoolMatrix {
    fn drop(&mut self) {
        unsafe {
            // GrB_Matrix_free takes &mut handle; the free is best-effort on drop.
            let _ = ffi::GrB_Matrix_free(&mut self.inner);
        }
    }
}
