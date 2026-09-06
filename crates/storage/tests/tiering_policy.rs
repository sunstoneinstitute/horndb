//! SPEC-25 S5 — the access-statistics placement policy (`Store::rebalance`).
//!
//! What these pin: idle partitions go cold after N rounds, a cold partition
//! that is read comes back warm, hints can only keep or pull a partition warm,
//! an empty hint set is exactly the stats-only policy (the `ml.enabled = false`
//! contract), small partitions stay warm, and no round ever changes what a
//! query sees — only where the data sits.

use horndb_storage::{Ordering, PlacementHints, Store, TermId, TieringConfig, DEFAULT_GRAPH};
use oxrdf::{NamedNode, Term};
use std::collections::BTreeSet;

fn iri(s: impl AsRef<str>) -> Term {
    Term::NamedNode(NamedNode::new(s.as_ref()).unwrap())
}

fn p_id(store: &Store, p: &Term) -> TermId {
    store.dictionary().get(p).expect("predicate was interned")
}

/// `rows` triples on predicate `p`, subjects and objects derived from the row
/// index so every partition holds distinct terms.
fn load(store: &Store, p: &Term, tag: &str, rows: usize) {
    let triples: Vec<_> = (0..rows)
        .map(|i| {
            (
                iri(format!("http://ex/{tag}/s{i}")),
                p.clone(),
                iri(format!("http://ex/{tag}/o{i}")),
            )
        })
        .collect();
    store.insert_triples(&triples).expect("insert");
}

/// A rebalance config with test-sized thresholds. `min_rows` is small enough
/// that a handful of triples is a demotion candidate.
fn cfg(store: &Store, idle_rounds: u32, min_rows: usize) -> TieringConfig {
    TieringConfig {
        cold_dir: store.cold_dir().to_path_buf(),
        demote_after_idle_rounds: idle_rounds,
        min_rows,
    }
}

/// Where `p` lives right now. Does not count as a read — see
/// `TierSnapshot::is_cold`.
fn is_cold(store: &Store, p: TermId) -> bool {
    store
        .snapshot()
        .tier_arc()
        .is_cold(DEFAULT_GRAPH, p)
        .expect("partition exists")
}

/// Read `p` once, through the same entry point a query uses.
fn read(store: &Store, p: TermId) {
    let snap = store.snapshot();
    let cols = snap
        .tier_arc()
        .ordered_predicate_at(DEFAULT_GRAPH, p, Ordering::Spo)
        .expect("partition exists");
    assert!(!cols.is_empty());
}

#[test]
fn idle_partitions_demote_after_n_rounds() {
    let store = Store::in_memory();
    let (hot, cold) = (iri("http://ex/hot"), iri("http://ex/cold"));
    load(&store, &hot, "h", 40);
    load(&store, &cold, "c", 40);
    let (hot_id, cold_id) = (p_id(&store, &hot), p_id(&store, &cold));
    let cfg = cfg(&store, 2, 8);
    let hints = PlacementHints::default();

    for round in 0..2 {
        read(&store, hot_id);
        let report = store.rebalance(&cfg, &hints).expect("rebalance");
        // Round 0 only raises the idle count; the demote lands on round 1.
        assert_eq!(
            report.demoted,
            if round == 0 {
                vec![]
            } else {
                vec![(DEFAULT_GRAPH, cold_id)]
            }
        );
    }

    assert!(!is_cold(&store, hot_id), "the read partition stays warm");
    assert!(is_cold(&store, cold_id), "the unread partition goes cold");
}

#[test]
fn cold_partition_read_promotes_next_round() {
    let store = Store::in_memory();
    let p = iri("http://ex/p");
    load(&store, &p, "p", 40);
    let id = p_id(&store, &p);
    let cfg = cfg(&store, 1, 8);
    let hints = PlacementHints::default();

    store.rebalance(&cfg, &hints).expect("rebalance");
    assert!(is_cold(&store, id), "one idle round is enough at n = 1");

    read(&store, id);
    let report = store.rebalance(&cfg, &hints).expect("rebalance");
    assert_eq!(report.promoted, vec![(DEFAULT_GRAPH, id)]);
    assert!(report.demoted.is_empty());
    assert!(!is_cold(&store, id));
}

#[test]
fn keep_warm_hint_vetoes_demotion_and_pulls_cold_warm() {
    let store = Store::in_memory();
    let (kept, dropped) = (iri("http://ex/kept"), iri("http://ex/dropped"));
    load(&store, &kept, "k", 40);
    load(&store, &dropped, "d", 40);
    let (kept_id, dropped_id) = (p_id(&store, &kept), p_id(&store, &dropped));
    let cfg = cfg(&store, 1, 8);

    let mut hints = PlacementHints::default();
    hints.keep_warm.insert((DEFAULT_GRAPH, kept_id));

    // Neither is read, so the stats alone would demote both. The hint vetoes
    // one of them.
    let report = store.rebalance(&cfg, &hints).expect("rebalance");
    assert_eq!(report.demoted, vec![(DEFAULT_GRAPH, dropped_id)]);
    assert!(!is_cold(&store, kept_id));
    assert!(is_cold(&store, dropped_id));

    // Now hint the cold one: it comes back warm without ever being read.
    hints.keep_warm.insert((DEFAULT_GRAPH, dropped_id));
    let report = store.rebalance(&cfg, &hints).expect("rebalance");
    assert_eq!(report.promoted, vec![(DEFAULT_GRAPH, dropped_id)]);
    assert!(!is_cold(&store, dropped_id));
}

/// One round's outcome: the report, plus where each of the two predicates
/// sits afterwards.
type RoundTrace = Vec<(horndb_storage::RebalanceReport, Vec<bool>)>;

/// Load a fresh store with two predicates, then run `rounds` rebalance rounds
/// against a fixed access sequence. `make_hints` builds the hint set from the
/// two predicate ids, so a caller can name a real partition without guessing
/// what the dictionary assigned it.
fn run_rounds(make_hints: impl Fn(TermId, TermId) -> PlacementHints, rounds: usize) -> RoundTrace {
    let store = Store::in_memory();
    let (a, b) = (iri("http://ex/a"), iri("http://ex/b"));
    load(&store, &a, "a", 40);
    load(&store, &b, "b", 40);
    let (a_id, b_id) = (p_id(&store, &a), p_id(&store, &b));
    let cfg = cfg(&store, 2, 8);
    let hints = make_hints(a_id, b_id);

    (0..rounds)
        .map(|round| {
            // A fixed, asymmetric access sequence: `a` is read on even rounds
            // only, `b` never.
            if round % 2 == 0 {
                read(&store, a_id);
            }
            let report = store.rebalance(&cfg, &hints).expect("rebalance");
            // Reported as (graph, predicate) pairs, which differ between the
            // two stores only if placement differs — the ids are assigned in
            // the same interning order by the identical load.
            (report, vec![is_cold(&store, a_id), is_cold(&store, b_id)])
        })
        .collect()
}

#[test]
fn empty_hints_are_bit_identical_to_stats_only() {
    // `PlacementHints::default()` vs. an explicitly-constructed empty set:
    // same reports and same placement, every round.
    let default_hints = run_rounds(|_, _| PlacementHints::default(), 5);
    let explicit_empty = run_rounds(
        |_, _| PlacementHints {
            keep_warm: BTreeSet::new(),
        },
        5,
    );
    assert_eq!(default_hints, explicit_empty);

    // The control that makes the assertion above non-vacuous: a *non-empty*
    // hint set over the same load and the same access sequence must produce a
    // different trace. Without it, "empty hints change nothing" would also
    // hold for a build that ignored hints outright.
    let hinted = run_rounds(
        |_, b_id| PlacementHints {
            keep_warm: BTreeSet::from([(DEFAULT_GRAPH, b_id)]),
        },
        5,
    );
    assert_ne!(
        default_hints, hinted,
        "hints must be load-bearing, or the equality above proves nothing"
    );
}

#[test]
fn rebalance_never_changes_query_results() {
    let store = Store::in_memory();
    let (a, b) = (iri("http://ex/a"), iri("http://ex/b"));
    load(&store, &a, "a", 40);
    load(&store, &b, "b", 40);
    let (a_id, b_id) = (p_id(&store, &a), p_id(&store, &b));
    let cfg = cfg(&store, 1, 8);
    let hints = PlacementHints::default();

    // The expectation comes from the data that was loaded, not from a read of
    // the store under test.
    let dict = store.dictionary();
    let expected: BTreeSet<(String, String, String)> = ["a", "b"]
        .iter()
        .flat_map(|tag| {
            let p = iri(format!("http://ex/{tag}")).to_string();
            (0..40).map(move |i| {
                (
                    iri(format!("http://ex/{tag}/s{i}")).to_string(),
                    p.clone(),
                    iri(format!("http://ex/{tag}/o{i}")).to_string(),
                )
            })
        })
        .collect();
    let expected_rows_per_predicate = 40usize;

    for round in 0..5 {
        // Force real tier churn: demote everything on the even rounds, and let
        // the policy promote it back on the odd ones (the verification reads
        // below are what the policy sees as access).
        if round % 2 == 0 {
            store.demote_all().expect("demote_all");
        }
        store.rebalance(&cfg, &hints).expect("rebalance");

        let actual: BTreeSet<(String, String, String)> = store
            .scan_all_term_ids()
            .into_iter()
            .map(|(s, p, o)| {
                (
                    dict.lookup(s).unwrap().to_string(),
                    dict.lookup(p).unwrap().to_string(),
                    dict.lookup(o).unwrap().to_string(),
                )
            })
            .collect();
        assert_eq!(actual, expected, "round {round}: whole-store scan changed");

        let snap = store.snapshot();
        for id in [a_id, b_id] {
            for &ord in Ordering::ALL.iter() {
                let cols = snap
                    .tier_arc()
                    .ordered_predicate_at(DEFAULT_GRAPH, id, ord)
                    .expect("partition exists");
                let rows: BTreeSet<(u64, u64)> = cols.scan().map(|(x, y)| (x.0, y.0)).collect();
                assert_eq!(
                    rows.len(),
                    expected_rows_per_predicate,
                    "round {round}, ordering {ord:?}: row count changed"
                );
            }
        }
    }
}

#[test]
fn min_rows_keeps_small_partitions_warm() {
    let store = Store::in_memory();
    let (big, small) = (iri("http://ex/big"), iri("http://ex/small"));
    load(&store, &big, "big", 40);
    load(&store, &small, "small", 3);
    let (big_id, small_id) = (p_id(&store, &big), p_id(&store, &small));
    let cfg = cfg(&store, 1, 8);
    let hints = PlacementHints::default();

    // Neither is read; only the one at or above `min_rows` is worth a file.
    let report = store.rebalance(&cfg, &hints).expect("rebalance");
    assert_eq!(report.demoted, vec![(DEFAULT_GRAPH, big_id)]);
    assert!(is_cold(&store, big_id));
    assert!(!is_cold(&store, small_id));
}
