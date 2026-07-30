//! SPEC-28 phase 2 (#265) acceptance #4: `scan_graph`/`graph_len` cost is
//! O(graph), not O(store). Two corpora — 1,000 named graphs and 2,000 named
//! graphs, 1,000 triples each — both also hold one 10-triple "small" graph.
//! `scan_graph/small_graph_in_1k_store` and `scan_graph/small_graph_in_2k_store`
//! time the same 10-triple scan against stores of different size; if the
//! access path is graph-scoped the two numbers must land within noise of
//! each other despite the store doubling. `graph_len` gets the same pairing
//! for the count-only path.
//!
//! Also prints `TierStats.bytes_estimated / total quads` for each corpus
//! from setup (not timed) — the SPEC-02 NF1 partition-overhead signal for
//! "thousands of small graphs" (budget: <=50 B/triple). Local smoke-check
//! only; the recorded hornbench run + docs/benchmarks.md entry is a separate
//! step (see PLAN-28-02 Task 5).

use criterion::{criterion_group, criterion_main, Criterion};
use horndb_storage::term::GraphId;
use horndb_storage::Store;
use oxrdf::{NamedNode, Term};

const PREDICATES_PER_GRAPH: usize = 5;
const TRIPLES_PER_GRAPH: usize = 1_000;
const SMALL_GRAPH_TRIPLES: usize = 10;
const INSERT_BATCH: usize = 65_000;

fn iri(s: String) -> Term {
    Term::NamedNode(NamedNode::new(s).unwrap())
}

/// One store holding `num_graphs` "normal" 1,000-triple graphs plus one
/// 10-triple "small" graph. Returns the store and the small graph's id.
fn build_corpus(num_graphs: usize) -> (Store, GraphId) {
    let store = Store::in_memory();

    let mut batch: Vec<(GraphId, Term, Term, Term)> = Vec::with_capacity(INSERT_BATCH);
    let flush = |batch: &mut Vec<(GraphId, Term, Term, Term)>| {
        if !batch.is_empty() {
            store.insert_quads(batch).expect("insert batch");
            batch.clear();
        }
    };

    for gi in 0..num_graphs {
        let g = store
            .intern_graph_uri(&iri(format!("http://ex/graph{gi}")))
            .expect("intern graph");
        for ti in 0..TRIPLES_PER_GRAPH {
            let p = ti % PREDICATES_PER_GRAPH;
            batch.push((
                g,
                iri(format!("http://ex/s{gi}_{ti}")),
                iri(format!("http://ex/p{p}")),
                iri(format!("http://ex/o{gi}_{ti}")),
            ));
            if batch.len() >= INSERT_BATCH {
                flush(&mut batch);
            }
        }
    }
    flush(&mut batch);

    let small_graph = store
        .intern_graph_uri(&iri("http://ex/small-graph".to_string()))
        .expect("intern small graph");
    for ti in 0..SMALL_GRAPH_TRIPLES {
        let p = ti % PREDICATES_PER_GRAPH;
        batch.push((
            small_graph,
            iri(format!("http://ex/small_s{ti}")),
            iri(format!("http://ex/p{p}")),
            iri(format!("http://ex/small_o{ti}")),
        ));
    }
    flush(&mut batch);

    let total_quads = (num_graphs * TRIPLES_PER_GRAPH + SMALL_GRAPH_TRIPLES) as u64;
    let stats = store.stats();
    let bytes_per_quad = stats.bytes_estimated as f64 / total_quads as f64;
    eprintln!(
        "graph_scan corpus: {num_graphs} graphs + 1 small graph, {total_quads} quads, \
         bytes_estimated={}, {bytes_per_quad:.2} B/quad (SPEC-02 NF1 budget: <=50 B/triple)",
        stats.bytes_estimated,
    );

    (store, small_graph)
}

fn bench_scan_graph(c: &mut Criterion) {
    let mut group = c.benchmark_group("scan_graph");

    // One corpus alive at a time keeps peak memory to ~1.5-3M quads instead
    // of ~3M+6M held simultaneously: the 1k corpus and its store are dropped
    // before the 2k corpus is built.
    {
        let (store, small_graph) = build_corpus(1_000);
        let snapshot = store.snapshot();
        group.bench_function("small_graph_in_1k_store", |b| {
            b.iter(|| std::hint::black_box(snapshot.scan_graph(small_graph).unwrap()));
        });
    }
    {
        let (store, small_graph) = build_corpus(2_000);
        let snapshot = store.snapshot();
        group.bench_function("small_graph_in_2k_store", |b| {
            b.iter(|| std::hint::black_box(snapshot.scan_graph(small_graph).unwrap()));
        });
    }

    group.finish();
}

fn bench_graph_len(c: &mut Criterion) {
    let mut group = c.benchmark_group("graph_len");

    let (store, small_graph) = build_corpus(1_000);
    let snapshot = store.snapshot();
    group.bench_function("small_graph_in_1k_store", |b| {
        b.iter(|| std::hint::black_box(snapshot.graph_len(small_graph)));
    });

    group.finish();
}

criterion_group!(benches, bench_scan_graph, bench_graph_len);
criterion_main!(benches);
