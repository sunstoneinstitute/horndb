//! Proof-tree integration tests (SPEC-04 F4, acceptance #5, NF4).
//!
//! Proves that a multi-step (deep) derivation produces a correct proof
//! tree that bottoms out at asserted triples, and that building it stays
//! well under the NF4 budget (100 ms).

use horndb_owlrl::integration::{Engine, StringProofTree};
use oxrdf::{Dataset, GraphName, NamedNode, NamedOrBlankNode, Quad};

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDFS_SUB_CLASS_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";

/// Build an `oxrdf` default-graph quad from three IRIs (mirrors the `nq`
/// helper in `integration.rs`'s private test module).
fn nq(s: &str, p: &str, o: &str) -> Quad {
    Quad::new(
        NamedOrBlankNode::NamedNode(NamedNode::new(s).unwrap()),
        NamedNode::new(p).unwrap(),
        NamedNode::new(o).unwrap(),
        GraphName::DefaultGraph,
    )
}

/// Count leaves and compute the depth of a proof tree.
/// Leaves (`Asserted`/`Cycle`, or an empty-premise `Derived`) have depth 1.
fn leaf_count_and_depth(t: &StringProofTree) -> (usize, usize) {
    match t {
        StringProofTree::Asserted(_) | StringProofTree::Cycle(_) => (1, 1),
        StringProofTree::Derived { premises, .. } => {
            if premises.is_empty() {
                return (1, 1);
            }
            let mut leaves = 0;
            let mut max_child = 0;
            for p in premises {
                let (l, d) = leaf_count_and_depth(p);
                leaves += l;
                max_child = max_child.max(d);
            }
            (leaves, max_child + 1)
        }
    }
}

/// Every leaf of the proof tree must be `Asserted` or `Cycle` — a derived
/// node must always have its premises expanded down to base facts.
fn leaves_all_asserted(t: &StringProofTree) -> bool {
    match t {
        StringProofTree::Asserted(_) | StringProofTree::Cycle(_) => true,
        StringProofTree::Derived { premises, .. } => premises.iter().all(leaves_all_asserted),
    }
}

#[test]
fn nf4_depth_chain_proof_is_correct_and_fast() {
    // Build a subClassOf chain c0 ⊑ c1 ⊑ ... ⊑ c10 plus `x rdf:type c0`.
    // Materialization derives `x rdf:type c10` via the scm-sco closure
    // (c0 ⊑ c10) followed by cax-sco (x a c0 ∧ c0 ⊑ c10 ⇒ x a c10).
    const N: usize = 11; // c0 .. c10
    let mut data = Dataset::new();
    for i in 0..(N - 1) {
        data.insert(&nq(
            &format!("http://ex/c{i}"),
            RDFS_SUB_CLASS_OF,
            &format!("http://ex/c{}", i + 1),
        ));
    }
    data.insert(&nq("http://ex/x", RDF_TYPE, "http://ex/c0"));

    let mut e = Engine::new();
    e.load(&data).unwrap();

    let start = std::time::Instant::now();
    let proof = e.proof("http://ex/x", RDF_TYPE, "http://ex/c10");
    let elapsed = start.elapsed();

    let proof = proof.expect("x rdf:type c10 should be derived and have a proof");

    let (leaves, depth) = leaf_count_and_depth(&proof);

    assert!(
        depth >= 2,
        "deep derivation should yield a multi-step proof (depth >= 2), got depth {depth}: {proof:?}"
    );
    assert!(leaves >= 1, "proof tree should have at least one leaf");
    assert!(
        leaves_all_asserted(&proof),
        "every leaf of the proof tree must be Asserted/Cycle: {proof:?}"
    );
    assert!(
        elapsed.as_millis() < 100,
        "building the proof tree must be well under the NF4 100 ms budget, took {elapsed:?}"
    );
}
