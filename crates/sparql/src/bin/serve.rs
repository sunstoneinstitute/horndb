//! `serve` — a thin HTTP wrapper that loads one or more RDF files into the
//! dictionary-encoded `HornBackend` and exposes the SPARQL 1.1 query endpoint
//! built by [`horndb_sparql::server::build_router`].
//!
//! Pass `--materialize` to run OWL 2 RL forward-chaining over the loaded data
//! before serving (requires the `reasoner` feature, on by default). If the
//! closure turns out to be inconsistent (some individual inferred to be
//! `owl:Nothing`), `[reasoning].on_inconsistency` decides what happens — see
//! `apply_inconsistency_policy`.
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

// HDB-86 E1: snmalloc as the process-wide allocator. Measured on the trainmarks
// bulk load (hornbench, xlarge): -10.6% on the `parse` phase and -6.3%
// end-to-end, because freeing on a different thread than allocated no longer
// takes the owning arena's lock. Build without the `snmalloc` feature to fall
// back to the system allocator.
#[cfg(feature = "snmalloc")]
#[global_allocator]
static GLOBAL: snmalloc_rs::SnMalloc = snmalloc_rs::SnMalloc;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Parser;
use horndb_config::{CliOverrides, LoadInputs, OnInconsistency};
#[cfg(feature = "reasoner")]
use oxrdf::{GraphName, Quad};
use oxrdf::{NamedOrBlankNode, Term as OxTerm};
use oxttl::{NTriplesParser, TurtleParser};
// HDB-113: every file loaded into a `serve --data <dir>` store is renamed
// per document so blank-node labels from different files never collide.
use horndb_storage::loader::{scope_blank_node, scope_term};
use parking_lot::RwLock;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

use horndb_sparql::exec::horn::HornBackend;
use horndb_sparql::server::{build_router, AppState, Limits};

#[derive(Parser, Debug)]
#[command(
    name = "serve",
    about = "Load flat RDF file(s) into the HornBackend store and serve SPARQL 1.1 over HTTP."
)]
struct Cli {
    /// One or more N-Triples (`.nt`), Turtle (`.ttl`), N-Quads (`.nq`), or
    /// TriG (`.trig`) files, or directories containing them, to load into the
    /// store. Repeatable. `.nq`/`.trig` are dataset (quad) formats: each
    /// quad loads into the named graph it carries, not the default graph.
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

    /// Override `[server].shutdown_drain` (HDB-124), e.g. `10s`. How long a
    /// graceful shutdown (SIGTERM/SIGINT) waits for in-flight requests to
    /// finish before the process force-exits. Wins over the config file;
    /// leave unset to use the resolved config value (default `30s`).
    #[arg(long = "shutdown-drain")]
    shutdown_drain: Option<String>,
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
            shutdown_drain: cli.shutdown_drain.clone(),
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

    // SPEC-30 §S6: the applied-position slot's startup gauges. The P1 store
    // is fully in-memory (no `--data` file carries a slot), so every process
    // starts with no slot to recover — `generation` and `recovery_gap_seconds`
    // are simply 0. They become non-trivial once P3/P4 give the slot
    // something durable to survive a restart in.
    record_feed_startup_metrics();

    // SPEC-29 D9: `[reasoning]`'s cross-key rules (a pattern reaching into the
    // reserved namespace, spine/select overlap, an unimplemented phase) are
    // domain checks serde cannot make, so — like `[simd].max_isa` above — they
    // land here and are startup-fatal.
    #[cfg(feature = "reasoner")]
    for warning in horndb_sparql::reasoning::validate(&cfg.reasoning)
        .map_err(|e| anyhow::anyhow!("invalid [reasoning] configuration: {e}"))?
    {
        eprintln!("serve: warning: {warning}");
    }
    #[cfg(not(feature = "reasoner"))]
    if cfg.reasoning.enabled {
        anyhow::bail!("[reasoning].enabled requires the `reasoner` feature");
    }

    let mut files: Vec<PathBuf> = Vec::new();
    for path in &cli.data {
        collect_data_files(path, &mut files)
            .with_context(|| format!("enumerating {}", path.display()))?;
    }
    if files.is_empty() {
        anyhow::bail!("no .nt/.ttl/.nq/.trig files found in the provided --data paths");
    }
    if cli.materialize && files.iter().any(|f| is_dataset_format(f)) {
        // --materialize parses every file into one oxrdf::Dataset default
        // graph before closure (see collect_into_dataset below); it does not
        // yet preserve named graphs from a dataset-format input. Fail fast
        // with a clear message rather than let NTriplesParser choke on
        // N-Quads' extra graph term.
        anyhow::bail!(
            "--materialize does not yet support .nq/.trig named-graph inputs \
             (they would collapse into the default graph); load them without --materialize"
        );
    }
    // Fail fast on a static misconfiguration (materialize requested, feature
    // off) before the socket binds — same as before HDB-124 moved the actual
    // load to a background task, where this would only surface after the
    // process was already answering `/healthz`.
    if cli.materialize && !cfg!(feature = "reasoner") {
        anyhow::bail!("--materialize requires the `reasoner` feature");
    }
    // SPEC-28 S5: the Graph Store Protocol refuses whole-graph writes to
    // `?default` on a materialized store, where asserted and inferred
    // triples share the default graph indistinguishably.
    if cli.materialize {
        horndb_sparql::server::flag_materialized();
    }

    // HDB-118: admission control + request-body cap from `[server.limits]`.
    // A zero slot count is startup-fatal rather than silently clamped —
    // `usize` gives serde no lower bound to reject it for us. Checked here,
    // with the other static misconfigurations, so it fails before the socket
    // binds.
    if cfg.server.limits.max_concurrent_queries == 0 {
        anyhow::bail!("[server.limits].max_concurrent_queries must be at least 1");
    }
    let admission = Limits::new(
        cfg.server.limits.max_concurrent_queries,
        cfg.server.limits.queue_timeout.0,
        cfg.server.limits.max_request_body.0 as usize,
    );

    // SPEC-26 S3: publish the resolved config behind an `ArcSwap` and start the
    // file watcher over it. Handlers snapshot the handle per request, so a hot
    // key (`[server.limits]` bar the three admission keys, `[logging]`,
    // `[reload]`) edited on disk takes effect on the next request. A restart-only
    // key is stored — a later restart honours it — and logged as needing a
    // restart; in particular the watcher never re-applies `[simd]`, whose ISA
    // selection and calibration already ran above.
    //
    // The watcher guard must outlive `axum::serve`: dropping it stops following
    // file edits.
    let config_handle = horndb_config::ConfigHandle::new(cfg.clone());
    let _config_watcher = horndb_config::watch(inputs, config_handle.clone())
        .context("starting the config reload watcher")?;

    // HDB-124: bind and start serving BEFORE the (potentially multi-minute,
    // no-persistence-yet) data load, so `/healthz` (process up) and `/readyz`
    // (503 until loaded) are both reachable during the load — a Kubernetes
    // readiness probe must be able to see "up but not ready", not just
    // "connection refused", or the pod never leaves the load balancer's
    // rotation cleanly and a slow load can look indistinguishable from a
    // dead process.
    let store = Arc::new(RwLock::new(HornBackend::new()));
    let ready = Arc::new(AtomicBool::new(false));
    // SPEC-26 S2/S3: the server holds the live config; its `[server.limits]`
    // are the *defaults* each request layers its own URL/form overrides on top
    // of (S4). No domain check is needed here — unlike the free-string
    // `[simd].max_isa` above, every limits field is typed, so a bad value
    // already failed `horndb_config::load()`, with file+key attribution.
    let state = AppState::<HornBackend> {
        store: Arc::clone(&store),
        config: config_handle.clone(),
        ready: Arc::clone(&ready),
        admission,
    };

    // Scrape-time storage size collector: reads a stats snapshot through a
    // `Weak` ref to the live store. Nothing is paid between scrapes; the gauges
    // are computed only when /metrics is scraped (and report nothing once the
    // store is dropped). The scrape is not always cheap, though: it takes the
    // store's read guard for the whole snapshot, and since HDB-84 the first
    // read after a batched write merges that partition's runs. So a scrape
    // landing just after a bulk load can hold this guard — and therefore block
    // every other reader and writer of the store — for the merge. See
    // `horndb_metrics::storage::StorageSnapshot`.
    let store_weak = Arc::downgrade(&state.store);
    horndb_metrics::register_collector(Box::new(horndb_metrics::storage::StorageCollector::new(
        move || {
            let arc = store_weak.upgrade()?;
            let guard = arc.read();
            let s = guard.storage_stats();
            Some(horndb_metrics::storage::StorageSnapshot {
                triples: s.triples as i64,
                graphs: s.graphs as i64,
                predicates: s.predicates as i64,
                dictionary_terms: s.dictionary_terms as i64,
                dictionary_terms_live: s.dictionary_terms_live as i64,
                dictionary_bytes: s.dictionary_bytes as i64,
                tier_bytes_estimated: s.bytes_estimated as i64,
            })
        },
    )));

    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind(&cfg.server.bind)
        .await
        .with_context(|| format!("binding {}", cfg.server.bind))?;
    let local = listener.local_addr().context("reading bound address")?;
    eprintln!(
        "serve: listening at http://{local} — loading {} file(s) in the background; \
         /readyz reports 503 until the load (and any --materialize pass) finishes",
        files.len()
    );

    // Load on a blocking-pool thread (parsing/materializing is sync CPU/IO
    // work, not async), then swap the populated store in and flip `ready` —
    // the "real signal" HDB-124 asks for, set once, at the end of the load.
    let materialize = cli.materialize;
    let on_inconsistency = cfg.reasoning.on_inconsistency;
    let reasoning_backend = cfg.reasoning.backend;
    #[cfg(feature = "reasoner")]
    let reasoning = cfg.reasoning.clone();
    tokio::task::spawn_blocking(move || {
        match run_load(materialize, &files, reasoning_backend, on_inconsistency) {
            Ok((loaded_store, total)) => {
                *store.write() = loaded_store;
                // SPEC-29 P1's view materializer. The first pass runs before
                // `ready` flips, so the first request that sees /readyz green
                // already sees derived quads; a background thread then polls
                // for staleness.
                #[cfg(feature = "reasoner")]
                if reasoning.enabled {
                    let store_arc = Arc::clone(&store);
                    let mut mgr = horndb_sparql::reasoning::ViewManager::new(&reasoning);
                    match mgr.run_until_clean(&mut store_arc.write()) {
                        Ok(derived) => eprintln!("serve: reasoning derived {derived} view(s)"),
                        Err(e) => {
                            eprintln!(
                                "serve: fatal: initial reasoning-view derivation failed: {e}"
                            );
                            std::process::exit(1);
                        }
                    }
                    // ponytail: a poll loop that takes the write lock each
                    // tick. Cheap while clean (one graph list + two small
                    // scans), and P2 replaces the whole re-derive step with
                    // the incremental path anyway. Move to a condvar signalled
                    // by the write funnel if the tick ever shows up in a
                    // latency profile.
                    std::thread::spawn(move || loop {
                        std::thread::sleep(std::time::Duration::from_millis(250));
                        if let Err(e) = mgr.run_until_clean(&mut store_arc.write()) {
                            eprintln!("serve: reasoning derivation failed: {e}");
                        }
                    });
                }
                ready.store(true, std::sync::atomic::Ordering::Release);
                eprintln!("serve: {total} triples loaded; ready");
            }
            Err(e) => {
                // The original (pre-HDB-124) behavior treated a load failure as
                // fatal at startup. The socket is already bound now, so mirror
                // that by tearing the whole process down rather than serving
                // forever against an empty, permanently-not-ready store.
                eprintln!("serve: fatal: data load failed: {e:#}");
                std::process::exit(1);
            }
        }
    });

    let drain = cfg.server.shutdown_drain.0;
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let serve_task = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
    });

    wait_for_shutdown_signal().await;
    eprintln!("serve: shutdown signal received; draining in-flight requests (up to {drain:?})");
    // Stop accepting new connections and let in-flight requests finish; a
    // dropped receiver (task already gone) is fine, `with_graceful_shutdown`
    // resolves immediately either way.
    let _ = shutdown_tx.send(());

    match tokio::time::timeout(drain, serve_task).await {
        Ok(Ok(Ok(()))) => eprintln!("serve: drained cleanly"),
        Ok(Ok(Err(e))) => return Err(e).context("axum serve loop"),
        Ok(Err(join_err)) => anyhow::bail!("server task panicked: {join_err}"),
        Err(_elapsed) => {
            eprintln!("serve: drain timeout ({drain:?}) exceeded; forcing exit");
            std::process::exit(1);
        }
    }
    Ok(())
}

/// Wait for SIGTERM or SIGINT (Ctrl+C). `axum::serve(...).with_graceful_shutdown`
/// stops accepting new connections once this resolves; in-flight requests are
/// then given up to `[server].shutdown_drain` to finish (enforced by the
/// `tokio::time::timeout` around the server task in `main`).
async fn wait_for_shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install the Ctrl+C (SIGINT) handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install the SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}

/// Parse `files` and either bulk-load them directly or run OWL 2 RL
/// materialization first (`materialize`), returning the populated store and
/// the total triple count. Extracted from `main` so it can run on a
/// `spawn_blocking` thread (HDB-124: the socket binds before this runs) and
/// so the parse/materialize/load sequencing stays unit-testable in
/// isolation from the HTTP boot sequence.
///
/// `reasoning_backend` (`[reasoning].backend`) and `on_inconsistency`
/// (`[reasoning].on_inconsistency`) are read from the config in `main` and
/// passed down because this runs off the main thread. Under `reject-startup`
/// the error returned here is fatal in the caller — the socket is already
/// bound by then (HDB-124), so the process exits rather than never binding.
fn run_load(
    materialize: bool,
    files: &[PathBuf],
    reasoning_backend: horndb_config::ReasoningBackend,
    on_inconsistency: OnInconsistency,
) -> Result<(HornBackend, u64)> {
    let mut store = HornBackend::new();
    let total;

    if materialize {
        #[cfg(feature = "reasoner")]
        {
            // Resolve `[reasoning].backend` before parsing anything: an
            // unbuildable choice is startup-fatal, not a surprise after a long
            // load.
            let (closure, backend_label) = resolve_reasoning_backend(reasoning_backend)?;
            eprintln!("serve: reasoning backend — {}", backend_label.as_str());
            horndb_metrics::metrics()
                .owlrl
                .record_backend(backend_label);

            // Parse all files into an oxrdf::Dataset, then run the OWL 2 RL
            // closure before loading into the served store.
            let mut dataset = oxrdf::Dataset::default();
            let mut input_bytes: u64 = 0;
            let t = Instant::now();
            for f in files {
                input_bytes += std::fs::metadata(f).map(|m| m.len()).unwrap_or(0);
                let n = collect_into_dataset(&store, f, &mut dataset)
                    .with_context(|| format!("loading {}", f.display()))?;
                eprintln!("serve: parsed {n} triples from {}", f.display());
            }
            let stats =
                horndb_sparql::exec::horn::load_with_reasoning(&mut store, &dataset, closure)
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
            apply_inconsistency_policy(on_inconsistency, &stats.inconsistent)?;
            total = stats.loaded;
        }
        #[cfg(not(feature = "reasoner"))]
        {
            let _ = reasoning_backend;
            anyhow::bail!("--materialize requires the `reasoner` feature");
        }
    } else {
        // No closure without --materialize: neither reasoning key applies.
        let _ = (reasoning_backend, on_inconsistency);
        let mut loaded: u64 = 0;
        for f in files {
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

    Ok((store, total))
}

/// HDB-125: surface an OWL 2 RL inconsistency (some individual inferred to be
/// `owl:Nothing`) instead of silently serving from an unsound closure.
///
/// Always publishes the `horndb_reasoning_inconsistent` gauge and, when
/// inconsistent, logs the witnesses (already capped by
/// `load_with_reasoning`). Then applies `[reasoning].on_inconsistency`:
/// `warn` serves anyway, `reject-startup` returns an error that tears the
/// process down before it ever reports ready, and `serve-with-flag` serves
/// with `x-horndb-inconsistent: true` on every response.
///
/// The witnesses are logged rather than exposed through the SPEC-27 provenance
/// view — that view is HDB-66, not yet landed.
#[cfg(feature = "reasoner")]
fn apply_inconsistency_policy(policy: OnInconsistency, witnesses: &[String]) -> Result<()> {
    horndb_metrics::metrics()
        .owlrl
        .reasoning_inconsistent
        .set(i64::from(!witnesses.is_empty()));
    if witnesses.is_empty() {
        return Ok(());
    }
    eprintln!(
        "serve: WARNING — OWL 2 RL inconsistency: {} individual(s) inferred to be owl:Nothing \
         (showing up to {}): {}",
        witnesses.len(),
        horndb_sparql::exec::horn::INCONSISTENT_WITNESS_CAP,
        witnesses.join(", ")
    );
    match policy {
        OnInconsistency::Warn => Ok(()),
        OnInconsistency::RejectStartup => anyhow::bail!(
            "refusing to serve an inconsistent closure ([reasoning].on_inconsistency = reject-startup)"
        ),
        OnInconsistency::ServeWithFlag => {
            horndb_sparql::server::flag_inconsistent();
            eprintln!(
                "serve: every response will carry {}: true",
                horndb_sparql::server::INCONSISTENT_HEADER
            );
            Ok(())
        }
    }
}

/// Map `[reasoning].backend` onto the `horndb-owlrl` closure backend and its
/// metrics label. Which backend closes the transitive/equivalence rules is the
/// only difference — every other OWL 2 RL rule is compiled rule firing either
/// way, so `graphblas` *is* the hybrid split (GraphBLAS closure + compiled
/// rules), and the two produce the same triple set
/// (`crates/owlrl/tests/closure_backend_differential.rs`).
///
/// `graphblas` only exists in a binary built with the `graphblas` feature,
/// which links SuiteSparse:GraphBLAS. Selecting it otherwise is startup-fatal
/// and names the feature rather than silently falling back to the slow path.
#[cfg(feature = "reasoner")]
fn resolve_reasoning_backend(
    configured: horndb_config::ReasoningBackend,
) -> Result<(
    horndb_owlrl::BackendChoice,
    horndb_metrics::labels::ReasoningBackend,
)> {
    use horndb_config::ReasoningBackend as Cfg;
    use horndb_metrics::labels::ReasoningBackend as Label;
    Ok(match configured {
        Cfg::RuleFiring => (horndb_owlrl::BackendChoice::RuleFiring, Label::RuleFiring),
        #[cfg(feature = "graphblas")]
        Cfg::GraphBlas => (horndb_owlrl::BackendChoice::GraphBlas, Label::GraphBlas),
        #[cfg(not(feature = "graphblas"))]
        Cfg::GraphBlas => anyhow::bail!(
            "[reasoning].backend = \"graphblas\" requires a build with the `graphblas` feature \
             (cargo build -p horndb-sparql --features graphblas)"
        ),
    })
}

/// SPEC-30 §S6: the applied-position slot's startup-observability gauges.
/// Called once, before the store exists — P1's store starts empty every
/// time (no persistence to recover a slot from), so both values are always
/// 0 today; a real value is P3/P4's job.
fn record_feed_startup_metrics() {
    let feed = &horndb_metrics::metrics().feed;
    feed.generation.set(0);
    feed.recovery_gap_seconds.set(0);
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

/// Recursively collect `.nt`/`.ttl`/`.nq`/`.trig` files under `path` (or
/// `path` itself if it is a regular file).
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
                Some("nt") | Some("ttl") | Some("nq") | Some("trig")
            ) {
                out.push(p);
            }
        }
    }
    Ok(())
}

/// True if `path`'s extension names a dataset (quad) serialization —
/// `.nq`/`.trig` — as opposed to a triples format (`.nt`/`.ttl`).
fn is_dataset_format(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("nq") | Some("trig")
    )
}

/// Parse one file and bulk-insert its data into the store in a single batch
/// (O(n) partitions rebuilt, not O(n²)). Returns the number of newly-live
/// triples/quads. Format is chosen by extension: `.nq`/`.trig` route through
/// [`horndb_sparql::update::parse_rdf_bytes`] (the same parser call site
/// `LOAD` uses) so each quad lands in the named graph it carries; anything
/// else is parsed here directly, `.ttl` as Turtle and everything else
/// (including `.nt`) as N-Triples, all landing in the default graph.
///
/// Blank-node labels are document-scoped (HDB-113): `serve --data <dir>`
/// loads several files into one store, so every blank node parsed from this
/// file is renamed with a fresh per-file tag before it reaches the store —
/// otherwise `_:b1` in two different files would land on the same node. The
/// dataset path passes that tag to `parse_rdf_bytes`; the triples path
/// applies `horndb_storage::loader::scope_blank_node` here.
fn load_file(store: &mut HornBackend, path: &Path) -> Result<u64> {
    if is_dataset_format(path) {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let extension = path.extension().and_then(|e| e.to_str());
        // A `file://` base IRI (the repo-wide convention, e.g.
        // `crates/harness/src/rdf.rs`) so a relative IRI in `.trig` resolves;
        // `.nq` ignores it (N-Quads requires absolute IRIs).
        let base = format!("file://{}", path.display());
        let tag = store.next_bnode_doc_tag();
        let quads = horndb_sparql::update::parse_rdf_bytes(tag, &bytes, extension, &base)
            .with_context(|| format!("parsing {}", path.display()))?;
        let n = quads.len() as u64;
        horndb_sparql::exec::Store::apply_quads(store, Vec::new(), quads)
            .with_context(|| format!("bulk inserting quads from {}", path.display()))?;
        return Ok(n);
    }

    let reader = std::io::BufReader::new(std::fs::File::open(path)?);
    let is_turtle = path.extension().and_then(|e| e.to_str()) == Some("ttl");
    let mut batch: Vec<(OxTerm, OxTerm, OxTerm)> = Vec::new();
    let tag = store.next_bnode_doc_tag();
    if is_turtle {
        for triple in TurtleParser::new().for_reader(reader) {
            let t = triple.with_context(|| format!("parsing {}", path.display()))?;
            batch.push((
                named_or_blank_to_term(tag, &t.subject),
                OxTerm::NamedNode(t.predicate),
                scope_term(tag, t.object),
            ));
        }
    } else {
        for triple in NTriplesParser::new().for_reader(reader) {
            let t = triple.with_context(|| format!("parsing {}", path.display()))?;
            batch.push((
                named_or_blank_to_term(tag, &t.subject),
                OxTerm::NamedNode(t.predicate),
                scope_term(tag, t.object),
            ));
        }
    }
    store
        .insert_oxrdf_batch(batch)
        .with_context(|| format!("bulk inserting triples from {}", path.display()))
}

fn named_or_blank_to_term(tag: u64, n: &NamedOrBlankNode) -> OxTerm {
    match n {
        NamedOrBlankNode::NamedNode(nn) => OxTerm::NamedNode(nn.clone()),
        NamedOrBlankNode::BlankNode(b) => OxTerm::BlankNode(scope_blank_node(tag, b.clone())),
    }
}

/// [`scope_blank_node`] for a `NamedOrBlankNode` subject; a named node passes
/// through. Used where the caller needs a `NamedOrBlankNode` rather than the
/// `OxTerm` [`named_or_blank_to_term`] returns (e.g. `Quad::new`'s subject).
#[cfg(feature = "reasoner")]
fn scoped_subject(tag: u64, s: NamedOrBlankNode) -> NamedOrBlankNode {
    match s {
        NamedOrBlankNode::BlankNode(b) => NamedOrBlankNode::BlankNode(scope_blank_node(tag, b)),
        other => other,
    }
}

/// Parse one file and collect each triple into an `oxrdf::Dataset` (default
/// graph). Returns the number of triples inserted. Used by `--materialize`.
///
/// Every file in `--data` feeds the same `dataset` before materialization, so
/// blank nodes are renamed per file here too (HDB-113) — same as
/// [`load_file`]'s non-materialize path.
#[cfg(feature = "reasoner")]
fn collect_into_dataset(
    store: &HornBackend,
    path: &Path,
    dataset: &mut oxrdf::Dataset,
) -> Result<usize> {
    let reader = std::io::BufReader::new(std::fs::File::open(path)?);
    let is_turtle = path.extension().and_then(|e| e.to_str()) == Some("ttl");
    let mut count = 0usize;
    let tag = store.next_bnode_doc_tag();
    if is_turtle {
        for triple in TurtleParser::new().for_reader(reader) {
            let t = triple?;
            dataset.insert(&Quad::new(
                scoped_subject(tag, t.subject),
                t.predicate,
                scope_term(tag, t.object),
                GraphName::DefaultGraph,
            ));
            count += 1;
        }
    } else {
        for triple in NTriplesParser::new().for_reader(reader) {
            let t = triple?;
            dataset.insert(&Quad::new(
                scoped_subject(tag, t.subject),
                t.predicate,
                scope_term(tag, t.object),
                GraphName::DefaultGraph,
            ));
            count += 1;
        }
    }
    Ok(count)
}

#[cfg(all(test, feature = "reasoner"))]
mod reasoning_backend_tests {
    use super::*;
    use horndb_config::ReasoningBackend as Cfg;
    use horndb_metrics::labels::ReasoningBackend as Label;

    #[test]
    fn rule_firing_maps_to_the_rule_firing_backend() {
        let (choice, label) = resolve_reasoning_backend(Cfg::RuleFiring).unwrap();
        assert_eq!(choice, horndb_owlrl::BackendChoice::RuleFiring);
        assert_eq!(label.as_str(), Label::RuleFiring.as_str());
    }

    /// With the `graphblas` feature on, `backend = "graphblas"` resolves to the
    /// GraphBLAS closure; without it, startup fails naming the feature so the
    /// operator is never silently served the slow path.
    #[test]
    fn graphblas_resolves_or_names_the_missing_feature() {
        let resolved = resolve_reasoning_backend(Cfg::GraphBlas);
        #[cfg(feature = "graphblas")]
        {
            let (choice, label) = resolved.unwrap();
            assert_eq!(choice, horndb_owlrl::BackendChoice::GraphBlas);
            assert_eq!(label.as_str(), Label::GraphBlas.as_str());
        }
        #[cfg(not(feature = "graphblas"))]
        {
            let err = resolved.unwrap_err().to_string();
            assert!(err.contains("graphblas"), "{err}");
            assert!(err.contains("feature"), "{err}");
        }
    }
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
            shutdown_drain: None,
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
        assert_eq!(inputs.cli_overrides.shutdown_drain, None);
    }

    #[test]
    fn present_value_flags_land_in_overrides() {
        let mut cli = bare_cli();
        cli.bind = Some("0.0.0.0:9".to_string());
        cli.simd_max_isa = Some("scalar".to_string());
        cli.simd_autotune = Some(false);
        cli.shutdown_drain = Some("10s".to_string());

        let inputs = load_inputs(&cli);
        assert_eq!(inputs.cli_overrides.bind.as_deref(), Some("0.0.0.0:9"));
        assert_eq!(inputs.cli_overrides.simd_max_isa.as_deref(), Some("scalar"));
        assert_eq!(inputs.cli_overrides.simd_autotune, Some(false));
        assert_eq!(inputs.cli_overrides.shutdown_drain.as_deref(), Some("10s"));
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
