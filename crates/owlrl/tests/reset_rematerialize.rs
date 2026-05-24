//! SPEC-04 F7: reset_and_materialize produces a bit-identical store.

use horndb_owlrl::backend::RuleFiringBackend;
use horndb_owlrl::store::{MemStore, TripleStore};
use horndb_owlrl::types::{TermId, Triple};
use horndb_owlrl::vocab::Vocabulary;
use horndb_owlrl::{materialize, reset_and_materialize};

fn t(s: u64, p: u64, o: u64) -> Triple {
    Triple::new(TermId(s), TermId(p), TermId(o))
}

#[test]
fn reset_then_rematerialize_is_identical() {
    let v = Vocabulary::synthetic(10_000);
    let mut s = MemStore::new(v);
    s.assert(t(1, v.rdfs_sub_class_of.0, 2));
    s.assert(t(2, v.rdfs_sub_class_of.0, 3));
    s.assert(t(100, v.rdf_type.0, 1));
    s.assert(t(101, v.rdf_type.0, 2));
    let mut b = RuleFiringBackend::new();

    materialize(&mut s, &mut b);
    let first = s.all_triples();

    reset_and_materialize(&mut s, &mut b);
    let second = s.all_triples();

    assert_eq!(
        first, second,
        "rematerialization differed from initial materialization"
    );
    assert!(
        first.len() > 4,
        "expected some inferred triples; got {}",
        first.len()
    );
}
