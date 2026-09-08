//! Rule shapes shared by the matrix-based closure backends.
//!
//! A matrix backend such as
//! [`GraphBlasBackend`](crate::graphblas_backend::GraphBlasBackend) supplies
//! only *how* one transitive closure is computed. Everything around that —
//! which predicates get closed, how `owl:sameAs` is symmetrized, which
//! `rule_id` each derived triple carries — lives here, so any future backend
//! shares this logic instead of re-deriving it.
//!
//! # Parity
//!
//! A matrix backend MUST return the same triple **set** as
//! [`RuleFiringBackend`](crate::backend::RuleFiringBackend) for every input —
//! the acceptance gate in SPEC-05 § Acceptance criteria (`HDB-SPEC-5`),
//! enforced by `tests/closure_backend_differential.rs`. The non-obvious
//! decisions:
//!
//! - `scm-sco` / `scm-spo` use the **strict** transitive closure (no
//!   identity). `RuleFiringBackend` does not add reflexive `?c ⊑ ?c` — that
//!   comes from a separate compiled rule, not the backend. We therefore
//!   deliberately do **not** use `horndb_closure::sink::BackendImpl`, whose
//!   `close_subclass` adds the identity.
//! - `owl:sameAs` is closed as the strict transitive closure of the
//!   **symmetrized** edge set `M ∨ Mᵀ`. This reproduces `eq-sym` followed by
//!   `eq-trans` iterated to fixpoint, including the diagonal `(a,a)` for any
//!   element in a class of size ≥ 2 (`a↔b ⇒ a→b→a`). We emit the closure as
//!   triples (the engine materializes `eq-*` into the store) rather than into
//!   an `EquivClasses` union-find, which is why we bypass
//!   `BackendImpl::add_sameas`.
//! - `eq-ref` (reflexive `?x sameAs ?x` for arbitrary `x`) is **not** computed,
//!   matching `RuleFiringBackend`.
//!
//! Provenance `premises` are recorded best-effort (empty): the gate is the
//! derived triple set, not the proof tree. `rule_id` is set correctly so
//! downstream provenance routing (SPEC-08) still sees the right rule.

use horndb_closure::types::DictId;
use rustc_hash::FxHashSet;

use crate::delta::Delta;
use crate::provenance::Provenance;
use crate::store::TripleStore;
use crate::types::{RuleId, TermId, Triple};

/// The one operation a matrix closure backend has to supply: the **strict**
/// transitive closure (`M ∨ M² ∨ M³ ∨ …`, no identity) of an edge list.
/// Empty input must give empty output.
pub(crate) trait ClosureKernel {
    fn closure_edges(&mut self, edges: &[(DictId, DictId)]) -> Vec<(TermId, TermId)>;
}

/// Run every closure-delegated rule shape over `store` using `kernel`.
pub(crate) fn close_all<K: ClosureKernel>(store: &dyn TripleStore, kernel: &mut K) -> Delta {
    let v = *store.vocab();
    let mut out = Delta::new();

    // scm-sco: strict transitive closure of rdfs:subClassOf.
    close_transitive(store, kernel, v.rdfs_sub_class_of, "scm-sco", &mut out);
    // scm-spo: strict transitive closure of rdfs:subPropertyOf.
    close_transitive(store, kernel, v.rdfs_sub_property_of, "scm-spo", &mut out);
    // eq-sym + eq-trans: strict transitive closure of the symmetrized
    // owl:sameAs relation.
    close_sameas(store, kernel, v.owl_same_as, &mut out);
    // prp-trp: strict transitive closure of each declared transitive property.
    close_transitive_properties(store, kernel, &v, &mut out);
    // TODO(TASKS.md #130): SPEC-11 T1 — the matrix backends do not yet close
    // the SSSOM mapping predicates (skos:exactMatch/broadMatch/narrowMatch)
    // that RuleFiringBackend closes. Bring to parity (SPEC-11/SPEC-05
    // follow-up).

    out
}

/// Read predicate `pred`'s edges from the store as dictionary-id pairs.
fn scan_edges(store: &dyn TripleStore, pred: TermId) -> Vec<(DictId, DictId)> {
    store
        .scan_predicate(pred)
        .map(|t| (DictId(t.s.0), DictId(t.o.0)))
        .collect()
}

/// Emit the strict transitive closure of `pred` under `rule_id`, skipping
/// triples already present in the store.
fn close_transitive<K: ClosureKernel>(
    store: &dyn TripleStore,
    kernel: &mut K,
    pred: TermId,
    rule_id: RuleId,
    out: &mut Delta,
) {
    let edges = scan_edges(store, pred);
    for (s, o) in kernel.closure_edges(&edges) {
        emit(store, Triple::new(s, pred, o), rule_id, out);
    }
}

/// Emit the symmetric-transitive closure of `owl:sameAs`. Edges whose reverse
/// is an asserted `sameAs` pair are attributed to `eq-sym`; the rest to
/// `eq-trans` (best-effort — only the triple set is gated).
fn close_sameas<K: ClosureKernel>(
    store: &dyn TripleStore,
    kernel: &mut K,
    same_as: TermId,
    out: &mut Delta,
) {
    let asserted = scan_edges(store, same_as);
    if asserted.is_empty() {
        return;
    }
    let asserted_set: FxHashSet<(DictId, DictId)> = asserted.iter().copied().collect();
    // Symmetrize before closing: M ∨ Mᵀ.
    let mut sym = Vec::with_capacity(asserted.len() * 2);
    for &(a, b) in &asserted {
        sym.push((a, b));
        sym.push((b, a));
    }
    for (s, o) in kernel.closure_edges(&sym) {
        // A derived edge that is the reverse of an asserted pair is an eq-sym
        // step; anything else only arises through transitivity.
        let rule_id: RuleId = if asserted_set.contains(&(DictId(o.0), DictId(s.0))) {
            "eq-sym"
        } else {
            "eq-trans"
        };
        emit(store, Triple::new(s, same_as, o), rule_id, out);
    }
}

/// Close every predicate declared `(p rdf:type owl:TransitiveProperty)`.
fn close_transitive_properties<K: ClosureKernel>(
    store: &dyn TripleStore,
    kernel: &mut K,
    vocab: &crate::vocab::Vocabulary,
    out: &mut Delta,
) {
    let predicates: Vec<TermId> = store
        .scan_predicate(vocab.rdf_type)
        .filter(|t| t.o == vocab.owl_transitive_property)
        .map(|t| t.s)
        .collect();
    for p in predicates {
        close_transitive(store, kernel, p, "prp-trp", out);
    }
}

/// Insert `head` into `out` iff it is not already materialized in the store.
fn emit(store: &dyn TripleStore, head: Triple, rule_id: RuleId, out: &mut Delta) {
    if !store.contains(&head) {
        out.insert(head, Provenance::new(rule_id, std::iter::empty()));
    }
}
