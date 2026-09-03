//! SPEC-29 P1 / PLAN-29-01 T7 — what a reasoning view costs.
//!
//! Four measurements, matching the three SPEC-29 rows in `docs/benchmarks.md`:
//!
//! | # | What | Where it lands |
//! |---|---|---|
//! | a | spine template build (close the vocabulary once) | context for b and c |
//! | b | one data graph changes → that view derived again | "single-graph update → view visible again", SPEC-06 NF1 ≤100 ms |
//! | c | spine changes → every view re-derived | "spine edit → all views converged" (P1 is linear in view count) |
//! | d | resident memory with every view clean | "steady-state memory, all views clean" |
//!
//! **Harness choice (T7 left it open).** (a) and (b) are criterion benches:
//! they are short and need repetition to mean anything. (c) and (d) are
//! one-shot measurements printed to stderr before criterion starts — a full
//! re-derive of every view is seconds to minutes, so criterion's warm-up plus
//! sample count would multiply the run time for no extra information, and (d)
//! is a process-wide gauge criterion cannot express at all. Same reason
//! `bench-trainmarks` prints its `[mem]` line rather than timing it.
//!
//! Corpus size is `VIEW_DERIV_GRAPHS` (default 1000, the "1k-graph corpus" the
//! benchmarks row names).
//!
//! Run: `cargo bench -p horndb-sparql --bench view_derivation`

use std::time::Instant;

use criterion::{criterion_group, Criterion};
use horndb_config::{Reasoning, Views};
use horndb_sparql::algebra::Term;
use horndb_sparql::exec::horn::HornBackend;
use horndb_sparql::exec::{AlgebraQuad, Store};
use horndb_sparql::reasoning::ViewManager;

/// Spine namespace. `cfg()` selects it as the shared vocabulary.
const VOCAB: &str = "https://ex.org/vocab/ont";
const DATA_NS: &str = "https://ex.org/data/g";
const SUB_CLASS_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";
const TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

/// Classes in the spine's `subClassOf` chain. Each instance in a data graph
/// therefore derives `DEPTH - 1` `rdf:type` triples through `cax-sco`, so a
/// view has real work to do rather than closing to nothing.
const DEPTH: usize = 20;
/// Instances per data graph.
const INSTANCES: usize = 10;

fn graphs() -> usize {
    std::env::var("VIEW_DERIV_GRAPHS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1000)
}

fn cfg() -> Reasoning {
    Reasoning {
        enabled: true,
        spine: vec!["https://ex.org/vocab/".to_string()],
        views: Views::default(),
        default_dataset_includes_inferred: false,
        ..Reasoning::default()
    }
}

fn quad(g: Option<&str>, s: &str, p: &str, o: &str) -> AlgebraQuad {
    (
        g.map(str::to_owned),
        Term::Iri(s.into()),
        Term::Iri(p.into()),
        Term::Iri(o.into()),
    )
}

/// The vocabulary spine: a `subClassOf` chain `C0 ⊑ C1 ⊑ … ⊑ C{DEPTH-1}`.
fn spine_quads() -> Vec<AlgebraQuad> {
    (0..DEPTH - 1)
        .map(|i| {
            quad(
                Some(VOCAB),
                &format!("https://ex.org/C{i}"),
                SUB_CLASS_OF,
                &format!("https://ex.org/C{}", i + 1),
            )
        })
        .collect()
}

/// One data graph: `INSTANCES` individuals typed at the bottom of the chain.
fn data_quads(g: usize) -> Vec<AlgebraQuad> {
    let name = format!("{DATA_NS}{g}");
    (0..INSTANCES)
        .map(|i| {
            quad(
                Some(&name),
                &format!("https://ex.org/data/i{g}_{i}"),
                TYPE,
                "https://ex.org/C0",
            )
        })
        .collect()
}

/// Store holding the spine plus `n` data graphs. Nothing is derived yet.
fn seeded(n: usize) -> HornBackend {
    let mut store = HornBackend::new();
    store.apply_quads(Vec::new(), spine_quads()).unwrap();
    for g in 0..n {
        store.apply_quads(Vec::new(), data_quads(g)).unwrap();
    }
    store
}

/// Resident set size in MiB, or `None` off Linux. Same `/proc` read as
/// `bench-trainmarks`' `[mem]` line.
fn rss_mib() -> Option<f64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let kb: f64 = status
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    Some(kb / 1024.0)
}

/// (c) spine edit → all views converged, and (d) steady-state memory.
///
/// Timed once per view count rather than sampled: a full re-derive is the
/// expensive operation this whole design is trying to avoid doing often, and
/// the row it feeds asks whether the cost is linear in view count, which two
/// points answer.
fn one_shot_measurements() {
    let n = graphs();
    eprintln!("[view_derivation] corpus: spine depth {DEPTH}, {INSTANCES} instances/graph");

    for count in [n / 4, n] {
        if count == 0 {
            continue;
        }
        let mut store = seeded(count);
        let mut mgr = ViewManager::new(&cfg());

        // Cold pass: build the spine template and derive every view.
        let t = Instant::now();
        let derived = mgr.run_until_clean(&mut store).unwrap();
        let cold = t.elapsed();

        // Touch the spine, which marks every view stale, then converge again.
        // This is the "spine edit" the row names, and it excludes the initial
        // load, so it is the re-derivation cost alone.
        store
            .apply_quads(
                Vec::new(),
                vec![quad(
                    Some(VOCAB),
                    "https://ex.org/CTop",
                    SUB_CLASS_OF,
                    "https://ex.org/C0",
                )],
            )
            .unwrap();
        let t = Instant::now();
        let redone = mgr.run_until_clean(&mut store).unwrap();
        let converge = t.elapsed();

        eprintln!(
            "[view_derivation] views={count} cold_derive={:.3}s ({derived} views) \
             spine_edit_converge={:.3}s ({redone} views) per_view={:.2}ms",
            cold.as_secs_f64(),
            converge.as_secs_f64(),
            converge.as_secs_f64() * 1000.0 / redone.max(1) as f64,
        );

        // (d) every view is clean at this point, so this is the steady state.
        if count == n {
            match rss_mib() {
                Some(rss) => eprintln!(
                    "[mem] view_derivation steady state: RSS {rss:.1} MiB over {count} clean views \
                     = {:.3} MiB/view",
                    rss / count as f64
                ),
                None => {
                    eprintln!("[mem] view_derivation steady state: RSS unavailable (not Linux)")
                }
            }
        }
    }
}

fn bench(c: &mut Criterion) {
    let n = graphs();
    let mut g = c.benchmark_group("view_derivation");
    // A pass over a 1k-graph corpus is far too slow for criterion's default
    // 100 samples.
    g.sample_size(10);

    // (a) Spine template build: a store holding only the vocabulary, so the
    // pass closes the spine and derives no view.
    g.bench_function("spine_build", |b| {
        b.iter_batched(
            || (seeded(0), ViewManager::new(&cfg())),
            |(mut store, mut mgr)| mgr.run_until_clean(&mut store).unwrap(),
            criterion::BatchSize::PerIteration,
        )
    });

    // (b) The SPEC-06 NF1 gate: one data graph changes, one view is derived
    // again. Setup brings the whole corpus to clean and is not timed; the
    // measured pass sees exactly one dirty view.
    g.bench_function("single_view_update", |b| {
        b.iter_batched(
            || {
                let mut store = seeded(n);
                let mut mgr = ViewManager::new(&cfg());
                mgr.run_until_clean(&mut store).unwrap();
                (store, mgr)
            },
            |(mut store, mut mgr)| {
                store
                    .apply_quads(
                        Vec::new(),
                        vec![quad(
                            Some(&format!("{DATA_NS}0")),
                            "https://ex.org/data/i_new",
                            TYPE,
                            "https://ex.org/C0",
                        )],
                    )
                    .unwrap();
                let derived = mgr.run_until_clean(&mut store).unwrap();
                assert_eq!(derived, 1, "exactly one view should be stale");
            },
            criterion::BatchSize::PerIteration,
        )
    });

    g.finish();
}

criterion_group!(benches, bench);

// Not `criterion_main!`: the one-shot measurements must run before criterion
// takes over, and criterion's generated main has no hook for that.
fn main() {
    one_shot_measurements();
    benches();
    Criterion::default().configure_from_args().final_summary();
}
