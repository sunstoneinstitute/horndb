//! Cardinality estimator (Stage 1 stub).
//!
//! SPEC-03 F6 requires per-predicate histograms from SPEC-02. We don't have
//! those yet; this stub gives the planner *enough* signal to make the
//! WCOJ-vs-binary-join cutover decision in Task 12.

use crate::pattern::{Term, TriplePattern};
use crate::source::TripleSource;

pub trait Cardinality {
    /// Estimated number of matching triples for `pat`.
    fn estimate(&self, pat: &TriplePattern) -> usize;
}

pub struct UniformEstimator {
    total: usize,
}

impl UniformEstimator {
    pub fn from_source<S: TripleSource + ?Sized>(src: &S) -> Self {
        Self {
            total: src.total_triples(),
        }
    }
}

impl Cardinality for UniformEstimator {
    fn estimate(&self, pat: &TriplePattern) -> usize {
        // Each bound position multiplies the selectivity by `1/16` — a
        // deliberately coarse "uniform & moderately selective" prior. The
        // real estimator (Stage 2) reads histograms from SPEC-02.
        let mut sel: f64 = 1.0;
        for t in [pat.s, pat.p, pat.o] {
            if matches!(t, Term::Bound(_)) {
                sel *= 1.0 / 16.0;
            }
        }
        ((self.total as f64) * sel).round().max(1.0) as usize
    }
}
