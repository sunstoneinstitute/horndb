use horndb_storage::{PredicatePartition, TermId, TermKind};

fn uri(payload: u64) -> TermId {
    TermId::new(TermKind::Uri, payload)
}

#[test]
fn empty_partition() {
    let p = PredicatePartition::builder().build();
    assert!(p.is_empty());
    assert_eq!(p.len(), 0);
    assert_eq!(p.subject_set().len(), 0);
    assert_eq!(p.object_set().len(), 0);
}

#[test]
fn append_and_scan_in_spo_order() {
    let mut b = PredicatePartition::builder();
    b.append(uri(3), uri(7));
    b.append(uri(1), uri(9));
    b.append(uri(1), uri(2));
    b.append(uri(2), uri(5));
    let p = b.build();
    let pairs: Vec<_> = p.scan().collect();
    assert_eq!(
        pairs,
        vec![
            (uri(1), uri(2)),
            (uri(1), uri(9)),
            (uri(2), uri(5)),
            (uri(3), uri(7)),
        ]
    );
}

#[test]
fn subject_and_object_sets_are_distinct_payloads() {
    let mut b = PredicatePartition::builder();
    b.append(uri(1), uri(10));
    b.append(uri(1), uri(20));
    b.append(uri(2), uri(10));
    let p = b.build();
    let subjs: Vec<u64> = p.subject_set().iter().collect();
    let objs: Vec<u64> = p.object_set().iter().collect();
    assert_eq!(subjs, vec![1, 2]);
    assert_eq!(objs, vec![10, 20]);
}

#[test]
fn arrow_columns_share_length_with_triples() {
    let mut b = PredicatePartition::builder();
    for i in 0..100u64 {
        b.append(uri(i), uri(i + 1));
    }
    let p = b.build();
    assert_eq!(p.subjects().len(), 100);
    assert_eq!(p.objects().len(), 100);
    assert_eq!(p.len(), 100);
}
