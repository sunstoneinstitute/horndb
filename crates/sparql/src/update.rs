//! SPARQL Update — `INSERT DATA` / `DELETE DATA` plus pattern-based
//! `INSERT`/`DELETE … WHERE` (SPEC-07 F5).
//!
//! Graph-management verbs (`LOAD`, `CLEAR`, `DROP`, `CREATE`, …) and
//! multi-operation updates are still parsed but rejected at apply time
//! (see `parser::ParsedUpdate::UnsupportedForm` and SPEC-07 Future Work).

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
        | ParsedUpdate::DeleteInsert { inner } => &inner.operations,
        ParsedUpdate::UnsupportedForm { .. } => {
            return Err(SparqlError::UnsupportedAlgebra(
                "update form not supported in Stage 1".into(),
            ));
        }
    };
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
            other => {
                return Err(SparqlError::UnsupportedAlgebra(format!(
                    "update operation: {other:?}"
                )));
            }
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
    // Reject a USING/USING NAMED dataset that redefines the graphs the
    // WHERE clause reads from (Stage-1 evaluates WHERE over the single
    // default graph only). A vacuous dataset (`None`, or one naming no
    // graphs) stays a no-op. This must run before any mutation so an
    // ignored USING can never silently target the wrong graph.
    if let Some(ds) = using {
        if !ds.default.is_empty() || ds.named.as_ref().is_some_and(|n| !n.is_empty()) {
            return Err(using_named_graph_unsupported());
        }
    }

    // Reject named-graph templates up front (Stage-1 default graph only),
    // so a partially-applied update can't leave the store inconsistent.
    for q in delete {
        require_default_graph(&q.graph_name)?;
    }
    for q in insert {
        require_default_graph(&q.graph_name)?;
    }

    // Reject a GRAPH pattern anywhere in the WHERE clause before mutating.
    // `translate_where` lowers `GraphPattern::Graph { name, inner }` to its
    // inner pattern over the single default graph — an accepted Stage-1
    // simplification for *read* queries, but for a mutating update it would
    // make e.g. `DELETE { ?s ?p ?o } WHERE { GRAPH <g> { ?s ?p ?o } }` delete
    // default-graph triples even though the named graph isn't represented
    // (silent data corruption). Stage-1 is default-graph only.
    if where_has_graph_pattern(pattern) {
        return Err(SparqlError::UnsupportedAlgebra(
            "GRAPH pattern in update WHERE clause (Stage-1 default graph only)".into(),
        ));
    }

    // Reject RDF 1.2 triple-term slots in any DELETE/INSERT template before
    // mutating. The Stage-1 store has no triple-term slot, so silently
    // dropping such a template triple (the `resolve_*` `Triple(_) => None`
    // arms) while reporting success is inconsistent with INSERT DATA /
    // DELETE DATA, which return `triple_term_unsupported()`. The up-front
    // scan makes those `None` arms unreachable for the triple-term reason.
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
