//! `StoreTripleSource` — a WCOJ [`TripleSource`] read straight off the
//! columnar partitions, with no per-query copy of the store (HDB-120).
//!
//! [`super::horn::HornBackend`] used to answer every query from a
//! `VecTripleSource`: a sorted copy of the scope's triples, up to six
//! orderings at 24 bytes/triple each, rebuilt or delta-merged on every write
//! and built-and-dropped per execution for every graph-scoped read. The store
//! already holds the same rows, sorted, in `horndb-storage`'s per-predicate
//! partitions — one subject-major `(s, o)` column pair and, on demand, one
//! object-major `(o, s)` pair. This source hands those columns to
//! [`MergedIter`], which merges the per-predicate blocks into the six global
//! trie orderings on the fly.
//!
//! What that buys and costs:
//!
//! * **Buys:** no per-query sorted copy. The columns come out of
//!   [`PredicatePartition::ordered_at`] as `Arc` clones of the stored Arrow
//!   buffers, so a scope with no retracted rows copies nothing at all. The
//!   SPEC-25 S1 copy-on-write snapshot already gives a stable view, so this
//!   source needs no locking of its own.
//! * **Costs:** every level operation is a linear pass over the live leaves
//!   rather than one binary search over a flat column, and the SIMD-intersect
//!   fast path (`OrderedTripleIter::active_run`) is not implemented — see
//!   [`horndb_wcoj::source::merged`]. Those two together measured **2-8x
//!   slower** on warm trainmarks-medium reads in a laptop smoke A/B, which is
//!   why `HornBackend` keeps this source behind `HORNDB_DIRECT_SOURCE=1`.
//!
//! # Scope
//!
//! One graph only. [`MergedIter`] relies on leaf keys being distinct, which
//! holds for the predicates of a single graph but not for a union of several
//! (the same predicate would appear once per graph, and a triple present in
//! two graphs must yield one row, not two). `HornBackend` keeps the
//! `VecTripleSource` path for a genuine multi-graph union — see
//! `HornBackend::query_source`.

use std::sync::Arc;

use horndb_storage::partition::OrderedColumns;
use horndb_storage::{GraphId, TermId, TierSnapshot};
use horndb_wcoj::error::Result;
use horndb_wcoj::ids::{Ordering, Triple as WTriple};
use horndb_wcoj::source::merged::{MergedIter, MergedLeaf};
use horndb_wcoj::source::TripleSource;

/// The two physical column layouts a partition can serve. Mirrors
/// `horndb_storage::ordering::PartitionAxis`, indexed here so the two lazily
/// built leaf sets can live in a fixed-size array.
const SUBJECT_MAJOR: usize = 0;
const OBJECT_MAJOR: usize = 1;

/// Which axis serves `ord` — the same split `horndb_storage::Ordering::axis`
/// makes, restated over the WCOJ ordering enum.
fn axis_of(ord: Ordering) -> usize {
    match ord {
        Ordering::Spo | Ordering::Sop | Ordering::Pso => SUBJECT_MAJOR,
        Ordering::Pos | Ordering::Osp | Ordering::Ops => OBJECT_MAJOR,
    }
}

fn storage_ordering(axis: usize) -> horndb_storage::ordering::Ordering {
    if axis == SUBJECT_MAJOR {
        horndb_storage::ordering::Ordering::Pso
    } else {
        horndb_storage::ordering::Ordering::Pos
    }
}

pub struct StoreTripleSource {
    tier: Arc<TierSnapshot>,
    graph: GraphId,
    /// Predicate ids in this graph, ascending. Held so a lazily built axis
    /// visits them in leaf-key order.
    predicates: Vec<TermId>,
    /// Per-axis `(predicate, columns)` leaves, built on first use of an
    /// ordering that needs that axis. `OnceLock` because the object-major
    /// layout costs a per-partition sort the first time anything asks for it,
    /// and most queries never do (HDB-97's reasoning, applied to the axis
    /// rather than to all six orderings).
    axes: [std::sync::OnceLock<Vec<(TermId, OrderedColumns)>>; 2],
    total: usize,
}

impl StoreTripleSource {
    /// Open a source over the visible triples of `graph` in `tier`.
    pub fn new(tier: Arc<TierSnapshot>, graph: GraphId) -> Self {
        let mut predicates = tier.predicates(graph);
        predicates.sort_by_key(|p| p.0);
        let total = tier.graph_len(graph);
        Self {
            tier,
            graph,
            predicates,
            axes: [std::sync::OnceLock::new(), std::sync::OnceLock::new()],
            total,
        }
    }

    fn leaves(&self, axis: usize) -> &[(TermId, OrderedColumns)] {
        self.axes[axis].get_or_init(|| {
            let ord = storage_ordering(axis);
            self.predicates
                .iter()
                .filter_map(|&p| {
                    self.tier
                        .ordered_predicate_at(self.graph, p, ord)
                        .filter(|c| !c.is_empty())
                        .map(|c| (p, c))
                })
                .collect()
        })
    }

    /// Is `t` a visible triple of this graph? The ground-pattern membership
    /// test `HornBackend` runs before executing a BGP.
    pub fn contains(&self, t: &WTriple) -> bool {
        self.tier
            .with_predicate(self.graph, TermId(t.p), |part| {
                part.contains_at(TermId(t.s), TermId(t.o), self.tier.version())
            })
            .unwrap_or(false)
    }
}

impl TripleSource for StoreTripleSource {
    type Iter<'a> = MergedIter<'a>;

    fn iter(&self, ord: Ordering) -> Result<MergedIter<'_>> {
        let leaves = self
            .leaves(axis_of(ord))
            .iter()
            .map(|(p, cols)| MergedLeaf {
                key: p.0,
                level0: cols.level0().values(),
                level1: cols.level1().values(),
            })
            .collect();
        Ok(MergedIter::new(ord, leaves))
    }

    fn total_triples(&self) -> usize {
        self.total
    }
}

/// The source one query execution reads from: the direct partition source
/// where the scope allows it, the `VecTripleSource` copy otherwise.
///
/// An enum rather than `&dyn TripleSource` because [`TripleSource`]'s iterator
/// is a generic associated type (deliberately, so the leapfrog hot path
/// monomorphises), which makes the trait non-object-safe.
pub enum QuerySource {
    Direct(Arc<StoreTripleSource>),
    Copy(Arc<horndb_wcoj::source::vec_source::VecTripleSource>),
}

/// [`QuerySource`]'s cursor. Every method forwards; the compiler sees one
/// branch per call, not a vtable.
pub enum QueryIter<'a> {
    Direct(MergedIter<'a>),
    Copy(horndb_wcoj::source::vec_source::VecIter<'a>),
}

macro_rules! forward {
    ($self:ident, $it:ident => $call:expr) => {
        match $self {
            QueryIter::Direct($it) => $call,
            QueryIter::Copy($it) => $call,
        }
    };
}

impl horndb_wcoj::source::OrderedTripleIter for QueryIter<'_> {
    #[inline]
    fn peek(&self, depth: u8) -> Option<u64> {
        forward!(self, it => it.peek(depth))
    }
    #[inline]
    fn seek(&mut self, depth: u8, value: u64) {
        forward!(self, it => it.seek(depth, value))
    }
    #[inline]
    fn open_level(&mut self, depth: u8) {
        forward!(self, it => it.open_level(depth))
    }
    #[inline]
    fn up(&mut self, depth: u8) {
        forward!(self, it => it.up(depth))
    }
    #[inline]
    fn rewind(&mut self, depth: u8) {
        forward!(self, it => it.rewind(depth))
    }
    #[inline]
    fn active_run(&mut self, depth: u8) -> Option<&[u64]> {
        forward!(self, it => it.active_run(depth))
    }
}

impl QuerySource {
    /// Is `t` a visible triple of this scope?
    pub fn contains(&self, t: &WTriple) -> bool {
        match self {
            QuerySource::Direct(s) => s.contains(t),
            QuerySource::Copy(s) => s.contains(t),
        }
    }
}

impl TripleSource for QuerySource {
    type Iter<'a> = QueryIter<'a>;

    fn iter(&self, ord: Ordering) -> Result<QueryIter<'_>> {
        Ok(match self {
            QuerySource::Direct(s) => QueryIter::Direct(s.iter(ord)?),
            QuerySource::Copy(s) => QueryIter::Copy(s.iter(ord)?),
        })
    }

    fn total_triples(&self) -> usize {
        match self {
            QuerySource::Direct(s) => s.total_triples(),
            QuerySource::Copy(s) => s.total_triples(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use horndb_storage::{Store, DEFAULT_GRAPH};
    use horndb_wcoj::source::vec_source::VecTripleSource;
    use oxrdf::{NamedNode, Term as OxTerm};
    use std::collections::BTreeSet;

    fn iri(n: u64) -> OxTerm {
        OxTerm::NamedNode(NamedNode::new(format!("http://e/{n}")).unwrap())
    }

    /// Every triple the source yields in `ord`, read through the trie
    /// contract alone.
    fn walk<S: TripleSource>(src: &S, ord: Ordering) -> BTreeSet<(u64, u64, u64)> {
        use horndb_wcoj::source::OrderedTripleIter;
        let mut it = src.iter(ord).unwrap();
        let mut out = BTreeSet::new();
        while let Some(a) = it.peek(0) {
            it.open_level(1);
            while let Some(b) = it.peek(1) {
                it.open_level(2);
                while let Some(c) = it.peek(2) {
                    let [s, p, o] = ord.unpermute(a, b, c);
                    out.insert((s, p, o));
                    it.seek(2, c.wrapping_add(1));
                }
                it.up(2);
                it.seek(1, b.wrapping_add(1));
            }
            it.up(1);
            it.seek(0, a.wrapping_add(1));
        }
        out
    }

    #[test]
    fn matches_a_vec_source_over_the_same_store() {
        let store = Store::in_memory();
        let mut triples = Vec::new();
        for s in 0..7u64 {
            for p in 0..3u64 {
                for o in 0..(s % 4) {
                    triples.push((iri(s), iri(100 + p), iri(200 + o)));
                }
            }
        }
        store.insert_triples(&triples).unwrap();
        // A retraction so the version filter (and its non-zero-copy path) is
        // exercised, not just the insert-only fast path.
        store.retract_triples(&triples[..2]).unwrap();

        let snap = store.snapshot();
        let rows: Vec<WTriple> = snap
            .iter_graph_term_ids(DEFAULT_GRAPH)
            .map(|(s, p, o)| WTriple::new(s.0, p.0, o.0))
            .collect();
        let vec_src = VecTripleSource::from_triples(rows.clone());
        let direct = StoreTripleSource::new(snap.tier_arc(), DEFAULT_GRAPH);

        assert_eq!(direct.total_triples(), rows.len());
        for ord in Ordering::ALL {
            assert_eq!(walk(&direct, ord), walk(&vec_src, ord), "{ord:?}");
        }
        for t in &rows {
            assert!(direct.contains(t));
        }
        assert!(!direct.contains(&WTriple::new(rows[0].s, rows[0].p, u64::MAX)));
    }

    #[test]
    fn an_empty_graph_yields_nothing() {
        let store = Store::in_memory();
        let src = StoreTripleSource::new(store.snapshot().tier_arc(), DEFAULT_GRAPH);
        assert_eq!(src.total_triples(), 0);
        for ord in Ordering::ALL {
            assert!(walk(&src, ord).is_empty());
        }
    }
}
