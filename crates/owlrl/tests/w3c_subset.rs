//! Hand-encoded fixtures patterned on the W3C OWL 2 RL test suite.
//! Each test corresponds to a single normative rule or simple rule combination.

use reasoner_owlrl::backend::RuleFiringBackend;
use reasoner_owlrl::materialize;
use reasoner_owlrl::store::{MemStore, TripleStore};
use reasoner_owlrl::types::{TermId, Triple};
use reasoner_owlrl::vocab::Vocabulary;

fn t(s: u64, p: u64, o: u64) -> Triple {
    Triple::new(TermId(s), TermId(p), TermId(o))
}

struct Case {
    name: &'static str,
    asserted: Vec<Triple>,
    expected: Vec<Triple>,
    forbidden: Vec<Triple>,
}

fn run(case: Case, v: Vocabulary) {
    let mut s = MemStore::new(v);
    for t in &case.asserted {
        s.assert(*t);
    }
    let mut b = RuleFiringBackend::new();
    materialize(&mut s, &mut b);
    for t in &case.expected {
        assert!(s.contains(t), "{}: missing expected {:?}", case.name, t);
    }
    for t in &case.forbidden {
        assert!(
            !s.contains(t),
            "{}: forbidden triple was derived {:?}",
            case.name,
            t
        );
    }
}

#[test]
fn fixtures() {
    let v = Vocabulary::synthetic(10_000);
    let ty = v.rdf_type.0;
    let sco = v.rdfs_sub_class_of.0;
    let dom = v.rdfs_domain.0;
    let rng = v.rdfs_range.0;
    let sp = v.rdfs_sub_property_of.0;

    // 1. cax-sco: direct subClassOf.
    run(
        Case {
            name: "cax-sco-direct",
            asserted: vec![t(1, sco, 2), t(100, ty, 1)],
            expected: vec![t(100, ty, 2)],
            forbidden: vec![],
        },
        v,
    );

    // 2. scm-sco (closure) + cax-sco: A ⊑ B ⊑ C, x : A ⇒ x : C.
    run(
        Case {
            name: "scm-sco-then-cax-sco",
            asserted: vec![t(1, sco, 2), t(2, sco, 3), t(100, ty, 1)],
            expected: vec![t(100, ty, 2), t(100, ty, 3), t(1, sco, 3)],
            forbidden: vec![],
        },
        v,
    );

    // 3. prp-dom: property domain.
    run(
        Case {
            name: "prp-dom",
            asserted: vec![t(50, dom, 60), t(100, 50, 200)],
            expected: vec![t(100, ty, 60)],
            forbidden: vec![],
        },
        v,
    );

    // 4. prp-rng: property range.
    run(
        Case {
            name: "prp-rng",
            asserted: vec![t(50, rng, 60), t(100, 50, 200)],
            expected: vec![t(200, ty, 60)],
            forbidden: vec![],
        },
        v,
    );

    // 5. prp-spo1: sub-property propagation.
    run(
        Case {
            name: "prp-spo1",
            asserted: vec![t(50, sp, 60), t(100, 50, 200)],
            expected: vec![t(100, 60, 200)],
            forbidden: vec![],
        },
        v,
    );

    // 6. prp-symp: symmetric property.
    run(
        Case {
            name: "prp-symp",
            asserted: vec![t(50, ty, v.owl_symmetric_property.0), t(100, 50, 200)],
            expected: vec![t(200, 50, 100)],
            forbidden: vec![],
        },
        v,
    );

    // 7. prp-inv1+inv2 cross-fire.
    run(
        Case {
            name: "prp-inv-both",
            asserted: vec![
                t(50, v.owl_inverse_of.0, 60),
                t(100, 50, 200),
                t(300, 60, 400),
            ],
            expected: vec![t(200, 60, 100), t(400, 50, 300)],
            forbidden: vec![],
        },
        v,
    );

    // 8. cax-eqc1+eqc2: equivalentClass instance propagation both ways.
    run(
        Case {
            name: "cax-eqc-both",
            asserted: vec![
                t(1, v.owl_equivalent_class.0, 2),
                t(100, ty, 1),
                t(101, ty, 2),
            ],
            expected: vec![t(100, ty, 2), t(101, ty, 1)],
            forbidden: vec![],
        },
        v,
    );

    // 9. scm-eqc1+eqc2: equivalentClass implies subClassOf both ways.
    run(
        Case {
            name: "scm-eqc-both",
            asserted: vec![t(1, v.owl_equivalent_class.0, 2)],
            expected: vec![t(1, sco, 2), t(2, sco, 1)],
            forbidden: vec![],
        },
        v,
    );

    // 10. scm-dom1: domain narrows along subClassOf.
    run(
        Case {
            name: "scm-dom1",
            asserted: vec![t(50, dom, 60), t(60, sco, 70)],
            expected: vec![t(50, dom, 70)],
            forbidden: vec![],
        },
        v,
    );

    // 11. cls-hv1: hasValue → property triple.
    run(
        Case {
            name: "cls-hv1",
            asserted: vec![
                t(70, v.owl_has_value.0, 90),
                t(70, v.owl_on_property.0, 80),
                t(100, ty, 70),
            ],
            expected: vec![t(100, 80, 90)],
            forbidden: vec![],
        },
        v,
    );

    // 12. eq-sym (closure-delegated): sameAs symmetry.
    run(
        Case {
            name: "eq-sym",
            asserted: vec![t(1, v.owl_same_as.0, 2)],
            expected: vec![t(2, v.owl_same_as.0, 1)],
            forbidden: vec![],
        },
        v,
    );
}
