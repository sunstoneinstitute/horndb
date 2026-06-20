//! Scale smoke for the storage/WCOJ-backed SPARQL backend (`HornBackend`,
//! issue #67) at the top trainmarks scale (~10M triples).
//!
//! This is the "does horn.rs survive the top trainmarks scale?" probe, NOT a
//! tracked criterion bench. It mirrors the real serve.rs load path exactly:
//! build one big `Vec<(s,p,o)>` and call `insert_oxrdf_batch` once, then run a
//! handful of representative SPARQL queries through the real
//! `api::execute_query` pipeline (parse -> translate -> plan -> WCOJ).
//!
//! `#[ignore]` so it never runs in the normal suite. Run it explicitly:
//!
//! ```text
//! HORN_LOAD_N=2000000 cargo test -p horndb-sparql --release \
//!     --test horn_load_hammer -- --ignored --nocapture
//! ```
//!
//! `HORN_LOAD_N` is the entity count; the dataset is ~5 triples/entity, so
//! 2,000,000 entities ≈ 10M triples. Default is 2,000,000.

use horndb_sparql::api::{execute_query, QueryAnswer};
use horndb_sparql::exec::horn::HornBackend;
use oxrdf::{Literal, NamedNode, Term as OxTerm};
use std::time::Instant;

/// Current resident set size (VmRSS) in GB, from /proc/self/status.
fn rss_gb() -> f64 {
    proc_field("VmRSS:")
}
/// Peak resident set size (VmHWM) in GB.
fn peak_rss_gb() -> f64 {
    proc_field("VmHWM:")
}
fn proc_field(field: &str) -> f64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines().find(|l| l.starts_with(field)).and_then(|l| {
                l.split_whitespace()
                    .nth(1)
                    .and_then(|v| v.parse::<f64>().ok())
            })
        })
        .map(|kb| kb / 1024.0 / 1024.0)
        .unwrap_or(0.0)
}

fn iri(s: &str) -> OxTerm {
    OxTerm::NamedNode(NamedNode::new_unchecked(s))
}

fn lit(s: String) -> OxTerm {
    OxTerm::Literal(Literal::new_simple_literal(s))
}

#[test]
#[ignore = "scale smoke; run explicitly with --ignored"]
fn hammer_horn_load() {
    let n: usize = std::env::var("HORN_LOAD_N")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2_000_000);

    const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
    const STATION: &str = "http://trains.ex/Station";
    const CONNECTS: &str = "http://trains.ex/connectsTo";
    const NAME: &str = "http://trains.ex/name";
    const LINE: &str = "http://trains.ex/line";
    const ZONE: &str = "http://trains.ex/zone";

    // --- generate (in-process; isolates horn.rs from the N-Triples parser) ---
    let t_gen = Instant::now();
    let mut batch: Vec<(OxTerm, OxTerm, OxTerm)> = Vec::with_capacity(n.saturating_mul(5));
    let (ty, st, co, na, li, zo) = (
        iri(RDF_TYPE),
        iri(STATION),
        iri(CONNECTS),
        iri(NAME),
        iri(LINE),
        iri(ZONE),
    );
    for i in 0..n {
        let s = iri(&format!("http://trains.ex/s{i}"));
        batch.push((s.clone(), ty.clone(), st.clone()));
        batch.push((s.clone(), na.clone(), lit(format!("Station {i}"))));
        batch.push((
            s.clone(),
            co.clone(),
            iri(&format!("http://trains.ex/s{}", (i + 1) % n)),
        ));
        batch.push((
            s.clone(),
            li.clone(),
            iri(&format!("http://trains.ex/line{}", i % 50)),
        ));
        batch.push((s, zo.clone(), lit(format!("Z{}", i % 10))));
    }
    let total = batch.len();
    let per = |gb: f64| (gb * 1024.0 * 1024.0 * 1024.0) / total as f64;
    eprintln!(
        "[gen]  {n} entities -> {total} triples in {:.2}s | RSS {:.2} GB ({:.0} B/triple, input batch only)",
        t_gen.elapsed().as_secs_f64(),
        rss_gb(),
        per(rss_gb()),
    );

    // --- load (single batch, exactly like serve.rs load_file) ---
    let mut backend = HornBackend::new();
    let t_load = Instant::now();
    let loaded = backend.insert_oxrdf_batch(batch).expect("bulk load failed");
    let load_s = t_load.elapsed().as_secs_f64();
    eprintln!(
        "[load] {loaded} live triples in {load_s:.2}s ({:.2} M triples/s) | RSS {:.2} GB ({:.0} B/triple, batch freed: storage+dict+stored_keys)",
        (loaded as f64 / 1e6) / load_s,
        rss_gb(),
        per(rss_gb()),
    );
    assert_eq!(backend.len(), total as u64);

    // --- queries through the real pipeline ---
    // The first query lazily builds the WCOJ snapshot, so its time bundles the
    // snapshot build; later queries are warm.
    let queries: &[(&str, &str)] = &[
        (
            "Q0 warmup ASK (builds snapshot)",
            "ASK { <http://trains.ex/s0> <http://trains.ex/connectsTo> ?o }",
        ),
        (
            "Q1 full scan + COUNT (connectsTo)",
            "SELECT (COUNT(?o) AS ?c) WHERE { ?s <http://trains.ex/connectsTo> ?o }",
        ),
        (
            "Q2 selective 2-hop join (bound start)",
            "SELECT ?o WHERE { <http://trains.ex/s0> <http://trains.ex/connectsTo> ?m . \
             ?m <http://trains.ex/connectsTo> ?o }",
        ),
        (
            "Q3 GROUP BY line + COUNT",
            "SELECT ?line (COUNT(?s) AS ?c) WHERE { ?s <http://trains.ex/line> ?line } \
             GROUP BY ?line",
        ),
        (
            "Q4 2-pattern subject join + LIMIT",
            "SELECT ?s ?z WHERE { ?s <http://trains.ex/connectsTo> ?o . \
             ?s <http://trains.ex/zone> ?z } LIMIT 10",
        ),
        (
            "Q5 typed star join + LIMIT",
            "SELECT ?s ?name WHERE { ?s a <http://trains.ex/Station> . \
             ?s <http://trains.ex/name> ?name } LIMIT 10",
        ),
    ];

    for (label, q) in queries {
        let t = Instant::now();
        match execute_query(q, &backend) {
            Ok(ans) => {
                let dt = t.elapsed().as_secs_f64();
                let (kind, rows) = match &ans {
                    QueryAnswer::Solutions { rows, .. } => ("rows", rows.len()),
                    QueryAnswer::Boolean(b) => ("ask", *b as usize),
                    QueryAnswer::Triples(v) => ("triples", v.len()),
                    QueryAnswer::Explanation { .. } => ("explain", 0),
                };
                eprintln!(
                    "[query] {label}: {dt:.3}s ({rows} {kind}) | RSS {:.2} GB",
                    rss_gb()
                );
            }
            Err(e) => eprintln!("[query] {label}: ERROR {e}"),
        }
    }

    // Peak resident set size for the whole process (Linux): VmHWM in
    // /proc/self/status, reported by the kernel in kB.
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        if let Some(line) = status.lines().find(|l| l.starts_with("VmHWM:")) {
            let kb: f64 = line
                .split_whitespace()
                .nth(1)
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.0);
            eprintln!("[mem]  peak RSS (VmHWM): {:.2} GB", kb / 1024.0 / 1024.0);
        }
    }
    eprintln!(
        "[mem]  peak RSS {:.2} GB = {:.0} B/triple over {total} triples",
        peak_rss_gb(),
        per(peak_rss_gb()),
    );
}
