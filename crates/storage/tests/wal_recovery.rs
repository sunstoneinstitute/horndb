//! SPEC-25 S3 / acceptance #3: a store opened with a write-ahead log comes
//! back after a kill with the same term ids, the same visible quads per
//! graph, the same physical rows and stamps, and the same commit version;
//! a torn tail is dropped; a corrupt record before the tail is an error;
//! a checkpoint switches generations atomically.
//!
//! "Kill" here is `std::mem::forget(store)`: no `Drop`, no flush — the
//! same bytes a SIGKILL would leave behind (everything the process wrote is
//! already in the kernel).

use horndb_storage::loader::nquads::load_nquads_reader;
use horndb_storage::loader::ntriples::load_ntriples_reader;
use horndb_storage::{GraphId, StorageError, Store, SyncPolicy};
use oxrdf::{Literal, NamedNode, Term};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

fn iri(s: &str) -> Term {
    Term::NamedNode(NamedNode::new(s).unwrap())
}

fn lit(s: &str) -> Term {
    Term::Literal(Literal::new_simple_literal(s))
}

const NQ: &[u8] = include_bytes!("fixtures/named_graphs.nq");

/// Blank-node-free N-Triples: a reload is a new document, and a blank node
/// would get a fresh label (HDB-113), so the id differential uses this one.
const NT: &str = "\
<http://example.org/s1> <http://example.org/name> \"Alice\" .
<http://example.org/s2> <http://example.org/name> \"Bob\"@en .
<http://example.org/s3> <http://example.org/age> \"42\"^^<http://www.w3.org/2001/XMLSchema#integer> .
<http://example.org/s4> <http://example.org/score> \"3.14\"^^<http://www.w3.org/2001/XMLSchema#decimal> .
<http://example.org/s1> <http://example.org/knows> <http://example.org/s2> .
";

/// Physical rows with stamps per (graph, predicate), visible quads per graph
/// (decoded), the commit version, and the dictionary's index space.
/// `(s, o, begin, end)` of one physical row.
type Row = (u64, u64, u64, u64);

#[derive(Debug, PartialEq, Eq)]
struct State {
    rows: BTreeMap<(u64, u64), Vec<Row>>,
    quads: BTreeMap<u64, Vec<String>>,
    version: u64,
    dict_len: usize,
}

fn state(store: &Store) -> State {
    let snap = store.snapshot();
    let tier = snap.tier_arc();
    let mut rows = BTreeMap::new();
    let mut quads = BTreeMap::new();
    for g in snap.graphs() {
        for p in tier.predicates(g) {
            let mut v = tier
                .with_predicate(g, p, |part| {
                    let part = part.as_warm().expect("replay rebuilds warm partitions");
                    (0..part.len())
                        .map(|i| {
                            (
                                part.subjects().value(i),
                                part.objects().value(i),
                                part.begins().value(i),
                                part.ends().value(i),
                            )
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap();
            v.sort_unstable();
            rows.insert((g.0, p.0), v);
        }
        let mut decoded: Vec<String> = snap
            .scan_graph(g)
            .unwrap()
            .into_iter()
            .map(|(s, p, o)| format!("{s} {p} {o}"))
            .collect();
        decoded.sort();
        quads.insert(g.0, decoded);
    }
    State {
        rows,
        quads,
        version: snap.version(),
        dict_len: store.dictionary().len(),
    }
}

/// Load both fixtures, then a mixed history: inserts of new terms, a
/// retraction, a combined apply, a no-op batch.
fn write_history(store: &Store) -> GraphId {
    load_nquads_reader(store, NQ).unwrap();
    load_ntriples_reader(store, NT.as_bytes()).unwrap();
    let g = store
        .intern_graph_uri(&iri("http://example.org/g3"))
        .unwrap();
    let q = |s: &str, p: &str, o: Term| (g, iri(s), iri(p), o);
    store
        .insert_quads(&[
            q("http://example.org/x", "http://example.org/p", lit("one")),
            q("http://example.org/x", "http://example.org/p", lit("two")),
            q("http://example.org/y", "http://example.org/p", lit("one")),
        ])
        .unwrap();
    assert_eq!(
        store
            .retract_quads(&[q(
                "http://example.org/x",
                "http://example.org/p",
                lit("two")
            )])
            .unwrap(),
        1
    );
    store
        .apply_quads(
            &[q(
                "http://example.org/y",
                "http://example.org/p",
                lit("one"),
            )],
            &[q(
                "http://example.org/y",
                "http://example.org/p",
                lit("three"),
            )],
        )
        .unwrap();
    // Net-empty: the WAL records it ahead, the tier does not bump.
    let before = store.snapshot().version();
    store
        .apply_quads(
            &[],
            &[q(
                "http://example.org/y",
                "http://example.org/p",
                lit("three"),
            )],
        )
        .unwrap();
    assert_eq!(store.snapshot().version(), before);
    store
        .retract_triples(&[(
            iri("http://example.org/Alice"),
            iri("http://example.org/knows"),
            iri("http://example.org/Bob"),
        )])
        .unwrap();
    g
}

fn dir_files(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn crash_after_append_recovers_ids_contents_and_stamps() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let g = write_history(&store);
    let before = state(&store);
    let x_id = store
        .dictionary()
        .get(&iri("http://example.org/x"))
        .unwrap();
    assert!(before.version > 0);
    std::mem::forget(store);

    let store = Store::open(dir.path()).unwrap();
    assert_eq!(state(&store), before);
    assert_eq!(
        store.dictionary().get(&iri("http://example.org/x")),
        Some(x_id)
    );
    assert_eq!(
        store.dictionary().lookup(x_id),
        Some(iri("http://example.org/x"))
    );
    assert_eq!(
        store
            .intern_graph_uri(&iri("http://example.org/g3"))
            .unwrap(),
        g
    );

    // The recovered store keeps issuing ids where the log stopped, and a
    // second kill/reopen carries the new work too.
    let new = store
        .dictionary()
        .intern(&iri("http://example.org/new"))
        .unwrap();
    assert_eq!(new.payload() as usize, before.dict_len + 1);
    store
        .insert_quads(&[(
            g,
            iri("http://example.org/new"),
            iri("http://example.org/p"),
            lit("n"),
        )])
        .unwrap();
    let after = state(&store);
    std::mem::forget(store);
    let store = Store::open(dir.path()).unwrap();
    assert_eq!(state(&store), after);
    assert_eq!(
        store.dictionary().lookup(new),
        Some(iri("http://example.org/new"))
    );
}

#[test]
fn id_differential_across_recovery() {
    // Same document into a fresh in-memory store and into a recovered WAL
    // store: identical ids; reloading into the recovered store allocates
    // nothing (the HDB-106 / dictionary_persist differential).
    let reference = Store::in_memory();
    load_ntriples_reader(&reference, NT.as_bytes()).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    load_ntriples_reader(&store, NT.as_bytes()).unwrap();
    std::mem::forget(store);

    let store = Store::open(dir.path()).unwrap();
    let len = store.dictionary().len();
    assert_eq!(len, reference.dictionary().len());
    load_ntriples_reader(&store, NT.as_bytes()).unwrap();
    assert_eq!(store.dictionary().len(), len, "reload must allocate no ids");
    let mut a = reference.scan_all_term_ids();
    let mut b = store.scan_all_term_ids();
    a.sort_unstable();
    b.sort_unstable();
    assert_eq!(a, b);
    assert_eq!(state(&reference).quads, state(&store).quads);
}

#[test]
fn checkpoint_then_append_then_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let g = write_history(&store);
    let checkpointed = state(&store);
    store.checkpoint().unwrap();
    assert_eq!(dir_files(dir.path()), ["MANIFEST", "dict.1", "wal.1"]);
    assert_eq!(
        state(&store),
        checkpointed,
        "checkpoint does not change state"
    );

    // Post-checkpoint work: a new term (logged after the base), a retraction,
    // then a kill.
    store
        .insert_quads(&[(
            g,
            iri("http://example.org/z"),
            iri("http://example.org/p"),
            lit("z"),
        )])
        .unwrap();
    store
        .retract_quads(&[(
            g,
            iri("http://example.org/x"),
            iri("http://example.org/p"),
            lit("one"),
        )])
        .unwrap();
    let after = state(&store);
    let z = store
        .dictionary()
        .get(&iri("http://example.org/z"))
        .unwrap();
    std::mem::forget(store);

    let store = Store::open(dir.path()).unwrap();
    let recovered = state(&store);
    assert_eq!(recovered.quads, after.quads);
    assert_eq!(recovered.version, after.version);
    assert_eq!(recovered.dict_len, after.dict_len);
    assert_eq!(
        store.dictionary().get(&iri("http://example.org/z")),
        Some(z)
    );
    assert_eq!(store.dictionary().base_len(), checkpointed.dict_len);
    // Rows committed after the checkpoint keep their stamps exactly; rows
    // the checkpoint carried restart at the checkpoint version.
    let z_rows = &recovered.rows[&(
        g.0,
        store
            .dictionary()
            .get(&iri("http://example.org/p"))
            .unwrap()
            .0,
    )];
    assert!(z_rows
        .iter()
        .any(|r| r.0 == z.0 && r.2 == after.version - 1));
    assert!(
        z_rows.iter().any(|r| r.3 == after.version),
        "retraction stamp"
    );

    // A second checkpoint retires the first generation.
    store.checkpoint().unwrap();
    assert_eq!(dir_files(dir.path()), ["MANIFEST", "dict.2", "wal.2"]);
    let after2 = state(&store);
    drop(store);
    // Rows the second checkpoint carried restart at its version and the dead
    // row is gone, so compare what the store says, not the physical stamps.
    let reopened = state(&Store::open(dir.path()).unwrap());
    assert_eq!(reopened.quads, after2.quads);
    assert_eq!(reopened.version, after2.version);
    assert_eq!(reopened.dict_len, after2.dict_len);
}

#[test]
fn checkpoint_of_emptied_store_restores_the_clock() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let t = (iri("http://ex/s"), iri("http://ex/p"), iri("http://ex/o"));
    store.insert_triples(std::slice::from_ref(&t)).unwrap();
    store.retract_triples(std::slice::from_ref(&t)).unwrap();
    store.checkpoint().unwrap();
    let version = store.snapshot().version();
    drop(store);
    let store = Store::open(dir.path()).unwrap();
    assert_eq!(store.snapshot().version(), version);
    assert_eq!(store.triple_count(), 0);
    store.insert_triples(&[t]).unwrap();
    assert_eq!(store.snapshot().version(), version + 1);
}

#[test]
fn torn_tail_record_is_dropped_and_truncated() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store
        .insert_triples(&[(iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"))])
        .unwrap();
    let good = state(&store);
    std::mem::forget(store);
    let wal = dir.path().join("wal.0");
    let intact = fs::read(&wal).unwrap();

    // A header promising more body than the file holds.
    let mut torn = intact.clone();
    torn.extend_from_slice(&1000u32.to_le_bytes());
    torn.extend_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
    torn.extend_from_slice(&[7u8; 20]);
    fs::write(&wal, &torn).unwrap();
    let store = Store::open(dir.path()).unwrap();
    assert_eq!(state(&store), good);
    assert_eq!(fs::read(&wal).unwrap(), intact, "tail truncated");
    // Appending after the truncation works, and the log stays readable.
    store
        .insert_triples(&[(iri("http://ex/c"), iri("http://ex/p"), iri("http://ex/d"))])
        .unwrap();
    let two = state(&store);
    drop(store);
    assert_eq!(state(&Store::open(dir.path()).unwrap()), two);

    // A complete last record with a bad checksum is torn too.
    let mut flipped = fs::read(&wal).unwrap();
    let last = flipped.len() - 1;
    flipped[last] ^= 0xff;
    fs::write(&wal, &flipped).unwrap();
    let store = Store::open(dir.path()).unwrap();
    assert_eq!(state(&store), good);
    assert_eq!(fs::read(&wal).unwrap(), intact);
}

#[test]
fn corrupted_middle_record_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    for i in 0..3 {
        store
            .insert_triples(&[(
                iri(&format!("http://ex/s{i}")),
                iri("http://ex/p"),
                iri("http://ex/o"),
            )])
            .unwrap();
    }
    std::mem::forget(store);
    let wal = dir.path().join("wal.0");
    let mut bytes = fs::read(&wal).unwrap();
    // Byte 8 is the first body byte of the first record (its kind).
    bytes[12] ^= 0x01;
    fs::write(&wal, &bytes).unwrap();
    match Store::open(dir.path()) {
        Err(StorageError::Wal(msg)) => assert!(msg.contains("checksum"), "{msg}"),
        other => panic!("expected a WAL error, got {:?}", other.map(|_| ())),
    }
    assert_eq!(
        fs::read(&wal).unwrap(),
        bytes,
        "a corrupt log is left alone"
    );
}

#[test]
fn compaction_between_records_keeps_the_log_replayable() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let gone = (
        iri("http://ex/s"),
        iri("http://ex/p"),
        iri("http://ex/gone"),
    );
    store.insert_triples(std::slice::from_ref(&gone)).unwrap();
    store.retract_triples(std::slice::from_ref(&gone)).unwrap();
    let gone_id = store.dictionary().get(&gone.2).unwrap();
    // An interned-but-never-committed term: the GC would free its index
    // before any record carried it, unless compact() logs it first.
    let orphan = store.dictionary().intern(&iri("http://ex/orphan")).unwrap();
    store.compact();
    assert_eq!(store.dictionary().lookup(gone_id), None);
    assert_eq!(store.dictionary().lookup(orphan), None);
    let keep = (
        iri("http://ex/s"),
        iri("http://ex/p"),
        iri("http://ex/keep"),
    );
    store.insert_triples(std::slice::from_ref(&keep)).unwrap();
    let keep_id = store.dictionary().get(&keep.2).unwrap();
    assert!(keep_id.payload() > orphan.payload());
    let quads = state(&store).quads;
    std::mem::forget(store);

    let store = Store::open(dir.path()).unwrap();
    assert_eq!(state(&store).quads, quads);
    assert_eq!(store.dictionary().get(&keep.2), Some(keep_id));
    // Compaction is not logged: the reclaimed indices come back resolvable,
    // and nothing visible names them.
    assert_eq!(store.dictionary().lookup(gone_id), Some(gone.2));
    assert_eq!(
        store.dictionary().lookup(orphan),
        Some(iri("http://ex/orphan"))
    );
}

#[test]
fn timed_policy_round_trips_and_in_memory_store_has_no_log() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_with(
        dir.path(),
        SyncPolicy::Every(std::time::Duration::from_secs(3600)),
    )
    .unwrap();
    store
        .insert_triples(&[(iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"))])
        .unwrap();
    store.sync_wal().unwrap();
    let s = state(&store);
    std::mem::forget(store);
    assert_eq!(state(&Store::open(dir.path()).unwrap()), s);

    let mem = Store::in_memory();
    assert!(matches!(mem.checkpoint(), Err(StorageError::Wal(_))));
    assert!(matches!(mem.sync_wal(), Err(StorageError::Wal(_))));
}

#[test]
fn stale_generation_files_are_swept_on_open() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store
        .insert_triples(&[(iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"))])
        .unwrap();
    let s = state(&store);
    drop(store);
    // A checkpoint that died before its MANIFEST rename.
    fs::write(dir.path().join("dict.1"), b"junk").unwrap();
    fs::write(dir.path().join("wal.1"), b"junk").unwrap();
    let store = Store::open(dir.path()).unwrap();
    assert_eq!(state(&store), s);
    assert_eq!(dir_files(dir.path()), ["MANIFEST", "wal.0"]);
}

#[test]
fn crash_after_manifest_switch_before_unlink() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    write_history(&store);
    let old_log = fs::read(dir.path().join("wal.0")).unwrap();
    store.checkpoint().unwrap();
    let after = state(&store);
    std::mem::forget(store);
    // The checkpoint died after its MANIFEST rename but before the unlink:
    // the previous generation is still on disk and must be ignored.
    fs::write(dir.path().join("wal.0"), old_log).unwrap();
    let reopened = state(&Store::open(dir.path()).unwrap());
    assert_eq!(reopened.quads, after.quads);
    assert_eq!(reopened.version, after.version);
    assert_eq!(reopened.dict_len, after.dict_len);
    assert_eq!(dir_files(dir.path()), ["MANIFEST", "dict.1", "wal.1"]);
}
