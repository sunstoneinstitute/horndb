//! `horndb-bench` — HornDB-side micro-runner for the RDFox comparison.
//!
//! This binary is internal benchmarking glue. It exposes the three HornDB
//! operations we have published goals against RDFox for, each as a
//! subcommand that loads a file (or files), runs the operation, and prints
//! a single line of JSON to stdout. The orchestration — generating
//! workloads, running the equivalent RDFox commands, and computing the
//! comparison — lives in `scripts/bench/compare-rdfox.sh`.
//!
//! | Subcommand    | HornDB path                                   | Goal (BENCHMARKS.md)                         |
//! |---------------|-----------------------------------------------|----------------------------------------------|
//! | `import`      | `horndb_storage` N-Triples bulk loader        | SPEC-02 F8: ≥1 M triples/sec bulk import     |
//! | `transitive`  | `horndb_closure` GraphBLAS transitive closure | SPEC-05 acc#1: ≥10× RDFox on a chain         |
//! | `materialize` | `horndb_owlrl` OWL 2 RL forward materialization| Stage-1 gate: within 3× RDFox on LUBM        |
//!
//! All timings are wall-clock from `std::time::Instant` around the named
//! phase only (parsing is reported separately from reasoning where the API
//! allows). Numbers are emitted in milliseconds.

use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use oxrdf::{Dataset, GraphName, Quad};
use oxttl::{NTriplesParser, TurtleParser};

#[derive(Parser, Debug)]
#[command(
    name = "horndb-bench",
    about = "HornDB-side micro-runner for the RDFox comparison harness. Emits one JSON object per run."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Bulk N-Triples import throughput (no reasoning). SPEC-02 F8.
    Import {
        /// N-Triples file to load.
        #[arg(long)]
        data: PathBuf,
    },
    /// Transitive closure of a single predicate via GraphBLAS. SPEC-05 acc#1.
    Transitive {
        /// N-Triples file holding the edge relation.
        #[arg(long)]
        data: PathBuf,
        /// Predicate IRI whose (subject, object) pairs form the graph.
        #[arg(long)]
        predicate: String,
    },
    /// OWL 2 RL forward materialization to fixpoint. Stage-1 LUBM gate.
    Materialize {
        /// One or more N-Triples files; concatenated into one default graph.
        #[arg(long = "data", required = true, num_args = 1..)]
        data: Vec<PathBuf>,
        /// Optional: write the full materialized closure to this path as
        /// N-Triples (asserted base + everything inferred). Lets a
        /// lightweight serve binary load an already-reasoned graph without
        /// linking the OWL/GraphBLAS stack.
        #[arg(long = "dump-nt")]
        dump_nt: Option<PathBuf>,
        /// Closure backend: `rulefiring` (nested-loop reference) or
        /// `graphblas` (SuiteSparse:GraphBLAS, SPEC-05). The emitted
        /// `*_ms` phase timings let an A/B run attribute the materialize cost.
        #[arg(long = "backend", value_enum, default_value_t = BackendArg::Rulefiring)]
        backend: BackendArg,
    },
}

/// Closure backend selector for the `materialize` subcommand.
#[derive(Copy, Clone, Debug, clap::ValueEnum)]
enum BackendArg {
    /// In-crate nested-loop rule firing (`RuleFiringBackend`).
    Rulefiring,
    /// GraphBLAS sparse-matrix closure (`GraphBlasBackend`).
    Graphblas,
}

impl BackendArg {
    fn choice(self) -> horndb_owlrl::BackendChoice {
        match self {
            BackendArg::Rulefiring => horndb_owlrl::BackendChoice::RuleFiring,
            BackendArg::Graphblas => horndb_owlrl::BackendChoice::GraphBlas,
        }
    }
    fn label(self) -> &'static str {
        match self {
            BackendArg::Rulefiring => "rulefiring",
            BackendArg::Graphblas => "graphblas",
        }
    }
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Import { data } => run_import(&data),
        Cmd::Transitive { data, predicate } => run_transitive(&data, &predicate),
        Cmd::Materialize {
            data,
            dump_nt,
            backend,
        } => run_materialize(&data, dump_nt.as_deref(), backend),
    }
}

/// Emit one flat JSON object. Values are pre-formatted JSON fragments
/// (numbers bare, strings already quoted) so we avoid a serde dependency.
fn emit(fields: &[(&str, String)]) {
    let body = fields
        .iter()
        .map(|(k, v)| format!("\"{k}\":{v}"))
        .collect::<Vec<_>>()
        .join(",");
    println!("{{{body}}}");
}

fn ms(t: std::time::Duration) -> String {
    format!("{:.3}", t.as_secs_f64() * 1e3)
}

/// Throughput = `count / secs`, guarding against a zero-duration phase
/// (which would otherwise yield NaN/inf and break the JSON consumer).
fn per_sec(count: f64, secs: f64) -> f64 {
    if secs > 0.0 {
        count / secs
    } else {
        0.0
    }
}

fn run_import(data: &Path) -> Result<()> {
    let store = horndb_storage::Store::in_memory();
    let stats = horndb_storage::loader::ntriples::load_ntriples_file(&store, data)
        .with_context(|| format!("loading {}", data.display()))?;
    let secs = stats.elapsed_ms as f64 / 1e3;
    let tps = per_sec(stats.triples as f64, secs);
    emit(&[
        ("kind", "\"import\"".into()),
        ("input_triples", stats.triples.to_string()),
        ("load_ms", stats.elapsed_ms.to_string()),
        ("tps", format!("{tps:.1}")),
    ]);
    Ok(())
}

fn run_transitive(data: &Path, predicate: &str) -> Result<()> {
    // Parse the N-Triples file, keep only the target predicate, and map
    // each distinct node IRI to a dense 0..n index for the bool matrix.
    let reader =
        BufReader::new(File::open(data).with_context(|| format!("opening {}", data.display()))?);
    let mut ids: HashMap<String, u64> = HashMap::new();
    let mut edges: Vec<(u64, u64)> = Vec::new();
    let parse_start = Instant::now();
    for triple in NTriplesParser::new().for_reader(reader) {
        let t = triple.with_context(|| format!("parsing {}", data.display()))?;
        if t.predicate.as_str() != predicate {
            continue;
        }
        let mut intern = |key: String| {
            let next = ids.len() as u64;
            *ids.entry(key).or_insert(next)
        };
        let s = intern(t.subject.to_string());
        let o = intern(t.object.to_string());
        edges.push((s, o));
    }
    let parse = parse_start.elapsed();
    let n = ids.len() as u64;

    horndb_closure::grb::init_once().context("GraphBLAS init")?;
    let build_start = Instant::now();
    let m = horndb_closure::grb::BoolMatrix::from_edges(n, &edges).context("building matrix")?;
    let build = build_start.elapsed();

    let reason_start = Instant::now();
    let star = horndb_closure::closure::transitive::transitive_closure(&m).context("closure")?;
    let reason = reason_start.elapsed();
    let closure_edges = star.nvals().context("nvals")?;

    let secs = reason.as_secs_f64();
    let tps = per_sec(closure_edges as f64, secs);
    emit(&[
        ("kind", "\"transitive\"".into()),
        ("nodes", n.to_string()),
        ("input_edges", edges.len().to_string()),
        ("closure_edges", closure_edges.to_string()),
        ("parse_ms", ms(parse)),
        ("build_ms", ms(build)),
        ("reason_ms", ms(reason)),
        ("closure_tps", format!("{tps:.1}")),
    ]);
    Ok(())
}

fn run_materialize(files: &[PathBuf], dump_nt: Option<&Path>, backend: BackendArg) -> Result<()> {
    let mut dataset = Dataset::new();
    let parse_start = Instant::now();
    let mut input: u64 = 0;
    for f in files {
        let reader =
            BufReader::new(File::open(f).with_context(|| format!("opening {}", f.display()))?);
        let mut insert = |t: oxrdf::Triple| {
            dataset.insert(
                Quad::new(t.subject, t.predicate, t.object, GraphName::DefaultGraph).as_ref(),
            );
            input += 1;
        };
        // Format by extension: `.ttl` → Turtle, everything else → N-Triples.
        // SPB ontologies and reference datasets ship as Turtle; the
        // generated Creative Works are N-Triples.
        let is_turtle = f.extension().and_then(|e| e.to_str()) == Some("ttl");
        if is_turtle {
            for triple in TurtleParser::new().for_reader(reader) {
                insert(triple.with_context(|| format!("parsing {}", f.display()))?);
            }
        } else {
            for triple in NTriplesParser::new().for_reader(reader) {
                insert(triple.with_context(|| format!("parsing {}", f.display()))?);
            }
        }
    }
    let parse = parse_start.elapsed();

    let mut engine = horndb_owlrl::Engine::with_backend(backend.choice());
    let reason_start = Instant::now();
    engine.load(&dataset).context("materializing")?;
    let reason = reason_start.elapsed();
    // Per-phase wall-clock attribution (summed across semi-naïve rounds).
    let stats = engine.last_stats();
    let timings = stats.map(|s| s.timings.clone()).unwrap_or_default();
    let rounds = stats.map(|s| s.rounds).unwrap_or(0);

    let asserted = engine.asserted_len().unwrap_or(0);
    let total = engine.materialized_len().unwrap_or(0);
    let inferred = total.saturating_sub(asserted);

    // Optional: dump the materialized closure to N-Triples so a
    // lightweight serve binary can load it without the OWL/GraphBLAS stack.
    if let Some(path) = dump_nt {
        let triples = engine
            .materialized_triples()
            .context("materialized_triples returned None after a successful load")?;
        write_ntriples(path, &triples)
            .with_context(|| format!("writing materialized N-Triples to {}", path.display()))?;
    }
    let secs = reason.as_secs_f64();
    // Throughput on *input* facts (comparable to RDFox "facts/sec" on the
    // asserted base) and on *output* facts (total closure size / time).
    let tps_in = per_sec(asserted as f64, secs);
    let tps_out = per_sec(total as f64, secs);
    emit(&[
        ("kind", "\"materialize\"".into()),
        ("backend", format!("\"{}\"", backend.label())),
        ("parsed_triples", input.to_string()),
        ("asserted", asserted.to_string()),
        ("inferred", inferred.to_string()),
        ("total", total.to_string()),
        ("rounds", rounds.to_string()),
        ("parse_ms", ms(parse)),
        ("reason_ms", ms(reason)),
        // Phase attribution within reason_ms (#61). Sums across all rounds.
        ("compiled_rules_ms", ms(timings.compiled_rules)),
        ("list_rules_ms", ms(timings.list_rules)),
        ("closure_backend_ms", ms(timings.closure_backend)),
        ("apply_ms", ms(timings.apply)),
        ("input_tps", format!("{tps_in:.1}")),
        ("output_tps", format!("{tps_out:.1}")),
    ]);
    Ok(())
}

/// Serialize lexical `(s, p, o)` triples (as produced by
/// `Engine::materialized_triples`) to an N-Triples file.
///
/// The term lexical convention from the engine is: IRIs are bare,
/// blank nodes carry the `_:` prefix, and literals already arrive in
/// N-Triples object form (`"v"@lang` or `"v"^^<dt>`). We only need to
/// angle-bracket IRIs; blank nodes and literals pass through verbatim.
fn write_ntriples(path: &Path, triples: &[(String, String, String)]) -> Result<()> {
    use std::io::Write;
    let file = File::create(path)?;
    let mut w = std::io::BufWriter::new(file);
    let mut line = String::new();
    let mut skipped_literal_subject = 0usize;
    for (s, p, o) in triples {
        // OWL 2 RL datatype rules (dt-type*) can derive `rdf:type` triples
        // whose subject is a literal (e.g. a dateTime literal typed as
        // xsd:date). N-Triples forbids literal subjects, and such triples
        // are not addressable as subjects in standard SPARQL, so drop them
        // from the flat dump rather than emit illegal syntax.
        if s.starts_with('"') {
            skipped_literal_subject += 1;
            continue;
        }
        line.clear();
        push_term(&mut line, s);
        line.push(' ');
        // Predicates are always IRIs.
        push_term(&mut line, p);
        line.push(' ');
        push_term(&mut line, o);
        line.push_str(" .\n");
        w.write_all(line.as_bytes())?;
    }
    w.flush()?;
    if skipped_literal_subject > 0 {
        eprintln!(
            "write_ntriples: skipped {skipped_literal_subject} triple(s) with a literal subject \
             (not representable in N-Triples)"
        );
    }
    Ok(())
}

/// Append one N-Triples term.
///
/// Blank nodes (`_:`) pass through. Literals (engine key form
/// `"value"^^<dt>` or `"value"@lang`) are re-emitted with the value's
/// inner specials (`"`, `\`, newlines, …) escaped — the engine stores the
/// raw lexical value verbatim, which can contain unescaped quotes that
/// would otherwise produce invalid N-Triples. Everything else is treated
/// as an IRI and angle-bracketed.
fn push_term(out: &mut String, term: &str) {
    if let Some(rest) = term.strip_prefix('"') {
        // Split the value from the trailing `"^^<dt>` or `"@lang` suffix
        // at the *last* unescaped — here, last literal — closing quote.
        // The engine never escapes, so find the final `"` that precedes
        // `^^` or `@` (or end of string).
        if let Some(close) = rest.rfind('"') {
            let value = &rest[..close];
            let suffix = &rest[close + 1..]; // `^^<dt>` or `@lang` or empty
            out.push('"');
            escape_nt_literal(out, value);
            out.push('"');
            out.push_str(suffix);
        } else {
            // Malformed — emit verbatim and let the consumer complain.
            out.push_str(term);
        }
    } else if term.starts_with("_:") {
        out.push_str(term);
    } else {
        out.push('<');
        out.push_str(term);
        out.push('>');
    }
}

/// Escape a literal value for an N-Triples quoted string (RDF 1.1 §3.3).
fn escape_nt_literal(out: &mut String, value: &str) {
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
}
