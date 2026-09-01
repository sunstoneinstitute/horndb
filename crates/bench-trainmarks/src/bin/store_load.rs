//! `store_load` — parse-thread sweep for a **real `Store` bulk load** (HDB-96).
//!
//! Every published thread sweep so far (HDB-83, HDB-86, HDB-94) measured the
//! trainmarks driver's path: parse into one `Vec<Triple>`, then hand the whole
//! vector to `HornBackend::insert_oxrdf_batch`. That is not what
//! `Store::load_*_file` does, and it is the tier leg — not the parse — that
//! decided the `HORNDB_LOAD_THREADS=1` default.
//!
//! This driver calls `load_turtle_slice_with_threads` /
//! `load_ntriples_slice_with_threads` into a fresh in-memory `Store`, forces
//! the deferred run merge with a first read (HDB-84 defers it, so a load
//! nobody reads has left work undone), and prints the `storage_load_phase_*`
//! table plus peak RSS.
//!
//! ```text
//! store_load --file target/trainmarks/data/xlarge.ttl --threads 16
//! ```
//!
//! Counters are cumulative per process, so run one thread count per process.

#[cfg(feature = "snmalloc")]
#[global_allocator]
static GLOBAL: snmalloc_rs::SnMalloc = snmalloc_rs::SnMalloc;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
use clap::Parser;
use horndb_storage::loader::ntriples::load_ntriples_slice_with_threads;
use horndb_storage::loader::turtle::load_turtle_slice_with_threads;
use horndb_storage::Store;

#[derive(Parser, Debug)]
#[command(
    name = "store_load",
    about = "Parse-thread sweep for a real Store bulk load."
)]
struct Cli {
    /// Corpus to load (`.ttl` or `.nt`).
    #[arg(long)]
    file: PathBuf,
    /// Parse threads handed to the slice loader. 1 = serial (no channel).
    #[arg(long, default_value_t = 1)]
    threads: usize,
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

/// Peak resident set size (`VmHWM`) in MiB. Linux only; 0.0 elsewhere.
fn peak_rss_mib() -> f64 {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return 0.0;
    };
    status
        .lines()
        .find_map(|l| l.strip_prefix("VmHWM:"))
        .and_then(|v| v.split_whitespace().next()?.parse::<f64>().ok())
        .map(|kb| kb / 1024.0)
        .unwrap_or(0.0)
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let turtle = cli.file.extension().and_then(|e| e.to_str()) == Some("ttl");
    let bytes = std::fs::read(&cli.file).with_context(|| format!("read {:?}", cli.file))?;
    println!(
        "=== store_load: {:?} ({} bytes, {}) threads {} ===",
        cli.file,
        bytes.len(),
        if turtle { "turtle" } else { "ntriples" },
        cli.threads
    );

    let store = Store::in_memory();
    let before = snapshot_phases();
    let t = Instant::now();
    let stats = if turtle {
        load_turtle_slice_with_threads(&store, &bytes, None, cli.threads)
    } else {
        load_ntriples_slice_with_threads(&store, &bytes, cli.threads)
    }
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    let load_wall = t.elapsed().as_secs_f64();
    // Forces the runs HDB-84 defers; a load nobody reads has work outstanding.
    let rows = store.triple_count();
    let wall = t.elapsed().as_secs_f64();
    let after = snapshot_phases();

    println!(
        "[load] wall {wall:.3}s (loader {load_wall:.3}s + first read {:.3}s)  \
         parsed {} rows {rows} dict {}",
        wall - load_wall,
        stats.triples,
        store.dictionary().len()
    );

    let mut accounted = 0.0;
    for (phase, (ns, rows)) in &after {
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
    println!("[mem] peak RSS {:.0} MiB", peak_rss_mib());
    Ok(())
}
