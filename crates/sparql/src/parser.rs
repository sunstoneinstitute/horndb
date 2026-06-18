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
    /// Non-standard `EXPLAIN` pragma (SPEC-07 F9): describe the chosen
    /// physical plan, its per-node cardinality estimates, and the
    /// execution mode instead of running the query. `inner` is the
    /// wrapped query (any form); `json` selects the rendering format
    /// (`EXPLAIN JSON …`).
    Explain {
        inner: Box<ParsedQuery>,
        json: bool,
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
    // Non-standard leading `EXPLAIN` pragma (SPEC-07 F9). spargebra does
    // not know this keyword, so we strip it before parsing and wrap the
    // result. `EXPLAIN` must lead the request (it precedes any PREFIX/BASE
    // prologue), matching the convention of other engines' EXPLAIN.
    if let Some((rest, json)) = strip_explain_pragma(input) {
        let inner = parse_query(rest)?;
        return Ok(ParsedQuery::Explain {
            inner: Box::new(inner),
            json,
        });
    }

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

/// If `input` begins with the non-standard `EXPLAIN` pragma (optionally
/// `EXPLAIN JSON`), return `(remaining_query, json_format)`; otherwise
/// `None`.
///
/// Matching is case-insensitive and requires the `EXPLAIN` (and optional
/// `JSON`) token to be followed by ASCII whitespace, so a query that
/// merely *starts* a variable or IRI with those letters is not mistaken
/// for the pragma (SPARQL keywords are not a concern here — `EXPLAIN` is
/// not a SPARQL keyword and a bare `EXPLAIN` with no following query is a
/// parse error from the recursive call). Leading whitespace is tolerated.
fn strip_explain_pragma(input: &str) -> Option<(&str, bool)> {
    let trimmed = input.trim_start();
    let rest = strip_keyword_ci(trimmed, "EXPLAIN")?;
    // `EXPLAIN` matched and was followed by whitespace; now look for an
    // optional `JSON` sub-keyword, also whitespace-terminated.
    if let Some(after_json) = strip_keyword_ci(rest.trim_start(), "JSON") {
        Some((after_json, true))
    } else {
        Some((rest, false))
    }
}

/// If `s` starts with `kw` (ASCII-case-insensitive) followed by ASCII
/// whitespace, return the slice after the keyword (the whitespace itself
/// is left for the caller to trim). Returns `None` if `kw` is not a
/// whitespace-delimited prefix of `s`.
///
/// `kw` must be ASCII (the callers pass `"EXPLAIN"` / `"JSON"`). The
/// comparison is done byte-wise rather than by string slicing so that a
/// non-ASCII `s` whose `kw.len()`-th byte falls in the middle of a
/// multibyte UTF-8 character does not panic (`&s[..kw.len()]` would
/// require a char boundary). The trailing-whitespace check and the
/// `kw.len()` slice index are both at the keyword's byte length, which is
/// a char boundary exactly when the leading bytes are all ASCII — which
/// the per-byte `eq_ignore_ascii_case` guarantees on the matching path.
fn strip_keyword_ci<'a>(s: &'a str, kw: &str) -> Option<&'a str> {
    debug_assert!(kw.is_ascii(), "strip_keyword_ci expects an ASCII keyword");
    let bytes = s.as_bytes();
    let kw_bytes = kw.as_bytes();
    if bytes.len() < kw_bytes.len() {
        return None;
    }
    // Per-byte ASCII-case-insensitive compare — never slices `s`, so a
    // multibyte char straddling the keyword length cannot panic.
    if !bytes[..kw_bytes.len()].eq_ignore_ascii_case(kw_bytes) {
        return None;
    }
    // The character immediately after the keyword must be ASCII
    // whitespace, so `EXPLAINING` / `EXPLAIN(...)` are not matched.
    // Because every keyword byte matched ASCII, `kw_bytes.len()` is a
    // char boundary here and `&s[kw_bytes.len()..]` is safe.
    match bytes.get(kw_bytes.len()) {
        Some(c) if c.is_ascii_whitespace() => Some(&s[kw_bytes.len()..]),
        _ => None,
    }
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
