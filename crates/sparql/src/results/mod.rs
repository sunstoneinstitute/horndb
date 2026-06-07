//! Result serialisation. Stage 1 supports SPARQL JSON, XML, CSV, TSV.

pub mod csv;
pub mod json;
pub mod tsv;
pub mod xml;

/// Wire-format selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultFormat {
    Json,
    Xml,
    Csv,
    Tsv,
}

impl ResultFormat {
    /// Map an `Accept` / format query-parameter value to a format.
    ///
    /// An explicit, unambiguous preference for one of the supported
    /// formats wins. When the client sends nothing, `*/*`, or any value
    /// that names no specific results format, we default to **XML** —
    /// the SPARQL Results XML format is the one tooling (e.g. the LDBC
    /// SPB driver's SAX analyzer) most commonly assumes, and emitting
    /// it keeps those clients working.
    pub fn from_accept(accept: &str) -> Self {
        let a = accept.to_ascii_lowercase();
        if a.contains("application/sparql-results+json") || a.contains("application/json") {
            Self::Json
        } else if a.contains("application/sparql-results+xml") || a.contains("application/xml") {
            Self::Xml
        } else if a.contains("text/csv") {
            Self::Csv
        } else if a.contains("text/tab-separated-values") || a.contains("tsv") {
            Self::Tsv
        } else {
            // No specific results format requested (empty, `*/*`,
            // `text/html`, …): default to XML.
            Self::Xml
        }
    }
    pub fn content_type(self) -> &'static str {
        match self {
            Self::Json => "application/sparql-results+json",
            Self::Xml => "application/sparql-results+xml",
            Self::Csv => "text/csv",
            Self::Tsv => "text/tab-separated-values",
        }
    }
}
