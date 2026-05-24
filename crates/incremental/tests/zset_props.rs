//! Property tests for `Zset<K>`. The DBSP correctness arguments lean on
//! the abelian-group structure: addition is commutative, associative, and
//! has inverses. We assert each.

use proptest::prelude::*;
use horndb_incremental::Zset;

fn arb_zset() -> impl Strategy<Value = Zset<i32>> {
    prop::collection::vec((0i32..50, -3i64..=3), 0..30).prop_map(Zset::from_iter)
}

proptest! {
    #[test]
    fn add_assign_is_commutative(a in arb_zset(), b in arb_zset()) {
        let mut x = a.clone(); x.add_assign(&b);
        let mut y = b.clone(); y.add_assign(&a);
        prop_assert_eq!(x, y);
    }

    #[test]
    fn add_assign_is_associative(a in arb_zset(), b in arb_zset(), c in arb_zset()) {
        let mut left = a.clone(); left.add_assign(&b); left.add_assign(&c);
        let mut bc = b.clone(); bc.add_assign(&c);
        let mut right = a.clone(); right.add_assign(&bc);
        prop_assert_eq!(left, right);
    }

    #[test]
    fn sub_assign_inverts_add_assign(a in arb_zset(), b in arb_zset()) {
        let mut x = a.clone();
        x.add_assign(&b);
        x.sub_assign(&b);
        prop_assert_eq!(x, a);
    }

    #[test]
    fn no_zero_multiplicity_rows_after_any_op(a in arb_zset(), b in arb_zset()) {
        let mut x = a.clone();
        x.add_assign(&b);
        for (_, m) in x.iter() {
            prop_assert!(m != 0, "zero rows must be pruned");
        }
    }
}
