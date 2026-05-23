//! Transitive closure of a Boolean adjacency matrix via iterated MxM.
//!
//! Algorithm (semi-naïve, repeated squaring would be denser per step but
//! converges in O(log n) iterations; we use the simpler "fold powers"
//! approach which is closer to the SPEC text and easier to reason about):
//!
//! ```text
//!   R   <- M                 // 1-step reachable
//!   F   <- M                 // current frontier
//!   loop:
//!     F'  <- F * M           // extend frontier by one more hop
//!     prev_nnz <- nnz(R)
//!     R  <- R ∨ F'           // accumulate
//!     if nnz(R) == prev_nnz: break
//!     F  <- F'
//! ```
//!
//! For dense graphs this is O(diameter) MxMs. SuiteSparse's automatic
//! hyper/sparse switching keeps the iteration cheap on skewed inputs.
//!
//! Stage-1 SPEC says `M_p* = I ∨ M_p ∨ M_p² ∨ … ∨ M_p^k`. We **omit `I`**
//! (the identity) because OWL 2 RL transitive-property closure (`prp-trp`)
//! does not infer `?x p ?x` for arbitrary `x` — only edges actually reached.
//! The schema closures (`scm-sco`, `scm-spo`) require reflexivity over the
//! class/property *extent*, which is added separately in `schema.rs`.

use crate::error::GrbError;
use crate::grb::BoolMatrix;

/// Compute `M⁺ = M ∨ M² ∨ M³ ∨ …` until fixed point. The identity is **not**
/// included; the result is the *strictly* transitive closure.
///
/// Stage-1 note: we lack a `GrB_Matrix_dup` wrapper, so we initialise both
/// the accumulator (`reach`) and the frontier by round-tripping through
/// `extract_edges` + `from_edges`. This is correct but performs two extra
/// allocations of size nnz(M); fast-path `dup` is a Stage-2 micro-opt.
pub fn transitive_closure(m: &BoolMatrix) -> Result<BoolMatrix, GrbError> {
    if m.nvals()? == 0 {
        return BoolMatrix::new(m.nrows());
    }

    let n = m.nrows();
    let edges = m.extract_edges()?;
    let mut reach = BoolMatrix::from_edges(n, &edges)?;
    let mut frontier = BoolMatrix::from_edges(n, &edges)?;

    loop {
        // Frontier := Frontier * M (one more hop).
        let next_frontier = frontier.mxm_lor_land(m)?;
        if next_frontier.nvals()? == 0 {
            break;
        }
        let prev_nvals = reach.nvals()?;
        reach.or_assign(&next_frontier)?;
        reach.wait()?; // force materialisation before reading nvals under GrB_NONBLOCKING
        if reach.nvals()? == prev_nvals {
            // No new edges contributed — fixed point.
            break;
        }
        frontier = next_frontier;
    }

    Ok(reach)
}

/// Build an `n x n` identity matrix in Boolean. Only used internally for
/// schema closure (where reflexivity is required).
pub fn identity_like(m: &BoolMatrix) -> Result<BoolMatrix, GrbError> {
    let n = m.nrows();
    let diag: Vec<(u64, u64)> = (0..n).map(|i| (i, i)).collect();
    BoolMatrix::from_edges(n, &diag)
}
