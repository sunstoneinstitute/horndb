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
//!   ops execute; the flag — plus the source operand and whether the op is the
//!   identity case (`<g> TO <g>`) — is recovered per verb occurrence by a
//!   source-text pre-scan (see [`recover_amc_hints`]).
//! * **The reserved namespace is closed to writes** (S4): every write form
//!   touching `https://horndb.io/graph/…` is refused. This is a
//!   permission-shaped error that `SILENT` does **not** suppress; it is
//!   checked before any silent/existence logic. Reads of reserved graphs stay
//!   allowed.
//!
//! **Atomicity** (§3.1.3): a multi-operation update preflights the whole
//! request against the pre-update store before the first mutation, so a failing
//! request mutates nothing. Two checks run: a recovered-`SILENT` source
//! existence sweep over the `ADD`/`MOVE`/`COPY` hints, then `validate_op` per
//! operation (reserved namespace, D11 existence, `LOAD` routing/fetch). This is
//! why `COPY <absent> TO DEFAULT` — which desugars to a destructive
//! `Drop{DEFAULT}` + a copy from a missing source — never clears the default
//! graph. One store batch per operation, applied in request order.

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

    // Recover the `SILENT` flag, source operand, and identity check of every
    // `ADD`/`MOVE`/`COPY` from the raw text — spargebra drops all three on
    // desugaring (see [`recover_amc_hints`]).
    let amc_hints = recover_amc_hints(source);

    // Atomicity (§3.1.3), part 1: a non-silent `ADD`/`MOVE`/`COPY` whose named
    // source is absent is an error (SPARQL 1.1 §3.2.3/§3.2.4/§3.2.5), caught
    // before any mutation. The identity case (`<g> TO <g>`) desugars to zero
    // ops and is a no-op even when `<g>` is absent, so it never raises this. An
    // operand that could not be resolved from text (an `Unknown` source) is not
    // existence-checked; its desugared ops apply as-is (a natural no-op on a
    // missing source).
    for h in &amc_hints {
        if !h.silent && !h.is_identity {
            if let AmcSource::Named(g) = &h.source {
                if !store.graph_exists(g) {
                    return Err(amc_source_missing_error(g));
                }
            }
        }
    }

    // Atomicity part 2: preflight every op for the remaining errors it would
    // hit at apply time — reserved namespace, D11 existence (against the
    // pre-update store), `LOAD` routing/fetch. See the module doc.
    for op in ops {
        validate_op(op, cfg, store)?;
    }

    for op in ops {
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
                // An `ADD`/`COPY` copy-op reading an absent named source binds
                // zero rows, so it is a natural no-op; a non-silent absent
                // source was already rejected in the preflight sweep above.
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
/// path in the apply loop except the `ADD`/`MOVE`/`COPY` source-existence
/// check, which runs once over the recovered hints in [`apply_update_with`].
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
        } => validate_delete_insert(delete, insert, pattern, cfg),
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

/// The source operand of an `ADD`/`MOVE`/`COPY`, recovered from raw text.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AmcSource {
    /// `DEFAULT` — the default graph always exists, never a missing-source error.
    Default,
    /// `[GRAPH] <iri>` — the named source graph.
    Named(String),
    /// The operand could not be resolved from the text alone (e.g. a prefixed
    /// name, which needs the query prologue). No source-existence check runs on
    /// it — the desugared ops apply as-is (a natural no-op on a missing source),
    /// the honest, non-destructive outcome.
    Unknown,
}

/// One recovered `ADD`/`MOVE`/`COPY` occurrence: its `SILENT` flag, source
/// operand, and whether it is the W3C identity case (`source == destination`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct AmcHint {
    silent: bool,
    source: AmcSource,
    /// `true` when the recovered source and destination operands are equal —
    /// spargebra's own identity check (`from == to`), which always desugars to
    /// zero operations and is a no-op regardless of whether the graph exists
    /// (SPARQL 1.1 §3.2.3/§3.2.4/§3.2.5). `false` whenever either operand cannot
    /// be resolved from text alone: the conservative default keeps the
    /// existence check enabled.
    is_identity: bool,
}

/// Recover the `SILENT` flag, source operand, and identity check
/// (`source == destination`) of every `ADD`/`MOVE`/`COPY` in the raw update
/// text, in source order.
///
/// **Why this exists.** spargebra 0.4.6 desugars `ADD`/`MOVE`/`COPY` into
/// `Drop` + `DeleteInsert` pairs and **discards the `SILENT` flag**. That was
/// harmless while named graphs were unrepresentable; now a `SILENT COPY
/// <missing> TO <g>` must be a no-op and a non-silent one an error (SPEC-28
/// S4), so the flag matters. Re-scan the source text — a small hand-written
/// tokenizer (no regex) that skips comments, IRIs, and string literals — to
/// recover it, plus the source operand and identity check so the missing-source
/// preflight needs no fragile op-shape matching or token↔op alignment.
///
/// The recovery drives the preflight directly (see [`apply_update_with`]): each
/// hint is independent, so an identity op (zero desugared ops, one verb token)
/// and a user-written copy-shaped `DeleteInsert` can no longer corrupt the
/// alignment, and a user's `SILENT` is always honoured.
///
/// Upstream: `# TODO` — file an issue asking oxigraph/spargebra to preserve the
/// `SILENT` flag on `ADD`/`MOVE`/`COPY` (no issue filed yet; do not invent a
/// number). Delete this whole machinery the day that ships.
fn recover_amc_hints(src: &str) -> Vec<AmcHint> {
    let toks = amc_tokenize(src);
    let mut hints = Vec::new();
    for (i, tok) in toks.iter().enumerate() {
        if *tok != AmcTok::Amc {
            continue;
        }
        // Grammar after the verb: `SILENT? GraphOrDefault TO GraphOrDefault`.
        // Whitespace and comments are not emitted, so the tokens are
        // consecutive and index arithmetic tracks the grammar.
        let mut j = i + 1;
        let silent = matches!(toks.get(j), Some(AmcTok::Silent));
        if silent {
            j += 1;
        }
        let (source, after_source) = amc_parse_operand(&toks, j);
        // `after_source` is `None` only when the source itself could not be
        // resolved (e.g. a prefixed name) — then the destination's token span
        // can't be located either, so identity stays `false` (conservative).
        // Otherwise skip one token for `TO` and parse the destination the same
        // way.
        let destination = after_source.map(|k| amc_parse_operand(&toks, k + 1).0);
        let is_identity = matches!(
            (&source, &destination),
            (AmcSource::Default, Some(AmcSource::Default))
        ) || matches!(
            (&source, &destination),
            (AmcSource::Named(a), Some(AmcSource::Named(b))) if a == b
        );
        hints.push(AmcHint {
            silent,
            source,
            is_identity,
        });
    }
    hints
}

/// Parse one `GraphOrDefault` operand (`DEFAULT | GRAPH? <iri>`) starting at
/// token index `k`. Returns the operand and the index of the first token after
/// it. The index is `None` when the operand's span could not be determined (an
/// unresolvable form, e.g. a prefixed name) — the caller then has no reliable
/// way to locate whatever follows.
fn amc_parse_operand(toks: &[AmcTok], k: usize) -> (AmcSource, Option<usize>) {
    match toks.get(k) {
        Some(AmcTok::Default) => (AmcSource::Default, Some(k + 1)),
        Some(AmcTok::Iri(s)) => (AmcSource::Named(s.clone()), Some(k + 1)),
        Some(AmcTok::Graph) => match toks.get(k + 1) {
            Some(AmcTok::Iri(s)) => (AmcSource::Named(s.clone()), Some(k + 2)),
            _ => (AmcSource::Unknown, None),
        },
        _ => (AmcSource::Unknown, None),
    }
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
