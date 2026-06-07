//! `serve` — a thin HTTP wrapper that loads one or more already-materialized
//! RDF files into the Stage-1 `MemStore` and exposes the SPARQL 1.1 query
//! endpoint built by [`horndb_sparql::server::build_router`].
//!
//! This binary intentionally does **no** reasoning. It loads flat
//! N-Triples / Turtle files (typically produced by
//! `horndb-bench materialize --dump-nt`) so it needs no link against the
//! OWL 2 RL / GraphBLAS stack. Materialization is a separate, heavier step.
//!
//! The SPARQL query endpoint is `http://<bind>/query` (GET or POST) — NOT
//! `/sparql`. SPARQL Update is at `/update`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Parser;
use oxrdf::{NamedOrBlankNode, Term as OxTerm};
use oxttl::{NTriplesParser, TurtleParser};
use std::sync::{Arc, RwLock};

use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::server::{build_router, AppState};

#[derive(Parser, Debug)]
#[command(
    name = "serve",
    about = "Load flat RDF file(s) into an in-memory store and serve SPARQL 1.1 over HTTP."
)]
struct Cli {
    /// One or more N-Triples (`.nt`) or Turtle (`.ttl`) files, or
    /// directories containing them, to load into the store. Repeatable.
    #[arg(long = "data", required = true, num_args = 1..)]
    data: Vec<PathBuf>,

    /// Address to bind, e.g. `127.0.0.1:7878`.
    #[arg(long = "bind", default_value = "127.0.0.1:7878")]
    bind: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let mut store = MemStore::default();
    let mut files: Vec<PathBuf> = Vec::new();
    for path in &cli.data {
        collect_data_files(path, &mut files)
            .with_context(|| format!("enumerating {}", path.display()))?;
    }
    if files.is_empty() {
        anyhow::bail!("no .nt/.ttl files found in the provided --data paths");
    }
    for f in &files {
        let n = load_file(&mut store, f).with_context(|| format!("loading {}", f.display()))?;
        eprintln!("serve: loaded {n} triples from {}", f.display());
    }
    let total = store.len();

    let state = AppState {
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
fn load_file(store: &mut MemStore, path: &Path) -> Result<usize> {
    let reader = std::io::BufReader::new(std::fs::File::open(path)?);
    let is_turtle = path.extension().and_then(|e| e.to_str()) == Some("ttl");
    let mut count = 0usize;
    if is_turtle {
        for triple in TurtleParser::new().for_reader(reader) {
            let t = triple?;
            store.insert(lex_triple(&t.subject, t.predicate.as_str(), &t.object));
            count += 1;
        }
    } else {
        for triple in NTriplesParser::new().for_reader(reader) {
            let t = triple?;
            store.insert(lex_triple(&t.subject, t.predicate.as_str(), &t.object));
            count += 1;
        }
    }
    Ok(count)
}

/// Convert an oxrdf triple into the lexical `(s, p, o)` strings the
/// `MemStore` query path compares against.
///
/// The convention must match `horndb_sparql::algebra::translate`: IRIs
/// are the bare IRI string, blank nodes the bare label, literals their
/// N-Triples Display form (`oxrdf::Literal::to_string`). Routing both the
/// loader and the query translator through `to_string()` keeps datatype
/// normalisation (e.g. dropping the redundant `xsd:string`) consistent on
/// both sides.
fn lex_triple(
    subject: &NamedOrBlankNode,
    predicate: &str,
    object: &OxTerm,
) -> (String, String, String) {
    (
        subject_lex(subject),
        predicate.to_owned(),
        object_lex(object),
    )
}

fn subject_lex(s: &NamedOrBlankNode) -> String {
    match s {
        NamedOrBlankNode::NamedNode(n) => n.as_str().to_owned(),
        NamedOrBlankNode::BlankNode(b) => b.as_str().to_owned(),
    }
}

fn object_lex(o: &OxTerm) -> String {
    match o {
        OxTerm::NamedNode(n) => n.as_str().to_owned(),
        OxTerm::BlankNode(b) => b.as_str().to_owned(),
        OxTerm::Literal(l) => l.to_string(),
        #[allow(unreachable_patterns)]
        other => other.to_string(),
    }
}
