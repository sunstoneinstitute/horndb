//! Boundary traits between SPEC-05 and its neighbours.
//!
//! - `TripleSink` is implemented by SPEC-02 (storage). It receives bulk
//!   inserts of inferred triples and must NOT re-fire SPEC-04 rules on them
//!   (avoid infinite re-derivation — F5 in SPEC-05).
//!
//! - `ClosureBackend` is consumed by SPEC-04 (rule engine). The engine routes
//!   the closure subset (prp-trp, scm-sco, scm-spo, eq-*) here instead of
//!   firing those rules itself.

use anyhow::Result;
use rustc_hash::FxHashMap;

pub use crate::closure::incremental::DeleteOutcome;
use crate::closure::incremental::IncrementalTransitiveClosure;
use crate::closure::schema::reflexive_transitive_closure;
use crate::closure::transitive::transitive_closure;
use crate::dense_id::DenseIdMap;
use crate::grb::{init_once, BoolMatrix};
use crate::sameas::EquivClasses;
use crate::types::{DenseIdx, DictId, PredicateId, Triple};

/// Implemented by SPEC-02 storage. Receives inferred triples in bulk.
///
/// Implementations MUST:
/// - Tag inserted triples as "GraphBLAS-derived" for provenance (SPEC-05 F5).
/// - Skip the SPEC-04 rule-firing path so we do not re-derive what we just
///   materialised (SPEC-05 F5; SPEC-04 F2 codegen must respect this flag).
pub trait TripleSink: Sync {
    /// Bulk-insert inferred triples. Returns the count actually inserted
    /// (after the sink's own de-duplication against existing data).
    fn bulk_insert_inferred(&self, triples: &mut dyn Iterator<Item = Triple>) -> Result<u64>;
}

/// Consumed by SPEC-04 rule engine. The rule engine compiles `prp-trp`,
/// `scm-sco`, `scm-spo`, and `eq-*` rule bodies into calls against this
/// trait rather than into native Datalog clauses.
pub trait ClosureBackend {
    /// Close a transitive predicate over its asserted edges and write the
    /// inferred edges (including the asserted ones, for the simple Stage-1
    /// path) into `sink` as `Triple { s, p, o }`.
    ///
    /// Returns the number of triples reported written by `sink`.
    fn close_transitive_predicate(
        &mut self,
        p: PredicateId,
        edges: &[(DictId, DictId)],
        sink: &dyn TripleSink,
    ) -> Result<u64>;

    /// Close `rdfs:subClassOf` (reflexive transitive closure) and write the
    /// closure as `Triple { s = subclass, p = subclassof_pid, o = superclass }`.
    fn close_subclass(
        &mut self,
        subclassof_pid: PredicateId,
        edges: &[(DictId, DictId)],
        sink: &dyn TripleSink,
    ) -> Result<u64>;

    /// Close `rdfs:subPropertyOf` (reflexive transitive closure).
    fn close_subproperty(
        &mut self,
        subpropertyof_pid: PredicateId,
        edges: &[(DictId, DictId)],
        sink: &dyn TripleSink,
    ) -> Result<u64>;

    /// Union all asserted `owl:sameAs` pairs into the equivalence-class
    /// structure. Does NOT emit triples — SPARQL/SPEC-04 consult the
    /// structure directly via `equiv_classes()`.
    fn add_sameas(&mut self, pairs: &[(DictId, DictId)]);

    /// Borrow the current equivalence-class state.
    fn equiv_classes(&self) -> &EquivClasses;
}

/// The default `ClosureBackend` we provide. Internally holds a per-predicate
/// `DenseIdMap` and a single `EquivClasses` for sameAs.
pub struct BackendImpl {
    sameas: EquivClasses,
}

impl Default for BackendImpl {
    fn default() -> Self {
        // Cheap & safe to call repeatedly.
        let _ = init_once();
        Self {
            sameas: EquivClasses::new(),
        }
    }
}

impl ClosureBackend for BackendImpl {
    fn close_transitive_predicate(
        &mut self,
        p: PredicateId,
        edges: &[(DictId, DictId)],
        sink: &dyn TripleSink,
    ) -> Result<u64> {
        if edges.is_empty() {
            return Ok(0);
        }
        let (matrix, map) = build_matrix(edges)?;
        let closure = transitive_closure(&matrix)?;
        let dense_edges = closure.extract_edges()?;
        write_closure(p, &dense_edges, &map, sink)
    }

    fn close_subclass(
        &mut self,
        subclassof_pid: PredicateId,
        edges: &[(DictId, DictId)],
        sink: &dyn TripleSink,
    ) -> Result<u64> {
        close_reflexive(subclassof_pid, edges, sink)
    }

    fn close_subproperty(
        &mut self,
        subpropertyof_pid: PredicateId,
        edges: &[(DictId, DictId)],
        sink: &dyn TripleSink,
    ) -> Result<u64> {
        close_reflexive(subpropertyof_pid, edges, sink)
    }

    fn add_sameas(&mut self, pairs: &[(DictId, DictId)]) {
        for &(a, b) in pairs {
            self.sameas.union(a, b);
        }
    }

    fn equiv_classes(&self) -> &EquivClasses {
        &self.sameas
    }
}

fn close_reflexive(
    p: PredicateId,
    edges: &[(DictId, DictId)],
    sink: &dyn TripleSink,
) -> Result<u64> {
    if edges.is_empty() {
        return Ok(0);
    }
    let (matrix, map) = build_matrix(edges)?;
    let closure = reflexive_transitive_closure(&matrix)?;
    let dense_edges = closure.extract_edges()?;
    write_closure(p, &dense_edges, &map, sink)
}

fn build_matrix(edges: &[(DictId, DictId)]) -> Result<(BoolMatrix, DenseIdMap)> {
    let mut map = DenseIdMap::with_capacity(edges.len() * 2);
    let dense = map.intern_edges(edges);
    let n = map.len() as u64;
    let m = BoolMatrix::from_edges(n, &dense)?;
    Ok((m, map))
}

fn write_closure(
    p: PredicateId,
    dense_edges: &[(u64, u64)],
    map: &DenseIdMap,
    sink: &dyn TripleSink,
) -> Result<u64> {
    let mut iter = dense_edges.iter().filter_map(|&(s, o)| {
        Some(Triple {
            s: map.to_dict(DenseIdx(s))?,
            p,
            o: map.to_dict(DenseIdx(o))?,
        })
    });
    sink.bulk_insert_inferred(&mut iter)
}

/// Map dense `(u64, u64)` index pairs back to `DictId` pairs, dropping any
/// endpoint absent from `map` (which never happens for interned edges).
fn pairs_to_dict(
    map: &DenseIdMap,
    pairs: impl IntoIterator<Item = (u64, u64)>,
) -> Vec<(DictId, DictId)> {
    pairs
        .into_iter()
        .filter_map(|(s, o)| Some((map.to_dict(DenseIdx(s))?, map.to_dict(DenseIdx(o))?)))
        .collect()
}

/// Convenience constructor for callers (SPEC-04 will use this until it has
/// its own factory).
pub fn default_backend() -> BackendImpl {
    BackendImpl::default()
}

/// Retraction outcome in `DictId` space (the [`DeleteOutcome`] mapped back from
/// dense ids). `withdrawn` are closure pairs that lost all support; `survived`
/// are deleted base edges still entailed via another path (PROMOTE candidates).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DictDeleteOutcome {
    pub withdrawn: Vec<(DictId, DictId)>,
    pub survived: Vec<(DictId, DictId)>,
}

/// Per-predicate retained closure state for the incremental path (SPEC-05 F6).
#[derive(Default)]
struct PredicateState {
    map: DenseIdMap,
    closure: IncrementalTransitiveClosure,
}

/// Incremental closure backend. Unlike [`BackendImpl`], which recomputes the
/// whole closure from the full edge set on every call, this retains
/// per-predicate closure state and folds in only the newly inserted edges,
/// writing **only the delta** triples to the sink (SPEC-05 F6).
///
/// Deletion is supported at the closure level via
/// [`Self::delete_transitive_edges`], which withdraws exactly the inferred
/// pairs that lose support when a base edge is retracted. That method *returns*
/// the withdrawn edges rather than writing to a sink: the `+/-` sign lives in
/// the SPEC-06 Z-set layer, and [`TripleSink`] stays a pure insertion boundary.
pub struct IncrementalClosureBackend {
    predicates: FxHashMap<PredicateId, PredicateState>,
    sameas: EquivClasses,
}

impl Default for IncrementalClosureBackend {
    fn default() -> Self {
        // Initialise GraphBLAS here too (mirrors `BackendImpl::default`) so a
        // `default()`-constructed backend is never left uninitialised, even
        // though today's incremental path is FFI-free. Cheap & idempotent.
        let _ = init_once();
        Self {
            predicates: FxHashMap::default(),
            sameas: EquivClasses::new(),
        }
    }
}

impl IncrementalClosureBackend {
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed predicate `p`'s retained closure state from an **already transitively
    /// closed** edge set (e.g. the output of a prior bulk `close_transitive_predicate`
    /// or the closure already materialized in storage). The caller guarantees the
    /// edges are closed; this does not re-close them. Call once before feeding
    /// incremental inserts for a predicate that already has a materialized closure.
    /// Replaces any existing state for `p`. Writes nothing to a sink.
    ///
    /// **Retraction semantics for a closed-seeded predicate.** The seed is an
    /// already-*closed* edge set whose asserted base is unknown, so the
    /// underlying [`IncrementalTransitiveClosure`] seeds its `base` from the
    /// closed extent itself (a conservative stand-in). Retraction then works
    /// uniformly: edges inserted *after* seeding via
    /// [`Self::insert_transitive_edges`] retract **exactly**; retracting a
    /// *seeded* edge is **sound but may under-withdraw** — the seeded base's
    /// redundant transitive edges can keep a pair reachable even when the true
    /// (unknown) asserted base would have dropped it. Predicates whose true
    /// asserted base is known and need exact seeded-edge retraction must build
    /// their state purely through [`Self::insert_transitive_edges`] (which
    /// records the genuine base edges); the SPEC-06 `Circuit` path does that.
    pub fn seed_transitive_closure(&mut self, p: PredicateId, closed_edges: &[(DictId, DictId)]) {
        let mut map = DenseIdMap::with_capacity(closed_edges.len() * 2);
        let dense = map.intern_edges(closed_edges);
        let closure = IncrementalTransitiveClosure::from_closed_edges(dense);
        self.predicates.insert(p, PredicateState { map, closure });
    }

    /// Seed predicate `p`'s retained closure from its **true asserted base**
    /// edges (not an already-closed extent), computing the closure at seed time.
    /// Unlike [`Self::seed_transitive_closure`] — which seeds a closed extent as
    /// a conservative base and can under-withdraw when a seeded edge is retracted
    /// — this records the genuine base, so retracting any seeded edge is
    /// **exact** (SPEC-24 S2). Costs one closure computation at seed; use the
    /// closed-extent seed only when the true base is unavailable. Replaces any
    /// existing state for `p`; writes nothing to a sink.
    pub fn seed_base_edges(&mut self, p: PredicateId, base_edges: &[(DictId, DictId)]) {
        let mut map = DenseIdMap::with_capacity(base_edges.len() * 2);
        let dense = map.intern_edges(base_edges);
        let closure = IncrementalTransitiveClosure::from_base_edges(dense);
        self.predicates.insert(p, PredicateState { map, closure });
    }

    /// Insert `new_edges` into predicate `p`'s transitive closure and write the
    /// newly inferred triples to `sink`. Returns the number of triples the sink
    /// reports written. Edges already implied by the existing closure produce
    /// no output.
    pub fn insert_transitive_edges(
        &mut self,
        p: PredicateId,
        new_edges: &[(DictId, DictId)],
        sink: &dyn TripleSink,
    ) -> Result<u64> {
        if new_edges.is_empty() {
            return Ok(0);
        }
        let state = self.predicates.entry(p).or_default();
        // intern_edges only adds new dict ids to the map; extra interned ids
        // with no edges are harmless and reused correctly on retry, so the
        // DenseIdMap is intentionally NOT rolled back on sink error.
        let dense = state.map.intern_edges(new_edges);
        // Fold edges one at a time, tracking which BASE edges this call newly
        // added (Finding 5). On a sink failure we must roll back not only the
        // closure delta but also the base edges we just added — otherwise an
        // aborted asserted edge lingers in `base` and wrongly supports later
        // retractions/reachability. A re-inserted edge that already existed in
        // `base` is NOT recorded here, so rollback never removes pre-existing
        // support.
        let mut delta = Vec::new();
        let mut new_base_edges = Vec::new();
        for (s, o) in dense {
            let (edge_delta, base_was_new) = state.closure.insert_edge_tracked(s, o);
            delta.extend(edge_delta);
            if base_was_new {
                new_base_edges.push((s, o));
            }
        }
        if delta.is_empty() {
            return Ok(0);
        }
        match write_closure(p, &delta, &state.map, sink) {
            Ok(n) => Ok(n),
            Err(e) => {
                // Sink write failed: roll back the just-inserted closure edges
                // AND the base edges this call newly added (Finding 5), so the
                // aborted assertion leaves no trace in either the closed set or
                // `base`. A retry re-emits them. Map interns are left in place —
                // they are harmless and will be reused correctly on retry.
                state.closure.rollback_inserted(&delta);
                state.closure.rollback_base_edges(&new_base_edges);
                Err(e)
            }
        }
    }

    /// Retract `edges` (asserted base edges) from predicate `p`'s transitive
    /// closure and **return** the retraction [`DictDeleteOutcome`] (mapped back
    /// to `DictId`): the `withdrawn` closure pairs that lose all support, and any
    /// `survived` deleted base edge that remains entailed via another path.
    /// Mirrors [`Self::insert_transitive_edges`] but for the negative direction.
    ///
    /// No sink parameter and nothing is written to a sink: the `+/-` sign lives
    /// in the SPEC-06 Z-set layer, which negates `withdrawn` and PROMOTES
    /// `survived` to materialized derived rows (BUG P1). If `p` has no retained
    /// state, returns an empty outcome. Edges that were never asserted withdraw
    /// nothing. **For a predicate seeded via [`Self::seed_transitive_closure`]**
    /// the closed extent is used as a conservative base, so post-seed inserted
    /// edges retract exactly while retracting a seeded edge is sound but may
    /// under-withdraw. See that method's retraction note.
    pub fn delete_transitive_edges(
        &mut self,
        p: PredicateId,
        edges: &[(DictId, DictId)],
    ) -> Result<DictDeleteOutcome> {
        if edges.is_empty() {
            return Ok(DictDeleteOutcome::default());
        }
        let Some(state) = self.predicates.get_mut(&p) else {
            return Ok(DictDeleteOutcome::default());
        };
        // Map DictId endpoints to dense ids. An endpoint we have never interned
        // cannot be part of any base edge, so it contributes no deletion; skip it.
        let dense: Vec<(u64, u64)> = edges
            .iter()
            .filter_map(|&(s, o)| Some((state.map.to_dense(s)?.0, state.map.to_dense(o)?.0)))
            .collect();
        let DeleteOutcome {
            withdrawn,
            survived,
        } = state.closure.delete_edges(dense);
        Ok(DictDeleteOutcome {
            withdrawn: pairs_to_dict(&state.map, withdrawn),
            survived: pairs_to_dict(&state.map, survived),
        })
    }

    /// All asserted base edges retained for predicate `p`, in `DictId` space
    /// (unordered; caller sorts). Returns an empty vec if `p` has no retained
    /// state. Primarily for tests and debugging — lets callers verify that an
    /// aborted insert left no base edge behind (Finding 5).
    pub fn base_edges(&self, p: PredicateId) -> Vec<(DictId, DictId)> {
        let Some(state) = self.predicates.get(&p) else {
            return Vec::new();
        };
        pairs_to_dict(&state.map, state.closure.base_edges())
    }

    /// Union `owl:sameAs` pairs (shared with the bulk backend's semantics).
    pub fn add_sameas(&mut self, pairs: &[(DictId, DictId)]) {
        for &(a, b) in pairs {
            self.sameas.union(a, b);
        }
    }

    /// Borrow the equivalence-class state.
    pub fn equiv_classes(&self) -> &EquivClasses {
        &self.sameas
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Triple;

    /// A sink whose `bulk_insert_inferred` always fails — used to prove the
    /// insert path rolls back cleanly on a downstream write failure.
    struct ErroringSink;
    impl TripleSink for ErroringSink {
        fn bulk_insert_inferred(&self, _triples: &mut dyn Iterator<Item = Triple>) -> Result<u64> {
            anyhow::bail!("sink write failed")
        }
    }

    /// A sink that succeeds — to establish base state around a failing insert.
    struct OkSink;
    impl TripleSink for OkSink {
        fn bulk_insert_inferred(&self, triples: &mut dyn Iterator<Item = Triple>) -> Result<u64> {
            Ok(triples.count() as u64)
        }
    }

    fn sorted(mut v: Vec<(DictId, DictId)>) -> Vec<(u64, u64)> {
        let mut out: Vec<(u64, u64)> = v.drain(..).map(|(s, o)| (s.0, o.0)).collect();
        out.sort_unstable();
        out
    }

    /// Finding 5: a failed sink write must roll back the BASE edge too, not just
    /// the closure delta. Before the fix the aborted asserted edge lingered in
    /// `base` and could wrongly support later retractions/reachability.
    #[test]
    fn failed_sink_insert_rolls_back_base_edge() {
        let p = PredicateId(100);
        let mut backend = IncrementalClosureBackend::new();

        // Establish a clean existing edge (1,2) via a succeeding sink.
        backend
            .insert_transitive_edges(p, &[(DictId(1), DictId(2))], &OkSink)
            .expect("ok insert");
        assert_eq!(sorted(backend.base_edges(p)), vec![(1, 2)]);

        // Now attempt to insert (3,4) through a FAILING sink: the call must Err
        // and leave NO trace of (3,4) in base.
        let err = backend.insert_transitive_edges(p, &[(DictId(3), DictId(4))], &ErroringSink);
        assert!(err.is_err(), "failing sink must propagate the error");
        assert_eq!(
            sorted(backend.base_edges(p)),
            vec![(1, 2)],
            "aborted (3,4) must NOT linger in base; only the pre-existing (1,2) survives"
        );

        // A subsequent retraction of the unrelated (1,2) is unaffected: it is
        // genuinely a base edge and withdraws (1,2).
        let outcome = backend
            .delete_transitive_edges(p, &[(DictId(1), DictId(2))])
            .expect("delete");
        assert_eq!(
            outcome.withdrawn,
            vec![(DictId(1), DictId(2))],
            "retracting the unrelated (1,2) must withdraw it cleanly"
        );
        assert!(backend.base_edges(p).is_empty(), "base now empty");
    }

    /// A failed insert that RE-INSERTS an already-present base edge must NOT
    /// remove that pre-existing edge on rollback (only genuinely-new base edges
    /// are rolled back).
    #[test]
    fn failed_sink_insert_keeps_preexisting_base_edge() {
        let p = PredicateId(100);
        let mut backend = IncrementalClosureBackend::new();
        backend
            .insert_transitive_edges(p, &[(DictId(1), DictId(2))], &OkSink)
            .expect("ok insert");

        // Attempt to insert (1,2) AGAIN plus a new (2,3), through a failing sink.
        // (1,2) is already a base edge, so even though the whole call aborts, the
        // pre-existing (1,2) must survive; only the new (2,3) is rolled back.
        let err = backend.insert_transitive_edges(
            p,
            &[(DictId(1), DictId(2)), (DictId(2), DictId(3))],
            &ErroringSink,
        );
        assert!(err.is_err());
        assert_eq!(
            sorted(backend.base_edges(p)),
            vec![(1, 2)],
            "pre-existing (1,2) survives; new (2,3) rolled back"
        );
    }

    /// A predicate seeded via `seed_transitive_closure` now seeds its `base`
    /// from the closed extent, so a *post-seed inserted* edge retracts exactly.
    /// Replaces the former `delete_on_closed_seeded_predicate_is_noop`, which
    /// pinned the OLD blanket no-op (now wrong: it leaked the inserted edge's
    /// derived closure on retraction).
    ///
    /// Seed closed {(1,2)}; insert (2,1) [now the closure holds the 1<->2 cycle:
    /// {(1,1),(1,2),(2,1),(2,2)}]; delete (2,1). The cycle-derived (1,1),(2,1),
    /// (2,2) lose all support and are withdrawn; the originally-seeded (1,2)
    /// survives (it is still in base and still reachable). This matches
    /// `transitive_closure` of the remaining base {(1,2)} == {(1,2)}.
    #[test]
    fn delete_on_closed_seeded_predicate_post_seed_insert_retracts() {
        let p = PredicateId(7);
        let mut backend = IncrementalClosureBackend::new();
        backend.seed_transitive_closure(p, &[(DictId(1), DictId(2))]);

        // Insert (2,1) — closes the 1<->2 cycle on top of the seeded (1,2).
        backend
            .insert_transitive_edges(p, &[(DictId(2), DictId(1))], &OkSink)
            .expect("ok insert");

        // Retract (2,1): the inserted edge retracts exactly. The cycle-derived
        // (1,1),(2,1),(2,2) are withdrawn; the seeded (1,2) survives.
        let out = backend
            .delete_transitive_edges(p, &[(DictId(2), DictId(1))])
            .expect("delete");
        assert_eq!(
            sorted(out.withdrawn),
            vec![(1, 1), (2, 1), (2, 2)],
            "retracting the post-seed (2,1) withdraws the cycle-derived pairs"
        );
        assert!(
            out.survived.is_empty(),
            "(2,1) lost all support, so it is not a survivor; got {:?}",
            out.survived
        );
        // The closure now equals transitive_closure({(1,2)}) == {(1,2)}.
        let state = backend.predicates.get(&p).expect("state");
        let s = state.map.to_dense(DictId(1)).expect("dense 1").0;
        let o = state.map.to_dense(DictId(2)).expect("dense 2").0;
        assert_eq!(
            state.closure.edges(),
            vec![(s, o)],
            "only the seeded (1,2) remains after retracting the inserted (2,1)"
        );
    }

    /// Exact warm-store seed: seeding the TRUE asserted base (not the closed extent)
    /// makes retracting a seeded edge exact, not conservative. Seed base {(1,2),(2,3)}
    /// (closes 1->3); retract the seeded (2,3): withdraws (1,3) and (2,3) exactly.
    #[test]
    fn seed_base_edges_retracts_exactly() {
        let p = PredicateId(9);
        let mut backend = IncrementalClosureBackend::new();
        backend.seed_base_edges(p, &[(DictId(1), DictId(2)), (DictId(2), DictId(3))]);
        let out = backend
            .delete_transitive_edges(p, &[(DictId(2), DictId(3))])
            .expect("delete");
        assert_eq!(
            sorted(out.withdrawn),
            vec![(1, 3), (2, 3)],
            "base-seeded retraction is exact: (1,3) and (2,3) withdraw"
        );
        assert!(out.survived.is_empty());
    }

    /// The closed-extent seed is conservative: seeding the CLOSED extent
    /// {(1,2),(2,3),(1,3)} and retracting the seeded (2,3) leaves (1,3) reachable
    /// via the redundant seeded (1,3), so it does NOT withdraw (1,3) — the exact
    /// base seed above does. This pins the documented behavioural difference.
    #[test]
    fn seed_closed_edges_under_withdraws_vs_base_seed() {
        let p = PredicateId(9);
        let mut backend = IncrementalClosureBackend::new();
        backend.seed_transitive_closure(
            p,
            &[
                (DictId(1), DictId(2)),
                (DictId(2), DictId(3)),
                (DictId(1), DictId(3)),
            ],
        );
        let out = backend
            .delete_transitive_edges(p, &[(DictId(2), DictId(3))])
            .expect("delete");
        // (1,3) survives via the seeded direct (1,3): conservative, not exact.
        assert_eq!(sorted(out.withdrawn), vec![(2, 3)]);
    }
}
