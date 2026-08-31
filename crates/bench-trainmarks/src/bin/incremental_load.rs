//! `incremental_load` — phase table for a bulk load into a **non-empty** store
//! (HDB-91).
//!
//! Every load number the project has (HDB-85, HDB-86, HDB-87, HDB-84) comes
//! from a load into an empty store. This driver loads a base corpus, brings the
//! store to a fully-merged state as a reopened store would be, then appends a
//! second corpus and reports the `storage_load_phase_*` deltas for the append
//! on its own.
//!
//! ```text
//! incremental_load --base xlarge.nt --append append_overlap.nt \
//!     --path insert --batch 65536
//! ```
//!
//! `--path` picks the tier entry point, because HDB-84 only changed one of
//! them:
//!
//! * `insert` — `HornBackend::insert_oxrdf_batch` -> `Store::insert_quads` ->
//!   `Tier::insert_quad_batch`. What a SPARQL-side bulk ingest uses, and the
//!   path HDB-85's table covers. No `copy_forward` since HDB-84; the carried
//!   rows show up as `merge_runs` on the first read.
//! * `load` — `loader::load_ntriples_file` -> `Tier::insert_quad_batch`, i.e.
//!   the storage bulk loader with no `HornBackend` dedupe/`live_keys` above it.
//! * `apply` — `Store::apply_quads` -> `Tier::apply_quad_batch`, the
//!   `INSERT DATA` / SPARQL-update path, which HDB-84 did not touch and which
//!   still emits `copy_forward`.
//!
//! Counters are cumulative per process, so each stage is reported as the delta
//! across it. Run one stage combination per process.

#[cfg(feature = "snmalloc")]
#[global_allocator]
static GLOBAL: snmalloc_rs::SnMalloc = snmalloc_rs::SnMalloc;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use horndb_sparql::exec::horn::HornBackend;
use horndb_storage::loader::ntriples::{for_each_ntriples_batch, load_ntriples_file};
use horndb_storage::loader::turtle::{for_each_turtle_batch, load_turtle_file};
use horndb_storage::loader::{load_threads, set_load_batch_triples};
use horndb_storage::{Store, DEFAULT_GRAPH};
use oxrdf::{Term as OxTerm, Triple};

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum PathKind {
    Insert,
    Load,
    Apply,
}

#[derive(Parser, Debug)]
#[command(
    name = "incremental_load",
    about = "Bulk-load phase table for a non-empty store."
)]
struct Cli {
    /// Base corpus loaded first (`.nt` or `.ttl`).
    #[arg(long)]
    base: PathBuf,
    /// Corpus appended to the already-loaded base.
    #[arg(long)]
    append: PathBuf,
    /// Tier entry point to measure.
    #[arg(long, value_enum, default_value_t = PathKind::Insert)]
    path: PathKind,
    /// Triples per tier insert call for the append. 0 = one call.
    #[arg(long, default_value_t = 65_536)]
    batch: usize,
}

fn is_turtle(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()) == Some("ttl")
}

/// Parse a document into oxrdf triples, timing nothing: the `parse` phase is
/// reported by the phase counters only where a stage needs it, and this driver
/// reports parse as a wall time per stage instead.
fn parse_file(path: &Path) -> Result<Vec<(OxTerm, OxTerm, OxTerm)>> {
    let bytes = std::fs::read(path).with_context(|| format!("read {path:?}"))?;
    let mut batch: Vec<(OxTerm, OxTerm, OxTerm)> = Vec::new();
    let mut push = |triples: Vec<Triple>| {
        batch.extend(
            triples
                .into_iter()
                .map(|t| (t.subject.into(), t.predicate.into(), t.object)),
        );
        Ok(())
    };
    let threads = load_threads();
    if is_turtle(path) {
        for_each_turtle_batch(&bytes, None, threads, &mut push)?;
    } else {
        for_each_ntriples_batch(&bytes, threads, &mut push)?;
    }
    Ok(batch)
}

/// One `storage_load_phase_*` snapshot: phase -> (nanoseconds, rows).
type Phases = BTreeMap<String, (u64, u64)>;

fn snapshot_phases() -> Phases {
    let mut out = Phases::new();
    for line in horndb_metrics::encode_metrics().lines() {
        let Some(rest) = line.strip_prefix("horndb_storage_load_phase_") else {
            continue;
        };
        let (kind, rest) = match rest.split_once('{') {
            Some(("nanoseconds_total", r)) => (0usize, r),
            Some(("rows_total", r)) => (1usize, r),
            _ => continue,
        };
        let Some((labels, value)) = rest.split_once("} ") else {
            continue;
        };
        let phase = labels
            .trim_start_matches("phase=\"")
            .trim_end_matches('"')
            .to_string();
        let Ok(v) = value.trim().parse::<u64>() else {
            continue;
        };
        let e = out.entry(phase).or_insert((0, 0));
        if kind == 0 {
            e.0 = v;
        } else {
            e.1 = v;
        }
    }
    out
}

fn report(stage: &str, before: &Phases, after: &Phases, wall: f64, extra: &str) {
    println!("[stage {stage}] wall {wall:.3}s {extra}");
    let mut accounted = 0.0;
    for (phase, (ns, rows)) in after {
        let (b_ns, b_rows) = before.get(phase).copied().unwrap_or((0, 0));
        let d_ns = ns.saturating_sub(b_ns);
        let d_rows = rows.saturating_sub(b_rows);
        if d_ns == 0 && d_rows == 0 {
            continue;
        }
        let secs = d_ns as f64 / 1e9;
        accounted += secs;
        println!(
            "  {phase:<14} {secs:>8.3}s  {:>6.2}%  rows {d_rows}",
            100.0 * secs / wall
        );
    }
    println!(
        "  {:<14} {accounted:>8.3}s  {:>6.2}%  (residual {:.3}s)",
        "accounted",
        100.0 * accounted / wall,
        wall - accounted
    );
}

/// Insert `triples` through `HornBackend` in chunks of `batch`.
fn insert_backend(
    backend: &mut HornBackend,
    mut triples: Vec<(OxTerm, OxTerm, OxTerm)>,
    batch: usize,
) -> Result<()> {
    if batch == 0 || batch >= triples.len() {
        backend
            .insert_oxrdf_batch(triples)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        return Ok(());
    }
    while !triples.is_empty() {
        let take = batch.min(triples.len());
        let rest = triples.split_off(take);
        backend
            .insert_oxrdf_batch(triples)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        triples = rest;
    }
    Ok(())
}

/// Insert `triples` through `Store::apply_quads` (the `apply_quad_batch` path)
/// in chunks of `batch`.
fn apply_store(store: &Store, triples: &[(OxTerm, OxTerm, OxTerm)], batch: usize) -> Result<()> {
    let chunk = if batch == 0 { triples.len() } else { batch };
    for part in triples.chunks(chunk.max(1)) {
        let adds: Vec<_> = part
            .iter()
            .map(|(s, p, o)| (DEFAULT_GRAPH, s.clone(), p.clone(), o.clone()))
            .collect();
        store.apply_quads(&[], &adds)?;
    }
    Ok(())
}

fn load_file(store: &Store, path: &Path) -> Result<u64> {
    let stats = if is_turtle(path) {
        load_turtle_file(store, path)?
    } else {
        load_ntriples_file(store, path)?
    };
    Ok(stats.triples)
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    println!(
        "=== incremental_load: base {:?} append {:?} path {:?} batch {} ===",
        cli.base, cli.append, cli.path, cli.batch
    );

    match cli.path {
        PathKind::Insert => {
            let mut backend = HornBackend::new();
            let triples = parse_file(&cli.base)?;
            let p0 = snapshot_phases();
            let t = Instant::now();
            insert_backend(&mut backend, triples, 0)?;
            let st = backend.storage_stats(); // forces the deferred merge
            let (base_rows, dict0) = (st.triples, st.dictionary_terms);
            let wall = t.elapsed().as_secs_f64();
            report(
                "base",
                &p0,
                &snapshot_phases(),
                wall,
                &format!("rows {base_rows} dict {dict0}"),
            );

            let t_parse = Instant::now();
            let triples = parse_file(&cli.append)?;
            println!("[append parse] {:.3}s", t_parse.elapsed().as_secs_f64());
            let n = triples.len();
            let p1 = snapshot_phases();
            let t = Instant::now();
            insert_backend(&mut backend, triples, cli.batch)?;
            let st = backend.storage_stats(); // forces the deferred merge
            let (rows, dict1) = (st.triples, st.dictionary_terms);
            let wall = t.elapsed().as_secs_f64();
            report(
                "append",
                &p1,
                &snapshot_phases(),
                wall,
                &format!(
                    "appended {n} rows {rows} (+{}) dict {dict1} (+{})",
                    rows - base_rows,
                    dict1 - dict0
                ),
            );
        }
        PathKind::Load => {
            let store = Store::in_memory();
            let p0 = snapshot_phases();
            let t = Instant::now();
            let base_triples = load_file(&store, &cli.base)?;
            let base_rows = store.triple_count(); // forces the deferred merge
            let wall = t.elapsed().as_secs_f64();
            let dict0 = store.dictionary().len();
            report(
                "base",
                &p0,
                &snapshot_phases(),
                wall,
                &format!("parsed {base_triples} rows {base_rows} dict {dict0}"),
            );

            set_load_batch_triples(if cli.batch == 0 {
                usize::MAX
            } else {
                cli.batch
            });
            let p1 = snapshot_phases();
            let t = Instant::now();
            let n = load_file(&store, &cli.append)?;
            let rows = store.triple_count(); // forces the deferred merge
            let wall = t.elapsed().as_secs_f64();
            let dict1 = store.dictionary().len();
            report(
                "append",
                &p1,
                &snapshot_phases(),
                wall,
                &format!(
                    "appended {n} rows {rows} (+{}) dict {dict1} (+{})",
                    rows - base_rows,
                    dict1 - dict0
                ),
            );
        }
        PathKind::Apply => {
            let store = Store::in_memory();
            let p0 = snapshot_phases();
            let t = Instant::now();
            let base_triples = load_file(&store, &cli.base)?;
            let base_rows = store.triple_count(); // forces the deferred merge
            let wall = t.elapsed().as_secs_f64();
            let dict0 = store.dictionary().len();
            report(
                "base",
                &p0,
                &snapshot_phases(),
                wall,
                &format!("parsed {base_triples} rows {base_rows} dict {dict0}"),
            );

            let t_parse = Instant::now();
            let triples = parse_file(&cli.append)?;
            println!("[append parse] {:.3}s", t_parse.elapsed().as_secs_f64());
            let n = triples.len();
            let p1 = snapshot_phases();
            let t = Instant::now();
            apply_store(&store, &triples, cli.batch)?;
            let rows = store.triple_count(); // forces the deferred merge
            let wall = t.elapsed().as_secs_f64();
            let dict1 = store.dictionary().len();
            report(
                "append",
                &p1,
                &snapshot_phases(),
                wall,
                &format!(
                    "appended {n} rows {rows} (+{}) dict {dict1} (+{})",
                    rows - base_rows,
                    dict1 - dict0
                ),
            );
        }
    }
    Ok(())
}
