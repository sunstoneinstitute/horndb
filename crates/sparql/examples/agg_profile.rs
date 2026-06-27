//! Local diagnostic harness for aggregation-query throughput against the
//! production `HornBackend` (storage + WCOJ). NOT a recorded benchmark —
//! a smoke/ablation tool for the 12-vs-150 aggregation-qps investigation.
//!
//! Run: `cargo run -p horndb-sparql --release --example agg_profile -- [works]`
//!
//! Synthesises an SPB-ish "creative works" graph:
//!   <cwk_i> <type>  <Work>
//!   <cwk_i> <cat>   <cat_{i%CATS}>      (low-cardinality grouping key)
//!   <cwk_i> <about> <ent_{i%ENTS}>      (higher-cardinality, for DISTINCT)
//!   <cwk_i> <value> "n"^^xsd:integer    (numeric, for SUM/AVG)
//! and times representative aggregation queries, plus ablations that
//! isolate the per-row String-materialization tax from the WCOJ join.

use std::hint::black_box;
use std::time::Instant;

use horndb_sparql::algebra::Term;
use horndb_sparql::api::execute_query;
use horndb_sparql::api::QueryAnswer;
use horndb_sparql::exec::horn::HornBackend;

const CATS: usize = 50; // grouping-key cardinality
const ENTS: usize = 20_000; // about-target cardinality

fn iri(s: &str) -> Term {
    Term::Iri(s.into())
}
fn int_lit(n: usize) -> Term {
    Term::Literal(format!(
        "\"{n}\"^^<http://www.w3.org/2001/XMLSchema#integer>"
    ))
}

fn rows(ans: QueryAnswer) -> usize {
    match ans {
        QueryAnswer::Solutions { rows, .. } => rows.len(),
        other => panic!("expected solutions, got {other:?}"),
    }
}

fn time_query(label: &str, store: &HornBackend, q: &str, iters: u32) {
    // warm
    let _ = execute_query(q, store).unwrap();
    let t = Instant::now();
    let mut n = 0;
    for _ in 0..iters {
        n = rows(black_box(execute_query(black_box(q), store).unwrap()));
    }
    let elapsed = t.elapsed();
    let per = elapsed / iters;
    let qps = 1.0 / per.as_secs_f64();
    println!(
        "{label:<28} {per:>12.2?}/q  {qps:>9.1} qps   (out rows={n})",
        per = per
    );
}

fn main() {
    let works: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(100_000);

    eprint!("loading {works} works (~{} triples)... ", works * 4);
    let load = Instant::now();
    let mut store = HornBackend::new();
    let mut batch: Vec<(Term, Term, Term)> = Vec::with_capacity(works * 4);
    for i in 0..works {
        let s = iri(&format!("http://ex/cwk{i}"));
        batch.push((s.clone(), iri("http://ex/type"), iri("http://ex/Work")));
        batch.push((
            s.clone(),
            iri("http://ex/cat"),
            iri(&format!("http://ex/cat{}", i % CATS)),
        ));
        batch.push((
            s.clone(),
            iri("http://ex/about"),
            iri(&format!("http://ex/ent{}", i % ENTS)),
        ));
        batch.push((s, iri("http://ex/value"), int_lit(i % 1000)));
        if batch.len() >= 40_000 {
            store.insert_algebra_triples_bulk(std::mem::take(&mut batch));
        }
    }
    store.insert_algebra_triples_bulk(batch);
    eprintln!("done in {:?}, live={}", load.elapsed(), store.len());

    // Build the WCOJ snapshot once (the nightly is read-only after load, so
    // this cost is amortised across all queries — exclude it from per-query).
    let warm = Instant::now();
    let _ = execute_query("ASK { ?s <http://ex/type> <http://ex/Work> }", &store).unwrap();
    eprintln!("(snapshot warm took {:?})\n", warm.elapsed());

    println!("--- aggregation queries (production HornBackend) ---");
    time_query(
        "Q1 COUNT(*) all triples",
        &store,
        "SELECT (COUNT(*) AS ?c) WHERE { ?s ?p ?o }",
        20,
    );
    time_query(
        "Q2 GROUP BY cat COUNT",
        &store,
        "SELECT ?cat (COUNT(?s) AS ?c) WHERE { ?s <http://ex/cat> ?cat } GROUP BY ?cat",
        20,
    );
    time_query(
        "Q3 join type+cat GROUP BY",
        &store,
        "SELECT ?cat (COUNT(?s) AS ?c) WHERE { ?s <http://ex/type> <http://ex/Work> . ?s <http://ex/cat> ?cat } GROUP BY ?cat",
        20,
    );
    time_query(
        "Q4 SUM(value) GROUP BY cat",
        &store,
        "SELECT ?cat (SUM(?v) AS ?s) WHERE { ?w <http://ex/cat> ?cat . ?w <http://ex/value> ?v } GROUP BY ?cat",
        20,
    );
    time_query(
        "Q5 COUNT(DISTINCT ?e)",
        &store,
        "SELECT (COUNT(DISTINCT ?e) AS ?c) WHERE { ?s <http://ex/about> ?e }",
        20,
    );

    println!("\n--- ablation: materialization tax ---");
    // len() counts live triples WITHOUT materializing any String row.
    let t = Instant::now();
    let mut c = 0u64;
    for _ in 0..20 {
        c = black_box(store.len());
    }
    println!(
        "len() (no materialize)       {:>12.2?}/q  (count={c})",
        t.elapsed() / 20
    );
    println!(
        "  ^ compare to Q1: same count, but Q1 decodes every TermId -> String\n    Bindings row before counting. The ratio is the per-row String tax."
    );
}
