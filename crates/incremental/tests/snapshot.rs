//! SPEC-24 S6 — reader snapshots backed by storage MVCC (ADR-0018: the storage
//! commit version is the one logical clock).

use std::collections::BTreeSet;
use std::sync::Arc;

use horndb_incremental::{Circuit, Snapshot, TransitiveClosureRule, TripleId};
use horndb_storage::{GraphId, Store, TermId, DEFAULT_GRAPH};
use oxrdf::{NamedNode, Term};

const DERIVED_GRAPH: &str = "https://horndb.io/graph/test-derived";

/// A circuit plus the store its snapshots read, wired the way the SPEC-24 S4
/// engine wires them: asserted rows in the default graph, derived rows in a
/// derived graph, and **one storage commit per tick** (ADR-0018).
struct Wired {
    circuit: Circuit,
    store: Arc<Store>,
    derived: GraphId,
    /// What the last commit put in the store, so a tick can commit a diff.
    committed: BTreeSet<(GraphId, TripleId)>,
}

impl Wired {
    fn new() -> Self {
        let store = Arc::new(Store::in_memory());
        let derived = store
            .intern_graph_uri(&Term::NamedNode(NamedNode::new_unchecked(DERIVED_GRAPH)))
            .expect("intern derived graph");
        let mut circuit = Circuit::new();
        circuit.attach_store(Arc::clone(&store), vec![DEFAULT_GRAPH, derived]);
        Self {
            circuit,
            store,
            derived,
            committed: BTreeSet::new(),
        }
    }

    /// The term id `http://ex/{n}` interns to. Test triples are built from
    /// these so the circuit's `TripleId`s are the store's own ids.
    fn id(&self, n: u64) -> u64 {
        self.store
            .dictionary()
            .intern(&Term::NamedNode(NamedNode::new_unchecked(format!(
                "http://ex/{n}"
            ))))
            .expect("intern term")
            .0
    }

    fn t(&self, s: u64, p: u64, o: u64) -> TripleId {
        (self.id(s), self.id(p), self.id(o))
    }

    /// Tick, then commit the circuit's whole `(asserted ∪ derived)` presence
    /// set to storage as one batch.
    fn commit(&mut self) {
        self.circuit.tick();
        let mut want: BTreeSet<(GraphId, TripleId)> = BTreeSet::new();
        for (t, m) in self.circuit.asserted_base().iter() {
            if m > 0 {
                want.insert((DEFAULT_GRAPH, *t));
            }
        }
        for (t, m) in self.circuit.derived_base().iter() {
            if m > 0 {
                want.insert((self.derived, *t));
            }
        }
        let dict = self.store.dictionary();
        let quad = |(g, t): &(GraphId, TripleId)| {
            dict.quad_from_ids(*g, TermId(t.0), TermId(t.1), TermId(t.2))
        };
        let dels: Vec<_> = self.committed.difference(&want).map(quad).collect();
        let adds: Vec<_> = want.difference(&self.committed).map(quad).collect();
        if !dels.is_empty() || !adds.is_empty() {
            self.store
                .apply_quad_ids(&dels, &adds)
                .expect("commit tick");
        }
        self.committed = want;
    }

    fn snapshot(&self) -> Snapshot {
        self.circuit.snapshot().expect("store attached")
    }
}

#[test]
fn circuit_without_a_store_has_no_snapshot() {
    assert!(
        Circuit::new().snapshot().is_none(),
        "an in-memory-only circuit has no reader view"
    );
}

#[test]
fn empty_store_snapshot_is_empty_at_version_zero() {
    let w = Wired::new();
    let snap = w.snapshot();
    assert!(snap.is_empty());
    assert_eq!(snap.len(), 0);
    assert_eq!(snap.logical_time(), 0, "version 0 is the empty store");
}

#[test]
fn snapshot_sees_asserted_rows_after_tick() {
    let mut w = Wired::new();
    let (a, b) = (w.t(1, 100, 2), w.t(2, 100, 3));
    w.circuit.assert_triple(a);
    w.circuit.assert_triple(b);
    w.commit();

    let snap = w.snapshot();
    assert_eq!(snap.len(), 2);
    assert!(snap.contains(&a));
    assert!(snap.contains(&b));
    assert!(!snap.contains(&w.t(9, 100, 9)));
}

#[test]
fn snapshot_is_pinned_across_a_later_tick() {
    let mut w = Wired::new();
    let a = w.t(1, 100, 2);
    w.circuit.assert_triple(a);
    w.commit();

    let snap = w.snapshot();
    assert_eq!(snap.len(), 1);
    let pinned_at = snap.logical_time();

    // A later tick adds a new triple. The pinned snapshot must NOT see it.
    let b = w.t(3, 100, 4);
    w.circuit.assert_triple(b);
    w.commit();

    assert_eq!(snap.len(), 1, "snapshot must stay pinned across the tick");
    assert!(snap.contains(&a));
    assert!(!snap.contains(&b));
    assert_eq!(snap.logical_time(), pinned_at, "as-of token is fixed");

    // A fresh snapshot does see both, at a higher commit version.
    let fresh = w.snapshot();
    assert_eq!(fresh.len(), 2);
    assert!(fresh.contains(&b));
    assert!(fresh.logical_time() > pinned_at);
}

/// A retraction is invisible to a snapshot pinned before it — the per-tuple
/// `end` stamp, not a rebuilt presence set, is what hides the row.
#[test]
fn retraction_is_invisible_to_an_earlier_pin() {
    let mut w = Wired::new();
    let a = w.t(1, 100, 2);
    w.circuit.assert_triple(a);
    w.commit();
    let before = w.snapshot();

    w.circuit.retract_triple(a);
    w.commit();
    let after = w.snapshot();

    assert!(before.contains(&a), "still visible at the older version");
    assert_eq!(before.len(), 1);
    assert!(!after.contains(&a), "hidden at the newer version");
    assert!(after.is_empty());
}

#[test]
fn overlapping_snapshots_stay_independent() {
    let mut w = Wired::new();
    let a = w.t(1, 100, 2);
    w.circuit.assert_triple(a);
    w.commit();
    let s1 = w.snapshot();

    let b = w.t(2, 100, 3);
    w.circuit.assert_triple(b);
    w.commit();
    let s2 = w.snapshot();

    let c = w.t(3, 100, 4);
    w.circuit.assert_triple(c);
    w.commit();
    let s3 = w.snapshot();

    assert_eq!(s1.len(), 1, "s1 pinned at 1 triple");
    assert_eq!(s2.len(), 2, "s2 pinned at 2 triples");
    assert_eq!(s3.len(), 3, "s3 sees all 3");

    // One clock: each committed tick is a higher storage commit version.
    assert!(s1.logical_time() < s2.logical_time());
    assert!(s2.logical_time() < s3.logical_time());
}

#[test]
fn snapshot_includes_and_pins_derived_rows() {
    let mut w = Wired::new();
    let p = w.id(100);
    w.circuit
        .add_closure_plan(Box::new(TransitiveClosureRule::new(p)));

    // 1->2, 2->3 ⇒ transitive closure derives 1->3.
    let (a, b) = (w.t(1, 100, 2), w.t(2, 100, 3));
    w.circuit.assert_triple(a);
    w.circuit.assert_triple(b);
    w.commit();

    let snap = w.snapshot();
    assert!(snap.contains(&a), "asserted edge");
    assert!(snap.contains(&b), "asserted edge");
    assert!(
        snap.contains(&w.t(1, 100, 3)),
        "derived transitive edge in snapshot"
    );
    let pinned_len = snap.len();

    // Extend the chain; the new derived edges must not leak into the pin.
    w.circuit.assert_triple(w.t(3, 100, 4));
    w.commit();

    assert_eq!(snap.len(), pinned_len, "derived rows stay pinned");
    assert!(
        !snap.contains(&w.t(1, 100, 4)),
        "new derived edge absent from old snap"
    );
    assert!(
        w.snapshot().contains(&w.t(1, 100, 4)),
        "fresh snapshot sees new derived edge"
    );
}

/// The gate's concurrency case: a reader holding a pin polls it while a writer
/// thread drives ticks that commit new versions. The pinned view must not move,
/// and neither side may block the other.
#[test]
fn pinned_reads_are_stable_under_concurrent_ticks() {
    let mut w = Wired::new();
    for i in 0..20u64 {
        let t = w.t(i, 100, i + 1);
        w.circuit.assert_triple(t);
    }
    w.commit();

    let snap = w.snapshot();
    let baseline_len = snap.len();
    let baseline_time = snap.logical_time();
    let probe = w.t(0, 100, 1);
    let absent = w.t(500, 100, 501);

    let reader = std::thread::spawn(move || {
        let mut stable = true;
        for _ in 0..500 {
            stable &= snap.len() == baseline_len
                && snap.logical_time() == baseline_time
                && snap.contains(&probe)
                && !snap.contains(&absent);
        }
        stable
    });

    // Writer keeps committing while the reader polls — must not block.
    for i in 100..300u64 {
        let t = w.t(i, 100, i + 1);
        w.circuit.assert_triple(t);
        w.commit();
    }

    assert!(
        reader.join().expect("reader thread"),
        "pinned snapshot must stay constant under concurrent ticks"
    );
    // The writer made progress concurrently, on the same clock.
    let fresh = w.snapshot();
    assert!(fresh.len() > baseline_len);
    assert!(fresh.logical_time() > baseline_time);
}

/// A triple present both as an asserted row (default graph) and as a derived
/// row (derived graph) is one member of the set, not two.
#[test]
fn snapshot_is_a_presence_set_not_a_multiset() {
    let mut w = Wired::new();
    let p = w.id(100);
    w.circuit
        .add_closure_plan(Box::new(TransitiveClosureRule::new(p)));

    // Derive (1,P,3) via the transitive closure of 1->2->3.
    w.circuit.assert_triple(w.t(1, 100, 2));
    w.circuit.assert_triple(w.t(2, 100, 3));
    w.commit();
    let derived = w.t(1, 100, 3);
    assert!(w.snapshot().contains(&derived), "derived");

    // The user now also asserts the already-derived triple, and double-asserts
    // a fresh one. Neither may make a triple appear more than once.
    let dup = w.t(9, 100, 9);
    w.circuit.assert_triple(derived);
    w.circuit.assert_triple(dup);
    w.circuit.assert_triple(dup);
    w.commit();

    let snap = w.snapshot();
    assert!(snap.contains(&derived), "asserted∩derived present");
    assert!(snap.contains(&dup), "double-asserted present");
    assert_eq!(
        snap.iter().filter(|t| *t == derived).count(),
        1,
        "asserted∩derived appears exactly once"
    );
    assert_eq!(
        snap.iter().filter(|t| *t == dup).count(),
        1,
        "double-asserted appears exactly once"
    );

    let triples: Vec<TripleId> = snap.iter().collect();
    let mut deduped = triples.clone();
    deduped.sort_unstable();
    deduped.dedup();
    assert_eq!(deduped.len(), triples.len(), "set view has no duplicates");
    assert_eq!(triples.len(), snap.len(), "len() agrees with iter()");
}

/// `iter()` is key-ordered: predicate ascending, then `(subject, object)`.
#[test]
fn iter_is_key_ordered() {
    let mut w = Wired::new();
    for (s, p, o) in [(3u64, 101u64, 1u64), (1, 100, 5), (2, 100, 2), (1, 101, 9)] {
        let t = w.t(s, p, o);
        w.circuit.assert_triple(t);
    }
    w.commit();

    let keys: Vec<(u64, u64, u64)> = w
        .snapshot()
        .iter()
        .map(|(s, p, o)| (p, s, o))
        .collect::<Vec<_>>();
    let mut sorted = keys.clone();
    sorted.sort_unstable();
    assert_eq!(keys, sorted, "predicate-major, then (subject, object)");
}

/// `logical_time()` is the storage commit version (ADR-0018), inclusive: the
/// rows a tick committed at version `v` are visible in a snapshot at `v`.
#[test]
fn logical_time_is_the_storage_commit_version() {
    let mut w = Wired::new();
    assert_eq!(w.snapshot().logical_time(), 0);

    let a = w.t(1, 100, 2);
    w.circuit.assert_triple(a);
    w.commit();

    let snap = w.snapshot();
    assert_eq!(
        snap.logical_time(),
        w.store.snapshot().version(),
        "one clock: the circuit's as-of token IS the store's commit version"
    );
    assert_eq!(snap.logical_time(), 1, "the first commit is version 1");
    assert!(snap.contains(&a), "the row committed at v is visible at v");
}

/// A retraction of a triple that was never asserted drives the asserted Z-set
/// multiplicity negative; the presence view must not expose it as a ghost row.
#[test]
fn over_retracted_triple_is_not_present() {
    let mut w = Wired::new();
    let a = w.t(1, 100, 2);
    w.circuit.assert_triple(a);
    w.commit();

    let ghost = w.t(7, 100, 8);
    w.circuit.retract_triple(ghost);
    w.commit();

    let snap = w.snapshot();
    assert!(!snap.contains(&ghost), "over-retracted triple is absent");
    assert!(snap.contains(&a), "the real triple is still present");
    assert_eq!(snap.len(), 1, "no ghost row from the negative count");
}
