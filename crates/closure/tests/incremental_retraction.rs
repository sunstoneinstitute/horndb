//! Differential test for incremental transitive closure **retraction**
//! (SPEC-05 F6, deletion half).
//!
//! For random sequences of insert/delete operations over a small node space,
//! the incrementally maintained closure must, after **every** op, equal the
//! from-scratch GraphBLAS closure (`transitive_closure`) of the current base
//! edge set. This is the SPEC-05 "no missing, no spurious" differential applied
//! to the retraction path, mirroring `tests/incremental.rs`.

use std::collections::BTreeSet;

use proptest::prelude::*;

use horndb_closure::closure::incremental::IncrementalTransitiveClosure;
use horndb_closure::closure::transitive::transitive_closure;
use horndb_closure::grb::{init_once, BoolMatrix};

/// Oracle: the bulk GraphBLAS transitive closure of `base`, as a sorted set.
fn grb_closure(n: u64, base: &BTreeSet<(u64, u64)>) -> BTreeSet<(u64, u64)> {
    if base.is_empty() {
        return BTreeSet::new();
    }
    let edges: Vec<(u64, u64)> = base.iter().copied().collect();
    let m = BoolMatrix::from_edges(n, &edges).unwrap();
    let star = transitive_closure(&m).unwrap();
    star.extract_edges().unwrap().into_iter().collect()
}

/// One operation in the random sequence.
#[derive(Debug, Clone, Copy)]
enum Op {
    Insert(u64, u64),
    Delete(u64, u64),
}

fn op_strategy(n: u64) -> impl Strategy<Value = Op> {
    (0..n, 0..n, any::<bool>()).prop_map(|(s, o, is_insert)| {
        if is_insert {
            Op::Insert(s, o)
        } else {
            Op::Delete(s, o)
        }
    })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 400, ..ProptestConfig::default() })]

    /// After every insert/delete, the incremental closure equals the bulk
    /// GraphBLAS closure of the current base edge set.
    #[test]
    fn insert_delete_sequence_matches_grb_closure(
        ops in {
            let n = 6u64;
            prop::collection::vec(op_strategy(n), 1..40)
        },
    ) {
        init_once().unwrap();
        let n = 6u64;

        let mut inc = IncrementalTransitiveClosure::new();
        // Reference base set maintained by the test.
        let mut base: BTreeSet<(u64, u64)> = BTreeSet::new();

        for op in ops {
            match op {
                Op::Insert(s, o) => {
                    inc.insert_edge(s, o);
                    base.insert((s, o));
                }
                Op::Delete(s, o) => {
                    inc.delete_edge(s, o);
                    base.remove(&(s, o));
                }
            }

            let got: BTreeSet<(u64, u64)> = inc.edges().into_iter().collect();
            // The incremental structure must also faithfully track the base set.
            let got_base: BTreeSet<(u64, u64)> = inc.base_edges().into_iter().collect();
            prop_assert_eq!(
                &got_base,
                &base,
                "base set drift after {:?}",
                op
            );

            let reference = grb_closure(n, &base);
            prop_assert_eq!(
                &got,
                &reference,
                "closure mismatch after {:?}\nbase={:?}\nonly in incremental: {:?}\nonly in reference: {:?}",
                op,
                base,
                got.difference(&reference).collect::<Vec<_>>(),
                reference.difference(&got).collect::<Vec<_>>(),
            );
        }
    }
}
