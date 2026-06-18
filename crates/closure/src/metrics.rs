//! Valued-reasoning readiness metrics (TASKS.md #11).
//!
//! Before committing to custom-semiring / JIT work (#12), we must *measure*
//! whether it pays off rather than guess. This module instruments a valued
//! `(max, ×)` transitive closure so a single run yields the numbers the
//! decision rule needs:
//!
//! - **Problem size** — matrix dimension `N`, `nnz` (input edges), density.
//! - **Convergence** — iterations-to-fixpoint and work (`GrB_mxm`s, frontier
//!   `nnz`) per iteration.
//! - **Kernel split** — wall-time of the valued semiring `GrB_mxm` against a
//!   Boolean-reachability baseline on the *same* shape (see the
//!   `valued_readiness` bench), and the semiring op's share of closure time.
//!
//! The decision rule this enables (recorded in `BENCHMARKS.md`): stay on
//! built-in semirings while the carrier is scalar **or** `N` is small; reach
//! for a custom semiring only when a use case needs a *structured* carrier;
//! PreJIT only when the measured generic-kernel share × the generic→inlined
//! speedup actually crosses the latency SLO.

use std::time::Duration;

use crate::error::GrbError;
use crate::grb::ValuedMatrix;

/// Carrier shape required by a valued query/rule.
///
/// Drives the Fork-A vs Fork-B decision: a scalar carrier stays on built-in
/// semirings (Fork A); a structured carrier is the only thing that justifies a
/// user-defined semiring (Fork B).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CarrierShape {
    /// Single `f64` confidence/cost — Fork A, built-in semirings.
    Scalar,
    /// `(confidence, match-type, provenance, …)` tuple — Fork B, user semiring.
    Structured,
}

/// Per-run readiness metrics captured while computing a valued closure.
#[derive(Debug, Clone)]
pub struct ClosureMetrics {
    /// Matrix dimension `N` (distinct nodes).
    pub n: u64,
    /// Stored entries in the *input* matrix (`nnz`).
    pub input_nnz: u64,
    /// Stored entries in the *closure* result.
    pub closure_nnz: u64,
    /// Input density: `input_nnz / N²` (0 when `N == 0`).
    pub density: f64,
    /// Number of MxM iterations performed before reaching the fixed point.
    pub iterations_to_fixpoint: u32,
    /// Frontier `nnz` observed at each iteration — the per-iteration work
    /// profile. Length equals `iterations_to_fixpoint`.
    pub frontier_nnz_per_iter: Vec<u64>,
    /// Total wall time spent inside `GrB_mxm` calls.
    pub mxm_time: Duration,
    /// Total wall time of the whole closure (MxM + accumulate + nnz reads).
    pub total_time: Duration,
    /// Carrier shape this run used.
    pub carrier: CarrierShape,
}

impl ClosureMetrics {
    /// Fraction of total closure time spent inside the semiring `GrB_mxm`.
    /// This is the slice a faster kernel (JIT/PreJIT) could shrink.
    pub fn mxm_share(&self) -> f64 {
        let total = self.total_time.as_secs_f64();
        if total == 0.0 {
            0.0
        } else {
            self.mxm_time.as_secs_f64() / total
        }
    }

    /// Total frontier work summed across iterations (proxy for FLOP-shaped
    /// work the semiring performed).
    pub fn total_frontier_work(&self) -> u64 {
        self.frontier_nnz_per_iter.iter().sum()
    }
}

/// Whether to use the built-in `(max, ×)` FactoryKernel or a user-defined-op
/// generic kernel for the closure. Selecting `Udf` measures the
/// generic-kernel penalty end-to-end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValuedKernel {
    /// Prepackaged `GrB_MAX_TIMES_SEMIRING_FP64` (FactoryKernel).
    Builtin,
    /// `(max, ×)` assembled from user-defined ops (generic kernel).
    Udf,
}

/// Compute the valued `(max, ×)` transitive closure of `m`, capturing
/// readiness metrics. The result is `M⁺` under best-confidence-path semantics:
/// entry `(i, j)` is the maximum over all paths `i → j` of the product of edge
/// weights.
///
/// The identity is **not** included (matches the Boolean `transitive_closure`
/// convention): only reachable pairs appear.
///
/// **Weight contract:** edge weights are confidences/probabilities in `(0, 1]`.
/// Under that contract the best-confidence weight of every pair is realised by
/// a *simple* path, so the closure converges in ≤ `N` iterations. Out-of-range
/// weights (`> 1` over a cycle) can make the `(max, ×)` product diverge; an
/// `N`-iteration safety cap stops the loop rather than letting the readiness
/// measurement spin forever.
pub fn valued_transitive_closure(
    m: &ValuedMatrix,
    kernel: ValuedKernel,
) -> Result<(ValuedMatrix, ClosureMetrics), GrbError> {
    let n = m.nrows();
    let input_nnz = m.nvals()?;
    let density = if n == 0 {
        0.0
    } else {
        input_nnz as f64 / (n as f64 * n as f64)
    };

    let user_semiring = match kernel {
        ValuedKernel::Builtin => None,
        ValuedKernel::Udf => Some(crate::grb::UserSemiring::max_times_fp64()?),
    };

    let total_start = std::time::Instant::now();
    let mut mxm_time = Duration::ZERO;
    let mut frontier_nnz_per_iter = Vec::new();

    // Empty input → empty closure, zero iterations.
    if input_nnz == 0 {
        let out = ValuedMatrix::new(n)?;
        let metrics = ClosureMetrics {
            n,
            input_nnz,
            closure_nnz: 0,
            density,
            iterations_to_fixpoint: 0,
            frontier_nnz_per_iter,
            mxm_time,
            total_time: total_start.elapsed(),
            carrier: CarrierShape::Scalar,
        };
        return Ok((out, metrics));
    }

    let edges = m.extract_weighted_edges()?;
    let mut reach = ValuedMatrix::from_weighted_edges(n, &edges)?;
    let mut frontier = ValuedMatrix::from_weighted_edges(n, &edges)?;
    let mut iterations: u32 = 0;
    let mut prev_sum = reach.reduce_sum()?;

    loop {
        // Frontier := Frontier ⊗ M (one more hop), timing just the MxM.
        let mxm_start = std::time::Instant::now();
        let next_frontier = match &user_semiring {
            None => frontier.mxm_max_times_builtin(m)?,
            Some(s) => frontier.mxm_max_times_udf(m, s)?,
        };
        next_frontier.wait()?; // force materialisation so the timing is real
        mxm_time += mxm_start.elapsed();

        iterations += 1;
        let frontier_nnz = next_frontier.nvals()?;
        frontier_nnz_per_iter.push(frontier_nnz);
        if frontier_nnz == 0 {
            break;
        }

        reach.max_assign(&next_frontier)?;
        reach.wait()?;
        // Fixed point on the accumulator's *value*, not just its support: under
        // the monotone `(max, ×)` accumulation the total sum strictly increases
        // whenever any edge weight improves, including longer paths that
        // overwrite an already-present pair with a better confidence. A pure
        // `nnz`-stable check would stop too early on such weight-only gains.
        let new_sum = reach.reduce_sum()?;
        if new_sum == prev_sum {
            break;
        }
        prev_sum = new_sum;
        frontier = next_frontier;

        // Safety cap. With weights in `(0, 1]` the best-confidence weight of any
        // pair is realised by a *simple* path (length ≤ N-1), so the value sum
        // is monotone *bounded* and converges within N iterations. The cap only
        // bites on out-of-contract inputs (weights > 1 over a cycle, where the
        // `(max, ×)` product can grow without bound); it stops the readiness
        // measurement from looping forever rather than silently diverging.
        if iterations as u64 >= n {
            break;
        }
    }

    let closure_nnz = reach.nvals()?;
    let metrics = ClosureMetrics {
        n,
        input_nnz,
        closure_nnz,
        density,
        iterations_to_fixpoint: iterations,
        frontier_nnz_per_iter,
        mxm_time,
        total_time: total_start.elapsed(),
        carrier: CarrierShape::Scalar,
    };
    Ok((reach, metrics))
}
