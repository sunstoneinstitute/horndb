//! Schemas bundled with HornDB, loadable into the reasoning spine by name
//! (`[reasoning].builtin_schemas`).
//!
//! Each schema is a list of lexical triples in the [`Engine::load_base`]
//! convention (bare IRIs), so loading one needs no RDF parser.
//!
//! [`Engine::load_base`]: crate::integration::Engine::load_base

const SKOS: &str = "http://www.w3.org/2004/02/skos/core#";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
const RDFS_SUB_CLASS_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";
const RDFS_SUB_PROPERTY_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subPropertyOf";
const OWL_INVERSE_OF: &str = "http://www.w3.org/2002/07/owl#inverseOf";
const OWL_DISJOINT_WITH: &str = "http://www.w3.org/2002/07/owl#disjointWith";
const OWL_SYMMETRIC: &str = "http://www.w3.org/2002/07/owl#SymmetricProperty";
const OWL_TRANSITIVE: &str = "http://www.w3.org/2002/07/owl#TransitiveProperty";

/// `skos-core`: the concept-hierarchy axioms of the W3C SKOS RDF schema
/// (<http://www.w3.org/2004/02/skos/core>), trimmed in two ways:
///
/// - **No mapping predicates.** `skos.rdf` makes `broadMatch ⊑ broader`,
///   `relatedMatch ⊑ related` and `mappingRelation ⊑ semanticRelation`. The
///   SSSOM rule RG2 turns every `rdfs:subClassOf` into `broadMatch`, so those
///   links would put every class hierarchy into `skos:broader`. Mapping edges
///   stay in the mapping layer (ADR-0017); the SSSOM rules in `rules.toml`
///   own their semantics.
/// - **No `rdfs:domain` / `rdfs:range`.** `semanticRelation`'s domain and
///   range `skos:Concept` would type every related term as a concept.
pub fn skos_core() -> Vec<(String, String, String)> {
    let s = |local: &str| format!("{SKOS}{local}");
    let mut out = Vec::new();
    let mut add = |subj: String, pred: &str, obj: String| out.push((subj, pred.to_string(), obj));

    for (a, b) in [
        ("broader", "narrower"),
        ("broaderTransitive", "narrowerTransitive"),
        ("hasTopConcept", "topConceptOf"),
    ] {
        add(s(a), OWL_INVERSE_OF, s(b));
    }
    for p in ["broaderTransitive", "narrowerTransitive"] {
        add(s(p), RDF_TYPE, OWL_TRANSITIVE.to_string());
    }
    add(s("related"), RDF_TYPE, OWL_SYMMETRIC.to_string());

    for (sub, sup) in [
        ("broader", "broaderTransitive"),
        ("narrower", "narrowerTransitive"),
        ("broaderTransitive", "semanticRelation"),
        ("narrowerTransitive", "semanticRelation"),
        ("related", "semanticRelation"),
        ("topConceptOf", "inScheme"),
        ("changeNote", "note"),
        ("definition", "note"),
        ("editorialNote", "note"),
        ("example", "note"),
        ("historyNote", "note"),
        ("scopeNote", "note"),
    ] {
        add(s(sub), RDFS_SUB_PROPERTY_OF, s(sup));
    }
    for p in ["prefLabel", "altLabel", "hiddenLabel"] {
        add(s(p), RDFS_SUB_PROPERTY_OF, RDFS_LABEL.to_string());
    }

    add(s("OrderedCollection"), RDFS_SUB_CLASS_OF, s("Collection"));
    for (a, b) in [
        ("Collection", "Concept"),
        ("Collection", "ConceptScheme"),
        ("ConceptScheme", "Concept"),
    ] {
        add(s(a), OWL_DISJOINT_WITH, s(b));
    }
    out
}
