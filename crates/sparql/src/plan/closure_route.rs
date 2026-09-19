//! Which backend evaluates a `PathClosure` node (SPEC-07 F3/F8).
//!
//! A Kleene property path (`p+`, `p*`) can be closed two ways:
//!
//! * **Native** — the BFS fixpoint in `exec::runtime::eval_path_closure`.
//!   Always available.
//! * **GraphBLAS** — the SPEC-05 closure operator (`horndb_closure`), which
//!   closes a Boolean adjacency matrix by iterated sparse matrix multiply.
//!   Only compiled in under the `graphblas` feature.
//!
//! The planner picks one by selectivity: it uses the `Stats`-backed estimate
//! of the `edge` sub-plan (the one-step relation the path denotes) that
//! `EXPLAIN` already computes. A small edge relation stays native — the
//! matrix build and FFI round trip are not worth it. A large one is where
//! the matrix kernel earns its keep.
//!
//! Both backends consume the *same* decoded edge rows and produce the same
//! pair set, so routing never changes an answer; it only changes which
//! kernel computes the fixpoint. That is what makes the choice safe to make
//! from an estimate, which may be wrong.

use crate::exec::Executor;
use crate::plan::explain::estimate;
use crate::plan::PhysicalPlan;
use std::sync::OnceLock;

/// The backend chosen for one `PathClosure` node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClosureRoute {
    /// BFS fixpoint in the SPARQL runtime.
    Native,
    /// SPEC-05 GraphBLAS closure operator.
    GraphBlas,
}

impl ClosureRoute {
    /// Stable name used in `EXPLAIN` output.
    pub fn label(self) -> &'static str {
        match self {
            ClosureRoute::Native => "native",
            ClosureRoute::GraphBlas => "graphblas",
        }
    }
}

/// Default edge-relation size at or above which a closure is delegated to
/// GraphBLAS.
///
/// **Unmeasured placeholder.** The real crossover has not been measured;
/// benchmark numbers for this repo must come from the `hornbench` host, and
/// that calibration run is still outstanding. Treat this value as a starting
/// point, not a result.
pub const GRAPHBLAS_MIN_EDGE_ROWS: usize = 100_000;

/// Env override for [`GRAPHBLAS_MIN_EDGE_ROWS`], so the calibration run can
/// sweep the threshold without a rebuild. Read once per process.
pub const GRAPHBLAS_MIN_EDGE_ROWS_ENV: &str = "HORNDB_PATH_CLOSURE_GRAPHBLAS_MIN_ROWS";

fn min_edge_rows() -> usize {
    static CELL: OnceLock<usize> = OnceLock::new();
    *CELL.get_or_init(|| {
        std::env::var(GRAPHBLAS_MIN_EDGE_ROWS_ENV)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(GRAPHBLAS_MIN_EDGE_ROWS)
    })
}

/// Pick the backend for a `PathClosure` whose one-step relation is `edge`.
///
/// Without the `graphblas` feature there is nothing to delegate to, so this
/// is always [`ClosureRoute::Native`] and plans are byte-identical to a
/// build that never had this module.
pub fn path_closure_route<E: Executor + ?Sized>(edge: &PhysicalPlan, exec: &E) -> ClosureRoute {
    if !cfg!(feature = "graphblas") {
        return ClosureRoute::Native;
    }
    route_for(estimate(edge, exec), min_edge_rows())
}

/// The selectivity rule itself, split out so it is testable without a store.
/// An unknown estimate stays native — delegation is an optimization, and a
/// backend that cannot estimate gives no reason to take it.
fn route_for(edge_estimate: Option<usize>, min_rows: usize) -> ClosureRoute {
    match edge_estimate {
        Some(n) if n >= min_rows => ClosureRoute::GraphBlas,
        _ => ClosureRoute::Native,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_by_edge_relation_size() {
        assert_eq!(route_for(Some(10), 100), ClosureRoute::Native);
        assert_eq!(route_for(Some(99), 100), ClosureRoute::Native);
        assert_eq!(route_for(Some(100), 100), ClosureRoute::GraphBlas);
        assert_eq!(route_for(Some(1_000), 100), ClosureRoute::GraphBlas);
    }

    #[test]
    fn unknown_estimate_stays_native() {
        assert_eq!(route_for(None, 0), ClosureRoute::Native);
        assert_eq!(route_for(None, 100), ClosureRoute::Native);
    }
}
