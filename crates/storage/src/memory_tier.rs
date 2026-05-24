//! In-memory tier — Stage 1 sole implementation of `Tier`.

use crate::error::Result;
use crate::partition::{PartitionBuilder, PredicatePartition};
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
}

#[derive(Default)]
struct Inner {
    graphs: HashMap<GraphId, GraphStore>,
}

impl MemoryTier {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(Inner::default()),
        }
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
            gs.partitions.insert(p, builder.build());
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
        // Each row: 8 bytes subject + 8 bytes object = 16 bytes; plus ~16 bytes/predicate overhead.
        let bytes_estimated = triples * 16 + predicates * 16;
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
