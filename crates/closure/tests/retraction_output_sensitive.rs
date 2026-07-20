//! SPEC-24 acceptance #2: closure deletion is output-sensitive — a delete whose
//! closure delta and frontier are small inspects a bounded amount of work even
//! as the surrounding store grows. Uses the deterministic `last_delete_probes`
//! counter rather than wall-clock so the gate is CI-stable.

use horndb_closure::closure::incremental::IncrementalTransitiveClosure;

/// Build N independent 2-edge chains a_i -> b_i -> c_i (disjoint node ids), plus
/// one extra redundant edge on chain 0 that, when deleted, withdraws nothing.
/// Deleting that redundant edge must inspect O(1) pairs regardless of N.
fn probes_for_store(n_chains: u64) -> usize {
    let mut c = IncrementalTransitiveClosure::new();
    for i in 0..n_chains {
        let a = i * 10;
        let b = i * 10 + 1;
        let d = i * 10 + 2;
        c.insert_edge(a, b);
        c.insert_edge(b, d);
    }
    // Chain 0 gets a redundant direct edge 0 -> 2 (already implied by 0->1->2).
    c.insert_edge(0, 2);
    // Deleting the redundant (0,2): (0,2) stays closed via 0->1->2, withdraws
    // nothing. Frontier = closed-fwd[2] ∪ {2} within chain 0 only.
    c.delete_edge(0, 2);
    c.last_delete_probes()
}

#[test]
fn deletion_probes_are_independent_of_store_size() {
    let small = probes_for_store(4);
    let large = probes_for_store(4_000);
    // Output-sensitive: the redundant-edge delete inspects the same bounded set
    // of pairs whether there are 4 chains or 4000. Allow a tiny constant slack.
    assert!(
        large <= small + 2,
        "probes must not scale with store: small={small}, large={large}"
    );
}
