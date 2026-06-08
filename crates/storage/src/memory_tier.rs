//! In-memory tier — Stage 1 sole implementation of `Tier`.

use crate::error::Result;
use crate::partition::{PartitionBuilder, PredicatePartition, DEFAULT_HOT_THRESHOLD};
use crate::term::{GraphId, TermId};
use crate::tier::{Tier, TierStats};
use parking_lot::RwLock;
use std::collections::HashMap;

#[derive(Default)]
struct GraphStore {
    partitions: HashMap<TermId, PredicatePartition>,
}

pub struct MemoryTier {
    inner: RwLock<Inner>,
    /// Predicates with at least this many triples eagerly materialise all six
    /// orderings; smaller ones materialise the object-major layout lazily
    /// (SPEC-02 F4).
    hot_threshold: usize,
}

#[derive(Default)]
struct Inner {
    graphs: HashMap<GraphId, GraphStore>,
}

impl MemoryTier {
    pub fn new() -> Self {
        Self::with_hot_threshold(DEFAULT_HOT_THRESHOLD)
    }

    /// Construct a tier with a custom hot-predicate threshold (SPEC-02 F4).
    pub fn with_hot_threshold(hot_threshold: usize) -> Self {
        Self {
            inner: RwLock::new(Inner::default()),
            hot_threshold,
        }
    }

    /// The hot-predicate triple-count threshold in effect for this tier.
    pub fn hot_threshold(&self) -> usize {
        self.hot_threshold
    }
}

impl Default for MemoryTier {
    fn default() -> Self {
        Self::new()
    }
}

impl Tier for MemoryTier {
    fn insert_quad_batch(&self, quads: &[(GraphId, TermId, TermId, TermId)]) -> Result<()> {
        // Group by (graph, predicate) into builders, merging with any existing
        // partition by replaying its existing pairs into the new builder.
        let mut groups: HashMap<(GraphId, TermId), PartitionBuilder> = HashMap::new();
        for &(g, s, p, o) in quads {
            groups.entry((g, p)).or_default().append(s, o);
        }
        let mut inner = self.inner.write();
        for ((g, p), mut builder) in groups {
            let gs = inner.graphs.entry(g).or_default();
            if let Some(existing) = gs.partitions.remove(&p) {
                for (s, o) in existing.scan() {
                    builder.append(s, o);
                }
            }
            gs.partitions
                .insert(p, builder.build_with_hot_threshold(self.hot_threshold));
        }
        Ok(())
    }

    fn predicate(&self, _graph: GraphId, _predicate: TermId) -> Option<&PredicatePartition> {
        // SAFETY caveat: returning `&PredicatePartition` across the RwLock
        // would require a guard-bound borrow. For Stage 1 we expose a guarded
        // accessor via `with_predicate` below; this trait method returns None
        // and is kept only for forward compatibility with a future ArcSwap
        // layout. Callers in Stage 1 use `MemoryTier::with_predicate`.
        None
    }

    fn predicates(&self, graph: GraphId) -> Vec<TermId> {
        let inner = self.inner.read();
        inner
            .graphs
            .get(&graph)
            .map(|gs| gs.partitions.keys().copied().collect())
            .unwrap_or_default()
    }

    fn graphs(&self) -> Vec<GraphId> {
        self.inner.read().graphs.keys().copied().collect()
    }

    fn triple_count(&self) -> u64 {
        let inner = self.inner.read();
        inner
            .graphs
            .values()
            .flat_map(|g| g.partitions.values())
            .map(|p| p.len() as u64)
            .sum()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn stats(&self) -> TierStats {
        let inner = self.inner.read();
        let graphs = inner.graphs.len() as u64;
        let predicates: u64 = inner
            .graphs
            .values()
            .map(|g| g.partitions.len() as u64)
            .sum();
        let triples: u64 = inner
            .graphs
            .values()
            .flat_map(|g| g.partitions.values())
            .map(|p| p.len() as u64)
            .sum();
        // Per-partition column footprint (16 B/row, doubled when the
        // object-major layout is materialised for a hot predicate); plus
        // ~16 bytes/predicate overhead.
        let column_bytes: u64 = inner
            .graphs
            .values()
            .flat_map(|g| g.partitions.values())
            .map(|p| p.estimated_bytes())
            .sum();
        let bytes_estimated = column_bytes + predicates * 16;
        TierStats {
            graphs,
            predicates,
            triples,
            bytes_estimated,
        }
    }
}

impl MemoryTier {
    /// Guarded accessor for a partition. The closure runs with a read-lock held.
    pub fn with_predicate<F, R>(&self, graph: GraphId, predicate: TermId, f: F) -> Option<R>
    where
        F: FnOnce(&PredicatePartition) -> R,
    {
        let inner = self.inner.read();
        inner
            .graphs
            .get(&graph)
            .and_then(|gs| gs.partitions.get(&predicate))
            .map(f)
    }

    /// Ordered access to a predicate partition in any of the six trie orderings
    /// (SPEC-02 F4). Returns [`crate::partition::OrderedColumns`], which owns
    /// `Arc` clones of the columns and so outlives the read-lock taken here.
    ///
    /// For a cold predicate's object-major orderings this materialises (and
    /// caches) the object-major layout on first call; the materialisation runs
    /// under the read-lock but is internally synchronised, so it never blocks
    /// concurrent readers of *other* partitions.
    pub fn ordered_predicate(
        &self,
        graph: GraphId,
        predicate: TermId,
        ord: crate::ordering::Ordering,
    ) -> Option<crate::partition::OrderedColumns> {
        let inner = self.inner.read();
        inner
            .graphs
            .get(&graph)
            .and_then(|gs| gs.partitions.get(&predicate))
            .map(|part| part.ordered(ord))
    }

    /// The top-`n` predicates in `graph` by triple count, descending. Ties are
    /// broken by predicate id for a deterministic order. Drives the SPEC-02
    /// acceptance check that the hottest predicates are queryable in all six
    /// orderings.
    pub fn top_predicates(&self, graph: GraphId, n: usize) -> Vec<(TermId, u64)> {
        let inner = self.inner.read();
        let mut counts: Vec<(TermId, u64)> = inner
            .graphs
            .get(&graph)
            .map(|gs| {
                gs.partitions
                    .iter()
                    .map(|(p, part)| (*p, part.len() as u64))
                    .collect()
            })
            .unwrap_or_default();
        counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0 .0.cmp(&b.0 .0)));
        counts.truncate(n);
        counts
    }
}

#[cfg(test)]
mod tests {
    use crate::memory_tier::MemoryTier;
    use crate::term::{GraphId, TermId, TermKind, DEFAULT_GRAPH};
    use crate::tier::Tier;

    fn id(payload: u64) -> TermId {
        TermId::new(TermKind::Uri, payload)
    }

    #[test]
    fn insert_and_count() {
        let tier = MemoryTier::new();
        let quads = vec![
            (DEFAULT_GRAPH, id(1), id(100), id(2)),
            (DEFAULT_GRAPH, id(1), id(100), id(3)),
            (DEFAULT_GRAPH, id(1), id(101), id(2)),
        ];
        tier.insert_quad_batch(&quads).unwrap();
        assert_eq!(tier.triple_count(), 3);
        let mut preds = tier.predicates(DEFAULT_GRAPH);
        preds.sort_by_key(|t| t.0);
        assert_eq!(preds, vec![id(100), id(101)]);
    }

    #[test]
    fn batched_inserts_merge_into_one_partition() {
        let tier = MemoryTier::new();
        tier.insert_quad_batch(&[(DEFAULT_GRAPH, id(1), id(100), id(2))])
            .unwrap();
        tier.insert_quad_batch(&[(DEFAULT_GRAPH, id(3), id(100), id(4))])
            .unwrap();
        let pairs = tier
            .with_predicate(DEFAULT_GRAPH, id(100), |p| p.scan().collect::<Vec<_>>())
            .unwrap();
        assert_eq!(pairs.len(), 2);
        // SPO sort: subject 1 < subject 3.
        assert_eq!(pairs[0].0, id(1));
        assert_eq!(pairs[1].0, id(3));
    }

    #[test]
    fn named_graphs_are_isolated() {
        let tier = MemoryTier::new();
        let g1 = GraphId(TermId::new(TermKind::Uri, 10).0);
        let g2 = GraphId(TermId::new(TermKind::Uri, 11).0);
        tier.insert_quad_batch(&[(g1, id(1), id(100), id(2)), (g2, id(1), id(100), id(3))])
            .unwrap();
        let g1_pairs = tier
            .with_predicate(g1, id(100), |p| p.scan().collect::<Vec<_>>())
            .unwrap();
        let g2_pairs = tier
            .with_predicate(g2, id(100), |p| p.scan().collect::<Vec<_>>())
            .unwrap();
        assert_eq!(g1_pairs, vec![(id(1), id(2))]);
        assert_eq!(g2_pairs, vec![(id(1), id(3))]);
    }
}
