//! `harness` — entrypoint for the SPEC-01 conformance & benchmark
//! harness. Used both locally and from GitHub Actions.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use tracing::info;

use horndb_harness::{
    ci::to_junit_xml, db::Db, manifest, report as report_mod, runner::run_selected,
    selected::Selected, stub::StubReasoner, Reasoner, Status,
};

#[derive(Parser, Debug)]
#[command(
    name = "harness",
    version,
    about = "HornDB conformance & benchmark harness"
)]
struct Cli {
    /// Path to workspace root (default: cwd).
    #[arg(long, default_value = ".")]
    workspace: PathBuf,
    /// SQLite result DB. Precedence: `--db` > `$HARNESS_DB` >
    /// `target/harness.sqlite`. Runs append to the DB, so point
    /// `$HARNESS_DB` at a path that persists between invocations (e.g. the
    /// nightly bench runner uses a path outside the ephemeral checkout) to
    /// accumulate a trend series rather than starting fresh each run. The
    /// `prune` subcommand bounds that accumulation to a retention window.
    #[arg(long)]
    db: Option<PathBuf>,
    /// Engine to dispatch against. Stage 0 only supports `stub`.
    #[arg(long, default_value = "stub")]
    engine: String,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(ValueEnum, Clone, Copy, Debug)]
enum ReportFormat {
    Text,
    Markdown,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run the currently-selected subset against the chosen engine.
    Run {
        /// Path to selected.toml (default: harness/selected.toml under workspace).
        #[arg(long)]
        selected: Option<PathBuf>,
        /// Write JUnit XML to this path.
        #[arg(long)]
        junit: Option<PathBuf>,
        /// Treat the run as green even if some tests fail (used by the
        /// stub self-test that deliberately includes a failing case).
        #[arg(long)]
        allow_failing: bool,
    },
    /// Query the trend database.
    Report {
        #[arg(long)]
        suite: String,
        #[arg(long)]
        metric: String,
        /// Output format. `text` is the plain trend dump; `markdown`
        /// emits a GitHub-flavoured table + Mermaid chart suitable for
        /// appending to `$GITHUB_STEP_SUMMARY`.
        #[arg(long, value_enum, default_value_t = ReportFormat::Text)]
        format: ReportFormat,
    },
    /// Walk `--root` and convert every `manifest.rdf` (RDF/XML) into
    /// a sibling `manifest.ttl`. Skips files that already have a .ttl
    /// counterpart. Stage-1 only.
    ConvertManifests {
        #[arg(long)]
        root: PathBuf,
    },
    /// Extract the W3C OWL 2 RL profile aggregate (`profile-RL.rdf`)
    /// into a harness-format `manifest.ttl` plus sibling
    /// `<id>.premise.ttl` / `<id>.conclusion.ttl` files. The W3C file
    /// embeds premise/conclusion ontologies as RDF/XML literals; this
    /// subcommand materialises them as standalone Turtle files so the
    /// in-tree manifest parser can read them.
    ExtractOwl2Rl {
        /// Path to `profile-RL.rdf` (the W3C aggregate).
        #[arg(long)]
        source: PathBuf,
        /// Directory to write `manifest.ttl` and the sibling
        /// `<id>.premise.ttl` / `<id>.conclusion.ttl` files into.
        #[arg(long)]
        out: PathBuf,
    },
    /// List candidate test IDs for a profile from a manifest.
    ListCases {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        profile: String,
        #[arg(long, default_value = "50")]
        max: usize,
    },
    /// Run LDBC SPB driver against an endpoint and record results.
    SpbRun {
        #[arg(long)]
        driver_jar: PathBuf,
        #[arg(long)]
        scenario: PathBuf,
        /// SPARQL query endpoint (overrides `endpointURL`).
        #[arg(long)]
        endpoint: String,
        /// SPARQL update endpoint (overrides `endpointUpdateURL`).
        /// Defaults to the query endpoint when omitted.
        #[arg(long)]
        endpoint_update: Option<String>,
        #[arg(long, default_value_t = 600)]
        duration: u64,
        /// Label used as the `dataset` column so we can A/B
        /// (e.g. "horndb" vs "graphdb-free").
        #[arg(long)]
        label: String,
    },
    /// Delete trend-DB runs (and their outcomes/metrics) older than the
    /// retention window, then VACUUM. The nightly runs this so the
    /// cumulative DB holds a bounded rolling window instead of growing
    /// forever.
    Prune {
        /// Retention window in days; runs started earlier are deleted.
        #[arg(long, default_value_t = 90)]
        keep_days: u32,
    },
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    match real_main() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("harness: error: {e:#}");
            ExitCode::from(2)
        }
    }
}

fn real_main() -> Result<ExitCode> {
    let cli = Cli::parse();
    let workspace = cli
        .workspace
        .canonicalize()
        .unwrap_or(cli.workspace.clone());
    let db_path = cli
        .db
        .or_else(|| std::env::var_os("HARNESS_DB").map(PathBuf::from))
        .unwrap_or_else(|| workspace.join("target/harness.sqlite"));
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let db = Db::open(&db_path)?;

    match cli.cmd {
        Cmd::Run {
            selected,
            junit,
            allow_failing,
        } => {
            let sel_path = selected.unwrap_or_else(|| workspace.join("harness/selected.toml"));
            let sel = Selected::load(&sel_path)?;
            let mut engine: Box<dyn Reasoner> = match cli.engine.as_str() {
                "stub" => Box::new(StubReasoner::new()),
                #[cfg(feature = "real-engine")]
                "owlrl" => Box::new(horndb_owlrl::Engine::new()),
                other => anyhow::bail!("unknown engine: {other}"),
            };
            let commit_sha = std::env::var("GITHUB_SHA").unwrap_or_else(|_| "unknown".into());
            let hw = hardware_fingerprint();
            let run_id = db.start_run(&commit_sha, &hw, engine.name())?;
            info!(run_id = %run_id, "harness run started");

            let report = run_selected(engine.as_mut(), &sel, &workspace, &|p, s| {
                manifest::parse(p, s)
            })?;
            for outcome in &report.outcomes {
                db.record_outcome(&run_id, outcome)?;
            }
            println!(
                "harness: run_id={} passed={} failed={} skipped={}",
                run_id,
                report.passed(),
                report.failed(),
                report.skipped(),
            );
            for o in &report.outcomes {
                let tag = match o.status {
                    Status::Passed => "PASS",
                    Status::Failed => "FAIL",
                    Status::Skipped => "SKIP",
                };
                let reason = o.reason.as_deref().unwrap_or("");
                println!("  [{tag}] {} {} {}", o.suite, o.test_id, reason);
            }
            if let Some(p) = junit {
                std::fs::write(&p, to_junit_xml(&report))
                    .with_context(|| format!("writing junit {}", p.display()))?;
            }
            if report.has_failures() && !allow_failing {
                return Ok(ExitCode::from(1));
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Report {
            suite,
            metric,
            format,
        } => {
            match format {
                ReportFormat::Text => {
                    let t = report_mod::trend(&db, &suite, &metric)?;
                    println!(
                        "trend suite={} metric={} points={} regression={}",
                        t.suite,
                        t.metric,
                        t.points.len(),
                        t.regression_flag,
                    );
                    for p in &t.points {
                        println!("  {} {} {}", p.timestamp, p.run_id, p.value);
                    }
                }
                ReportFormat::Markdown => {
                    let groups = report_mod::series_by_dataset(&db, &suite, &metric)?;
                    print!("{}", report_mod::render_markdown(&suite, &metric, &groups));
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::ConvertManifests { root } => {
            use oxrdfio::{RdfFormat, RdfParser, RdfSerializer};
            let mut count = 0usize;
            for entry in walkdir::WalkDir::new(&root) {
                let entry = entry?;
                if entry.file_name() != "manifest.rdf" {
                    continue;
                }
                let src = entry.path().to_path_buf();
                let dst = src.with_extension("ttl");
                if dst.exists() {
                    continue;
                }
                let base_iri = format!("file://{}", src.display());
                let bytes = std::fs::read(&src)?;
                let parser = RdfParser::from_format(RdfFormat::RdfXml).with_base_iri(&base_iri)?;
                let mut serializer =
                    RdfSerializer::from_format(RdfFormat::Turtle).for_writer(Vec::<u8>::new());
                for quad in parser.for_slice(&bytes) {
                    serializer.serialize_quad(&quad?)?;
                }
                let out = serializer.finish()?;
                std::fs::write(&dst, out)?;
                count += 1;
            }
            println!("converted {count} manifest.rdf → manifest.ttl");
            Ok(ExitCode::SUCCESS)
        }
        Cmd::ExtractOwl2Rl { source, out } => {
            let stats = horndb_harness::owl2_rl_extract::extract(&source, &out)?;
            println!(
                "extracted owl2-rl: scanned={} entries={} ttl_files={} skipped={}",
                stats.cases_scanned,
                stats.entries_emitted,
                stats.turtle_files_written,
                stats.skipped_no_payload,
            );
            Ok(ExitCode::SUCCESS)
        }
        Cmd::ListCases {
            manifest,
            profile,
            max,
        } => {
            // Stage-1 minimal: read the manifest, print the first
            // `max` test IDs (the implementer hand-curates which 50
            // to keep based on rule coverage — see
            // harness/curation/owl2-rl-50.md).
            let suite = if manifest.to_string_lossy().contains("sparql11") {
                horndb_harness::testcase::Suite::Sparql11
            } else {
                horndb_harness::testcase::Suite::Owl2
            };
            let cases = manifest::parse(&manifest, suite)?;
            let _ = profile; // profile filter requires `mf:profile` parsing;
                             // wired by the Stage-1 implementer.
            for case in cases.iter().take(max) {
                println!("{}", case.id);
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::SpbRun {
            driver_jar,
            scenario,
            endpoint,
            endpoint_update,
            duration,
            label,
        } => {
            let cfg = horndb_harness::ldbc_spb::SpbConfig {
                driver_jar: &driver_jar,
                scenario: &scenario,
                endpoint: &endpoint,
                endpoint_update: endpoint_update.as_deref(),
                duration_seconds: duration,
            };
            let result = horndb_harness::ldbc_spb::run(&cfg)?;
            let commit_sha = std::env::var("GITHUB_SHA").unwrap_or_else(|_| "unknown".into());
            let run_id = db.start_run(&commit_sha, &hardware_fingerprint(), &label)?;
            horndb_harness::ldbc_spb::record(&db, &run_id, &label, &result)?;
            println!(
                "spb-run: run_id={run_id} editorial_ops_per_sec={} aggregation_queries_per_sec={} duration_s={}",
                result.editorial_ops_per_sec,
                result.aggregation_queries_per_sec,
                result.run_duration_seconds
            );
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Prune { keep_days } => {
            let stats = db.prune_older_than(keep_days)?;
            println!(
                "prune: keep_days={keep_days} deleted runs={} outcomes={} metrics={}",
                stats.runs, stats.outcomes, stats.metrics
            );
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn hardware_fingerprint() -> String {
    // Stage 0: minimal — OS + arch. Stage 2 deepens this per F7.
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}
