//! `serve` — a thin HTTP wrapper that loads one or more RDF files into the
//! dictionary-encoded `HornBackend` and exposes the SPARQL 1.1 query endpoint
//! built by [`horndb_sparql::server::build_router`].
//!
//! Pass `--materialize` to run OWL 2 RL forward-chaining over the loaded data
//! before serving (requires the `reasoner` feature, on by default).
//!
//! The storage and join execution are backed by `horndb-storage` (dictionary
//! encoding) and `horndb-wcoj` (Leapfrog Triejoin).
//!
//! The SPARQL query endpoint is `http://<bind>/query` (GET or POST) — NOT
//! `/sparql`. SPARQL Update is at `/update`.
//!
//! Configuration (SPEC-26) is resolved through `horndb-config` before any data
//! loads or the socket binds: built-in defaults < `--config`'s (or
//! `$HORNDB_CONFIG`'s, or `/etc/horndb/config.toml`'s) `config.toml` <
//! `config.d/*.toml` < `HORNDB_SERVER__BIND` / `HORNDB_SIMD__*` env vars <
//! `--bind` / `--simd-max-isa` / `--simd-autotune`. An invalid config is fatal
//! at startup.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Parser;
use horndb_config::{CliOverrides, LoadInputs};
#[cfg(feature = "reasoner")]
use oxrdf::{GraphName, Quad};
use oxrdf::{NamedOrBlankNode, Term as OxTerm};
use oxttl::{NTriplesParser, TurtleParser};
use std::sync::{Arc, RwLock};
use std::time::Instant;

use horndb_sparql::exec::horn::HornBackend;
use horndb_sparql::server::{build_router, AppState};

#[derive(Parser, Debug)]
#[command(
    name = "serve",
    about = "Load flat RDF file(s) into the HornBackend store and serve SPARQL 1.1 over HTTP."
)]
struct Cli {
    /// One or more N-Triples (`.nt`) or Turtle (`.ttl`) files, or
    /// directories containing them, to load into the store. Repeatable.
    #[arg(long = "data", required = true, num_args = 1..)]
    data: Vec<PathBuf>,

    /// Path to a `config.toml` (SPEC-26). Highest precedence for *which* file
    /// is read; falls back to `$HORNDB_CONFIG`, then `/etc/horndb/config.toml`.
    #[arg(long = "config")]
    config: Option<PathBuf>,

    /// Override `[server].bind`, e.g. `127.0.0.1:3840` (3840 is HornDB's
    /// standard port). Wins over `HORNDB_SERVER__BIND` and the config file;
    /// leave unset to use the resolved config value. No default here — a
    /// `clap` default would silently clobber a file/env value.
    #[arg(long = "bind")]
    bind: Option<String>,

    /// Override `[simd].max_isa` (`scalar`/`avx2`/`avx512`/`neon`). Seeds the
    /// `crates/simd` ISA cap before the first dispatch; leave unset to
    /// auto-detect.
    #[arg(long = "simd-max-isa")]
    simd_max_isa: Option<String>,

    /// Override `[simd].autotune`. Seeds the `crates/simd` autotune toggle
    /// before the first dispatch; leave unset to keep autotune on.
    #[arg(long = "simd-autotune")]
    simd_autotune: Option<bool>,

    /// Run OWL 2 RL materialization over the loaded data and serve the
    /// closure (requires the `reasoner` feature, on by default).
    #[arg(long = "materialize", default_value_t = false)]
    materialize: bool,
}

/// Map CLI flags onto `horndb_config::LoadInputs`. Only the value flags that
/// are present (`Some`) enter `cli_overrides`; an absent flag stays `None` and
/// contributes nothing, so it cannot clobber a lower layer (env or file).
/// Extracted from `main()` so this mapping is unit-testable without spawning
/// a server. `env_config_path` reads `HORNDB_CONFIG` directly (not a `clap`
/// flag) so file-location precedence is file < env < `--config`, matching
/// `horndb_config::load`'s `cli_config_path > env_config_path > default`
/// resolution order.
fn load_inputs(cli: &Cli) -> LoadInputs {
    LoadInputs {
        cli_config_path: cli.config.clone(),
        env_config_path: std::env::var_os("HORNDB_CONFIG").map(PathBuf::from),
        cli_overrides: CliOverrides {
            bind: cli.bind.clone(),
            simd_max_isa: cli.simd_max_isa.clone(),
            simd_autotune: cli.simd_autotune,
        },
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Resolve configuration before anything else: an invalid config (unknown
    // key, out-of-range value, a `--config`/`HORNDB_CONFIG` file that does not
    // exist) is fatal at startup, before any data loads or the socket binds.
    let inputs = load_inputs(&cli);
    let cfg = horndb_config::load(&inputs).context("resolving configuration")?;

    // Seed the SIMD policy (ISA cap + auto-tune) from the resolved `[simd]`
    // config BEFORE the first dispatch or priming, so it reaches the `OnceLock`s
    // before any primitive resolves them. This MUST precede
    // `record_simd_calibration()`, which primes every kernel (and thus resolves
    // the policy cells) — seeding after it would be a silent no-op. An unknown
    // `max_isa` string is startup-fatal, naming the bad value: `horndb-config`
    // types `max_isa` as a free `Option<String>`, so the enum check lands here.
    let simd_max_isa = match cfg.simd.max_isa.as_deref() {
        None => None,
        Some(s) => Some(horndb_simd::parse_max_isa(s).ok_or_else(|| {
            anyhow::anyhow!(
                "invalid [simd].max_isa {s:?}: expected one of scalar, avx2, avx512, neon"
            )
        })?),
    };
    horndb_simd::configure(simd_max_isa, cfg.simd.autotune);

    // Pay the SIMD startup micro-calibration cost up front and publish which
    // kernel/ISA each primitive picked as `horndb_simd_kernel_isa` gauges.
    record_simd_calibration();

    // SPEC-28 S3/D2: map `[server.limits].default_graph` into the typed
    // `SparqlConfig` the query handlers use. Unlike `[simd].max_isa` above,
    // no domain check is needed here — `default_graph` is a serde-level enum
    // (`horndb_config::DefaultGraph`), so an unrecognized value already
    // failed `horndb_config::load()` above, with file+key attribution.
    let sparql_cfg = horndb_sparql::SparqlConfig {
        rdf12: cfg.server.limits.rdf12,
        default_graph: cfg.server.limits.default_graph.into(),
    };

    let mut files: Vec<PathBuf> = Vec::new();
    for path in &cli.data {
        collect_data_files(path, &mut files)
            .with_context(|| format!("enumerating {}", path.display()))?;
    }
    if files.is_empty() {
        anyhow::bail!("no .nt/.ttl files found in the provided --data paths");
    }

    let mut store = HornBackend::new();
    let total;

    if cli.materialize {
        #[cfg(feature = "reasoner")]
        {
            // Parse all files into an oxrdf::Dataset, then run the OWL 2 RL
            // closure before loading into the served store.
            let mut dataset = oxrdf::Dataset::default();
            let mut input_bytes: u64 = 0;
            let t = Instant::now();
            for f in &files {
                input_bytes += std::fs::metadata(f).map(|m| m.len()).unwrap_or(0);
                let n = collect_into_dataset(f, &mut dataset)
                    .with_context(|| format!("loading {}", f.display()))?;
                eprintln!("serve: parsed {n} triples from {}", f.display());
            }
            let stats = horndb_sparql::exec::horn::load_with_reasoning(&mut store, &dataset)
                .context("OWL 2 RL materialization")?;
            // Record the whole parse+materialize+load span as ONE observation
            // (no per-file double-count). Note: this branch's
            // `load_duration_seconds` sample is per-batch, whereas the
            // non-materialize branch below records one sample per file.
            horndb_metrics::metrics()
                .storage
                .load_bytes
                .inc_by(input_bytes);
            horndb_metrics::metrics()
                .storage
                .load_duration_seconds
                .observe(t.elapsed().as_secs_f64());
            eprintln!(
                "serve: materialized closure — {} asserted, {} total loaded",
                stats.asserted, stats.loaded
            );
            total = stats.loaded;
        }
        #[cfg(not(feature = "reasoner"))]
        {
            anyhow::bail!("--materialize requires the `reasoner` feature");
        }
    } else {
        let mut loaded: u64 = 0;
        for f in &files {
            // One `load_duration_seconds`/`load_bytes` observation per file
            // (cf. the materialize branch, which records once per batch).
            let bytes = std::fs::metadata(f).map(|m| m.len()).unwrap_or(0);
            let t = Instant::now();
            let n = load_file(&mut store, f).with_context(|| format!("loading {}", f.display()))?;
            horndb_metrics::metrics().storage.load_bytes.inc_by(bytes);
            horndb_metrics::metrics()
                .storage
                .load_duration_seconds
                .observe(t.elapsed().as_secs_f64());
            eprintln!("serve: loaded {n} triples from {}", f.display());
            loaded += n;
        }
        total = loaded;
    }

    let state = AppState::<HornBackend> {
        store: Arc::new(RwLock::new(store)),
        cfg: sparql_cfg,
    };

    // Scrape-time storage size collector: reads a cheap stats snapshot through
    // a `Weak` ref to the live store. Steady-state cost is zero; the gauges are
    // computed only when /metrics is scraped (and report nothing once the store
    // is dropped).
    let store_weak = Arc::downgrade(&state.store);
    horndb_metrics::register_collector(Box::new(horndb_metrics::storage::StorageCollector::new(
        move || {
            let arc = store_weak.upgrade()?;
            let guard = arc.read().ok()?;
            let s = guard.storage_stats();
            Some(horndb_metrics::storage::StorageSnapshot {
                triples: s.triples as i64,
                graphs: s.graphs as i64,
                predicates: s.predicates as i64,
                dictionary_terms: s.dictionary_terms as i64,
                tier_bytes_estimated: s.bytes_estimated as i64,
            })
        },
    )));

    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind(&cfg.server.bind)
        .await
        .with_context(|| format!("binding {}", cfg.server.bind))?;
    let local = listener.local_addr().context("reading bound address")?;
    eprintln!("serve: {total} triples loaded; SPARQL query endpoint at http://{local}/query");

    axum::serve(listener, app)
        .await
        .context("axum serve loop")?;
    Ok(())
}

/// Run the `horndb-simd` startup calibration and publish the chosen kernel/ISA
/// per primitive as `horndb_simd_kernel_isa` gauges (1 on the active series).
/// The metrics crate keeps its own `SimdKernel`/`SimdIsa`/`SimdSource` label
/// enums, so this is the one place the two type universes are bridged.
fn record_simd_calibration() {
    use horndb_metrics::labels::{SimdIsa, SimdKernel, SimdSource};

    horndb_simd::init();
    match horndb_simd::cpu_identity() {
        Some(cpu) => eprintln!("serve: SIMD host CPU — {cpu}"),
        None => eprintln!("serve: SIMD host CPU — unidentified (calibration-only)"),
    }
    let metrics = horndb_metrics::metrics();
    for (kernel, isa, source) in horndb_simd::calibration_report() {
        let simd_kernel = match kernel {
            horndb_simd::Kernel::Intersect => SimdKernel::Intersect,
            horndb_simd::Kernel::LowerBound => SimdKernel::LowerBound,
            horndb_simd::Kernel::Merge => SimdKernel::Merge,
            horndb_simd::Kernel::Dedup => SimdKernel::Dedup,
            horndb_simd::Kernel::FilterRange => SimdKernel::FilterRange,
            horndb_simd::Kernel::FilterIndicesEq => SimdKernel::FilterIndicesEq,
            horndb_simd::Kernel::Gather => SimdKernel::Gather,
        };
        let simd_isa = match isa {
            horndb_simd::Isa::Scalar => SimdIsa::Scalar,
            horndb_simd::Isa::Avx2 => SimdIsa::Avx2,
            horndb_simd::Isa::Avx512 => SimdIsa::Avx512,
            horndb_simd::Isa::Neon => SimdIsa::Neon,
        };
        let simd_source = match source {
            horndb_simd::Source::Table => SimdSource::Table,
            horndb_simd::Source::Calibrated => SimdSource::Calibrated,
            horndb_simd::Source::Static => SimdSource::Static,
        };
        eprintln!(
            "serve: SIMD calibration — {} -> {:?} ({})",
            kernel.name(),
            isa,
            source.name()
        );
        metrics.simd.record(simd_kernel, simd_isa, simd_source);
    }
}

/// Recursively collect `.nt`/`.ttl` files under `path` (or `path` itself
/// if it is a regular file).
fn collect_data_files(path: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let meta = std::fs::metadata(path)?;
    if meta.is_file() {
        out.push(path.to_path_buf());
        return Ok(());
    }
    if meta.is_dir() {
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let p = entry.path();
            if p.is_dir() {
                collect_data_files(&p, out)?;
            } else if matches!(
                p.extension().and_then(|e| e.to_str()),
                Some("nt") | Some("ttl")
            ) {
                out.push(p);
            }
        }
    }
    Ok(())
}

/// Parse one file and bulk-insert all triples into the store in a single
/// batch (O(n) partitions rebuilt, not O(n²)). Returns the number of
/// newly-live triples. Format is chosen by extension; anything other than
/// `.ttl` is parsed as N-Triples.
fn load_file(store: &mut HornBackend, path: &Path) -> Result<u64> {
    let reader = std::io::BufReader::new(std::fs::File::open(path)?);
    let is_turtle = path.extension().and_then(|e| e.to_str()) == Some("ttl");
    let mut batch: Vec<(OxTerm, OxTerm, OxTerm)> = Vec::new();
    if is_turtle {
        for triple in TurtleParser::new().for_reader(reader) {
            let t = triple.with_context(|| format!("parsing {}", path.display()))?;
            batch.push((
                named_or_blank_to_term(&t.subject),
                OxTerm::NamedNode(t.predicate),
                t.object,
            ));
        }
    } else {
        for triple in NTriplesParser::new().for_reader(reader) {
            let t = triple.with_context(|| format!("parsing {}", path.display()))?;
            batch.push((
                named_or_blank_to_term(&t.subject),
                OxTerm::NamedNode(t.predicate),
                t.object,
            ));
        }
    }
    store
        .insert_oxrdf_batch(batch)
        .with_context(|| format!("bulk inserting triples from {}", path.display()))
}

fn named_or_blank_to_term(n: &NamedOrBlankNode) -> OxTerm {
    match n {
        NamedOrBlankNode::NamedNode(nn) => OxTerm::NamedNode(nn.clone()),
        NamedOrBlankNode::BlankNode(b) => OxTerm::BlankNode(b.clone()),
    }
}

/// Parse one file and collect each triple into an `oxrdf::Dataset` (default
/// graph). Returns the number of triples inserted. Used by `--materialize`.
#[cfg(feature = "reasoner")]
fn collect_into_dataset(path: &Path, dataset: &mut oxrdf::Dataset) -> Result<usize> {
    let reader = std::io::BufReader::new(std::fs::File::open(path)?);
    let is_turtle = path.extension().and_then(|e| e.to_str()) == Some("ttl");
    let mut count = 0usize;
    if is_turtle {
        for triple in TurtleParser::new().for_reader(reader) {
            let t = triple?;
            dataset.insert(&Quad::new(
                t.subject,
                t.predicate,
                t.object,
                GraphName::DefaultGraph,
            ));
            count += 1;
        }
    } else {
        for triple in NTriplesParser::new().for_reader(reader) {
            let t = triple?;
            dataset.insert(&Quad::new(
                t.subject,
                t.predicate,
                t.object,
                GraphName::DefaultGraph,
            ));
            count += 1;
        }
    }
    Ok(count)
}

#[cfg(test)]
mod load_inputs_tests {
    use super::*;

    /// A minimal `Cli` with every value flag unset — the "nothing passed on
    /// argv" case.
    fn bare_cli() -> Cli {
        Cli {
            data: vec![PathBuf::from("x.nt")],
            config: None,
            bind: None,
            simd_max_isa: None,
            simd_autotune: None,
            materialize: false,
        }
    }

    // Deliberately does NOT touch `HORNDB_CONFIG` (or any env var): unit tests
    // in this module run concurrently within the same process, and mutating
    // process-global env state here would race with sibling tests. Env-driven
    // file-location precedence (file < `HORNDB_CONFIG` < `--config`) is
    // covered by the subprocess-level integration tests in
    // `tests/serve_config_wiring.rs`, which each own a dedicated process.

    #[test]
    fn absent_value_flags_leave_overrides_none() {
        let inputs = load_inputs(&bare_cli());
        assert_eq!(inputs.cli_overrides.bind, None);
        assert_eq!(inputs.cli_overrides.simd_max_isa, None);
        assert_eq!(inputs.cli_overrides.simd_autotune, None);
    }

    #[test]
    fn present_value_flags_land_in_overrides() {
        let mut cli = bare_cli();
        cli.bind = Some("0.0.0.0:9".to_string());
        cli.simd_max_isa = Some("scalar".to_string());
        cli.simd_autotune = Some(false);

        let inputs = load_inputs(&cli);
        assert_eq!(inputs.cli_overrides.bind.as_deref(), Some("0.0.0.0:9"));
        assert_eq!(inputs.cli_overrides.simd_max_isa.as_deref(), Some("scalar"));
        assert_eq!(inputs.cli_overrides.simd_autotune, Some(false));
    }

    #[test]
    fn cli_config_path_maps_through_unchanged() {
        let mut cli = bare_cli();
        cli.config = Some(PathBuf::from("/tmp/horndb-test-config.toml"));

        let inputs = load_inputs(&cli);
        assert_eq!(
            inputs.cli_config_path,
            Some(PathBuf::from("/tmp/horndb-test-config.toml"))
        );
    }

    #[test]
    fn absent_config_flag_leaves_cli_config_path_none() {
        let inputs = load_inputs(&bare_cli());
        assert_eq!(inputs.cli_config_path, None);
    }
}
