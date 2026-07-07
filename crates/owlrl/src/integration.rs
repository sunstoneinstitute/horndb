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

use anyhow::{anyhow, bail, Result};
use oxrdf::{Dataset, GraphName, NamedOrBlankNodeRef, Quad, TermRef};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::backend::RuleFiringBackend;
use crate::engine::{reset_and_materialize, Stats};
use crate::provenance::ProofTree;
use crate::store::{MemStore, TripleStore};
use crate::types::{MaxCardRestriction, QualMaxCardRestriction, TermId, Triple};
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
const OWL_MAX_QUALIFIED_CARDINALITY: &str = "http://www.w3.org/2002/07/owl#maxQualifiedCardinality";
const OWL_ON_CLASS: &str = "http://www.w3.org/2002/07/owl#onClass";
const OWL_SOURCE_INDIVIDUAL: &str = "http://www.w3.org/2002/07/owl#sourceIndividual";
const OWL_ASSERTION_PROPERTY: &str = "http://www.w3.org/2002/07/owl#assertionProperty";
const OWL_TARGET_INDIVIDUAL: &str = "http://www.w3.org/2002/07/owl#targetIndividual";
const OWL_TARGET_VALUE: &str = "http://www.w3.org/2002/07/owl#targetValue";
const OWL_OBJECT_PROPERTY: &str = "http://www.w3.org/2002/07/owl#ObjectProperty";
const OWL_PROPERTY_CHAIN_AXIOM: &str = "http://www.w3.org/2002/07/owl#propertyChainAxiom";
const OWL_HAS_KEY: &str = "http://www.w3.org/2002/07/owl#hasKey";
const OWL_ALL_DISJOINT_CLASSES: &str = "http://www.w3.org/2002/07/owl#AllDisjointClasses";
const OWL_ALL_DISJOINT_PROPERTIES: &str = "http://www.w3.org/2002/07/owl#AllDisjointProperties";
const OWL_ALL_DIFFERENT: &str = "http://www.w3.org/2002/07/owl#AllDifferent";
const OWL_MEMBERS: &str = "http://www.w3.org/2002/07/owl#members";
const OWL_DISTINCT_MEMBERS: &str = "http://www.w3.org/2002/07/owl#distinctMembers";
const OWL_NAMED_INDIVIDUAL: &str = "http://www.w3.org/2002/07/owl#NamedIndividual";
// SPEC-11 SSSOM mapping vocabulary (F1).
const SKOS_EXACT_MATCH: &str = "http://www.w3.org/2004/02/skos/core#exactMatch";
const SKOS_CLOSE_MATCH: &str = "http://www.w3.org/2004/02/skos/core#closeMatch";
const SKOS_BROAD_MATCH: &str = "http://www.w3.org/2004/02/skos/core#broadMatch";
const SKOS_NARROW_MATCH: &str = "http://www.w3.org/2004/02/skos/core#narrowMatch";
const SKOS_RELATED_MATCH: &str = "http://www.w3.org/2004/02/skos/core#relatedMatch";
const SEMAPV_CROSS_SPECIES_EXACT_MATCH: &str =
    "https://w3id.org/semapv/vocab/crossSpeciesExactMatch";
const SEMAPV_CROSS_SPECIES_NARROW_MATCH: &str =
    "https://w3id.org/semapv/vocab/crossSpeciesNarrowMatch";
const SEMAPV_CROSS_SPECIES_BROAD_MATCH: &str =
    "https://w3id.org/semapv/vocab/crossSpeciesBroadMatch";
const SEMAPV_MAPPING_CHAINING: &str = "https://w3id.org/semapv/vocab/MappingChaining";
const SEMAPV_MAPPING_INVERSION: &str = "https://w3id.org/semapv/vocab/MappingInversion";
const SSSOM_MAPPING: &str = "https://w3id.org/sssom/Mapping";
const SSSOM_SUBJECT_ID: &str = "https://w3id.org/sssom/subject_id";
const SSSOM_PREDICATE_ID: &str = "https://w3id.org/sssom/predicate_id";
const SSSOM_OBJECT_ID: &str = "https://w3id.org/sssom/object_id";
const SSSOM_MAPPING_JUSTIFICATION: &str = "https://w3id.org/sssom/mapping_justification";
const SSSOM_CONFIDENCE: &str = "https://w3id.org/sssom/confidence";
const SSSOM_PREDICATE_MODIFIER: &str = "https://w3id.org/sssom/predicate_modifier";
const SSSOM_DERIVED_FROM: &str = "https://w3id.org/sssom/derived_from";
const HORNDB_NOT_EXACT_MATCH: &str = "https://w3id.org/horndb/internal#notExactMatch";

/// First non-reserved `TermId` value. Vocabulary terms occupy `1..=67`.
const USER_TERMS_BASE: u64 = 68;

/// Stateful OWL 2 RL reasoning façade.
///
/// Each `load` discards prior state and re-materializes from scratch.
/// `entails`, `is_consistent`, and `ask` query the materialized closure.
/// Which [`ClosureBackend`](crate::backend::ClosureBackend) the [`Engine`] uses
/// to close the transitive-closure-shaped rules (`scm-sco`, `scm-spo`, `eq-*`,
/// `prp-trp`). The default is the always-available, GraphBLAS-free
/// [`RuleFiringBackend`]; `GraphBlas` is gated on the `graphblas-backend`
/// feature (SPEC-05, #61) and produces an identical closure (see
/// `crates/owlrl/tests/closure_backend_differential.rs`).
#[derive(Copy, Clone, Default, Debug, Eq, PartialEq)]
pub enum BackendChoice {
    /// In-crate nested-loop rule firing — "slow but obviously correct".
    #[default]
    RuleFiring,
    /// SuiteSparse:GraphBLAS sparse-matrix closure (`horndb-closure`).
    #[cfg(feature = "graphblas-backend")]
    GraphBlas,
}

/// A proof tree with terms decoded back to their lexical forms (the same
/// lexical convention as [`Engine::materialized_triples`]).
///
/// Mirrors [`ProofTree`](crate::ProofTree) with each `(subject, predicate,
/// object)` decoded from `TermId`s into lexical strings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StringProofTree {
    /// A base (asserted) triple, or a triple with no recorded derivation.
    Asserted((String, String, String)),
    /// A derived triple: the rule that produced it and the proofs of its
    /// premises.
    Derived {
        triple: (String, String, String),
        rule_id: String,
        premises: Vec<StringProofTree>,
    },
    /// A derivation cycle, cut to keep the tree finite.
    Cycle((String, String, String)),
}

pub struct Engine {
    vocab: Vocabulary,
    /// Maps a canonical RDF term key (see [`term_key`]) to its dictionary ID.
    /// Pre-populated with the OWL/RDF/RDFS vocabulary IRIs so user data
    /// referencing them gets the same IDs the vocab uses.
    base_dict: FxHashMap<String, TermId>,
    /// Closure backend selection, applied on every [`load`](Self::load).
    backend: BackendChoice,
    /// Materialize statistics (incl. per-phase timings) from the most recent
    /// [`load`](Self::load). `None` until the first load.
    last_stats: Option<Stats>,
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
        Self::with_backend(BackendChoice::default())
    }

    /// Construct an `Engine` that uses the given closure backend on every
    /// [`load`](Self::load). `Engine::new()` is `with_backend(RuleFiring)`.
    pub fn with_backend(backend: BackendChoice) -> Self {
        let (vocab, base_dict) = build_vocab();
        Self {
            vocab,
            base_dict,
            backend,
            last_stats: None,
            state: None,
        }
    }

    /// Materialize statistics — including the per-phase wall-clock attribution
    /// in [`Stats::timings`] — from the most recent [`load`](Self::load).
    /// `None` if nothing has been loaded yet.
    pub fn last_stats(&self) -> Option<&Stats> {
        self.last_stats.as_ref()
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
        // dt-type1 / dt-type2: inject the XSD datatype declarations and
        // subsumption lattice as base axioms. Unconditional — dt-type1's
        // declarations must be present even for an empty premise
        // (WebOnt-I5.8-011). Borrow `dict`/`next_id` disjointly from
        // `store` so the intern closure and the store mutation coexist.
        {
            let LoadState {
                dict,
                next_id,
                store,
                ..
            } = &mut state;
            crate::datatypes::inject_datatype_axioms(store, &self.vocab, |iri| {
                if let Some(&t) = dict.get(iri) {
                    return t;
                }
                let t = TermId(*next_id);
                *next_id += 1;
                dict.insert(iri.to_string(), t);
                t
            });
        }
        // dt-eq / dt-diff / dt-not-type: reason over the *values* of the
        // instance literals now, while the dictionary can still recover each
        // literal's datatype and lexical form. Asserts owl:sameAs /
        // owl:differentFrom / owl:Nothing base facts that the compiled rules
        // (eq-diff1, eq-rep-*) then propagate. Must run after the data is
        // loaded (so all instance literals are present) and after the dt-type
        // axioms (harmless either way — those are datatype IRIs, not literals).
        inject_datatype_literal_axioms(&mut state.store, &self.vocab, &state.dict);
        // Value-space intersection narrowing of `rdfs:range` declarations
        // (WebOnt-I5.8-008/009-pe, #160): a property declared with two or
        // more range datatypes whose value spaces intersect into something
        // narrower than any single declared range gets that narrower range
        // asserted too, so `scm-rng1`/`prp-rng` propagate it during the
        // fixpoint below. See `datatype_ranges.rs` for the value-space
        // model and the "supersets only ⇒ sound" invariant. Borrow
        // `dict`/`next_id` disjointly from `store`, same as the dt-type
        // axiom injection above.
        {
            let LoadState {
                dict,
                next_id,
                store,
                ..
            } = &mut state;
            crate::datatype_ranges::derive_range_intersections(store, &self.vocab, |iri| {
                if let Some(&t) = dict.get(iri) {
                    return t;
                }
                let t = TermId(*next_id);
                *next_id += 1;
                dict.insert(iri.to_string(), t);
                t
            });
        }
        // cls-maxc1/cls-maxc2: classify unqualified max-cardinality
        // restrictions now, while the dictionary can still parse the literal
        // value. The resolved list rides on the store for the firing loop.
        let restrictions = resolve_max_card_restrictions(&state.store, &self.vocab, &state.dict);
        state.store.set_card_restrictions(restrictions);
        let qual_restrictions =
            resolve_qual_max_card_restrictions(&state.store, &self.vocab, &state.dict);
        state.store.set_qual_card_restrictions(qual_restrictions);
        let materialize_once = |store: &mut MemStore| match self.backend {
            BackendChoice::RuleFiring => {
                let mut backend = RuleFiringBackend::new();
                reset_and_materialize(store, &mut backend)
            }
            #[cfg(feature = "graphblas-backend")]
            BackendChoice::GraphBlas => {
                let mut backend = crate::graphblas_backend::GraphBlasBackend::new();
                reset_and_materialize(store, &mut backend)
            }
        };
        let mut stats = materialize_once(&mut state.store);
        // dt-not-type over *derived* datatype memberships: materialization may
        // have typed a literal as a narrower datatype via prp-rng / prp-dom
        // (e.g. `:p rdfs:range xsd:byte` + `:s :p "999"^^xsd:integer` ⇒
        // `"999" rdf:type xsd:byte`). The load-time pass above only validated
        // each literal's *intrinsic* datatype, so re-check the materialized
        // `?lit rdf:type ?D` edges now and assert `?lit rdf:type owl:Nothing`
        // on any value-space violation. If that asserted anything, re-run the
        // fixpoint once so the inconsistency propagates through eq-rep-* (e.g. a
        // named resource `owl:sameAs` the offending literal also becomes
        // `owl:Nothing`); the re-run is a no-op for the common case where the
        // initial materialisation found no derived violations.
        if validate_derived_datatype_memberships(&mut state.store, &self.vocab, &state.dict) {
            stats = materialize_once(&mut state.store);
        }
        self.last_stats = Some(stats);
        self.state = Some(state);
        Ok(())
    }

    /// Total triples in the materialized store after the most recent
    /// [`load`](Self::load) — asserted base plus everything inferred.
    /// `None` if nothing has been loaded yet.
    ///
    /// Walks the store, so this is O(triples); intended for benchmarking
    /// and introspection, not a hot path.
    pub fn materialized_len(&self) -> Option<usize> {
        self.state.as_ref().map(|s| s.store.all_triples().len())
    }

    /// Number of asserted (base) triples ingested by the most recent
    /// [`load`](Self::load), before inference. `None` if never loaded.
    pub fn asserted_len(&self) -> Option<usize> {
        self.state.as_ref().map(|s| s.loaded_count)
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

    /// Return the full materialized triple set (asserted base plus
    /// everything inferred) as lexical `(subject, predicate, object)`
    /// triples, decoded back from the dictionary.
    ///
    /// The lexical forms match the dictionary keys built during
    /// [`load`](Self::load):
    /// - IRIs are bare (no angle brackets), e.g. `http://ex/x`.
    /// - Blank nodes carry the `_:` prefix, e.g. `_:b0`.
    /// - Literals are N-Triples-style, e.g. `"hi"@en` or
    ///   `"42"^^<http://www.w3.org/2001/XMLSchema#integer>`.
    ///
    /// `None` if nothing has been loaded yet. O(triples) — intended for
    /// dumping / benchmarking, not a hot path.
    pub fn materialized_triples(&self) -> Option<Vec<(String, String, String)>> {
        let state = self.state.as_ref()?;
        let rev = invert_dict(&state.dict);
        let mut out = Vec::new();
        for t in state.store.all_triples() {
            let (Some(&s), Some(&p), Some(&o)) = (rev.get(&t.s), rev.get(&t.p), rev.get(&t.o))
            else {
                // A term with no lexical key should not happen — every id
                // is interned through the dict. Skip defensively rather
                // than panic in a serving path.
                continue;
            };
            out.push((s.to_string(), p.to_string(), o.to_string()));
        }
        Some(out)
    }

    /// Proof tree for the materialised triple `(s, p, o)`, given as lexical
    /// keys per [`materialized_triples`](Self::materialized_triples), with
    /// every node's terms decoded back to their lexical forms.
    ///
    /// Returns `None` if nothing has been loaded, if any of `s`/`p`/`o` was
    /// never interned, or if the triple is not present in the materialised
    /// store.
    ///
    /// Builds the full reverse dictionary on each call (O(dict)) — intended
    /// for introspection / debugging, not a hot path.
    pub fn proof(&self, s: &str, p: &str, o: &str) -> Option<StringProofTree> {
        let state = self.state.as_ref()?;
        let sid = *state.dict.get(s)?;
        let pid = *state.dict.get(p)?;
        let oid = *state.dict.get(o)?;
        let triple = Triple::new(sid, pid, oid);
        if !state.store.contains(&triple) {
            return None;
        }
        let rev = invert_dict(&state.dict);
        Some(decode_proof(&state.store.proof_tree(&triple), &rev))
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

/// Invert the dictionary into a `TermId → lexical key` map. The forward dict
/// maps each canonical lexical key to its id; the read paths below (proof
/// decoding, triple/literal/cardinality resolution) need the reverse. Vocabulary
/// IRIs live in the dict too (seeded from `base_dict`), so OWL/RDF/RDFS terms
/// decode correctly.
fn invert_dict(dict: &FxHashMap<String, TermId>) -> FxHashMap<TermId, &str> {
    dict.iter().map(|(lex, &id)| (id, lex.as_str())).collect()
}

/// Decode a [`ProofTree`] into a [`StringProofTree`], mapping each node's
/// `TermId`s through `rev` (the inverted dictionary). A term with no lexical
/// key falls back to `"?"` — defensive only; every id is interned through
/// the dict (mirrors the skip-defensively stance in `materialized_triples`).
fn decode_proof(tree: &ProofTree, rev: &FxHashMap<TermId, &str>) -> StringProofTree {
    let decode = |t: &Triple| -> (String, String, String) {
        let lex = |id: TermId| rev.get(&id).map_or("?", |s| s).to_string();
        (lex(t.s), lex(t.p), lex(t.o))
    };
    match tree {
        ProofTree::Asserted(t) => StringProofTree::Asserted(decode(t)),
        ProofTree::Cycle(t) => StringProofTree::Cycle(decode(t)),
        ProofTree::Derived {
            triple,
            rule_id,
            premises,
        } => StringProofTree::Derived {
            triple: decode(triple),
            rule_id: rule_id.to_string(),
            premises: premises.iter().map(|p| decode_proof(p, rev)).collect(),
        },
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

/// Resolve unqualified max-cardinality restrictions for `cls-maxc1`/`cls-maxc2`.
///
/// Scans `?x owl:maxCardinality ?n`, parses the literal value of `?n` (any
/// XSD integer datatype — the OWL 2 RL/RDF rules write
/// `"0"^^xsd:nonNegativeInteger`, but we accept other integer spellings of
/// the same value), and joins with `?x owl:onProperty ?p`. Only values `0`
/// and `1` are retained — higher cardinalities have no OWL 2 RL rule.
///
/// Runs at load time because the literal value is only recoverable from the
/// dictionary (`TermId → lexical key`); the resolved list then rides on the
/// store through `TripleStore::card_restrictions`.
fn resolve_max_card_restrictions(
    store: &MemStore,
    vocab: &Vocabulary,
    dict: &FxHashMap<String, TermId>,
) -> Vec<MaxCardRestriction> {
    let rev = invert_dict(dict);
    let mut out = Vec::new();
    for card in store.scan_predicate(vocab.owl_max_cardinality) {
        let class = card.s;
        let Some(max) = rev.get(&card.o).and_then(|lex| parse_card_literal(lex)) else {
            continue;
        };
        if max > 1 {
            continue;
        }
        // Join with onProperty (there should be exactly one per restriction).
        for op in store.probe(Some(class), vocab.owl_on_property, None) {
            out.push(MaxCardRestriction {
                class,
                property: op.o,
                max,
            });
        }
    }
    out
}

/// Resolve qualified max-cardinality restrictions for `cls-maxqc1`–`cls-maxqc4`.
///
/// Scans `?x owl:maxQualifiedCardinality ?n`, parses the literal value (reusing
/// `parse_card_literal`; only `0` and `1` have OWL 2 RL rules), then joins with
/// `?x owl:onProperty ?p` and `?x owl:onClass ?c`. The `owl:Thing` filler
/// (cls-maxqc2/maxqc4) is carried through as `filler == vocab.owl_thing`.
fn resolve_qual_max_card_restrictions(
    store: &MemStore,
    vocab: &Vocabulary,
    dict: &FxHashMap<String, TermId>,
) -> Vec<QualMaxCardRestriction> {
    let rev = invert_dict(dict);
    let mut out = Vec::new();
    for card in store.scan_predicate(vocab.owl_max_qualified_cardinality) {
        let class = card.s;
        let Some(max) = rev.get(&card.o).and_then(|lex| parse_card_literal(lex)) else {
            continue;
        };
        if max > 1 {
            continue;
        }
        // Join with onProperty and onClass (one of each per restriction).
        for op in store.probe(Some(class), vocab.owl_on_property, None) {
            for oc in store.probe(Some(class), vocab.owl_on_class, None) {
                out.push(QualMaxCardRestriction {
                    class,
                    property: op.o,
                    filler: oc.o,
                    max,
                });
            }
        }
    }
    out
}

/// Inject the OWL 2 RL literal-value datatype rules (`dt-eq`, `dt-diff`,
/// `dt-not-type`) as base axioms over the literals present in the store.
///
/// Runs at load time — like [`resolve_max_card_restrictions`] — because the
/// datatype and parsed value behind a literal `TermId` are only recoverable
/// from the dictionary (`TermId → lexical key`), which the rule-firing layer
/// cannot see. Stage-1 is insertion-only (SPEC-06), so the literal set and
/// their values do not change across materialisation rounds; asserting the
/// conclusions once as base facts is sound and lets the existing compiled
/// rules (`eq-diff1`, `eq-rep-*`) propagate them.
///
/// For each pair of *comparable* literals (see
/// [`crate::datatype_literals::ValueClass`]):
/// - same value  ⇒ `l1 owl:sameAs l2`        (`dt-eq`)
/// - different value ⇒ `l1 owl:differentFrom l2` (`dt-diff`)
///
/// and for each literal whose lexical form is outside its datatype's value
/// space ⇒ `l rdf:type owl:Nothing` (`dt-not-type`, a global inconsistency).
///
/// Only literals that actually occur as triple **objects** in the loaded data
/// are considered — the datatype declarations injected by
/// [`crate::datatypes`] never appear as objects, so this stays bounded by the
/// instance literals. The pairwise comparison is O(k²) in the number of
/// distinct object literals `k`; conformance graphs carry a handful, so this
/// is not a hot path. A value-space-bucketed pass is a Stage-2 optimisation if
/// `k` ever grows large.
fn inject_datatype_literal_axioms(
    store: &mut MemStore,
    vocab: &Vocabulary,
    dict: &FxHashMap<String, TermId>,
) {
    use crate::datatype_literals::{classify, parse_literal_key, ValueClass};

    let rev = invert_dict(dict);

    // Collect the distinct literal object terms actually present in the data.
    // A term is a literal iff its lexical key parses as one (`"…"^^<…>` or
    // `"…"@lang`). We dedup by TermId so a literal used many times is
    // classified once.
    let mut literals: Vec<(TermId, ValueClass)> = Vec::new();
    let mut seen: FxHashSet<TermId> = FxHashSet::default();
    for t in store.all_triples() {
        let o = t.o;
        if !seen.insert(o) {
            continue;
        }
        let Some(key) = rev.get(&o) else { continue };
        let Some(parsed) = parse_literal_key(key) else {
            continue;
        };
        match classify(&parsed) {
            // Well-typed and placed into a comparable value class.
            Ok(Some(vc)) => literals.push((o, vc)),
            // Well-typed but opaque (user datatype / unhandled value space):
            // no cross-lexical reasoning. Distinct TermIds are distinct
            // lexical keys, so we neither sameAs nor differentFrom them.
            Ok(None) => {}
            // Ill-typed: dt-not-type → global inconsistency on this term.
            Err(()) => {
                store.assert(Triple::new(o, vocab.rdf_type, vocab.owl_nothing));
            }
        }
    }

    // Pairwise dt-eq / dt-diff over comparable literals. Two literals are
    // *comparable* iff their `ValueClass` variants match (e.g. two integers,
    // two strings) — we never compare across disjoint value spaces.
    for i in 0..literals.len() {
        for j in (i + 1)..literals.len() {
            let (a_id, a_vc) = (&literals[i].0, &literals[i].1);
            let (b_id, b_vc) = (&literals[j].0, &literals[j].1);
            if !comparable(a_vc, b_vc) {
                continue;
            }
            if a_vc == b_vc {
                // dt-eq: symmetric `owl:sameAs` (eq-sym in the closure
                // backend will mirror it, but assert both directions so the
                // RuleFiring smoke path is order-independent).
                store.assert(Triple::new(*a_id, vocab.owl_same_as, *b_id));
                store.assert(Triple::new(*b_id, vocab.owl_same_as, *a_id));
            } else {
                // dt-diff: symmetric `owl:differentFrom`.
                store.assert(Triple::new(*a_id, vocab.owl_different_from, *b_id));
                store.assert(Triple::new(*b_id, vocab.owl_different_from, *a_id));
            }
        }
    }
}

/// Post-materialization `dt-not-type` over *derived* datatype memberships.
///
/// Scans the materialized `?lit rdf:type ?D` edges. When `?lit` is a literal
/// and `?D` is an XSD datatype IRI, validate the literal's *intrinsic value*
/// against `?D`'s value space (via [`literal_in_datatype`], which preserves the
/// literal's own datatype/language). A violation — e.g. `"999"^^xsd:integer`
/// typed `xsd:byte` via `prp-rng`, or `"5"^^xsd:string` typed `xsd:integer` —
/// asserts `?lit rdf:type owl:Nothing`.
///
/// Returns `true` iff at least one violation was asserted, so the caller can
/// re-run the fixpoint to propagate the `owl:Nothing` through `eq-rep-*`.
fn validate_derived_datatype_memberships(
    store: &mut MemStore,
    vocab: &Vocabulary,
    dict: &FxHashMap<String, TermId>,
) -> bool {
    use crate::datatype_literals::{literal_in_datatype, parse_literal_key};

    const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

    // TermId → lexical key, for both literal subjects and datatype-IRI objects.
    let rev = invert_dict(dict);

    let mut violations: Vec<TermId> = Vec::new();
    for t in store.scan_predicate(vocab.rdf_type) {
        let (Some(lit_key), Some(dt_iri)) = (rev.get(&t.s), rev.get(&t.o)) else {
            continue;
        };
        // Subject must be a literal; object must be an XSD datatype IRI.
        let Some(parsed) = parse_literal_key(lit_key) else {
            continue;
        };
        let Some(target_local) = dt_iri.strip_prefix(XSD) else {
            continue;
        };
        // Check the literal's *intrinsic* value against the derived datatype's
        // value space (preserving its own datatype/language — re-keying the
        // lexical form under the target would wrongly accept e.g.
        // `"5"^^xsd:string rdf:type xsd:integer`). `Some(false)` is a genuine
        // value-space violation; `None` (undecidable in Stage-1) is left
        // consistent to avoid false inconsistencies.
        if literal_in_datatype(&parsed, target_local) == Some(false) {
            violations.push(t.s);
        }
    }
    let any = !violations.is_empty();
    for lit in violations {
        store.assert(Triple::new(lit, vocab.rdf_type, vocab.owl_nothing));
    }
    any
}

/// Two value classes are comparable (eligible for `dt-eq`/`dt-diff`) iff they
/// are the same variant. Disjoint value spaces (string vs integer) are never
/// declared `differentFrom` — OWL 2 RL `dt-diff` concludes disequality only
/// for literals of a comparable kind, and a string is simply not a number.
fn comparable(
    a: &crate::datatype_literals::ValueClass,
    b: &crate::datatype_literals::ValueClass,
) -> bool {
    use crate::datatype_literals::ValueClass::*;
    matches!(
        (a, b),
        (Integer(_), Integer(_)) | (Boolean(_), Boolean(_)) | (Plain(_), Plain(_))
    )
}

/// XSD integer datatypes accepted for `owl:maxCardinality`. The OWL 2 RL/RDF
/// rules write the cardinality literal as `"0"^^xsd:nonNegativeInteger`; we
/// additionally accept the value-equal integer-tower spellings. Datatypes
/// outside this set (e.g. `xsd:string`, `xsd:decimal`, or a user datatype)
/// are rejected so a numeric *lexical* value under the wrong datatype does
/// not spuriously match the cardinality-literal shape and fire
/// `cls-maxc1`/`cls-maxc2`.
const XSD_CARD_INTEGER_DATATYPES: &[&str] = &[
    "http://www.w3.org/2001/XMLSchema#integer",
    "http://www.w3.org/2001/XMLSchema#nonNegativeInteger",
    "http://www.w3.org/2001/XMLSchema#positiveInteger",
    "http://www.w3.org/2001/XMLSchema#long",
    "http://www.w3.org/2001/XMLSchema#int",
    "http://www.w3.org/2001/XMLSchema#short",
    "http://www.w3.org/2001/XMLSchema#byte",
    "http://www.w3.org/2001/XMLSchema#unsignedLong",
    "http://www.w3.org/2001/XMLSchema#unsignedInt",
    "http://www.w3.org/2001/XMLSchema#unsignedShort",
    "http://www.w3.org/2001/XMLSchema#unsignedByte",
];

/// Parse the integer value out of a dictionary literal key of the form
/// `"<value>"^^<<datatype>>` (see `intern_literal`). Returns `None` for
/// non-literals, language-tagged literals, non-integer datatypes (so a
/// numeric value under e.g. `xsd:string` does not match), or non-integer
/// lexical values.
fn parse_card_literal(lex: &str) -> Option<u8> {
    let rest = lex.strip_prefix('"')?;
    let close = rest.find("\"^^<")?;
    let value = &rest[..close];
    let datatype = rest[close + 4..].strip_suffix('>')?;
    if !XSD_CARD_INTEGER_DATATYPES.contains(&datatype) {
        return None;
    }
    value.parse::<u8>().ok()
}

fn build_vocab() -> (Vocabulary, FxHashMap<String, TermId>) {
    let mut id: u64 = 1;
    let mut dict: FxHashMap<String, TermId> = FxHashMap::default();
    let mut alloc = |iri: &str| -> TermId {
        let t = TermId(id);
        id += 1;
        dict.insert(iri.to_string(), t);
        t
    };
    let vocab = Vocabulary {
        rdf_type: alloc(RDF_TYPE),
        rdf_first: alloc(RDF_FIRST),
        rdf_rest: alloc(RDF_REST),
        rdf_nil: alloc(RDF_NIL),
        rdfs_sub_class_of: alloc(RDFS_SUB_CLASS_OF),
        rdfs_sub_property_of: alloc(RDFS_SUB_PROPERTY_OF),
        rdfs_domain: alloc(RDFS_DOMAIN),
        rdfs_range: alloc(RDFS_RANGE),
        owl_class: alloc(OWL_CLASS),
        owl_thing: alloc(OWL_THING),
        owl_nothing: alloc(OWL_NOTHING),
        owl_same_as: alloc(OWL_SAME_AS),
        owl_different_from: alloc(OWL_DIFFERENT_FROM),
        owl_equivalent_class: alloc(OWL_EQUIVALENT_CLASS),
        owl_equivalent_property: alloc(OWL_EQUIVALENT_PROPERTY),
        owl_inverse_of: alloc(OWL_INVERSE_OF),
        owl_functional_property: alloc(OWL_FUNCTIONAL_PROPERTY),
        owl_inverse_functional_property: alloc(OWL_INVERSE_FUNCTIONAL_PROPERTY),
        owl_symmetric_property: alloc(OWL_SYMMETRIC_PROPERTY),
        owl_transitive_property: alloc(OWL_TRANSITIVE_PROPERTY),
        owl_irreflexive_property: alloc(OWL_IRREFLEXIVE_PROPERTY),
        owl_reflexive_property: alloc(OWL_REFLEXIVE_PROPERTY),
        owl_asymmetric_property: alloc(OWL_ASYMMETRIC_PROPERTY),
        owl_property_disjoint_with: alloc(OWL_PROPERTY_DISJOINT_WITH),
        owl_disjoint_with: alloc(OWL_DISJOINT_WITH),
        owl_complement_of: alloc(OWL_COMPLEMENT_OF),
        owl_intersection_of: alloc(OWL_INTERSECTION_OF),
        owl_union_of: alloc(OWL_UNION_OF),
        owl_some_values_from: alloc(OWL_SOME_VALUES_FROM),
        owl_all_values_from: alloc(OWL_ALL_VALUES_FROM),
        owl_has_value: alloc(OWL_HAS_VALUE),
        owl_on_property: alloc(OWL_ON_PROPERTY),
        owl_max_cardinality: alloc(OWL_MAX_CARDINALITY),
        owl_max_qualified_cardinality: alloc(OWL_MAX_QUALIFIED_CARDINALITY),
        owl_on_class: alloc(OWL_ON_CLASS),
        owl_source_individual: alloc(OWL_SOURCE_INDIVIDUAL),
        owl_assertion_property: alloc(OWL_ASSERTION_PROPERTY),
        owl_target_individual: alloc(OWL_TARGET_INDIVIDUAL),
        owl_target_value: alloc(OWL_TARGET_VALUE),
        owl_object_property: alloc(OWL_OBJECT_PROPERTY),
        owl_property_chain_axiom: alloc(OWL_PROPERTY_CHAIN_AXIOM),
        owl_has_key: alloc(OWL_HAS_KEY),
        owl_all_disjoint_classes: alloc(OWL_ALL_DISJOINT_CLASSES),
        owl_all_disjoint_properties: alloc(OWL_ALL_DISJOINT_PROPERTIES),
        owl_all_different: alloc(OWL_ALL_DIFFERENT),
        owl_members: alloc(OWL_MEMBERS),
        owl_distinct_members: alloc(OWL_DISTINCT_MEMBERS),
        owl_named_individual: alloc(OWL_NAMED_INDIVIDUAL),
        skos_exact_match: alloc(SKOS_EXACT_MATCH),
        skos_close_match: alloc(SKOS_CLOSE_MATCH),
        skos_broad_match: alloc(SKOS_BROAD_MATCH),
        skos_narrow_match: alloc(SKOS_NARROW_MATCH),
        skos_related_match: alloc(SKOS_RELATED_MATCH),
        semapv_cross_species_exact_match: alloc(SEMAPV_CROSS_SPECIES_EXACT_MATCH),
        semapv_cross_species_narrow_match: alloc(SEMAPV_CROSS_SPECIES_NARROW_MATCH),
        semapv_cross_species_broad_match: alloc(SEMAPV_CROSS_SPECIES_BROAD_MATCH),
        semapv_mapping_chaining: alloc(SEMAPV_MAPPING_CHAINING),
        semapv_mapping_inversion: alloc(SEMAPV_MAPPING_INVERSION),
        sssom_mapping: alloc(SSSOM_MAPPING),
        sssom_subject_id: alloc(SSSOM_SUBJECT_ID),
        sssom_predicate_id: alloc(SSSOM_PREDICATE_ID),
        sssom_object_id: alloc(SSSOM_OBJECT_ID),
        sssom_mapping_justification: alloc(SSSOM_MAPPING_JUSTIFICATION),
        sssom_confidence: alloc(SSSOM_CONFIDENCE),
        sssom_predicate_modifier: alloc(SSSOM_PREDICATE_MODIFIER),
        sssom_derived_from: alloc(SSSOM_DERIVED_FROM),
        horndb_not_exact_match: alloc(HORNDB_NOT_EXACT_MATCH),
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
    // RDF 1.2's data model keeps subjects as the 1.1-shaped
    // `NamedOrBlankNodeRef`; triple terms appear only as objects. The
    // match is exhaustive even with `oxrdf/rdf-12` enabled.
    match s {
        NamedOrBlankNodeRef::NamedNode(n) => Ok(intern_named(state, n.as_str())),
        NamedOrBlankNodeRef::BlankNode(b) => Ok(intern_blank(state, b.as_str())),
    }
}

fn intern_term(state: &mut LoadState, t: TermRef<'_>) -> Result<TermId> {
    match t {
        TermRef::NamedNode(n) => Ok(intern_named(state, n.as_str())),
        TermRef::BlankNode(b) => Ok(intern_blank(state, b.as_str())),
        TermRef::Literal(l) => Ok(intern_literal(
            state,
            l.value(),
            l.datatype().as_str(),
            l.language(),
        )),
        // SPEC-04 §7 + crates/owlrl/AGENTS.md §7: the Stage-1 OWL 2 RL
        // engine rejects RDF 1.2 triple-term inputs. Triple-term semantics
        // for entailment (reified rules, sameTerm/sameAs on triple terms)
        // are a Stage-2 question; until then any premise carrying a triple
        // term should fail loudly rather than be silently dropped.
        TermRef::Triple(_) => {
            bail!("RDF 1.2 triple-term object is not supported by the Stage-1 OWL 2 RL engine")
        }
    }
}

/// Look up `key` in the dictionary, allocating a fresh consecutive `TermId` on
/// first sight. Building the canonical key (IRI / `_:bnode` / literal form) is
/// the caller's responsibility.
fn intern_key(state: &mut LoadState, key: &str) -> TermId {
    if let Some(&t) = state.dict.get(key) {
        return t;
    }
    let t = TermId(state.next_id);
    state.next_id += 1;
    state.dict.insert(key.to_string(), t);
    t
}

fn intern_named(state: &mut LoadState, iri: &str) -> TermId {
    intern_key(state, iri)
}

fn intern_blank(state: &mut LoadState, id: &str) -> TermId {
    intern_key(state, &format!("_:{id}"))
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
    intern_key(state, &key)
}

/// Check whether the materialized store entails `q` (treating bnodes in
/// the conclusion as existential wildcards on a per-triple basis).
fn triple_entailed(state: &LoadState, q: &Quad) -> Result<bool> {
    let p = match state.dict.get(q.predicate.as_str()) {
        Some(&id) => id,
        // Predicate IRI never seen in premise → not entailed.
        None => return Ok(false),
    };
    // RDF 1.2 keeps subjects as `NamedOrBlankNodeRef`; the object-side
    // `TermRef::Triple` arm is handled at the end of the `match` below
    // (SPEC-04 §7 — triple terms in conclusions are not entailed by the
    // Stage-1 engine).
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
        // SPEC-04 §7: triple-term objects in conclusion graphs imply
        // entailment over RDF 1.2 semantics, which the Stage-1 engine
        // does not implement. Fail loudly rather than silently report
        // "not entailed" — that would mask test bugs.
        TermRef::Triple(_) => {
            bail!(
                "RDF 1.2 triple-term in conclusion is not supported by the Stage-1 OWL 2 RL engine"
            )
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

    const XSD_NNI: &str = "http://www.w3.org/2001/XMLSchema#nonNegativeInteger";
    const OWL_MAX_CARDINALITY_IRI: &str = "http://www.w3.org/2002/07/owl#maxCardinality";
    const OWL_ON_PROPERTY_IRI: &str = "http://www.w3.org/2002/07/owl#onProperty";

    fn nq_card(s: &str, value: &str) -> Quad {
        Quad::new(
            NamedOrBlankNode::NamedNode(NamedNode::new(s).unwrap()),
            NamedNode::new(OWL_MAX_CARDINALITY_IRI).unwrap(),
            Literal::new_typed_literal(value, NamedNode::new(XSD_NNI).unwrap()),
            GraphName::DefaultGraph,
        )
    }

    fn nq_on_prop(s: &str, p: &str) -> Quad {
        Quad::new(
            NamedOrBlankNode::NamedNode(NamedNode::new(s).unwrap()),
            NamedNode::new(OWL_ON_PROPERTY_IRI).unwrap(),
            NamedNode::new(p).unwrap(),
            GraphName::DefaultGraph,
        )
    }

    const OWL_MAX_QUALIFIED_CARDINALITY_IRI: &str =
        "http://www.w3.org/2002/07/owl#maxQualifiedCardinality";
    const OWL_ON_CLASS_IRI: &str = "http://www.w3.org/2002/07/owl#onClass";

    fn nq_qual_card(s: &str, value: &str) -> Quad {
        Quad::new(
            NamedOrBlankNode::NamedNode(NamedNode::new(s).unwrap()),
            NamedNode::new(OWL_MAX_QUALIFIED_CARDINALITY_IRI).unwrap(),
            Literal::new_typed_literal(value, NamedNode::new(XSD_NNI).unwrap()),
            GraphName::DefaultGraph,
        )
    }

    fn nq_on_class(s: &str, c: &str) -> Quad {
        Quad::new(
            NamedOrBlankNode::NamedNode(NamedNode::new(s).unwrap()),
            NamedNode::new(OWL_ON_CLASS_IRI).unwrap(),
            NamedNode::new(c).unwrap(),
            GraphName::DefaultGraph,
        )
    }

    #[test]
    fn cls_maxc1_makes_inconsistent_via_engine() {
        let mut engine = Engine::new();
        let mut data = Dataset::new();
        // :R maxCardinality 0 onProperty :p ; :u a :R ; :u :p :y
        data.insert(&nq_card("http://ex/R", "0"));
        data.insert(&nq_on_prop("http://ex/R", "http://ex/p"));
        data.insert(&nq("http://ex/u", RDF_TYPE, "http://ex/R"));
        data.insert(&nq("http://ex/u", "http://ex/p", "http://ex/y"));
        engine.load(&data).unwrap();
        assert!(
            !engine.is_consistent().unwrap(),
            "maxCardinality 0 with a value ⇒ inconsistent (cls-maxc1)"
        );
    }

    #[test]
    fn prp_adp_makes_inconsistent_via_engine() {
        let mut engine = Engine::new();
        let mut data = Dataset::new();
        // _:adp a owl:AllDisjointProperties ; owl:members (:p1 :p2) .
        // :u :p1 :w ; :u :p2 :w  ⇒ inconsistent (shared (u, w) pair).
        data.insert(&nq("http://ex/adp", RDF_TYPE, OWL_ALL_DISJOINT_PROPERTIES));
        data.insert(&nq("http://ex/adp", OWL_MEMBERS, "http://ex/l1"));
        data.insert(&nq("http://ex/l1", RDF_FIRST, "http://ex/p1"));
        data.insert(&nq("http://ex/l1", RDF_REST, "http://ex/l2"));
        data.insert(&nq("http://ex/l2", RDF_FIRST, "http://ex/p2"));
        data.insert(&nq("http://ex/l2", RDF_REST, RDF_NIL));
        data.insert(&nq("http://ex/u", "http://ex/p1", "http://ex/w"));
        data.insert(&nq("http://ex/u", "http://ex/p2", "http://ex/w"));
        engine.load(&data).unwrap();
        assert!(
            !engine.is_consistent().unwrap(),
            "AllDisjointProperties with a shared (u, w) pair ⇒ inconsistent (prp-adp)"
        );
    }

    #[test]
    fn prp_adp_distinct_objects_consistent_via_engine() {
        let mut engine = Engine::new();
        let mut data = Dataset::new();
        // Same disjoint-properties axiom, but :u relates to distinct objects —
        // the W3C `DisjointObjectProperties-*-cons` shape; must stay consistent.
        data.insert(&nq("http://ex/adp", RDF_TYPE, OWL_ALL_DISJOINT_PROPERTIES));
        data.insert(&nq("http://ex/adp", OWL_MEMBERS, "http://ex/l1"));
        data.insert(&nq("http://ex/l1", RDF_FIRST, "http://ex/p1"));
        data.insert(&nq("http://ex/l1", RDF_REST, "http://ex/l2"));
        data.insert(&nq("http://ex/l2", RDF_FIRST, "http://ex/p2"));
        data.insert(&nq("http://ex/l2", RDF_REST, RDF_NIL));
        data.insert(&nq("http://ex/u", "http://ex/p1", "http://ex/w1"));
        data.insert(&nq("http://ex/u", "http://ex/p2", "http://ex/w2"));
        engine.load(&data).unwrap();
        assert!(
            engine.is_consistent().unwrap(),
            "AllDisjointProperties with distinct objects ⇒ consistent"
        );
    }

    #[test]
    fn cls_maxc2_entails_sameas_via_engine() {
        let mut engine = Engine::new();
        let mut data = Dataset::new();
        // :R maxCardinality 1 onProperty :p ; :u a :R ; :u :p :y1 ; :u :p :y2
        data.insert(&nq_card("http://ex/R", "1"));
        data.insert(&nq_on_prop("http://ex/R", "http://ex/p"));
        data.insert(&nq("http://ex/u", RDF_TYPE, "http://ex/R"));
        data.insert(&nq("http://ex/u", "http://ex/p", "http://ex/y1"));
        data.insert(&nq("http://ex/u", "http://ex/p", "http://ex/y2"));
        engine.load(&data).unwrap();
        let mut concl = Dataset::new();
        concl.insert(&nq("http://ex/y1", OWL_SAME_AS, "http://ex/y2"));
        assert!(
            engine.entails(&concl).unwrap(),
            "maxCardinality 1 with two values ⇒ y1 owl:sameAs y2 (cls-maxc2)"
        );
    }

    #[test]
    fn cls_maxqc3_entails_sameas_via_engine() {
        let mut engine = Engine::new();
        let mut data = Dataset::new();
        // :R maxQualifiedCardinality 1 onProperty :p onClass :c ;
        // :u a :R ; :u :p :y1 ; :u :p :y2 ; :y1 a :c ; :y2 a :c
        data.insert(&nq_qual_card("http://ex/R", "1"));
        data.insert(&nq_on_prop("http://ex/R", "http://ex/p"));
        data.insert(&nq_on_class("http://ex/R", "http://ex/c"));
        data.insert(&nq("http://ex/u", RDF_TYPE, "http://ex/R"));
        data.insert(&nq("http://ex/u", "http://ex/p", "http://ex/y1"));
        data.insert(&nq("http://ex/u", "http://ex/p", "http://ex/y2"));
        data.insert(&nq("http://ex/y1", RDF_TYPE, "http://ex/c"));
        data.insert(&nq("http://ex/y2", RDF_TYPE, "http://ex/c"));
        engine.load(&data).unwrap();
        let mut concl = Dataset::new();
        concl.insert(&nq("http://ex/y1", OWL_SAME_AS, "http://ex/y2"));
        assert!(
            engine.entails(&concl).unwrap(),
            "maxQualifiedCardinality 1 with two typed values ⇒ y1 owl:sameAs y2 (cls-maxqc3)"
        );
    }

    #[test]
    fn cls_maxqc1_makes_inconsistent_via_engine() {
        let mut engine = Engine::new();
        let mut data = Dataset::new();
        // :R maxQualifiedCardinality 0 onProperty :p onClass :c ;
        // :u a :R ; :u :p :y ; :y a :c
        data.insert(&nq_qual_card("http://ex/R", "0"));
        data.insert(&nq_on_prop("http://ex/R", "http://ex/p"));
        data.insert(&nq_on_class("http://ex/R", "http://ex/c"));
        data.insert(&nq("http://ex/u", RDF_TYPE, "http://ex/R"));
        data.insert(&nq("http://ex/u", "http://ex/p", "http://ex/y"));
        data.insert(&nq("http://ex/y", RDF_TYPE, "http://ex/c"));
        engine.load(&data).unwrap();
        assert!(
            !engine.is_consistent().unwrap(),
            "maxQualifiedCardinality 0 with a typed value ⇒ inconsistent (cls-maxqc1)"
        );
    }

    #[test]
    fn max_cardinality_two_is_ignored() {
        // Only 0 and 1 are acted on; maxCardinality 2 is a no-op in Stage-1.
        let mut engine = Engine::new();
        let mut data = Dataset::new();
        data.insert(&nq_card("http://ex/R", "2"));
        data.insert(&nq_on_prop("http://ex/R", "http://ex/p"));
        data.insert(&nq("http://ex/u", RDF_TYPE, "http://ex/R"));
        data.insert(&nq("http://ex/u", "http://ex/p", "http://ex/y"));
        engine.load(&data).unwrap();
        assert!(engine.is_consistent().unwrap());
    }

    #[test]
    fn parse_card_literal_handles_integer_spellings() {
        assert_eq!(
            super::parse_card_literal(
                "\"0\"^^<http://www.w3.org/2001/XMLSchema#nonNegativeInteger>"
            ),
            Some(0)
        );
        assert_eq!(
            super::parse_card_literal("\"1\"^^<http://www.w3.org/2001/XMLSchema#integer>"),
            Some(1)
        );
        assert_eq!(
            super::parse_card_literal("\"2\"^^<http://www.w3.org/2001/XMLSchema#integer>"),
            Some(2)
        );
        // Not a literal key.
        assert_eq!(super::parse_card_literal("http://ex/x"), None);
        // Language-tagged literal — no `^^<…>` suffix.
        assert_eq!(super::parse_card_literal("\"hi\"@en"), None);
        // Non-integer lexical value.
        assert_eq!(
            super::parse_card_literal("\"x\"^^<http://www.w3.org/2001/XMLSchema#string>"),
            None
        );
        // Numeric lexical value but a NON-integer datatype must be rejected,
        // else a `"1"^^xsd:string` literal would spuriously fire cls-maxc.
        assert_eq!(
            super::parse_card_literal("\"1\"^^<http://www.w3.org/2001/XMLSchema#string>"),
            None
        );
        assert_eq!(
            super::parse_card_literal("\"0\"^^<http://www.w3.org/2001/XMLSchema#decimal>"),
            None
        );
        // A user/custom datatype with a numeric value is likewise rejected.
        assert_eq!(
            super::parse_card_literal("\"1\"^^<http://example.org/myType>"),
            None
        );
    }

    #[test]
    fn max_cardinality_one_with_string_datatype_is_ignored() {
        // A `"1"^^xsd:string`-typed maxCardinality is not the OWL 2 RL
        // cardinality-literal shape; it must NOT entail owl:sameAs.
        let mut engine = Engine::new();
        let mut data = Dataset::new();
        data.insert(&Quad::new(
            NamedOrBlankNode::NamedNode(NamedNode::new("http://ex/R").unwrap()),
            NamedNode::new(OWL_MAX_CARDINALITY_IRI).unwrap(),
            Literal::new_typed_literal(
                "1",
                NamedNode::new("http://www.w3.org/2001/XMLSchema#string").unwrap(),
            ),
            GraphName::DefaultGraph,
        ));
        data.insert(&nq_on_prop("http://ex/R", "http://ex/p"));
        data.insert(&nq("http://ex/u", RDF_TYPE, "http://ex/R"));
        data.insert(&nq("http://ex/u", "http://ex/p", "http://ex/y1"));
        data.insert(&nq("http://ex/u", "http://ex/p", "http://ex/y2"));
        engine.load(&data).unwrap();
        let mut concl = Dataset::new();
        concl.insert(&nq("http://ex/y1", OWL_SAME_AS, "http://ex/y2"));
        assert!(
            !engine.entails(&concl).unwrap(),
            "maxCardinality \"1\"^^xsd:string must not fire cls-maxc2"
        );
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
    fn materialized_triples_includes_inferred() {
        let mut engine = Engine::new();
        let mut premise = Dataset::new();
        premise.insert(&nq("http://ex/A", RDFS_SUB_CLASS_OF, "http://ex/B"));
        premise.insert(&nq("http://ex/x", RDF_TYPE, "http://ex/A"));
        engine.load(&premise).unwrap();
        let triples = engine.materialized_triples().unwrap();
        // Asserted base survives.
        assert!(triples.contains(&(
            "http://ex/x".to_string(),
            RDF_TYPE.to_string(),
            "http://ex/A".to_string(),
        )));
        // Inferred `:x a :B` shows up (cax-sco).
        assert!(triples.contains(&(
            "http://ex/x".to_string(),
            RDF_TYPE.to_string(),
            "http://ex/B".to_string(),
        )));
    }

    #[test]
    fn materialized_triples_none_before_load() {
        let engine = Engine::new();
        assert!(engine.materialized_triples().is_none());
    }

    #[test]
    fn materialized_triples_round_trips_literals() {
        let mut engine = Engine::new();
        let mut premise = Dataset::new();
        premise.insert(&Quad::new(
            NamedOrBlankNode::NamedNode(NamedNode::new("http://ex/x").unwrap()),
            NamedNode::new("http://ex/p").unwrap(),
            Literal::new_simple_literal("hi"),
            GraphName::DefaultGraph,
        ));
        engine.load(&premise).unwrap();
        let triples = engine.materialized_triples().unwrap();
        // Simple literal decodes with the xsd:string datatype suffix.
        assert!(triples.iter().any(|(s, p, o)| {
            s == "http://ex/x" && p == "http://ex/p" && o.starts_with("\"hi\"")
        }));
    }

    fn leaves_all_asserted(tree: &StringProofTree) -> bool {
        match tree {
            StringProofTree::Asserted(_) | StringProofTree::Cycle(_) => true,
            StringProofTree::Derived { premises, .. } => {
                // An empty-premise Derived is treated as a leaf (acceptable).
                premises.iter().all(leaves_all_asserted)
            }
        }
    }

    #[test]
    fn engine_proof_decodes_multistep_derivation() {
        let mut engine = Engine::new();
        let mut data = Dataset::new();
        // c1 ⊑ c2, c2 ⊑ c3, x a c1  ⇒  x a c3 (two-step scm-sco + cax-sco).
        data.insert(&nq("http://ex/c1", RDFS_SUB_CLASS_OF, "http://ex/c2"));
        data.insert(&nq("http://ex/c2", RDFS_SUB_CLASS_OF, "http://ex/c3"));
        data.insert(&nq("http://ex/x", RDF_TYPE, "http://ex/c1"));
        engine.load(&data).unwrap();

        let proof = engine
            .proof("http://ex/x", RDF_TYPE, "http://ex/c3")
            .expect("x a c3 should be in the store with a proof");
        match &proof {
            StringProofTree::Derived {
                triple, premises, ..
            } => {
                assert_eq!(
                    *triple,
                    (
                        "http://ex/x".to_string(),
                        RDF_TYPE.to_string(),
                        "http://ex/c3".to_string(),
                    )
                );
                assert!(!premises.is_empty(), "derivation should have premises");
            }
            other => panic!("expected Derived for x a c3, got {other:?}"),
        }
        assert!(
            leaves_all_asserted(&proof),
            "every leaf of the proof tree should be Asserted/Cycle"
        );
    }

    #[test]
    fn engine_proof_none_before_load_and_for_absent() {
        let engine = Engine::new();
        assert!(engine
            .proof("http://ex/x", RDF_TYPE, "http://ex/c3")
            .is_none());

        let mut engine = Engine::new();
        let mut data = Dataset::new();
        data.insert(&nq("http://ex/x", RDF_TYPE, "http://ex/c1"));
        engine.load(&data).unwrap();
        // Predicate/term never interned, or triple simply absent.
        assert!(engine
            .proof("http://ex/x", RDF_TYPE, "http://ex/absent")
            .is_none());
        assert!(engine
            .proof("http://ex/x", "http://ex/never", "http://ex/c1")
            .is_none());
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
