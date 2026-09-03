//! HDB-123: does a small write make the next query pay a full statistics build?
//!
//! `plan_after_batch` alternates a 1k-quad batch with a 4-way star join and
//! times **only the query**: the batch before it is untimed setup. Planning is
//! what reads `SnapshotStats`, and it is reached through `EXPLAIN` (the only
//! caller of `cardinality_estimate`). Before this change every batch dropped
//! the cached summary, so the timed half included an `O(store)` rebuild.
//!
//! `plan_only` is the same query against an untouched store — the floor.
//!
//! `plan_after_batch` is deliberately bimodal, so read its **lower bound**, not
//! its mean: almost every query takes the merged-summary path and lands within
//! a small multiple of `plan_only`, while roughly one batch in twenty spends
//! the drift bound and pays a full rebuild. A `plan_after_batch` whose *lower*
//! bound tracks store size means the merge path is broken and every query is
//! rebuilding again.
//!
//! Each iteration retires the previous batch as well as inserting the next, so
//! the store stays the same size and the series is steady-state rather than a
//! graph growing under the benchmark.
//!
//! Recorded numbers come from `hornbench`, never a laptop — see CLAUDE.md.

use std::hint::black_box;
use std::time::{Duration, Instant};

use criterion::{criterion_group, criterion_main, Criterion};
use horndb_sparql::api::execute_query;
use horndb_sparql::exec::horn::HornBackend;
use horndb_sparql::parser::parse_update;
use horndb_sparql::update::apply_update;

const BATCH: usize = 1_000;
/// Distinct batches cycled through. One is live at a time, so cycling re-uses
/// a batch only after it has been retired — and it keeps the update text
/// pre-built, off the setup path.
const ROTATION: usize = 32;
/// Store size. A batch pair (one retired, one inserted) is 2k rows against
/// this, so `SnapshotStats`'s drift bound (1/10 of the graph) forces a full
/// rebuild about every twenty batches. That ratio is the whole point: the
/// amortized rebuild cost per batch is proportional to the *delta*, not to the
/// store, so the gap to `plan_only` shrinks as the graph grows — the opposite
/// of the drop-the-cache behaviour this replaced.
const SEED_ROWS: usize = 400_000;

/// A 4-way star join: four predicates sharing one subject variable.
const QUERY: &str = "EXPLAIN SELECT ?s WHERE {
    ?s <http://ex/p0> ?a .
    ?s <http://ex/p1> ?b .
    ?s <http://ex/p2> ?c .
    ?s <http://ex/p3> ?d .
}";

fn update(store: &mut HornBackend, text: &str) {
    apply_update(&parse_update(text).unwrap(), store).unwrap();
}

/// `(INSERT DATA, DELETE DATA)` text for each rotation slot. Slots are
/// disjoint, so retiring one never touches another.
fn rotation() -> Vec<(String, String)> {
    (0..ROTATION)
        .map(|round| {
            let mut body = String::new();
            for i in 0..BATCH {
                let n = round * BATCH + i;
                body.push_str(&format!(
                    " <http://ex/n{n}> <http://ex/p{}> <http://ex/o{}> .",
                    n % 4,
                    n % 97
                ));
            }
            (
                format!("INSERT DATA {{{body}}}"),
                format!("DELETE DATA {{{body}}}"),
            )
        })
        .collect()
}

/// A store where every subject carries all four star predicates. Seeded in
/// large batches — one `insert_triple` per row would dominate setup.
fn seeded() -> HornBackend {
    let mut b = HornBackend::new();
    let mut body = String::new();
    for i in 0..SEED_ROWS / 4 {
        for p in 0..4 {
            body.push_str(&format!(
                " <http://ex/s{i}> <http://ex/p{p}> <http://ex/o{}> .",
                i % 97
            ));
        }
        if i % 2_000 == 1_999 {
            update(&mut b, &format!("INSERT DATA {{{body}}}"));
            body.clear();
        }
    }
    if !body.is_empty() {
        update(&mut b, &format!("INSERT DATA {{{body}}}"));
    }
    b
}

fn bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("stats_incremental");

    // Seeded once, outside the routine: criterion re-enters the routine
    // closure for every sample, so a store built inside it would be rebuilt
    // (and re-warmed) dozens of times and every sample would measure a cold
    // first query.
    let batches = rotation();
    let mut store = seeded();
    update(&mut store, &batches[0].0);
    let mut round = 1usize;
    // Warm the snapshot and its summary; the first query on a fresh store
    // builds both and is not what this measures.
    execute_query(QUERY, &store).unwrap();

    group.bench_function("plan_after_batch", |bencher| {
        bencher.iter_custom(|iters| {
            let mut elapsed = Duration::ZERO;
            for _ in 0..iters {
                // Untimed: retire the live batch, install the next one.
                update(&mut store, &batches[(round - 1) % ROTATION].1);
                update(&mut store, &batches[round % ROTATION].0);
                round += 1;

                let started = Instant::now();
                black_box(execute_query(QUERY, &store).unwrap());
                elapsed += started.elapsed();
            }
            elapsed
        });
    });
    drop(store);

    let baseline = seeded();
    execute_query(QUERY, &baseline).unwrap();
    group.bench_function("plan_only", |bencher| {
        bencher.iter(|| black_box(execute_query(QUERY, &baseline).unwrap()));
    });

    group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
