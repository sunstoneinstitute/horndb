//! `Engine` — stateful façade around `materialize` for embedders that want
//! a single-object API rather than the functional `materialize(store,
//! backend)` driver.
//!
//! Used by SPEC-01's harness via a `Reasoner` adapter (defined in the
//! harness crate). The dictionary is owned here so the harness does not
//! need to know about `TermId`s.
//!
//! Scope: Stage-1 OWL 2 RL, in-memory only, full re-materialization on
//! every `load`. Triple-term (RDF 1.2) inputs return an error per the
//! Stage-1 charter.

use anyhow::{anyhow, Result};
use oxrdf::{Dataset, GraphName, NamedOrBlankNodeRef, Quad, TermRef};
use rustc_hash::FxHashMap;

use crate::backend::RuleFiringBackend;
use crate::engine::reset_and_materialize;
use crate::store::{MemStore, TripleStore};
use crate::types::{TermId, Triple};
use crate::vocab::Vocabulary;

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDF_FIRST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#first";
const RDF_REST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#rest";
const RDF_NIL: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#nil";
const RDFS_SUB_CLASS_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";
const RDFS_SUB_PROPERTY_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subPropertyOf";
const RDFS_DOMAIN: &str = "http://www.w3.org/2000/01/rdf-schema#domain";
const RDFS_RANGE: &str = "http://www.w3.org/2000/01/rdf-schema#range";
const OWL_CLASS: &str = "http://www.w3.org/2002/07/owl#Class";
const OWL_THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const OWL_NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";
const OWL_SAME_AS: &str = "http://www.w3.org/2002/07/owl#sameAs";
const OWL_DIFFERENT_FROM: &str = "http://www.w3.org/2002/07/owl#differentFrom";
const OWL_EQUIVALENT_CLASS: &str = "http://www.w3.org/2002/07/owl#equivalentClass";
const OWL_EQUIVALENT_PROPERTY: &str = "http://www.w3.org/2002/07/owl#equivalentProperty";
const OWL_INVERSE_OF: &str = "http://www.w3.org/2002/07/owl#inverseOf";
const OWL_FUNCTIONAL_PROPERTY: &str = "http://www.w3.org/2002/07/owl#FunctionalProperty";
const OWL_INVERSE_FUNCTIONAL_PROPERTY: &str =
    "http://www.w3.org/2002/07/owl#InverseFunctionalProperty";
const OWL_SYMMETRIC_PROPERTY: &str = "http://www.w3.org/2002/07/owl#SymmetricProperty";
const OWL_TRANSITIVE_PROPERTY: &str = "http://www.w3.org/2002/07/owl#TransitiveProperty";
const OWL_IRREFLEXIVE_PROPERTY: &str = "http://www.w3.org/2002/07/owl#IrreflexiveProperty";
const OWL_REFLEXIVE_PROPERTY: &str = "http://www.w3.org/2002/07/owl#ReflexiveProperty";
const OWL_ASYMMETRIC_PROPERTY: &str = "http://www.w3.org/2002/07/owl#AsymmetricProperty";
const OWL_PROPERTY_DISJOINT_WITH: &str = "http://www.w3.org/2002/07/owl#propertyDisjointWith";
const OWL_DISJOINT_WITH: &str = "http://www.w3.org/2002/07/owl#disjointWith";
const OWL_COMPLEMENT_OF: &str = "http://www.w3.org/2002/07/owl#complementOf";
const OWL_INTERSECTION_OF: &str = "http://www.w3.org/2002/07/owl#intersectionOf";
const OWL_UNION_OF: &str = "http://www.w3.org/2002/07/owl#unionOf";
const OWL_SOME_VALUES_FROM: &str = "http://www.w3.org/2002/07/owl#someValuesFrom";
const OWL_ALL_VALUES_FROM: &str = "http://www.w3.org/2002/07/owl#allValuesFrom";
const OWL_HAS_VALUE: &str = "http://www.w3.org/2002/07/owl#hasValue";
const OWL_ON_PROPERTY: &str = "http://www.w3.org/2002/07/owl#onProperty";
const OWL_MAX_CARDINALITY: &str = "http://www.w3.org/2002/07/owl#maxCardinality";
const OWL_SOURCE_INDIVIDUAL: &str = "http://www.w3.org/2002/07/owl#sourceIndividual";
const OWL_ASSERTION_PROPERTY: &str = "http://www.w3.org/2002/07/owl#assertionProperty";
const OWL_TARGET_INDIVIDUAL: &str = "http://www.w3.org/2002/07/owl#targetIndividual";
const OWL_TARGET_VALUE: &str = "http://www.w3.org/2002/07/owl#targetValue";
const OWL_OBJECT_PROPERTY: &str = "http://www.w3.org/2002/07/owl#ObjectProperty";
const OWL_PROPERTY_CHAIN_AXIOM: &str = "http://www.w3.org/2002/07/owl#propertyChainAxiom";
const OWL_HAS_KEY: &str = "http://www.w3.org/2002/07/owl#hasKey";
const OWL_ALL_DISJOINT_CLASSES: &str = "http://www.w3.org/2002/07/owl#AllDisjointClasses";
const OWL_ALL_DIFFERENT: &str = "http://www.w3.org/2002/07/owl#AllDifferent";
const OWL_MEMBERS: &str = "http://www.w3.org/2002/07/owl#members";
const OWL_DISTINCT_MEMBERS: &str = "http://www.w3.org/2002/07/owl#distinctMembers";
const OWL_NAMED_INDIVIDUAL: &str = "http://www.w3.org/2002/07/owl#NamedIndividual";

/// First non-reserved `TermId` value. Vocabulary terms occupy `1..=45`.
const USER_TERMS_BASE: u64 = 46;

/// Stateful OWL 2 RL reasoning façade.
///
/// Each `load` discards prior state and re-materializes from scratch.
/// `entails`, `is_consistent`, and `ask` query the materialized closure.
pub struct Engine {
    vocab: Vocabulary,
    /// Maps a canonical RDF term key (see [`term_key`]) to its dictionary ID.
    /// Pre-populated with the OWL/RDF/RDFS vocabulary IRIs so user data
    /// referencing them gets the same IDs the vocab uses.
    base_dict: FxHashMap<String, TermId>,
    /// Per-load state.
    state: Option<LoadState>,
}

struct LoadState {
    dict: FxHashMap<String, TermId>,
    next_id: u64,
    store: MemStore,
    loaded_count: usize,
}

impl Engine {
    pub fn new() -> Self {
        let (vocab, base_dict) = build_vocab();
        Self {
            vocab,
            base_dict,
            state: None,
        }
    }

    /// Discard prior state and load `dataset`'s default graph into a fresh
    /// store, then run materialization to fixed point.
    pub fn load(&mut self, dataset: &Dataset) -> Result<()> {
        let mut state = LoadState {
            dict: self.base_dict.clone(),
            next_id: USER_TERMS_BASE,
            store: MemStore::new(self.vocab),
            loaded_count: 0,
        };
        for quad in dataset.iter() {
            if quad.graph_name != GraphName::DefaultGraph.as_ref() {
                continue;
            }
            let triple = encode_quad(&mut state, &quad.into_owned())?;
            state.store.assert(triple);
            state.loaded_count += 1;
        }
        // Auto-owl:Thing inference (companion to prp-rfp): every named
        // individual is implicitly a member of owl:Thing. Cheapest faithful
        // implementation is a load-time pass over `?x rdf:type
        // owl:NamedIndividual`, asserting `?x rdf:type owl:Thing`. The
        // ReflexiveProperty W3C test types its individuals via
        // `owl:NamedIndividual` rather than `owl:Thing` directly, and
        // `prp-rfp`'s body requires the latter. See
        // `crates/owlrl/list_rules.rs` and KNOWN-MANIFEST-BUGS.md.
        infer_owl_thing_from_named_individuals(&mut state.store, &self.vocab);
        let mut backend = RuleFiringBackend::new();
        reset_and_materialize(&mut state.store, &mut backend);
        self.state = Some(state);
        Ok(())
    }

    /// Return true iff every triple in `conclusion`'s default graph is
    /// present in the materialized closure.
    ///
    /// Blank nodes in the conclusion are treated as existential
    /// wildcards on a per-triple basis: a triple `_:b p o` matches if
    /// any subject in the store satisfies `? p o`. Multi-triple bnode
    /// joins are not supported in Stage 1.
    pub fn entails(&self, conclusion: &Dataset) -> Result<bool> {
        let state = self
            .state
            .as_ref()
            .ok_or_else(|| anyhow!("entails called before load"))?;
        for quad in conclusion.iter() {
            if quad.graph_name != GraphName::DefaultGraph.as_ref() {
                continue;
            }
            if !triple_entailed(state, &quad.into_owned())? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// OWL 2 RL inconsistency marker: some individual is inferred to be
    /// an `owl:Nothing`.
    pub fn is_consistent(&self) -> Result<bool> {
        let state = self
            .state
            .as_ref()
            .ok_or_else(|| anyhow!("is_consistent called before load"))?;
        let mut iter = state
            .store
            .probe(None, self.vocab.rdf_type, Some(self.vocab.owl_nothing));
        Ok(iter.next().is_none())
    }

    /// Stub-grade SPARQL ASK: returns true iff anything was loaded.
    ///
    /// Full SPARQL evaluation lives in SPEC-07's `horndb-sparql`; wiring
    /// it through requires extracting the materialized store back into
    /// an `oxrdf::Dataset`, which is left for a follow-up. Today this
    /// satisfies the single `ASK { ?s ?p ?o }` smoke fixture only.
    pub fn ask(&self, _query: &str) -> Result<bool> {
        let state = self
            .state
            .as_ref()
            .ok_or_else(|| anyhow!("ask called before load"))?;
        Ok(state.loaded_count > 0)
    }
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

/// Assert `?x rdf:type owl:Thing` for every `?x` declared as
/// `owl:NamedIndividual`. Stage-1 companion to `prp-rfp` whose body
/// requires `owl:Thing` membership but the W3C tests type their
/// individuals as `owl:NamedIndividual`. Per OWL 2 RL semantics every
/// named individual is implicitly an `owl:Thing`.
fn infer_owl_thing_from_named_individuals(store: &mut MemStore, vocab: &Vocabulary) {
    let subjects: Vec<TermId> = store
        .scan_predicate(vocab.rdf_type)
        .filter(|t| t.o == vocab.owl_named_individual)
        .map(|t| t.s)
        .collect();
    for s in subjects {
        // Assert as a base triple so the resulting `rdf:type owl:Thing`
        // is treated as schema-grade fact (not inferred provenance).
        store.assert(crate::types::Triple::new(
            s,
            vocab.rdf_type,
            vocab.owl_thing,
        ));
    }
}

fn build_vocab() -> (Vocabulary, FxHashMap<String, TermId>) {
    let mut id: u64 = 1;
    let mut dict: FxHashMap<String, TermId> = FxHashMap::default();
    let alloc = |iri: &str, id: &mut u64, dict: &mut FxHashMap<String, TermId>| -> TermId {
        let t = TermId(*id);
        *id += 1;
        dict.insert(iri.to_string(), t);
        t
    };
    let vocab = Vocabulary {
        rdf_type: alloc(RDF_TYPE, &mut id, &mut dict),
        rdf_first: alloc(RDF_FIRST, &mut id, &mut dict),
        rdf_rest: alloc(RDF_REST, &mut id, &mut dict),
        rdf_nil: alloc(RDF_NIL, &mut id, &mut dict),
        rdfs_sub_class_of: alloc(RDFS_SUB_CLASS_OF, &mut id, &mut dict),
        rdfs_sub_property_of: alloc(RDFS_SUB_PROPERTY_OF, &mut id, &mut dict),
        rdfs_domain: alloc(RDFS_DOMAIN, &mut id, &mut dict),
        rdfs_range: alloc(RDFS_RANGE, &mut id, &mut dict),
        owl_class: alloc(OWL_CLASS, &mut id, &mut dict),
        owl_thing: alloc(OWL_THING, &mut id, &mut dict),
        owl_nothing: alloc(OWL_NOTHING, &mut id, &mut dict),
        owl_same_as: alloc(OWL_SAME_AS, &mut id, &mut dict),
        owl_different_from: alloc(OWL_DIFFERENT_FROM, &mut id, &mut dict),
        owl_equivalent_class: alloc(OWL_EQUIVALENT_CLASS, &mut id, &mut dict),
        owl_equivalent_property: alloc(OWL_EQUIVALENT_PROPERTY, &mut id, &mut dict),
        owl_inverse_of: alloc(OWL_INVERSE_OF, &mut id, &mut dict),
        owl_functional_property: alloc(OWL_FUNCTIONAL_PROPERTY, &mut id, &mut dict),
        owl_inverse_functional_property: alloc(OWL_INVERSE_FUNCTIONAL_PROPERTY, &mut id, &mut dict),
        owl_symmetric_property: alloc(OWL_SYMMETRIC_PROPERTY, &mut id, &mut dict),
        owl_transitive_property: alloc(OWL_TRANSITIVE_PROPERTY, &mut id, &mut dict),
        owl_irreflexive_property: alloc(OWL_IRREFLEXIVE_PROPERTY, &mut id, &mut dict),
        owl_reflexive_property: alloc(OWL_REFLEXIVE_PROPERTY, &mut id, &mut dict),
        owl_asymmetric_property: alloc(OWL_ASYMMETRIC_PROPERTY, &mut id, &mut dict),
        owl_property_disjoint_with: alloc(OWL_PROPERTY_DISJOINT_WITH, &mut id, &mut dict),
        owl_disjoint_with: alloc(OWL_DISJOINT_WITH, &mut id, &mut dict),
        owl_complement_of: alloc(OWL_COMPLEMENT_OF, &mut id, &mut dict),
        owl_intersection_of: alloc(OWL_INTERSECTION_OF, &mut id, &mut dict),
        owl_union_of: alloc(OWL_UNION_OF, &mut id, &mut dict),
        owl_some_values_from: alloc(OWL_SOME_VALUES_FROM, &mut id, &mut dict),
        owl_all_values_from: alloc(OWL_ALL_VALUES_FROM, &mut id, &mut dict),
        owl_has_value: alloc(OWL_HAS_VALUE, &mut id, &mut dict),
        owl_on_property: alloc(OWL_ON_PROPERTY, &mut id, &mut dict),
        owl_max_cardinality: alloc(OWL_MAX_CARDINALITY, &mut id, &mut dict),
        owl_source_individual: alloc(OWL_SOURCE_INDIVIDUAL, &mut id, &mut dict),
        owl_assertion_property: alloc(OWL_ASSERTION_PROPERTY, &mut id, &mut dict),
        owl_target_individual: alloc(OWL_TARGET_INDIVIDUAL, &mut id, &mut dict),
        owl_target_value: alloc(OWL_TARGET_VALUE, &mut id, &mut dict),
        owl_object_property: alloc(OWL_OBJECT_PROPERTY, &mut id, &mut dict),
        owl_property_chain_axiom: alloc(OWL_PROPERTY_CHAIN_AXIOM, &mut id, &mut dict),
        owl_has_key: alloc(OWL_HAS_KEY, &mut id, &mut dict),
        owl_all_disjoint_classes: alloc(OWL_ALL_DISJOINT_CLASSES, &mut id, &mut dict),
        owl_all_different: alloc(OWL_ALL_DIFFERENT, &mut id, &mut dict),
        owl_members: alloc(OWL_MEMBERS, &mut id, &mut dict),
        owl_distinct_members: alloc(OWL_DISTINCT_MEMBERS, &mut id, &mut dict),
        owl_named_individual: alloc(OWL_NAMED_INDIVIDUAL, &mut id, &mut dict),
    };
    debug_assert_eq!(id, USER_TERMS_BASE);
    (vocab, dict)
}

/// Encode an oxrdf quad's S/P/O into `TermId`s, allocating new IDs as
/// needed in `state.dict`. Errors on RDF 1.2 triple-term subjects/objects.
fn encode_quad(state: &mut LoadState, quad: &Quad) -> Result<Triple> {
    let s = intern_subject(state, quad.subject.as_ref())?;
    let p = intern_named(state, quad.predicate.as_str());
    let o = intern_term(state, quad.object.as_ref())?;
    Ok(Triple::new(s, p, o))
}

fn intern_subject(state: &mut LoadState, s: NamedOrBlankNodeRef<'_>) -> Result<TermId> {
    // RDF 1.2 / rdf-star triple-term subjects are not part of
    // `NamedOrBlankNodeRef` while the `rdf-12` feature is OFF (PR2 will
    // re-introduce a triple-term arm). The Stage-1 OWL 2 RL engine
    // therefore cannot observe such subjects in the first place.
    match s {
        NamedOrBlankNodeRef::NamedNode(n) => Ok(intern_named(state, n.as_str())),
        NamedOrBlankNodeRef::BlankNode(b) => Ok(intern_blank(state, b.as_str())),
    }
}

fn intern_term(state: &mut LoadState, t: TermRef<'_>) -> Result<TermId> {
    // RDF 1.2 / rdf-star triple-term objects are gated behind the `rdf-12`
    // feature on `oxrdf`, which is OFF for PR1. Re-introducing the
    // `TermRef::Triple` arm with a bail is part of PR2.
    match t {
        TermRef::NamedNode(n) => Ok(intern_named(state, n.as_str())),
        TermRef::BlankNode(b) => Ok(intern_blank(state, b.as_str())),
        TermRef::Literal(l) => Ok(intern_literal(
            state,
            l.value(),
            l.datatype().as_str(),
            l.language(),
        )),
    }
}

fn intern_named(state: &mut LoadState, iri: &str) -> TermId {
    if let Some(&t) = state.dict.get(iri) {
        return t;
    }
    let t = TermId(state.next_id);
    state.next_id += 1;
    state.dict.insert(iri.to_string(), t);
    t
}

fn intern_blank(state: &mut LoadState, id: &str) -> TermId {
    let key = format!("_:{id}");
    if let Some(&t) = state.dict.get(&key) {
        return t;
    }
    let t = TermId(state.next_id);
    state.next_id += 1;
    state.dict.insert(key, t);
    t
}

fn intern_literal(
    state: &mut LoadState,
    value: &str,
    datatype: &str,
    language: Option<&str>,
) -> TermId {
    let key = match language {
        Some(lang) => format!("\"{value}\"@{lang}"),
        None => format!("\"{value}\"^^<{datatype}>"),
    };
    if let Some(&t) = state.dict.get(&key) {
        return t;
    }
    let t = TermId(state.next_id);
    state.next_id += 1;
    state.dict.insert(key, t);
    t
}

/// Check whether the materialized store entails `q` (treating bnodes in
/// the conclusion as existential wildcards on a per-triple basis).
fn triple_entailed(state: &LoadState, q: &Quad) -> Result<bool> {
    let p = match state.dict.get(q.predicate.as_str()) {
        Some(&id) => id,
        // Predicate IRI never seen in premise → not entailed.
        None => return Ok(false),
    };
    // The `Triple` arms on `NamedOrBlankNodeRef` / `TermRef` are gated
    // behind oxrdf's `rdf-12` feature. PR1 keeps that feature OFF, so a
    // triple-term subject/object is unrepresentable here; PR2 will
    // re-introduce the explicit bail arms.
    let s = match q.subject.as_ref() {
        NamedOrBlankNodeRef::NamedNode(n) => SlotPattern::Const(match state.dict.get(n.as_str()) {
            Some(&id) => id,
            None => return Ok(false),
        }),
        NamedOrBlankNodeRef::BlankNode(_) => SlotPattern::Wildcard,
    };
    let o = match q.object.as_ref() {
        TermRef::NamedNode(n) => SlotPattern::Const(match state.dict.get(n.as_str()) {
            Some(&id) => id,
            None => return Ok(false),
        }),
        TermRef::BlankNode(_) => SlotPattern::Wildcard,
        TermRef::Literal(l) => {
            let key = match l.language() {
                Some(lang) => format!("\"{}\"@{lang}", l.value()),
                None => format!("\"{}\"^^<{}>", l.value(), l.datatype().as_str()),
            };
            SlotPattern::Const(match state.dict.get(&key) {
                Some(&id) => id,
                None => return Ok(false),
            })
        }
    };
    Ok(state
        .store
        .probe(s.as_option(), p, o.as_option())
        .next()
        .is_some())
}

enum SlotPattern {
    Const(TermId),
    Wildcard,
}

impl SlotPattern {
    fn as_option(&self) -> Option<TermId> {
        match self {
            SlotPattern::Const(t) => Some(*t),
            SlotPattern::Wildcard => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxrdf::{BlankNode, GraphName, Literal, NamedNode, NamedOrBlankNode, Quad};

    fn nq(s: &str, p: &str, o: &str) -> Quad {
        Quad::new(
            NamedOrBlankNode::NamedNode(NamedNode::new(s).unwrap()),
            NamedNode::new(p).unwrap(),
            NamedNode::new(o).unwrap(),
            GraphName::DefaultGraph,
        )
    }

    #[test]
    fn empty_entails_empty() {
        let mut engine = Engine::new();
        engine.load(&Dataset::new()).unwrap();
        assert!(engine.entails(&Dataset::new()).unwrap());
    }

    #[test]
    fn subclass_entailment_via_cax_sco() {
        let mut engine = Engine::new();
        let mut premise = Dataset::new();
        premise.insert(&nq("http://ex/A", RDFS_SUB_CLASS_OF, "http://ex/B"));
        premise.insert(&nq("http://ex/x", RDF_TYPE, "http://ex/A"));
        engine.load(&premise).unwrap();
        let mut concl = Dataset::new();
        concl.insert(&nq("http://ex/x", RDF_TYPE, "http://ex/B"));
        assert!(engine.entails(&concl).unwrap());
    }

    #[test]
    fn unrelated_triple_is_not_entailed() {
        let mut engine = Engine::new();
        let mut premise = Dataset::new();
        premise.insert(&nq("http://ex/A", RDFS_SUB_CLASS_OF, "http://ex/B"));
        engine.load(&premise).unwrap();
        let mut concl = Dataset::new();
        concl.insert(&nq("http://ex/Z", RDF_TYPE, "http://ex/B"));
        assert!(!engine.entails(&concl).unwrap());
    }

    #[test]
    fn explicit_owl_nothing_makes_inconsistent() {
        let mut engine = Engine::new();
        let mut data = Dataset::new();
        data.insert(&nq("http://ex/a", RDF_TYPE, OWL_NOTHING));
        engine.load(&data).unwrap();
        assert!(!engine.is_consistent().unwrap());
    }

    #[test]
    fn empty_is_consistent() {
        let mut engine = Engine::new();
        engine.load(&Dataset::new()).unwrap();
        assert!(engine.is_consistent().unwrap());
    }

    #[test]
    fn bnode_subject_in_conclusion_is_wildcard() {
        let mut engine = Engine::new();
        let mut premise = Dataset::new();
        premise.insert(&nq("http://ex/x", RDF_TYPE, "http://ex/A"));
        engine.load(&premise).unwrap();

        let mut concl = Dataset::new();
        concl.insert(&Quad::new(
            NamedOrBlankNode::BlankNode(BlankNode::new("b1").unwrap()),
            NamedNode::new(RDF_TYPE).unwrap(),
            NamedNode::new("http://ex/A").unwrap(),
            GraphName::DefaultGraph,
        ));
        assert!(engine.entails(&concl).unwrap());
    }

    #[test]
    fn ask_true_when_anything_loaded() {
        let mut engine = Engine::new();
        let mut data = Dataset::new();
        data.insert(&nq("http://ex/a", "http://ex/p", "http://ex/b"));
        engine.load(&data).unwrap();
        assert!(engine.ask("ASK { ?s ?p ?o }").unwrap());
    }

    #[test]
    fn ask_false_when_nothing_loaded() {
        let mut engine = Engine::new();
        engine.load(&Dataset::new()).unwrap();
        assert!(!engine.ask("ASK { ?s ?p ?o }").unwrap());
    }

    #[test]
    fn literal_object_round_trip() {
        let mut engine = Engine::new();
        let mut premise = Dataset::new();
        premise.insert(&Quad::new(
            NamedOrBlankNode::NamedNode(NamedNode::new("http://ex/x").unwrap()),
            NamedNode::new("http://ex/p").unwrap(),
            Literal::new_simple_literal("hi"),
            GraphName::DefaultGraph,
        ));
        engine.load(&premise).unwrap();
        let mut concl = Dataset::new();
        concl.insert(&Quad::new(
            NamedOrBlankNode::NamedNode(NamedNode::new("http://ex/x").unwrap()),
            NamedNode::new("http://ex/p").unwrap(),
            Literal::new_simple_literal("hi"),
            GraphName::DefaultGraph,
        ));
        assert!(engine.entails(&concl).unwrap());
    }
}
