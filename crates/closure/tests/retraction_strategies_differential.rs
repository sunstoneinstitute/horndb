//! SPEC-24 S2 differential test: the output-sensitive support-counting deletion
//! and the recompute fallback produce byte-identical closed sets and withdrawn
//! sets after every op in a random insert/delete sequence, and both equal the
//! from-scratch GraphBLAS closure of the current base.

use std::collections::BTreeSet;

use proptest::prelude::*;

use horndb_closure::closure::incremental::{DeleteStrategy, IncrementalTransitiveClosure};
use horndb_closure::closure::transitive::transitive_closure;
use horndb_closure::grb::{init_once, BoolMatrix};

fn grb_closure(n: u64, base: &BTreeSet<(u64, u64)>) -> BTreeSet<(u64, u64)> {
    if base.is_empty() {
        return BTreeSet::new();
    }
    let edges: Vec<(u64, u64)> = base.iter().copied().collect();
    let m = BoolMatrix::from_edges(n, &edges).unwrap();
    transitive_closure(&m)
        .unwrap()
        .extract_edges()
        .unwrap()
        .into_iter()
        .collect()
}

#[derive(Debug, Clone, Copy)]
enum Op {
    Insert(u64, u64),
    Delete(u64, u64),
}

fn op_strategy(n: u64) -> impl Strategy<Value = Op> {
    (0..n, 0..n, any::<bool>()).prop_map(|(s, o, ins)| {
        if ins {
            Op::Insert(s, o)
        } else {
            Op::Delete(s, o)
        }
    })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 400, ..ProptestConfig::default() })]

    #[test]
    fn strategies_agree_and_match_grb(
        ops in {
            let n = 6u64;
            prop::collection::vec(op_strategy(n), 1..40)
        },
    ) {
        init_once().unwrap();
        let n = 6u64;

        let mut sc = IncrementalTransitiveClosure::new();
        sc.set_delete_strategy(DeleteStrategy::SupportCounting);
        let mut rc = IncrementalTransitiveClosure::new();
        rc.set_delete_strategy(DeleteStrategy::Recompute);
        let mut base: BTreeSet<(u64, u64)> = BTreeSet::new();

        for op in ops {
            match op {
                Op::Insert(s, o) => {
                    sc.insert_edge(s, o);
                    rc.insert_edge(s, o);
                    base.insert((s, o));
                }
                Op::Delete(s, o) => {
                    let a = sc.delete_edge(s, o);
                    let b = rc.delete_edge(s, o);
                    let mut aw = a.withdrawn.clone();
                    let mut bw = b.withdrawn.clone();
                    aw.sort_unstable();
                    bw.sort_unstable();
                    prop_assert_eq!(&aw, &bw, "withdrawn differs after {:?}", op);
                    let mut asv = a.survived.clone();
                    let mut bsv = b.survived.clone();
                    asv.sort_unstable();
                    bsv.sort_unstable();
                    prop_assert_eq!(&asv, &bsv, "survived differs after {:?}", op);
                    base.remove(&(s, o));
                }
            }

            let got_sc: BTreeSet<(u64, u64)> = sc.edges().into_iter().collect();
            let got_rc: BTreeSet<(u64, u64)> = rc.edges().into_iter().collect();
            prop_assert_eq!(&got_sc, &got_rc, "closed sets diverge after {:?}", op);
            let reference = grb_closure(n, &base);
            prop_assert_eq!(&got_sc, &reference, "support-counting != GRB after {:?}", op);
        }
    }
}
