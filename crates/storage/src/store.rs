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

    /// Scan a single predicate in `g`, returning materialized (subject,
    /// object) `Term` pairs. Used by tests; production code should use the
    /// tier's columnar scan directly.
    pub fn scan_predicate(&self, g: GraphId, predicate: &Term) -> Result<Vec<(Term, Term)>> {
        self.snapshot().scan_predicate(g, predicate)
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
    /// `Tier::graphs()` is visibility-filtered (D11), so this is just "any
    /// enumerated graph other than the default one". Routed through a pinned
    /// snapshot so the public method and the snapshot-pinned exporter check
    /// share one implementation.
    pub fn has_named_graph_data(&self) -> bool {
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

    /// Scan a single predicate in `g`, returning materialized (subject,
    /// object) `Term` pairs. A read transaction never mutates the dictionary:
    /// an absent predicate (never interned) yields no rows.
    pub fn scan_predicate(&self, g: GraphId, predicate: &Term) -> Result<Vec<(Term, Term)>> {
        let p_id = match self.dictionary.get(predicate) {
            Some(id) => id,
            None => return Ok(Vec::new()),
        };
        let pairs = self
            .tier
            .with_predicate(g, p_id, |part| {
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
    /// `tier.graphs()` is already visibility-filtered (D11), so this is just
    /// "any enumerated graph other than the default one".
    pub fn has_named_graph_data(&self) -> bool {
        self.tier.graphs().into_iter().any(|g| g != DEFAULT_GRAPH)
    }

    /// SPEC-24 S6 as-of token: the commit version this view is pinned to (==
    /// the engine's logical clock, ADR-0018).
    pub fn logical_time(&self) -> u64 {
        self.tier.version()
    }

    /// Number of triples visible in this pinned view, across ALL graphs
    /// (SPEC-28 S2). Equivalent to `triple_count() as usize`. The old
    /// default-graph-scoped contract moved to [`Self::graph_len`] — see the
    /// `snapshot_len_is_whole_store` test for why the inversion is safe.
    pub fn len(&self) -> usize {
        self.triple_count() as usize
    }

    /// True if this pinned view has no visible triples in any graph. See
    /// [`Self::len`] for scope.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Number of triples visible in graph `g` at this pinned view. O(number
    /// of predicates in `g`); unconditional — an absent or never-interned
    /// graph yields 0, not an error.
    pub fn graph_len(&self, g: GraphId) -> usize {
        self.tier
            .predicates(g)
            .into_iter()
            .filter_map(|p| self.tier.with_predicate(g, p, |part| part.live_len()))
            .sum()
    }

    /// The graphs holding at least one visible quad in this pinned view (D11
    /// — see [`crate::tier::Tier::graphs`] for the full contract), including
    /// `DEFAULT_GRAPH` when it holds data. Callers wanting named graphs only
    /// should filter out `DEFAULT_GRAPH`.
    pub fn graphs(&self) -> Vec<GraphId> {
        self.tier.graphs()
    }

    /// Decode `g` back to the IRI [`Term`] it was interned from. Errors on
    /// `DEFAULT_GRAPH`: the sentinel has no dictionary entry — it is not a
    /// real graph name.
    pub fn graph_uri(&self, g: GraphId) -> Result<Term> {
        if g == DEFAULT_GRAPH {
            return Err(crate::StorageError::InvalidTerm(
                "DEFAULT_GRAPH has no URI".into(),
            ));
        }
        self.term(TermId(g.0))
    }

    /// Every visible triple in graph `g`, decoded, from this single pinned
    /// snapshot. O(quads in `g` + predicates in `g`) — never O(store). This is
    /// the GSP (Graph Store Protocol) `GET` path and stage 3 of the
    /// whole-graph `PUT` diff (SPEC-28), so it must stay graph-scoped rather
    /// than filtering a whole-store scan. Predicates are visited in ascending
    /// `TermId` order for a deterministic result, mirroring
    /// [`Self::scan_all_term_ids`]'s pattern with `g` in place of
    /// `DEFAULT_GRAPH`.
    pub fn scan_graph(&self, g: GraphId) -> Result<Vec<(Term, Term, Term)>> {
        let version = self.tier.version();
        let mut preds = self.tier.predicates(g);
        preds.sort_by_key(|t| t.0);
        let mut out = Vec::with_capacity(self.graph_len(g));
        for p_id in preds {
            let rows = self
                .tier
                .with_predicate(g, p_id, |part| part.scan_at(version).collect::<Vec<_>>())
                .unwrap_or_default();
            for (s_id, o_id) in rows {
                out.push((self.term(s_id)?, self.term(p_id)?, self.term(o_id)?));
            }
        }
        Ok(out)
    }

    /// Key-ordered iteration over every visible triple in graph `g`, as raw
    /// `TermId`s: predicates in ascending id order, subject-major within each
    /// predicate. The id-level twin of [`Self::scan_graph`] — this is what the
    /// future SPEC-24 S6 backing and the phase-5 GSP diff consume. Mirrors
    /// [`Self::iter_all_term_ids`]'s ordering contract, scoped to `g`.
    pub fn iter_graph_term_ids(
        &self,
        g: GraphId,
    ) -> impl Iterator<Item = (TermId, TermId, TermId)> + '_ {
        let version = self.tier.version();
        let mut preds = self.tier.predicates(g);
        preds.sort_by_key(|t| t.0);
        preds.into_iter().flat_map(move |p_id| {
            self.tier
                .with_predicate(g, p_id, |part| {
                    part.scan_at(version)
                        .map(move |(s, o)| (s, p_id, o))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
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
            .scan_predicate(DEFAULT_GRAPH, &absent)
            .unwrap()
            .is_empty());
        assert!(snap
            .scan_predicate_ordered(&absent, Ordering::Spo)
            .unwrap()
            .is_empty());
        assert!(store
            .scan_predicate(DEFAULT_GRAPH, &absent)
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
            snap.scan_predicate(DEFAULT_GRAPH, &iri("http://ex/p"))
                .unwrap()
                .len(),
            1
        );

        // The live store sees both triples.
        assert_eq!(store.triple_count(), 2);
        assert_eq!(
            store
                .scan_predicate(DEFAULT_GRAPH, &iri("http://ex/p"))
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

    /// `StoreSnapshot::len()` is whole-store (SPEC-28 S2, #265): the old
    /// default-graph-scoped contract relocated to `graph_len`. The old test
    /// pinned `len()` to the default graph on the belief that the SPEC-24 S6
    /// surface backs the single-graph incremental circuit — verified false:
    /// `crates/incremental` has no dependency on `horndb-storage` (its
    /// `Snapshot` type only mirrors this shape, in anticipation of the S6
    /// swap tracked by #213, not yet landed). So inverting `len()` breaks no
    /// circuit; `graph_len` is the graph-scoped surface #213 will wire to
    /// (see PLAN-28-02 design).
    #[test]
    fn snapshot_len_is_whole_store() {
        let store = Store::in_memory();
        store
            .insert_triples(std::slice::from_ref(&(
                iri("http://ex/a"),
                iri("http://ex/p"),
                iri("http://ex/b"),
            )))
            .unwrap();
        let g1 = store.intern_graph_uri(&iri("http://ex/graph1")).unwrap();
        store
            .insert_quads(&[(
                g1,
                iri("http://ex/x"),
                iri("http://ex/q"),
                iri("http://ex/y"),
            )])
            .unwrap();
        let absent = store.intern_graph_uri(&iri("http://ex/absent")).unwrap();

        let snap = store.snapshot();
        assert_eq!(
            snap.len(),
            2,
            "len() is whole-store, not default-graph scoped"
        );
        assert_eq!(snap.graph_len(DEFAULT_GRAPH), 1);
        assert_eq!(snap.graph_len(g1), 1);
        assert_eq!(snap.graph_len(absent), 0, "absent graph has no rows");
    }

    /// `graphs()` is visibility-filtered (D11: a graph exists iff it holds at
    /// least one visible quad — a fully-retracted graph ceases to exist).
    #[test]
    fn graphs_is_visibility_filtered() {
        let store = Store::in_memory();
        let g1 = store.intern_graph_uri(&iri("http://ex/graph1")).unwrap();
        let g2 = store.intern_graph_uri(&iri("http://ex/graph2")).unwrap();
        let q1 = (
            g1,
            iri("http://ex/a"),
            iri("http://ex/p"),
            iri("http://ex/b"),
        );
        let q2 = (
            g2,
            iri("http://ex/c"),
            iri("http://ex/p"),
            iri("http://ex/d"),
        );
        store.insert_quads(&[q1, q2.clone()]).unwrap();
        let n = store.retract_quads(std::slice::from_ref(&q2)).unwrap();
        assert_eq!(n, 1, "g2's only quad must be retracted");

        let snap = store.snapshot();
        let graphs = snap.graphs();
        assert!(graphs.contains(&g1), "g1 still holds a visible quad");
        assert!(
            !graphs.contains(&g2),
            "g2 is fully retracted and must not be enumerated"
        );
        assert_eq!(
            graphs.contains(&DEFAULT_GRAPH),
            snap.graph_len(DEFAULT_GRAPH) > 0,
            "DEFAULT_GRAPH appears in graphs() iff it holds data"
        );

        // The tier-level view (used directly by production callers such as
        // `has_named_graph_data`) must agree with the snapshot view.
        let tier_graphs = store.tier().graphs();
        assert!(tier_graphs.contains(&g1));
        assert!(!tier_graphs.contains(&g2));
    }

    /// `graph_uri` decodes a `GraphId` back to the IRI it was interned from;
    /// the `DEFAULT_GRAPH` sentinel has no dictionary entry and errors.
    #[test]
    fn graph_uri_roundtrip() {
        let store = Store::in_memory();
        let t = iri("http://ex/graph1");
        let g = store.intern_graph_uri(&t).unwrap();

        let snap = store.snapshot();
        assert_eq!(snap.graph_uri(g).unwrap(), t);
        assert!(
            snap.graph_uri(DEFAULT_GRAPH).is_err(),
            "the default-graph sentinel has no URI"
        );
    }

    #[test]
    fn retract_quads_removes_only_the_targeted_named_graph_quad() {
        let store = Store::in_memory();
        let g = store.intern_graph_uri(&iri("http://ex/graph1")).unwrap();
        let q1 = (
            g,
            iri("http://ex/a"),
            iri("http://ex/p"),
            iri("http://ex/b"),
        );
        let q2 = (
            g,
            iri("http://ex/c"),
            iri("http://ex/p"),
            iri("http://ex/d"),
        );
        store.insert_quads(&[q1.clone(), q2.clone()]).unwrap();

        let before = store.snapshot();
        let n = store.retract_quads(std::slice::from_ref(&q1)).unwrap();
        assert_eq!(n, 1);

        let p_id = store.dictionary().get(&q1.2).unwrap();
        let a_id = store.dictionary().get(&q1.1).unwrap();
        let b_id = store.dictionary().get(&q1.3).unwrap();
        let c_id = store.dictionary().get(&q2.1).unwrap();
        let d_id = store.dictionary().get(&q2.3).unwrap();

        // Pinned-before snapshot still sees both quads in the named graph.
        let before_rows = before
            .tier
            .with_predicate(g, p_id, |part| {
                part.scan_at(before.version()).collect::<Vec<_>>()
            })
            .unwrap();
        assert!(before_rows.contains(&(a_id, b_id)));
        assert!(before_rows.contains(&(c_id, d_id)));

        // A fresh snapshot sees the retraction: q1 gone, q2 survives.
        let after = store.snapshot();
        let after_rows = after
            .tier
            .with_predicate(g, p_id, |part| {
                part.scan_at(after.version()).collect::<Vec<_>>()
            })
            .unwrap();
        assert!(
            !after_rows.contains(&(a_id, b_id)),
            "retracted quad must be gone"
        );
        assert!(after_rows.contains(&(c_id, d_id)), "surviving quad remains");
    }

    /// `scan_graph` returns exactly one graph's visible triples, decoded. A
    /// triple asserted in two graphs (same `(s, p, o)`, different `GraphId`)
    /// must appear in both graphs' scans — graph membership, not triple
    /// identity, is what's scoped.
    #[test]
    fn scan_graph_returns_exactly_the_graphs_quads() {
        let store = Store::in_memory();
        let g1 = store.intern_graph_uri(&iri("http://ex/g1")).unwrap();
        let g2 = store.intern_graph_uri(&iri("http://ex/g2")).unwrap();
        let shared = (iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"));
        let g1_only = (iri("http://ex/c"), iri("http://ex/p"), iri("http://ex/d"));
        let g2_only = (iri("http://ex/e"), iri("http://ex/q"), iri("http://ex/f"));
        let default_only = (iri("http://ex/x"), iri("http://ex/p"), iri("http://ex/y"));

        store
            .insert_quads(&[
                (g1, shared.0.clone(), shared.1.clone(), shared.2.clone()),
                (g2, shared.0.clone(), shared.1.clone(), shared.2.clone()),
                (g1, g1_only.0.clone(), g1_only.1.clone(), g1_only.2.clone()),
                (g2, g2_only.0.clone(), g2_only.1.clone(), g2_only.2.clone()),
            ])
            .unwrap();
        store
            .insert_triples(std::slice::from_ref(&default_only))
            .unwrap();

        let snap = store.snapshot();

        let g1_rows = snap.scan_graph(g1).unwrap();
        assert_eq!(g1_rows.len(), 2, "g1 holds the shared triple plus its own");
        assert!(g1_rows.contains(&shared));
        assert!(g1_rows.contains(&g1_only));
        assert!(
            !g1_rows.contains(&g2_only),
            "g1's scan must not see g2's triple"
        );
        assert!(
            !g1_rows.contains(&default_only),
            "g1's scan must not see default-graph data"
        );

        let g2_rows = snap.scan_graph(g2).unwrap();
        assert_eq!(g2_rows.len(), 2, "g2 holds the shared triple plus its own");
        assert!(
            g2_rows.contains(&shared),
            "the shared triple appears in both graphs' scans"
        );
        assert!(g2_rows.contains(&g2_only));
        assert!(!g2_rows.contains(&g1_only));
    }

    /// `scan_graph` respects visibility: a snapshot pinned before a retraction
    /// still returns the retracted quad; a fresh snapshot omits it.
    #[test]
    fn scan_graph_respects_visibility() {
        let store = Store::in_memory();
        let g1 = store.intern_graph_uri(&iri("http://ex/g1")).unwrap();
        let keep = (iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"));
        let gone = (iri("http://ex/c"), iri("http://ex/p"), iri("http://ex/d"));
        store
            .insert_quads(&[
                (g1, keep.0.clone(), keep.1.clone(), keep.2.clone()),
                (g1, gone.0.clone(), gone.1.clone(), gone.2.clone()),
            ])
            .unwrap();

        // Pin a snapshot BEFORE the retraction.
        let before = store.snapshot();

        let n = store
            .retract_quads(&[(g1, gone.0.clone(), gone.1.clone(), gone.2.clone())])
            .unwrap();
        assert_eq!(n, 1);

        // The old, pinned-before snapshot still sees the retracted quad.
        let before_rows = before.scan_graph(g1).unwrap();
        assert_eq!(before_rows.len(), 2);
        assert!(before_rows.contains(&keep));
        assert!(before_rows.contains(&gone));

        // A fresh snapshot omits it.
        let after_rows = store.snapshot().scan_graph(g1).unwrap();
        assert_eq!(after_rows.len(), 1);
        assert!(after_rows.contains(&keep));
        assert!(!after_rows.contains(&gone));
    }

    /// `iter_graph_term_ids` mirrors `iter_all_term_ids`'s ordering contract:
    /// predicates in ascending `TermId` order, subject-major (rows sorted by
    /// subject id) within each predicate — scoped to one graph. Interning
    /// order below is chosen so the expected `TermId` order is known: `pb` is
    /// interned before `pa` (so `pb`'s id is lower), and within `pa`, `s3` is
    /// interned before `s2` (so `s3`'s row must come first even though it was
    /// inserted second).
    #[test]
    fn iter_graph_term_ids_is_key_ordered() {
        let store = Store::in_memory();
        let g1 = store.intern_graph_uri(&iri("http://ex/g1")).unwrap();
        let o = iri("http://ex/o");

        // Interning order: s1, pb, o, s3, pa, s2.
        store
            .insert_quads(&[(g1, iri("http://ex/s1"), iri("http://ex/pb"), o.clone())])
            .unwrap();
        store
            .insert_quads(&[(g1, iri("http://ex/s3"), iri("http://ex/pa"), o.clone())])
            .unwrap();
        store
            .insert_quads(&[(g1, iri("http://ex/s2"), iri("http://ex/pa"), o.clone())])
            .unwrap();

        let snap = store.snapshot();
        let d = store.dictionary();
        let (s1, pb, o_id, s3, pa, s2) = (
            d.get(&iri("http://ex/s1")).unwrap(),
            d.get(&iri("http://ex/pb")).unwrap(),
            d.get(&o).unwrap(),
            d.get(&iri("http://ex/s3")).unwrap(),
            d.get(&iri("http://ex/pa")).unwrap(),
            d.get(&iri("http://ex/s2")).unwrap(),
        );
        assert!(pb.0 < pa.0, "pb must be interned before pa");
        assert!(s3.0 < s2.0, "s3 must be interned before s2");

        let ids: Vec<_> = snap.iter_graph_term_ids(g1).collect();
        assert_eq!(
            ids,
            vec![(s1, pb, o_id), (s3, pa, o_id), (s2, pa, o_id),],
            "predicates ascending, subject-major within each predicate"
        );
    }

    /// `scan_predicate` takes a graph parameter: `scan_predicate(g1, &p)` sees
    /// only `g1`'s rows, and `scan_predicate(DEFAULT_GRAPH, &p)` reproduces the
    /// old default-graph-only behaviour on the same fixture.
    #[test]
    fn scan_predicate_takes_a_graph() {
        let store = Store::in_memory();
        let g1 = store.intern_graph_uri(&iri("http://ex/g1")).unwrap();
        let p = iri("http://ex/p");

        store
            .insert_triples(&[(iri("http://ex/a"), p.clone(), iri("http://ex/b"))])
            .unwrap();
        store
            .insert_quads(&[(g1, iri("http://ex/c"), p.clone(), iri("http://ex/d"))])
            .unwrap();

        let snap = store.snapshot();

        let g1_rows = snap.scan_predicate(g1, &p).unwrap();
        assert_eq!(g1_rows.len(), 1);
        assert_eq!(g1_rows[0], (iri("http://ex/c"), iri("http://ex/d")));

        let default_rows = snap.scan_predicate(DEFAULT_GRAPH, &p).unwrap();
        assert_eq!(default_rows.len(), 1);
        assert_eq!(default_rows[0], (iri("http://ex/a"), iri("http://ex/b")));
    }
}
