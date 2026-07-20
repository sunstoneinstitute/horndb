//! SPEC-02 F4 — all six index orderings queryable per predicate.
//!
//! Within a predicate partition the predicate component is constant, so the six
//! global orderings collapse to two physical layouts: subject-major (Spo/Sop/
//! Pso) and object-major (Pos/Osp/Ops). These tests pin the ordering semantics,
//! lazy vs. eager (hot) materialisation, the footprint accounting, and the
//! store-level acceptance check ("top predicates queryable in all six
//! orderings").

use horndb_storage::ordering::PartitionAxis;
use horndb_storage::partition::PredicatePartition;
use horndb_storage::term::{TermId, TermKind, DEFAULT_GRAPH};
use horndb_storage::tier::Tier;
use horndb_storage::{MemoryTier, Ordering, Store};
use oxrdf::{NamedNode, Term};

fn id(payload: u64) -> TermId {
    TermId::new(TermKind::Uri, payload)
}

fn nn(s: &str) -> Term {
    Term::NamedNode(NamedNode::new(s).unwrap())
}

/// (subject, object) rows used across the partition-level tests.
const ROWS: &[(u64, u64)] = &[(1, 5), (1, 2), (3, 2), (2, 9), (3, 1)];

fn build_partition(hot_threshold: usize) -> PredicatePartition {
    let mut b = PredicatePartition::builder();
    for &(s, o) in ROWS {
        b.append(id(s), id(o));
    }
    b.build_with_hot_threshold(hot_threshold)
}

fn payloads(part: &PredicatePartition, ord: Ordering) -> Vec<(u64, u64)> {
    part.ordered(ord)
        .scan()
        .map(|(a, b)| (a.payload(), b.payload()))
        .collect()
}

#[test]
fn subject_major_orderings_sort_by_subject_then_object() {
    let part = build_partition(usize::MAX);
    let expected = vec![(1, 2), (1, 5), (2, 9), (3, 1), (3, 2)];
    for ord in [Ordering::Spo, Ordering::Sop, Ordering::Pso] {
        assert_eq!(
            part.ordered(ord).axis(),
            PartitionAxis::SubjectMajor,
            "{ord:?}"
        );
        assert_eq!(payloads(&part, ord), expected, "{ord:?}");
    }
}

#[test]
fn object_major_orderings_sort_by_object_then_subject() {
    let part = build_partition(usize::MAX);
    // (level0 = object, level1 = subject), sorted by (object, subject).
    let expected = vec![(1, 3), (2, 1), (2, 3), (5, 1), (9, 2)];
    for ord in [Ordering::Pos, Ordering::Osp, Ordering::Ops] {
        assert_eq!(
            part.ordered(ord).axis(),
            PartitionAxis::ObjectMajor,
            "{ord:?}"
        );
        assert_eq!(payloads(&part, ord), expected, "{ord:?}");
    }
}

#[test]
fn level0_is_sorted_in_every_ordering() {
    let part = build_partition(usize::MAX);
    for ord in Ordering::ALL {
        let cols = part.ordered(ord);
        let l0 = cols.level0();
        for i in 1..cols.len() {
            assert!(l0.value(i - 1) <= l0.value(i), "{ord:?} level0 not sorted");
        }
    }
}

#[test]
fn semantic_subject_object_multiset_is_ordering_invariant() {
    let part = build_partition(usize::MAX);
    let mut canonical: Vec<(u64, u64)> = ROWS.to_vec();
    canonical.sort_unstable();
    canonical.dedup();
    for ord in Ordering::ALL {
        let mut got: Vec<(u64, u64)> = part
            .ordered(ord)
            .subject_object()
            .map(|(s, o)| (s.payload(), o.payload()))
            .collect();
        got.sort_unstable();
        assert_eq!(got, canonical, "{ord:?} lost or reordered semantic rows");
    }
}

#[test]
fn cold_predicate_materializes_object_major_lazily() {
    let part = build_partition(usize::MAX); // cold: never eager
    assert!(!part.object_major_materialized());

    // Subject-major requests must NOT trigger materialisation.
    let _ = part.ordered(Ordering::Spo);
    assert!(!part.object_major_materialized());

    // First object-major request triggers (and caches) it.
    let _ = part.ordered(Ordering::Pos);
    assert!(part.object_major_materialized());
    // A second request reuses the cached layout (still materialised).
    let _ = part.ordered(Ordering::Osp);
    assert!(part.object_major_materialized());
}

#[test]
fn hot_predicate_materializes_object_major_eagerly() {
    let part = build_partition(1); // threshold 1 ⇒ any non-empty predicate is hot
    assert!(part.object_major_materialized());
}

#[test]
fn footprint_accounts_for_materialized_object_major() {
    let rows = {
        let mut r: Vec<(u64, u64)> = ROWS.to_vec();
        r.sort_unstable();
        r.dedup();
        r.len() as u64
    };

    // SPEC-25 S1: each row now also carries begin/end visibility stamps (16 B),
    // so the subject-major base is 32 B/row instead of 16 B/row: 16 B for
    // (s, o) + 16 B for (begin, end). The object-major layout, when
    // materialised, carries its own re-sorted (o, s) + (begin, end) columns —
    // another 32 B/row, not 16 — so a hot (or lazily-materialised) partition
    // is 64 B/row total.
    let cold = build_partition(usize::MAX);
    assert_eq!(cold.estimated_bytes(), rows * 32);

    let hot = build_partition(1);
    assert_eq!(hot.estimated_bytes(), rows * 64);

    // Lazy materialisation flips a cold partition's footprint.
    let _ = cold.ordered(Ordering::Pos);
    assert_eq!(cold.estimated_bytes(), rows * 64);
}

#[test]
fn tier_ordered_predicate_outlives_lock_and_differs_by_axis() {
    let tier = MemoryTier::with_hot_threshold(usize::MAX);
    let quads: Vec<_> = ROWS
        .iter()
        .map(|&(s, o)| (DEFAULT_GRAPH, id(s), id(100), id(o)))
        .collect();
    tier.insert_quad_batch(&quads).unwrap();

    // OrderedColumns owns Arc clones, so it escapes the read-lock.
    let spo = tier
        .ordered_predicate(DEFAULT_GRAPH, id(100), Ordering::Spo)
        .unwrap();
    let pos = tier
        .ordered_predicate(DEFAULT_GRAPH, id(100), Ordering::Pos)
        .unwrap();

    let spo_l0: Vec<u64> = (0..spo.len()).map(|i| spo.level0().value(i)).collect();
    let pos_l0: Vec<u64> = (0..pos.len()).map(|i| pos.level0().value(i)).collect();
    assert_eq!(spo_l0, vec![1, 1, 2, 3, 3]); // subjects
    assert_eq!(pos_l0, vec![1, 2, 2, 5, 9]); // objects

    let missing = tier.ordered_predicate(DEFAULT_GRAPH, id(999), Ordering::Spo);
    assert!(missing.is_none());
}

#[test]
fn store_scan_predicate_ordered_round_trips_terms_in_every_ordering() {
    let store = Store::in_memory();
    let p = nn("http://example.org/p");
    let rows = [
        (nn("http://example.org/a"), nn("http://example.org/y")),
        (nn("http://example.org/a"), nn("http://example.org/x")),
        (nn("http://example.org/b"), nn("http://example.org/x")),
    ];
    let triples: Vec<_> = rows
        .iter()
        .map(|(s, o)| (s.clone(), p.clone(), o.clone()))
        .collect();
    store.insert_triples(&triples).unwrap();

    let mut canonical: Vec<(String, String)> = rows
        .iter()
        .map(|(s, o)| (s.to_string(), o.to_string()))
        .collect();
    canonical.sort();

    for ord in Ordering::ALL {
        let got = store.scan_predicate_ordered(&p, ord).unwrap();
        assert_eq!(got.len(), rows.len(), "{ord:?}");
        // Predicate is preserved on every row.
        assert!(got.iter().all(|(_, pp, _)| *pp == p), "{ord:?}");
        let mut semantic: Vec<(String, String)> = got
            .iter()
            .map(|(s, _, o)| (s.to_string(), o.to_string()))
            .collect();
        semantic.sort();
        assert_eq!(semantic, canonical, "{ord:?} changed the triple set");
    }
}

#[test]
fn acceptance_top_predicates_queryable_in_all_six_orderings() {
    // Mirrors SPEC-02 acceptance #6 at small scale: the hottest predicates are
    // queryable in all six orderings. `p_hot` clears the threshold (eager),
    // `p_cold` does not (lazy).
    let store = Store::in_memory_with_hot_threshold(3);
    let p_hot = nn("http://example.org/hot");
    let p_cold = nn("http://example.org/cold");

    let mut triples = Vec::new();
    for i in 0..5u64 {
        let s = nn(&format!("http://example.org/s{i}"));
        let o = nn(&format!("http://example.org/o{i}"));
        triples.push((s, p_hot.clone(), o));
    }
    triples.push((
        nn("http://example.org/s0"),
        p_cold.clone(),
        nn("http://example.org/o0"),
    ));
    store.insert_triples(&triples).unwrap();

    let top = store.top_predicates(10).unwrap();
    // Hottest first.
    assert_eq!(top[0].0, p_hot);
    assert_eq!(top[0].1, 5);
    assert!(top.iter().any(|(p, c)| *p == p_cold && *c == 1));

    // Each top predicate is queryable in all six orderings, returning its full
    // row set every time.
    for (p, count) in &top {
        for ord in Ordering::ALL {
            let got = store.scan_predicate_ordered(p, ord).unwrap();
            assert_eq!(got.len() as u64, *count, "{p} / {ord:?}");
        }
    }
}
