use horndb_wcoj::ids::{Ordering, Triple};
use horndb_wcoj::source::vec_source::VecTripleSource;
use horndb_wcoj::source::TripleSource;

#[test]
fn vec_source_seeks_within_spo_ordering() {
    let triples = vec![
        Triple::new(1, 10, 100),
        Triple::new(1, 10, 200),
        Triple::new(1, 20, 100),
        Triple::new(2, 10, 100),
    ];
    let src = VecTripleSource::from_triples(triples);

    let mut it = src.iter(Ordering::Spo).expect("SPO supported");

    // Level-0 (subject) iteration.
    assert_eq!(it.peek(0), Some(1));
    it.seek(0, 2);
    assert_eq!(it.peek(0), Some(2));
    it.seek(0, 3);
    assert_eq!(it.peek(0), None);
}

#[test]
fn vec_source_descends_levels() {
    let triples = vec![
        Triple::new(1, 10, 100),
        Triple::new(1, 10, 200),
        Triple::new(1, 20, 100),
    ];
    let src = VecTripleSource::from_triples(triples);

    let mut it = src.iter(Ordering::Spo).unwrap();
    it.seek(0, 1);
    it.open_level(1);
    assert_eq!(it.peek(1), Some(10));
    it.open_level(2);
    assert_eq!(it.peek(2), Some(100));
    it.seek(2, 150);
    assert_eq!(it.peek(2), Some(200));
}

#[test]
fn vec_source_reports_total_count() {
    let triples = vec![Triple::new(1, 10, 100), Triple::new(2, 10, 200)];
    let src = VecTripleSource::from_triples(triples);
    assert_eq!(src.total_triples(), 2);
}
