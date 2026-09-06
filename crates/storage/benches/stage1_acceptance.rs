//! SPEC-25 S6 (HDB-61): the three deferred SPEC-02 Stage-1 acceptance
//! measurements, run once on a real LUBM-scale corpus.
//!
//! Importing ~1.1 B triples takes on the order of tens of minutes, so this
//! is **not** a criterion benchmark — criterion resamples a function ten or
//! more times to get a confidence interval, which is not affordable here.
//! Instead this is a plain `fn main` that measures each thing once (import,
//! footprint) or a handful of times keeping the best (the `rdf:type` scan
//! and the STREAM Triad kernel are cheap enough to resample, and a single
//! scheduler or page-cache hiccup should not decide the verdict). It is
//! still registered as a `harness = false` Cargo bench so `cargo bench`
//! builds it in release mode.
//!
//! Every result prints as one `[s1] key=value` line on stdout, so
//! `scripts/bench/stage1-acceptance.sh` can grep them out and turn them into
//! a summary table. See `docs/specs/SPEC-25-storage-stage2.md` S6 for the
//! three acceptance criteria this measures.

use horndb_storage::loader::ntriples::load_ntriples_file;
use horndb_storage::term::DEFAULT_GRAPH;
use horndb_storage::Store;
use oxrdf::{NamedNode, Term};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const RDF_TYPE_IRI: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

// SPEC-02 Stage-1 acceptance targets 2-4 (SPEC-25 S6).
const IMPORT_TARGET_SECONDS: f64 = 30.0 * 60.0;
// 55 GB decimal (55e9 bytes, SI convention) — the spec states this as a
// 50 B/triple budget over 1.1 B triples, which is the same number.
const FOOTPRINT_TARGET_BYTES: f64 = 55e9;
const SCAN_OVER_TRIAD_TARGET: f64 = 0.80;

fn main() {
    let nt_path = required_env_path("LUBM_NT");
    let input_bytes = std::fs::metadata(&nt_path)
        .unwrap_or_else(|e| panic!("stat {}: {e}", nt_path.display()))
        .len();

    let store = Store::in_memory();

    // `load_*_file` reads a document whole — and so parses it across threads
    // — only between a floor and `max_slice_bytes`. Only the ceiling matters
    // at LUBM scale, so report exactly that rather than restate the whole
    // heuristic: over the cap, the import is single-threaded streaming.
    let over_slice_cap = input_bytes > horndb_storage::loader::max_slice_bytes() as u64;

    if let Some(tbox_path) = optional_env_path("LUBM_TBOX") {
        // Loaded before the timer starts: acceptance 2 is about importing
        // the (huge) instance data, and the ontology is a few hundred
        // triples that would only add noise to the number.
        load_ntriples_file(&store, &tbox_path).expect("load LUBM_TBOX");
    }

    // --- acceptance 2: import wall clock ---
    let import_start = Instant::now();
    let stats = load_ntriples_file(&store, &nt_path).expect("load LUBM_NT");
    let import_seconds = import_start.elapsed().as_secs_f64();

    let triples = stats.triples;
    let triples_per_sec = if import_seconds > 0.0 {
        triples as f64 / import_seconds
    } else {
        f64::INFINITY
    };
    println!("[s1] import_seconds={import_seconds:.3}");
    println!("[s1] triples={triples}");
    println!("[s1] triples_per_sec={triples_per_sec:.1}");
    println!("[s1] input_bytes={input_bytes}");
    // Which parse path the loader actually took, so a slow import is not
    // mistaken for a slow parser. `load_*_file` only reads a document whole
    // (and so only parses it across threads) up to `max_slice_bytes`, 2 GiB
    // by default; anything larger streams through one thread. A LUBM-8000
    // N-Triples file is ~188 GB, so it streams. That is the shipped default
    // for a file this size, which is what acceptance 2 has to measure.
    println!("[s1] over_slice_cap={over_slice_cap}");
    println!(
        "[s1] load_threads={}",
        horndb_storage::loader::load_threads()
    );
    println!(
        "[s1] load_max_slice_bytes={}",
        horndb_storage::loader::max_slice_bytes()
    );
    println!(
        "[s1] verdict_acceptance2={}",
        verdict(import_seconds <= IMPORT_TARGET_SECONDS)
    );

    // --- acceptance 3: fully-warm footprint ---
    let footprint = store.report_footprint();
    println!("[s1] footprint_bytes={}", footprint.bytes_estimated);
    println!(
        "[s1] footprint_bytes_per_triple={:.3}",
        footprint.bytes_per_triple
    );

    // `report_footprint` totals the store's own column/dictionary
    // accounting. `/proc/self/status` reports what the OS actually mapped
    // into this process — the number that answers "would this box hold
    // it" — and the two can differ (allocator overhead, fragmentation,
    // anything the store's own accounting misses).
    let (peak_rss_bytes, rss_bytes) = read_proc_self_rss();
    println!("[s1] peak_rss_bytes={peak_rss_bytes}");
    println!("[s1] rss_bytes={rss_bytes}");
    println!(
        "[s1] verdict_acceptance3={}",
        verdict(footprint.bytes_estimated as f64 <= FOOTPRINT_TARGET_BYTES)
    );

    // --- acceptance 4, half 1: rdf:type partition scan ---
    let rdf_type = Term::NamedNode(NamedNode::new(RDF_TYPE_IRI).expect("valid IRI"));
    let rdf_type_id = store.dictionary().get(&rdf_type);
    let (rdf_type_rows, rdf_type_scan_seconds, rdf_type_scan_bytes) = match rdf_type_id {
        Some(id) => {
            let pin = store.pin();
            pin.with_predicate(DEFAULT_GRAPH, id, scan_rdf_type_partition)
                .unwrap_or((0, 0.0, 0))
        }
        // A corpus with no rdf:type triples at all (e.g. a smoke-test
        // fixture) — report zeros rather than panic; this only makes the
        // acceptance-4 verdict a FAIL, which is correct for that corpus.
        None => (0, 0.0, 0),
    };
    let rdf_type_scan_gb_per_s = gb_per_sec(rdf_type_scan_bytes, rdf_type_scan_seconds);
    println!("[s1] rdf_type_rows={rdf_type_rows}");
    println!("[s1] rdf_type_scan_seconds={rdf_type_scan_seconds:.6}");
    println!("[s1] rdf_type_scan_bytes={rdf_type_scan_bytes}");
    println!("[s1] rdf_type_scan_gb_per_s={rdf_type_scan_gb_per_s:.3}");

    // The store can be tens of GB (up to the 55 GB target) and the host has
    // no swap. Drop it before allocating the Triad arrays so the two never
    // coexist at their peak size.
    drop(store);

    // --- acceptance 4, half 2: STREAM Triad on this host, this run ---
    let (triad_1t_gb_per_s, triad_nt_gb_per_s) = run_triad();
    println!("[s1] triad_1t_gb_per_s={triad_1t_gb_per_s:.3}");
    println!("[s1] triad_nt_gb_per_s={triad_nt_gb_per_s:.3}");

    let scan_over_triad_nt = rdf_type_scan_gb_per_s / triad_nt_gb_per_s;
    let scan_over_triad_1t = rdf_type_scan_gb_per_s / triad_1t_gb_per_s;
    println!("[s1] scan_over_triad_nt={scan_over_triad_nt:.4}");
    println!("[s1] scan_over_triad_1t={scan_over_triad_1t:.4}");
    println!(
        "[s1] verdict_acceptance4={}",
        if rdf_type_rows == 0 {
            "NO-DATA"
        } else {
            verdict(scan_over_triad_nt >= SCAN_OVER_TRIAD_TARGET)
        }
    );
}

/// Best-of-5 `subjects_with_object` scan over the `rdf:type` partition,
/// same shape as `partition_scan.rs`'s SPEC-12 bench but on the real corpus
/// instead of a synthetic one. Any object id scans the whole object column
/// — `horndb_simd::filter_indices_eq` is a full pass regardless of hit
/// count — so the choice of probe object does not change the bytes moved;
/// the first row's object is used so the call looks like a real point
/// lookup rather than a synthetic probe.
fn scan_rdf_type_partition(part: &horndb_storage::Partition) -> (u64, f64, u64) {
    let Some(warm) = part.as_warm() else {
        // A cold rdf:type partition right after import would be surprising,
        // but report zero rather than panic.
        return (0, 0.0, 0);
    };
    let rows = warm.len();
    let scan_bytes = (rows * std::mem::size_of::<u64>()) as u64;
    let probe_object = if rows > 0 { warm.objects().value(0) } else { 0 };

    let mut best = Duration::MAX;
    for _ in 0..5 {
        let start = Instant::now();
        let hits = std::hint::black_box(warm.subjects_with_object(probe_object));
        let elapsed = start.elapsed();
        std::hint::black_box(&hits);
        best = best.min(elapsed);
    }
    (rows as u64, best.as_secs_f64(), scan_bytes)
}

/// Zero bytes means the scan never ran, not infinite bandwidth. Returning
/// `INFINITY` here made acceptance 4 report PASS on a corpus with no
/// `rdf:type` triples at all, which is the worst possible answer: a green
/// verdict from a measurement that did not happen.
fn gb_per_sec(bytes: u64, seconds: f64) -> f64 {
    if bytes == 0 || seconds <= 0.0 {
        0.0
    } else {
        bytes as f64 / 1e9 / seconds
    }
}

fn verdict(pass: bool) -> &'static str {
    if pass {
        "PASS"
    } else {
        "FAIL"
    }
}

fn required_env_path(var: &str) -> PathBuf {
    let Ok(raw) = std::env::var(var) else {
        eprintln!("{var} is required (path to an N-Triples file to import); aborting.");
        std::process::exit(1);
    };
    let path = PathBuf::from(raw);
    if !path.is_file() {
        eprintln!("{var}={} does not exist or is not a file", path.display());
        std::process::exit(1);
    }
    path
}

fn optional_env_path(var: &str) -> Option<PathBuf> {
    std::env::var(var).ok().map(PathBuf::from)
}

/// `VmHWM` (peak resident set) and `VmRSS` (current resident set) from
/// `/proc/self/status`, in bytes. Linux-only — this bench only ever runs on
/// hornbench.
fn read_proc_self_rss() -> (u64, u64) {
    let status = std::fs::read_to_string("/proc/self/status").expect("read /proc/self/status");
    let mut peak_kb = 0u64;
    let mut rss_kb = 0u64;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            peak_kb = parse_kb_field(rest);
        } else if let Some(rest) = line.strip_prefix("VmRSS:") {
            rss_kb = parse_kb_field(rest);
        }
    }
    (peak_kb * 1024, rss_kb * 1024)
}

fn parse_kb_field(field: &str) -> u64 {
    field
        .trim()
        .trim_end_matches("kB")
        .trim()
        .parse()
        .unwrap_or(0)
}

/// STREAM Triad (`a[i] = b[i] + scalar * c[i]`) over three `f64` arrays big
/// enough to blow past cache — default 1 GiB each (override with
/// `TRIAD_BYTES` for a smaller smoke check), so 3 GiB total, well inside the
/// "don't allocate more than ~4 GiB" budget. Measured single-threaded and
/// across every core (best of 3 each), right here rather than taken from an
/// old ad hoc number, so the acceptance-4 ratio is reproducible on the same
/// host in the same run.
fn run_triad() -> (f64, f64) {
    let bytes_per_array = std::env::var("TRIAD_BYTES")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(1 << 30);
    let n = bytes_per_array / std::mem::size_of::<f64>();

    let mut a = vec![0.0f64; n];
    let b = vec![1.0f64; n];
    let c = vec![2.0f64; n];
    let scalar = 3.0f64;

    // STREAM convention: 2 reads + 1 write per element, 8 bytes each.
    let bytes_moved = (n * 24) as u64;

    let one_thread = best_of(3, || {
        let start = Instant::now();
        for i in 0..n {
            a[i] = b[i] + scalar * c[i];
        }
        std::hint::black_box(&a);
        start.elapsed()
    });

    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let chunk = n.div_ceil(threads);
    let n_thread = best_of(3, || {
        let start = Instant::now();
        std::thread::scope(|scope| {
            for ((a_c, b_c), c_c) in a
                .chunks_mut(chunk)
                .zip(b.chunks(chunk))
                .zip(c.chunks(chunk))
            {
                scope.spawn(move || {
                    for i in 0..a_c.len() {
                        a_c[i] = b_c[i] + scalar * c_c[i];
                    }
                });
            }
        });
        std::hint::black_box(&a);
        start.elapsed()
    });

    (
        gb_per_sec(bytes_moved, one_thread.as_secs_f64()),
        gb_per_sec(bytes_moved, n_thread.as_secs_f64()),
    )
}

fn best_of(reps: u32, mut f: impl FnMut() -> Duration) -> Duration {
    (0..reps).map(|_| f()).min().expect("reps > 0")
}
