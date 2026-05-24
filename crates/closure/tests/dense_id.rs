use reasoner_closure::dense_id::DenseIdMap;
use reasoner_closure::types::{DenseIdx, DictId};

#[test]
fn renumbers_in_first_seen_order() {
    let mut m = DenseIdMap::new();
    assert_eq!(m.intern(DictId(42)), DenseIdx(0));
    assert_eq!(m.intern(DictId(7)), DenseIdx(1));
    assert_eq!(m.intern(DictId(42)), DenseIdx(0));
    assert_eq!(m.len(), 2);
}

#[test]
fn round_trips_dict_to_dense_and_back() {
    let mut m = DenseIdMap::new();
    m.intern(DictId(100));
    m.intern(DictId(200));
    m.intern(DictId(300));
    assert_eq!(m.to_dict(DenseIdx(0)), Some(DictId(100)));
    assert_eq!(m.to_dict(DenseIdx(2)), Some(DictId(300)));
    assert_eq!(m.to_dict(DenseIdx(99)), None);
    assert_eq!(m.to_dense(DictId(200)), Some(DenseIdx(1)));
    assert_eq!(m.to_dense(DictId(404)), None);
}

#[test]
fn bulk_intern_pairs_returns_dense_edges() {
    let mut m = DenseIdMap::new();
    let edges = m.intern_edges(&[(DictId(10), DictId(20)), (DictId(20), DictId(30))]);
    // 10 -> 0, 20 -> 1, 30 -> 2 (first-seen order).
    assert_eq!(edges, vec![(0u64, 1u64), (1u64, 2u64)]);
    assert_eq!(m.len(), 3);
}
