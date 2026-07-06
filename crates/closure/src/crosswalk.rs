//! Fork A — valued best-confidence crosswalk closure (TASKS.md #12).
//!
//! This is the **scalar-confidence** fork of the valued-reasoning ladder: a
//! weighted concept/entity adjacency built from RDF 1.2 triple-term–annotated
//! edges (a confidence in `(0, 1]` per edge), closed under the built-in
//! `(max, ×)` "best-confidence path" semiring. It is the large win the #12
//! issue calls out over SPARQL property-path crawling for crosswalk
//! resolution (rdf-registry #10) and weighted-edge propagation (#9):
//!
//! - **Crosswalk resolution.** Given `skos:exactMatch` / `closeMatch` /
//!   `broadMatch` edges between vocabularies, each carrying a confidence on the
//!   annotating triple term, the best-confidence closure answers "what is the
//!   strongest mapping from concept `a` to concept `b`, over any chain of
//!   matches?" in one matrix closure instead of an unbounded property-path
//!   walk.
//! - **Weighted-edge propagation.** Same machinery propagates GTIO weighted
//!   relations (e.g. derived-from / influenced-by chains) and reports the
//!   strongest path weight.
//!
//! ## Why this stays on the built-in semiring (no JIT)
//!
//! The carrier is a single `f64`. The #11 readiness metrics measured the
//! generic-kernel (user-defined-op) penalty for exactly this scalar `(max, ×)`
//! op at **~1.0×** versus the prepackaged FactoryKernel — i.e. a JIT/PreJIT
//! kernel would buy ≈0. So Fork A uses [`ValuedKernel::Builtin`] and the whole
//! custom-semiring / PreJIT apparatus (Fork B / PreJIT) is **deferred** until a
//! use case needs a *structured* carrier `(confidence, match-type, provenance)`
//! that the built-in semirings cannot express. See `docs/specs/SPEC-05-*.md`
//! (valued-reasoning addendum) and `docs/benchmarks.md`.
//!
//! ## Dictionary IDs → dense matrix indices
//!
//! Callers key edges by **dictionary ID** (SPEC-02). [`CrosswalkGraph`] keeps a
//! per-graph dense renumbering (SPEC-05 F7) so the matrix stays compact even
//! when the dictionary IDs are sparse, and maps results back to dictionary IDs
//! on the way out. The closure itself never sees the original IDs.

use std::collections::HashMap;

use thiserror::Error;

use crate::error::GrbError;
use crate::grb::{init_once, ValuedMatrix};
use crate::metrics::{valued_transitive_closure, ClosureMetrics, ValuedKernel};

/// Errors building or resolving a [`CrosswalkGraph`].
#[derive(Debug, Error)]
pub enum CrosswalkError {
    /// An edge carried a confidence outside the `(0, 1]` contract the
    /// best-confidence `(max, ×)` closure relies on. Weights `> 1` can make the
    /// product diverge over a cycle (caught only by the `N`-iteration safety
    /// cap, yielding cap-dependent — i.e. unsound — answers); weights `≤ 0` are
    /// not confidences. Rejected at the boundary so the closure stays sound.
    #[error("edge {src}->{dst} has confidence {confidence}, expected a finite value in (0, 1]")]
    InvalidConfidence { src: u64, dst: u64, confidence: f64 },
    /// A GraphBLAS operation failed.
    #[error(transparent)]
    Grb(#[from] GrbError),
}

/// A single annotated crosswalk/relation edge: `src --confidence--> dst`,
/// keyed by dictionary IDs. `confidence` is the value carried on the RDF 1.2
/// triple term annotating the mapping, expected in `(0, 1]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CrosswalkEdge {
    /// Source concept/entity dictionary ID.
    pub src: u64,
    /// Target concept/entity dictionary ID.
    pub dst: u64,
    /// Edge confidence in `(0, 1]`.
    pub confidence: f64,
}

/// A best-confidence pair from the closure: the strongest path `src → dst`
/// has weight `confidence` (the product of edge confidences along the
/// best-confidence path).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CrosswalkPair {
    pub src: u64,
    pub dst: u64,
    pub confidence: f64,
}

/// A weighted crosswalk graph over dictionary IDs, ready for Fork-A
/// best-confidence closure.
///
/// Construct with [`CrosswalkGraph::from_edges`]; resolve with
/// [`CrosswalkGraph::best_confidence_closure`].
pub struct CrosswalkGraph {
    /// `dictionary_id → dense matrix index` (SPEC-05 F7 dense renumbering).
    id_to_dense: HashMap<u64, u64>,
    /// `dense matrix index → dictionary_id` (inverse, for result mapping).
    dense_to_id: Vec<u64>,
    /// The weighted adjacency in dense-index space.
    matrix: ValuedMatrix,
}

impl CrosswalkGraph {
    /// Build a crosswalk graph from annotated edges.
    ///
    /// Dictionary IDs are densely renumbered in first-appearance order.
    /// Duplicate `(src, dst)` edges keep the **maximum** confidence (matching
    /// the `(max, ×)` carrier — the strongest direct assertion wins).
    ///
    /// **Self-initialising:** calls [`init_once`] internally (idempotent), so a
    /// caller does not have to set GraphBLAS up first — this is the high-level
    /// Fork-A entry point.
    ///
    /// **Validation:** every edge confidence must be a finite value in
    /// `(0, 1]` (the contract the best-confidence closure relies on for
    /// soundness and convergence); otherwise returns
    /// [`CrosswalkError::InvalidConfidence`]. An empty edge list yields an
    /// empty graph (closure is then empty).
    pub fn from_edges(edges: &[CrosswalkEdge]) -> Result<Self, CrosswalkError> {
        // Self-initialise so the documented entry point works standalone.
        init_once()?;

        let mut id_to_dense: HashMap<u64, u64> = HashMap::new();
        let mut dense_to_id: Vec<u64> = Vec::new();

        // Intern a dictionary ID to a dense index in first-appearance order.
        fn intern(id: u64, id_to_dense: &mut HashMap<u64, u64>, dense_to_id: &mut Vec<u64>) -> u64 {
            if let Some(&d) = id_to_dense.get(&id) {
                d
            } else {
                let d = dense_to_id.len() as u64;
                id_to_dense.insert(id, d);
                dense_to_id.push(id);
                d
            }
        }

        let mut dense_edges: Vec<(u64, u64, f64)> = Vec::with_capacity(edges.len());
        for e in edges {
            if !(e.confidence.is_finite() && e.confidence > 0.0 && e.confidence <= 1.0) {
                return Err(CrosswalkError::InvalidConfidence {
                    src: e.src,
                    dst: e.dst,
                    confidence: e.confidence,
                });
            }
            let r = intern(e.src, &mut id_to_dense, &mut dense_to_id);
            let c = intern(e.dst, &mut id_to_dense, &mut dense_to_id);
            dense_edges.push((r, c, e.confidence));
        }

        let n = dense_to_id.len() as u64;
        let matrix = ValuedMatrix::from_weighted_edges(n, &dense_edges)?;
        Ok(Self {
            id_to_dense,
            dense_to_id,
            matrix,
        })
    }

    /// Number of distinct concepts/entities (matrix dimension `N`).
    pub fn n(&self) -> u64 {
        self.dense_to_id.len() as u64
    }

    /// Compute the best-confidence transitive closure over the built-in
    /// `(max, ×)` semiring (Fork A), returning every reachable pair with the
    /// confidence of its strongest path, plus the #11 readiness metrics.
    ///
    /// The identity is **not** included (only genuinely reachable pairs),
    /// matching the boolean `transitive_closure` convention.
    pub fn best_confidence_closure(
        &self,
    ) -> Result<(Vec<CrosswalkPair>, ClosureMetrics), GrbError> {
        let (star, metrics) = valued_transitive_closure(&self.matrix, ValuedKernel::Builtin)?;
        let dense_pairs = star.extract_weighted_edges()?;
        let pairs = dense_pairs
            .into_iter()
            .map(|(r, c, w)| CrosswalkPair {
                src: self.dense_to_id[r as usize],
                dst: self.dense_to_id[c as usize],
                confidence: w,
            })
            .collect();
        Ok((pairs, metrics))
    }

    /// Resolve the best-confidence mapping from one dictionary ID to another,
    /// if any path exists. Convenience over a full closure for point queries.
    ///
    /// Returns `Ok(None)` when `from`/`to` are unknown to the graph or no path
    /// connects them.
    pub fn best_confidence(&self, from: u64, to: u64) -> Result<Option<f64>, GrbError> {
        if !self.id_to_dense.contains_key(&from) || !self.id_to_dense.contains_key(&to) {
            return Ok(None);
        }
        let (pairs, _metrics) = self.best_confidence_closure()?;
        Ok(pairs
            .into_iter()
            .find(|p| p.src == from && p.dst == to)
            .map(|p| p.confidence))
    }
}
