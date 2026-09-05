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

// HDB-86 E1: snmalloc is the process-wide allocator. The bulk load frees ~30M
// oxrdf terms on the main thread that were allocated on parse threads, and
// glibc malloc takes the owning arena's lock for every such cross-thread free;
// snmalloc keeps per-thread free lists and services a remote free without it.
// Disable the `snmalloc` feature to fall back to the system allocator — that
// is both the revert path and how the A/B is re-run.
#[cfg(feature = "snmalloc")]
#[global_allocator]
static GLOBAL: snmalloc_rs::SnMalloc = snmalloc_rs::SnMalloc;

/// Name of the allocator this binary was built with, printed with the results
/// so a recorded number is never ambiguous about which build produced it.
const ALLOCATOR: &str = if cfg!(feature = "snmalloc") {
    "snmalloc"
} else {
    "system"
};

use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use horndb_sparql::api::{execute_query, execute_update, QueryAnswer};
use horndb_sparql::exec::horn::HornBackend;
use horndb_storage::loader::load_threads;
use horndb_storage::loader::ntriples::for_each_ntriples_batch;
use horndb_storage::loader::turtle::for_each_turtle_batch;
use oxrdf::{NamedNode, NamedOrBlankNode, Term as OxTerm, Triple};
use oxttl::{NTriplesSerializer, TurtleSerializer};
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
    /// Stop after the two read (load) operations, skipping writes and queries.
    /// For load-path work, where only the `storage_load_phase_*` counters are
    /// wanted and the query suite is minutes of irrelevant runtime.
    #[arg(long, default_value_t = false)]
    load_only: bool,
    /// Measure the serving footprint honestly: load the corpus ONCE, run the
    /// read queries (which build the query snapshot), then report RSS. Skips
    /// the write legs, the discarded second load of the `read_ntriples` leg,
    /// and q6 (the only UPDATE).
    ///
    /// Without this, the `[mem]` line at the end of `main` is not a serving
    /// footprint: the `read_ntriples` leg builds and drops a whole second
    /// store, and the allocator does not return that arena to the OS, so the
    /// sampled VmRSS carries a freed store's pages. That inflated the first
    /// hornbench measurement roughly eightfold (HDB-144).
    #[arg(long, default_value_t = false)]
    mem_only: bool,
    /// Preallocate the parse batch for this many triples. 0 (the default)
    /// estimates the count from the file instead; pass a value only to pin it.
    ///
    /// The batch reaches ~1 GB at xlarge, so growing it on demand means a
    /// handful of reallocs of one very large block. glibc serves those from
    /// `mmap` and grows them with `mremap`, which only edits page tables,
    /// whereas snmalloc copies — which is why the allocator swap tripled the
    /// `materialize` phase (0.59s -> 1.77s) until the batch was preallocated.
    #[arg(long, default_value_t = 0)]
    reserve_triples: usize,
}

const FRAMEWORK: &str = "horndb";

/// Read queries, in upstream order. q6 (the only UPDATE) is handled separately.
/// Resident set size in MiB from `/proc/self/status`: `VmRSS` (current) for
/// `field = "VmRSS:"`, `VmHWM` (peak) for `field = "VmHWM:"`. Linux only;
/// 0.0 elsewhere.
fn rss_mib(field: &str) -> f64 {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return 0.0;
    };
    status
        .lines()
        .find_map(|l| l.strip_prefix(field))
        .and_then(|v| v.split_whitespace().next()?.parse::<f64>().ok())
        .map(|kb| kb / 1024.0)
        .unwrap_or(0.0)
}

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

/// Parse a document and bulk-insert it.
///
/// Parsing runs on `horndb_storage::loader::load_threads()` threads via the
/// shared parallel-chunking primitives (HDB-83); interning and index build
/// stay where they were. Set `HORNDB_LOAD_THREADS=1` to measure the serial
/// baseline. Turtle only splits when the document clears
/// `turtle_split_is_safe`; trainmarks files declare every prefix up front, so
/// it does.
/// Estimate the triple count of a document from its bytes, for preallocating
/// the parse batch.
///
/// Both formats this driver reads put one triple per line, so the mean line
/// length over a sample gives the count directly. Sampling a prefix rather than
/// scanning the whole file keeps this off the measured path; the estimate only
/// has to be within a factor or so of the truth to remove the repeated doubling
/// of a ~1 GB `Vec`, and `Vec` still grows if it is short.
fn estimate_triples(bytes: &[u8]) -> usize {
    const SAMPLE: usize = 1 << 20;
    let sample = &bytes[..bytes.len().min(SAMPLE)];
    let lines = count_newlines(sample);
    if lines == 0 {
        return 0;
    }
    let mean_line = (sample.len() as f64) / (lines as f64);
    // +10% headroom: undershooting costs a realloc of the whole block, while
    // overshooting costs only untouched virtual address space.
    (((bytes.len() as f64) / mean_line) * 1.1) as usize
}

fn count_newlines(b: &[u8]) -> usize {
    b.iter().filter(|&&c| c == b'\n').count()
}

fn load(path: &Path, turtle: bool, reserve_triples: usize) -> Result<HornBackend> {
    let bytes = std::fs::read(path).with_context(|| format!("read {path:?}"))?;
    let threads = load_threads();
    let reserve = if reserve_triples > 0 {
        reserve_triples
    } else {
        estimate_triples(&bytes)
    };
    let t_parse = std::time::Instant::now();
    let mut batch: Vec<(OxTerm, OxTerm, OxTerm)> = Vec::with_capacity(reserve);
    // Time only the materialisation into `batch`, so `parse` minus this is
    // oxttl tokenisation. The closure runs once per chunk batch, not per
    // triple, and accumulates into a local (SPEC-17 §5.4).
    let mut materialize_ns = 0u64;
    let mut push = |triples: Vec<Triple>| {
        let t = std::time::Instant::now();
        batch.extend(
            triples
                .into_iter()
                .map(|t| (t.subject.into(), t.predicate.into(), t.object)),
        );
        materialize_ns += t.elapsed().as_nanos() as u64;
        Ok(())
    };
    if turtle {
        for_each_turtle_batch(&bytes, None, threads, &mut push)?;
    } else {
        for_each_ntriples_batch(&bytes, threads, &mut push)?;
    }
    horndb_metrics::metrics().storage.record_load_phase(
        horndb_metrics::labels::LoadPhase::Parse,
        t_parse.elapsed(),
        batch.len() as u64,
    );
    horndb_metrics::metrics().storage.record_load_phase(
        horndb_metrics::labels::LoadPhase::Materialize,
        std::time::Duration::from_nanos(materialize_ns),
        batch.len() as u64,
    );
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

/// Print the cumulative `storage_load_phase_*` counters (SPEC-17 §5.4.1) after
/// a load, so a trainmarks run reports where bulk-load time actually went.
/// Counters are cumulative across the process; subtract successive dumps to get
/// a single load's share.
fn dump_load_phases(label: &str) {
    let encoded = horndb_metrics::encode_metrics();
    eprintln!("  [load-phases after {label}]");
    for line in encoded.lines() {
        if line.starts_with("horndb_storage_load_phase") {
            eprintln!("    {line}");
        }
    }
}

/// Print the cumulative `sparql_exec_phase_*` counters (HDB-99) and the
/// `wcoj_*` per-query counters so a trainmarks run reports which operator
/// inside `exec` a query spent its time in, and how many leapfrog seeks the
/// join needed to get there.
///
/// The whole dump is gated on `HORNDB_EXEC_PHASES=1`. The exec-phase
/// counters are never touched without it, but the `wcoj_*` histograms are
/// always live, so printing them ungated would add ~1000 lines of stderr to
/// every ordinary trainmarks run. Only their `_sum`/`_count` lines are
/// printed — the twelve exponential buckets say nothing a run of one query
/// per dump interval needs.
///
/// Counters are cumulative across the process, like `dump_load_phases` —
/// but unlike that helper (called exactly twice, nothing else running in
/// between), a read loop calls this four times per query, so a *single run's*
/// share is only the diff between an adjacent pair of that query's own dumps:
/// `"{qname}_pre"` → `"{qname}_cold"` for the cold run, and
/// `"{qname}_warm_pre"` → `"{qname}_warm"` for the last of the three warm
/// runs. Diffing any other pair (e.g. `"q3_cold"` minus `"q2_cold"`, or
/// `"q3_cold"` minus `"q3_pre"` against a warm figure) folds in unrelated
/// work: it made a HDB-99 measurement pass briefly show `GROUP BY` phase
/// activity for q3, a query with no `GROUP BY` at all, borrowed from q2's
/// warm runs.
///
/// Cold and warm splits differ substantially and are not interchangeable.
/// A cold run also carries the first-use build of whatever WCOJ trie
/// ordering the query needs (HDB-97/98), which lands in `residual`: q3 at
/// xlarge is 66% `scan_wcoj` cold but 95% warm (HDB-108). Quote the warm
/// pair against a warm wall-clock number, the cold pair against a cold one.
fn dump_exec_phases(label: &str) {
    if !matches!(
        std::env::var("HORNDB_EXEC_PHASES").as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE")
    ) {
        return;
    }
    let encoded = horndb_metrics::encode_metrics();
    eprintln!("  [exec-phases after {label}]");
    for line in encoded.lines() {
        let keep = line.starts_with("horndb_sparql_exec_phase")
            || (line.starts_with("horndb_wcoj_")
                && (line.contains("_sum ") || line.contains("_count ")));
        if keep {
            eprintln!("    {line}");
        }
    }
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

    eprintln!("=== horndb — {} (allocator: {ALLOCATOR}) ===", cli.scale);

    // --- read Turtle (this backend feeds the queries) ---
    let t = Instant::now();
    let mut backend = load(&ttl, true, cli.reserve_triples)?;
    let secs = t.elapsed().as_secs_f64();
    eprintln!("  read_turtle: {secs:.4}s ({} triples)", backend.len());
    results.record("read_turtle", json!(secs));
    dump_load_phases("read_turtle");

    // --- write Turtle / N-Triples --- (both skipped under --load-only; the
    // read_ntriples leg below reads the source file, not what these produce)
    if !cli.load_only && !cli.mem_only {
        let t = Instant::now();
        write_turtle(&backend, &tmp_ttl)?;
        let secs = t.elapsed().as_secs_f64();
        eprintln!("  write_turtle: {secs:.4}s");
        results.record("write_turtle", json!(secs));
        let _ = std::fs::remove_file(&tmp_ttl);

        let t = Instant::now();
        write_ntriples(&backend, &tmp_nt)?;
        let secs = t.elapsed().as_secs_f64();
        eprintln!("  write_ntriples: {secs:.4}s");
        results.record("write_ntriples", json!(secs));
        let _ = std::fs::remove_file(&tmp_nt);
    }

    // --- read N-Triples (discarded; just I/O timing) ---
    // Skipped under --mem-only: this builds a second full store and drops it,
    // and the freed arena stays resident, which is exactly what corrupts the
    // footprint sample at the end of main.
    if !cli.mem_only {
        let t = Instant::now();
        drop(load(&nt, false, cli.reserve_triples)?);
        let secs = t.elapsed().as_secs_f64();
        eprintln!("  read_ntriples: {secs:.4}s");
        results.record("read_ntriples", json!(secs));
        dump_load_phases("read_ntriples");
    }

    if cli.load_only {
        eprintln!("  load-only: skipping writes and queries");
        return Ok(());
    }

    eprintln!("  queries:");

    // q6 (the only UPDATE) runs first, on the owned &mut backend. Its result
    // does not affect the read queries (none of q1..q5 read :unitPrice), and
    // running it here lets the read queries share an Arc<HornBackend> across
    // worker threads. Updates are fast and not run under the worker-timeout.
    // Skipped under --mem-only: q6 mutates the store, and the footprint
    // sample is meant to be the store as loaded and queried.
    if !cli.mem_only {
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
    }

    let backend = Arc::new(backend);

    for qname in READ_QUERIES {
        let sql = std::fs::read_to_string(cli.queries_dir.join(format!("{qname}.rq")))
            .with_context(|| format!("read {qname}.rq"))?;

        dump_exec_phases(&format!("{qname}_pre"));
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
        dump_exec_phases(&format!("{qname}_cold"));

        // Best of 3 warm runs. The last one is bracketed by its own dump
        // pair so a warm phase split can be read off without the cold run's
        // one-off ordering build folded in — see `dump_exec_phases`.
        let mut best = f64::INFINITY;
        let mut timed_out = false;
        for i in 0..3 {
            if i == 2 {
                dump_exec_phases(&format!("{qname}_warm_pre"));
            }
            match run_read_timed(&backend, &sql, timeout) {
                Some(Ok(secs)) => best = best.min(secs),
                Some(Err(_)) => {}
                None => {
                    timed_out = true;
                    break;
                }
            }
        }
        dump_exec_phases(&format!("{qname}_warm"));
        if timed_out {
            eprintln!("    {qname}: TIMEOUT on warm run (>{}s)", timeout.as_secs());
            results.record(&format!("query_{qname}"), Value::String("TIMEOUT".into()));
        } else {
            eprintln!("    {qname}: {best:.4}s (best of 3)");
            results.record(&format!("query_{qname}"), json!(best));
        }
    }

    // Serving footprint (HDB-120): RSS with the store loaded AND a query
    // snapshot built, over the triples actually served. This is the number
    // `docs/benchmarks.md`'s "serving footprint" row reports.
    //
    // Only meaningful under --mem-only. In a full run this process has also
    // built and dropped a second store (the `read_ntriples` leg) and written
    // two serialisations, and the allocator keeps those pages, so the sample
    // is a high-water mark of the whole run rather than a serving footprint.
    //
    // Scope of the number: one process, one loaded store, one query snapshot,
    // whole-process RSS. That is columnar partitions + dictionary + the
    // per-scope query source + the binary, its stacks and allocator overhead.
    // It is NOT attributed between those, and it is not comparable with the
    // partition-only B/quad figure, which is a different scope on a different
    // corpus.
    {
        let triples = backend.len();
        let rss = rss_mib("VmRSS:");
        let peak = rss_mib("VmHWM:");
        let per_triple = if triples == 0 {
            0.0
        } else {
            rss * 1024.0 * 1024.0 / triples as f64
        };
        let peak_per_triple = if triples == 0 {
            0.0
        } else {
            peak * 1024.0 * 1024.0 / triples as f64
        };
        // HDB-146: attribute the RSS. Each component accounts for itself; the
        // rest of RSS is the residual, which is where allocator retention,
        // per-query intermediates, the binary and its stacks land.
        let split = backend.memory_split();
        let mib = |b: u64| b as f64 / (1024.0 * 1024.0);
        for (label, bytes) in split.rows() {
            eprintln!(
                "  [mem] {label}: {:.0} MiB ({:.1}% of RSS, {:.1} B/triple)",
                mib(bytes),
                if rss > 0.0 {
                    100.0 * mib(bytes) / rss
                } else {
                    0.0
                },
                if triples == 0 {
                    0.0
                } else {
                    bytes as f64 / triples as f64
                },
            );
        }
        let attributed = mib(split.total());
        eprintln!(
            "  [mem] attributed: {attributed:.0} MiB; unattributed (allocator \
             retention + query intermediates): {:.0} MiB ({:.1}% of RSS)",
            rss - attributed,
            if rss > 0.0 {
                100.0 * (rss - attributed) / rss
            } else {
                0.0
            },
        );
        eprintln!(
            "  [mem] serving footprint{}: RSS {rss:.0} MiB over {triples} triples \
             = {per_triple:.1} B/triple; peak {peak:.0} MiB = {peak_per_triple:.1} B/triple",
            if cli.mem_only {
                ""
            } else {
                " (NOT ISOLATED — rerun with --mem-only)"
            }
        );
    }

    eprintln!("  done; results -> {}", cli.out.display());
    Ok(())
}
