//! Differential parity: the GraphBLAS closure backend must materialize an
//! identical triple set to the reference `RuleFiringBackend` on every input
//! (acceptance gate for #61). This extends SPEC-05's differential-equality
//! discipline (already present in `crates/closure` / `crates/incremental`) to
//! the owlrl `Engine` path.
//!
//! Each fixture drives the *full* materialize loop — not just the backend in
//! isolation — so a divergence in how the closure delta cascades through the
//! compiled rules (`cax-sco`, `prp-spo`, `eq-rep-*`) or the list rules would
//! also be caught.
//!
//! Only built with `--features graphblas-backend`.
#![cfg(feature = "graphblas-backend")]

use std::collections::BTreeSet;

use horndb_owlrl::{BackendChoice, Engine};
use oxrdf::{Dataset, GraphName, NamedNode, NamedOrBlankNode, Quad};

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDFS_SUBCLASS_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";
const RDFS_SUBPROPERTY_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subPropertyOf";
const OWL_SAME_AS: &str = "http://www.w3.org/2002/07/owl#sameAs";
const OWL_TRANSITIVE_PROPERTY: &str = "http://www.w3.org/2002/07/owl#TransitiveProperty";

fn ex(local: &str) -> String {
    format!("http://example.org/{local}")
}

fn nq(s: &str, p: &str, o: &str) -> Quad {
    Quad::new(
        NamedOrBlankNode::NamedNode(NamedNode::new(s).unwrap()),
        NamedNode::new(p).unwrap(),
        NamedNode::new(o).unwrap(),
        GraphName::DefaultGraph,
    )
}

/// Materialize `dataset` with the given backend and return the closure as a
/// sorted triple set.
fn materialize(dataset: &Dataset, backend: BackendChoice) -> BTreeSet<(String, String, String)> {
    let mut engine = Engine::with_backend(backend);
    engine.load(dataset).expect("load");
    engine
        .materialized_triples()
        .expect("materialized_triples after load")
        .into_iter()
        .collect()
}

/// Assert the two backends produce byte-for-byte identical closures, printing
/// the symmetric difference on failure.
fn assert_parity(name: &str, dataset: &Dataset) {
    let rule_firing = materialize(dataset, BackendChoice::RuleFiring);
    let graphblas = materialize(dataset, BackendChoice::GraphBlas);
    if rule_firing != graphblas {
        let only_rf: Vec<_> = rule_firing.difference(&graphblas).collect();
        let only_gb: Vec<_> = graphblas.difference(&rule_firing).collect();
        panic!(
            "closure parity mismatch for {name}:\n  RuleFiring-only ({}): {only_rf:#?}\n  GraphBlas-only ({}): {only_gb:#?}",
            only_rf.len(),
            only_gb.len(),
        );
    }
}

#[test]
fn subclass_chain_with_instances() {
    // A ⊑ B ⊑ C ⊑ D, plus typed instances → exercises scm-sco + cax-sco cascade.
    let mut d = Dataset::new();
    d.insert(&nq(&ex("A"), RDFS_SUBCLASS_OF, &ex("B")));
    d.insert(&nq(&ex("B"), RDFS_SUBCLASS_OF, &ex("C")));
    d.insert(&nq(&ex("C"), RDFS_SUBCLASS_OF, &ex("D")));
    d.insert(&nq(&ex("i1"), RDF_TYPE, &ex("A")));
    d.insert(&nq(&ex("i2"), RDF_TYPE, &ex("B")));
    assert_parity("subclass_chain_with_instances", &d);
}

#[test]
fn subproperty_chain_with_data() {
    // p ⊑ q ⊑ r, plus data on p → scm-spo + prp-spo cascade.
    let mut d = Dataset::new();
    d.insert(&nq(&ex("p"), RDFS_SUBPROPERTY_OF, &ex("q")));
    d.insert(&nq(&ex("q"), RDFS_SUBPROPERTY_OF, &ex("r")));
    d.insert(&nq(&ex("s1"), &ex("p"), &ex("o1")));
    assert_parity("subproperty_chain_with_data", &d);
}

#[test]
fn sameas_classes_various_sizes() {
    // sameAs classes of size 2, 3 and 4 → eq-sym + eq-trans, including the
    // diagonal that arises for non-singleton classes.
    let mut d = Dataset::new();
    // size 2
    d.insert(&nq(&ex("a1"), OWL_SAME_AS, &ex("a2")));
    // size 3 (chain)
    d.insert(&nq(&ex("b1"), OWL_SAME_AS, &ex("b2")));
    d.insert(&nq(&ex("b2"), OWL_SAME_AS, &ex("b3")));
    // size 4 (star)
    d.insert(&nq(&ex("c1"), OWL_SAME_AS, &ex("c2")));
    d.insert(&nq(&ex("c1"), OWL_SAME_AS, &ex("c3")));
    d.insert(&nq(&ex("c1"), OWL_SAME_AS, &ex("c4")));
    assert_parity("sameas_classes_various_sizes", &d);
}

#[test]
fn sameas_with_property_replacement() {
    // sameAs interacting with assertions → eq-rep-* cascade off the closure.
    let mut d = Dataset::new();
    d.insert(&nq(&ex("x"), OWL_SAME_AS, &ex("y")));
    d.insert(&nq(&ex("y"), OWL_SAME_AS, &ex("z")));
    d.insert(&nq(&ex("x"), &ex("knows"), &ex("w")));
    d.insert(&nq(&ex("w"), &ex("knows"), &ex("z")));
    assert_parity("sameas_with_property_replacement", &d);
}

#[test]
fn transitive_property_chain() {
    // p is a TransitiveProperty over a chain and a small DAG → prp-trp.
    let mut d = Dataset::new();
    d.insert(&nq(&ex("ancestorOf"), RDF_TYPE, OWL_TRANSITIVE_PROPERTY));
    d.insert(&nq(&ex("n1"), &ex("ancestorOf"), &ex("n2")));
    d.insert(&nq(&ex("n2"), &ex("ancestorOf"), &ex("n3")));
    d.insert(&nq(&ex("n3"), &ex("ancestorOf"), &ex("n4")));
    d.insert(&nq(&ex("n2"), &ex("ancestorOf"), &ex("n5")));
    assert_parity("transitive_property_chain", &d);
}

#[test]
fn sameas_cycle() {
    // A 3-cycle of sameAs forces the full diagonal + every ordered pair.
    let mut d = Dataset::new();
    d.insert(&nq(&ex("p1"), OWL_SAME_AS, &ex("p2")));
    d.insert(&nq(&ex("p2"), OWL_SAME_AS, &ex("p3")));
    d.insert(&nq(&ex("p3"), OWL_SAME_AS, &ex("p1")));
    assert_parity("sameas_cycle", &d);
}

#[test]
fn mixed_all_families() {
    // Every closure family at once, with cross-cutting instance data.
    let mut d = Dataset::new();
    d.insert(&nq(&ex("Animal"), RDFS_SUBCLASS_OF, &ex("LivingThing")));
    d.insert(&nq(&ex("Dog"), RDFS_SUBCLASS_OF, &ex("Animal")));
    d.insert(&nq(
        &ex("hasParent"),
        RDFS_SUBPROPERTY_OF,
        &ex("hasAncestor"),
    ));
    d.insert(&nq(&ex("hasAncestor"), RDF_TYPE, OWL_TRANSITIVE_PROPERTY));
    d.insert(&nq(&ex("rex"), RDF_TYPE, &ex("Dog")));
    d.insert(&nq(&ex("rex"), &ex("hasParent"), &ex("fido")));
    d.insert(&nq(&ex("fido"), &ex("hasParent"), &ex("spot")));
    d.insert(&nq(&ex("rex"), OWL_SAME_AS, &ex("rextoo")));
    assert_parity("mixed_all_families", &d);
}

#[test]
fn empty_dataset() {
    assert_parity("empty_dataset", &Dataset::new());
}
