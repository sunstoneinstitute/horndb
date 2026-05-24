//! Loaded representation of a single W3C-style test case.

use std::path::PathBuf;

/// Suites the harness understands at Stage 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Suite {
    Owl2,
    Sparql11,
}

impl Suite {
    pub fn as_str(self) -> &'static str {
        match self {
            Suite::Owl2 => "owl2",
            Suite::Sparql11 => "sparql11",
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
}

#[derive(Debug, Clone)]
pub struct TestCase {
    /// Globally unique within a manifest (used in selected.toml).
    pub id: String,
    pub suite: Suite,
    pub name: String,
    pub kind: TestKind,
}
