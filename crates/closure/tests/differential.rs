//! Differential test: GraphBLAS closure vs naive Rust reference closure.
//!
//! Stand-in for SPEC-05 acceptance criterion 4 until SPEC-04 (rule engine)
//! exists to provide the canonical reference. The naive reference here is
//! Floyd–Warshall over a dense Boolean matrix — slow but obviously correct.

use std::collections::BTreeSet;

use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};

use reasoner_closure::closure::transitive::transitive_closure;
use reasoner_closure::grb::{init_once, BoolMatrix};

#[allow(clippy::needless_range_loop)]
fn naive_closure(n: usize, edges: &[(u64, u64)]) -> BTreeSet<(u64, u64)> {
    let mut reach = vec![vec![false; n]; n];
    for &(s, o) in edges {
        reach[s as usize][o as usize] = true;
    }
    // Floyd–Warshall over Booleans. Index-style loops are required here
    // because we read reach[i][k] and reach[k][j] while writing reach[i][j];
    // splitting borrows by iterator is more obfuscating than it is worth.
    for k in 0..n {
        for i in 0..n {
            if !reach[i][k] {
                continue;
            }
            for j in 0..n {
                if reach[k][j] {
                    reach[i][j] = true;
                }
            }
        }
    }
    let mut out = BTreeSet::new();
    for i in 0..n {
        for j in 0..n {
            if reach[i][j] {
                out.insert((i as u64, j as u64));
            }
        }
    }
    out
}

fn random_edges(n: usize, density_per_node: usize, seed: u64) -> Vec<(u64, u64)> {
    let mut rng = SmallRng::seed_from_u64(seed);
    let mut set: BTreeSet<(u64, u64)> = BTreeSet::new();
    for s in 0..n {
        for _ in 0..density_per_node {
            let o = rng.gen_range(0..n);
            set.insert((s as u64, o as u64));
        }
    }
    set.into_iter().collect()
}

#[test]
fn random_graphs_match_naive_closure() {
    init_once().unwrap();
    for (seed, n, density) in [(1u64, 10usize, 2usize), (2, 20, 3), (3, 50, 4), (4, 100, 2)] {
        let edges = random_edges(n, density, seed);
        let naive = naive_closure(n, &edges);

        let m = BoolMatrix::from_edges(n as u64, &edges).unwrap();
        let star = transitive_closure(&m).unwrap();
        let grb: BTreeSet<(u64, u64)> = star.extract_edges().unwrap().into_iter().collect();

        assert_eq!(
            grb,
            naive,
            "mismatch on seed={seed} n={n} density={density}\n\
             only in grb: {:?}\nonly in naive: {:?}",
            grb.difference(&naive).collect::<Vec<_>>(),
            naive.difference(&grb).collect::<Vec<_>>()
        );
    }
}
