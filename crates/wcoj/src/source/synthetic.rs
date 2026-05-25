//! Synthetic graph generators for benchmarks.
//!
//! `SyntheticGraph::cyclic(n, k, p, seed)` produces a directed graph of `n`
//! vertices (IDs `0..n`) where each vertex has `k` outgoing edges to
//! pseudo-randomly chosen other vertices, all with predicate `p`. Vertex
//! IDs are dense, edges are uniform — this is the canonical benchmark
//! shape from the WCOJ literature.

use std::collections::BTreeSet;

use crate::ids::{Ordering, Triple};
use crate::source::vec_source::{VecIter, VecTripleSource};
use crate::source::TripleSource;

pub struct SyntheticGraph {
    inner: VecTripleSource,
}

impl SyntheticGraph {
    pub fn cyclic(n: u64, k: u64, predicate: u64, seed: u64) -> Self {
        // Simple xorshift RNG, deterministic given seed.
        let mut state = seed | 1;
        let mut rand = || -> u64 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };

        let mut edges: BTreeSet<Triple> = BTreeSet::new();
        for s in 0..n {
            let mut added = 0;
            while added < k {
                let o = rand() % n;
                if o == s {
                    continue;
                }
                if edges.insert(Triple::new(s, predicate, o)) {
                    added += 1;
                }
            }
        }
        let triples: Vec<Triple> = edges.into_iter().collect();
        Self {
            inner: VecTripleSource::from_triples(triples),
        }
    }
}

impl TripleSource for SyntheticGraph {
    type Iter<'a> = VecIter<'a>;
    fn iter(&self, ord: Ordering) -> crate::error::Result<VecIter<'_>> {
        self.inner.iter(ord)
    }
    fn total_triples(&self) -> usize {
        self.inner.total_triples()
    }
    fn supports(&self, ord: Ordering) -> bool {
        self.inner.supports(ord)
    }
}
