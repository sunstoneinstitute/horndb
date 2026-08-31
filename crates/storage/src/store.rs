//! Public store facade.
//!
//! Composes a `Dictionary` with one `Tier` implementation. Stage 1 only
//! supports an in-memory tier; the constructor signature leaves room for
//! plugging in cold tiers later.

use crate::dictionary::Dictionary;
use crate::error::Result;
use crate::memory_tier::MemoryTier;
use crate::ordering::Ordering;
use crate::term::{GraphId, InternedQuad, TermId, DEFAULT_GRAPH};
use crate::tier::{ApplyReport, Tier, TierStats};
use bytemuck::TransparentWrapper;
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

    /// Insert (graph, s, p, o) quads (SPEC-28 S6: a thin wrapper over
    /// [`Store::apply_quads`] with no deletions). Caller-supplied `GraphId`s
    /// must already have been interned via `intern_graph_uri`. Returns the
    /// number of quads actually inserted — a quad already visible is a
    /// counted no-op, not double-counted.
    pub fn insert_quads(&self, quads: &[(GraphId, Term, Term, Term)]) -> Result<usize> {
        Ok(self.apply_quads(&[], quads)?.inserted)
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

    /// Retract (graph, s, p, o) quads (SPEC-28 S6: a thin wrapper over
    /// [`Store::apply_quads`] with no insertions). `GraphId`s must already
    /// have been interned via `intern_graph_uri`. See [`Store::retract_triples`]
    /// for the term-lookup (not intern) semantics.
    pub fn retract_quads(&self, quads: &[(GraphId, Term, Term, Term)]) -> Result<usize> {
        Ok(self.apply_quads(quads, &[])?.retracted)
    }

    /// Apply a combined batch of deletions and insertions as one commit
    /// version (SPEC-28 S6, the store boundary a future change-feed
    /// materializer builds on). Deletions apply before insertions, so a
    /// delete+insert of the same quad within one batch ends present.
    /// `GraphId`s must already have been interned via `intern_graph_uri`.
    /// Deletion terms are looked up, not interned (mirroring
    /// [`Store::retract_quads`]: a term never seen retracts nothing);
    /// insertion terms are interned. A batch whose net effect is empty does
    /// not bump the store's commit version.
    pub fn apply_quads(
        &self,
        dels: &[(GraphId, Term, Term, Term)],
        adds: &[(GraphId, Term, Term, Term)],
    ) -> Result<ApplyReport> {
        let mut del_ids = Vec::with_capacity(dels.len());
        for (g, s, p, o) in dels {
            let (Some(s_id), Some(p_id), Some(o_id)) = (
                self.dictionary.get(s),
                self.dictionary.get(p),
                self.dictionary.get(o),
            ) else {
                continue;
            };
            del_ids.push(InternedQuad::from_ids(*g, s_id, p_id, o_id));
        }
        let mut add_ids = Vec::with_capacity(adds.len());
        // One clock read on each side of the loop; the loop body touches no
        // metric handle (SPEC-17 §5.4).
        let t_intern = std::time::Instant::now();
        for (g, s, p, o) in adds {
            add_ids.push(self.dictionary.intern_quad(*g, s, p, o)?);
        }
        if !adds.is_empty() {
            horndb_metrics::metrics().storage.record_load_phase(
                horndb_metrics::labels::LoadPhase::Intern,
                t_intern.elapsed(),
                adds.len() as u64,
            );
        }
        self.apply_quad_ids(&del_ids, &add_ids)
    }

    /// [`Store::apply_quads`] for callers that already hold interned ids.
    ///
    /// A bulk loader that has to intern anyway — to deduplicate before the
    /// write, as `HornBackend` does — would otherwise hand the terms back for
    /// a second, identical dictionary lookup (HDB-87). Passing
    /// [`InternedQuad`]s skips that pass and the term buffer behind it. Same
    /// commit semantics as [`Store::apply_quads`]: deletions before
    /// insertions, one commit version, a net-empty batch does not bump it.
    /// Nothing is interned here, so no `intern` load phase is recorded.
    pub fn apply_quad_ids(
        &self,
        dels: &[InternedQuad],
        adds: &[InternedQuad],
    ) -> Result<ApplyReport> {
        self.tier.apply_quad_batch(
            InternedQuad::peel_slice(dels),
            InternedQuad::peel_slice(adds),
        )
    }

    /// Insert already-interned quads (SPEC-28 S6): a thin wrapper over
    /// [`Store::apply_quad_ids`] with no deletions, and the id-based twin of
    /// [`Store::insert_quads`]. Returns the number of quads actually inserted.
    pub fn insert_quad_ids(&self, adds: &[InternedQuad]) -> Result<usize> {
        Ok(self.apply_quad_ids(&[], adds)?.inserted)
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
    /// object) `Term` pairs, subject-major. Used by tests; production code
    /// should use the tier's columnar scan directly.
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

    /// True if any non-default graph holds at least one triple; the snapshot
    /// format currently covers the default graph only, so export refuses to
    /// run rather than silently drop data when this is true.
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
    // --- whole-store ---

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

    /// Visible triples across all graphs. [`Self::len`] is the `usize` alias;
    /// [`Self::graph_len`] is the per-graph form.
    pub fn triple_count(&self) -> u64 {
        self.tier.triple_count()
    }

    pub fn stats(&self) -> TierStats {
        self.tier.stats()
    }

    /// The snapshot id (monotonic tier version) this view is pinned to.
    pub fn version(&self) -> u64 {
        self.tier.version()
    }

    /// SPEC-24 S6 as-of token: the commit version this view is pinned to (==
    /// the engine's logical clock, ADR-0018).
    pub fn logical_time(&self) -> u64 {
        self.tier.version()
    }

    // --- default-graph-scoped ---

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
        self.iter_graph_term_ids(DEFAULT_GRAPH)
    }

    /// Dump every default-graph triple as raw `TermId`s, key-ordered
    /// (predicates ascending, subject-major within each predicate — see
    /// [`Self::iter_all_term_ids`]), from this single pinned snapshot (so the
    /// dump is internally consistent even under concurrent writes — the NF5
    /// checkpoint-consistency property).
    pub fn scan_all_term_ids(&self) -> Vec<(TermId, TermId, TermId)> {
        let mut out = Vec::with_capacity(self.graph_len(DEFAULT_GRAPH));
        out.extend(self.iter_all_term_ids());
        out
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

    // --- graph-scoped (SPEC-28 S2) ---

    /// Number of triples visible in graph `g` at this pinned view. O(number
    /// of predicates in `g`); unconditional — an absent or never-interned
    /// graph yields 0, not an error.
    pub fn graph_len(&self, g: GraphId) -> usize {
        self.tier.graph_len(g)
    }

    /// The graphs holding at least one visible quad in this pinned view (D11
    /// — see [`crate::tier::Tier::graphs`] for the full contract), including
    /// `DEFAULT_GRAPH` when it holds data. Callers wanting named graphs only
    /// should filter out `DEFAULT_GRAPH`. Sorted by `GraphId` for a
    /// deterministic result — public API that Graph Store Protocol responses
    /// enumerate.
    pub fn graphs(&self) -> Vec<GraphId> {
        let mut graphs = self.tier.graphs();
        graphs.sort_by_key(|g| g.0);
        graphs
    }

    /// Decode `g` back to the IRI [`Term`] it was interned from. Errors on
    /// `DEFAULT_GRAPH`: the sentinel has no dictionary entry — it is not a
    /// real graph name.
    pub fn graph_uri(&self, g: GraphId) -> Result<Term> {
        if g == DEFAULT_GRAPH {
            return Err(crate::StorageError::InvalidTerm(
                "the default graph has no IRI (it is a sentinel, not a named graph)".into(),
            ));
        }
        self.term(TermId(g.0))
    }

    /// Every visible triple in graph `g`, decoded, from this single pinned
    /// snapshot. O(quads in `g` + predicates in `g`) — never O(store). This is
    /// the GSP (Graph Store Protocol) `GET` path and stage 3 of the
    /// whole-graph `PUT` diff (SPEC-28), so it must stay graph-scoped rather
    /// than filtering a whole-store scan. Predicates ascending, subject-major
    /// within each predicate.
    pub fn scan_graph(&self, g: GraphId) -> Result<Vec<(Term, Term, Term)>> {
        let version = self.tier.version();
        let mut preds = self.tier.predicates(g);
        preds.sort_by_key(|t| t.0);
        let mut out = Vec::with_capacity(self.graph_len(g));
        for p_id in preds {
            let p = self.term(p_id)?;
            self.tier
                .with_predicate(g, p_id, |part| -> Result<()> {
                    for (s_id, o_id) in part.scan_at(version) {
                        out.push((self.term(s_id)?, p.clone(), self.term(o_id)?));
                    }
                    Ok(())
                })
                .transpose()?;
        }
        Ok(out)
    }

    /// Key-ordered iteration over every visible triple in graph `g`, as raw
    /// `TermId`s: predicates in ascending id order, subject-major within each
    /// predicate. The id-level twin of [`Self::scan_graph`] (SPEC-28).
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

    /// Scan a single predicate in `g`, returning materialized (subject,
    /// object) `Term` pairs, subject-major. A read transaction never mutates
    /// the dictionary: an absent predicate (never interned) yields no rows.
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

    /// True if any non-default graph in this pinned snapshot holds at least
    /// one triple; lets an exporter check this and scan the default graph
    /// from the same snapshot (no TOCTOU between the check and the scan).
    pub fn has_named_graph_data(&self) -> bool {
        self.tier.graphs().into_iter().any(|g| g != DEFAULT_GRAPH)
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
#[path = "store_tests.rs"]
mod tests;
