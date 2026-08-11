//! SPARQL Update — `INSERT DATA` / `DELETE DATA`, pattern-based
//! `INSERT`/`DELETE … WHERE`, and the graph-management verbs
//! `LOAD`/`CLEAR`/`DROP`/`CREATE`/`ADD`/`MOVE`/`COPY` plus multi-operation
//! updates (SPEC-07 F5, SPEC-28 S4/S6, #267).
//!
//! Named graphs are first-class here (they were unrepresentable in Stage 1):
//!
//! - **Quad data.** `INSERT DATA` / `DELETE DATA` route each quad to the graph
//!   its `GRAPH <g> { … }` block names (or the default graph). Each operation
//!   is one [`Store::apply_quads`] batch — one operation = one commit (S4).
//! - **Pattern updates.** A named-graph template (`GRAPH <g> { … }` or a graph
//!   variable bound by the row) instantiates into that graph. `WITH` / `USING`
//!   / `USING NAMED` scope the WHERE clause's dataset (S3/D10); spargebra
//!   surfaces all three through the `using` field, which we lower with the same
//!   `FROM`/`FROM NAMED` machinery as a query.
//! - **Graph management (D11).** A graph exists iff it holds ≥1 visible quad.
//!   `CREATE <g>` succeeds if absent, errors if present unless `SILENT`;
//!   `CLEAR`/`DROP <g>` errors if absent unless `SILENT`, else retracts every
//!   visible quad **through [`Store::apply_quads`]** (never a structural
//!   unlink, so a delta consumer sees quad-grain retractions). `DROP ALL` /
//!   `CLEAR ALL` reset the default graph and every *non-reserved* named graph.
//! - **Reserved namespace closed to writes.** Any write targeting a graph IRI
//!   under [`RESERVED_GRAPH_PREFIX`] is an error, **not** suppressible by
//!   `SILENT` (it is a permission-shaped error, not an existence one). Reads of
//!   reserved graphs stay allowed.
//! - **`LOAD` routing.** Triples formats (`.nt`/`.ttl`) load into the
//!   destination (default if no `INTO GRAPH`); dataset formats (`.nq`/`.trig`)
//!   route each quad to its own graph on a plain `LOAD`, and `LOAD … INTO GRAPH`
//!   of a dataset format is an error (redirecting quads is undefined). `LOAD`
//!   stays `file:`-only (#189).
//! - **`ADD`/`MOVE`/`COPY`.** spargebra 0.4.6 desugars these into `Drop` +
//!   `DeleteInsert` pairs and **drops the `SILENT` flag**; we recover it (and
//!   the source operand) from the raw update text — see [`recover_amc_hints`].
//!   With that flag, a `SILENT` op with a missing source is a no-op and a
//!   non-silent one an error (S4). The same-graph identity case already
//!   collapses to zero operations in the parser.
//!
//! **Atomicity.** A multi-operation update preflights every operation
//! (`validate_op`, plus the `ADD`/`MOVE`/`COPY` source-existence check) against
//! the store *before* the first mutation, so a failing request mutates nothing.

use crate::algebra::translate::{dataset_spec_from, translate_where};
use crate::algebra::Term;
use crate::error::{Result, SparqlError};
use crate::exec::runtime::Runtime;
use crate::exec::{is_reserved_graph, Bindings, FullBackend, Store, RESERVED_GRAPH_PREFIX};
use crate::parser::ParsedUpdate;
use crate::plan::planner;
use crate::{DefaultGraphMode, SparqlConfig};
use spargebra::term::{
    GraphNamePattern, GroundQuadPattern, GroundTerm, GroundTermPattern, NamedNodePattern,
    NamedOrBlankNode, QuadPattern, Term as SpgTerm, TermPattern,
};
use std::collections::HashSet;

/// Lexical form for an RDF 1.2 triple term embedded in an update. The
/// Stage-1 store carries `Term::Literal(String)` slots only, so there is
/// no in-store representation for a triple term in this crate.
fn triple_term_unsupported() -> SparqlError {
    SparqlError::UnsupportedAlgebra("RDF 1.2 triple term in update (SPARQL 1.1 mode)".into())
}

/// The write-to-a-reserved-graph error (SPEC-28 S4). Not suppressible by
/// `SILENT`: it is a permission-shaped error, not an existence one.
fn reserved_write(iri: &str) -> SparqlError {
    SparqlError::UnsupportedAlgebra(format!(
        "write to a reserved graph is not allowed (the `{RESERVED_GRAPH_PREFIX}` namespace is \
         HornDB-internal): {iri}"
    ))
}

/// A `CLEAR`/`DROP` of a graph that does not exist (D11), non-`SILENT`.
fn graph_absent(iri: &str) -> SparqlError {
    SparqlError::Executor(format!(
        "graph does not exist (nothing to clear/drop): {iri}"
    ))
}

/// A `CREATE` of a graph that already exists (D11), non-`SILENT`.
fn graph_already_exists(iri: &str) -> SparqlError {
    SparqlError::Executor(format!("graph already exists: {iri}"))
}

/// An `ADD`/`MOVE`/`COPY` whose source graph does not exist, non-`SILENT`
/// (SPEC-28 S4).
fn amc_source_absent(iri: &str) -> SparqlError {
    SparqlError::Executor(format!(
        "source graph of ADD/MOVE/COPY does not exist: {iri}"
    ))
}

/// Reject a named graph IRI under the reserved namespace as a write target.
fn reject_reserved(iri: &str) -> Result<()> {
    if is_reserved_graph(iri) {
        Err(reserved_write(iri))
    } else {
        Ok(())
    }
}

/// Reject a reserved IRI carried by an `INSERT DATA`/`DELETE DATA` quad's
/// `GRAPH` name.
fn reject_reserved_graph_name(g: &spargebra::term::GraphName) -> Result<()> {
    match g {
        spargebra::term::GraphName::DefaultGraph => Ok(()),
        spargebra::term::GraphName::NamedNode(n) => reject_reserved(n.as_str()),
    }
}

/// Reject a reserved IRI carried by a ground template quad's `GRAPH` name. A
/// graph *variable* is left to runtime binding: `GRAPH ?g` enumeration already
/// excludes reserved graphs, so a bound `?g` can never name one.
fn reject_reserved_graph_pattern(g: &GraphNamePattern) -> Result<()> {
    match g {
        GraphNamePattern::NamedNode(n) => reject_reserved(n.as_str()),
        GraphNamePattern::DefaultGraph | GraphNamePattern::Variable(_) => Ok(()),
    }
}

/// Apply an update with the default [`SparqlConfig`] (SPARQL 1.1).
pub fn apply_update<B: FullBackend>(u: &ParsedUpdate, store: &mut B) -> Result<()> {
    apply_update_with(u, store, &SparqlConfig::default())
}

/// Apply an update, taking an explicit [`SparqlConfig`].
pub fn apply_update_with<B: FullBackend>(
    u: &ParsedUpdate,
    store: &mut B,
    cfg: &SparqlConfig,
) -> Result<()> {
    use spargebra::GraphUpdateOperation;
    let (ops, source) = match u {
        ParsedUpdate::InsertData { inner } | ParsedUpdate::DeleteData { inner } => {
            (&inner.operations, None)
        }
        ParsedUpdate::DeleteInsert { inner, source }
        | ParsedUpdate::GraphManagement { inner, source } => {
            (&inner.operations, Some(source.as_str()))
        }
        ParsedUpdate::UnsupportedForm { .. } => {
            return Err(SparqlError::UnsupportedAlgebra(
                "update form not supported in Stage 1".into(),
            ));
        }
    };

    // Recover the `SILENT` flag and source operand of every `ADD`/`MOVE`/`COPY`
    // from the raw text (spargebra drops the flag on desugaring — see
    // `recover_amc_hints`). Only `DeleteInsert`/`GraphManagement` carry the raw
    // source; the pure-data variants never contain these verbs.
    let amc_hints = source.map(recover_amc_hints).unwrap_or_default();
    // A silent `MOVE <missing> TO <g>` desugars to a trailing *non-silent*
    // `Drop <missing>` (spargebra's quirk); collect the missing sources of
    // silent verbs so that drop is not turned into a spurious existence error.
    let silent_absent_sources: HashSet<&str> = amc_hints
        .iter()
        .filter(|h| h.silent)
        .filter_map(|h| match &h.source {
            AmcSource::Named(g) => Some(g.as_str()),
            _ => None,
        })
        .collect();

    // Preflight (atomicity, SPARQL 1.1 §3.1.3): validate the whole request
    // against the store's current state before any mutation. A non-silent
    // `ADD`/`MOVE`/`COPY` whose named source is absent is an error (S4).
    for h in &amc_hints {
        if !h.silent {
            if let AmcSource::Named(g) = &h.source {
                if !store.graph_exists(g) {
                    return Err(amc_source_absent(g));
                }
            }
        }
    }
    for op in ops {
        validate_op(op, store, cfg, &silent_absent_sources)?;
    }

    // Apply. `validate_op` has rejected every statically- and existence-checkable
    // error, so the handlers mutate freely (a no-op sweep on an absent graph is
    // still a harmless no-op through `apply_quads`).
    for op in ops {
        match op {
            GraphUpdateOperation::InsertData { data } => {
                let mut adds = Vec::with_capacity(data.len());
                for q in data {
                    let g = graph_name_to_slot(&q.graph_name);
                    let s = subject_to_term(&q.subject);
                    let p = Term::Iri(q.predicate.as_str().to_owned());
                    let o = object_to_term(&q.object)?;
                    adds.push((g, s, p, o));
                }
                store.apply_quads(Vec::new(), adds)?;
            }
            GraphUpdateOperation::DeleteData { data } => {
                let mut dels = Vec::with_capacity(data.len());
                for q in data {
                    let g = graph_name_to_slot(&q.graph_name);
                    let s = Term::Iri(q.subject.as_str().to_owned());
                    let p = Term::Iri(q.predicate.as_str().to_owned());
                    let o = ground_term_to_term(&q.object)?;
                    dels.push((g, s, p, o));
                }
                store.apply_quads(dels, Vec::new())?;
            }
            GraphUpdateOperation::DeleteInsert {
                delete,
                insert,
                using,
                pattern,
            } => {
                apply_delete_insert(store, cfg, delete, insert, using.as_ref(), pattern)?;
            }
            GraphUpdateOperation::Clear { graph, .. } => apply_clear_drop(store, graph)?,
            GraphUpdateOperation::Drop { graph, .. } => apply_clear_drop(store, graph)?,
            // D11 CREATE: creating an absent graph is a no-op that succeeds (no
            // registry — the graph "exists" only once it holds a quad); a
            // create of an existing graph was rejected in `validate_op`.
            GraphUpdateOperation::Create { .. } => {}
            GraphUpdateOperation::Load {
                silent,
                source,
                destination,
            } => {
                apply_load(store, *silent, source, destination)?;
            }
        }
    }
    Ok(())
}

/// Preflight one operation against `store`: return the error it would produce
/// at apply time, without mutating (SPARQL Update atomicity, §3.1.3). Reserved
/// namespace and existence (D11) are both mirrored here so a failing multi-op
/// request mutates nothing. `silent_absent_sources` names the missing sources
/// of silent `ADD`/`MOVE`/`COPY` verbs, whose desugared (non-silent) source
/// drop must not raise an existence error.
fn validate_op<B: FullBackend>(
    op: &spargebra::GraphUpdateOperation,
    store: &B,
    cfg: &SparqlConfig,
    silent_absent_sources: &HashSet<&str>,
) -> Result<()> {
    use spargebra::algebra::GraphTarget;
    use spargebra::term::GraphName;
    use spargebra::GraphUpdateOperation;
    match op {
        GraphUpdateOperation::InsertData { data } => {
            for q in data {
                reject_reserved_graph_name(&q.graph_name)?;
                object_to_term(&q.object)?;
            }
            Ok(())
        }
        GraphUpdateOperation::DeleteData { data } => {
            for q in data {
                reject_reserved_graph_name(&q.graph_name)?;
                ground_term_to_term(&q.object)?;
            }
            Ok(())
        }
        GraphUpdateOperation::DeleteInsert {
            delete,
            insert,
            using,
            pattern,
        } => validate_delete_insert(delete, insert, using.as_ref(), pattern, cfg),
        GraphUpdateOperation::Clear { silent, graph }
        | GraphUpdateOperation::Drop { silent, graph } => {
            // Reserved check first — not suppressible by SILENT.
            if let GraphTarget::NamedNode(n) = graph {
                reject_reserved(n.as_str())?;
            }
            match graph {
                // DEFAULT always exists; NAMED/ALL sweep whatever is there.
                GraphTarget::DefaultGraph | GraphTarget::NamedGraphs | GraphTarget::AllGraphs => {
                    Ok(())
                }
                GraphTarget::NamedNode(n) => {
                    let iri = n.as_str();
                    if *silent || store.graph_exists(iri) || silent_absent_sources.contains(iri) {
                        Ok(())
                    } else {
                        Err(graph_absent(iri))
                    }
                }
            }
        }
        GraphUpdateOperation::Create { silent, graph } => {
            reject_reserved(graph.as_str())?;
            if !*silent && store.graph_exists(graph.as_str()) {
                Err(graph_already_exists(graph.as_str()))
            } else {
                Ok(())
            }
        }
        GraphUpdateOperation::Load {
            silent,
            source,
            destination,
        } => {
            // Reserved destination is rejected even for a SILENT load.
            if let GraphName::NamedNode(n) = destination {
                reject_reserved(n.as_str())?;
            }
            if *silent {
                // A silent LOAD swallows fetch/parse/format failures, so it can
                // never abort the request — nothing else to preflight.
                return Ok(());
            }
            // Fetch + parse now (pure read) to surface a non-silent failure —
            // and the dataset-into-a-named-graph rejection — before any prior
            // op mutates.
            let doc = fetch_and_parse(source.as_str())?;
            if doc.dataset_format && matches!(destination, GraphName::NamedNode(_)) {
                return Err(load_dataset_into_named());
            }
            Ok(())
        }
    }
}

/// Shared rejection scan for a pattern-based update, used by both the
/// atomicity preflight and (implicitly, via preflight) the apply path. Reserved
/// namespace, triple-terms, and WHERE-clause translatability are all checked
/// without mutating.
fn validate_delete_insert(
    delete: &[GroundQuadPattern],
    insert: &[QuadPattern],
    using: Option<&spargebra::algebra::QueryDataset>,
    pattern: &spargebra::algebra::GraphPattern,
    cfg: &SparqlConfig,
) -> Result<()> {
    // Reserved namespace: reject a ground named-graph template targeting it.
    for q in delete {
        reject_reserved_graph_pattern(&q.graph_name)?;
    }
    for q in insert {
        reject_reserved_graph_pattern(&q.graph_name)?;
    }

    // Reject RDF 1.2 triple-term slots in any DELETE/INSERT template (the
    // Stage-1 store has no triple-term slot), so the `resolve_*` `Triple(_)`
    // arms are unreachable for that reason and never silently drop a triple.
    for q in delete {
        if ground_quad_has_triple_term(q) {
            return Err(triple_term_unsupported());
        }
    }
    for q in insert {
        if quad_has_triple_term(q) {
            return Err(triple_term_unsupported());
        }
    }

    // Translate + plan the WHERE clause now (pure — no store access) so an
    // unsupported construct (`SERVICE`, `MINUS`, …) aborts the whole request
    // before any earlier operation mutates. The throwaway plan is recomputed in
    // `apply_delete_insert`; planning is cheap next to the atomicity it buys.
    let _ = using; // `using` is a dataset, never a rejectable construct now.
    let alg = translate_where(pattern, cfg)?;
    planner::plan(&alg)?;
    Ok(())
}

/// Evaluate the WHERE pattern, then instantiate the DELETE/INSERT templates
/// per solution, routing each instantiated quad to its own graph. Per SPARQL
/// 1.1 §3.1.3 the deletions are computed from the pre-update solutions and
/// applied before the insertions; the whole operation is **one**
/// [`Store::apply_quads`] batch (SPEC-28 S4).
///
/// **`WITH` / `USING` (SPEC-28 D10) — spargebra 0.4.6 discovery.** `WITH <g>`
/// desugars so that (a) every template quad acquires `graph_name = g`, and (b)
/// the WHERE clause is scoped via `using = QueryDataset { default: [g] }` — the
/// WHERE pattern is **not** wrapped in `GraphPattern::Graph`. So we do not wrap
/// the WHERE side ourselves: honouring `using` as the dataset already scopes it.
/// `USING` / `USING NAMED` populate the same `using` field, so all three go
/// through [`dataset_spec_from`], the exact `FROM`/`FROM NAMED` machinery.
fn apply_delete_insert<B: FullBackend>(
    store: &mut B,
    cfg: &SparqlConfig,
    delete: &[GroundQuadPattern],
    insert: &[QuadPattern],
    using: Option<&spargebra::algebra::QueryDataset>,
    pattern: &spargebra::algebra::GraphPattern,
) -> Result<()> {
    let alg = translate_where(pattern, cfg)?;
    let plan = planner::plan(&alg)?;
    // With no `USING`/`WITH`, the WHERE reads the default-graph sentinel only
    // (`Strict`) — an update reads exactly the graph a bare template writes.
    // When `using` names a dataset (including the `WITH <g>` desugaring), that
    // dataset drives the read and this mode is not consulted.
    let dataset = dataset_spec_from(using);
    let rows: Vec<Bindings> = Runtime::new(store)
        .with_dataset(dataset, DefaultGraphMode::Strict)
        .run(&plan)?
        .collect();

    // Deletions computed from the original bindings first.
    let mut dels: Vec<crate::exec::AlgebraQuad> = Vec::new();
    for row in &rows {
        for q in delete {
            let Some(graph) = resolve_graph_name(&q.graph_name, row) else {
                continue; // unbound / non-IRI graph variable: skip this quad
            };
            if let (Some(s), Some(p), Some(o)) = (
                resolve_ground(&q.subject, row).and_then(subject_or_skip),
                resolve_pred(&q.predicate, row),
                resolve_ground(&q.object, row),
            ) {
                dels.push((graph, s, p, o));
            }
        }
    }
    // Insertions allocate fresh blank nodes per solution row.
    let mut adds: Vec<crate::exec::AlgebraQuad> = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        for q in insert {
            let Some(graph) = resolve_graph_name(&q.graph_name, row) else {
                continue;
            };
            if let (Some(s), Some(p), Some(o)) = (
                resolve_term(&q.subject, row, i).and_then(subject_or_skip),
                resolve_pred(&q.predicate, row),
                resolve_term(&q.object, row, i),
            ) {
                adds.push((graph, s, p, o));
            }
        }
    }

    store.apply_quads(dels, adds)?;
    Ok(())
}

/// Apply `CLEAR`/`DROP` against `store`. `SILENT` and the absent-graph error
/// are already handled in [`validate_op`]; here the target is swept through
/// [`Store::clear_graph`] (never a structural unlink — SPEC-28 S4). `ALL`/
/// `NAMED` spare reserved (HornDB-internal) graphs.
fn apply_clear_drop<B: FullBackend>(
    store: &mut B,
    graph: &spargebra::algebra::GraphTarget,
) -> Result<()> {
    use spargebra::algebra::GraphTarget;
    match graph {
        GraphTarget::DefaultGraph => {
            store.clear_graph(&GraphTarget::DefaultGraph)?;
        }
        GraphTarget::NamedNode(_) => {
            store.clear_graph(graph)?;
        }
        // NAMED = every named graph; ALL = default + every named graph. Both
        // spare reserved graphs, which are HornDB-internal (SPEC-28 S4:
        // `DROP ALL` is a data reset, not a system reset).
        GraphTarget::NamedGraphs | GraphTarget::AllGraphs => {
            if matches!(graph, GraphTarget::AllGraphs) {
                store.clear_graph(&GraphTarget::DefaultGraph)?;
            }
            // `Store::named_graphs` (not the `Executor` overload) — every named
            // graph, reserved ones included, which we then filter out.
            for g in Store::named_graphs(store) {
                if !is_reserved_graph(&g) {
                    store.clear_graph(&named_target(&g))?;
                }
            }
        }
    }
    Ok(())
}

/// A `GraphTarget::NamedNode` for `iri`.
fn named_target(iri: &str) -> spargebra::algebra::GraphTarget {
    spargebra::algebra::GraphTarget::NamedNode(spargebra::term::NamedNode::new_unchecked(iri))
}

/// Apply `LOAD <source> [INTO GRAPH <destination>]`. Routing (SPEC-28 S4):
/// a triples format loads into `destination` (the default graph if none); a
/// dataset format (`.nq`/`.trig`) routes each quad to its own graph on a plain
/// `LOAD`, and `LOAD … INTO GRAPH` of a dataset format is an error. Reserved
/// destination and the dataset-into-named rejection are mirrored in
/// [`validate_op`]. A non-silent fetch/parse failure propagates; `SILENT`
/// swallows every failure (SPARQL 1.1 §3.1.5).
fn apply_load<B: FullBackend>(
    store: &mut B,
    silent: bool,
    source: &spargebra::term::NamedNode,
    destination: &spargebra::term::GraphName,
) -> Result<()> {
    use spargebra::term::GraphName;
    let doc = match fetch_and_parse(source.as_str()) {
        Ok(d) => d,
        Err(e) => return if silent { Ok(()) } else { Err(e) },
    };
    let dest_named: Option<String> = match destination {
        GraphName::DefaultGraph => None,
        GraphName::NamedNode(n) => Some(n.as_str().to_owned()),
    };
    if doc.dataset_format && dest_named.is_some() {
        return if silent {
            Ok(())
        } else {
            Err(load_dataset_into_named())
        };
    }
    let adds: Vec<crate::exec::AlgebraQuad> = doc
        .quads
        .into_iter()
        .map(|(file_graph, s, p, o)| {
            // A named destination overrides the file's graph (triples formats
            // only — dataset-into-named errored above). No destination keeps
            // the file's graph (the default-graph sentinel for triples files).
            let graph = match &dest_named {
                Some(d) => Some(Term::Iri(d.clone())),
                None => file_graph,
            };
            (graph, s, p, o)
        })
        .collect();
    store.apply_quads(Vec::new(), adds)?;
    Ok(())
}

/// A `LOAD … INTO GRAPH` of a dataset (`.nq`/`.trig`) document. W3C LOAD is a
/// graph operation, so redirecting a multi-graph document's quads into one
/// graph has no defined meaning.
fn load_dataset_into_named() -> SparqlError {
    SparqlError::UnsupportedAlgebra(
        "LOAD of a dataset document (.nq/.trig) INTO a named graph is not defined — a plain LOAD \
         routes each quad to its own graph"
            .into(),
    )
}

/// A parsed LOAD document: its quads (each with its graph slot — `None` is the
/// default-graph sentinel) and whether the serialization was a dataset format.
struct LoadedDoc {
    quads: Vec<crate::exec::AlgebraQuad>,
    /// `true` for `.nq`/`.trig` (a quad source that carries graph names).
    dataset_format: bool,
}

/// Fetch and parse an RDF document named by `source`. Stage-1 supports `file:`
/// IRIs only; remote (`http(s):`) sources are rejected (no HTTP client). The
/// serialization is chosen from the path extension, defaulting to Turtle.
/// Triples formats yield default-graph quads; dataset formats keep each quad's
/// own graph name.
fn fetch_and_parse(source: &str) -> Result<LoadedDoc> {
    use oxttl::{NQuadsParser, NTriplesParser, TriGParser, TurtleParser};

    let raw = file_iri_to_path(source)?;
    // A file IRI percent-encodes reserved characters (e.g. a space as `%20`);
    // decode to the real filesystem path before reading.
    let path = percent_decode(&raw);

    let bytes = std::fs::read(&path)
        .map_err(|e| SparqlError::Executor(format!("LOAD reading {path}: {e}")))?;
    let map_err =
        |e: oxttl::TurtleSyntaxError| SparqlError::Executor(format!("LOAD parsing {path}: {e}"));

    let mut quads = Vec::new();
    let mut dataset_format = false;
    match path
        .rsplit('.')
        .next()
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        // N-Triples/N-Quads require absolute IRIs (no base).
        Some("nt") => {
            for t in NTriplesParser::new().for_slice(&bytes) {
                let t = t.map_err(map_err)?;
                let (s, p, o) = oxrdf_triple_to_terms(&t.subject, &t.predicate, &t.object);
                quads.push((None, s, p, o));
            }
        }
        Some("nq") => {
            dataset_format = true;
            for q in NQuadsParser::new().for_slice(&bytes) {
                let q = q.map_err(map_err)?;
                let (s, p, o) = oxrdf_triple_to_terms(&q.subject, &q.predicate, &q.object);
                quads.push((oxrdf_graph_to_slot(&q.graph_name), s, p, o));
            }
        }
        // Turtle/TriG may carry relative IRIs resolved against the document IRI;
        // use `source` as the base so `<s> <p> <o> .` loaded from
        // `file:///tmp/data.ttl` resolves correctly (mirrors the storage loader).
        Some("trig") => {
            dataset_format = true;
            let parser = with_base(TriGParser::new(), source)?;
            for q in parser.for_slice(&bytes) {
                let q = q.map_err(map_err)?;
                let (s, p, o) = oxrdf_triple_to_terms(&q.subject, &q.predicate, &q.object);
                quads.push((oxrdf_graph_to_slot(&q.graph_name), s, p, o));
            }
        }
        // `.ttl` and anything else default to Turtle.
        _ => {
            let parser = with_base(TurtleParser::new(), source)?;
            for t in parser.for_slice(&bytes) {
                let t = t.map_err(map_err)?;
                let (s, p, o) = oxrdf_triple_to_terms(&t.subject, &t.predicate, &t.object);
                quads.push((None, s, p, o));
            }
        }
    }
    Ok(LoadedDoc {
        quads,
        dataset_format,
    })
}

/// Lower an `oxrdf` graph name to a quad graph slot: `None` for the default
/// graph, `Some(term)` for a named (IRI) or blank-node graph.
fn oxrdf_graph_to_slot(g: &oxrdf::GraphName) -> Option<Term> {
    match g {
        oxrdf::GraphName::DefaultGraph => None,
        oxrdf::GraphName::NamedNode(n) => Some(Term::Iri(n.as_str().to_owned())),
        oxrdf::GraphName::BlankNode(b) => Some(Term::BlankNode(b.as_str().to_owned())),
    }
}

/// Set the document IRI as the parser's base so relative IRIs in Turtle/TriG
/// resolve against `source`. `source` is the `LOAD <iri>` operand, already
/// validated as an IRI, so `with_base_iri` succeeds in practice; a rejected
/// base is surfaced as a clear LOAD error.
trait WithBase: Sized {
    fn with_base_iri_checked(self, base: &str) -> Result<Self>;
}
impl WithBase for oxttl::TurtleParser {
    fn with_base_iri_checked(self, base: &str) -> Result<Self> {
        self.with_base_iri(base)
            .map_err(|e| SparqlError::Executor(format!("LOAD base IRI invalid ({base}): {e}")))
    }
}
impl WithBase for oxttl::TriGParser {
    fn with_base_iri_checked(self, base: &str) -> Result<Self> {
        self.with_base_iri(base)
            .map_err(|e| SparqlError::Executor(format!("LOAD base IRI invalid ({base}): {e}")))
    }
}
fn with_base<P: WithBase>(parser: P, source: &str) -> Result<P> {
    parser.with_base_iri_checked(source)
}

/// Extract the local filesystem path from a `file:` IRI (still percent-encoded).
///
/// Handles the authority component: `file:///abs` (empty authority) and
/// `file://localhost/abs` are local and yield `/abs`; `file:/abs` (no `//`)
/// yields `/abs`. A non-empty, non-`localhost` authority (e.g. a remote host)
/// is rejected. A non-`file:` source is rejected (Stage-1 fetches `file:` only).
fn file_iri_to_path(source: &str) -> Result<String> {
    let non_file = || {
        SparqlError::UnsupportedAlgebra(format!(
            "LOAD of a non-file source (Stage-1 fetches file: IRIs only): {source}"
        ))
    };
    if let Some(rest) = source.strip_prefix("file://") {
        // `rest` is `<authority><path>`; the authority runs up to the first `/`.
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            // No path slash at all (`file://host`): treat the whole thing as the
            // authority with an empty path — not a usable local file.
            None => (rest, ""),
        };
        if authority.is_empty() || authority.eq_ignore_ascii_case("localhost") {
            Ok(path.to_owned())
        } else {
            Err(SparqlError::UnsupportedAlgebra(format!(
                "LOAD of a non-local file authority (Stage-1 fetches local files only): {source}"
            )))
        }
    } else if let Some(path) = source.strip_prefix("file:") {
        // `file:/abs` or `file:relative` — no authority component.
        Ok(path.to_owned())
    } else {
        Err(non_file())
    }
}

/// Percent-decode a file-IRI path component (RFC 3986). A `%XX` escape becomes
/// the decoded byte; a malformed escape is left verbatim. The decoded byte
/// sequence is interpreted as UTF-8 (lossy), which covers ordinary filesystem
/// paths; this is a minimal decoder sufficient for `file:` LOAD sources.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Lower a parsed `(subject, predicate, object)` from oxttl to algebra terms.
fn oxrdf_triple_to_terms(
    subject: &oxrdf::NamedOrBlankNode,
    predicate: &oxrdf::NamedNode,
    object: &oxrdf::Term,
) -> (Term, Term, Term) {
    (
        oxrdf_subject_to_term(subject),
        Term::Iri(predicate.as_str().to_owned()),
        oxrdf_term_to_term(object),
    )
}

/// Lower an `oxrdf` subject (named node or blank node) to an algebra [`Term`].
/// Blank-node labels are carried through verbatim, matching the N-Triples/Turtle
/// bulk loaders; per-document blank-node scoping is deferred to the dictionary
/// store (SPEC-02).
fn oxrdf_subject_to_term(s: &oxrdf::NamedOrBlankNode) -> Term {
    match s {
        oxrdf::NamedOrBlankNode::NamedNode(n) => Term::Iri(n.as_str().to_owned()),
        oxrdf::NamedOrBlankNode::BlankNode(b) => Term::BlankNode(b.as_str().to_owned()),
    }
}

/// Lower an `oxrdf` object term to an algebra [`Term`]. Literals keep their
/// N-Triples lexical form, matching the rest of the Stage-1 store.
fn oxrdf_term_to_term(t: &oxrdf::Term) -> Term {
    match t {
        oxrdf::Term::NamedNode(n) => Term::Iri(n.as_str().to_owned()),
        oxrdf::Term::BlankNode(b) => Term::BlankNode(b.as_str().to_owned()),
        oxrdf::Term::Literal(l) => Term::Literal(l.to_string()),
        // RDF 1.2 triple-term objects: the Stage-1 store has no triple-term
        // slot, so they are surfaced as their N-Triples lexical form (the same
        // best-effort lowering the loader applies).
        oxrdf::Term::Triple(tr) => Term::Literal(tr.to_string()),
    }
}

// ── ADD/MOVE/COPY SILENT recovery (deletable on an upstream spargebra fix) ───

/// The source operand of an `ADD`/`MOVE`/`COPY`, recovered from raw text.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AmcSource {
    /// `DEFAULT` — the default graph always exists, never a missing-source error.
    Default,
    /// `[GRAPH] <iri>` — the named source graph.
    Named(String),
    /// The operand could not be resolved from the text alone (e.g. a prefixed
    /// name, which needs the query prologue). We do not run a source-existence
    /// check on it — the desugared ops apply as-is (a natural no-op on a
    /// missing source), which is the honest, non-destructive outcome.
    Unknown,
}

/// One recovered `ADD`/`MOVE`/`COPY` occurrence: its `SILENT` flag and source.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AmcHint {
    silent: bool,
    source: AmcSource,
}

/// Recover the `SILENT` flag (and the source operand) of every
/// `ADD`/`MOVE`/`COPY` in the raw update text, in source order.
///
/// **Why this exists.** spargebra 0.4.6 desugars `ADD`/`MOVE`/`COPY` into
/// `Drop` + `DeleteInsert` pairs and **discards the `SILENT` flag**. That was
/// harmless while named graphs were unrepresentable; now a `SILENT COPY
/// <missing> TO <g>` must be a no-op and a non-silent one an error (SPEC-28
/// S4), so the flag matters. We re-scan the source text — a small hand-written
/// tokenizer (no regex) that skips comments, IRIs, and string literals — to
/// recover it, plus the source operand so the missing-source check needs no
/// fragile op-shape matching. Delete this whole machinery once spargebra
/// preserves the flag (or exposes structured `Add`/`Move`/`Copy` ops).
///
/// Upstream: `# TODO` — file an issue asking oxigraph/spargebra to preserve the
/// `SILENT` flag on `ADD`/`MOVE`/`COPY` (no issue filed yet; do not invent a
/// number).
fn recover_amc_hints(src: &str) -> Vec<AmcHint> {
    let toks = amc_tokenize(src);
    let mut hints = Vec::new();
    for (i, tok) in toks.iter().enumerate() {
        if *tok != AmcTok::Amc {
            continue;
        }
        // After the verb: an optional SILENT keyword, then the source operand
        // `DEFAULT | GRAPH? <iri>`. The tokens are consecutive (whitespace and
        // comments are not emitted), so index arithmetic tracks the grammar.
        let mut j = i + 1;
        let silent = matches!(toks.get(j), Some(AmcTok::Silent));
        if silent {
            j += 1;
        }
        let source = match toks.get(j) {
            Some(AmcTok::Default) => AmcSource::Default,
            Some(AmcTok::Iri(s)) => AmcSource::Named(s.clone()),
            Some(AmcTok::Graph) => match toks.get(j + 1) {
                Some(AmcTok::Iri(s)) => AmcSource::Named(s.clone()),
                _ => AmcSource::Unknown,
            },
            _ => AmcSource::Unknown,
        };
        hints.push(AmcHint { silent, source });
    }
    hints
}

/// The token kinds the SILENT-recovery scan needs. Everything not one of the
/// tracked keywords or an IRI is [`AmcTok::Other`] — kept (not dropped) so token
/// adjacency mirrors the grammar and a non-keyword source operand (a prefixed
/// name, a variable) reads as [`AmcSource::Unknown`], not the next IRI.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AmcTok {
    Amc,
    Silent,
    Default,
    Graph,
    Iri(String),
    Other,
}

/// True for bytes that continue a word (keyword or name segment). Includes `-`
/// so a hyphenated name segment is one token and cannot be mistaken for a
/// trailing keyword.
fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// Tokenize `src` for [`recover_amc_hints`]. Skips ASCII whitespace, `#`
/// comments (to end of line), IRIs (`<…>`), and string literals (`'…'`, `"…"`,
/// and their triple-quoted forms, honouring `\` escapes). Emits one token per
/// keyword/IRI/other-run.
fn amc_tokenize(src: &str) -> Vec<AmcTok> {
    let b = src.as_bytes();
    let n = b.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        let c = b[i];
        match c {
            b' ' | b'\t' | b'\r' | b'\n' => i += 1,
            b'#' => {
                while i < n && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'<' => {
                // IRIREF: no `>` or whitespace inside. If unterminated, stop at
                // end (best-effort — the parser already accepted the update).
                let start = i + 1;
                let mut j = start;
                while j < n && b[j] != b'>' && !b[j].is_ascii_whitespace() {
                    j += 1;
                }
                out.push(AmcTok::Iri(
                    String::from_utf8_lossy(&b[start..j]).into_owned(),
                ));
                i = if j < n && b[j] == b'>' { j + 1 } else { j };
            }
            b'"' | b'\'' => {
                i = amc_skip_string(b, i);
                out.push(AmcTok::Other);
            }
            _ if is_word_byte(c) => {
                let start = i;
                let mut j = i;
                while j < n && is_word_byte(b[j]) {
                    j += 1;
                }
                // A word is a keyword only when it is not part of a prefixed
                // name (`ex:ADD`, `ADD:`) and not a variable (`?ADD`/`$ADD`).
                let prev = if start > 0 { b[start - 1] } else { 0 };
                let next = if j < n { b[j] } else { 0 };
                let name_ctx = prev == b':' || prev == b'?' || prev == b'$' || next == b':';
                let word = &src[start..j];
                out.push(if name_ctx {
                    AmcTok::Other
                } else if word.eq_ignore_ascii_case("ADD")
                    || word.eq_ignore_ascii_case("MOVE")
                    || word.eq_ignore_ascii_case("COPY")
                {
                    AmcTok::Amc
                } else if word.eq_ignore_ascii_case("SILENT") {
                    AmcTok::Silent
                } else if word.eq_ignore_ascii_case("DEFAULT") {
                    AmcTok::Default
                } else if word.eq_ignore_ascii_case("GRAPH") {
                    AmcTok::Graph
                } else {
                    AmcTok::Other
                });
                i = j;
            }
            _ => {
                out.push(AmcTok::Other);
                i += 1;
            }
        }
    }
    out
}

/// Skip a SPARQL string literal starting at `b[start]` (`'` or `"`), including
/// the triple-quoted forms, honouring `\` escapes. Returns the index just past
/// the closing quote (or end of input if unterminated).
fn amc_skip_string(b: &[u8], start: usize) -> usize {
    let q = b[start];
    let n = b.len();
    // Triple-quoted (`"""` / `'''`)?
    if start + 2 < n && b[start + 1] == q && b[start + 2] == q {
        let mut i = start + 3;
        while i < n {
            if b[i] == b'\\' {
                i += 2;
                continue;
            }
            if i + 2 < n && b[i] == q && b[i + 1] == q && b[i + 2] == q {
                return i + 3;
            }
            i += 1;
        }
        return n;
    }
    let mut i = start + 1;
    while i < n {
        if b[i] == b'\\' {
            i += 2;
            continue;
        }
        if b[i] == q {
            return i + 1;
        }
        i += 1;
    }
    n
}

// ── Template / quad slot resolution ──────────────────────────────────────────

/// Resolve a template quad's `GRAPH` name to a quad graph slot. Returns
/// `Some(None)` for the default graph, `Some(Some(iri))` for a named graph, and
/// `None` to **skip the quad** (an unbound graph variable, or one bound to a
/// non-IRI term — neither names a writable graph).
fn resolve_graph_name(g: &GraphNamePattern, row: &Bindings) -> Option<Option<Term>> {
    match g {
        GraphNamePattern::DefaultGraph => Some(None),
        GraphNamePattern::NamedNode(n) => Some(Some(Term::Iri(n.as_str().to_owned()))),
        GraphNamePattern::Variable(v) => match row.get(v.as_str()) {
            Some(Term::Iri(iri)) => Some(Some(Term::Iri(iri.clone()))),
            _ => None,
        },
    }
}

/// The quad graph slot for a data quad's `GRAPH` name: `None` for the default
/// graph, `Some(iri)` for a named one.
fn graph_name_to_slot(g: &spargebra::term::GraphName) -> Option<Term> {
    match g {
        spargebra::term::GraphName::DefaultGraph => None,
        spargebra::term::GraphName::NamedNode(n) => Some(Term::Iri(n.as_str().to_owned())),
    }
}

/// True if any subject/object slot of an INSERT-template quad is an RDF 1.2
/// triple term.
fn quad_has_triple_term(q: &QuadPattern) -> bool {
    matches!(q.subject, TermPattern::Triple(_)) || matches!(q.object, TermPattern::Triple(_))
}

/// True if any subject/object slot of a DELETE-template quad is an RDF 1.2
/// triple term.
fn ground_quad_has_triple_term(q: &GroundQuadPattern) -> bool {
    matches!(q.subject, GroundTermPattern::Triple(_))
        || matches!(q.object, GroundTermPattern::Triple(_))
}

/// Resolve an INSERT-template `TermPattern` against a solution row. `row_ix`
/// scopes per-solution blank nodes so each row's template blank node is
/// distinct (SPARQL 1.1 §4.1.4). Returns `None` when a variable slot is unbound.
///
/// Lockstep invariant: mirrors `runtime.rs::construct_triples`'s `resolve_term`.
fn resolve_term(t: &TermPattern, row: &Bindings, row_ix: usize) -> Option<Term> {
    match t {
        TermPattern::NamedNode(n) => Some(Term::Iri(n.as_str().to_owned())),
        TermPattern::Literal(l) => Some(Term::Literal(l.to_string())),
        TermPattern::BlankNode(b) => Some(Term::BlankNode(format!("{}_r{row_ix}", b.as_str()))),
        TermPattern::Variable(v) => row.get(v.as_str()).cloned(),
        // Triple-term template slots are rejected up front (triple_term_unsupported).
        TermPattern::Triple(_) => None,
    }
}

/// Resolve a DELETE-template `GroundTermPattern` (no blank nodes allowed) against
/// a solution row.
fn resolve_ground(t: &GroundTermPattern, row: &Bindings) -> Option<Term> {
    match t {
        GroundTermPattern::NamedNode(n) => Some(Term::Iri(n.as_str().to_owned())),
        GroundTermPattern::Literal(l) => Some(Term::Literal(l.to_string())),
        GroundTermPattern::Variable(v) => row.get(v.as_str()).cloned(),
        GroundTermPattern::Triple(_) => None,
    }
}

/// Resolve a predicate template slot. A predicate variable binding is valid only
/// if it resolves to an IRI (a literal/blank node in predicate position drops
/// the triple). Lockstep with `runtime.rs::construct_triples`'s `resolve_pred`.
fn resolve_pred(p: &NamedNodePattern, row: &Bindings) -> Option<Term> {
    match p {
        NamedNodePattern::NamedNode(n) => Some(Term::Iri(n.as_str().to_owned())),
        NamedNodePattern::Variable(v) => match row.get(v.as_str()) {
            Some(Term::Iri(s)) => Some(Term::Iri(s.clone())),
            _ => None,
        },
    }
}

/// Position-aware subject guard: an instantiated template triple is legal only
/// if its subject is an IRI or blank node; a literal (or RDF 1.2 triple term) in
/// subject position makes it an illegal RDF triple, which is **silently
/// skipped** (SPARQL 1.1 §4.1.4 / §10.2.1, the same rule CONSTRUCT applies).
fn subject_or_skip(s: Term) -> Option<Term> {
    match s {
        Term::Iri(_) | Term::BlankNode(_) => Some(s),
        Term::Literal(_) | Term::Var(_) | Term::Triple(_) => None,
    }
}

fn subject_to_term(s: &NamedOrBlankNode) -> Term {
    match s {
        NamedOrBlankNode::NamedNode(n) => Term::Iri(n.as_str().to_owned()),
        NamedOrBlankNode::BlankNode(b) => Term::BlankNode(b.as_str().to_owned()),
    }
}

fn object_to_term(t: &SpgTerm) -> Result<Term> {
    Ok(match t {
        SpgTerm::NamedNode(n) => Term::Iri(n.as_str().to_owned()),
        SpgTerm::BlankNode(b) => Term::BlankNode(b.as_str().to_owned()),
        SpgTerm::Literal(l) => Term::Literal(l.to_string()),
        SpgTerm::Triple(_) => return Err(triple_term_unsupported()),
    })
}

fn ground_term_to_term(gt: &GroundTerm) -> Result<Term> {
    Ok(match gt {
        GroundTerm::NamedNode(n) => Term::Iri(n.as_str().to_owned()),
        GroundTerm::Literal(l) => Term::Literal(l.to_string()),
        GroundTerm::Triple(_) => return Err(triple_term_unsupported()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amc_recover_basic_verbs() {
        let h = recover_amc_hints("ADD <http://g/s> TO <http://g/d>");
        assert_eq!(
            h,
            vec![AmcHint {
                silent: false,
                source: AmcSource::Named("http://g/s".into())
            }]
        );
    }

    #[test]
    fn amc_recover_silent_and_default() {
        assert_eq!(
            recover_amc_hints("MOVE SILENT DEFAULT TO <http://g/d>"),
            vec![AmcHint {
                silent: true,
                source: AmcSource::Default
            }]
        );
        assert_eq!(
            recover_amc_hints("COPY SILENT GRAPH <http://g/s> TO DEFAULT"),
            vec![AmcHint {
                silent: true,
                source: AmcSource::Named("http://g/s".into())
            }]
        );
    }

    #[test]
    fn amc_recover_multiple_in_order() {
        let h = recover_amc_hints(
            "ADD SILENT <http://g/a> TO <http://g/b> ; COPY <http://g/c> TO <http://g/d>",
        );
        assert_eq!(h.len(), 2);
        assert!(h[0].silent);
        assert!(!h[1].silent);
        assert_eq!(h[0].source, AmcSource::Named("http://g/a".into()));
        assert_eq!(h[1].source, AmcSource::Named("http://g/c".into()));
    }

    #[test]
    fn amc_tokenizer_ignores_verbs_in_strings_iris_comments() {
        // `ADD`/`COPY`/`MOVE` appearing inside a string literal, an IRI, a
        // comment, or a prefixed name must not be recovered as verbs.
        let src = "# ADD a comment\n\
                   INSERT DATA { <http://ex/ADD> <http://ex/p> \"COPY MOVE\" . \
                   ex:MOVE ex:p ex:o }";
        assert_eq!(recover_amc_hints(src), vec![]);
    }
}
