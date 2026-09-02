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
//!   ops execute; the flag — plus the source operand (resolved to an absolute
//!   IRI against the update's own `PREFIX`/`BASE` prologue) and whether the op
//!   is the identity case (`<g> TO <g>`) — is recovered per verb occurrence by a
//!   source-text pre-scan (see [`recover_amc_hints`]). The preflight reads only
//!   those hints, never the desugared ops.
//! * **The reserved namespace is closed to writes** (S4): every write form
//!   touching `https://horndb.io/graph/…` is refused. This is a
//!   permission-shaped error that `SILENT` does **not** suppress; it is
//!   checked before any silent/existence logic. Reads of reserved graphs stay
//!   allowed.
//!
//! **Atomicity** (§3.1.3): an update request is all-or-nothing. Two mechanisms
//! deliver that.
//!
//! *Preflight* rejects most bad requests before the first mutation, against the
//! pre-update store: a recovered-`SILENT` source existence sweep over the
//! `ADD`/`MOVE`/`COPY` hints, then `validate_op` per operation (reserved
//! namespace, D11 existence, `LOAD` routing/fetch). This is why `COPY <absent>
//! TO DEFAULT` — which desugars to a destructive `Drop{DEFAULT}` + a copy from
//! a missing source — never clears the default graph.
//!
//! *Rollback* ([`Journal`]) covers what preflight cannot see: a later operation
//! that fails only because an earlier one in the same request changed the store
//! (see [`validate_op`]). Operations still apply in request order against the
//! live store, so a later WHERE clause reads the earlier ones' writes; the
//! journal records each touched quad's pre-request visibility, and any failure
//! restores exactly that in one batch. One store batch per operation.

use crate::algebra::translate::{dataset_spec_from, translate_where};
use crate::algebra::{DatasetSpec, Term};
use crate::error::{Result, SparqlError};
use crate::exec::runtime::Runtime;
use crate::exec::RESERVED_GRAPH_PREFIX;
use crate::exec::{is_reserved_graph, AlgebraQuad, Bindings, FullBackend, GraphName};
use crate::parser::ParsedUpdate;
use crate::plan::planner;
use crate::{DefaultGraphMode, SparqlConfig};
use oxiri::Iri;
use spargebra::algebra::{GraphPattern, GraphTarget, QueryDataset};
use spargebra::term::{
    GraphName as SpgGraphName, GraphNamePattern, GroundQuadPattern, GroundTerm, GroundTermPattern,
    NamedNodePattern, NamedOrBlankNode, QuadPattern, Term as SpgTerm, TermPattern,
};
use spargebra::GraphUpdateOperation;
use std::collections::HashMap;

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

/// `ADD`/`MOVE`/`COPY` whose named source graph does not exist (D11) and whose
/// recovered `SILENT` flag was not set (SPARQL 1.1 §3.2.3/§3.2.4/§3.2.5).
fn amc_source_missing_error(iri: &str) -> SparqlError {
    SparqlError::Executor(format!(
        "source graph <{iri}> does not exist (use SILENT to make it a no-op)"
    ))
}

/// A non-silent `ADD`/`MOVE`/`COPY` whose source graph IRI the SILENT-recovery
/// scan cannot faithfully reproduce (it needs a `\uXXXX` UCHAR or a
/// `PN_LOCAL_ESC` backslash escape). We fail closed rather than resolve it wrong
/// and risk writing — or, for `COPY`/`MOVE`, wiping — the wrong graph.
fn amc_source_unresolvable_error() -> SparqlError {
    SparqlError::Executor(
        "ADD/MOVE/COPY source graph uses an IRI escape (\\uXXXX or a PN_LOCAL_ESC backslash) that \
         the SILENT-recovery scan cannot resolve; a non-silent op fails closed here to avoid \
         writing the wrong graph (use SILENT to make it a no-op)"
            .to_owned(),
    )
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
    let (ops, source, base_iri) = match u {
        ParsedUpdate::InsertData { inner, source }
        | ParsedUpdate::DeleteData { inner, source }
        | ParsedUpdate::DeleteInsert { inner, source }
        | ParsedUpdate::GraphManagement { inner, source } => {
            (&inner.operations, source.as_str(), inner.base_iri.as_ref())
        }
        ParsedUpdate::UnsupportedForm { .. } => {
            return Err(SparqlError::UnsupportedAlgebra(
                "update form not supported in Stage 1".into(),
            ));
        }
    };

    // Recover the `SILENT` flag, resolved source operand, and identity check of
    // every `ADD`/`MOVE`/`COPY` from the raw text — spargebra drops all three on
    // desugaring. Operands are resolved to absolute IRIs against the update's
    // own `PREFIX`/`BASE` prologue (see [`recover_amc_hints`]).
    let amc_hints = recover_amc_hints(source, base_iri);

    // Atomicity (§3.1.3), part 1: a non-silent `ADD`/`MOVE`/`COPY` whose source
    // is absent (or unresolvable) is an error (SPARQL 1.1 §3.2.3/§3.2.4/§3.2.5),
    // caught before any mutation. The identity case (`<g> TO <g>`) desugars to
    // zero ops and is a no-op even when `<g>` is absent, so it is excluded. A
    // `DEFAULT` source always exists. A source the scan could not resolve (an
    // escaped IRI — see [`AmcSource::Unknown`]) **fails closed**: a non-silent op
    // errors rather than fall through to a silent no-op that could wipe a
    // destination. This reads only the recovered hints — never the desugared ops
    // — so it cannot collide with a user-written `DeleteInsert`.
    for h in &amc_hints {
        if h.silent || h.is_identity {
            continue;
        }
        match &h.source {
            AmcSource::Named(g) => {
                if !store.graph_exists(g) {
                    return Err(amc_source_missing_error(g));
                }
            }
            AmcSource::Unknown => return Err(amc_source_unresolvable_error()),
            AmcSource::Default => {}
        }
    }

    // Atomicity part 2: preflight every op for the remaining errors it would
    // hit at apply time — reserved namespace, D11 existence (against the
    // pre-update store), `LOAD` routing/fetch. See the module doc.
    for op in ops {
        validate_op(op, cfg, store)?;
    }

    // Atomicity part 3: ops apply in request order against the live store (so
    // each one reads the previous ones' writes), journalled so a failure undoes
    // the whole request. A single-op request is already one atomic batch, so its
    // journal is disabled and costs nothing.
    let mut journal = Journal::new(ops.len() > 1);
    for op in ops {
        if let Err(e) = apply_op(store, cfg, op, &mut journal) {
            journal.rollback(store);
            return Err(e);
        }
    }
    Ok(())
}

/// Apply one operation, recording its pre-touch state in `journal` so
/// [`apply_update_with`] can undo it if a later operation fails.
fn apply_op<B: FullBackend>(
    store: &mut B,
    cfg: &SparqlConfig,
    op: &GraphUpdateOperation,
    journal: &mut Journal,
) -> Result<()> {
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
            journal.record(store, &adds);
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
            journal.record(store, &dels);
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
            apply_delete_insert(store, cfg, delete, insert, using.as_ref(), pattern, journal)?;
        }
        GraphUpdateOperation::Clear { silent, graph } => {
            apply_clear_drop(store, "CLEAR", *silent, graph, journal)?;
        }
        GraphUpdateOperation::Drop { silent, graph } => {
            apply_clear_drop(store, "DROP", *silent, graph, journal)?;
        }
        GraphUpdateOperation::Create { silent, graph } => {
            apply_create(store, *silent, graph.as_str())?;
        }
        GraphUpdateOperation::Load {
            silent,
            source,
            destination,
        } => {
            apply_load(store, *silent, source, destination, journal)?;
        }
    }
    Ok(())
}

// ── Rollback journal (§3.1.3) ────────────────────────────────────────────────

/// Undo log for a multi-operation update request.
///
/// For every quad an operation is about to touch it records one bit — was that
/// quad visible before this request first touched it? — read from the store
/// just before the touching batch. [`Self::rollback`] then restores exactly
/// that state: re-insert the quads that were there, retract the ones that were
/// not. Recording the *first* reading per quad is what makes the undo
/// order-independent, so ops that touch the same quad twice still roll back to
/// the pre-request state.
///
/// This is a rollback, not a working copy: ops mutate the live store in order,
/// which is what keeps read-your-own-writes inside a request. An overlay would
/// avoid the recording reads, but the read seam ([`crate::exec::Executor`])
/// evaluates a whole BGP join at once, so a pending delta cannot be layered
/// over its results.
///
/// Disabled for a single-operation request: one op is already one atomic
/// `apply_quads` batch, so recording would be pure cost.
struct Journal {
    enabled: bool,
    /// Touched quad → was it visible before this request first touched it.
    before: HashMap<AlgebraQuad, bool>,
}

impl Journal {
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            before: HashMap::new(),
        }
    }

    /// Record the pre-touch visibility of every quad in `quads`. Call it
    /// immediately before the batch that touches them; a quad already recorded
    /// keeps its first reading.
    fn record<B: FullBackend>(&mut self, store: &B, quads: &[AlgebraQuad]) {
        if !self.enabled {
            return;
        }
        for q in quads {
            if !self.before.contains_key(q) {
                let live = store.quad_exists(q);
                self.before.insert(q.clone(), live);
            }
        }
    }

    /// Record every quad a `CLEAR`/`DROP` sweep of one graph is about to
    /// retract — all of them visible by construction, so no point read is
    /// needed. `graph` names a single graph (`None` = the default graph).
    fn record_cleared<B: FullBackend>(&mut self, store: &B, graph: &GraphName) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let target = match graph {
            None => GraphTarget::DefaultGraph,
            Some(iri) => GraphTarget::NamedNode(named_node(iri)),
        };
        for (s, p, o) in store.scan_graph_quads(&target)? {
            self.before.entry((graph.clone(), s, p, o)).or_insert(true);
        }
        Ok(())
    }

    /// Restore the pre-request state of every recorded quad in one batch.
    /// Best effort: the caller is already returning the operation's own error,
    /// which is the one the client needs to see.
    fn rollback<B: FullBackend>(self, store: &mut B) {
        if self.before.is_empty() {
            return;
        }
        let mut dels: Vec<AlgebraQuad> = Vec::new();
        let mut adds: Vec<AlgebraQuad> = Vec::new();
        for (q, was_live) in self.before {
            if was_live {
                adds.push(q);
            } else {
                dels.push(q);
            }
        }
        let _ = store.apply_quads(dels, adds);
    }
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
/// errors, a missing/existing graph always errors — so what preflight misses is
/// only the *timing*: a later op whose D11 existence an earlier op flipped, or
/// a variable-bound reserved write an earlier op's mutation preceded, fails at
/// apply time rather than up front. [`Journal`] covers that case — the failure
/// rolls the whole request back, so the request still mutates nothing.
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
        } => validate_load(store, *silent, source, destination),
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
    journal: &mut Journal,
) -> Result<()> {
    match graph {
        // Sweep just the default graph. Never an error (the default graph
        // always exists), even when it is empty.
        GraphTarget::DefaultGraph => {
            journal.record_cleared(store, &None)?;
            store.clear_graph(&GraphTarget::DefaultGraph)?;
            Ok(())
        }
        // `ALL` = the default graph + every *non-reserved* named graph, quad by
        // quad. Reserved graphs are left untouched (SPEC-30 owns the
        // store-level reset), so this cannot use `clear_graph(AllGraphs)`.
        GraphTarget::AllGraphs => {
            journal.record_cleared(store, &None)?;
            store.clear_graph(&GraphTarget::DefaultGraph)?;
            clear_named_graphs(store, journal)?;
            Ok(())
        }
        // `NAMED` = every non-reserved named graph (default graph left alone).
        GraphTarget::NamedGraphs => clear_named_graphs(store, journal),
        GraphTarget::NamedNode(n) => {
            let iri = n.as_str();
            reserved_iri_write_check(iri)?;
            if store.graph_exists(iri) {
                journal.record_cleared(store, &Some(iri.to_owned()))?;
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
fn clear_named_graphs<B: FullBackend>(store: &mut B, journal: &mut Journal) -> Result<()> {
    for g in store.graphs() {
        if is_reserved_graph(&g) {
            continue;
        }
        journal.record_cleared(store, &Some(g.clone()))?;
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
    journal: &mut Journal,
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
    journal.record(store, &dels);
    journal.record(store, &adds);
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
fn validate_load<B: FullBackend>(
    store: &B,
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
    fetch_and_parse(store.next_bnode_doc_tag(), source.as_str()).map(|_| ())
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
    journal: &mut Journal,
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
    match fetch_and_parse(store.next_bnode_doc_tag(), source.as_str()) {
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
            journal.record(store, &adds);
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
/// Blank-node labels are document-scoped, so every one parsed here is renamed
/// with `tag` (HDB-113) before it becomes an algebra term.
fn fetch_and_parse(tag: u64, source: &str) -> Result<Vec<AlgebraQuad>> {
    let raw = file_iri_to_path(source)?;
    // A file IRI percent-encodes reserved characters (e.g. a space as `%20`);
    // decode to the real filesystem path before reading.
    let path = percent_decode(&raw);

    let bytes = std::fs::read(&path)
        .map_err(|e| SparqlError::Executor(format!("LOAD reading {path}: {e}")))?;
    parse_rdf_bytes(tag, &bytes, source_extension(&path).as_deref(), source)
}

/// Parse an RDF document's `bytes` by `extension` (`"nt"`, `"nq"`, `"trig"`,
/// or anything else — including `None` — which defaults to Turtle), returning
/// its quads as algebra [`Term`]s tagged by graph. Triples formats
/// (`.nt`/`.ttl`/default) tag every quad with the default graph (`None`);
/// dataset formats (`.nq`/`.trig`) tag each quad with its own graph name.
/// `base` resolves relative IRIs in Turtle/TriG (N-Triples/N-Quads require
/// absolute IRIs, so it is unused for those) and labels parse errors.
///
/// The one parser call site for both `LOAD` ([`fetch_and_parse`]) and the
/// `serve --data` startup loader (`crates/sparql/src/bin/serve.rs`), so the
/// two never drift on format handling.
///
/// `tag` scopes this document's blank-node labels (HDB-113): the label `_:b1`
/// is document-scoped in every RDF syntax, so both call sites pass a fresh tag
/// per document (`exec::Store::next_bnode_doc_tag`) to keep `_:b1` in two
/// files from landing on the same node.
pub fn parse_rdf_bytes(
    tag: u64,
    bytes: &[u8],
    extension: Option<&str>,
    base: &str,
) -> Result<Vec<AlgebraQuad>> {
    use oxttl::{NQuadsParser, NTriplesParser, TriGParser, TurtleParser};

    let map_err =
        |e: oxttl::TurtleSyntaxError| SparqlError::Executor(format!("parsing {base}: {e}"));

    let mut out: Vec<AlgebraQuad> = Vec::new();
    match extension {
        // N-Triples/N-Quads require absolute IRIs (no base).
        Some("nt") => {
            for t in NTriplesParser::new().for_slice(bytes) {
                let t = t.map_err(map_err)?;
                let (s, p, o) = oxrdf_triple_to_terms(tag, &t.subject, &t.predicate, &t.object);
                out.push((None, s, p, o));
            }
        }
        Some("nq") => {
            for q in NQuadsParser::new().for_slice(bytes) {
                let q = q.map_err(map_err)?;
                let (s, p, o) = oxrdf_triple_to_terms(tag, &q.subject, &q.predicate, &q.object);
                out.push((oxrdf_graph_to_name(tag, &q.graph_name), s, p, o));
            }
        }
        // Turtle/TriG may carry relative IRIs resolved against the document IRI.
        Some("trig") => {
            let parser = with_base(TriGParser::new(), base)?;
            for q in parser.for_slice(bytes) {
                let q = q.map_err(map_err)?;
                let (s, p, o) = oxrdf_triple_to_terms(tag, &q.subject, &q.predicate, &q.object);
                out.push((oxrdf_graph_to_name(tag, &q.graph_name), s, p, o));
            }
        }
        // `.ttl` and anything else default to Turtle (a triples format).
        _ => {
            let parser = with_base(TurtleParser::new(), base)?;
            for t in parser.for_slice(bytes) {
                let t = t.map_err(map_err)?;
                let (s, p, o) = oxrdf_triple_to_terms(tag, &t.subject, &t.predicate, &t.object);
                out.push((None, s, p, o));
            }
        }
    }
    Ok(out)
}

/// Lower an `oxrdf` graph name to a store [`GraphName`]. A blank-node graph
/// name is renamed with `tag` first (HDB-113), same as every other blank node
/// this `LOAD` parses.
fn oxrdf_graph_to_name(tag: u64, g: &oxrdf::GraphName) -> GraphName {
    match g {
        oxrdf::GraphName::DefaultGraph => None,
        oxrdf::GraphName::NamedNode(n) => Some(n.as_str().to_owned()),
        oxrdf::GraphName::BlankNode(b) => Some(horndb_storage::loader::scope_blank_node_label(
            tag,
            b.as_str(),
        )),
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
    tag: u64,
    subject: &oxrdf::NamedOrBlankNode,
    predicate: &oxrdf::NamedNode,
    object: &oxrdf::Term,
) -> (Term, Term, Term) {
    (
        oxrdf_subject_to_term(tag, subject),
        Term::Iri(predicate.as_str().to_owned()),
        oxrdf_term_to_term(tag, object),
    )
}

/// Lower an `oxrdf` subject (named node or blank node) to an algebra [`Term`].
/// A blank-node label is renamed with `tag` (HDB-113) so it can't collide
/// with the same label from a different `LOAD` or bulk load into the same
/// store — matching the bulk loaders' `crate::loader::scope_blank_node`.
fn oxrdf_subject_to_term(tag: u64, s: &oxrdf::NamedOrBlankNode) -> Term {
    match s {
        oxrdf::NamedOrBlankNode::NamedNode(n) => Term::Iri(n.as_str().to_owned()),
        oxrdf::NamedOrBlankNode::BlankNode(b) => Term::BlankNode(
            horndb_storage::loader::scope_blank_node_label(tag, b.as_str()),
        ),
    }
}

/// Lower an `oxrdf` object term to an algebra [`Term`]. Literals keep their
/// N-Triples lexical form; a blank node is renamed with `tag`, same as
/// [`oxrdf_subject_to_term`].
fn oxrdf_term_to_term(tag: u64, t: &oxrdf::Term) -> Term {
    match t {
        oxrdf::Term::NamedNode(n) => Term::Iri(n.as_str().to_owned()),
        oxrdf::Term::BlankNode(b) => Term::BlankNode(
            horndb_storage::loader::scope_blank_node_label(tag, b.as_str()),
        ),
        oxrdf::Term::Literal(l) => Term::Literal(l.to_string()),
        // RDF 1.2 triple-term objects: best-effort lexical form (the same
        // lowering the loader applies).
        oxrdf::Term::Triple(tr) => Term::Literal(tr.to_string()),
    }
}

// ── ADD / MOVE / COPY: SILENT recovery ───────────────────────────────────────

/// The source operand of an `ADD`/`MOVE`/`COPY`, recovered and fully resolved
/// from the raw update text (its own `PREFIX`/`BASE` prologue).
#[derive(Debug, Clone, PartialEq, Eq)]
enum AmcSource {
    /// `DEFAULT` — the default graph always exists, never a missing-source error.
    Default,
    /// `[GRAPH] iri` — the named source graph, as an absolute IRI (a prefixed
    /// name or a base-relative `<…>` is already expanded).
    Named(String),
    /// The operand could not be faithfully resolved from the raw text. This
    /// arises only for a graph IRI that needs a `\uXXXX` (UCHAR) or a
    /// `PN_LOCAL_ESC` backslash escape: the raw scan cannot reproduce the escape,
    /// so it refuses to guess a (possibly wrong) IRI. Ordinary operands always
    /// resolve to `Named`/`Default`. A non-silent `ADD`/`MOVE`/`COPY` with an
    /// `Unknown` **source** fails closed — it errors before any mutation (see the
    /// preflight in [`apply_update_with`]) rather than risk writing or wiping the
    /// wrong graph. A `SILENT` op is still a no-op. Full unescape-parity
    /// resolution is a possible future improvement.
    Unknown,
}

/// One recovered `ADD`/`MOVE`/`COPY` occurrence: its `SILENT` flag, resolved
/// source operand, and whether it is the W3C identity case (`source ==
/// destination`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct AmcHint {
    silent: bool,
    source: AmcSource,
    /// `true` when the resolved source and destination operands are equal —
    /// spargebra's own identity check (`from == to`), which always desugars to
    /// zero operations and is a no-op regardless of whether the graph exists
    /// (SPARQL 1.1 §3.2.3/§3.2.4/§3.2.5). Because operands are resolved to
    /// absolute IRIs first, `ex:g TO ex:g` is recognised as identity just like
    /// `<http://ex/g> TO <http://ex/g>`.
    is_identity: bool,
}

/// Recover the `SILENT` flag, resolved source operand, and identity check
/// (`source == destination`) of every `ADD`/`MOVE`/`COPY` in the raw update
/// text, in source order.
///
/// **Why this exists.** spargebra 0.4.6 desugars `ADD`/`MOVE`/`COPY` into
/// `Drop` + `DeleteInsert` pairs and **discards the `SILENT` flag**. That was
/// harmless while named graphs were unrepresentable; now a `SILENT COPY
/// <missing> TO <g>` must be a no-op and a non-silent one an error (SPEC-28
/// S4), so the flag matters. Re-scan the source text — a small hand-written
/// tokenizer (no regex) that skips comments, IRIs, and string literals.
///
/// **How operands resolve.** The scan tracks the update's own prologue: it reads
/// each `BASE <iri>` and `PREFIX pfx: <iri>` as it goes and resolves every
/// operand to an absolute IRI against them — a prefixed name via the prefix map,
/// a relative `<…>` against the current base (`base_iri` seeds the base with any
/// externally supplied one, e.g. an HTTP request base). So both the source IRI
/// **and** `is_identity` come purely from text, for every operand form. The
/// preflight then checks each non-silent, non-identity `Named` source directly
/// (see [`apply_update_with`]) — no inspection of the desugared ops, so nothing
/// can collide with a user-written `DeleteInsert` and a prefixed identity op is
/// recognised like any other.
///
/// **Escapes fail closed.** An operand IRI that needs a `\uXXXX` (UCHAR) or a
/// `PN_LOCAL_ESC` backslash escape can't be reproduced by the raw scan, so the
/// tokenizer marks it unresolvable ([`AmcTok::Escaped`]) and it becomes
/// [`AmcSource::Unknown`] rather than a wrong (truncated/partial) `Named`. The
/// preflight then errors on a non-silent op with such a source instead of
/// letting a wrong-graph or destructive write slip through.
///
/// Upstream: `# TODO` — file an issue asking oxigraph/spargebra to preserve the
/// `SILENT` flag on `ADD`/`MOVE`/`COPY` (no issue filed yet; do not invent a
/// number). Delete this whole machinery the day that ships.
fn recover_amc_hints(src: &str, base_iri: Option<&Iri<String>>) -> Vec<AmcHint> {
    let toks = amc_tokenize(src);
    let mut prefixes: HashMap<String, String> = HashMap::new();
    let mut base: Option<Iri<String>> = base_iri.cloned();
    let mut hints = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        match &toks[i] {
            // `BASE <iri>` — resolve against the current base, then adopt it.
            AmcTok::Base => {
                if let Some(AmcTok::Iri(raw)) = toks.get(i + 1) {
                    if let Some(resolved) = resolve_iri(raw, base.as_ref()) {
                        if let Ok(b) = Iri::parse(resolved) {
                            base = Some(b);
                        }
                    }
                    i += 2;
                } else {
                    i += 1;
                }
            }
            // `PREFIX pfx: <iri>` — record the (base-resolved) namespace.
            AmcTok::Prefix => {
                if let (Some(AmcTok::PName(ns)), Some(AmcTok::Iri(raw))) =
                    (toks.get(i + 1), toks.get(i + 2))
                {
                    if let Some(resolved) = resolve_iri(raw, base.as_ref()) {
                        let prefix = ns.split(':').next().unwrap_or("").to_owned();
                        prefixes.insert(prefix, resolved);
                    }
                    i += 3;
                } else {
                    i += 1;
                }
            }
            // A verb: `SILENT? GraphOrDefault TO GraphOrDefault`. Whitespace and
            // comments are not emitted, so the operands are consecutive tokens.
            AmcTok::Amc => {
                let mut j = i + 1;
                let silent = matches!(toks.get(j), Some(AmcTok::Silent));
                if silent {
                    j += 1;
                }
                let (source, after_source) = amc_parse_operand(&toks, j, &prefixes, base.as_ref());
                let destination = after_source
                    .map(|k| amc_parse_operand(&toks, k + 1, &prefixes, base.as_ref()).0);
                let is_identity = match (&source, &destination) {
                    (AmcSource::Default, Some(AmcSource::Default)) => true,
                    (AmcSource::Named(a), Some(AmcSource::Named(b))) => a == b,
                    _ => false,
                };
                hints.push(AmcHint {
                    silent,
                    source,
                    is_identity,
                });
                i += 1;
            }
            _ => i += 1,
        }
    }
    hints
}

/// Parse one `GraphOrDefault` operand (`DEFAULT | GRAPH? iri`) starting at token
/// index `k`, resolving it to an absolute IRI. Returns the operand and the index
/// of the first token after it (`None` when the operand form is unrecognisable,
/// so the caller can't locate what follows).
fn amc_parse_operand(
    toks: &[AmcTok],
    k: usize,
    prefixes: &HashMap<String, String>,
    base: Option<&Iri<String>>,
) -> (AmcSource, Option<usize>) {
    match toks.get(k) {
        Some(AmcTok::Default) => (AmcSource::Default, Some(k + 1)),
        Some(AmcTok::Iri(raw)) => (resolve_named(raw, base), Some(k + 1)),
        Some(AmcTok::PName(name)) => (expand_pname(name, prefixes), Some(k + 1)),
        // An operand carrying an escape the scan can't reproduce — the span is
        // still one token, so advance past it, but it is unresolvable.
        Some(AmcTok::Escaped) => (AmcSource::Unknown, Some(k + 1)),
        Some(AmcTok::Graph) => match toks.get(k + 1) {
            Some(AmcTok::Iri(raw)) => (resolve_named(raw, base), Some(k + 2)),
            Some(AmcTok::PName(name)) => (expand_pname(name, prefixes), Some(k + 2)),
            Some(AmcTok::Escaped) => (AmcSource::Unknown, Some(k + 2)),
            _ => (AmcSource::Unknown, None),
        },
        _ => (AmcSource::Unknown, None),
    }
}

/// Resolve an `<…>` operand (absolute or base-relative) to `Named(absolute)`.
fn resolve_named(raw: &str, base: Option<&Iri<String>>) -> AmcSource {
    match resolve_iri(raw, base) {
        Some(iri) => AmcSource::Named(iri),
        None => AmcSource::Unknown,
    }
}

/// Expand a prefixed name `pfx:local` (split on the first `:`) against the
/// prefix map to `Named(namespace + local)`. An undeclared prefix — which
/// spargebra would have rejected — yields `Unknown`.
fn expand_pname(name: &str, prefixes: &HashMap<String, String>) -> AmcSource {
    match name.split_once(':') {
        Some((prefix, local)) => match prefixes.get(prefix) {
            Some(ns) => AmcSource::Named(format!("{ns}{local}")),
            None => AmcSource::Unknown,
        },
        None => AmcSource::Unknown,
    }
}

/// Resolve an IRIREF string to an absolute IRI: against `base` (RFC 3986) when
/// one is in scope, else it must already be absolute. `None` if it is neither.
fn resolve_iri(raw: &str, base: Option<&Iri<String>>) -> Option<String> {
    match base {
        Some(b) => b.resolve(raw).ok().map(|r| r.into_inner()),
        None => Iri::parse(raw.to_owned()).ok().map(|i| i.into_inner()),
    }
}

/// The token kinds the SILENT-recovery scan needs. Everything not a tracked
/// keyword, an IRI, or a prefixed name is [`AmcTok::Other`] — kept (not dropped)
/// so token adjacency mirrors the grammar and a non-operand word never slides
/// into an operand slot.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AmcTok {
    Amc,
    Silent,
    Default,
    Graph,
    Prefix,
    Base,
    /// The raw inner text of a `<…>` IRIREF (may be relative). Never contains a
    /// backslash — an IRIREF with a `\uXXXX` escape is emitted as [`Self::Escaped`].
    Iri(String),
    /// A prefixed name / namespace, raw (`ex:local`, `:local`, `ex:`). Never
    /// carries a `PN_LOCAL_ESC` backslash — such a name is [`Self::Escaped`].
    PName(String),
    /// An operand IRI/prefixed-name that carries a `\` escape (`\uXXXX` UCHAR or
    /// `PN_LOCAL_ESC`) the raw scan cannot reproduce. Held as one token so the
    /// operand span is preserved, but it resolves to [`AmcSource::Unknown`] so a
    /// non-silent op fails closed instead of resolving a wrong IRI.
    Escaped,
    Other,
}

/// A byte that can start a keyword or a prefixed name: ASCII alphanumeric, `_`,
/// or `:` (an empty-prefix name like `:g`).
fn is_pname_start(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b':'
}

/// A byte that continues a keyword or prefixed name. Adds `-`, `.`, `:`, and `%`
/// to the start set so a hyphenated/dotted/prefixed/percent-encoded name is one
/// token (a trailing `.` is trimmed by the caller).
fn is_pname_cont(b: u8) -> bool {
    is_pname_start(b) || b == b'-' || b == b'.' || b == b'%'
}

/// Tokenize `src` for [`recover_amc_hints`]. Skips ASCII whitespace, `#`
/// comments (to end of line), and string literals (`'…'`, `"…"`, and their
/// triple-quoted forms, honouring `\` escapes). Emits one token per
/// keyword / IRI / prefixed-name / other-run.
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
                // Treat every `<` as an IRIREF open: no `>` or whitespace
                // inside; if unterminated, stop at end (best-effort — the parser
                // already accepted the update). A `<` used as a comparison
                // operator (e.g. `FILTER(?x < 5)`) tokenizes to a short/empty
                // `Iri`, but that is harmless: recovery only reads the fixed
                // operand window right after each verb (`SILENT? src TO dst`),
                // and that grammar has no comparison, so such a token never
                // reaches an operand slot.
                let start = i + 1;
                let mut j = start;
                while j < n && b[j] != b'>' && !b[j].is_ascii_whitespace() {
                    j += 1;
                }
                let inner = &b[start..j];
                // A `\` in an IRIREF is a `\uXXXX`/`\UXXXXXXXX` UCHAR escape the
                // raw scan can't expand — mark the operand unresolvable, don't
                // pass the literal backslash on as an IRI.
                out.push(if inner.contains(&b'\\') {
                    AmcTok::Escaped
                } else {
                    AmcTok::Iri(String::from_utf8_lossy(inner).into_owned())
                });
                i = if j < n && b[j] == b'>' { j + 1 } else { j };
            }
            b'"' | b'\'' => {
                i = amc_skip_string(b, i);
                out.push(AmcTok::Other);
            }
            _ if is_pname_start(c) => {
                let start = i;
                let mut j = i;
                while j < n && is_pname_cont(b[j]) {
                    j += 1;
                }
                // PN_PREFIX / PN_LOCAL cannot end in `.`; drop trailing dots
                // (e.g. a statement-terminating `.` glued to a name).
                let mut end = j;
                while end > start && b[end - 1] == b'.' {
                    end -= 1;
                }
                let word = &src[start..end];
                // A variable (`?g`/`$g`) is not a keyword or a name.
                let prev = if start > 0 { b[start - 1] } else { 0 };
                let tok = if prev == b'?' || prev == b'$' {
                    AmcTok::Other
                } else if j < n && b[j] == b'\\' {
                    // The name is interrupted by a `PN_LOCAL_ESC` backslash the
                    // raw scan can't reproduce — mark it unresolvable rather than
                    // emit the truncated prefix (`ex:a` for `ex:a\,b`).
                    AmcTok::Escaped
                } else if word.as_bytes().contains(&b':') {
                    AmcTok::PName(word.to_owned())
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
                } else if word.eq_ignore_ascii_case("PREFIX") {
                    AmcTok::Prefix
                } else if word.eq_ignore_ascii_case("BASE") {
                    AmcTok::Base
                } else {
                    AmcTok::Other
                };
                out.push(tok);
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
