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
    Select { inner: Query },
    Ask { inner: Query },
    Construct { inner: Query },
    /// DESCRIBE is recognised by the parser but rejected here in
    /// Stage 1; left as its own variant so the regression surface
    /// shows up at the `match` site, not as a silent fallthrough.
    Describe { inner: Query },
}

/// A successfully parsed SPARQL update. Stage 1 only supports
/// `INSERT DATA` and `DELETE DATA` literal forms.
#[derive(Debug, Clone)]
pub enum ParsedUpdate {
    InsertData { inner: Update },
    DeleteData { inner: Update },
    /// Any other update form (LOAD/CLEAR/DROP/INSERT WHERE/...) is
    /// parsed but flagged as out-of-scope at runtime.
    UnsupportedForm { inner: Update },
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
/// In Stage 1 we recognise `INSERT DATA` and `DELETE DATA` only.
/// Other update forms parse successfully but are classified as
/// `UnsupportedForm`; the executor returns an explicit error when
/// asked to apply them.
pub fn parse_update(input: &str) -> Result<ParsedUpdate> {
    let u = SparqlParser::new()
        .parse_update(input)
        .map_err(|e| SparqlError::Parse(e.to_string()))?;

    // `spargebra::Update` is a sequence of `GraphUpdateOperation`s.
    // We classify by the *first* operation in Stage 1; multi-op
    // updates degrade to `UnsupportedForm` and the executor rejects
    // them. This is fine for the W3C subset we're targeting.
    use spargebra::GraphUpdateOperation;
    match u.operations.first() {
        Some(GraphUpdateOperation::InsertData { .. }) if u.operations.len() == 1 => {
            Ok(ParsedUpdate::InsertData { inner: u })
        }
        Some(GraphUpdateOperation::DeleteData { .. }) if u.operations.len() == 1 => {
            Ok(ParsedUpdate::DeleteData { inner: u })
        }
        Some(_) => Ok(ParsedUpdate::UnsupportedForm { inner: u }),
        None => Err(SparqlError::Parse("update contains no operations".into())),
    }
}
