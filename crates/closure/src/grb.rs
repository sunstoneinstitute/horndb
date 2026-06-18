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
            GrbError::check(ffi::GrB_Matrix_new(&mut handle, ffi::GrB_BOOL, n, n))?;
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

        Ok(Self {
            inner: handle,
            nrows: n,
            ncols: n,
        })
    }

    /// Construct a fresh empty `n x n` Boolean matrix.
    pub fn new(n: u64) -> Result<Self, GrbError> {
        Self::from_edges(n, &[])
    }

    pub fn nrows(&self) -> u64 {
        self.nrows
    }
    pub fn ncols(&self) -> u64 {
        self.ncols
    }

    pub fn nvals(&self) -> Result<u64, GrbError> {
        let mut n: u64 = 0;
        unsafe {
            GrbError::check(ffi::GrB_Matrix_nvals(&mut n, self.inner))?;
        }
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

/// Owned `f64`-valued GraphBLAS matrix used for *valued* (annotated) closure.
///
/// This is the carrier for "Fork A" scalar-confidence reasoning (SPEC-05
/// addendum / TASKS.md #11–#12): weighted edges combined under a valued
/// semiring such as `(max, ×)` (best-confidence path) or `(min, +)`
/// (shortest / least-cost path). It exists primarily so the readiness-metrics
/// instrumentation can measure the cost of a valued `GrB_mxm` against the
/// Boolean reachability baseline on the *same* matrix shape.
///
/// Two multiply paths are exposed deliberately:
/// - [`ValuedMatrix::mxm_max_times_builtin`] uses the prepackaged
///   `GrB_MAX_TIMES_SEMIRING_FP64`, which SuiteSparse dispatches to a
///   hand-tuned *FactoryKernel*.
/// - [`ValuedMatrix::mxm_max_times_udf`] builds an equivalent semiring from a
///   *user-defined* binary op, forcing SuiteSparse onto its *generic* kernel.
///
/// The throughput ratio between the two is exactly the multiplier that a
/// JIT/PreJIT specialization would remove (issue #11, "generic-kernel
/// penalty").
pub struct ValuedMatrix {
    inner: ffi::GrB_Matrix,
    nrows: u64,
    ncols: u64,
}

// Safety: same rationale as `BoolMatrix` — distinct handles, internally
// serialised by SuiteSparse in non-blocking mode.
unsafe impl Send for ValuedMatrix {}

impl ValuedMatrix {
    /// Construct an `n x n` `f64` matrix from `(row, col, weight)` triples.
    /// Duplicate coordinates are combined with `GrB_MAX_FP64` (keep the best
    /// confidence), matching the `(max, ×)` carrier.
    pub fn from_weighted_edges(n: u64, edges: &[(u64, u64, f64)]) -> Result<Self, GrbError> {
        let mut handle: ffi::GrB_Matrix = std::ptr::null_mut();
        unsafe {
            GrbError::check(ffi::GrB_Matrix_new(&mut handle, ffi::GrB_FP64, n, n))?;
        }

        if !edges.is_empty() {
            let rows: Vec<u64> = edges.iter().map(|(s, _, _)| *s).collect();
            let cols: Vec<u64> = edges.iter().map(|(_, o, _)| *o).collect();
            let vals: Vec<f64> = edges.iter().map(|(_, _, w)| *w).collect();
            unsafe {
                GrbError::check(ffi::GrB_Matrix_build_FP64(
                    handle,
                    rows.as_ptr(),
                    cols.as_ptr(),
                    vals.as_ptr(),
                    edges.len() as u64,
                    ffi::GrB_MAX_FP64,
                ))?;
            }
        }

        Ok(Self {
            inner: handle,
            nrows: n,
            ncols: n,
        })
    }

    /// Fresh empty `n x n` `f64` matrix.
    pub fn new(n: u64) -> Result<Self, GrbError> {
        Self::from_weighted_edges(n, &[])
    }

    pub fn nrows(&self) -> u64 {
        self.nrows
    }
    pub fn ncols(&self) -> u64 {
        self.ncols
    }

    pub fn nvals(&self) -> Result<u64, GrbError> {
        let mut n: u64 = 0;
        unsafe {
            GrbError::check(ffi::GrB_Matrix_nvals(&mut n, self.inner))?;
        }
        Ok(n)
    }

    /// `C = A ⊗ B` over the built-in `(max, ×)` FP64 semiring (FactoryKernel).
    pub fn mxm_max_times_builtin(&self, other: &ValuedMatrix) -> Result<ValuedMatrix, GrbError> {
        assert_eq!(self.ncols, other.nrows, "shape mismatch for MxM");
        let c = ValuedMatrix::new(self.nrows)?;
        unsafe {
            GrbError::check(ffi::GrB_mxm(
                c.inner,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                ffi::GrB_MAX_TIMES_SEMIRING_FP64,
                self.inner,
                other.inner,
                std::ptr::null_mut(),
            ))?;
        }
        Ok(c)
    }

    /// `C = A ⊗ B` over a `(max, ×)` semiring assembled from a *user-defined*
    /// multiply op. Functionally identical to
    /// [`mxm_max_times_builtin`](Self::mxm_max_times_builtin) but routes
    /// SuiteSparse onto its generic kernel — the measurement target for the
    /// JIT/PreJIT penalty.
    ///
    /// The multiply is **materialised before returning** (`GrB_Matrix_wait`).
    /// GraphBLAS runs in nonblocking mode, so the `GrB_mxm` could otherwise
    /// stay pending and reference `semiring`'s ops after the borrowed
    /// `&UserSemiring` is dropped — a use-after-free of the op/monoid handles.
    /// Forcing completion here makes the returned matrix independent of the
    /// semiring's lifetime. (This is also why this call is timed as the kernel
    /// cost in the readiness bench: the work has actually happened.)
    pub fn mxm_max_times_udf(
        &self,
        other: &ValuedMatrix,
        semiring: &UserSemiring,
    ) -> Result<ValuedMatrix, GrbError> {
        assert_eq!(self.ncols, other.nrows, "shape mismatch for MxM");
        let c = ValuedMatrix::new(self.nrows)?;
        unsafe {
            GrbError::check(ffi::GrB_mxm(
                c.inner,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                semiring.inner,
                self.inner,
                other.inner,
                std::ptr::null_mut(),
            ))?;
        }
        // Materialise while `semiring` is still borrowed/alive.
        c.wait()?;
        Ok(c)
    }

    /// `self = max(self, other)` element-wise (accumulate best confidence).
    pub fn max_assign(&mut self, other: &ValuedMatrix) -> Result<(), GrbError> {
        assert_eq!(self.nrows, other.nrows);
        assert_eq!(self.ncols, other.ncols);
        unsafe {
            GrbError::check(ffi::GrB_Matrix_eWiseAdd_Monoid(
                self.inner,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                ffi::GrB_MAX_MONOID_FP64,
                self.inner,
                other.inner,
                std::ptr::null_mut(),
            ))?;
        }
        Ok(())
    }

    /// Force pending lazy computation to complete (see [`BoolMatrix::wait`]).
    pub fn wait(&self) -> Result<(), GrbError> {
        unsafe {
            GrbError::check(ffi::GrB_Matrix_wait(
                self.inner,
                ffi::GrB_WaitMode_GrB_MATERIALIZE as i32,
            ))
        }
    }

    /// Sum of all stored entries (`PLUS` reduction over `f64`).
    ///
    /// A cheap aggregate over the valued carrier — total accumulated
    /// confidence mass. (Not used as the closure fixed-point signal: an exact
    /// entrywise `>` count is used there, because a single edge can improve by
    /// less than the ULP of a large sum.)
    pub fn reduce_sum(&self) -> Result<f64, GrbError> {
        let mut acc: f64 = 0.0;
        unsafe {
            GrbError::check(ffi::GrB_Matrix_reduce_FP64(
                &mut acc,
                std::ptr::null_mut(),
                ffi::GrB_PLUS_MONOID_FP64,
                self.inner,
                std::ptr::null_mut(),
            ))?;
        }
        Ok(acc)
    }

    /// Count coordinates where `self > other` (strictly), over the pattern
    /// **intersection** of the two matrices.
    ///
    /// Exact and entirely GraphBLAS-side (one `eWiseMult` with `GrB_GT_FP64`
    /// plus an `nvals` read), so it can be used as a cheap per-iteration
    /// fixed-point signal without the cost of extracting and comparing tuples
    /// in Rust. Coordinates present in `self` but absent in `other` (new
    /// support) are *not* counted here — the closure loop detects those
    /// separately via `nvals` growth, so the two signals together are exact.
    pub fn count_strictly_greater(&self, other: &ValuedMatrix) -> Result<u64, GrbError> {
        assert_eq!(self.nrows, other.nrows);
        assert_eq!(self.ncols, other.ncols);
        let mask = ValuedMatrix::new(self.nrows)?;
        unsafe {
            // mask = (self .> other) over the intersection; GT yields 1.0 where
            // self > other and stores nothing where self <= other? No — GT
            // stores a result for every intersection coordinate. We then count
            // the nonzeros: GxB stores explicit 0.0 too, so select afterwards.
            GrbError::check(ffi::GrB_Matrix_eWiseMult_BinaryOp(
                mask.inner,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                ffi::GrB_GT_FP64,
                self.inner,
                other.inner,
                std::ptr::null_mut(),
            ))?;
        }
        mask.wait()?;
        // `GT` writes an explicit entry (1.0 or 0.0) at every shared coord, so
        // count only the truthy ones by reducing the boolean-as-f64 to a sum.
        let truthy = mask.reduce_sum()?;
        Ok(truthy as u64)
    }

    /// Extract all stored entries as `(row, col, weight)` triples, row-major.
    pub fn extract_weighted_edges(&self) -> Result<Vec<(u64, u64, f64)>, GrbError> {
        let nvals = self.nvals()?;
        let mut rows = vec![0u64; nvals as usize];
        let mut cols = vec![0u64; nvals as usize];
        let mut vals = vec![0.0f64; nvals as usize];
        let mut n_out: u64 = nvals;
        unsafe {
            GrbError::check(ffi::GrB_Matrix_extractTuples_FP64(
                rows.as_mut_ptr(),
                cols.as_mut_ptr(),
                vals.as_mut_ptr(),
                &mut n_out,
                self.inner,
            ))?;
        }
        rows.truncate(n_out as usize);
        cols.truncate(n_out as usize);
        vals.truncate(n_out as usize);
        let mut out: Vec<(u64, u64, f64)> = rows
            .into_iter()
            .zip(cols)
            .zip(vals)
            .map(|((r, c), v)| (r, c, v))
            .collect();
        out.sort_unstable_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
        Ok(out)
    }
}

impl Drop for ValuedMatrix {
    fn drop(&mut self) {
        unsafe {
            let _ = ffi::GrB_Matrix_free(&mut self.inner);
        }
    }
}

/// `z = max(x, y)` for `f64`, exposed to GraphBLAS as a user-defined binary op.
///
/// Marked `extern "C"` so SuiteSparse can call it through its generic kernel.
unsafe extern "C" fn udf_max(
    z: *mut std::os::raw::c_void,
    x: *const std::os::raw::c_void,
    y: *const std::os::raw::c_void,
) {
    let x = *(x as *const f64);
    let y = *(y as *const f64);
    *(z as *mut f64) = if x > y { x } else { y };
}

/// `z = x * y` for `f64`, exposed to GraphBLAS as a user-defined binary op.
unsafe extern "C" fn udf_times(
    z: *mut std::os::raw::c_void,
    x: *const std::os::raw::c_void,
    y: *const std::os::raw::c_void,
) {
    let x = *(x as *const f64);
    let y = *(y as *const f64);
    *(z as *mut f64) = x * y;
}

/// A `(max, ×)` FP64 semiring built from *user-defined* ops, forcing the
/// SuiteSparse generic (non-Factory) kernel. Owns the underlying ops/monoid so
/// they outlive any `GrB_mxm` that references them.
pub struct UserSemiring {
    inner: ffi::GrB_Semiring,
    monoid: ffi::GrB_Monoid,
    add_op: ffi::GrB_BinaryOp,
    mul_op: ffi::GrB_BinaryOp,
}

unsafe impl Send for UserSemiring {}

impl UserSemiring {
    /// Build the user-defined `(max, ×)` FP64 semiring. Identity of the `max`
    /// monoid is `f64::NEG_INFINITY`.
    pub fn max_times_fp64() -> Result<Self, GrbError> {
        let mut add_op: ffi::GrB_BinaryOp = std::ptr::null_mut();
        let mut mul_op: ffi::GrB_BinaryOp = std::ptr::null_mut();
        let mut monoid: ffi::GrB_Monoid = std::ptr::null_mut();
        let mut semiring: ffi::GrB_Semiring = std::ptr::null_mut();
        unsafe {
            GrbError::check(ffi::GrB_BinaryOp_new(
                &mut add_op,
                Some(udf_max),
                ffi::GrB_FP64,
                ffi::GrB_FP64,
                ffi::GrB_FP64,
            ))?;
            GrbError::check(ffi::GrB_BinaryOp_new(
                &mut mul_op,
                Some(udf_times),
                ffi::GrB_FP64,
                ffi::GrB_FP64,
                ffi::GrB_FP64,
            ))?;
            GrbError::check(ffi::GrB_Monoid_new_FP64(
                &mut monoid,
                add_op,
                f64::NEG_INFINITY,
            ))?;
            GrbError::check(ffi::GrB_Semiring_new(&mut semiring, monoid, mul_op))?;
        }
        Ok(Self {
            inner: semiring,
            monoid,
            add_op,
            mul_op,
        })
    }
}

impl Drop for UserSemiring {
    fn drop(&mut self) {
        unsafe {
            let _ = ffi::GrB_Semiring_free(&mut self.inner);
            let _ = ffi::GrB_Monoid_free(&mut self.monoid);
            let _ = ffi::GrB_BinaryOp_free(&mut self.add_op);
            let _ = ffi::GrB_BinaryOp_free(&mut self.mul_op);
        }
    }
}
