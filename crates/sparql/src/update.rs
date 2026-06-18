//! SPARQL Update — `INSERT DATA` / `DELETE DATA`, pattern-based
//! `INSERT`/`DELETE … WHERE`, and the graph-management verbs
//! `LOAD`/`CLEAR`/`DROP`/`CREATE` plus multi-operation updates (SPEC-07 F5,
//! #52).
//!
//! Under the Stage-1 default-graph-only model the graph-management verbs map
//! onto the single merged default graph and honour the `SILENT` modifier:
//! `CLEAR`/`DROP DEFAULT`/`ALL` empty the store; `LOAD` fetches a `file:`
//! source and merges it into the default graph; and a `CLEAR`/`DROP`/`CREATE`/
//! `LOAD` that would touch an unrepresentable named graph is an error unless
//! `SILENT` (then a no-op).
//!
//! `ADD`/`MOVE`/`COPY` are not distinct spargebra variants: the parser rewrites
//! them (per the W3C spec) into `Drop` + `DeleteInsert` sequences, with the
//! same-graph identity case (`… <g> TO <g>`) collapsing to zero operations (a
//! valid no-op). A named-graph operand surfaces as a named `GRAPH` pattern in
//! the desugared `DeleteInsert` and is rejected by `apply_delete_insert`'s
//! existing named-graph guards. **The `SILENT` flag is dropped by spargebra's
//! desugaring**, so a named-operand `ADD`/`MOVE`/`COPY` errors even with
//! `SILENT` — preserving it would require re-parsing the verb, which is out of
//! scope while named graphs are unrepresentable (the no-op and the error are
//! observationally identical to a default-graph-only store either way: no data
//! moves). True named-graph scoping and remote (`http(s):`) `LOAD` stay
//! deferred — see `INTEGRATION-NOTES.md`.

use crate::algebra::translate::translate_where;
use crate::algebra::Term;
use crate::error::{Result, SparqlError};
use crate::exec::runtime::Runtime;
use crate::exec::{Bindings, FullBackend};
use crate::parser::ParsedUpdate;
use crate::plan::planner;
use crate::SparqlConfig;
use spargebra::term::{
    GraphNamePattern, GroundQuadPattern, GroundTerm, GroundTermPattern, NamedNodePattern,
    NamedOrBlankNode, QuadPattern, Term as SpgTerm, TermPattern,
};

/// Lexical form for an RDF 1.2 triple term embedded in an update. The
/// Stage-1 store carries `Term::Literal(String)` slots only, so there is
/// no in-store representation for a triple term in this crate.
fn triple_term_unsupported() -> SparqlError {
    SparqlError::UnsupportedAlgebra("RDF 1.2 triple term in update (SPARQL 1.1 mode)".into())
}

fn named_graph_unsupported() -> SparqlError {
    SparqlError::UnsupportedAlgebra(
        "named-graph target in update (Stage-1 default graph only)".into(),
    )
}

fn using_named_graph_unsupported() -> SparqlError {
    SparqlError::UnsupportedAlgebra(
        "USING named-graph dataset in update (Stage-1 default graph only)".into(),
    )
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
    let ops = match u {
        ParsedUpdate::InsertData { inner }
        | ParsedUpdate::DeleteData { inner }
        | ParsedUpdate::DeleteInsert { inner }
        | ParsedUpdate::GraphManagement { inner } => &inner.operations,
        ParsedUpdate::UnsupportedForm { .. } => {
            return Err(SparqlError::UnsupportedAlgebra(
                "update form not supported in Stage 1".into(),
            ));
        }
    };

    // SPARQL Update is atomic: a failed update must not partially apply
    // (§3.1.3). spargebra desugars `COPY`/`MOVE <named> TO DEFAULT` into a
    // destructive `Drop{DEFAULT}` *followed by* a `DeleteInsert` that reads the
    // unrepresentable named graph — so applying op-by-op would clear the
    // default graph and only then reject, losing data on a failing update.
    // Preflight every operation for the rejections we can detect without
    // mutating; only mutate once the whole sequence is known-applyable. (The
    // remaining failure mode — a `LOAD` whose fetch/parse fails — is checked in
    // its own pass below, before any op mutates.)
    for op in ops {
        validate_op(op)?;
    }

    for op in ops {
        match op {
            GraphUpdateOperation::InsertData { data } => {
                for q in data {
                    let s = subject_to_term(&q.subject);
                    let p = Term::Iri(q.predicate.as_str().to_owned());
                    let o = object_to_term(&q.object)?;
                    store.insert_triple(s, p, o);
                }
            }
            GraphUpdateOperation::DeleteData { data } => {
                for q in data {
                    let s = Term::Iri(q.subject.as_str().to_owned());
                    let p = Term::Iri(q.predicate.as_str().to_owned());
                    let o = ground_term_to_term(&q.object)?;
                    store.delete_triple(&s, &p, &o);
                }
            }
            GraphUpdateOperation::DeleteInsert {
                delete,
                insert,
                using,
                pattern,
            } => {
                apply_delete_insert(store, cfg, delete, insert, using.as_ref(), pattern)?;
            }
            GraphUpdateOperation::Clear { silent, graph } => {
                apply_clear_drop(store, *silent, graph)?;
            }
            GraphUpdateOperation::Drop { silent, graph } => {
                apply_clear_drop(store, *silent, graph)?;
            }
            GraphUpdateOperation::Create { silent, .. } => {
                // No named-graph store exists (Stage-1 default-graph only),
                // so a named graph cannot be created. SPARQL 1.1 §3.1.4:
                // CREATE of an unrepresentable target is an error unless
                // SILENT, in which case it is a no-op.
                if !silent {
                    return Err(create_named_graph_unsupported());
                }
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

/// Error for a graph-management verb that targets a named graph, which the
/// Stage-1 default-graph-only store cannot represent.
fn create_named_graph_unsupported() -> SparqlError {
    SparqlError::UnsupportedAlgebra("CREATE of a named graph (Stage-1 default graph only)".into())
}

/// Apply `CLEAR`/`DROP` against the single default graph. The two verbs are
/// semantically identical here: there are no named graphs to remove, so a
/// `DEFAULT`/`ALL` target clears the store and any named/`NAMED` target refers
/// to a graph that does not exist (SPARQL 1.1 §3.2.{1,2}: an error unless
/// `SILENT`, otherwise a no-op).
fn apply_clear_drop<B: FullBackend>(
    store: &mut B,
    silent: bool,
    graph: &spargebra::algebra::GraphTarget,
) -> Result<()> {
    use spargebra::algebra::GraphTarget;
    match graph {
        GraphTarget::DefaultGraph | GraphTarget::AllGraphs => {
            store.clear_all();
            Ok(())
        }
        // No named graphs exist in the Stage-1 store: a named target (or the
        // `NAMED` keyword) addresses nothing.
        GraphTarget::NamedNode(_) | GraphTarget::NamedGraphs => {
            if silent {
                Ok(())
            } else {
                Err(named_graph_unsupported())
            }
        }
    }
}

/// Apply `LOAD <source> [INTO GRAPH <destination>]`. The document is fetched,
/// parsed, and its triples inserted into the default graph. Stage-1 boundaries:
/// only `file:` sources are fetched (no HTTP client dependency), and a named
/// `destination` cannot be targeted. A boundary violation is an error unless
/// `SILENT`, in which case the whole load is skipped (SPARQL 1.1 §3.1.5).
fn apply_load<B: FullBackend>(
    store: &mut B,
    silent: bool,
    source: &spargebra::term::NamedNode,
    destination: &spargebra::term::GraphName,
) -> Result<()> {
    use spargebra::term::GraphName;
    // A named destination graph cannot be represented (default-graph only).
    if let GraphName::NamedNode(_) = destination {
        return if silent {
            Ok(())
        } else {
            Err(SparqlError::UnsupportedAlgebra(
                "LOAD INTO a named graph (Stage-1 default graph only)".into(),
            ))
        };
    }
    match fetch_and_parse(source.as_str()) {
        Ok(triples) => {
            for (s, p, o) in triples {
                store.insert_triple(s, p, o);
            }
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

/// Fetch and parse an RDF document named by `source`, returning its triples as
/// algebra [`Term`]s. Stage-1 supports `file:` IRIs only; remote (`http(s):`)
/// sources are rejected (the workspace carries no HTTP client). The
/// serialization is chosen from the path extension, defaulting to Turtle. All
/// graph names in a quad source are merged into the default graph (Stage-1 has
/// no named-graph store), matching the N-Quads bulk loader.
fn fetch_and_parse(source: &str) -> Result<Vec<(Term, Term, Term)>> {
    use oxttl::{NQuadsParser, NTriplesParser, TriGParser, TurtleParser};

    let raw = match source
        .strip_prefix("file://")
        .or_else(|| source.strip_prefix("file:"))
    {
        // `file:///abs/path` → `/abs/path`; the common absolute form.
        Some(p) => p,
        None => {
            return Err(SparqlError::UnsupportedAlgebra(format!(
                "LOAD of a non-file source (Stage-1 fetches file: IRIs only): {source}"
            )));
        }
    };
    // A file IRI percent-encodes reserved characters (e.g. a space as `%20`);
    // decode to the real filesystem path before reading.
    let path = percent_decode(raw);

    let bytes = std::fs::read(&path)
        .map_err(|e| SparqlError::Executor(format!("LOAD reading {path}: {e}")))?;
    let map_err =
        |e: oxttl::TurtleSyntaxError| SparqlError::Executor(format!("LOAD parsing {path}: {e}"));

    let mut out = Vec::new();
    match path
        .rsplit('.')
        .next()
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("nt") => {
            for t in NTriplesParser::new().for_slice(&bytes) {
                let t = t.map_err(map_err)?;
                out.push(oxrdf_triple_to_terms(&t.subject, &t.predicate, &t.object));
            }
        }
        Some("nq") => {
            for q in NQuadsParser::new().for_slice(&bytes) {
                let q = q.map_err(map_err)?;
                out.push(oxrdf_triple_to_terms(&q.subject, &q.predicate, &q.object));
            }
        }
        Some("trig") => {
            for q in TriGParser::new().for_slice(&bytes) {
                let q = q.map_err(map_err)?;
                out.push(oxrdf_triple_to_terms(&q.subject, &q.predicate, &q.object));
            }
        }
        // `.ttl` and anything else default to Turtle.
        _ => {
            for t in TurtleParser::new().for_slice(&bytes) {
                let t = t.map_err(map_err)?;
                out.push(oxrdf_triple_to_terms(&t.subject, &t.predicate, &t.object));
            }
        }
    }
    Ok(out)
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
///
/// Blank-node labels are carried through verbatim (`b.as_str()`). This shares
/// the Stage-1 store's known blank-node approximation with the N-Triples/Turtle
/// bulk loaders and `construct_triples`: labels are not freshened per loaded
/// document, so a `_:b` in one `LOAD` is identified with the same label in
/// another `LOAD` (or already in the store), and re-loading an identical
/// blank-node triple dedups. Per-document blank-node scoping belongs with the
/// dictionary store (SPEC-02), which carries blank-node identity explicitly.
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
        // best-effort lowering the loader applies). A LOAD of triple-term data
        // into a SPARQL 1.1 store is an edge case; keeping the lexical form is
        // better than dropping the triple silently.
        oxrdf::Term::Triple(tr) => Term::Literal(tr.to_string()),
    }
}

/// Preflight one operation: return the error it *would* produce at apply time,
/// without touching the store. Mirrors every rejecting path in the apply loop so
/// the whole update can be validated before the first mutation (SPARQL Update
/// atomicity, §3.1.3). A `LOAD` is validated by fetching + parsing its source
/// (a pure read); on success the parsed triples are discarded and re-fetched at
/// apply time — acceptable because LOAD is not on a hot path and the alternative
/// (threading the parsed triples through to apply) complicates the op loop.
fn validate_op(op: &spargebra::GraphUpdateOperation) -> Result<()> {
    use spargebra::term::GraphName;
    use spargebra::GraphUpdateOperation;
    match op {
        GraphUpdateOperation::InsertData { data } => {
            for q in data {
                object_to_term(&q.object)?;
            }
            Ok(())
        }
        GraphUpdateOperation::DeleteData { data } => {
            for q in data {
                ground_term_to_term(&q.object)?;
            }
            Ok(())
        }
        GraphUpdateOperation::DeleteInsert {
            delete,
            insert,
            using,
            pattern,
        } => validate_delete_insert(delete, insert, using.as_ref(), pattern),
        GraphUpdateOperation::Clear { silent, graph }
        | GraphUpdateOperation::Drop { silent, graph } => {
            use spargebra::algebra::GraphTarget;
            match graph {
                GraphTarget::DefaultGraph | GraphTarget::AllGraphs => Ok(()),
                GraphTarget::NamedNode(_) | GraphTarget::NamedGraphs => {
                    if *silent {
                        Ok(())
                    } else {
                        Err(named_graph_unsupported())
                    }
                }
            }
        }
        GraphUpdateOperation::Create { silent, .. } => {
            if *silent {
                Ok(())
            } else {
                Err(create_named_graph_unsupported())
            }
        }
        GraphUpdateOperation::Load {
            silent,
            source,
            destination,
        } => {
            if *silent {
                // A silent LOAD swallows every failure, so it can never abort
                // the update — nothing to preflight.
                return Ok(());
            }
            if let GraphName::NamedNode(_) = destination {
                return Err(SparqlError::UnsupportedAlgebra(
                    "LOAD INTO a named graph (Stage-1 default graph only)".into(),
                ));
            }
            // Fetch + parse now (pure read) to surface a non-silent fetch/parse
            // failure before any prior op mutates.
            fetch_and_parse(source.as_str()).map(|_| ())
        }
    }
}

/// Shared rejection scan for a pattern-based update. Returns the error a
/// `DeleteInsert` would produce, without mutating — used both by the atomicity
/// preflight and by `apply_delete_insert` itself.
fn validate_delete_insert(
    delete: &[GroundQuadPattern],
    insert: &[QuadPattern],
    using: Option<&spargebra::algebra::QueryDataset>,
    pattern: &spargebra::algebra::GraphPattern,
) -> Result<()> {
    // Reject a USING/USING NAMED dataset that redefines the graphs the
    // WHERE clause reads from (Stage-1 evaluates WHERE over the single
    // default graph only). A vacuous dataset (`None`, or one naming no
    // graphs) stays a no-op.
    if let Some(ds) = using {
        if !ds.default.is_empty() || ds.named.as_ref().is_some_and(|n| !n.is_empty()) {
            return Err(using_named_graph_unsupported());
        }
    }

    // Reject named-graph templates (Stage-1 default graph only).
    for q in delete {
        require_default_graph(&q.graph_name)?;
    }
    for q in insert {
        require_default_graph(&q.graph_name)?;
    }

    // Reject a GRAPH pattern anywhere in the WHERE clause. `translate_where`
    // lowers `GraphPattern::Graph { name, inner }` to its inner pattern over the
    // single default graph — an accepted Stage-1 simplification for *read*
    // queries, but for a mutating update it would make e.g.
    // `DELETE { ?s ?p ?o } WHERE { GRAPH <g> { ?s ?p ?o } }` delete
    // default-graph triples even though the named graph isn't represented
    // (silent data corruption). Stage-1 is default-graph only.
    if where_has_graph_pattern(pattern) {
        return Err(SparqlError::UnsupportedAlgebra(
            "GRAPH pattern in update WHERE clause (Stage-1 default graph only)".into(),
        ));
    }

    // Reject RDF 1.2 triple-term slots in any DELETE/INSERT template. The
    // Stage-1 store has no triple-term slot, so silently dropping such a
    // template triple (the `resolve_*` `Triple(_) => None` arms) while
    // reporting success is inconsistent with INSERT DATA / DELETE DATA, which
    // return `triple_term_unsupported()`. The up-front scan makes those `None`
    // arms unreachable for the triple-term reason.
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
    Ok(())
}

/// Evaluate the WHERE pattern, then instantiate the DELETE/INSERT
/// templates per solution. Per SPARQL 1.1 §3.1.3 the deletions are
/// computed and applied before the insertions; both are derived from the
/// WHERE solutions over the *pre-update* graph (we collect every row
/// first, which also releases the immutable read borrow before mutating).
fn apply_delete_insert<B: FullBackend>(
    store: &mut B,
    cfg: &SparqlConfig,
    delete: &[GroundQuadPattern],
    insert: &[QuadPattern],
    using: Option<&spargebra::algebra::QueryDataset>,
    pattern: &spargebra::algebra::GraphPattern,
) -> Result<()> {
    // All the rejections below must run before any mutation so a failing
    // update can't partially apply (and so the atomicity preflight in
    // `apply_update_with` can detect them without side effects).
    validate_delete_insert(delete, insert, using, pattern)?;

    let alg = translate_where(pattern, cfg)?;
    let plan = planner::plan(&alg)?;
    let rows: Vec<Bindings> = Runtime::new(store).run(&plan)?.collect();

    // Compute deletions from the original bindings first.
    let mut deletions: Vec<(Term, Term, Term)> = Vec::new();
    for row in &rows {
        for q in delete {
            if let (Some(s), Some(p), Some(o)) = (
                resolve_ground(&q.subject, row).and_then(subject_or_skip),
                resolve_pred(&q.predicate, row),
                resolve_ground(&q.object, row),
            ) {
                deletions.push((s, p, o));
            }
        }
    }
    // Insertions allocate fresh blank nodes per solution row.
    let mut insertions: Vec<(Term, Term, Term)> = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        for q in insert {
            if let (Some(s), Some(p), Some(o)) = (
                resolve_term(&q.subject, row, i).and_then(subject_or_skip),
                resolve_pred(&q.predicate, row),
                resolve_term(&q.object, row, i),
            ) {
                insertions.push((s, p, o));
            }
        }
    }

    for (s, p, o) in &deletions {
        store.delete_triple(s, p, o);
    }
    for (s, p, o) in insertions {
        store.insert_triple(s, p, o);
    }
    Ok(())
}

/// Recursively scan a WHERE pattern for any `GraphPattern::Graph` node.
/// Exhaustive over spargebra 0.4.6's `GraphPattern` variants so a new
/// variant forces a compile error here rather than silently passing.
fn where_has_graph_pattern(p: &spargebra::algebra::GraphPattern) -> bool {
    use spargebra::algebra::GraphPattern as GP;
    match p {
        // GRAPH node — the thing we reject.
        GP::Graph { .. } => true,
        // Leaves: no nested patterns.
        GP::Bgp { .. } | GP::Path { .. } | GP::Values { .. } => false,
        // Two children.
        GP::Join { left, right }
        | GP::LeftJoin { left, right, .. }
        | GP::Lateral { left, right }
        | GP::Union { left, right }
        | GP::Minus { left, right } => {
            where_has_graph_pattern(left) || where_has_graph_pattern(right)
        }
        // One inner child.
        GP::Filter { inner, .. }
        | GP::Extend { inner, .. }
        | GP::OrderBy { inner, .. }
        | GP::Project { inner, .. }
        | GP::Distinct { inner }
        | GP::Reduced { inner }
        | GP::Slice { inner, .. }
        | GP::Group { inner, .. } => where_has_graph_pattern(inner),
        // Service wraps a GRAPH-like remote target and an inner pattern;
        // the translator already rejects Service, but recurse for safety.
        GP::Service { inner, .. } => where_has_graph_pattern(inner),
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

fn require_default_graph(g: &GraphNamePattern) -> Result<()> {
    match g {
        GraphNamePattern::DefaultGraph => Ok(()),
        GraphNamePattern::NamedNode(_) | GraphNamePattern::Variable(_) => {
            Err(named_graph_unsupported())
        }
    }
}

/// Resolve an INSERT-template `TermPattern` against a solution row.
/// `row_ix` scopes per-solution blank nodes so each row's template
/// blank node is distinct (SPARQL 1.1 §4.1.4). Returns `None` when a
/// variable slot is unbound (the caller drops the triple).
///
/// Lockstep invariant: mirrors `runtime.rs::construct_triples`'s
/// `resolve_term`. They differ deliberately (this returns `Term` and
/// scopes blank nodes per row; construct returns `String`), but must stay
/// in lockstep on shared rules — especially when `Term::Triple` support
/// lands.
fn resolve_term(t: &TermPattern, row: &Bindings, row_ix: usize) -> Option<Term> {
    match t {
        TermPattern::NamedNode(n) => Some(Term::Iri(n.as_str().to_owned())),
        TermPattern::Literal(l) => Some(Term::Literal(l.to_string())),
        // Per-row blank-node scoping satisfies SPARQL §4.1.4 within one
        // solution (each row gets a distinct node) and assumes
        // spargebra-normalized template labels. Freshness *across*
        // separate updates is a known Stage-1 parity limit shared with
        // `runtime.rs::construct_triples`.
        TermPattern::BlankNode(b) => Some(Term::BlankNode(format!("{}_r{row_ix}", b.as_str()))),
        TermPattern::Variable(v) => row.get(v.as_str()).cloned(),
        // Triple-term template slots are rejected up front in
        // `apply_delete_insert` (triple_term_unsupported); this arm is
        // therefore unreachable for that reason but kept exhaustive.
        TermPattern::Triple(_) => None,
    }
}

/// Resolve a DELETE-template `GroundTermPattern` (no blank nodes allowed
/// in DELETE templates) against a solution row.
///
/// Lockstep invariant: see `resolve_pred` / `runtime.rs::construct_triples`.
fn resolve_ground(t: &GroundTermPattern, row: &Bindings) -> Option<Term> {
    match t {
        GroundTermPattern::NamedNode(n) => Some(Term::Iri(n.as_str().to_owned())),
        GroundTermPattern::Literal(l) => Some(Term::Literal(l.to_string())),
        GroundTermPattern::Variable(v) => row.get(v.as_str()).cloned(),
        // Rejected up front in `apply_delete_insert`; see `resolve_term`.
        GroundTermPattern::Triple(_) => None,
    }
}

/// Resolve a predicate template slot. Shared invariant with
/// `runtime.rs::construct_triples`'s `resolve_pred`: a predicate variable
/// binding is only valid if it resolves to an IRI (a literal or blank node
/// in predicate position drops the triple). The two copies legitimately
/// differ (this returns `Term`, construct returns `String`) but encode the
/// *same* rule and must stay in lockstep — especially when `Term::Triple`
/// support lands. See `runtime.rs::construct_triples`.
fn resolve_pred(p: &NamedNodePattern, row: &Bindings) -> Option<Term> {
    match p {
        NamedNodePattern::NamedNode(n) => Some(Term::Iri(n.as_str().to_owned())),
        NamedNodePattern::Variable(v) => match row.get(v.as_str()) {
            Some(Term::Iri(s)) => Some(Term::Iri(s.clone())),
            _ => None,
        },
    }
}

/// Position-aware subject guard. An instantiated template triple is a
/// legal RDF triple only if its subject is an IRI or a blank node; a
/// literal (or RDF 1.2 triple term) in subject position is illegal. Per
/// SPARQL 1.1 Update's illegal-RDF-construct rule (§4.1.4 / §10.2.1, the
/// same rule CONSTRUCT applies), such a template triple is **silently
/// skipped** — not an error — so the update still succeeds and the other
/// valid template triples in the same solution are still applied.
///
/// Returning `None` drops the whole triple in the caller's `if let`. Note
/// the object slot needs no such guard (literals are legal objects) and
/// predicate validity already lives in `resolve_pred` (IRI-only).
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
