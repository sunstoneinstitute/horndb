//! ADR-0017 — skos:exactMatch is a crosswalk edge, NEVER OWL identity.
use horndb_owlrl::backend::RuleFiringBackend;
use horndb_owlrl::engine::materialize;
use horndb_owlrl::store::{MemStore, TripleStore};
use horndb_owlrl::types::{TermId, Triple};
use horndb_owlrl::vocab::Vocabulary;

fn t(s: TermId, p: TermId, o: TermId) -> Triple {
    Triple::new(s, p, o)
}

#[test]
fn exact_match_never_becomes_sameas() {
    let v = Vocabulary::synthetic(10_000);
    let mut s = MemStore::new(v);
    let a = TermId(1);
    let b = TermId(2);
    let p = TermId(3);
    let o = TermId(4);
    // A exactMatch B, plus a triple about A.
    s.assert(t(a, v.skos_exact_match, b));
    s.assert(t(a, p, o));
    materialize(&mut s, &mut RuleFiringBackend::new());

    // exactMatch must NOT create owl:sameAs identity...
    assert!(!s.contains(&t(a, v.owl_same_as, b)));
    // ...and must NOT substitute A's triples onto B (no eq-rep-* over a crosswalk).
    assert!(!s.contains(&t(b, p, o)));
}

#[test]
fn sameas_still_reaches_identity() {
    let v = Vocabulary::synthetic(10_000);
    let mut s = MemStore::new(v);
    let a = TermId(1);
    let b = TermId(2);
    let p = TermId(3);
    let o = TermId(4);
    s.assert(t(a, v.owl_same_as, b));
    s.assert(t(a, p, o));
    materialize(&mut s, &mut RuleFiringBackend::new());

    // Genuine owl:sameAs DOES substitute (eq-rep-s) and is symmetric (eq-sym).
    assert!(s.contains(&t(b, p, o)));
    assert!(s.contains(&t(b, v.owl_same_as, a)));
}
