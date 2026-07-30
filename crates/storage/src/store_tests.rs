use super::*;
use oxrdf::NamedNode;

fn iri(s: &str) -> Term {
    Term::NamedNode(NamedNode::new(s).unwrap())
}

#[test]
fn scan_all_term_ids_returns_every_default_graph_triple() {
    let store = Store::in_memory();
    store
        .insert_triples(&[
            (iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b")),
            (iri("http://ex/a"), iri("http://ex/q"), iri("http://ex/c")),
        ])
        .unwrap();
    let all = store.scan_all_term_ids();
    assert_eq!(all.len(), 2);
    let p = store.dictionary().get(&iri("http://ex/p")).unwrap();
    let q = store.dictionary().get(&iri("http://ex/q")).unwrap();
    let preds: Vec<TermId> = all.iter().map(|t| t.1).collect();
    assert!(preds.contains(&p) && preds.contains(&q));
}

#[test]
fn scanning_absent_predicate_does_not_mutate_dictionary() {
    let store = Store::in_memory();
    store
        .insert_triples(&[(iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"))])
        .unwrap();
    let absent = iri("http://ex/never-interned");

    // A read of an absent predicate yields no rows and must NOT intern the
    // query term (a read transaction is non-mutating).
    let snap = store.snapshot();
    assert!(snap
        .scan_predicate(DEFAULT_GRAPH, &absent)
        .unwrap()
        .is_empty());
    assert!(snap
        .scan_predicate_ordered(&absent, Ordering::Spo)
        .unwrap()
        .is_empty());
    assert!(store
        .scan_predicate(DEFAULT_GRAPH, &absent)
        .unwrap()
        .is_empty());

    // The absent term was never added to the dictionary by those reads.
    assert!(store.dictionary().get(&absent).is_none());
}

#[test]
fn store_snapshot_is_stable_across_writes() {
    let store = Store::in_memory();
    store
        .insert_triples(&[(iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"))])
        .unwrap();
    let snap = store.snapshot();
    assert_eq!(snap.version(), 1);
    assert_eq!(snap.triple_count(), 1);

    // Mutate the live store; the pinned snapshot is unaffected.
    store
        .insert_triples(&[(iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/c"))])
        .unwrap();
    assert_eq!(snap.triple_count(), 1);
    assert_eq!(
        snap.scan_predicate(DEFAULT_GRAPH, &iri("http://ex/p"))
            .unwrap()
            .len(),
        1
    );

    // The live store sees both triples.
    assert_eq!(store.triple_count(), 2);
    assert_eq!(
        store
            .scan_predicate(DEFAULT_GRAPH, &iri("http://ex/p"))
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn store_retract_is_visible_to_new_reads_only() {
    let store = Store::in_memory();
    let t = (iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"));
    store.insert_triples(std::slice::from_ref(&t)).unwrap();
    let before = store.snapshot();
    let n = store.retract_triples(std::slice::from_ref(&t)).unwrap();
    assert_eq!(n, 1);

    assert_eq!(before.triple_count(), 1, "pinned-before read still sees it");
    assert_eq!(store.snapshot().triple_count(), 0, "new read does not");
}

#[test]
fn retract_of_uninterned_term_is_a_noop() {
    let store = Store::in_memory();
    let t = (iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"));
    store.insert_triples(std::slice::from_ref(&t)).unwrap();
    // A triple mentioning a term that was never inserted retracts nothing.
    let never = iri("http://ex/never-interned");
    let n = store
        .retract_triples(&[(never.clone(), iri("http://ex/p"), iri("http://ex/b"))])
        .unwrap();
    assert_eq!(n, 0);
    assert_eq!(store.triple_count(), 1);
    assert!(store.dictionary().get(&never).is_none());
}

#[test]
fn snapshot_s6_surface() {
    let store = Store::in_memory();
    let t = (iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"));
    store.insert_triples(std::slice::from_ref(&t)).unwrap();
    let snap = store.snapshot();

    let (s, p, o) = {
        let d = store.dictionary();
        (
            d.get(&t.0).unwrap(),
            d.get(&t.1).unwrap(),
            d.get(&t.2).unwrap(),
        )
    };
    assert!(snap.contains(s, p, o), "contains a present triple");
    assert!(
        !snap.contains(s, p, TermId(o.0 + 1)),
        "does not contain an absent one"
    );
    assert_eq!(snap.len(), 1);
    assert!(!snap.is_empty());
    assert_eq!(snap.logical_time(), snap.version());

    // Ordered iteration is key-sorted and stable.
    let ids: Vec<_> = snap.iter_all_term_ids().collect();
    assert_eq!(ids.len(), 1);
    assert_eq!(ids[0], (s, p, o));
}

#[test]
fn compact_reclaims_dead_rows_and_leaves_live_count_correct() {
    let store = Store::in_memory();
    let a = (iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"));
    let c = (iri("http://ex/c"), iri("http://ex/p"), iri("http://ex/d"));
    store.insert_triples(&[a.clone(), c.clone()]).unwrap();
    store.retract_triples(std::slice::from_ref(&a)).unwrap();

    // No pinned snapshot below the retraction's version, so the dead row
    // is reclaimable.
    store.compact();

    assert_eq!(store.triple_count(), 1, "live count still correct");
    let snap = store.snapshot();
    assert_eq!(snap.len(), 1);
    assert!(
        !snap.contains(
            store.dictionary().get(&a.0).unwrap(),
            store.dictionary().get(&a.1).unwrap(),
            store.dictionary().get(&a.2).unwrap(),
        ),
        "retracted triple stays absent after compaction"
    );
    // Physical check: the partition backing predicate `p` holds exactly
    // one row after compaction (the dead row was reclaimed, not just
    // hidden by the visibility filter). `tests` is inside `store.rs`, so
    // it can reach `StoreSnapshot.tier` (a `PinnedSnapshot`, Derefs to
    // `TierSnapshot`) directly.
    let p_id = store.dictionary().get(&a.1).unwrap();
    let phys = snap
        .tier
        .with_predicate(DEFAULT_GRAPH, p_id, |part| part.len())
        .unwrap();
    assert_eq!(phys, 1, "dead row physically reclaimed");
}

/// `StoreSnapshot::len()` is whole-store; the old default-graph-scoped
/// contract relocated to `graph_len` (SPEC-28 S2, #265, PLAN-28-02).
#[test]
fn snapshot_len_is_whole_store() {
    let store = Store::in_memory();
    store
        .insert_triples(std::slice::from_ref(&(
            iri("http://ex/a"),
            iri("http://ex/p"),
            iri("http://ex/b"),
        )))
        .unwrap();
    let g1 = store.intern_graph_uri(&iri("http://ex/graph1")).unwrap();
    store
        .insert_quads(&[(
            g1,
            iri("http://ex/x"),
            iri("http://ex/q"),
            iri("http://ex/y"),
        )])
        .unwrap();
    let absent = store.intern_graph_uri(&iri("http://ex/absent")).unwrap();

    let snap = store.snapshot();
    assert_eq!(
        snap.len(),
        2,
        "len() is whole-store, not default-graph scoped"
    );
    assert_eq!(snap.graph_len(DEFAULT_GRAPH), 1);
    assert_eq!(snap.graph_len(g1), 1);
    assert_eq!(snap.graph_len(absent), 0, "absent graph has no rows");
    assert_eq!(
        snap.iter_all_term_ids().count(),
        snap.graph_len(DEFAULT_GRAPH),
        "the default-graph iterator must not pick up named-graph rows"
    );
}

/// `graphs()` is visibility-filtered (D11: a graph exists iff it holds at
/// least one visible quad — a fully-retracted graph ceases to exist).
#[test]
fn graphs_is_visibility_filtered() {
    let store = Store::in_memory();
    let g1 = store.intern_graph_uri(&iri("http://ex/graph1")).unwrap();
    let g2 = store.intern_graph_uri(&iri("http://ex/graph2")).unwrap();
    let q1 = (
        g1,
        iri("http://ex/a"),
        iri("http://ex/p"),
        iri("http://ex/b"),
    );
    let q2 = (
        g2,
        iri("http://ex/c"),
        iri("http://ex/p"),
        iri("http://ex/d"),
    );
    store.insert_quads(&[q1, q2.clone()]).unwrap();
    store
        .insert_triples(&[(iri("http://ex/x"), iri("http://ex/p"), iri("http://ex/y"))])
        .unwrap();
    let n = store.retract_quads(std::slice::from_ref(&q2)).unwrap();
    assert_eq!(n, 1, "g2's only quad must be retracted");

    let snap = store.snapshot();
    let graphs = snap.graphs();
    assert!(graphs.contains(&g1), "g1 still holds a visible quad");
    assert!(
        !graphs.contains(&g2),
        "g2 is fully retracted and must not be enumerated"
    );
    assert_eq!(
        graphs.contains(&DEFAULT_GRAPH),
        snap.graph_len(DEFAULT_GRAPH) > 0,
        "DEFAULT_GRAPH appears in graphs() iff it holds data"
    );
    assert!(
        graphs.contains(&DEFAULT_GRAPH),
        "the default graph holds data in this fixture, so it must be enumerated"
    );

    // The tier-level view (used directly by production callers such as
    // `has_named_graph_data`) must agree with the snapshot view.
    let tier_graphs = store.tier().graphs();
    assert!(tier_graphs.contains(&g1));
    assert!(!tier_graphs.contains(&g2));
}

/// `live_len()` is frozen at partition-build time and is only equal to
/// "live at this snapshot's version" for the invariant documented on
/// `PredicatePartition::live_len` — a pinned older snapshot must still see
/// a graph a later retraction removed from the live view.
#[test]
fn graphs_on_a_pinned_snapshot_predate_a_later_retraction() {
    let store = Store::in_memory();
    let g2 = store.intern_graph_uri(&iri("http://ex/g2")).unwrap();
    let last = (
        g2,
        iri("http://ex/a"),
        iri("http://ex/p"),
        iri("http://ex/b"),
    );
    store.insert_quads(std::slice::from_ref(&last)).unwrap();

    let pinned = store.snapshot();
    assert!(pinned.graphs().contains(&g2), "g2 holds data when pinned");

    let n = store.retract_quads(std::slice::from_ref(&last)).unwrap();
    assert_eq!(n, 1, "g2's last quad must be retracted");

    assert!(
        pinned.graphs().contains(&g2),
        "the pinned-older snapshot must still list g2"
    );
    assert!(
        !store.snapshot().graphs().contains(&g2),
        "a fresh snapshot must not list the now-fully-retracted g2"
    );
}

/// `graph_uri` decodes a `GraphId` back to the IRI it was interned from;
/// the `DEFAULT_GRAPH` sentinel has no dictionary entry and errors.
#[test]
fn graph_uri_roundtrip() {
    let store = Store::in_memory();
    let t = iri("http://ex/graph1");
    let g = store.intern_graph_uri(&t).unwrap();

    let snap = store.snapshot();
    assert_eq!(snap.graph_uri(g).unwrap(), t);
    assert!(
        snap.graph_uri(DEFAULT_GRAPH).is_err(),
        "the default-graph sentinel has no URI"
    );
}

#[test]
fn retract_quads_removes_only_the_targeted_named_graph_quad() {
    let store = Store::in_memory();
    let g = store.intern_graph_uri(&iri("http://ex/graph1")).unwrap();
    let q1 = (
        g,
        iri("http://ex/a"),
        iri("http://ex/p"),
        iri("http://ex/b"),
    );
    let q2 = (
        g,
        iri("http://ex/c"),
        iri("http://ex/p"),
        iri("http://ex/d"),
    );
    store.insert_quads(&[q1.clone(), q2.clone()]).unwrap();

    let before = store.snapshot();
    let n = store.retract_quads(std::slice::from_ref(&q1)).unwrap();
    assert_eq!(n, 1);

    let p_id = store.dictionary().get(&q1.2).unwrap();
    let a_id = store.dictionary().get(&q1.1).unwrap();
    let b_id = store.dictionary().get(&q1.3).unwrap();
    let c_id = store.dictionary().get(&q2.1).unwrap();
    let d_id = store.dictionary().get(&q2.3).unwrap();

    // Pinned-before snapshot still sees both quads in the named graph.
    let before_rows = before
        .tier
        .with_predicate(g, p_id, |part| {
            part.scan_at(before.version()).collect::<Vec<_>>()
        })
        .unwrap();
    assert!(before_rows.contains(&(a_id, b_id)));
    assert!(before_rows.contains(&(c_id, d_id)));

    // A fresh snapshot sees the retraction: q1 gone, q2 survives.
    let after = store.snapshot();
    let after_rows = after
        .tier
        .with_predicate(g, p_id, |part| {
            part.scan_at(after.version()).collect::<Vec<_>>()
        })
        .unwrap();
    assert!(
        !after_rows.contains(&(a_id, b_id)),
        "retracted quad must be gone"
    );
    assert!(after_rows.contains(&(c_id, d_id)), "surviving quad remains");
}

/// `scan_graph` returns exactly one graph's visible triples, decoded. A
/// triple asserted in two graphs (same `(s, p, o)`, different `GraphId`)
/// must appear in both graphs' scans — graph membership, not triple
/// identity, is what's scoped.
#[test]
fn scan_graph_returns_exactly_the_graphs_quads() {
    let store = Store::in_memory();
    let g1 = store.intern_graph_uri(&iri("http://ex/g1")).unwrap();
    let g2 = store.intern_graph_uri(&iri("http://ex/g2")).unwrap();
    let shared = (iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"));
    let g1_only = (iri("http://ex/c"), iri("http://ex/p"), iri("http://ex/d"));
    let g2_only = (iri("http://ex/e"), iri("http://ex/q"), iri("http://ex/f"));
    let default_only = (iri("http://ex/x"), iri("http://ex/p"), iri("http://ex/y"));

    store
        .insert_quads(&[
            (g1, shared.0.clone(), shared.1.clone(), shared.2.clone()),
            (g2, shared.0.clone(), shared.1.clone(), shared.2.clone()),
            (g1, g1_only.0.clone(), g1_only.1.clone(), g1_only.2.clone()),
            (g2, g2_only.0.clone(), g2_only.1.clone(), g2_only.2.clone()),
        ])
        .unwrap();
    store
        .insert_triples(std::slice::from_ref(&default_only))
        .unwrap();

    let snap = store.snapshot();

    let g1_rows = snap.scan_graph(g1).unwrap();
    assert_eq!(g1_rows.len(), 2, "g1 holds the shared triple plus its own");
    assert!(g1_rows.contains(&shared));
    assert!(g1_rows.contains(&g1_only));
    assert!(
        !g1_rows.contains(&g2_only),
        "g1's scan must not see g2's triple"
    );
    assert!(
        !g1_rows.contains(&default_only),
        "g1's scan must not see default-graph data"
    );

    let g2_rows = snap.scan_graph(g2).unwrap();
    assert_eq!(g2_rows.len(), 2, "g2 holds the shared triple plus its own");
    assert!(
        g2_rows.contains(&shared),
        "the shared triple appears in both graphs' scans"
    );
    assert!(g2_rows.contains(&g2_only));
    assert!(!g2_rows.contains(&g1_only));
}

/// `scan_graph` respects visibility: a snapshot pinned before a retraction
/// still returns the retracted quad; a fresh snapshot omits it.
#[test]
fn scan_graph_respects_visibility() {
    let store = Store::in_memory();
    let g1 = store.intern_graph_uri(&iri("http://ex/g1")).unwrap();
    let keep = (iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"));
    let gone = (iri("http://ex/c"), iri("http://ex/p"), iri("http://ex/d"));
    store
        .insert_quads(&[
            (g1, keep.0.clone(), keep.1.clone(), keep.2.clone()),
            (g1, gone.0.clone(), gone.1.clone(), gone.2.clone()),
        ])
        .unwrap();

    // Pin a snapshot BEFORE the retraction.
    let before = store.snapshot();

    let n = store
        .retract_quads(&[(g1, gone.0.clone(), gone.1.clone(), gone.2.clone())])
        .unwrap();
    assert_eq!(n, 1);

    // The old, pinned-before snapshot still sees the retracted quad.
    let before_rows = before.scan_graph(g1).unwrap();
    assert_eq!(before_rows.len(), 2);
    assert!(before_rows.contains(&keep));
    assert!(before_rows.contains(&gone));

    // A fresh snapshot omits it.
    let after_rows = store.snapshot().scan_graph(g1).unwrap();
    assert_eq!(after_rows.len(), 1);
    assert!(after_rows.contains(&keep));
    assert!(!after_rows.contains(&gone));
}

/// `iter_graph_term_ids` mirrors `iter_all_term_ids`'s ordering contract:
/// predicates in ascending `TermId` order, subject-major (rows sorted by
/// subject id) within each predicate — scoped to one graph. Three
/// predicates (not two): with only two, deleting the predicate sort still
/// passes about half the time (`HashMap` iteration order has a 50% chance
/// of matching), which is exactly what let a missing sort through review
/// once. Within `p_a`, `s_hi`'s row is inserted before `s_lo`'s even
/// though `s_lo` has the lower id (interned earlier, via `p_b`'s row) —
/// subject id order opposes insertion order, so a broken
/// "insertion-order" implementation cannot pass by accident.
#[test]
fn iter_graph_term_ids_is_key_ordered() {
    let store = Store::in_memory();
    let g1 = store.intern_graph_uri(&iri("http://ex/g1")).unwrap();
    let o = iri("http://ex/o");

    // Interning order: s_lo, p_b, o, s_hi, p_a, s3, p_c.
    store
        .insert_quads(&[(g1, iri("http://ex/s_lo"), iri("http://ex/p_b"), o.clone())])
        .unwrap();
    store
        .insert_quads(&[(g1, iri("http://ex/s_hi"), iri("http://ex/p_a"), o.clone())])
        .unwrap();
    store
        .insert_quads(&[(g1, iri("http://ex/s_lo"), iri("http://ex/p_a"), o.clone())])
        .unwrap();
    store
        .insert_quads(&[(g1, iri("http://ex/s3"), iri("http://ex/p_c"), o.clone())])
        .unwrap();

    let snap = store.snapshot();
    let d = store.dictionary();
    let (s_lo, p_b, o_id, s_hi, p_a, s3, p_c) = (
        d.get(&iri("http://ex/s_lo")).unwrap(),
        d.get(&iri("http://ex/p_b")).unwrap(),
        d.get(&o).unwrap(),
        d.get(&iri("http://ex/s_hi")).unwrap(),
        d.get(&iri("http://ex/p_a")).unwrap(),
        d.get(&iri("http://ex/s3")).unwrap(),
        d.get(&iri("http://ex/p_c")).unwrap(),
    );
    assert!(
        p_b.0 < p_a.0 && p_a.0 < p_c.0,
        "predicates must intern in ascending order p_b, p_a, p_c"
    );
    assert!(s_lo.0 < s_hi.0, "s_lo must be interned before s_hi");

    let ids: Vec<_> = snap.iter_graph_term_ids(g1).collect();
    assert_eq!(
        ids,
        vec![
            (s_lo, p_b, o_id),
            (s_lo, p_a, o_id),
            (s_hi, p_a, o_id),
            (s3, p_c, o_id),
        ],
        "predicates ascending, subject-major within each predicate"
    );
}

/// `scan_predicate` takes a graph parameter: `scan_predicate(g1, &p)` sees
/// only `g1`'s rows, and `scan_predicate(DEFAULT_GRAPH, &p)` reproduces the
/// old default-graph-only behaviour on the same fixture.
#[test]
fn scan_predicate_takes_a_graph() {
    let store = Store::in_memory();
    let g1 = store.intern_graph_uri(&iri("http://ex/g1")).unwrap();
    let p = iri("http://ex/p");

    store
        .insert_triples(&[(iri("http://ex/a"), p.clone(), iri("http://ex/b"))])
        .unwrap();
    store
        .insert_quads(&[(g1, iri("http://ex/c"), p.clone(), iri("http://ex/d"))])
        .unwrap();

    let snap = store.snapshot();

    let g1_rows = snap.scan_predicate(g1, &p).unwrap();
    assert_eq!(g1_rows.len(), 1);
    assert_eq!(g1_rows[0], (iri("http://ex/c"), iri("http://ex/d")));

    let default_rows = snap.scan_predicate(DEFAULT_GRAPH, &p).unwrap();
    assert_eq!(default_rows.len(), 1);
    assert_eq!(default_rows[0], (iri("http://ex/a"), iri("http://ex/b")));
}
