//! Typed error wrapping `GrB_Info` return codes.

use thiserror::Error;

/// Errors returned by the GraphBLAS C API. Wraps `GrB_Info`.
///
/// We do not enumerate every code — we keep the raw value for diagnostics
/// and only specialize the few we expect to handle programmatically.
#[derive(Debug, Error)]
pub enum GrbError {
    #[error("GraphBLAS returned non-success code {code}")]
    Failed { code: i32 },
}

impl GrbError {
    /// Convert a `GrB_Info` return value into `Result<(), GrbError>`.
    ///
    /// `0` is `GrB_SUCCESS`. Any other value is treated as an error.
    #[inline]
    pub fn check(code: i32) -> Result<(), Self> {
        if code == 0 {
            Ok(())
        } else {
            Err(Self::Failed { code })
        }
    }
}
