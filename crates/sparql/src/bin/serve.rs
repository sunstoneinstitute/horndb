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

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Parser;
use oxrdf::{GraphName, NamedOrBlankNode, Quad, Term as OxTerm};
use oxttl::{NTriplesParser, TurtleParser};
use std::sync::{Arc, RwLock};

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

    /// Address to bind, e.g. `127.0.0.1:7878`.
    #[arg(long = "bind", default_value = "127.0.0.1:7878")]
    bind: String,

    /// Run OWL 2 RL materialization over the loaded data and serve the
    /// closure (requires the `reasoner` feature, on by default).
    #[arg(long = "materialize", default_value_t = false)]
    materialize: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

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
            for f in &files {
                let n = collect_into_dataset(f, &mut dataset)
                    .with_context(|| format!("loading {}", f.display()))?;
                eprintln!("serve: parsed {n} triples from {}", f.display());
            }
            let stats = horndb_sparql::exec::horn::load_with_reasoning(&mut store, &dataset)
                .context("OWL 2 RL materialization")?;
            eprintln!(
                "serve: materialized closure — {} asserted, {} total loaded",
                stats.asserted, stats.loaded
            );
            total = store.len();
        }
        #[cfg(not(feature = "reasoner"))]
        {
            anyhow::bail!("--materialize requires the `reasoner` feature");
        }
    } else {
        for f in &files {
            let n = load_file(&mut store, f).with_context(|| format!("loading {}", f.display()))?;
            eprintln!("serve: loaded {n} triples from {}", f.display());
        }
        total = store.len();
    }

    let state = AppState::<HornBackend> {
        store: Arc::new(RwLock::new(store)),
    };
    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind(&cli.bind)
        .await
        .with_context(|| format!("binding {}", cli.bind))?;
    let local = listener.local_addr().context("reading bound address")?;
    eprintln!("serve: {total} triples loaded; SPARQL query endpoint at http://{local}/query");

    axum::serve(listener, app)
        .await
        .context("axum serve loop")?;
    Ok(())
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

/// Parse one file and insert each triple into the store. Returns the
/// number of triples inserted. Format is chosen by extension; anything
/// other than `.ttl` is parsed as N-Triples.
fn load_file(store: &mut HornBackend, path: &Path) -> Result<usize> {
    let reader = std::io::BufReader::new(std::fs::File::open(path)?);
    let is_turtle = path.extension().and_then(|e| e.to_str()) == Some("ttl");
    let mut count = 0usize;
    if is_turtle {
        for triple in TurtleParser::new().for_reader(reader) {
            let t = triple?;
            let s = named_or_blank_to_term(&t.subject);
            let p = OxTerm::NamedNode(t.predicate);
            store
                .insert_oxrdf(&s, &p, &t.object)
                .with_context(|| format!("inserting triple from {}", path.display()))?;
            count += 1;
        }
    } else {
        for triple in NTriplesParser::new().for_reader(reader) {
            let t = triple?;
            let s = named_or_blank_to_term(&t.subject);
            let p = OxTerm::NamedNode(t.predicate);
            store
                .insert_oxrdf(&s, &p, &t.object)
                .with_context(|| format!("inserting triple from {}", path.display()))?;
            count += 1;
        }
    }
    Ok(count)
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
