//! `bench-trainmarks` — run the DataTreehouse *trainmarks* RDF benchmark
//! against HornDB's storage/WCOJ SPARQL backend (`HornBackend`).
//!
//! Upstream: <https://github.com/DataTreehouse/trainmarks>. We mirror its
//! per-framework driver protocol so our numbers slot into the same report:
//!
//! For one scale (`medium` / `large` / `xlarge`) we time, in order:
//!   read_turtle, write_turtle, write_ntriples, read_ntriples,
//!   then queries q1..q6 — each a cold run (`query_<q>_cold`) plus the best
//!   of three warm runs (`query_<q>`).
//!
//! Each read query runs on a worker thread with a wall-clock timeout (default
//! 600s, matching upstream). On timeout we record `"TIMEOUT"`, abandon the
//! worker (it finishes on its own; the process reclaims it when the scale
//! ends) and continue to the next query — so one pathological query (q4's
//! `OPTIONAL` left-join is the prime suspect at `xlarge`) cannot prevent the
//! rest of the suite from being measured.
//!
//! Results accumulate into one JSON file across scales (run once per scale
//! into the same `--out`). `scripts/bench/trainmarks.sh` drives the three
//! scales, one process each (bounded peak memory).

use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use horndb_sparql::api::{execute_query, execute_update, QueryAnswer};
use horndb_sparql::exec::horn::HornBackend;
use oxrdf::{NamedNode, NamedOrBlankNode, Term as OxTerm, Triple};
use oxttl::{NTriplesParser, NTriplesSerializer, TurtleParser, TurtleSerializer};
use serde_json::{json, Value};

#[derive(Parser, Debug)]
#[command(
    name = "bench-trainmarks",
    about = "Run the trainmarks benchmark against HornDB."
)]
struct Cli {
    /// Directory holding `<scale>.ttl` / `<scale>.nt`.
    #[arg(long)]
    data_dir: PathBuf,
    /// Directory holding `q1_count.rq` .. `q6_delete_insert.rq`.
    #[arg(long)]
    queries_dir: PathBuf,
    /// Scale to run: medium | large | xlarge.
    #[arg(long)]
    scale: String,
    /// Results JSON to append to (created if absent).
    #[arg(long)]
    out: PathBuf,
    /// Per-(read-)query timeout in seconds.
    #[arg(long, default_value_t = 600)]
    timeout_secs: u64,
}

const FRAMEWORK: &str = "horndb";

/// Read queries, in upstream order. q6 (the only UPDATE) is handled separately.
const READ_QUERIES: &[&str] = &[
    "q1_count",
    "q2_customer_orders",
    "q3_join_3_entities",
    "q4_optional_aggregation",
    "q5_construct",
];

/// Accumulates result records and flushes the whole JSON after each one, so a
/// long or abandoned run still leaves a complete record of what finished.
struct Results {
    rows: Vec<Value>,
    out: PathBuf,
    scale: String,
}

impl Results {
    fn record(&mut self, operation: &str, seconds: Value) {
        self.rows.push(json!({
            "framework": FRAMEWORK,
            "scale": self.scale,
            "operation": operation,
            "seconds": seconds,
        }));
        if let Err(e) = self.flush() {
            eprintln!("warning: failed to flush results: {e}");
        }
    }
    fn flush(&self) -> Result<()> {
        let f = std::fs::File::create(&self.out)?;
        serde_json::to_writer_pretty(BufWriter::new(f), &self.rows)?;
        Ok(())
    }
}

fn read_existing(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<Value>>(&s).ok())
        .unwrap_or_default()
}

fn load(path: &Path, turtle: bool) -> Result<HornBackend> {
    let reader =
        BufReader::new(std::fs::File::open(path).with_context(|| format!("open {path:?}"))?);
    let mut batch: Vec<(OxTerm, OxTerm, OxTerm)> = Vec::new();
    if turtle {
        for t in TurtleParser::new().for_reader(reader) {
            let t = t?;
            batch.push((t.subject.into(), t.predicate.into(), t.object));
        }
    } else {
        for t in NTriplesParser::new().for_reader(reader) {
            let t = t?;
            batch.push((t.subject.into(), t.predicate.into(), t.object));
        }
    }
    let mut backend = HornBackend::new();
    backend
        .insert_oxrdf_batch(batch)
        .map_err(|e| anyhow::anyhow!("load: {e}"))?;
    Ok(backend)
}

/// (Term, Term, Term) -> oxrdf::Triple, dropping anything with a non-IRI/bnode
/// subject or non-IRI predicate (cannot occur for trainmarks data).
fn to_triple(s: OxTerm, p: OxTerm, o: OxTerm) -> Option<Triple> {
    let subject: NamedOrBlankNode = match s {
        OxTerm::NamedNode(n) => n.into(),
        OxTerm::BlankNode(b) => b.into(),
        _ => return None,
    };
    let predicate: NamedNode = match p {
        OxTerm::NamedNode(n) => n,
        _ => return None,
    };
    Some(Triple::new(subject, predicate, o))
}

fn write_turtle(backend: &HornBackend, path: &Path) -> Result<()> {
    let f = BufWriter::new(std::fs::File::create(path)?);
    let mut ser = TurtleSerializer::new().for_writer(f);
    for (s, p, o) in backend.iter_oxrdf() {
        if let Some(t) = to_triple(s, p, o) {
            ser.serialize_triple(&t)?;
        }
    }
    ser.finish()?.flush()?;
    Ok(())
}

fn write_ntriples(backend: &HornBackend, path: &Path) -> Result<()> {
    let f = BufWriter::new(std::fs::File::create(path)?);
    let mut ser = NTriplesSerializer::new().for_writer(f);
    for (s, p, o) in backend.iter_oxrdf() {
        if let Some(t) = to_triple(s, p, o) {
            ser.serialize_triple(&t)?;
        }
    }
    ser.finish().flush()?;
    Ok(())
}

/// Run a read query on a worker thread, returning its elapsed seconds, an
/// error string, or `None` on timeout (the worker is abandoned and keeps
/// running until it finishes — the process reclaims it at scale end).
fn run_read_timed(
    backend: &Arc<HornBackend>,
    sql: &str,
    timeout: Duration,
) -> Option<Result<f64, String>> {
    let (tx, rx) = mpsc::channel();
    let backend = Arc::clone(backend);
    let sql = sql.to_string();
    std::thread::spawn(move || {
        let t = Instant::now();
        let outcome = match execute_query(&sql, &*backend) {
            Ok(ans) => {
                match &ans {
                    QueryAnswer::Solutions { rows, .. } => {
                        std::hint::black_box(rows.len());
                    }
                    QueryAnswer::Triples(tr) => {
                        std::hint::black_box(tr.len());
                    }
                    QueryAnswer::Boolean(b) => {
                        std::hint::black_box(b);
                    }
                    QueryAnswer::Explanation { .. } => {}
                }
                Ok(t.elapsed().as_secs_f64())
            }
            Err(e) => Err(e.to_string()),
        };
        let _ = tx.send(outcome);
    });
    rx.recv_timeout(timeout).ok()
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let timeout = Duration::from_secs(cli.timeout_secs);
    let mut results = Results {
        rows: read_existing(&cli.out),
        out: cli.out.clone(),
        scale: cli.scale.clone(),
    };

    let ttl = cli.data_dir.join(format!("{}.ttl", cli.scale));
    let nt = cli.data_dir.join(format!("{}.nt", cli.scale));
    let tmp_ttl = cli.data_dir.join(format!("{}_horndb_out.ttl", cli.scale));
    let tmp_nt = cli.data_dir.join(format!("{}_horndb_out.nt", cli.scale));

    eprintln!("=== horndb — {} ===", cli.scale);

    // --- read Turtle (this backend feeds the queries) ---
    let t = Instant::now();
    let mut backend = load(&ttl, true)?;
    let secs = t.elapsed().as_secs_f64();
    eprintln!("  read_turtle: {secs:.4}s ({} triples)", backend.len());
    results.record("read_turtle", json!(secs));

    // --- write Turtle ---
    let t = Instant::now();
    write_turtle(&backend, &tmp_ttl)?;
    let secs = t.elapsed().as_secs_f64();
    eprintln!("  write_turtle: {secs:.4}s");
    results.record("write_turtle", json!(secs));
    let _ = std::fs::remove_file(&tmp_ttl);

    // --- write N-Triples ---
    let t = Instant::now();
    write_ntriples(&backend, &tmp_nt)?;
    let secs = t.elapsed().as_secs_f64();
    eprintln!("  write_ntriples: {secs:.4}s");
    results.record("write_ntriples", json!(secs));
    let _ = std::fs::remove_file(&tmp_nt);

    // --- read N-Triples (discarded; just I/O timing) ---
    let t = Instant::now();
    drop(load(&nt, false)?);
    let secs = t.elapsed().as_secs_f64();
    eprintln!("  read_ntriples: {secs:.4}s");
    results.record("read_ntriples", json!(secs));

    eprintln!("  queries:");

    // q6 (the only UPDATE) runs first, on the owned &mut backend. Its result
    // does not affect the read queries (none of q1..q5 read :unitPrice), and
    // running it here lets the read queries share an Arc<HornBackend> across
    // worker threads. Updates are fast and not run under the worker-timeout.
    {
        let sql = std::fs::read_to_string(cli.queries_dir.join("q6_delete_insert.rq"))
            .context("read q6")?;
        let run = |b: &mut HornBackend| -> (Result<(), String>, f64) {
            let t = Instant::now();
            let r = execute_update(&sql, b).map_err(|e| e.to_string());
            (r, t.elapsed().as_secs_f64())
        };
        let (r, secs) = run(&mut backend);
        match r {
            Ok(()) => results.record("query_q6_delete_insert_cold", json!(secs)),
            Err(e) => {
                eprintln!("    q6_delete_insert: ERROR {e}");
                results.record(
                    "query_q6_delete_insert_cold",
                    Value::String(format!("ERROR: {e}")),
                );
                results.record(
                    "query_q6_delete_insert",
                    Value::String(format!("ERROR: {e}")),
                );
            }
        }
        // best of 3 warm (only if cold succeeded)
        if !results
            .rows
            .last()
            .is_some_and(|r| r["seconds"].is_string())
        {
            let mut best = f64::INFINITY;
            for _ in 0..3 {
                let (r, secs) = run(&mut backend);
                if r.is_ok() {
                    best = best.min(secs);
                }
            }
            eprintln!("    q6_delete_insert: {best:.4}s (best of 3)");
            results.record("query_q6_delete_insert", json!(best));
        }
    }

    let backend = Arc::new(backend);

    for qname in READ_QUERIES {
        let sql = std::fs::read_to_string(cli.queries_dir.join(format!("{qname}.rq")))
            .with_context(|| format!("read {qname}.rq"))?;

        // Cold run.
        match run_read_timed(&backend, &sql, timeout) {
            None => {
                eprintln!("    {qname}: TIMEOUT (>{}s)", timeout.as_secs());
                results.record(
                    &format!("query_{qname}_cold"),
                    Value::String("TIMEOUT".into()),
                );
                results.record(&format!("query_{qname}"), Value::String("TIMEOUT".into()));
                continue;
            }
            Some(Err(e)) => {
                eprintln!("    {qname}: ERROR {e}");
                results.record(
                    &format!("query_{qname}_cold"),
                    Value::String(format!("ERROR: {e}")),
                );
                results.record(
                    &format!("query_{qname}"),
                    Value::String(format!("ERROR: {e}")),
                );
                continue;
            }
            Some(Ok(secs)) => results.record(&format!("query_{qname}_cold"), json!(secs)),
        }

        // Best of 3 warm runs.
        let mut best = f64::INFINITY;
        let mut timed_out = false;
        for _ in 0..3 {
            match run_read_timed(&backend, &sql, timeout) {
                Some(Ok(secs)) => best = best.min(secs),
                Some(Err(_)) => {}
                None => {
                    timed_out = true;
                    break;
                }
            }
        }
        if timed_out {
            eprintln!("    {qname}: TIMEOUT on warm run (>{}s)", timeout.as_secs());
            results.record(&format!("query_{qname}"), Value::String("TIMEOUT".into()));
        } else {
            eprintln!("    {qname}: {best:.4}s (best of 3)");
            results.record(&format!("query_{qname}"), json!(best));
        }
    }

    eprintln!("  done; results -> {}", cli.out.display());
    Ok(())
}
