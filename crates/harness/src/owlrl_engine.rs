//! `Reasoner` impl for the real OWL 2 RL engine.
//!
//! Lives in the harness crate (not the engine crate) so the engine crate
//! doesn't depend on harness types — the trait is defined here and the
//! engine is foreign, so the orphan rule lets us add the impl on this
//! side. Gated behind the `real-engine` feature so the default harness
//! build doesn't pull in the rule engine.

use anyhow::Result;
use horndb_owlrl::Engine;
use oxrdf::Dataset;

use crate::reasoner::Reasoner;

impl Reasoner for Engine {
    fn name(&self) -> &str {
        "horndb-owlrl"
    }

    fn load(&mut self, dataset: &Dataset) -> Result<()> {
        Engine::load(self, dataset)
    }

    fn entails(&self, conclusion: &Dataset) -> Result<bool> {
        Engine::entails(self, conclusion)
    }

    fn is_consistent(&self) -> Result<bool> {
        Engine::is_consistent(self)
    }

    fn ask(&self, query: &str) -> Result<bool> {
        Engine::ask(self, query)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxrdf::{GraphName, NamedNode, NamedOrBlankNode, Quad};

    const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
    const RDFS_SUB_CLASS_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";

    fn nq(s: &str, p: &str, o: &str) -> Quad {
        Quad::new(
            NamedOrBlankNode::NamedNode(NamedNode::new(s).unwrap()),
            NamedNode::new(p).unwrap(),
            NamedNode::new(o).unwrap(),
            GraphName::DefaultGraph,
        )
    }

    #[test]
    fn reasoner_object_is_object_safe() {
        // Compile-time check: `Engine` satisfies the `Box<dyn Reasoner>`
        // path the harness binary uses.
        let _: Box<dyn Reasoner> = Box::new(Engine::new());
    }

    #[test]
    fn subclass_entailment_via_trait() {
        let mut engine: Box<dyn Reasoner> = Box::new(Engine::new());
        let mut p = Dataset::new();
        p.insert(&nq("http://ex/A", RDFS_SUB_CLASS_OF, "http://ex/B"));
        p.insert(&nq("http://ex/x", RDF_TYPE, "http://ex/A"));
        engine.load(&p).unwrap();
        let mut c = Dataset::new();
        c.insert(&nq("http://ex/x", RDF_TYPE, "http://ex/B"));
        assert!(engine.entails(&c).unwrap());
        assert_eq!(engine.name(), "horndb-owlrl");
    }
}
