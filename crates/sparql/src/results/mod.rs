//! Result serialisation. Stage 1 supports SPARQL JSON, CSV, TSV;
//! XML is deferred.

pub mod csv;
pub mod json;
pub mod tsv;

/// Wire-format selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultFormat {
    Json,
    Csv,
    Tsv,
}

impl ResultFormat {
    /// Map a `Accept` / format query-parameter value to a format.
    /// Defaults to JSON.
    pub fn from_accept(accept: &str) -> Self {
        let a = accept.to_ascii_lowercase();
        if a.contains("text/csv") {
            Self::Csv
        } else if a.contains("text/tab-separated-values") || a.contains("tsv") {
            Self::Tsv
        } else {
            Self::Json
        }
    }
    pub fn content_type(self) -> &'static str {
        match self {
            Self::Json => "application/sparql-results+json",
            Self::Csv => "text/csv",
            Self::Tsv => "text/tab-separated-values",
        }
    }
}
