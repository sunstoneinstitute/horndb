use horndb_wcoj::source::synthetic::SyntheticGraph;
use horndb_wcoj::source::TripleSource;

#[test]
fn synthetic_4_cycle_graph_has_expected_size() {
    // Graph of 1000 vertices, each with out-degree 4. Edge predicate = 10.
    let g = SyntheticGraph::cyclic(1000, 4, 10, 0xCAFE);
    assert_eq!(g.total_triples(), 4000);
}

#[test]
fn synthetic_graph_supports_all_orderings() {
    let g = SyntheticGraph::cyclic(100, 2, 10, 0xCAFE);
    for ord in horndb_wcoj::ids::Ordering::ALL {
        assert!(g.supports(ord));
    }
}
