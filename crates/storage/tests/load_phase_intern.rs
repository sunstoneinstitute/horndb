//! `intern` is a counted load phase on the bulk-load path (HDB-106).
//!
//! Before HDB-106 the storage loaders instrumented `parse` and the tier
//! phases but not interning, so `intern` could only be reported as a residue —
//! wall clock minus the counted phases. It was the largest phase of a load
//! (56–61% on trainmarks xlarge) and the one nobody could optimise against,
//! because a subtraction also absorbs whatever else is unmetered.
//!
//! This pins the counter against going quietly dead: it has to move by exactly
//! the number of rows loaded, on the probed (multi-thread) and unprobed
//! (single-thread) paths alike, and it has to charge them some time.
//!
//! One test function, because `storage_load_phase_*` is process-global.

use horndb_metrics::labels::{LoadPhase, LoadPhaseLabel};
use horndb_storage::loader::ntriples::load_ntriples_slice_with_threads;
use horndb_storage::Store;

fn intern_counters() -> (u64, u64) {
    let m = horndb_metrics::metrics();
    let label = LoadPhaseLabel {
        phase: LoadPhase::Intern,
    };
    (
        m.storage.load_phase_nanoseconds.get_or_create(&label).get(),
        m.storage.load_phase_rows.get_or_create(&label).get(),
    )
}

/// Enough distinct terms that the load is not all misses, and enough bytes
/// that `oxttl` will hand back more than one chunk at 4 threads (it applies a
/// 16 KiB-per-chunk floor).
fn corpus(triples: usize) -> Vec<u8> {
    let mut out = String::new();
    for i in 0..triples {
        out.push_str(&format!(
            "<http://example.org/s{}> <http://example.org/p{}> <http://example.org/o{i}> .\n",
            i % 64,
            i % 8
        ));
    }
    out.into_bytes()
}

#[test]
fn the_intern_phase_is_counted_on_both_slice_paths() {
    let doc = corpus(20_000);
    assert!(doc.len() > 4 * (16 << 10), "corpus must split into chunks");

    // Probed path: several parse threads, each resolving what it can before
    // the calling thread interns the rest.
    let (ns0, rows0) = intern_counters();
    let parallel = Store::in_memory();
    let stats = load_ntriples_slice_with_threads(&parallel, &doc, 4).unwrap();
    let (ns1, rows1) = intern_counters();
    assert_eq!(stats.triples, 20_000);
    assert_eq!(rows1 - rows0, 20_000, "intern rows on the probed path");
    assert!(ns1 > ns0, "intern charged no time on the probed path");

    // Unprobed path: one chunk, no probe, same counter.
    let serial = Store::in_memory();
    load_ntriples_slice_with_threads(&serial, &doc, 1).unwrap();
    let (ns2, rows2) = intern_counters();
    assert_eq!(rows2 - rows1, 20_000, "intern rows on the unprobed path");
    assert!(ns2 > ns1, "intern charged no time on the unprobed path");

    // And the two paths still agree on the dictionary they built.
    assert_eq!(parallel.dictionary().len(), serial.dictionary().len());
    assert_eq!(parallel.triple_count(), serial.triple_count());
}
