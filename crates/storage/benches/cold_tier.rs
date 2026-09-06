//! SPEC-25 S5 cold tier: footprint (SPEC-02 NF1, ≤ 6 bytes/triple amortised)
//! and scan read amplification (SPEC-02 NF4, ≤ 2x over a contiguous encoded
//! scan). Run on hornbench via `scripts/bench/cold-tier.sh`, which turns the
//! `[cold]` stderr lines below into `bench-out/SUMMARY.md`.
//!
//! Two stores are loaded from the same corpus: one left warm, one fully
//! demoted. Keeping both alive is what lets every measurement compare the two
//! tiers over identical rows, in either direction, at any point in the run.

use criterion::{criterion_group, criterion_main, Criterion};
use horndb_storage::loader::ntriples::load_ntriples_file;
use horndb_storage::{Ordering, Store, TermId, DEFAULT_GRAPH};
use oxrdf::{NamedNode, Term};
use std::path::PathBuf;
use std::time::Instant;

/// How many of the biggest predicates the amplification pass covers.
const TOP_N: usize = 3;
/// Repeats per hand-timed measurement; the fastest run is reported (least
/// noise from scheduling and page-cache warm-up).
const REPEATS: usize = 5;

fn iri(s: String) -> Term {
    Term::NamedNode(NamedNode::new(s).unwrap())
}

/// The synthetic LUBM-shaped corpus from `tests/snapshot_footprint.rs`: 10
/// universities x 20 departments x 50 graduate students, 4 triples each, with
/// courses and professors shared inside a department so the dictionary
/// amortises the way it does at real LUBM scale.
fn synthetic(store: &Store) {
    let base = "http://www.lehigh.edu/univ-bench";
    let type_p = iri(format!("{base}#type"));
    let advisor_p = iri(format!("{base}#advisor"));
    let member_p = iri(format!("{base}#memberOf"));
    let takes_p = iri(format!("{base}#takesCourse"));
    let grad = iri(format!("{base}#GraduateStudent"));

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
                triples.push((student.clone(), type_p.clone(), grad.clone()));
                triples.push((student.clone(), member_p.clone(), dept.clone()));
                triples.push((student.clone(), advisor_p.clone(), prof));
                triples.push((student.clone(), takes_p.clone(), course));
            }
        }
    }
    store.insert_triples(&triples).unwrap();
}

/// A real N-Triples corpus if `LUBM_NT` names one that exists (same variable
/// `benches/load_lubm.rs` reads), else the synthetic generator above. The
/// label is printed and lands in the summary: a bytes/triple number whose
/// corpus is ambiguous is worthless.
fn load_corpus() -> (Store, String) {
    let store = Store::in_memory();
    let path = std::env::var("LUBM_NT").ok().map(PathBuf::from);
    // Labels stay whitespace-free: the summary script parses these lines as
    // `key=value` pairs.
    match path {
        Some(p) if p.is_file() => {
            load_ntriples_file(&store, &p).expect("load LUBM_NT");
            let label = p.display().to_string();
            (store, label)
        }
        Some(p) => {
            eprintln!(
                "[cold] LUBM_NT={} does not exist — falling back to the synthetic corpus",
                p.display()
            );
            synthetic(&store);
            (store, "synthetic-lubm-shaped".to_string())
        }
        None => {
            synthetic(&store);
            (store, "synthetic-lubm-shaped".to_string())
        }
    }
}

/// Fastest of `REPEATS` runs, in nanoseconds.
fn best_ns(mut f: impl FnMut() -> u64) -> f64 {
    let mut best = f64::INFINITY;
    for _ in 0..REPEATS {
        let t = Instant::now();
        std::hint::black_box(f());
        best = best.min(t.elapsed().as_secs_f64());
    }
    best * 1e9
}

/// Full subject-major scan of one predicate at the store's current version.
fn scan_rows(store: &Store, p: TermId) -> u64 {
    let pin = store.pin();
    let at = pin.version();
    pin.with_predicate_uncounted(DEFAULT_GRAPH, p, |part| part.scan_at(at).count() as u64)
        .unwrap_or(0)
}

/// Full ordered materialisation of one predicate. `Ordering::Pos` is the
/// object-major axis: warm serves it from a materialised column, cold decodes
/// and sorts transiently (only the subject-major block is stored).
fn ordered_rows(store: &Store, p: TermId, ord: Ordering) -> u64 {
    let pin = store.pin();
    let at = pin.version();
    pin.with_predicate_uncounted(DEFAULT_GRAPH, p, |part| {
        part.ordered_at(ord, at).len() as u64
    })
    .unwrap_or(0)
}

/// One `[cold] ratio` line. `graded` marks the ratios the NF4 verdict is taken
/// over; an ungraded one is printed for context only.
fn emit_ratio(pred: &str, rows: u64, what: &str, base_ns: f64, cold_ns: f64, graded: bool) {
    eprintln!(
        "[cold] ratio pred={pred} rows={rows} what={} base_ns={base_ns:.0} cold_ns={cold_ns:.0} ratio={:.3} graded={}",
        what.replace(' ', "_"),
        cold_ns / base_ns.max(1.0),
        u8::from(graded),
    );
}

fn mapped_bytes(store: &Store, p: TermId) -> u64 {
    let pin = store.pin();
    pin.with_predicate_uncounted(DEFAULT_GRAPH, p, |part| part.estimated_bytes())
        .unwrap_or(0)
}

/// One measured predicate. The two stores intern independently, so the same
/// predicate has a `TermId` per store.
struct Pred {
    name: String,
    warm_id: TermId,
    cold_id: TermId,
    rows: u64,
}

fn bench_cold_tier(c: &mut Criterion) {
    let (warm, label) = load_corpus();
    let (cold, _) = load_corpus();
    let triples = warm.triple_count().max(1);

    // Footprint: warm total before any demotion, cold mapped bytes after.
    let warm_bytes = warm.stats().bytes_estimated;
    let demoted = cold.demote_all().expect("demote_all");
    let cold_bytes = cold.stats().bytes_cold;

    eprintln!("[cold] corpus label={label} triples={triples} demoted_partitions={demoted}");
    eprintln!(
        "[cold] footprint warm_bytes={warm_bytes} warm_bpt={:.3} cold_bytes={cold_bytes} cold_bpt={:.3}",
        warm_bytes as f64 / triples as f64,
        cold_bytes as f64 / triples as f64,
    );

    // Top predicates by row count, resolved back to ids for the read paths.
    // Each store has its own dictionary, and a parallel load need not intern
    // in the same order, so the id is looked up per store rather than shared.
    let top = warm
        .snapshot()
        .top_predicates(TOP_N)
        .expect("top predicates");
    let top: Vec<Pred> = top
        .into_iter()
        .filter_map(|(term, rows)| {
            Some(Pred {
                name: term.to_string(),
                warm_id: warm.dictionary().get(&term)?,
                cold_id: cold.dictionary().get(&term)?,
                rows,
            })
        })
        .collect();

    for pred in &top {
        let Pred {
            ref name,
            warm_id,
            cold_id,
            rows,
        } = *pred;
        // NF4 proper: the cold subject-major scan against the same scan over
        // the warm columns — the contiguous encoded scan the clause names.
        let warm_scan = best_ns(|| scan_rows(&warm, warm_id));
        let cold_scan = best_ns(|| scan_rows(&cold, cold_id));
        emit_ratio(name, rows, "scan_at cold/warm", warm_scan, cold_scan, true);

        // The object-major axis. Cold decodes and sorts on every call; the
        // honest baseline for that is its own subject-major decode over the
        // same file, so both sides read the same bytes the same way.
        let cold_obj = best_ns(|| ordered_rows(&cold, cold_id, Ordering::Pos));
        emit_ratio(
            name,
            rows,
            "ordered_at(Pos) cold/cold-scan",
            cold_scan,
            cold_obj,
            true,
        );

        // Warm vs cold on the same call, for reference only. A warm partition
        // materialises its object-major columns once (eagerly when hot, lazily
        // otherwise) and hands out `Arc` clones after that, so this ratio
        // measures a cache hit against a decode — not read amplification.
        let warm_obj = best_ns(|| ordered_rows(&warm, warm_id, Ordering::Pos));
        emit_ratio(
            name,
            rows,
            "ordered_at(Pos) cold/warm-cached",
            warm_obj,
            cold_obj,
            false,
        );

        // Structural amplification: bytes the cold scan touches (one forward
        // pass over the mapped file) against a contiguous encoded scan of the
        // same rows — the warm subject/object columns, two u64 per row.
        let mapped = mapped_bytes(&cold, cold_id);
        let encoded = rows * 2 * std::mem::size_of::<u64>() as u64;
        eprintln!(
            "[cold] bytes pred={name} rows={rows} mapped={mapped} encoded={encoded} amp={:.3}",
            mapped as f64 / encoded.max(1) as f64
        );
    }

    // Roundtrip cost for the largest predicate, for the durable-placement
    // follow-up. Timed as an alternating pair: a second promote in a row is a
    // no-op, so each direction must be measured with the partition in the
    // other state.
    if let Some(pred) = top.first() {
        let (name, id, rows) = (&pred.name, pred.cold_id, pred.rows);
        let (mut promote, mut demote) = (f64::INFINITY, f64::INFINITY);
        for _ in 0..REPEATS {
            let t = Instant::now();
            assert!(cold.promote(DEFAULT_GRAPH, id).expect("promote"));
            promote = promote.min(t.elapsed().as_secs_f64());
            let t = Instant::now();
            assert!(cold.demote(DEFAULT_GRAPH, id).expect("demote"));
            demote = demote.min(t.elapsed().as_secs_f64());
        }
        eprintln!(
            "[cold] roundtrip pred={name} rows={rows} promote_ms={:.3} demote_ms={:.3}",
            promote * 1e3,
            demote * 1e3
        );
    }

    // Criterion ids are `p0..p2` in the same order as the `[cold] scan` lines
    // above: an IRI contains slashes, which criterion turns into directories.
    let mut group = c.benchmark_group("cold_tier");
    group.sample_size(10);
    for (i, pred) in top.iter().enumerate() {
        let (warm_id, cold_id) = (pred.warm_id, pred.cold_id);
        group.bench_function(format!("scan_at/warm/p{i}"), |b| {
            b.iter(|| scan_rows(&warm, warm_id))
        });
        group.bench_function(format!("scan_at/cold/p{i}"), |b| {
            b.iter(|| scan_rows(&cold, cold_id))
        });
        group.bench_function(format!("ordered_pos/warm/p{i}"), |b| {
            b.iter(|| ordered_rows(&warm, warm_id, Ordering::Pos))
        });
        group.bench_function(format!("ordered_pos/cold/p{i}"), |b| {
            b.iter(|| ordered_rows(&cold, cold_id, Ordering::Pos))
        });
    }
    if let Some(pred) = top.first() {
        let id = pred.cold_id;
        group.bench_function("promote_demote_roundtrip/p0", |b| {
            b.iter(|| {
                cold.promote(DEFAULT_GRAPH, id).expect("promote");
                cold.demote(DEFAULT_GRAPH, id).expect("demote")
            })
        });
    }
    group.finish();
}

criterion_group!(benches, bench_cold_tier);
criterion_main!(benches);
