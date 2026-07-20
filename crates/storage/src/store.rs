//! Public store facade.
//!
//! Composes a `Dictionary` with one `Tier` implementation. Stage 1 only
//! supports an in-memory tier; the constructor signature leaves room for
//! plugging in cold tiers later.

use crate::dictionary::Dictionary;
use crate::error::Result;
use crate::memory_tier::MemoryTier;
use crate::ordering::Ordering;
use crate::term::{GraphId, TermId, DEFAULT_GRAPH};
use crate::tier::{Tier, TierStats};
use oxrdf::Term;

#[derive(Debug, Clone, Copy)]
pub struct FootprintReport {
    pub triples: u64,
    pub bytes_estimated: u64,
    pub bytes_per_triple: f64,
}

pub struct Store {
    dictionary: Dictionary,
    tier: Box<dyn Tier>,
}

impl Store {
    pub fn in_memory() -> Self {
        Self {
            dictionary: Dictionary::new(),
            tier: Box::new(MemoryTier::new()),
        }
    }

    /// In-memory store with a custom hot-predicate threshold (SPEC-02 F4):
    /// predicates with at least `hot_threshold` triples eagerly materialise all
    /// six index orderings.
    pub fn in_memory_with_hot_threshold(hot_threshold: usize) -> Self {
        Self {
            dictionary: Dictionary::new(),
            tier: Box::new(MemoryTier::with_hot_threshold(hot_threshold)),
        }
    }

    pub fn dictionary(&self) -> &Dictionary {
        &self.dictionary
    }

    pub fn tier(&self) -> &dyn Tier {
        self.tier.as_ref()
    }

    pub fn triple_count(&self) -> u64 {
        self.tier.triple_count()
    }

    pub fn stats(&self) -> TierStats {
        self.tier.stats()
    }

    /// Begin a read transaction: pin a stable, internally-consistent snapshot of
    /// the store (SPEC-02 copy-on-write snapshots — the Stage-1 substitute for
    /// per-tuple MVCC). Concurrent writers append to a new snapshot and never
    /// disturb the pinned view; it stays readable until dropped. The dictionary
    /// is append-only, so term ids in the pinned view never change meaning even
    /// as new terms are interned by other transactions.
    pub fn snapshot(&self) -> StoreSnapshot<'_> {
        let mt = self
            .tier
            .as_any()
            .downcast_ref::<MemoryTier>()
            .expect("Stage-1 store always wraps MemoryTier");
        StoreSnapshot {
            tier: mt.snapshot(),
            dictionary: &self.dictionary,
        }
    }

    /// Insert into the default graph.
    pub fn insert_triples(&self, triples: &[(Term, Term, Term)]) -> Result<()> {
        let mut quads = Vec::with_capacity(triples.len());
        for (s, p, o) in triples {
            let (s_id, p_id, o_id) = self.dictionary.intern_triple(s, p, o)?;
            quads.push((DEFAULT_GRAPH, s_id, p_id, o_id));
        }
        self.tier.insert_quad_batch(&quads)
    }

    /// Insert (graph, s, p, o) quads. Caller-supplied `GraphId`s must already
    /// have been interned via `intern_graph_uri`.
    pub fn insert_quads(&self, quads: &[(GraphId, Term, Term, Term)]) -> Result<()> {
        let mut encoded = Vec::with_capacity(quads.len());
        for (g, s, p, o) in quads {
            let (s_id, p_id, o_id) = self.dictionary.intern_triple(s, p, o)?;
            encoded.push((*g, s_id, p_id, o_id));
        }
        self.tier.insert_quad_batch(&encoded)
    }

    /// Retract triples from the default graph (SPEC-25 S1). Returns the number
    /// of tuples actually retracted. Terms are looked up, not interned: a
    /// triple mentioning a term that was never inserted retracts nothing (the
    /// dictionary is append-only and a read/delete transaction must not mutate
    /// it).
    pub fn retract_triples(&self, triples: &[(Term, Term, Term)]) -> Result<usize> {
        let mut quads = Vec::with_capacity(triples.len());
        for (s, p, o) in triples {
            let (Some(s_id), Some(p_id), Some(o_id)) = (
                self.dictionary.get(s),
                self.dictionary.get(p),
                self.dictionary.get(o),
            ) else {
                continue; // an un-interned term was never stored, so nothing to retract
            };
            quads.push((DEFAULT_GRAPH, s_id, p_id, o_id));
        }
        self.tier.retract_quad_batch(&quads)
    }

    /// Retract (graph, s, p, o) quads (SPEC-25 S1). `GraphId`s must already
    /// have been interned via `intern_graph_uri`. See [`Store::retract_triples`]
    /// for the term-lookup (not intern) semantics.
    pub fn retract_quads(&self, quads: &[(GraphId, Term, Term, Term)]) -> Result<usize> {
        let mut encoded = Vec::with_capacity(quads.len());
        for (g, s, p, o) in quads {
            let (Some(s_id), Some(p_id), Some(o_id)) = (
                self.dictionary.get(s),
                self.dictionary.get(p),
                self.dictionary.get(o),
            ) else {
                continue;
            };
            encoded.push((*g, s_id, p_id, o_id));
        }
        self.tier.retract_quad_batch(&encoded)
    }

    /// Reclaim physically-dead rows (`end <= min pinned version`) across the
    /// tier (SPEC-25 S1). A thin passthrough to `MemoryTier::compact` — without
    /// this, compaction is only reachable from tests that construct a
    /// `MemoryTier` directly.
    pub fn compact(&self) {
        let mt = self
            .tier
            .as_any()
            .downcast_ref::<MemoryTier>()
            .expect("Stage-1 store always wraps MemoryTier");
        mt.compact();
    }

    pub fn intern_graph_uri(&self, graph_uri: &Term) -> Result<GraphId> {
        let id = self.dictionary.intern(graph_uri)?;
        Ok(GraphId(id.0))
    }

    /// Scan a single predicate in the default graph, returning materialized
    /// (subject, object) `Term` pairs. Used by tests; production code should
    /// use the tier's columnar scan directly.
    pub fn scan_predicate_default_graph(&self, predicate: &Term) -> Result<Vec<(Term, Term)>> {
        self.snapshot().scan_predicate_default_graph(predicate)
    }

    /// Scan a single predicate in the default graph in the requested index
    /// ordering (SPEC-02 F4), returning materialized `(subject, predicate,
    /// object)` `Term` triples. Rows come back in the global order implied by
    /// `ord` (the predicate is constant within a partition, so the ordering is
    /// determined by the subject/object axis). For object-major orderings on a
    /// cold predicate the layout is materialised lazily on first call.
    pub fn scan_predicate_ordered(
        &self,
        predicate: &Term,
        ord: Ordering,
    ) -> Result<Vec<(Term, Term, Term)>> {
        self.snapshot().scan_predicate_ordered(predicate, ord)
    }

    /// The top-`n` predicates in the default graph by triple count (descending),
    /// as `(predicate Term, triple_count)`. Used to demonstrate SPEC-02
    /// acceptance #6 (top predicates queryable in all six orderings).
    pub fn top_predicates(&self, n: usize) -> Result<Vec<(Term, u64)>> {
        self.snapshot().top_predicates(n)
    }

    pub fn report_footprint(&self) -> FootprintReport {
        let stats = self.tier.stats();
        let bpt = if stats.triples == 0 {
            0.0
        } else {
            stats.bytes_estimated as f64 / stats.triples as f64
        };
        FootprintReport {
            triples: stats.triples,
            bytes_estimated: stats.bytes_estimated,
            bytes_per_triple: bpt,
        }
    }

    /// Dump every default-graph triple as raw `TermId`s, in arbitrary order,
    /// from a single pinned snapshot (internally consistent even under
    /// concurrent writes). O(triples) and materialized — intended for snapshot
    /// builders, not hot paths.
    pub fn scan_all_term_ids(&self) -> Vec<(TermId, TermId, TermId)> {
        self.snapshot().scan_all_term_ids()
    }

    /// True if any non-default graph holds at least one triple. The snapshot
    /// format currently covers the default graph only; export refuses to run
    /// (rather than silently dropping data) when this is true.
    pub fn has_named_graph_data(&self) -> bool {
        // A predicate key only exists in a graph once at least one (s, o) pair
        // has been appended for it (see `MemoryTier::insert_quad_batch`), so a
        // non-default graph with any predicate partition holds ≥1 triple.
        // NB: `Tier::predicate` is a Stage-1 stub that always returns `None`
        // (real partition access is via `MemoryTier::with_predicate`), so this
        // guard relies on `predicates(g)` rather than scanning a partition.
        // Routed through a pinned snapshot so the public method and the
        // snapshot-pinned exporter check share one implementation.
        self.snapshot().has_named_graph_data()
    }

    /// Export the default graph to a writer in the HDT-derived snapshot format
    /// (SPEC-02 F9). See `crate::snapshot`.
    pub fn export_snapshot<W: std::io::Write>(
        &self,
        w: &mut W,
    ) -> Result<crate::snapshot::SnapshotStats> {
        crate::snapshot::export_snapshot(self, w)
    }

    /// Import a snapshot into this store (default graph).
    pub fn import_snapshot<R: std::io::Read>(&self, r: &mut R) -> Result<()> {
        crate::snapshot::import_snapshot_into(self, r)
    }
}

/// A pinned, internally-consistent read view of a [`Store`] (SPEC-02
/// copy-on-write snapshot). Holds an `Arc` to the immutable tier state captured
/// at [`Store::snapshot`] time plus a borrow of the append-only dictionary for
/// term materialization. Cheap to create; cheap to drop.
pub struct StoreSnapshot<'a> {
    tier: crate::memory_tier::PinnedSnapshot,
    dictionary: &'a Dictionary,
}

impl StoreSnapshot<'_> {
    /// The snapshot id (monotonic tier version) this view is pinned to.
    pub fn version(&self) -> u64 {
        self.tier.version()
    }

    pub fn triple_count(&self) -> u64 {
        self.tier.triple_count()
    }

    pub fn stats(&self) -> TierStats {
        self.tier.stats()
    }

    /// Scan a single predicate in the default graph, returning materialized
    /// (subject, object) `Term` pairs. A read transaction never mutates the
    /// dictionary: an absent predicate (never interned) yields no rows.
    pub fn scan_predicate_default_graph(&self, predicate: &Term) -> Result<Vec<(Term, Term)>> {
        let p_id = match self.dictionary.get(predicate) {
            Some(id) => id,
            None => return Ok(Vec::new()),
        };
        let pairs = self
            .tier
            .with_predicate(DEFAULT_GRAPH, p_id, |part| {
                part.scan_at(self.tier.version()).collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut out = Vec::with_capacity(pairs.len());
        for (s_id, o_id) in pairs {
            out.push((self.term(s_id)?, self.term(o_id)?));
        }
        Ok(out)
    }

    /// Scan a single predicate in the default graph in the requested index
    /// ordering (SPEC-02 F4), returning materialized `(s, p, o)` triples.
    pub fn scan_predicate_ordered(
        &self,
        predicate: &Term,
        ord: Ordering,
    ) -> Result<Vec<(Term, Term, Term)>> {
        let p_id = match self.dictionary.get(predicate) {
            Some(id) => id,
            None => return Ok(Vec::new()),
        };
        let cols = match self.tier.ordered_predicate_at(DEFAULT_GRAPH, p_id, ord) {
            Some(cols) => cols,
            None => return Ok(Vec::new()),
        };
        let mut out = Vec::with_capacity(cols.len());
        for (s_id, o_id) in cols.subject_object() {
            out.push((self.term(s_id)?, predicate.clone(), self.term(o_id)?));
        }
        Ok(out)
    }

    /// The top-`n` predicates in the default graph by triple count (descending).
    pub fn top_predicates(&self, n: usize) -> Result<Vec<(Term, u64)>> {
        let top = self.tier.top_predicates(DEFAULT_GRAPH, n);
        let mut out = Vec::with_capacity(top.len());
        for (p_id, count) in top {
            out.push((self.term(p_id)?, count));
        }
        Ok(out)
    }

    /// Dump every default-graph triple as raw `TermId`s, in arbitrary order,
    /// from this single pinned snapshot (so the dump is internally consistent
    /// even under concurrent writes — the NF5 checkpoint-consistency property).
    pub fn scan_all_term_ids(&self) -> Vec<(TermId, TermId, TermId)> {
        let version = self.tier.version();
        let mut out = Vec::with_capacity(self.tier.triple_count() as usize);
        for p_id in self.tier.predicates(DEFAULT_GRAPH) {
            self.tier.with_predicate(DEFAULT_GRAPH, p_id, |part| {
                out.extend(part.scan_at(version).map(|(s, o)| (s, p_id, o)));
            });
        }
        out
    }

    /// True if any non-default graph in this pinned snapshot holds at least one
    /// triple. Mirrors [`Store::has_named_graph_data`] but against the pinned
    /// tier state, so an exporter can check this and scan the default graph from
    /// the *same* snapshot (no TOCTOU between the check and the scan).
    pub fn has_named_graph_data(&self) -> bool {
        let version = self.tier.version();
        self.tier.graphs().into_iter().any(|g| {
            g != DEFAULT_GRAPH
                && self.tier.predicates(g).into_iter().any(|p| {
                    self.tier
                        .with_predicate(g, p, |part| part.len_at(version) > 0)
                        .unwrap_or(false)
                })
        })
    }

    /// SPEC-24 S6 as-of token: the commit version this view is pinned to (==
    /// the engine's logical clock, ADR-0018).
    pub fn logical_time(&self) -> u64 {
        self.tier.version()
    }

    /// Number of triples visible in this pinned view (default graph only —
    /// mirrors [`Self::triple_count`]).
    pub fn len(&self) -> usize {
        self.tier.triple_count() as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// True if `(s, p, o)` is visible in the default graph at this pinned
    /// version (SPEC-24 S6 point read). O(partition size) for S1: a linear
    /// scan of the predicate partition's rows. Fine for the point reads S6
    /// targets against modest per-predicate partitions; a sorted-column binary
    /// search is a later optimization (tracked with the WCOJ columnar source).
    pub fn contains(&self, s: TermId, p: TermId, o: TermId) -> bool {
        let version = self.tier.version();
        self.tier
            .with_predicate(DEFAULT_GRAPH, p, |part| {
                part.scan_at(version).any(|(rs, ro)| rs == s && ro == o)
            })
            .unwrap_or(false)
    }

    /// Key-ordered iteration over every visible default-graph triple as raw
    /// `TermId`s: predicates in ascending id order, subject-major within each
    /// predicate. Stable across concurrent writes (reads the pinned view).
    pub fn iter_all_term_ids(&self) -> impl Iterator<Item = (TermId, TermId, TermId)> + '_ {
        let version = self.tier.version();
        let mut preds = self.tier.predicates(DEFAULT_GRAPH);
        preds.sort_by_key(|t| t.0);
        preds.into_iter().flat_map(move |p_id| {
            self.tier
                .with_predicate(DEFAULT_GRAPH, p_id, |part| {
                    part.scan_at(version)
                        .map(move |(s, o)| (s, p_id, o))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
    }

    /// The append-only dictionary backing this snapshot, for term materialization.
    pub fn dictionary(&self) -> &Dictionary {
        self.dictionary
    }

    fn term(&self, id: TermId) -> Result<Term> {
        self.dictionary
            .lookup(id)
            .ok_or_else(|| crate::StorageError::InvalidTerm(format!("unknown id {id:?}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxrdf::NamedNode;

    fn iri(s: &str) -> Term {
        Term::NamedNode(NamedNode::new(s).unwrap())
    }

    #[test]
    fn scan_all_term_ids_returns_every_default_graph_triple() {
        let store = Store::in_memory();
        store
            .insert_triples(&[
                (iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b")),
                (iri("http://ex/a"), iri("http://ex/q"), iri("http://ex/c")),
            ])
            .unwrap();
        let all = store.scan_all_term_ids();
        assert_eq!(all.len(), 2);
        let p = store.dictionary().get(&iri("http://ex/p")).unwrap();
        let q = store.dictionary().get(&iri("http://ex/q")).unwrap();
        let preds: Vec<TermId> = all.iter().map(|t| t.1).collect();
        assert!(preds.contains(&p) && preds.contains(&q));
    }

    #[test]
    fn scanning_absent_predicate_does_not_mutate_dictionary() {
        let store = Store::in_memory();
        store
            .insert_triples(&[(iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"))])
            .unwrap();
        let absent = iri("http://ex/never-interned");

        // A read of an absent predicate yields no rows and must NOT intern the
        // query term (a read transaction is non-mutating).
        let snap = store.snapshot();
        assert!(snap
            .scan_predicate_default_graph(&absent)
            .unwrap()
            .is_empty());
        assert!(snap
            .scan_predicate_ordered(&absent, Ordering::Spo)
            .unwrap()
            .is_empty());
        assert!(store
            .scan_predicate_default_graph(&absent)
            .unwrap()
            .is_empty());

        // The absent term was never added to the dictionary by those reads.
        assert!(store.dictionary().get(&absent).is_none());
    }

    #[test]
    fn store_snapshot_is_stable_across_writes() {
        let store = Store::in_memory();
        store
            .insert_triples(&[(iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"))])
            .unwrap();
        let snap = store.snapshot();
        assert_eq!(snap.version(), 1);
        assert_eq!(snap.triple_count(), 1);

        // Mutate the live store; the pinned snapshot is unaffected.
        store
            .insert_triples(&[(iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/c"))])
            .unwrap();
        assert_eq!(snap.triple_count(), 1);
        assert_eq!(
            snap.scan_predicate_default_graph(&iri("http://ex/p"))
                .unwrap()
                .len(),
            1
        );

        // The live store sees both triples.
        assert_eq!(store.triple_count(), 2);
        assert_eq!(
            store
                .scan_predicate_default_graph(&iri("http://ex/p"))
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn store_retract_is_visible_to_new_reads_only() {
        let store = Store::in_memory();
        let t = (iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"));
        store.insert_triples(std::slice::from_ref(&t)).unwrap();
        let before = store.snapshot();
        let n = store.retract_triples(std::slice::from_ref(&t)).unwrap();
        assert_eq!(n, 1);

        assert_eq!(before.triple_count(), 1, "pinned-before read still sees it");
        assert_eq!(store.snapshot().triple_count(), 0, "new read does not");
    }

    #[test]
    fn retract_of_uninterned_term_is_a_noop() {
        let store = Store::in_memory();
        let t = (iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"));
        store.insert_triples(std::slice::from_ref(&t)).unwrap();
        // A triple mentioning a term that was never inserted retracts nothing.
        let never = iri("http://ex/never-interned");
        let n = store
            .retract_triples(&[(never.clone(), iri("http://ex/p"), iri("http://ex/b"))])
            .unwrap();
        assert_eq!(n, 0);
        assert_eq!(store.triple_count(), 1);
        assert!(store.dictionary().get(&never).is_none());
    }

    #[test]
    fn snapshot_s6_surface() {
        let store = Store::in_memory();
        let t = (iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"));
        store.insert_triples(std::slice::from_ref(&t)).unwrap();
        let snap = store.snapshot();

        let (s, p, o) = {
            let d = store.dictionary();
            (
                d.get(&t.0).unwrap(),
                d.get(&t.1).unwrap(),
                d.get(&t.2).unwrap(),
            )
        };
        assert!(snap.contains(s, p, o), "contains a present triple");
        assert!(
            !snap.contains(s, p, TermId(o.0 + 1)),
            "does not contain an absent one"
        );
        assert_eq!(snap.len(), 1);
        assert!(!snap.is_empty());
        assert_eq!(snap.logical_time(), snap.version());

        // Ordered iteration is key-sorted and stable.
        let ids: Vec<_> = snap.iter_all_term_ids().collect();
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0], (s, p, o));
    }

    #[test]
    fn compact_reclaims_dead_rows_and_leaves_live_count_correct() {
        let store = Store::in_memory();
        let a = (iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"));
        let c = (iri("http://ex/c"), iri("http://ex/p"), iri("http://ex/d"));
        store.insert_triples(&[a.clone(), c.clone()]).unwrap();
        store.retract_triples(std::slice::from_ref(&a)).unwrap();

        // No pinned snapshot below the retraction's version, so the dead row
        // is reclaimable.
        store.compact();

        assert_eq!(store.triple_count(), 1, "live count still correct");
        let snap = store.snapshot();
        assert_eq!(snap.len(), 1);
        assert!(
            !snap.contains(
                store.dictionary().get(&a.0).unwrap(),
                store.dictionary().get(&a.1).unwrap(),
                store.dictionary().get(&a.2).unwrap(),
            ),
            "retracted triple stays absent after compaction"
        );
        // Physical check: the partition backing predicate `p` holds exactly
        // one row after compaction (the dead row was reclaimed, not just
        // hidden by the visibility filter). `tests` is inside `store.rs`, so
        // it can reach `StoreSnapshot.tier` (a `PinnedSnapshot`, Derefs to
        // `TierSnapshot`) directly.
        let p_id = store.dictionary().get(&a.1).unwrap();
        let phys = snap
            .tier
            .with_predicate(DEFAULT_GRAPH, p_id, |part| part.len())
            .unwrap();
        assert_eq!(phys, 1, "dead row physically reclaimed");
    }
}
