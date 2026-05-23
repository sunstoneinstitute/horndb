use reasoner_closure::sameas::EquivClasses;
use reasoner_closure::types::DictId;

#[test]
fn singletons_are_their_own_representatives() {
    let mut ec = EquivClasses::new();
    ec.insert(DictId(1));
    ec.insert(DictId(2));
    assert_eq!(ec.canonical(DictId(1)), Some(DictId(1)));
    assert_eq!(ec.canonical(DictId(2)), Some(DictId(2)));
    assert!(!ec.same(DictId(1), DictId(2)));
}

#[test]
fn union_merges_classes_and_picks_min_canonical() {
    let mut ec = EquivClasses::new();
    ec.union(DictId(7), DictId(3));
    ec.union(DictId(3), DictId(5));
    // Canonical of {3,5,7} is min = 3.
    assert_eq!(ec.canonical(DictId(3)), Some(DictId(3)));
    assert_eq!(ec.canonical(DictId(5)), Some(DictId(3)));
    assert_eq!(ec.canonical(DictId(7)), Some(DictId(3)));
    assert!(ec.same(DictId(5), DictId(7)));
}

#[test]
fn unknown_id_returns_none() {
    let ec = EquivClasses::new();
    assert!(ec.canonical(DictId(999)).is_none());
}

#[test]
fn class_iter_lists_all_members() {
    let mut ec = EquivClasses::new();
    ec.union(DictId(10), DictId(20));
    ec.union(DictId(20), DictId(30));
    let mut members: Vec<DictId> = ec.class_members(DictId(20)).collect();
    members.sort();
    assert_eq!(members, vec![DictId(10), DictId(20), DictId(30)]);
}

#[test]
fn one_million_unions_yields_one_class() {
    // Stress: chain unions 0~1~2~...~999_999. Canonical of all is DictId(0).
    let mut ec = EquivClasses::new();
    for i in 0..1_000_000u64 {
        ec.union(DictId(i), DictId(i + 1));
    }
    assert_eq!(ec.canonical(DictId(999_999)), Some(DictId(0)));
    assert_eq!(ec.canonical(DictId(123_456)), Some(DictId(0)));
}
