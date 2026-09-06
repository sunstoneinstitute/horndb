//! SPEC-25 S5 — cold, memory-mapped partitions behind the warm read surface.
//!
//! Demotion and promotion are maintenance, not logical writes: they must leave
//! the commit version alone and leave every pinned reader's view untouched.
//! These tests pin that, plus the read-surface equivalence across all six
//! orderings, the promote-before-write rule, and the NF1 cold byte budget.

use horndb_storage::cold::{cold_path, ColdPartition};
use horndb_storage::{Ordering, Store, TermId, DEFAULT_GRAPH};
use oxrdf::{NamedNode, Term};
use std::collections::BTreeSet;
use std::path::Path;

fn iri(s: impl AsRef<str>) -> Term {
    Term::NamedNode(NamedNode::new(s.as_ref()).unwrap())
}

fn p_id(store: &Store, p: &Term) -> TermId {
    store.dictionary().get(p).expect("predicate was interned")
}

/// Everything a reader can ask a partition, at one version: all six orderings,
/// the subject-major scan, the row count, and a membership probe per row. This
/// is the surface a demotion must not change.
type PartitionView = (Vec<Vec<(u64, u64)>>, Vec<(u64, u64)>, usize, Vec<bool>);

fn view(store: &Store, p: TermId, probes: &[(TermId, TermId)]) -> PartitionView {
    let snap = store.snapshot();
    let at = snap.version();
    snap.tier_arc()
        .with_predicate(DEFAULT_GRAPH, p, |part| {
            (
                Ordering::ALL
                    .iter()
                    .map(|&ord| {
                        part.ordered_at(ord, at)
                            .scan()
                            .map(|(a, b)| (a.0, b.0))
                            .collect()
                    })
                    .collect(),
                part.scan_at(at).map(|(s, o)| (s.0, o.0)).collect(),
                part.len_at(at),
                probes
                    .iter()
                    .map(|&(s, o)| part.contains_at(s, o, at))
                    .collect(),
            )
        })
        .expect("partition exists")
}

fn is_cold(store: &Store, p: TermId) -> bool {
    store
        .snapshot()
        .tier_arc()
        .with_predicate(DEFAULT_GRAPH, p, |part| part.is_cold())
        .expect("partition exists")
}

/// Physically stored rows, dead MVCC history included.
fn physical_len(store: &Store, p: TermId) -> usize {
    store
        .snapshot()
        .tier_arc()
        .with_predicate(DEFAULT_GRAPH, p, |part| part.len())
        .expect("partition exists")
}

/// Every default-graph triple as a comparable set of stringified terms.
fn triple_set(store: &Store) -> BTreeSet<(String, String, String)> {
    let dict = store.dictionary();
    store
        .scan_all_term_ids()
        .into_iter()
        .map(|(s, p, o)| {
            (
                dict.lookup(s).unwrap().to_string(),
                dict.lookup(p).unwrap().to_string(),
                dict.lookup(o).unwrap().to_string(),
            )
        })
        .collect()
}

/// Sum of every cold partition's mapped file length, and how many are cold.
fn cold_bytes(store: &Store) -> (u64, usize) {
    let snap = store.snapshot();
    let tier = snap.tier_arc();
    let mut bytes = 0u64;
    let mut count = 0usize;
    for g in snap.graphs() {
        for p in tier.predicates(g) {
            tier.with_predicate(g, p, |part| {
                if part.is_cold() {
                    bytes += part.estimated_bytes();
                    count += 1;
                }
            });
        }
    }
    (bytes, count)
}

#[test]
fn cold_roundtrip_all_six_orderings() {
    let store = Store::in_memory();
    let p = iri("http://ex/p");
    // Deliberately not in subject or object order, so the six orderings are
    // actually exercised rather than accidentally agreeing.
    let rows = [(1u32, 5u32), (1, 2), (3, 2), (2, 9), (3, 1)];
    let triples: Vec<_> = rows
        .iter()
        .map(|&(s, o)| {
            (
                iri(format!("http://ex/s{s}")),
                p.clone(),
                iri(format!("http://ex/o{o}")),
            )
        })
        .collect();
    store.insert_triples(&triples).unwrap();
    let pid = p_id(&store, &p);

    // Probe every stored pair plus one that is not there.
    let dict = store.dictionary();
    let mut probes: Vec<(TermId, TermId)> = triples
        .iter()
        .map(|(s, _, o)| (dict.get(s).unwrap(), dict.get(o).unwrap()))
        .collect();
    probes.push((probes[0].0, probes[3].1));

    let before = view(&store, pid, &probes);
    assert!(!is_cold(&store, pid));

    assert!(store.demote(DEFAULT_GRAPH, pid).unwrap());
    assert!(is_cold(&store, pid), "demote must flip the partition cold");
    assert_eq!(
        view(&store, pid, &probes),
        before,
        "cold read surface drifted"
    );

    assert!(store.promote(DEFAULT_GRAPH, pid).unwrap());
    assert!(!is_cold(&store, pid), "promote must flip it back warm");
    assert_eq!(
        view(&store, pid, &probes),
        before,
        "warm read surface drifted"
    );
}

#[test]
fn write_to_cold_partition_promotes_first() {
    // Each scenario (insert / retract / apply) uses its own predicate. A
    // demote refuses once its partition holds a dead row a live pin still
    // needs (see `has_retractions` in `MemoryTier::demote`), so re-demoting
    // the *same* predicate after retracting from it would be refused for the
    // rest of this test's lifetime — that is the exact case
    // `demote_runs_compaction_and_encodes_only_visible_rows` and
    // `pinned_reader_survives_demote_and_dictionary_gc` exercise. Here we
    // only care that a write to an already-cold partition promotes it first,
    // so each predicate is demoted exactly once, before it has any dead
    // history of its own.
    let store = Store::in_memory();
    let p_ins = iri("http://ex/p-ins");
    let p_ret = iri("http://ex/p-ret");
    let p_app = iri("http://ex/p-app");
    let t = |p: &Term, s: &str, o: &str| (iri(s), p.clone(), iri(o));
    let a = t(&p_ins, "http://ex/a", "http://ex/1");
    let b = t(&p_ret, "http://ex/b", "http://ex/2");
    let c = t(&p_app, "http://ex/c", "http://ex/3");
    let d = t(&p_app, "http://ex/d", "http://ex/4");
    store
        .insert_triples(&[a.clone(), b.clone(), c.clone()])
        .unwrap();
    let pid_ins = p_id(&store, &p_ins);
    let pid_ret = p_id(&store, &p_ret);
    let pid_app = p_id(&store, &p_app);

    let pin_before = store.pin();
    let at_before = pin_before.version();

    // Insert lands on a demoted partition.
    let e = t(&p_ins, "http://ex/e", "http://ex/5");
    assert!(store.demote(DEFAULT_GRAPH, pid_ins).unwrap());
    store.insert_triples(std::slice::from_ref(&e)).unwrap();
    assert!(!is_cold(&store, pid_ins), "an insert must promote first");
    assert_eq!(view(&store, pid_ins, &[]).2, 2);

    // Retraction lands on a demoted partition.
    assert!(store.demote(DEFAULT_GRAPH, pid_ret).unwrap());
    assert_eq!(store.retract_triples(std::slice::from_ref(&b)).unwrap(), 1);
    assert!(!is_cold(&store, pid_ret), "a retraction must promote first");
    assert_eq!(view(&store, pid_ret, &[]).2, 0);

    // Combined apply lands on a demoted partition.
    assert!(store.demote(DEFAULT_GRAPH, pid_app).unwrap());
    let quad = |t: &(Term, Term, Term)| (DEFAULT_GRAPH, t.0.clone(), t.1.clone(), t.2.clone());
    let report = store.apply_quads(&[quad(&c)], &[quad(&d)]).expect("apply");
    assert_eq!((report.retracted, report.inserted), (1, 1));
    assert!(!is_cold(&store, pid_app), "an apply must promote first");

    // Visible set is right at the newest version …
    let now = triple_set(&store);
    assert_eq!(now.len(), 3, "{now:?}");
    assert!(now.contains(&(
        "<http://ex/a>".into(),
        "<http://ex/p-ins>".into(),
        "<http://ex/1>".into()
    )));
    assert!(now.contains(&(
        "<http://ex/e>".into(),
        "<http://ex/p-ins>".into(),
        "<http://ex/5>".into()
    )));
    assert!(now.contains(&(
        "<http://ex/d>".into(),
        "<http://ex/p-app>".into(),
        "<http://ex/4>".into()
    )));

    // … and at the version pinned before any of this happened: each
    // predicate had exactly one row at `at_before`.
    let old = store.snapshot_at(&pin_before);
    assert_eq!(old.version(), at_before);
    for pid in [pid_ins, pid_ret, pid_app] {
        assert_eq!(
            old.tier_arc()
                .with_predicate(DEFAULT_GRAPH, pid, |part| part.len_at(at_before))
                .unwrap(),
            1,
            "the pre-demotion pin must still see exactly its own row"
        );
    }
}

#[test]
fn demote_is_not_a_logical_write() {
    let store = Store::in_memory();
    let p = iri("http://ex/p");
    let t = |s: &str| (iri(s), p.clone(), iri("http://ex/o"));
    let (t1, t2, t3) = (t("http://ex/a"), t("http://ex/b"), t("http://ex/c"));
    store
        .insert_triples(&[t1.clone(), t2.clone(), t3.clone()])
        .unwrap();
    let pid = p_id(&store, &p);

    // Pinned before the retraction: must keep seeing all three rows.
    let pin_pre_retract = store.pin();
    let v_pre = pin_pre_retract.version();
    assert_eq!(store.retract_triples(std::slice::from_ref(&t3)).unwrap(), 1);

    // Pinned after the retraction, before the demotion.
    let pin_pre_demote = store.pin();
    let v_now = pin_pre_demote.version();
    assert!(v_now > v_pre);
    let read = |pin: &horndb_storage::PinnedSnapshot| {
        let at = pin.version();
        pin.with_predicate(DEFAULT_GRAPH, pid, |part| {
            let mut rows: Vec<(u64, u64)> = part.scan_at(at).map(|(s, o)| (s.0, o.0)).collect();
            rows.sort_unstable();
            rows
        })
        .unwrap()
    };
    let pre_retract_rows = read(&pin_pre_retract);
    let pre_demote_rows = read(&pin_pre_demote);
    assert_eq!(pre_retract_rows.len(), 3);
    assert_eq!(pre_demote_rows.len(), 2);

    // `pin_pre_retract` sits below the compaction horizon and still needs
    // t3's dead row (see `has_retractions` in `MemoryTier::demote`), so a
    // demote here would be correctly refused while it is held — that
    // contract is `pinned_reader_survives_demote_and_dictionary_gc`'s job to
    // check. Drop it so `compact()` can reclaim the row and this test can
    // get back to its own job: proving demote/promote never move the commit
    // version and never disturb a pin that *doesn't* need reclaimed history.
    drop(pin_pre_retract);

    assert!(store.demote(DEFAULT_GRAPH, pid).unwrap());
    assert_eq!(
        store.snapshot().version(),
        v_now,
        "demotion must not advance the commit version"
    );
    assert_eq!(
        read(&pin_pre_demote),
        pre_demote_rows,
        "a pin taken before the demotion must read the identical set"
    );

    assert!(store.promote(DEFAULT_GRAPH, pid).unwrap());
    assert_eq!(
        store.snapshot().version(),
        v_now,
        "promotion must not advance the commit version"
    );
    assert_eq!(read(&pin_pre_demote), pre_demote_rows);
}

#[test]
fn demote_runs_compaction_and_encodes_only_visible_rows() {
    let store = Store::in_memory();
    let (pa, pb, pc) = (
        iri("http://ex/pa"),
        iri("http://ex/pb"),
        iri("http://ex/pc"),
    );
    let row = |p: &Term, s: &str| (iri(s), p.clone(), iri("http://ex/o"));
    store
        .insert_triples(&[
            row(&pa, "http://ex/a1"),
            row(&pa, "http://ex/a2"),
            row(&pa, "http://ex/a3"),
            row(&pb, "http://ex/b1"),
            row(&pb, "http://ex/b2"),
            row(&pc, "http://ex/c1"),
            row(&pc, "http://ex/c2"),
        ])
        .unwrap();
    let (pa_id, pb_id, pc_id) = (p_id(&store, &pa), p_id(&store, &pb), p_id(&store, &pc));

    // Pinned before the retraction, so compaction cannot reclaim the rows it
    // ends — dead history below this pin's version is exactly what a pinned
    // reader can still resolve.
    let pin = store.pin();
    assert_eq!(
        store
            .retract_triples(&[
                row(&pa, "http://ex/a3"),
                row(&pb, "http://ex/b2"),
                row(&pc, "http://ex/c2"),
            ])
            .unwrap(),
        3
    );
    let at = store.snapshot().version();

    // --- with the pin alive: demote must refuse rather than encode only the
    // rows visible now and drop the dead row the pin still needs.
    assert!(
        !store.demote(DEFAULT_GRAPH, pc_id).unwrap(),
        "a retraction below the pinned horizon must block demotion"
    );
    assert!(!is_cold(&store, pc_id), "the refused partition stays warm");
    assert_eq!(
        physical_len(&store, pa_id),
        3,
        "the pin must keep dead history alive, or this test proves nothing"
    );
    drop(pin);

    // --- with no pin: demotion's compaction pass reclaims the dead history,
    // and the cold file it writes holds only the rows still visible.
    assert!(store.demote(DEFAULT_GRAPH, pc_id).unwrap());
    let cold_c = ColdPartition::open(&cold_path(store.cold_dir(), DEFAULT_GRAPH, pc_id)).unwrap();
    assert_eq!(cold_c.len(), 1, "cold must hold only rows visible at `at`");
    assert_eq!(cold_c.version(), at);

    assert!(store.demote(DEFAULT_GRAPH, pa_id).unwrap());
    let cold_a = ColdPartition::open(&cold_path(store.cold_dir(), DEFAULT_GRAPH, pa_id)).unwrap();
    assert_eq!(cold_a.len(), 2);
    assert_eq!(cold_a.len(), view(&store, pa_id, &[]).2);
    assert_eq!(
        physical_len(&store, pb_id),
        1,
        "demotion must run the compaction pass"
    );
}

/// The bug this guards against (HDB-177 review): `demote` used to encode only
/// the rows visible at its own version, dropping any row a pin below the
/// compaction horizon still needed. `Store::compact()` is the only caller of
/// the dictionary GC (`gc_dictionary`), which marks live terms from the rows
/// the tier physically holds — so a cold file missing that dead row made the
/// GC free its terms out from under the still-live pin, and the pin's next
/// read failed with `InvalidTerm` instead of returning the triple.
#[test]
fn pinned_reader_survives_demote_and_dictionary_gc() {
    let store = Store::in_memory();
    let p = iri("http://ex/p");
    let (t1, t2) = (
        (iri("http://ex/a"), p.clone(), iri("http://ex/1")),
        (iri("http://ex/b"), p.clone(), iri("http://ex/2")),
    );
    store.insert_triples(&[t1.clone(), t2.clone()]).unwrap();
    let pid = p_id(&store, &p);

    let pin = store.pin();
    assert_eq!(store.retract_triples(std::slice::from_ref(&t1)).unwrap(), 1);

    // Demotion must refuse — the pin still needs t1's dead row — and the
    // dictionary sweep must not free t1's terms out from under it.
    assert!(!store.demote(DEFAULT_GRAPH, pid).unwrap());
    store.compact();

    let pinned = store.snapshot_at(&pin);
    let rows = pinned
        .scan_graph(DEFAULT_GRAPH)
        .expect("the pinned reader must still resolve both triples' terms");
    assert_eq!(
        rows.len(),
        2,
        "both triples must still be visible to the pin"
    );
}

#[test]
fn export_snapshot_covers_cold_partitions() {
    let store = Store::in_memory();
    let triples: Vec<_> = (0..40u32)
        .map(|i| {
            (
                iri(format!("http://ex/s{}", i % 7)),
                iri(format!("http://ex/p{}", i % 3)),
                iri(format!("http://ex/o{i}")),
            )
        })
        .collect();
    store.insert_triples(&triples).unwrap();
    let before = triple_set(&store);

    assert_eq!(store.demote_all().unwrap(), 3);
    assert_eq!(cold_bytes(&store).1, 3);

    let mut bytes = Vec::new();
    store.export_snapshot(&mut bytes).unwrap();
    let reimported = Store::in_memory();
    reimported.import_snapshot(&mut &bytes[..]).unwrap();
    assert_eq!(triple_set(&reimported), before);
}

#[test]
fn cold_placement_is_not_durable() {
    let dir = tempfile::tempdir().unwrap();
    let cold_dir = dir.path().join("cold");
    let triples: Vec<_> = (0..20u32)
        .map(|i| {
            (
                iri(format!("http://ex/s{i}")),
                iri(format!("http://ex/p{}", i % 2)),
                iri("http://ex/o"),
            )
        })
        .collect();

    let before = {
        let store = Store::open(dir.path()).unwrap();
        store.insert_triples(&triples).unwrap();
        assert_eq!(
            store.cold_dir(),
            cold_dir.as_path(),
            "demote_all must derive `<dir>/cold`"
        );
        assert_eq!(store.demote_all().unwrap(), 2);
        assert!(cold_dir.exists());
        triple_set(&store)
    };

    let store = Store::open(dir.path()).unwrap();
    assert_eq!(
        triple_set(&store),
        before,
        "replay must restore every triple"
    );
    assert_eq!(cold_bytes(&store), (0, 0), "everything comes back warm");
    assert!(
        !cold_dir.exists(),
        "reopen must drop the stale cold directory"
    );
}

/// SPEC-25 NF1 on the cold tier: the mapped bytes of a demoted store must stay
/// under 6 bytes per triple. Synthetic LUBM-shaped data, the same corpus
/// `snapshot_footprint.rs` measures the export format against; the hornbench
/// number is a separate task.
#[test]
fn cold_bytes_per_triple_under_six() {
    let store = Store::in_memory();
    let base = "http://www.lehigh.edu/univ-bench";
    let type_p = iri(format!("{base}#type"));
    let advisor_p = iri(format!("{base}#advisor"));
    let member_p = iri(format!("{base}#memberOf"));
    let takes_p = iri(format!("{base}#takesCourse"));

    let mut triples = Vec::new();
    for u in 0..10 {
        for d in 0..20 {
            let dept = iri(format!("{base}/University{u}/Department{d}"));
            for s in 0..50 {
                let student = iri(format!(
                    "{base}/University{u}/Department{d}/GraduateStudent{s}"
                ));
                let course = iri(format!(
                    "{base}/University{u}/Department{d}/Course{}",
                    s % 12
                ));
                let prof = iri(format!(
                    "{base}/University{u}/Department{d}/Professor{}",
                    s % 6
                ));
                let grad = iri(format!("{base}#GraduateStudent"));
                triples.push((student.clone(), type_p.clone(), grad));
                triples.push((student.clone(), member_p.clone(), dept.clone()));
                triples.push((student.clone(), advisor_p.clone(), prof));
                triples.push((student.clone(), takes_p.clone(), course));
            }
        }
    }
    store.insert_triples(&triples).unwrap();
    let count = triples.len() as u64;

    assert_eq!(store.demote_all().unwrap(), 4);
    let (bytes, cold) = cold_bytes(&store);
    assert_eq!(cold, 4);
    let bpt = bytes as f64 / count as f64;
    eprintln!("cold: {count} triples in {cold} partitions, {bytes} B => {bpt:.3} B/triple");
    assert!(
        bpt <= 6.0,
        "cold footprint {bpt:.3} B/triple exceeds the NF1 budget of 6.0"
    );
}

/// `TierStats.bytes_cold` (SPEC-25 S5, HDB-178) must track the real mapped
/// file bytes of cold partitions, not a placeholder. Checked against file
/// sizes read straight off disk (not the field compared to itself), so a
/// `bytes_cold` hardwired to `0` or to the total would fail this.
#[test]
fn tier_stats_bytes_cold_matches_mapped_file_bytes() {
    let store = Store::in_memory();
    let triples: Vec<_> = (0..30u32)
        .map(|i| {
            (
                iri(format!("http://ex/s{i}")),
                iri(format!("http://ex/p{}", i % 3)),
                iri(format!("http://ex/o{i}")),
            )
        })
        .collect();
    store.insert_triples(&triples).unwrap();

    let before = store.stats();
    assert_eq!(before.bytes_cold, 0, "nothing is cold yet");
    assert!(before.bytes_estimated > 0);

    assert_eq!(store.demote_all().unwrap(), 3);

    // Sum every file actually written under the cold directory — the ground
    // truth `bytes_cold` must match.
    let disk_bytes: u64 = std::fs::read_dir(store.cold_dir())
        .unwrap()
        .map(|entry| entry.unwrap().metadata().unwrap().len())
        .sum();
    assert!(disk_bytes > 0, "demote_all must have written cold files");

    let after = store.stats();
    assert_eq!(
        after.bytes_cold, disk_bytes,
        "bytes_cold must equal the real summed cold file lengths"
    );
    // `demote_all` demoted every one of the 3 predicate partitions (asserted
    // above), so nothing warm is left: `bytes_estimated` is exactly the cold
    // sum plus the fixed 16 B/physically-retained-predicate overhead
    // `MemoryTier::stats` adds on top of the per-partition byte sum.
    assert_eq!(
        after.bytes_estimated,
        after.bytes_cold + 3 * 16,
        "bytes_estimated must stay the warm+cold total"
    );
}

/// A cold file that outlives the mapping's directory entry still reads: the
/// mapping holds the inode. This is what makes `promote`'s unlink safe while a
/// reader is pinned on the cold partition.
#[test]
fn unlinked_cold_file_stays_readable() {
    let dir = tempfile::tempdir().unwrap();
    let path: &Path = &dir.path().join("x.cold");
    let store = Store::in_memory();
    let p = iri("http://ex/p");
    store
        .insert_triples(&[(iri("http://ex/a"), p.clone(), iri("http://ex/b"))])
        .unwrap();
    let pid = p_id(&store, &p);
    let snap = store.snapshot();
    let rows: Vec<_> = snap
        .tier_arc()
        .with_predicate(DEFAULT_GRAPH, pid, |part| {
            part.scan_at(snap.version()).collect::<Vec<_>>()
        })
        .unwrap();
    ColdPartition::write(path, DEFAULT_GRAPH, pid, snap.version(), rows.into_iter()).unwrap();
    let cold = ColdPartition::open(path).unwrap();
    std::fs::remove_file(path).unwrap();
    assert_eq!(cold.scan().count(), 1);
}
