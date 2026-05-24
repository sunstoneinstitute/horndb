use horndb_closure::closure::schema::reflexive_transitive_closure;
use horndb_closure::grb::{init_once, BoolMatrix};

#[test]
fn sco_chain_includes_reflexivity_over_extent() {
    init_once().unwrap();
    // 3 classes in a chain: A <- B <- C (subClassOf edges B->A, C->B).
    let m = BoolMatrix::from_edges(3, &[(1, 0), (2, 1)]).unwrap();
    let rtc = reflexive_transitive_closure(&m).unwrap();
    let edges = rtc.extract_edges().unwrap();
    // Strict closure: (1,0), (2,0), (2,1). Plus reflexive (0,0),(1,1),(2,2).
    let mut expected: Vec<(u64, u64)> = vec![(0, 0), (1, 0), (1, 1), (2, 0), (2, 1), (2, 2)];
    expected.sort();
    assert_eq!(edges, expected);
}

#[test]
fn empty_input_yields_only_diagonal() {
    init_once().unwrap();
    let m = BoolMatrix::from_edges(4, &[]).unwrap();
    let rtc = reflexive_transitive_closure(&m).unwrap();
    assert_eq!(rtc.nvals().unwrap(), 4);
}
