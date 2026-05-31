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
    /// Deterministically generate the cyclic graph's edges (no source built).
    /// Exposed so benches can build both a dense and a compressed source from
    /// identical edges.
    pub fn cyclic_edges(n: u64, k: u64, predicate: u64, seed: u64) -> Vec<Triple> {
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
        edges.into_iter().collect()
    }

    pub fn cyclic(n: u64, k: u64, predicate: u64, seed: u64) -> Self {
        let triples = Self::cyclic_edges(n, k, predicate, seed);
        Self {
            inner: VecTripleSource::from_triples(triples),
        }
    }

    /// Deterministically generate the edges of the *canonical WCOJ-win*
    /// 4-cycle graph (no source built).
    ///
    /// The graph has four disjoint vertex layers wired A→B→C→D with a thin,
    /// dedicated D→A closure, all on a single predicate. It is designed so the
    /// 4-cycle query `(a)-p->(b)-p->(c)-p->(d)-p->(a)` exhibits the asymptotic
    /// gap that makes a worst-case-optimal join dominate a binary join.
    ///
    /// The crux is to **decouple the sources that drive the binary-join
    /// blow-up from the sources that actually close a cycle**:
    ///
    /// * **A→B** stem: every one of `params.sources` vertices gets `a_out`
    ///   out-edges into its own slice of B (so all of B is reachable).
    /// * **B→C** fans *every* reachable B vertex into *all* `params.hubs`
    ///   hubs, so every `a→b` extends to every hub. These edges are the bulk
    ///   of the graph and their count equals the number of 2-paths.
    /// * **C→D** gives each hub out-degree `params.hub_out` into the *bulk*
    ///   sink pool. A binary-hash join materialises the full 3-path relation
    ///   `(a,b,c,d)` — size `#2-paths · hub_out` — over **all** sources before
    ///   it can apply the closure. That is the work WCOJ avoids.
    /// * **closure**: only the first `params.close_sources` source vertices
    ///   are cycle-closable. A dedicated set of `params.close_sinks` closing
    ///   sinks (their own ID range, *not* in the bulk pool) is wired
    ///   `hub₀ → close_sink → close_source`. So the only 4-cycles run
    ///   `a → b → hub₀ → close_sink → a`, giving a small, exactly-predictable
    ///   output of `close_sources · a_out · close_sinks` rows.
    ///
    /// WCOJ binds variables in order `[a,b,c,d]`; because `a` is shared by the
    /// first and last atom, the depth-0 leapfrog intersects "sources with an
    /// out-edge" with "sources with an in-edge" (= the `close_sources`), so it
    /// only ever explores cycles rooted at a closure target. Binary-hash, by
    /// contrast, materialises 3-paths for every source. The result is a
    /// speedup that grows with `sources · hub_out`.
    pub fn skewed_four_cycle_edges(params: &SkewedFourCycle) -> Vec<Triple> {
        let SkewedFourCycle {
            sources,
            a_out,
            hubs,
            hub_out,
            bulk_sinks,
            close_sources,
            close_sinks,
            predicate,
            seed,
        } = *params;

        assert!(sources > 0 && a_out > 0, "need a non-empty A→B stem");
        assert!(hubs > 0 && hub_out > 0, "need hubs with out-edges");
        assert!(bulk_sinks > 0, "need a non-empty bulk sink pool");
        assert!(hub_out <= bulk_sinks, "hub_out cannot exceed |bulk sinks|");
        assert!(
            close_sources > 0 && close_sources <= sources,
            "close_sources must be in 1..=sources"
        );
        assert!(close_sinks > 0, "need at least one closing sink");

        let middles = sources.saturating_mul(a_out);
        // Disjoint, dense vertex-ID ranges per layer.
        let a_lo = 0u64;
        let b_lo = a_lo + sources;
        let c_lo = b_lo + middles;
        let d_lo = c_lo + hubs; // bulk sink pool
        let dclose_lo = d_lo + bulk_sinks; // dedicated closing sinks

        let mut state = seed | 1;
        let mut rand = move || -> u64 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };

        // Sample `count` distinct offsets in `[0, range)` (range >= count by
        // the asserts); linear-probe on collision so we get exactly `count`.
        let distinct = |rand: &mut dyn FnMut() -> u64, range: u64, count: u64| -> Vec<u64> {
            let mut seen: BTreeSet<u64> = BTreeSet::new();
            while (seen.len() as u64) < count {
                let mut v = rand() % range;
                while !seen.insert(v) {
                    v = (v + 1) % range;
                }
            }
            seen.into_iter().collect()
        };

        let mut edges: BTreeSet<Triple> = BTreeSet::new();

        // A→B: stem. Each source s gets its own contiguous slice of B.
        for s in 0..sources {
            for j in 0..a_out {
                let b = b_lo + s * a_out + j;
                edges.insert(Triple::new(a_lo + s, predicate, b));
            }
        }

        // B→C: every reachable B vertex fans into every hub (the bulk).
        for b in 0..middles {
            for c in 0..hubs {
                edges.insert(Triple::new(b_lo + b, predicate, c_lo + c));
            }
        }

        // C→D: each hub gets `hub_out` distinct *bulk* sink targets (blow-up).
        for c in 0..hubs {
            for off in distinct(&mut rand, bulk_sinks, hub_out) {
                edges.insert(Triple::new(c_lo + c, predicate, d_lo + off));
            }
        }

        // Dedicated closure: hub₀ → each closing sink → each closure source.
        // Closing sinks live outside the bulk pool, so no other hub reaches
        // them and the 4-cycle output stays exactly
        // `close_sources * a_out * close_sinks`.
        for k in 0..close_sinks {
            let dclose = dclose_lo + k;
            edges.insert(Triple::new(c_lo, predicate, dclose)); // hub 0 → closing sink
            for a in 0..close_sources {
                edges.insert(Triple::new(dclose, predicate, a_lo + a)); // closing sink → source
            }
        }

        edges.into_iter().collect()
    }

    /// Build a [`SyntheticGraph`] over the canonical WCOJ-win 4-cycle graph.
    pub fn skewed_four_cycle(params: &SkewedFourCycle) -> Self {
        Self {
            inner: VecTripleSource::from_triples(Self::skewed_four_cycle_edges(params)),
        }
    }
}

/// Parameters for [`SyntheticGraph::skewed_four_cycle_edges`]. See that
/// method for the meaning of each layer and how the parameters drive the
/// WCOJ-vs-binary-join asymptotic gap.
#[derive(Debug, Clone, Copy)]
pub struct SkewedFourCycle {
    /// `|A|` — every source generates 2-paths (drives the binary blow-up).
    pub sources: u64,
    /// A→B out-degree per source.
    pub a_out: u64,
    /// `|C|` — number of hubs every reachable B vertex fans into.
    pub hubs: u64,
    /// C→D out-degree per hub — the binary-join 3-path blow-up factor.
    pub hub_out: u64,
    /// Size of the bulk sink pool hubs draw their out-edges from.
    pub bulk_sinks: u64,
    /// Number of leading source vertices that are cycle-closable (keep small).
    pub close_sources: u64,
    /// Number of dedicated closing sinks (keep small); 4-cycle output is
    /// `close_sources * a_out * close_sinks`.
    pub close_sinks: u64,
    /// Single edge predicate.
    pub predicate: u64,
    /// RNG seed (deterministic output).
    pub seed: u64,
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
