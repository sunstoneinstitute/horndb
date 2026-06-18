//! Loaded representation of a single W3C-style test case.

use std::path::PathBuf;

/// Suites the harness understands at Stage 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Suite {
    Owl2,
    Sparql11,
    /// W3C SPARQL 1.1 *syntax* tests (positive + negative, query + update).
    /// These assert only that the SPARQL grammar accepts/rejects the input;
    /// there is no data, no result set, and no reasoner involvement. Source:
    /// <https://www.w3.org/2009/sparql/docs/tests/> (the `syntax-query` /
    /// `syntax-update-1` / `syntax-update-2` sub-suites).
    Sparql11Syntax,
    /// W3C RDF 1.2 N-Triples syntax tests (positive + negative).
    /// Source: <https://w3c.github.io/rdf-tests/rdf/rdf12/rdf-n-triples/syntax/>.
    Rdf12NTriples,
}

impl Suite {
    pub fn as_str(self) -> &'static str {
        match self {
            Suite::Owl2 => "owl2",
            Suite::Sparql11 => "sparql11",
            Suite::Sparql11Syntax => "sparql11-syntax",
            Suite::Rdf12NTriples => "rdf12-n-triples",
        }
    }
}

/// Kinds of tests the harness recognises (SPEC-01 F1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestKind {
    /// Premise entails conclusion (both are RDF graphs).
    PositiveEntailment {
        premise: PathBuf,
        conclusion: PathBuf,
    },
    /// Premise does *not* entail conclusion.
    NegativeEntailment {
        premise: PathBuf,
        conclusion: PathBuf,
    },
    /// Premise graph is consistent.
    Consistency { premise: PathBuf },
    /// Premise graph is inconsistent.
    Inconsistency { premise: PathBuf },
    /// SPARQL ASK whose expected boolean answer is known.
    SparqlAsk {
        query: PathBuf,
        data: PathBuf,
        expected: bool,
    },
    /// `input` parses successfully under the named syntax. Used by the
    /// W3C RDF 1.2 N-Triples syntax suite (positive cases), where we
    /// only assert "the parser accepts the file" — no entailment,
    /// no reasoner involvement.
    SyntaxPositive { input: PathBuf },
    /// `input` *must* fail to parse. Used by the bad-syntax cases of
    /// the same suite.
    SyntaxNegative { input: PathBuf },
    /// `input` is a SPARQL query/update that the SPARQL 1.1 grammar must
    /// *accept*. Graded by `spargebra` (the same parser the SPEC-07 engine
    /// uses) — a positive syntax test passes iff parsing succeeds. No data,
    /// no result set, no reasoner.
    SparqlSyntaxPositive { input: PathBuf, update: bool },
    /// `input` is a SPARQL query/update that the SPARQL 1.1 grammar must
    /// *reject*. Passes iff `spargebra` fails to parse it.
    SparqlSyntaxNegative { input: PathBuf, update: bool },
}

#[derive(Debug, Clone)]
pub struct TestCase {
    /// Globally unique within a manifest (used in selected.toml).
    pub id: String,
    pub suite: Suite,
    pub name: String,
    pub kind: TestKind,
}
