//! SPARQL Update — `INSERT DATA` / `DELETE DATA`, pattern-based
//! `INSERT`/`DELETE … WHERE` (with `WITH` / `USING` / `USING NAMED`), and the
//! graph-management verbs `LOAD`/`CLEAR`/`DROP`/`CREATE`/`ADD`/`MOVE`/`COPY`,
//! plus multi-operation updates (SPEC-07 F5, SPEC-28 phase 4 / S4, #267).
//!
//! Named graphs are first-class here (they were not before phase 4): a write
//! routes to any graph, not just the merged default graph. The write seam is
//! [`crate::exec::Store::apply_quads`] — one atomic, idempotent, counted batch
//! of `(graph, s, p, o)` quads per Update operation.
//!
//! What each construct does:
//!
//! * **Quad data** — `INSERT DATA` / `DELETE DATA` group their quads (each
//!   carrying `GRAPH <g>` or the default graph) into one `apply_quads` call
//!   per operation. One operation = one batch = one commit; the multi-op
//!   collapse optimisation is deliberately *not* taken (correct-first).
//! * **Pattern updates** — each DELETE/INSERT template quad routes by its own
//!   `GraphNamePattern` (default / named / a variable the WHERE row binds).
//!   The whole operation is one `apply_quads` call (dels before adds).
//! * **`WITH` / `USING` / `USING NAMED`** (D10) — the WHERE clause runs
//!   through the phase-3 query path, so it understands `GRAPH`. `USING` /
//!   `USING NAMED` build the WHERE dataset via the phase-3 `DatasetSpec`
//!   machinery. See `apply_delete_insert` for the spargebra-`WITH` finding.
//! * **Graph management** (D11: a graph exists iff it holds ≥1 visible quad —
//!   no registry). `CREATE <g>`: absent → succeeds (no-op), existing → error
//!   unless `SILENT`. `CLEAR`/`DROP <g>`: absent → error unless `SILENT`,
//!   present → retract every visible quad through the store (never a
//!   structural unlink). `CLEAR`/`DROP DEFAULT` sweeps the default graph;
//!   `DROP ALL` sweeps the default graph and every non-reserved named graph
//!   quad by quad, leaving reserved graphs untouched (SPEC-30 owns the
//!   store-level reset).
//! * **`LOAD <src> [INTO GRAPH <g>]`** — triples formats route to the
//!   destination (default graph if no `INTO`); a plain `LOAD` of a dataset
//!   format (`.nq`/`.trig`) routes each quad to its own named graph; `LOAD …
//!   INTO GRAPH` of a dataset format is an error (redirecting a quad source is
//!   undefined). `file:`-only (#189).
//! * **`ADD`/`MOVE`/`COPY`** — spargebra desugars these into `Drop` +
//!   `DeleteInsert` sequences and **drops the `SILENT` flag**. The desugared
//!   ops execute; the flag is recovered by a source-text pre-scan (see
//!   [`scan_amc_silent_hints`]).
//! * **The reserved namespace is closed to writes** (S4): every write form
//!   touching `https://horndb.io/graph/…` is refused. This is a
//!   permission-shaped error that `SILENT` does **not** suppress; it is
//!   checked before any silent/existence logic. Reads of reserved graphs stay
//!   allowed.
//!
//! **Atomicity** (§3.1.3): a multi-operation update preflights every operation
//! (`validate_op`) for the errors it would hit at apply time — reserved
//! namespace, recovered-`SILENT` source existence, D11 existence, `LOAD`
//! routing/fetch — before the first mutation, so a failing request mutates
//! nothing (e.g. `COPY <absent> TO DEFAULT`, which desugars to a destructive
//! `Drop{DEFAULT}` + a copy from a missing source, never clears the default
//! graph). One store batch per operation, applied in request order.

use crate::algebra::translate::{dataset_spec_from, translate_where};
use crate::algebra::{DatasetSpec, Term};
use crate::error::{Result, SparqlError};
use crate::exec::runtime::Runtime;
use crate::exec::RESERVED_GRAPH_PREFIX;
use crate::exec::{is_reserved_graph, AlgebraQuad, Bindings, FullBackend, GraphName};
use crate::parser::ParsedUpdate;
use crate::plan::planner;
use crate::{DefaultGraphMode, SparqlConfig};
use spargebra::algebra::{GraphPattern, GraphTarget, QueryDataset};
use spargebra::term::{
    GraphName as SpgGraphName, GraphNamePattern, GroundQuadPattern, GroundTerm, GroundTermPattern,
    NamedNodePattern, NamedOrBlankNode, QuadPattern, Term as SpgTerm, TermPattern,
};
use spargebra::GraphUpdateOperation;

/// Lexical form for an RDF 1.2 triple term embedded in an update. The
/// Stage-1 store carries `Term::Literal(String)` slots only, so there is
/// no in-store representation for a triple term in this crate.
fn triple_term_unsupported() -> SparqlError {
    SparqlError::UnsupportedAlgebra("RDF 1.2 triple term in update (SPARQL 1.1 mode)".into())
}

/// A write touching the reserved (HornDB-internal) namespace. Not suppressible
/// by `SILENT` — a permission-shaped error checked before any silent/existence
/// logic (SPEC-28 S4).
fn reserved_graph_write_error(iri: &str) -> SparqlError {
    SparqlError::Executor(format!(
        "write to reserved graph <{iri}> is not permitted: the {RESERVED_GRAPH_PREFIX} \
         namespace is closed to writes (SPEC-28 S4). Reads are allowed."
    ))
}

/// `CREATE GRAPH <g>` where `g` already exists (D11) and `SILENT` was not set.
fn create_graph_exists_error(iri: &str) -> SparqlError {
    SparqlError::Executor(format!(
        "CREATE GRAPH <{iri}>: graph already exists (use SILENT to make it a no-op)"
    ))
}

/// `CLEAR`/`DROP GRAPH <g>` (or an `ADD`/`MOVE`/`COPY` source-drop) where `g`
/// does not exist (D11) and `SILENT` was not set.
fn missing_graph_error(verb: &str, iri: &str) -> SparqlError {
    SparqlError::Executor(format!(
        "{verb} GRAPH <{iri}>: graph does not exist (use SILENT to make it a no-op)"
    ))
}

/// `ADD`/`COPY` whose named source graph does not exist (D11) and whose
/// recovered `SILENT` flag was not set (SPARQL 1.1 §3.2.3/§3.2.5).
fn amc_source_missing_error(iri: &str) -> SparqlError {
    SparqlError::Executor(format!(
        "source graph <{iri}> does not exist (use SILENT to make it a no-op)"
    ))
}

/// `LOAD … INTO GRAPH` of a dataset format (`.nq`/`.trig`).
fn load_dataset_into_graph_error(source: &str, dest: &str) -> SparqlError {
    SparqlError::UnsupportedAlgebra(format!(
        "LOAD of a dataset format <{source}> INTO GRAPH <{dest}> is not defined: a quad source \
         already carries its own graph names, so redirecting them into one graph has no meaning \
         (W3C LOAD is a graph operation)."
    ))
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
    let (ops, source) = match u {
        ParsedUpdate::InsertData { inner, source }
        | ParsedUpdate::DeleteData { inner, source }
        | ParsedUpdate::DeleteInsert { inner, source }
        | ParsedUpdate::GraphManagement { inner, source } => (&inner.operations, source.as_str()),
        ParsedUpdate::UnsupportedForm { .. } => {
            return Err(SparqlError::UnsupportedAlgebra(
                "update form not supported in Stage 1".into(),
            ));
        }
    };

    // Recover the `SILENT` flag spargebra dropped from `ADD`/`MOVE`/`COPY` and
    // align it with the desugared copy-graph ops by occurrence order.
    let op_hints = align_amc_hints(ops, source);

    // Atomicity (§3.1.3): preflight every op for the errors it would hit at
    // apply time — using the pre-update store for D11 existence — so a failing
    // multi-op request mutates nothing. See the module doc.
    for (op, hint) in ops.iter().zip(&op_hints) {
        validate_op(op, cfg, store, *hint)?;
    }

    for (op, hint) in ops.iter().zip(&op_hints) {
        match op {
            GraphUpdateOperation::InsertData { data } => {
                let mut adds: Vec<AlgebraQuad> = Vec::with_capacity(data.len());
                for q in data {
                    let g = spg_graph_name(&q.graph_name);
                    let s = subject_to_term(&q.subject);
                    let p = Term::Iri(q.predicate.as_str().to_owned());
                    let o = object_to_term(&q.object)?;
                    adds.push((g, s, p, o));
                }
                store.apply_quads(Vec::new(), adds)?;
            }
            GraphUpdateOperation::DeleteData { data } => {
                let mut dels: Vec<AlgebraQuad> = Vec::with_capacity(data.len());
                for q in data {
                    let g = spg_graph_name(&q.graph_name);
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
                // ADD/COPY of an absent named source with the recovered SILENT
                // flag is a no-op; without it, an error (caught in preflight).
                if let AmcSourceStatus::Skip = amc_source_status(op, *hint, store) {
                    continue;
                }
                apply_delete_insert(store, cfg, delete, insert, using.as_ref(), pattern)?;
            }
            GraphUpdateOperation::Clear { silent, graph } => {
                apply_clear_drop(store, "CLEAR", *silent, graph)?;
            }
            GraphUpdateOperation::Drop { silent, graph } => {
                apply_clear_drop(store, "DROP", *silent, graph)?;
            }
            GraphUpdateOperation::Create { silent, graph } => {
                apply_create(store, *silent, graph.as_str())?;
            }
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

/// Preflight one operation: return the error it *would* produce at apply time,
/// without mutating (SPARQL Update atomicity, §3.1.3). Mirrors every rejecting
/// path in the apply loop.
///
/// Two apply-time checks read state the preflight cannot see, so they cannot be
/// mirrored exactly: (a) D11 existence (read here against the *pre-update*
/// store), and (b) a reserved-graph write through a **variable** template graph
/// (`resolve_graph_name` can only test it once a WHERE row binds the variable).
/// In both, the POLICY still fires unconditionally — a reserved write always
/// errors, a missing/existing graph always errors — so only multi-op
/// *atomicity* (nothing-mutated-on-failure) can slip: a later op whose D11
/// existence an earlier op flipped, or a variable-bound reserved write an
/// earlier op's mutation preceded. Closing either would need store-level
/// rollback (out of scope); a single op and independent ops are exact.
fn validate_op<B: FullBackend>(
    op: &GraphUpdateOperation,
    cfg: &SparqlConfig,
    store: &B,
    hint: Option<AmcHint>,
) -> Result<()> {
    match op {
        GraphUpdateOperation::InsertData { data } => {
            for q in data {
                reserved_write_check(&q.graph_name)?;
                object_to_term(&q.object)?;
            }
            Ok(())
        }
        GraphUpdateOperation::DeleteData { data } => {
            for q in data {
                reserved_write_check(&q.graph_name)?;
                ground_term_to_term(&q.object)?;
            }
            Ok(())
        }
        GraphUpdateOperation::DeleteInsert {
            delete,
            insert,
            using: _,
            pattern,
        } => {
            validate_delete_insert(delete, insert, pattern, cfg)?;
            // ADD/COPY source existence (SILENT-recoverable) — mirrored so the
            // preflight aborts before any destructive pre-Drop runs. `Skip`
            // (silent + absent) passes preflight; the apply loop then skips it.
            if let AmcSourceStatus::Err(e) = amc_source_status(op, hint, store) {
                return Err(e);
            }
            Ok(())
        }
        GraphUpdateOperation::Clear { silent, graph } => {
            validate_clear_drop("CLEAR", *silent, graph, store)
        }
        GraphUpdateOperation::Drop { silent, graph } => {
            validate_clear_drop("DROP", *silent, graph, store)
        }
        GraphUpdateOperation::Create { silent, graph } => {
            let iri = graph.as_str();
            reserved_iri_write_check(iri)?;
            if store.graph_exists(iri) && !*silent {
                return Err(create_graph_exists_error(iri));
            }
            Ok(())
        }
        GraphUpdateOperation::Load {
            silent,
            source,
            destination,
        } => validate_load(*silent, source, destination),
    }
}

// ── Reserved-namespace checks (S4) ───────────────────────────────────────────

/// Reject a write whose graph names the reserved namespace (data/DATA quad
/// side: a `GraphName` is default or a ground IRI).
fn reserved_write_check(g: &SpgGraphName) -> Result<()> {
    if let SpgGraphName::NamedNode(n) = g {
        reserved_iri_write_check(n.as_str())?;
    }
    Ok(())
}

/// Reject a write whose graph names the reserved namespace (template side: a
/// `GraphNamePattern` is default / a ground IRI / a WHERE-bound variable — the
/// variable case is checked at apply time once the row binds it).
fn reserved_template_check(g: &GraphNamePattern) -> Result<()> {
    if let GraphNamePattern::NamedNode(n) = g {
        reserved_iri_write_check(n.as_str())?;
    }
    Ok(())
}

fn reserved_iri_write_check(iri: &str) -> Result<()> {
    if is_reserved_graph(iri) {
        return Err(reserved_graph_write_error(iri));
    }
    Ok(())
}

// ── CLEAR / DROP (D11) ───────────────────────────────────────────────────────

/// Preflight for `CLEAR`/`DROP`: reserved-namespace check (not suppressible),
/// then D11 existence for a single named target.
fn validate_clear_drop<B: FullBackend>(
    verb: &str,
    silent: bool,
    graph: &GraphTarget,
    store: &B,
) -> Result<()> {
    match graph {
        GraphTarget::DefaultGraph | GraphTarget::AllGraphs | GraphTarget::NamedGraphs => Ok(()),
        GraphTarget::NamedNode(n) => {
            let iri = n.as_str();
            reserved_iri_write_check(iri)?;
            if !store.graph_exists(iri) && !silent {
                return Err(missing_graph_error(verb, iri));
            }
            Ok(())
        }
    }
}

/// Apply `CLEAR`/`DROP` under D11 (SPEC-28 S4). `CLEAR` and `DROP` behave
/// identically here — the store keeps no per-graph state beyond the quads a
/// graph holds, so "clear the contents" and "drop the graph" both retract
/// every visible quad through the store (never a structural unlink, so the
/// incremental delta path sees each retraction).
fn apply_clear_drop<B: FullBackend>(
    store: &mut B,
    verb: &str,
    silent: bool,
    graph: &GraphTarget,
) -> Result<()> {
    match graph {
        // Sweep just the default graph. Never an error (the default graph
        // always exists), even when it is empty.
        GraphTarget::DefaultGraph => {
            store.clear_graph(&GraphTarget::DefaultGraph)?;
            Ok(())
        }
        // `ALL` = the default graph + every *non-reserved* named graph, quad by
        // quad. Reserved graphs are left untouched (SPEC-30 owns the
        // store-level reset), so this cannot use `clear_graph(AllGraphs)`.
        GraphTarget::AllGraphs => {
            store.clear_graph(&GraphTarget::DefaultGraph)?;
            clear_named_graphs(store)?;
            Ok(())
        }
        // `NAMED` = every non-reserved named graph (default graph left alone).
        GraphTarget::NamedGraphs => clear_named_graphs(store),
        GraphTarget::NamedNode(n) => {
            let iri = n.as_str();
            reserved_iri_write_check(iri)?;
            if store.graph_exists(iri) {
                store.clear_graph(graph)?;
            } else if !silent {
                return Err(missing_graph_error(verb, iri));
            }
            Ok(())
        }
    }
}

/// Sweep every non-reserved named graph quad by quad. Backs `DROP ALL`'s
/// named-graph half and `CLEAR`/`DROP NAMED`.
fn clear_named_graphs<B: FullBackend>(store: &mut B) -> Result<()> {
    for g in store.graphs() {
        if is_reserved_graph(&g) {
            continue;
        }
        store.clear_graph(&GraphTarget::NamedNode(named_node(&g)))?;
    }
    Ok(())
}

// ── CREATE (D11) ─────────────────────────────────────────────────────────────

/// Apply `CREATE GRAPH <g>` under D11: reserved-namespace check first, then —
/// with no registry — an existing graph errors (unless `SILENT`) and an absent
/// graph is a no-op that succeeds (a graph exists only once it holds a quad, so
/// `CREATE` cannot conjure an empty one).
fn apply_create<B: FullBackend>(store: &mut B, silent: bool, iri: &str) -> Result<()> {
    reserved_iri_write_check(iri)?;
    if store.graph_exists(iri) && !silent {
        return Err(create_graph_exists_error(iri));
    }
    Ok(())
}

// ── Pattern updates ──────────────────────────────────────────────────────────

/// Shared rejection scan for a pattern-based update, without mutating — used
/// both by the atomicity preflight and by `apply_delete_insert`.
///
/// The `USING`/`USING NAMED` blanket rejection and the WHERE-side `GRAPH`
/// rejection are both **gone** (SPEC-28 phase 4): the WHERE clause now runs
/// through the phase-3 query path, which understands `GRAPH`, and inherits
/// phase-3's own two `GRAPH ?g` refusals (expected).
fn validate_delete_insert(
    delete: &[GroundQuadPattern],
    insert: &[QuadPattern],
    pattern: &GraphPattern,
    cfg: &SparqlConfig,
) -> Result<()> {
    // Reserved-namespace check on any ground template graph (a variable graph
    // is checked at apply time once the WHERE row binds it).
    for q in delete {
        reserved_template_check(&q.graph_name)?;
    }
    for q in insert {
        reserved_template_check(&q.graph_name)?;
    }

    // Reject RDF 1.2 triple-term slots in any DELETE/INSERT template (the
    // Stage-1 store has no triple-term slot), so the `resolve_*` `Triple(_)`
    // arms are unreachable for that reason.
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

    // Translate and plan the WHERE clause now (pure — no store access) so an
    // unsupported algebra construct aborts the whole update before any earlier
    // op mutates. The throwaway plan is recomputed in `apply_delete_insert`.
    let alg = translate_where(pattern, cfg)?;
    planner::plan(&alg)?;
    Ok(())
}

/// Evaluate the WHERE pattern, then instantiate the DELETE/INSERT templates
/// per solution and route each instantiated quad to the graph its
/// `GraphNamePattern` names (SPARQL 1.1 §3.1.3: deletions before insertions,
/// both derived from the pre-update graph).
///
/// **spargebra-`WITH` finding (0.4.6):** `WITH <g>` injects `<g>` into every
/// DELETE/INSERT template quad whose graph is the default graph, **and** — when
/// no explicit `USING` is written — sets `using = Some(default:[g])`. It does
/// **not** wrap the WHERE `pattern` in `GraphPattern::Graph`. So honouring
/// `using` here scopes the WHERE side to `<g>` exactly as `WITH` intends; no
/// manual wrapping is needed (wrapping would double-scope). This is why
/// `USING`/`WITH` share one code path: the WHERE dataset is built from `using`.
fn apply_delete_insert<B: FullBackend>(
    store: &mut B,
    cfg: &SparqlConfig,
    delete: &[GroundQuadPattern],
    insert: &[QuadPattern],
    using: Option<&QueryDataset>,
    pattern: &GraphPattern,
) -> Result<()> {
    validate_delete_insert(delete, insert, pattern, cfg)?;

    let alg = translate_where(pattern, cfg)?;
    let plan = planner::plan(&alg)?;

    // The WHERE dataset comes from `USING`/`USING NAMED` (and, via spargebra,
    // `WITH`); absent a clause, `dataset_spec_from(None)` is the empty spec and
    // the strict mode pins the WHERE to the store's default graph — the graph a
    // default-graph template writes, so bound rows and written rows agree. When
    // `USING`/`WITH` set an explicit `default`, that overrides the mode (the
    // dataset's `default` decides), so the mode passed here only matters for
    // the no-clause case.
    let dataset: DatasetSpec = dataset_spec_from(&using.cloned());
    let rows: Vec<Bindings> = Runtime::new(store)
        .with_dataset(dataset, DefaultGraphMode::Strict)
        .run(&plan)?
        .collect();

    // Deletions first, from the original bindings.
    let mut dels: Vec<AlgebraQuad> = Vec::new();
    for row in &rows {
        for q in delete {
            if let (Some(g), Some(s), Some(p), Some(o)) = (
                resolve_graph_name(&q.graph_name, row)?,
                resolve_ground(&q.subject, row).and_then(subject_or_skip),
                resolve_pred(&q.predicate, row),
                resolve_ground(&q.object, row),
            ) {
                dels.push((g, s, p, o));
            }
        }
    }
    // Insertions allocate fresh blank nodes per solution row.
    let mut adds: Vec<AlgebraQuad> = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        for q in insert {
            if let (Some(g), Some(s), Some(p), Some(o)) = (
                resolve_graph_name(&q.graph_name, row)?,
                resolve_term(&q.subject, row, i).and_then(subject_or_skip),
                resolve_pred(&q.predicate, row),
                resolve_term(&q.object, row, i),
            ) {
                adds.push((g, s, p, o));
            }
        }
    }

    // One atomic batch (SPEC-28 S6): dels apply before adds.
    store.apply_quads(dels, adds)?;
    Ok(())
}

/// Resolve a template quad's `GraphNamePattern` to a store [`GraphName`]:
/// `Some(None)` = default graph, `Some(Some(iri))` = a named graph, `None` =
/// skip this quad (an unbound variable, or a variable bound to a non-IRI). A
/// resolved reserved graph is a write error (S4), checked here because a
/// variable graph is only known once the row binds it.
fn resolve_graph_name(g: &GraphNamePattern, row: &Bindings) -> Result<Option<GraphName>> {
    let iri = match g {
        GraphNamePattern::DefaultGraph => return Ok(Some(None)),
        GraphNamePattern::NamedNode(n) => n.as_str().to_owned(),
        GraphNamePattern::Variable(v) => match row.get(v.as_str()) {
            Some(Term::Iri(s)) => s.clone(),
            // Unbound, or bound to a literal/blank node: no legal graph → skip.
            _ => return Ok(None),
        },
    };
    reserved_iri_write_check(&iri)?;
    Ok(Some(Some(iri)))
}

// ── LOAD ─────────────────────────────────────────────────────────────────────

/// Preflight for `LOAD`: reserved-namespace destination check (not
/// suppressible), then — for a non-silent load — the dataset-format/`INTO`
/// error and a fetch+parse of the source (a pure read that surfaces a
/// fetch/parse failure before any earlier op mutates).
fn validate_load(
    silent: bool,
    source: &spargebra::term::NamedNode,
    destination: &SpgGraphName,
) -> Result<()> {
    if let SpgGraphName::NamedNode(n) = destination {
        reserved_iri_write_check(n.as_str())?;
    }
    if silent {
        // A silent LOAD swallows every non-reserved failure, so nothing else to
        // preflight (the reserved check above already ran and is not silent).
        return Ok(());
    }
    if let SpgGraphName::NamedNode(n) = destination {
        if is_dataset_format(source.as_str()) {
            return Err(load_dataset_into_graph_error(source.as_str(), n.as_str()));
        }
    }
    fetch_and_parse(source.as_str()).map(|_| ())
}

/// Apply `LOAD <source> [INTO GRAPH <destination>]`. Routing:
/// * triples format (`.nt`/`.ttl`/default) → the destination graph (default
///   graph if no `INTO`);
/// * dataset format (`.nq`/`.trig`) with no `INTO` → each quad to its own named
///   graph (matching the N-Quads bulk loader);
/// * dataset format with `INTO GRAPH` → error (undefined redirection).
///
/// `file:`-only (#189). A boundary failure is an error unless `SILENT`.
fn apply_load<B: FullBackend>(
    store: &mut B,
    silent: bool,
    source: &spargebra::term::NamedNode,
    destination: &SpgGraphName,
) -> Result<()> {
    // Reserved destination: refused regardless of SILENT.
    if let SpgGraphName::NamedNode(n) = destination {
        reserved_iri_write_check(n.as_str())?;
    }
    // Dataset-format INTO is undefined — known from the extension, no fetch.
    if let SpgGraphName::NamedNode(n) = destination {
        if is_dataset_format(source.as_str()) {
            return if silent {
                Ok(())
            } else {
                Err(load_dataset_into_graph_error(source.as_str(), n.as_str()))
            };
        }
    }
    match fetch_and_parse(source.as_str()) {
        Ok(quads) => {
            let adds: Vec<AlgebraQuad> = match destination {
                // No `INTO`: each quad keeps its parsed graph (None = default
                // for triples formats; the parsed graph for dataset formats).
                SpgGraphName::DefaultGraph => quads,
                // `INTO GRAPH <g>` of a triples format: route every triple to
                // `<g>` (a dataset format was rejected above).
                SpgGraphName::NamedNode(n) => {
                    let g = n.as_str().to_owned();
                    quads
                        .into_iter()
                        .map(|(_, s, p, o)| (Some(g.clone()), s, p, o))
                        .collect()
                }
            };
            store.apply_quads(Vec::new(), adds)?;
            Ok(())
        }
        Err(e) => {
            if silent {
                Ok(())
            } else {
                Err(e)
            }
        }
    }
}

/// True if `source`'s extension names a dataset (quad) serialization.
fn is_dataset_format(source: &str) -> bool {
    matches!(
        source_extension(source).as_deref(),
        Some("nq") | Some("trig")
    )
}

/// The lower-cased file extension of `source`, if any.
fn source_extension(source: &str) -> Option<String> {
    source.rsplit('.').next().map(str::to_ascii_lowercase)
}

/// Fetch and parse an RDF document named by `source`, returning its quads as
/// algebra [`Term`]s tagged by graph. Stage-1 supports `file:` IRIs only.
/// Triples formats (`.nt`/`.ttl`/default) tag every quad with the default graph
/// (`None`); dataset formats (`.nq`/`.trig`) tag each quad with its own graph
/// name, so a plain `LOAD` can route them.
fn fetch_and_parse(source: &str) -> Result<Vec<AlgebraQuad>> {
    use oxttl::{NQuadsParser, NTriplesParser, TriGParser, TurtleParser};

    let raw = file_iri_to_path(source)?;
    // A file IRI percent-encodes reserved characters (e.g. a space as `%20`);
    // decode to the real filesystem path before reading.
    let path = percent_decode(&raw);

    let bytes = std::fs::read(&path)
        .map_err(|e| SparqlError::Executor(format!("LOAD reading {path}: {e}")))?;
    let map_err =
        |e: oxttl::TurtleSyntaxError| SparqlError::Executor(format!("LOAD parsing {path}: {e}"));

    let mut out: Vec<AlgebraQuad> = Vec::new();
    match source_extension(&path).as_deref() {
        // N-Triples/N-Quads require absolute IRIs (no base).
        Some("nt") => {
            for t in NTriplesParser::new().for_slice(&bytes) {
                let t = t.map_err(map_err)?;
                let (s, p, o) = oxrdf_triple_to_terms(&t.subject, &t.predicate, &t.object);
                out.push((None, s, p, o));
            }
        }
        Some("nq") => {
            for q in NQuadsParser::new().for_slice(&bytes) {
                let q = q.map_err(map_err)?;
                let (s, p, o) = oxrdf_triple_to_terms(&q.subject, &q.predicate, &q.object);
                out.push((oxrdf_graph_to_name(&q.graph_name), s, p, o));
            }
        }
        // Turtle/TriG may carry relative IRIs resolved against the document IRI.
        Some("trig") => {
            let parser = with_base(TriGParser::new(), source)?;
            for q in parser.for_slice(&bytes) {
                let q = q.map_err(map_err)?;
                let (s, p, o) = oxrdf_triple_to_terms(&q.subject, &q.predicate, &q.object);
                out.push((oxrdf_graph_to_name(&q.graph_name), s, p, o));
            }
        }
        // `.ttl` and anything else default to Turtle (a triples format).
        _ => {
            let parser = with_base(TurtleParser::new(), source)?;
            for t in parser.for_slice(&bytes) {
                let t = t.map_err(map_err)?;
                let (s, p, o) = oxrdf_triple_to_terms(&t.subject, &t.predicate, &t.object);
                out.push((None, s, p, o));
            }
        }
    }
    Ok(out)
}

/// Lower an `oxrdf` graph name to a store [`GraphName`]. A blank-node graph
/// name is kept as its label (rare; matches the loader's best effort).
fn oxrdf_graph_to_name(g: &oxrdf::GraphName) -> GraphName {
    match g {
        oxrdf::GraphName::DefaultGraph => None,
        oxrdf::GraphName::NamedNode(n) => Some(n.as_str().to_owned()),
        oxrdf::GraphName::BlankNode(b) => Some(b.as_str().to_owned()),
    }
}

/// Set the document IRI as the parser's base so relative IRIs in Turtle/TriG
/// resolve against `source`.
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
/// yields `/abs`. A non-empty, non-`localhost` authority is rejected. A
/// non-`file:` source is rejected (Stage-1 fetches `file:` only).
fn file_iri_to_path(source: &str) -> Result<String> {
    let non_file = || {
        SparqlError::UnsupportedAlgebra(format!(
            "LOAD of a non-file source (Stage-1 fetches file: IRIs only): {source}"
        ))
    };
    if let Some(rest) = source.strip_prefix("file://") {
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
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
        Ok(path.to_owned())
    } else {
        Err(non_file())
    }
}

/// Percent-decode a file-IRI path component (RFC 3986). A `%XX` escape becomes
/// the decoded byte; a malformed escape is left verbatim. The decoded byte
/// sequence is interpreted as UTF-8 (lossy).
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
/// Blank-node labels are carried through verbatim, sharing the Stage-1 store's
/// known blank-node approximation with the bulk loaders.
fn oxrdf_subject_to_term(s: &oxrdf::NamedOrBlankNode) -> Term {
    match s {
        oxrdf::NamedOrBlankNode::NamedNode(n) => Term::Iri(n.as_str().to_owned()),
        oxrdf::NamedOrBlankNode::BlankNode(b) => Term::BlankNode(b.as_str().to_owned()),
    }
}

/// Lower an `oxrdf` object term to an algebra [`Term`]. Literals keep their
/// N-Triples lexical form.
fn oxrdf_term_to_term(t: &oxrdf::Term) -> Term {
    match t {
        oxrdf::Term::NamedNode(n) => Term::Iri(n.as_str().to_owned()),
        oxrdf::Term::BlankNode(b) => Term::BlankNode(b.as_str().to_owned()),
        oxrdf::Term::Literal(l) => Term::Literal(l.to_string()),
        // RDF 1.2 triple-term objects: best-effort lexical form (the same
        // lowering the loader applies).
        oxrdf::Term::Triple(tr) => Term::Literal(tr.to_string()),
    }
}

// ── ADD / MOVE / COPY: SILENT recovery ───────────────────────────────────────

/// One of the three graph-management verbs spargebra desugars away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AmcVerb {
    Add,
    Move,
    Copy,
}

/// A recovered `(verb, silent)` hint for one `ADD`/`MOVE`/`COPY` in the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AmcHint {
    verb: AmcVerb,
    silent: bool,
}

/// The recognized shape of a spargebra `copy_graph` desugaring — the
/// `DeleteInsert` that every non-identity `ADD`/`MOVE`/`COPY` produces.
struct CopyGraph {
    /// `None` = the default-graph source (always exists); `Some(iri)` = a named
    /// source graph.
    source: Option<String>,
}

/// Recognize the `DeleteInsert` that `ADD`/`MOVE`/`COPY` desugar to (spargebra
/// `copy_graph`): empty delete, one `(?s ?p ?o)` insert quad, no `USING`, and a
/// WHERE of either `{ ?s ?p ?o }` (default source) or
/// `GRAPH <from> { ?s ?p ?o }` (named source). Only the shape identifies it;
/// the aligned hint (below) supplies the verb and the recovered `SILENT` flag.
fn as_copy_graph(op: &GraphUpdateOperation) -> Option<CopyGraph> {
    let GraphUpdateOperation::DeleteInsert {
        delete,
        insert,
        using,
        pattern,
    } = op
    else {
        return None;
    };
    if !delete.is_empty() || using.is_some() || insert.len() != 1 {
        return None;
    }
    let q = &insert[0];
    if !is_term_var(&q.subject, "s")
        || !is_pred_var(&q.predicate, "p")
        || !is_term_var(&q.object, "o")
    {
        return None;
    }
    match pattern.as_ref() {
        GraphPattern::Bgp { patterns } if is_spo_bgp(patterns) => Some(CopyGraph { source: None }),
        GraphPattern::Graph {
            name: NamedNodePattern::NamedNode(from),
            inner,
        } => match inner.as_ref() {
            GraphPattern::Bgp { patterns } if is_spo_bgp(patterns) => Some(CopyGraph {
                source: Some(from.as_str().to_owned()),
            }),
            _ => None,
        },
        _ => None,
    }
}

fn is_term_var(t: &TermPattern, name: &str) -> bool {
    matches!(t, TermPattern::Variable(v) if v.as_str() == name)
}
fn is_pred_var(p: &NamedNodePattern, name: &str) -> bool {
    matches!(p, NamedNodePattern::Variable(v) if v.as_str() == name)
}
fn is_spo_bgp(patterns: &[spargebra::term::TriplePattern]) -> bool {
    patterns.len() == 1
        && is_term_var(&patterns[0].subject, "s")
        && is_pred_var(&patterns[0].predicate, "p")
        && is_term_var(&patterns[0].object, "o")
}

/// Per-op recovered `SILENT` hints, indexed to line up with `ops`.
///
/// spargebra desugars each non-identity `ADD`/`MOVE`/`COPY` into exactly one
/// `copy_graph` `DeleteInsert` (plus, for `MOVE`/`COPY`, `Drop` ops that keep
/// their flags). [`scan_amc_silent_hints`] recovers `(verb, silent)` per source
/// occurrence in order. Three cases:
///
/// * **Aligned** (hint count == copy-op count): attach each recovered hint to
///   its copy-op positionally.
/// * **Ambiguous** (≥1 `ADD`/`MOVE`/`COPY` token present, but the counts differ
///   — an identity `ADD <g> TO <g>` is one token but zero ops, or a user
///   `DeleteInsert` mimics the copy shape): the alignment cannot be trusted, so
///   fall back to **non-silent** for every copy-op. This is
///   deliberate — a `COPY`'s only absent-source guard is
///   [`amc_source_status`] (its desugaring has a destination `Drop` but no
///   source `Drop`), so a silent-equivalent `None` fallback here would let a
///   non-silent `COPY <absent> TO <dst>` run the unconditional `Drop(<dst>)`
///   and wipe an existing destination with no error, which SPARQL 1.1 §3.2.4
///   forbids. Non-silent means an absent source errors in preflight, before any
///   destructive `Drop` runs; a present source still copies. "An honest error,
///   never a silent wrong outcome" (PLAN-28-04 §Design). `MOVE`'s own preserved
///   source-`Drop` flag is an independent guard, and an identity case has no
///   copy-op to error on, so both stay correct.
/// * **No AMC tokens at all** (`hints` empty): every copy-shaped op is a genuine
///   user `DeleteInsert`, never a desugared AMC — leave it plain (`None`). A
///   user `INSERT { … } WHERE { GRAPH <absent> { … } }` reads zero rows and is a
///   valid no-op; forcing it to error would violate SPARQL semantics, and it
///   carries no destination `Drop`, so there is no data to lose.
fn align_amc_hints(ops: &[GraphUpdateOperation], source: &str) -> Vec<Option<AmcHint>> {
    let hints = scan_amc_silent_hints(source);
    let copy_positions: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, op)| as_copy_graph(op).is_some())
        .map(|(i, _)| i)
        .collect();
    let mut out = vec![None; ops.len()];
    if copy_positions.is_empty() {
        return out; // no copy-op to hint
    }
    if hints.len() == copy_positions.len() {
        for (h, &pos) in hints.iter().zip(&copy_positions) {
            out[pos] = Some(*h);
        }
    } else if !hints.is_empty() {
        // Ambiguous: force the non-silent source-existence check on every
        // copy-op. The verb is unknown here; `Copy` is the "run the check"
        // marker — `Add` and `Copy` check identically, and `Move` keeps its own
        // source-`Drop` guard, so forcing the check is safe for all three.
        for &pos in &copy_positions {
            out[pos] = Some(AmcHint {
                verb: AmcVerb::Copy,
                silent: false,
            });
        }
    }
    // else: no AMC tokens — leave every copy-shaped op plain (`None`).
    out
}

/// Outcome of an `ADD`/`COPY` source-existence test for one copy-op.
enum AmcSourceStatus {
    /// Not an aligned `ADD`/`COPY` copy-op, or its source exists — proceed.
    Ok,
    /// Aligned, silent, and the named source is absent — skip (a no-op).
    Skip,
    /// Aligned, non-silent, and the named source is absent — error.
    Err(SparqlError),
}

/// Apply the recovered `SILENT` flag to an `ADD`/`COPY` copy-op: a missing
/// named source is a no-op when silent, an error when not (SPARQL 1.1
/// §3.2.3/§3.2.5). `MOVE` (verb `Move`) is handled by its own preserved
/// source-`Drop` flag, so it falls through to `Ok` here. A copy-op with **no**
/// hint (`None` — the token-free plain case, see [`align_amc_hints`]) also
/// falls through: a user `DeleteInsert` that just reads an absent graph is a
/// valid no-op, never an error.
fn amc_source_status<B: FullBackend>(
    op: &GraphUpdateOperation,
    hint: Option<AmcHint>,
    store: &B,
) -> AmcSourceStatus {
    let Some(hint) = hint else {
        return AmcSourceStatus::Ok;
    };
    if !matches!(hint.verb, AmcVerb::Add | AmcVerb::Copy) {
        return AmcSourceStatus::Ok;
    }
    let Some(cg) = as_copy_graph(op) else {
        return AmcSourceStatus::Ok;
    };
    match cg.source {
        // Default source always exists.
        None => AmcSourceStatus::Ok,
        Some(iri) => {
            if store.graph_exists(&iri) {
                AmcSourceStatus::Ok
            } else if hint.silent {
                AmcSourceStatus::Skip
            } else {
                AmcSourceStatus::Err(amc_source_missing_error(&iri))
            }
        }
    }
}

/// Scan the raw update text for `ADD`/`MOVE`/`COPY` keywords and the `SILENT`
/// modifier that spargebra's desugaring discards, in source order.
///
/// A small hand tokenizer (no regex) that skips the three lexical contexts a
/// bare keyword scan would trip on: `# …` comments, `<…>` IRIs, and `"…"` /
/// `'…'` string literals (single- and triple-quoted). Everything else is read
/// as whitespace-delimited words; a word equal to `ADD`/`MOVE`/`COPY` (ASCII
/// case-insensitive, not a prefixed name and not preceded by `:`/`?`/`$`)
/// records a hint, whose `silent` is whether the next word is `SILENT`.
///
/// This is a stopgap: spargebra 0.4.6 drops the `SILENT` flag when it rewrites
/// `ADD`/`MOVE`/`COPY` into `Drop`+`DeleteInsert` (its parser's `Add`/`Move`/
/// `Copy` rules take `silent` but discard it for `ADD`, and keep it only on
/// `MOVE`/`COPY`'s source-`Drop`). File an upstream issue on the spargebra
/// (oxigraph) tracker asking for a structured Add/Move/Copy op — or a preserved
/// `silent` flag on the desugared ops — and link it here; delete this whole
/// tokenizer the day that ships. See PLAN-28-04 §Design (ADD/MOVE/COPY).
fn scan_amc_silent_hints(src: &str) -> Vec<AmcHint> {
    let b = src.as_bytes();
    let n = b.len();
    let mut i = 0;
    let mut out = Vec::new();
    while i < n {
        match b[i] {
            b'#' => {
                while i < n && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'<' => i = skip_iri(b, i),
            b'"' | b'\'' => i = skip_string(b, i),
            c if is_word_char(c) => {
                let start = i;
                while i < n && is_word_char(b[i]) {
                    i += 1;
                }
                // A prefixed name (`ex:ADD` or `ADD:foo`) or a variable
                // (`?ADD`/`$ADD`) is not the keyword.
                let prefixed_before = start > 0 && matches!(b[start - 1], b':' | b'?' | b'$');
                let prefixed_after = i < n && b[i] == b':';
                if !prefixed_before && !prefixed_after {
                    if let Some(verb) = amc_verb(&src[start..i]) {
                        out.push(AmcHint {
                            verb,
                            silent: next_word_is_silent(b, src, i),
                        });
                    }
                }
            }
            _ => i += 1,
        }
    }
    out
}

/// A word byte for keyword scanning: ASCII alphanumeric or `_`.
fn is_word_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

fn amc_verb(word: &str) -> Option<AmcVerb> {
    if word.eq_ignore_ascii_case("ADD") {
        Some(AmcVerb::Add)
    } else if word.eq_ignore_ascii_case("MOVE") {
        Some(AmcVerb::Move)
    } else if word.eq_ignore_ascii_case("COPY") {
        Some(AmcVerb::Copy)
    } else {
        None
    }
}

/// Skip a `<…>` IRI starting at `i` (a `<`). If the run to the next `>` looks
/// like an IRI (no whitespace, no nested `<`), skip past the `>`; otherwise the
/// `<` was a comparison operator — advance one byte.
fn skip_iri(b: &[u8], i: usize) -> usize {
    let n = b.len();
    let mut j = i + 1;
    while j < n {
        match b[j] {
            b'>' => return j + 1,
            c if c.is_ascii_whitespace() || c == b'<' => return i + 1,
            _ => j += 1,
        }
    }
    i + 1
}

/// Skip a string literal starting at `i` (a `"` or `'`). Handles triple-quoted
/// and single-quoted forms with `\` escapes; a single-quoted string ends at the
/// closing quote or a newline (SPARQL single-line strings hold no raw newline).
fn skip_string(b: &[u8], i: usize) -> usize {
    let n = b.len();
    let q = b[i];
    // Triple-quoted?
    if i + 2 < n && b[i + 1] == q && b[i + 2] == q {
        let mut j = i + 3;
        while j < n {
            if b[j] == b'\\' {
                j += 2;
                continue;
            }
            if j + 2 < n && b[j] == q && b[j + 1] == q && b[j + 2] == q {
                return j + 3;
            }
            j += 1;
        }
        return n;
    }
    let mut j = i + 1;
    while j < n {
        match b[j] {
            b'\\' => j += 2,
            c if c == q => return j + 1,
            b'\n' => return j + 1,
            _ => j += 1,
        }
    }
    n
}

/// After a verb keyword ending at `i`, is the next word `SILENT`? Skips
/// whitespace and `# …` comments between the two.
fn next_word_is_silent(b: &[u8], src: &str, i: usize) -> bool {
    let n = b.len();
    let mut j = i;
    loop {
        while j < n && b[j].is_ascii_whitespace() {
            j += 1;
        }
        if j < n && b[j] == b'#' {
            while j < n && b[j] != b'\n' {
                j += 1;
            }
            continue;
        }
        break;
    }
    let start = j;
    while j < n && is_word_char(b[j]) {
        j += 1;
    }
    start < j && src[start..j].eq_ignore_ascii_case("SILENT")
}

// ── Small lowering helpers ───────────────────────────────────────────────────

/// A ground quad-data graph slot → store [`GraphName`].
fn spg_graph_name(g: &SpgGraphName) -> GraphName {
    match g {
        SpgGraphName::DefaultGraph => None,
        SpgGraphName::NamedNode(n) => Some(n.as_str().to_owned()),
    }
}

fn named_node(iri: &str) -> spargebra::term::NamedNode {
    spargebra::term::NamedNode::new_unchecked(iri)
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

/// Resolve an INSERT-template `TermPattern` against a solution row.
/// `row_ix` scopes per-solution blank nodes (SPARQL 1.1 §4.1.4). `None` when a
/// variable slot is unbound (the caller drops the triple).
///
/// Lockstep invariant: mirrors `runtime.rs::construct_triples`'s `resolve_term`.
fn resolve_term(t: &TermPattern, row: &Bindings, row_ix: usize) -> Option<Term> {
    match t {
        TermPattern::NamedNode(n) => Some(Term::Iri(n.as_str().to_owned())),
        TermPattern::Literal(l) => Some(Term::Literal(l.to_string())),
        TermPattern::BlankNode(b) => Some(Term::BlankNode(format!("{}_r{row_ix}", b.as_str()))),
        TermPattern::Variable(v) => row.get(v.as_str()).cloned(),
        // Rejected up front in `validate_delete_insert`; kept exhaustive.
        TermPattern::Triple(_) => None,
    }
}

/// Resolve a DELETE-template `GroundTermPattern` (no blank nodes in DELETE
/// templates) against a solution row.
fn resolve_ground(t: &GroundTermPattern, row: &Bindings) -> Option<Term> {
    match t {
        GroundTermPattern::NamedNode(n) => Some(Term::Iri(n.as_str().to_owned())),
        GroundTermPattern::Literal(l) => Some(Term::Literal(l.to_string())),
        GroundTermPattern::Variable(v) => row.get(v.as_str()).cloned(),
        GroundTermPattern::Triple(_) => None,
    }
}

/// Resolve a predicate template slot: a predicate variable binding is valid
/// only if it resolves to an IRI. Shared invariant with
/// `runtime.rs::construct_triples`'s `resolve_pred`.
fn resolve_pred(p: &NamedNodePattern, row: &Bindings) -> Option<Term> {
    match p {
        NamedNodePattern::NamedNode(n) => Some(Term::Iri(n.as_str().to_owned())),
        NamedNodePattern::Variable(v) => match row.get(v.as_str()) {
            Some(Term::Iri(s)) => Some(Term::Iri(s.clone())),
            _ => None,
        },
    }
}

/// Position-aware subject guard: an instantiated template triple is legal RDF
/// only if its subject is an IRI or blank node; a literal (or triple term) in
/// subject position is **silently skipped** (SPARQL 1.1 §4.1.4 / §10.2.1), not
/// an error, so the update still succeeds.
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
