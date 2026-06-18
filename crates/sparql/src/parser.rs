//! Thin wrapper around the `spargebra` crate.
//!
//! We re-shape `spargebra::Query` / `spargebra::Update` into a smaller
//! `ParsedQuery` / `ParsedUpdate` enum that the rest of the crate
//! pattern-matches against. This isolates the upstream churn surface:
//! `spargebra` does not yet guarantee API stability, and our acceptance
//! tests rely on the W3C grammar via spargebra, not on spargebra's own
//! API shape.

use crate::error::{Result, SparqlError};
use spargebra::{Query, SparqlParser, Update};

/// A successfully parsed SPARQL query, classified by query form.
///
/// The variants carry the upstream `spargebra::Query` payload verbatim
/// (which holds the algebra) so that downstream code can pattern-match
/// without re-parsing.
#[derive(Debug, Clone)]
pub enum ParsedQuery {
    Select {
        inner: Query,
    },
    Ask {
        inner: Query,
    },
    Construct {
        inner: Query,
    },
    /// DESCRIBE: executed as a forward, one-level Concise Bounded
    /// Description over the resources bound to the query's projected
    /// variables (see `api::execute_query_with` and
    /// `runtime::describe_triples`).
    Describe {
        inner: Query,
    },
}

/// A successfully parsed SPARQL update. Stage 1 only supports
/// `INSERT DATA` and `DELETE DATA` literal forms.
#[derive(Debug, Clone)]
pub enum ParsedUpdate {
    InsertData {
        inner: Update,
    },
    DeleteData {
        inner: Update,
    },
    /// Pattern-based update: `INSERT { … } WHERE { … }`,
    /// `DELETE { … } WHERE { … }`, `DELETE WHERE { … }`, or the
    /// combined `WITH/DELETE/INSERT … WHERE` form. spargebra lowers all
    /// of these (including the `DELETE WHERE` shorthand) into a single
    /// `GraphUpdateOperation::DeleteInsert`.
    DeleteInsert {
        inner: Update,
    },
    /// A graph-management and/or multi-operation update whose operations are
    /// all in the executable set (`LOAD`, `CLEAR`, `DROP`, `CREATE`, plus the
    /// data/pattern forms). `ADD`/`MOVE`/`COPY` desugar (in spargebra) into
    /// `Drop` + `DeleteInsert` sequences and land here. The executor walks the
    /// whole operation sequence in order.
    GraphManagement {
        inner: Update,
    },
    /// Any update form the executor cannot apply is parsed but flagged as
    /// out-of-scope at runtime. With spargebra 0.3.5 every standard verb is
    /// executable, so this arm is reserved for forward-compatibility (a future
    /// spargebra variant) and is unreachable today.
    UnsupportedForm {
        inner: Update,
    },
}

/// Parse a SPARQL 1.1 query string.
///
/// Defaults: no base IRI, no prefix mappings beyond those declared
/// in the query itself.
pub fn parse_query(input: &str) -> Result<ParsedQuery> {
    let q = SparqlParser::new()
        .parse_query(input)
        .map_err(|e| SparqlError::Parse(e.to_string()))?;
    Ok(match &q {
        Query::Select { .. } => ParsedQuery::Select { inner: q },
        Query::Ask { .. } => ParsedQuery::Ask { inner: q },
        Query::Construct { .. } => ParsedQuery::Construct { inner: q },
        Query::Describe { .. } => ParsedQuery::Describe { inner: q },
    })
}

/// Parse a SPARQL 1.1 update string.
///
/// A single data or pattern operation gets its dedicated classification
/// (`InsertData`, `DeleteData`, `DeleteInsert` — the last covers
/// `INSERT { … } WHERE { … }`, `DELETE { … } WHERE { … }`, `DELETE WHERE { … }`,
/// and the combined `WITH/DELETE/INSERT … WHERE` form, all lowered by spargebra
/// to one `GraphUpdateOperation::DeleteInsert`). Everything else — a
/// graph-management verb (`LOAD`/`CLEAR`/`DROP`/`CREATE`) or any multi-operation
/// sequence (including spargebra's `ADD`/`MOVE`/`COPY` desugaring) — is a
/// `GraphManagement` update the executor walks in order. An empty operation
/// list (the W3C identity-case rewrite of `ADD`/`MOVE`/`COPY <g> TO <g>`) is a
/// valid no-op. The `UnsupportedForm` arm is reserved for a future spargebra
/// variant the executor cannot apply; with spargebra 0.3.5 nothing reaches it.
pub fn parse_update(input: &str) -> Result<ParsedUpdate> {
    let u = SparqlParser::new()
        .parse_update(input)
        .map_err(|e| SparqlError::Parse(e.to_string()))?;

    // `spargebra::Update` is a sequence of `GraphUpdateOperation`s. A
    // single data/pattern operation keeps its dedicated classification so
    // existing call sites are unchanged. Anything else — a graph-management
    // verb (`LOAD`/`CLEAR`/`DROP`/`CREATE`) or a multi-operation sequence
    // (which is also how spargebra desugars `ADD`/`MOVE`/`COPY`) — is a
    // `GraphManagement` update; the executor walks the whole sequence. With
    // spargebra 0.3.5 every operation variant is executable, so no update
    // degrades to `UnsupportedForm` here.
    use spargebra::GraphUpdateOperation;
    match u.operations.as_slice() {
        [GraphUpdateOperation::InsertData { .. }] => Ok(ParsedUpdate::InsertData { inner: u }),
        [GraphUpdateOperation::DeleteData { .. }] => Ok(ParsedUpdate::DeleteData { inner: u }),
        [GraphUpdateOperation::DeleteInsert { .. }] => Ok(ParsedUpdate::DeleteInsert { inner: u }),
        // An empty operation list is the W3C identity-case rewrite of
        // `ADD`/`MOVE`/`COPY <g> TO <g>` (same source and destination):
        // spargebra lowers it to no operations, and SPARQL 1.1 defines it as a
        // valid no-op. The executor walks an empty list and does nothing.
        _ => Ok(ParsedUpdate::GraphManagement { inner: u }),
    }
}
