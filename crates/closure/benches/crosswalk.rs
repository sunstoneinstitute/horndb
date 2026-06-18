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

use horndb_closure::closure::transitive::transitive_closure;
use horndb_closure::crosswalk::{CrosswalkEdge, CrosswalkGraph};
use horndb_closure::grb::{init_once, BoolMatrix};

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

fn bench_crosswalk_closure(c: &mut Criterion) {
    init_once().unwrap();
    let mut group = c.benchmark_group("crosswalk_best_confidence");
    group.measurement_time(Duration::from_secs(12));

    // (vocabs, depth) shapes — a small registry-scale crosswalk and a larger
    // one. depth*vocabs distinct concepts.
    for &(vocabs, depth) in &[(8u64, 32u64), (16, 64)] {
        let edges = crosswalk_edges(vocabs, depth);
        let n_concepts = vocabs * depth;
        group.throughput(Throughput::Elements(edges.len() as u64));

        let label = format!("v{vocabs}_d{depth}");

        group.bench_with_input(
            BenchmarkId::new("valued_builtin", &label),
            &edges,
            |b, edges| {
                let g = CrosswalkGraph::from_edges(edges).unwrap();
                b.iter(|| {
                    let (pairs, _metrics) = g.best_confidence_closure().unwrap();
                    assert!(!pairs.is_empty());
                });
            },
        );

        // Boolean reachability on the SAME shape (drop the weights), so the
        // scalar-confidence carrier cost is visible apples-to-apples.
        group.bench_with_input(
            BenchmarkId::new("boolean_reach", &label),
            &edges,
            |b, edges| {
                // Densely renumber the same way the graph does, into 0..n_concepts.
                let mut ids: Vec<u64> = edges.iter().flat_map(|e| [e.src, e.dst]).collect();
                ids.sort_unstable();
                ids.dedup();
                let dense: std::collections::HashMap<u64, u64> = ids
                    .iter()
                    .enumerate()
                    .map(|(i, &id)| (id, i as u64))
                    .collect();
                let bool_edges: Vec<(u64, u64)> = edges
                    .iter()
                    .map(|e| (dense[&e.src], dense[&e.dst]))
                    .collect();
                let n = dense.len() as u64;
                let _ = n_concepts; // documented relationship, not asserted
                let m = BoolMatrix::from_edges(n, &bool_edges).unwrap();
                b.iter(|| {
                    let star = transitive_closure(&m).unwrap();
                    assert!(star.nvals().unwrap() > 0);
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_crosswalk_closure);
criterion_main!(benches);
