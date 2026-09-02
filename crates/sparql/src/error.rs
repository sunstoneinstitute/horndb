//! Crate-wide error type for the SPARQL frontend.

use thiserror::Error;

/// Errors produced by the SPARQL frontend.
#[derive(Debug, Error)]
pub enum SparqlError {
    /// The input could not be parsed by `spargebra`.
    #[error("parse error: {0}")]
    Parse(String),

    /// The parsed AST contains a construct we do not translate yet.
    #[error("unsupported algebra construct: {0}")]
    UnsupportedAlgebra(String),

    /// The query references a property-path operator outside Stage 1 scope.
    #[error("unsupported property-path operator: {0}")]
    UnsupportedPathOp(String),

    /// The planner could not lower the algebra to a physical plan.
    #[error("planner error: {0}")]
    Planner(String),

    /// The executor rejected a plan or pattern.
    #[error("executor error: {0}")]
    Executor(String),

    /// SPEC-26 S5: the query's `query_timeout` elapsed and the query was
    /// cancelled through its `wcoj::CancelToken`. Distinct from
    /// [`SparqlError::Executor`] so a client can tell "your query was too
    /// slow" from "your query was wrong".
    #[error("query timeout exceeded")]
    QueryTimeout,

    /// SPEC-26 S5: the query produced more solutions than `max_result_rows`.
    /// The result is NOT truncated — the response ends with this error
    /// instead (before the headers commit: a 400; after: an aborted body),
    /// so a short result set can never be mistaken for a complete one.
    #[error("result row limit exceeded (max_result_rows = {0})")]
    ResultRowLimit(u64),

    /// I/O error.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Convenience `Result` alias.
pub type Result<T> = std::result::Result<T, SparqlError>;
