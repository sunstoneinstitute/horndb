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
        let s_dict = map.to_dict(DenseIdx(s))?;
        let o_dict = map.to_dict(DenseIdx(o))?;
        Some(Triple {
            s: s_dict,
            p,
            o: o_dict,
        })
    });
    sink.bulk_insert_inferred(&mut iter)
}

/// Convenience constructor for callers (SPEC-04 will use this until it has
/// its own factory).
pub fn default_backend() -> BackendImpl {
    BackendImpl::default()
}
