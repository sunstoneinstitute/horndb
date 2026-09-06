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
    // same reports and same placement, every round. (`PlacementHints::default()`
    // and an explicit empty `BTreeSet` are the same value, so this alone
    // cannot catch a policy that mishandles the hint *sense* — the next
    // check below is what actually constrains that.)
    let default_hints = run_rounds(|_, _| PlacementHints::default(), 5);
    let explicit_empty = run_rounds(
        |_, _| PlacementHints {
            keep_warm: BTreeSet::new(),
        },
        5,
    );
    assert_eq!(default_hints, explicit_empty);

    // Hints only ever add (SPEC-25 S5): run the same access sequence with a
    // real hint on `b` and assert the difference only ever goes warm-ward —
    // per round, per predicate, the hinted run must never be cold where the
    // stats-only run is warm. This is what actually constrains the hint
    // *sense*: inverting it (making a hint cause demotion instead of
    // preventing it) would still pass `assert_eq!` above but fails this.
    let hinted = run_rounds(
        |_, b_id| PlacementHints {
            keep_warm: BTreeSet::from([(DEFAULT_GRAPH, b_id)]),
        },
        5,
    );
    for (round, ((_, stats_cold), (_, hint_cold))) in
        default_hints.iter().zip(hinted.iter()).enumerate()
    {
        for (idx, (&stats, &hint)) in stats_cold.iter().zip(hint_cold.iter()).enumerate() {
            assert!(
                !hint || stats,
                "round {round}, predicate {idx}: hinted run is cold where the \
                 stats-only run is warm — a hint must never cause a demotion"
            );
        }
    }

    // The control that makes the checks above non-vacuous: a *non-empty*
    // hint set over the same load and the same access sequence must produce a
    // different trace. Without it, "hints never make it colder" would also
    // hold for a build that ignored hints outright.
    assert_ne!(
        default_hints, hinted,
        "hints must be load-bearing, or the checks above prove nothing"
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
    let mut any_demoted = false;

    for round in 0..5 {
        // Force extra tier churn on top of `rebalance`'s own idle-based
        // demotion: demote everything on the later even rounds, then let the
        // policy promote back what gets read (the verification read of
        // `a_id` below is what the policy sees as access). Skipped on round 0
        // so the very first call sees `b_id` in its true post-load state —
        // warm and never yet read — and demotes it through `rebalance`'s own
        // logic rather than one this test forced ahead of it.
        if round != 0 && round % 2 == 0 {
            store.demote_all().expect("demote_all");
        }
        let report = store.rebalance(&cfg, &hints).expect("rebalance");
        any_demoted |= !report.demoted.is_empty();

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

        // Only `a_id` goes through the counted accessor here. Reading `b_id`
        // too would keep it permanently "read" from the policy's point of
        // view, so it would never go idle and `rebalance`'s own demote path
        // would never run — `b_id`'s content is still checked above, through
        // the uncounted whole-store scan.
        let snap = store.snapshot();
        for &ord in Ordering::ALL.iter() {
            let cols = snap
                .tier_arc()
                .ordered_predicate_at(DEFAULT_GRAPH, a_id, ord)
                .expect("partition exists");
            let rows: BTreeSet<(u64, u64)> = cols.scan().map(|(x, y)| (x.0, y.0)).collect();
            assert_eq!(
                rows.len(),
                expected_rows_per_predicate,
                "round {round}, ordering {ord:?}: row count changed"
            );
        }
    }

    assert!(
        any_demoted,
        "rebalance never exercised its own demote path — b_id never went idle"
    );
    // Silence an "unused" complaint if the assertion above is ever loosened.
    let _ = b_id;
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

/// `demote` refuses while a pin below the compaction horizon still needs a
/// dead row (`crates/storage/tests/cold_partition.rs` pins this at the
/// `demote` level). `rebalance` must not treat that refusal as a reason to
/// drop the idle count it has been building: it keeps retrying on later
/// rounds and succeeds once the row is reclaimable.
#[test]
fn rebalance_retries_after_demote_refuses() {
    let store = Store::in_memory();
    let p = iri("http://ex/p");
    load(&store, &p, "p", 40);
    let id = p_id(&store, &p);
    // Two idle rounds to demote, so the test can tell "the count survived a
    // refused round" from "the count was silently reset and rebuilt" — a
    // reset-on-refusal bug would need a *third* round to reach the threshold
    // again from zero.
    let cfg = cfg(&store, 2, 8);
    let hints = PlacementHints::default();

    // Pinned before a retraction, so the retracted row sits below the pin's
    // horizon: `demote`'s compaction pass cannot reclaim it, and `demote`
    // refuses rather than encode a file missing a row the pin still needs.
    let pin = store.pin();
    assert_eq!(
        store
            .retract_triples(&[(iri("http://ex/p/s0"), p.clone(), iri("http://ex/p/o0"))])
            .unwrap(),
        1
    );

    // Round 1: idle count reaches 1, below the threshold — no demote attempt.
    let report = store.rebalance(&cfg, &hints).expect("rebalance");
    assert!(report.demoted.is_empty());
    assert!(!is_cold(&store, id));

    // Round 2: idle count reaches 2, at the threshold — `demote` is attempted
    // and refused (the pin is still alive), so nothing moves.
    let report = store.rebalance(&cfg, &hints).expect("rebalance");
    assert!(
        report.demoted.is_empty(),
        "demote must be refused while the pin is alive"
    );
    assert!(!is_cold(&store, id));

    drop(pin);

    // Round 3: still unread, and the idle count was never reset by the
    // refusal — it is already at the threshold, so `demote` is retried
    // immediately (not after two more idle rounds) and now succeeds.
    let report = store.rebalance(&cfg, &hints).expect("rebalance");
    assert_eq!(report.demoted, vec![(DEFAULT_GRAPH, id)]);
    assert!(is_cold(&store, id));
}

/// F1 (HDB-179 review): `Store::scan_all_term_ids` walks every predicate in
/// the default graph through `iter_graph_term_ids` — the entry point the
/// default `VecTripleSource` rebuild uses on every SPARQL query. Before the
/// fix, that whole-store walk counted as a per-partition read, so `read ==
/// true` for every partition every round and `rebalance` never demoted
/// anything. This pins the fix on the real read path: repeated whole-store
/// sweeps must not stop an otherwise-idle partition from going cold.
#[test]
fn whole_store_sweep_does_not_block_demotion() {
    let store = Store::in_memory();
    let p = iri("http://ex/p");
    load(&store, &p, "p", 40);
    let id = p_id(&store, &p);
    let cfg = cfg(&store, 1, 8);
    let hints = PlacementHints::default();

    for _ in 0..3 {
        assert_eq!(store.scan_all_term_ids().len(), 40);
    }

    let report = store.rebalance(&cfg, &hints).expect("rebalance");
    assert_eq!(
        report.demoted,
        vec![(DEFAULT_GRAPH, id)],
        "a whole-store sweep must not keep an idle partition warm"
    );
    assert!(is_cold(&store, id));
}
