use criterion::{criterion_group, criterion_main, Criterion};
use horndb_metrics::labels::{QueryKind, QueryKindLabel};

fn counter_inc(c: &mut Criterion) {
    let m = horndb_metrics::metrics();
    let handle = m
        .sparql
        .query_total
        .get_or_create(&QueryKindLabel {
            kind: QueryKind::Select,
        })
        .clone();
    c.bench_function("counter_inc_resolved", |b| {
        b.iter(|| handle.inc());
    });
}

criterion_group!(benches, counter_inc);
criterion_main!(benches);
