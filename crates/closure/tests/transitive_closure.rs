use reasoner_closure::closure::transitive::transitive_closure;
use reasoner_closure::grb::{init_once, BoolMatrix};

#[test]
fn chain_of_five_produces_complete_upper_triangle() {
    init_once().unwrap();
    // Chain: 0 -> 1 -> 2 -> 3 -> 4. Closure should add (0,2),(0,3),(0,4),
    // (1,3),(1,4),(2,4) for a total of 10 directed edges.
    let m = BoolMatrix::from_edges(5, &[(0, 1), (1, 2), (2, 3), (3, 4)]).unwrap();
    let mstar = transitive_closure(&m).unwrap();
    let edges = mstar.extract_edges().unwrap();

    let expected: Vec<(u64, u64)> = (0..5)
        .flat_map(|i| ((i + 1)..5).map(move |j| (i, j)))
        .collect();
    assert_eq!(edges, expected);
    assert_eq!(mstar.nvals().unwrap(), 10);
}

#[test]
fn single_self_loop_is_fixed_point() {
    init_once().unwrap();
    let m = BoolMatrix::from_edges(1, &[(0, 0)]).unwrap();
    let mstar = transitive_closure(&m).unwrap();
    assert_eq!(mstar.extract_edges().unwrap(), vec![(0, 0)]);
}

#[test]
fn triangle_cycle_closes_to_full_3x3() {
    init_once().unwrap();
    // 0 -> 1 -> 2 -> 0. Closure: every pair reachable, including self-loops.
    let m = BoolMatrix::from_edges(3, &[(0, 1), (1, 2), (2, 0)]).unwrap();
    let mstar = transitive_closure(&m).unwrap();
    assert_eq!(mstar.nvals().unwrap(), 9);
}

#[test]
fn empty_matrix_is_empty_closure() {
    init_once().unwrap();
    let m = BoolMatrix::from_edges(4, &[]).unwrap();
    let mstar = transitive_closure(&m).unwrap();
    assert_eq!(mstar.nvals().unwrap(), 0);
}
