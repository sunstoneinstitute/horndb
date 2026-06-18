//! Bench: Fork-A best-confidence crosswalk closure (TASKS.md #12).
//!
//! The #12 "done-when" for Fork A is a bench against a GTIO/SKOS-shaped
//! crosswalk graph, with the #11 readiness metrics populated for it. This bench
//! builds a synthetic-but-structurally-faithful crosswalk and measures:
//!
//! 1. **Fork-A best-confidence closure** (`(max, ×)`, built-in semiring) — the
//!    operation that replaces an unbounded SPARQL property-path crawl with one
//!    matrix closure.
//! 2. The **boolean reachability baseline** on the *same shape*, so the cost of
//!    carrying a scalar confidence is visible (this is the #11 valued-vs-boolean
//!    split, measured here on the crosswalk shape rather than a bare n-chain).
//!
//! Run: `cargo bench -p horndb-closure --bench crosswalk`.

use std::time::Duration;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use std::collections::HashMap;

use horndb_closure::closure::transitive::transitive_closure;
use horndb_closure::crosswalk::{CrosswalkEdge, CrosswalkGraph};
use horndb_closure::grb::{init_once, BoolMatrix, ValuedMatrix};
use horndb_closure::metrics::{valued_transitive_closure, ValuedKernel};

/// Build a GTIO/SKOS-shaped crosswalk graph with `vocabs` source vocabularies,
/// each holding a `depth`-deep `skos:broader` concept ladder, cross-linked into
/// the next vocabulary by `skos:exactMatch`/`closeMatch` edges of varying
/// confidence. The result is a layered DAG — the realistic shape of a
/// multi-vocabulary crosswalk (rdf-registry #10).
///
/// Concept `(v, d)` gets dictionary ID `v * 1_000_000 + d` so IDs are sparse
/// (exercising the dense renumbering) yet deterministic.
fn crosswalk_edges(vocabs: u64, depth: u64) -> Vec<CrosswalkEdge> {
    let id = |v: u64, d: u64| v * 1_000_000 + d;
    let mut edges = Vec::new();
    for v in 0..vocabs {
        // Intra-vocab `broader` ladder: (v,0) -> (v,1) -> ... -> (v,depth-1),
        // high-confidence structural edges.
        for d in 0..depth - 1 {
            edges.push(CrosswalkEdge {
                src: id(v, d),
                dst: id(v, d + 1),
                confidence: 0.95,
            });
        }
        // Cross-vocab mappings into the next vocabulary: an exactMatch at the
        // same depth (strong) plus a weaker closeMatch one level down, giving
        // competing paths the best-confidence closure must arbitrate.
        if v + 1 < vocabs {
            for d in 0..depth {
                edges.push(CrosswalkEdge {
                    src: id(v, d),
                    dst: id(v + 1, d),
                    confidence: 0.90,
                });
                if d + 1 < depth {
                    edges.push(CrosswalkEdge {
                        src: id(v, d),
                        dst: id(v + 1, d + 1),
                        confidence: 0.60,
                    });
                }
            }
        }
    }
    edges
}

/// Dense matrix forms of a crosswalk, sharing one renumbering so the valued and
/// boolean legs run on an identical matrix shape.
struct DenseForms {
    /// Matrix dimension `N` (distinct concepts).
    n: u64,
    /// `(row, col, confidence)` for the valued `(max,×)` matrix.
    valued: Vec<(u64, u64, f64)>,
    /// `(row, col)` for the boolean reachability matrix.
    boolean: Vec<(u64, u64)>,
}

/// Densely renumber the crosswalk's dictionary IDs into `0..N` (the same
/// renumbering `CrosswalkGraph` does internally).
fn dense_forms(edges: &[CrosswalkEdge]) -> DenseForms {
    let mut dense: HashMap<u64, u64> = HashMap::new();
    let mut intern = |id: u64| -> u64 {
        let next = dense.len() as u64;
        *dense.entry(id).or_insert(next)
    };
    let mut valued = Vec::with_capacity(edges.len());
    let mut boolean = Vec::with_capacity(edges.len());
    for e in edges {
        let r = intern(e.src);
        let c = intern(e.dst);
        valued.push((r, c, e.confidence));
        boolean.push((r, c));
    }
    DenseForms {
        n: dense.len() as u64,
        valued,
        boolean,
    }
}

fn bench_crosswalk_closure(c: &mut Criterion) {
    init_once().unwrap();
    let mut group = c.benchmark_group("crosswalk_best_confidence");
    group.measurement_time(Duration::from_secs(12));

    // (vocabs, depth) shapes — a small registry-scale crosswalk and a larger
    // one. depth*vocabs distinct concepts.
    for &(vocabs, depth) in &[(8u64, 32u64), (16, 64)] {
        let edges = crosswalk_edges(vocabs, depth);
        group.throughput(Throughput::Elements(edges.len() as u64));

        let label = format!("v{vocabs}_d{depth}");
        let DenseForms {
            n,
            valued: valued_edges,
            boolean: bool_edges,
        } = dense_forms(&edges);

        // CARRIER-COST COMPARISON — the matrix is built *outside* `b.iter`
        // (matrix construction / dictionary renumbering is not the workload of
        // interest and is not timed). Both legs then do the same timed work on
        // the prebuilt matrix: close to fixpoint, read `nvals()`. The *only*
        // difference is the carrier (FP64 `(max,×)` vs Boolean `(∨,∧)`), so the
        // ratio is an apples-to-apples scalar-confidence carrier cost. Neither
        // leg extracts result tuples.
        group.bench_with_input(
            BenchmarkId::new("valued_closure", &label),
            &valued_edges,
            |b, valued_edges| {
                let m = ValuedMatrix::from_weighted_edges(n, valued_edges).unwrap();
                b.iter(|| {
                    let (star, _metrics) =
                        valued_transitive_closure(&m, ValuedKernel::Builtin).unwrap();
                    assert!(star.nvals().unwrap() > 0);
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("boolean_reach", &label),
            &bool_edges,
            |b, bool_edges| {
                let m = BoolMatrix::from_edges(n, bool_edges).unwrap();
                b.iter(|| {
                    let star = transitive_closure(&m).unwrap();
                    assert!(star.nvals().unwrap() > 0);
                });
            },
        );

        // END-TO-END Fork-A entry point — the full *query* cost on a prebuilt
        // graph: closure *plus* extracting + mapping every best-confidence pair
        // back to dictionary IDs (`CrosswalkGraph::best_confidence_closure`).
        // Graph construction (`from_edges`) is outside `b.iter`, same as the
        // matrices above. Reported separately so it is not confused with the
        // pure carrier-cost ratio above (the extraction is O(result nnz) and
        // only this leg pays it).
        group.bench_with_input(
            BenchmarkId::new("valued_end_to_end", &label),
            &edges,
            |b, edges| {
                let g = CrosswalkGraph::from_edges(edges).unwrap();
                b.iter(|| {
                    let (pairs, _metrics) = g.best_confidence_closure().unwrap();
                    assert!(!pairs.is_empty());
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_crosswalk_closure);
criterion_main!(benches);
