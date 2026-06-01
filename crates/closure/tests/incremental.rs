//! Differential test for the incremental transitive closure (SPEC-05 F6).
//!
//! For many random graphs and random insertion orders, the incrementally
//! maintained closure must equal the from-scratch GraphBLAS closure
//! (`transitive_closure`). Two scenarios are covered:
//!   (a) from empty — insert every edge one at a time;
//!   (b) seeded — close a prefix on GraphBLAS, then insert the rest
//!       incrementally.

use std::collections::BTreeSet;

use rand::rngs::SmallRng;
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};

use horndb_closure::closure::incremental::IncrementalTransitiveClosure;
use horndb_closure::closure::transitive::transitive_closure;
use horndb_closure::grb::{init_once, BoolMatrix};

fn random_edges(n: usize, density_per_node: usize, rng: &mut SmallRng) -> Vec<(u64, u64)> {
    let mut set: BTreeSet<(u64, u64)> = BTreeSet::new();
    for s in 0..n {
        for _ in 0..density_per_node {
            let o = rng.gen_range(0..n);
            set.insert((s as u64, o as u64));
        }
    }
    set.into_iter().collect()
}

fn grb_closure(n: usize, edges: &[(u64, u64)]) -> BTreeSet<(u64, u64)> {
    if edges.is_empty() {
        return BTreeSet::new();
    }
    let m = BoolMatrix::from_edges(n as u64, edges).unwrap();
    let star = transitive_closure(&m).unwrap();
    star.extract_edges().unwrap().into_iter().collect()
}

#[test]
fn incremental_from_empty_matches_grb_closure() {
    init_once().unwrap();
    for (seed, n, density) in [(1u64, 8usize, 2usize), (2, 15, 3), (3, 30, 2), (4, 60, 3)] {
        let mut rng = SmallRng::seed_from_u64(seed);
        let mut edges = random_edges(n, density, &mut rng);
        let reference = grb_closure(n, &edges);

        // Insert in a shuffled order to exercise order-independence.
        edges.shuffle(&mut rng);
        let mut inc = IncrementalTransitiveClosure::new();
        inc.insert_edges(edges.iter().copied());
        let got: BTreeSet<(u64, u64)> = inc.edges().into_iter().collect();

        assert_eq!(
            got,
            reference,
            "from-empty mismatch seed={seed} n={n} density={density}\n\
             only in incremental: {:?}\nonly in reference: {:?}",
            got.difference(&reference).collect::<Vec<_>>(),
            reference.difference(&got).collect::<Vec<_>>()
        );
    }
}

#[test]
fn seeded_then_incremental_matches_grb_closure() {
    init_once().unwrap();
    for (seed, n, density) in [(11u64, 10usize, 2usize), (12, 20, 3), (13, 40, 2)] {
        let mut rng = SmallRng::seed_from_u64(seed);
        let edges = random_edges(n, density, &mut rng);
        if edges.len() < 4 {
            continue;
        }
        let split = edges.len() / 2;
        let (prefix, rest) = edges.split_at(split);

        // Seed the incremental structure from a real GraphBLAS closure of the
        // prefix, then insert the remaining edges incrementally.
        let seeded = grb_closure(n, prefix);
        let mut inc = IncrementalTransitiveClosure::from_closed_edges(seeded.iter().copied());
        inc.insert_edges(rest.iter().copied());
        let got: BTreeSet<(u64, u64)> = inc.edges().into_iter().collect();

        let reference = grb_closure(n, &edges);
        assert_eq!(
            got,
            reference,
            "seeded mismatch seed={seed} n={n} density={density}\n\
             only in incremental: {:?}\nonly in reference: {:?}",
            got.difference(&reference).collect::<Vec<_>>(),
            reference.difference(&got).collect::<Vec<_>>()
        );
    }
}
