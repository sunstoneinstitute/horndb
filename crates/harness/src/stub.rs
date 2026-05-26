//! In-tree stub reasoner used by SPEC-01 F12.
//!
//! Purpose: prove the harness itself works *before* any real engine
//! exists. The stub is deliberately weak — it only "knows" how to:
//!
//! 1. Confirm that the empty graph entails the empty graph (so the
//!    most-trivial positive-entailment test passes).
//! 2. Report inconsistency when an explicit `owl:Nothing` membership
//!    triple is present (so a hand-rolled inconsistency test fails red
//!    against a graph that lacks it).
//! 3. Answer `ASK { ?s ?p ?o }` truthfully based on whether any triple
//!    was loaded.
//!
//! Everything else returns `false` (which makes "real" tests fail —
//! that is the whole point: a deliberately-failing reference
//! implementation is correctly flagged red, per SPEC-01 Stage-0 exit
//! criterion 3).

use anyhow::Result;
use oxrdf::{Dataset, NamedNodeRef, TermRef};

use crate::reasoner::Reasoner;

#[derive(Default)]
pub struct StubReasoner {
    triple_count: usize,
    contains_owl_nothing_membership: bool,
}

impl StubReasoner {
    pub fn new() -> Self {
        Self::default()
    }
}

const OWL_NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

impl Reasoner for StubReasoner {
    fn name(&self) -> &str {
        "stub"
    }

    fn load(&mut self, dataset: &Dataset) -> Result<()> {
        self.triple_count = dataset.len();
        let nothing = NamedNodeRef::new(OWL_NOTHING)?;
        let rdf_type = NamedNodeRef::new(RDF_TYPE)?;
        let nothing_term: TermRef<'_> = nothing.into();
        self.contains_owl_nothing_membership = dataset
            .quads_for_predicate(rdf_type)
            .any(|q| q.object == nothing_term);
        Ok(())
    }

    fn entails(&self, conclusion: &Dataset) -> Result<bool> {
        // The empty graph entails the empty graph, and nothing else.
        Ok(conclusion.is_empty())
    }

    fn is_consistent(&self) -> Result<bool> {
        Ok(!self.contains_owl_nothing_membership)
    }

    fn ask(&self, _query: &str) -> Result<bool> {
        // The stub does not parse SPARQL. It returns `true` iff
        // anything was loaded, which is just enough to make a trivial
        // ASK test pass and any non-trivial one fail.
        Ok(self.triple_count > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxrdf::{Dataset, NamedNode, NamedOrBlankNode, Quad};

    fn quad(s: &str, p: &str, o: &str) -> Quad {
        Quad::new(
            NamedOrBlankNode::NamedNode(NamedNode::new(s).unwrap()),
            NamedNode::new(p).unwrap(),
            NamedNode::new(o).unwrap(),
            oxrdf::GraphName::DefaultGraph,
        )
    }

    #[test]
    fn empty_entails_empty() {
        let s = StubReasoner::new();
        assert!(s.entails(&Dataset::new()).unwrap());
    }

    #[test]
    fn nonempty_conclusion_is_not_entailed() {
        let s = StubReasoner::new();
        let mut concl = Dataset::new();
        concl.insert(&quad("http://ex/a", RDF_TYPE, "http://ex/C"));
        assert!(!s.entails(&concl).unwrap());
    }

    #[test]
    fn graph_with_owl_nothing_membership_is_inconsistent() {
        let mut s = StubReasoner::new();
        let mut data = Dataset::new();
        data.insert(&quad("http://ex/a", RDF_TYPE, OWL_NOTHING));
        s.load(&data).unwrap();
        assert!(!s.is_consistent().unwrap());
    }

    #[test]
    fn empty_graph_is_consistent() {
        let mut s = StubReasoner::new();
        s.load(&Dataset::new()).unwrap();
        assert!(s.is_consistent().unwrap());
    }

    #[test]
    fn ask_true_when_anything_loaded() {
        let mut s = StubReasoner::new();
        let mut data = Dataset::new();
        data.insert(&quad("http://ex/a", "http://ex/p", "http://ex/b"));
        s.load(&data).unwrap();
        assert!(s.ask("ASK { ?s ?p ?o }").unwrap());
    }

    #[test]
    fn ask_false_when_nothing_loaded() {
        let s = StubReasoner::new();
        assert!(!s.ask("ASK { ?s ?p ?o }").unwrap());
    }
}
