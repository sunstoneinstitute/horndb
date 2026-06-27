//! SPEC-11 F4 — monotone negative-mapping chaining (the inference.md xanthene case).
use horndb_owlrl::backend::RuleFiringBackend;
use horndb_owlrl::engine::materialize;
use horndb_owlrl::store::{MemStore, TripleStore};
use horndb_owlrl::types::{TermId, Triple};
use horndb_owlrl::vocab::Vocabulary;

fn t(s: TermId, p: TermId, o: TermId) -> Triple {
    Triple::new(s, p, o)
}

#[test]
fn positive_then_negative_yields_negative_not_positive() {
    let v = Vocabulary::synthetic(10_000);
    let mut s = MemStore::new(v);
    let a = TermId(1); // e.g. xanthene-A
    let b = TermId(2);
    let c = TermId(3);
    // A exactMatch B (positive),  B exactMatch[Not] C (negated).
    s.assert(t(a, v.skos_exact_match, b));
    s.assert(t(b, v.horndb_not_exact_match, c));
    materialize(&mut s, &mut RuleFiringBackend::new());

    // Derives the negated A-C mapping...
    assert!(s.contains(&t(a, v.horndb_not_exact_match, c)));
    // ...and must NOT derive a positive exactMatch across the negative link.
    assert!(!s.contains(&t(a, v.skos_exact_match, c)));
}
