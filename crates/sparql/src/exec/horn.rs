//! `HornBackend` — the storage/WCOJ-backed implementation of the
//! [`Executor`](crate::exec::Executor) + [`Store`](crate::exec::Store)
//! seam (SPEC-07 wiring increment, issue #67).
//!
//! Term identity lives in `horndb_storage::Dictionary` (kind-tagged
//! `TermId`s — fixes the Stage-1 lexical type erasure). BGPs execute on
//! the SPEC-03 Leapfrog Triejoin over a lazily-rebuilt sorted snapshot.

use crate::algebra::Term;
use crate::error::{Result, SparqlError};
use crate::exec::runtime::{literal_parts, unescape_ntriples};
use oxrdf::{BlankNode, Literal, NamedNode, Term as OxTerm};

/// algebra::Term constant -> oxrdf::Term (dictionary key form).
/// Errors on variables and RDF 1.2 triple terms.
///
/// # Literal normalization
///
/// oxrdf applies two normalizations that are consistent on both the data path
/// and the query path (both go through oxrdf), so matching stays correct even
/// though the lexical strings may not be byte-identical to the original input:
///
/// * **`xsd:string` collapsing** — `"v"^^<http://www.w3.org/2001/XMLSchema#string>`
///   round-trips as the plain form `"v"` (RDF 1.1 §3.3 says plain literals and
///   `xsd:string` literals are the same node).
/// * **BCP-47 language-tag lowercasing** — `"x"@EN` round-trips as `"x"@en`.
///
/// Callers that persist or compare the algebra `Term::Literal` form after a
/// round-trip should expect these normalizations rather than byte identity.
#[allow(dead_code)] // used by HornBackend (Task 5/6)
pub(crate) fn algebra_to_oxrdf(t: &Term) -> Result<OxTerm> {
    match t {
        Term::Iri(s) => Ok(OxTerm::NamedNode(NamedNode::new_unchecked(s.clone()))),
        Term::BlankNode(s) => Ok(OxTerm::BlankNode(BlankNode::new_unchecked(s.clone()))),
        Term::Literal(raw) => Ok(OxTerm::Literal(parse_literal(raw))),
        Term::Var(v) => Err(SparqlError::Executor(format!(
            "algebra_to_oxrdf called on variable ?{}",
            v.name()
        ))),
        Term::Triple(_) => Err(SparqlError::Executor(
            "RDF 1.2 triple terms are not supported by the storage backend yet".into(),
        )),
    }
}

/// N-Triples literal lexical form -> oxrdf::Literal.
/// `literal_parts` keeps the value escaped; unescape before building.
#[allow(dead_code)] // used by HornBackend (Task 5/6)
fn parse_literal(raw: &str) -> Literal {
    let (escaped, lang, dt) = literal_parts(raw);
    let value = unescape_ntriples(&escaped);
    match (lang, dt) {
        (Some(lang), _) => Literal::new_language_tagged_literal(&value, lang)
            .unwrap_or_else(|_| Literal::new_simple_literal(value)),
        (None, Some(dt)) => Literal::new_typed_literal(value, NamedNode::new_unchecked(dt)),
        (None, None) => Literal::new_simple_literal(value),
    }
}

/// oxrdf::Term -> algebra::Term, preserving kind (the point of #67).
#[allow(dead_code)] // used by HornBackend (Task 5/6)
pub(crate) fn oxrdf_to_algebra(t: &OxTerm) -> Term {
    match t {
        OxTerm::NamedNode(n) => Term::Iri(n.as_str().to_owned()),
        OxTerm::BlankNode(b) => Term::BlankNode(b.as_str().to_owned()),
        OxTerm::Literal(l) => Term::Literal(l.to_string()),
        // Triple terms never enter the backend (rejected on insert/lookup),
        // so this arm is unreachable in practice; degrade gracefully.
        #[allow(unreachable_patterns)]
        other => Term::Iri(other.to_string()),
    }
}

/// One lexical term in the `Engine::materialized_triples()` convention:
/// leading `"` = literal (N-Triples form), leading `_:` = blank node
/// (prefix stripped), anything else = bare IRI.
#[allow(dead_code)] // used by HornBackend (Task 5/6)
pub(crate) fn lexical_to_oxrdf(s: &str) -> OxTerm {
    if s.starts_with('"') {
        OxTerm::Literal(parse_literal(s))
    } else if let Some(label) = s.strip_prefix("_:") {
        OxTerm::BlankNode(BlankNode::new_unchecked(label))
    } else {
        OxTerm::NamedNode(NamedNode::new_unchecked(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algebra::Var;

    #[test]
    fn literal_round_trips_through_oxrdf() {
        for raw in [
            "\"hello\"",
            "\"hej\"@sv",
            "\"42\"^^<http://www.w3.org/2001/XMLSchema#integer>",
            "\"a \\\"quoted\\\" word\"",
        ] {
            let ox = algebra_to_oxrdf(&Term::Literal(raw.to_owned())).unwrap();
            // xsd:string normalisation: oxrdf may render plain literals
            // identically; the invariant is algebra->oxrdf->algebra fixpoint.
            let back = oxrdf_to_algebra(&ox);
            assert_eq!(back, Term::Literal(raw.to_owned()), "round trip of {raw}");
        }
    }

    #[test]
    fn iri_and_bnode_conventions_match_translate() {
        let iri = algebra_to_oxrdf(&Term::Iri("http://ex/a".into())).unwrap();
        assert_eq!(oxrdf_to_algebra(&iri), Term::Iri("http://ex/a".into()));
        let b = algebra_to_oxrdf(&Term::BlankNode("b0".into())).unwrap();
        assert_eq!(oxrdf_to_algebra(&b), Term::BlankNode("b0".into()));
    }

    #[test]
    fn lexical_convention_covers_owlrl_dump_forms() {
        assert!(matches!(
            lexical_to_oxrdf("http://ex/a"),
            OxTerm::NamedNode(_)
        ));
        match lexical_to_oxrdf("_:b0") {
            OxTerm::BlankNode(b) => assert_eq!(b.as_str(), "b0"),
            other => panic!("expected bnode, got {other:?}"),
        }
        assert!(matches!(lexical_to_oxrdf("\"x\"@en"), OxTerm::Literal(_)));
    }

    #[test]
    fn variables_are_rejected() {
        assert!(algebra_to_oxrdf(&Term::Var(Var::new("x"))).is_err());
    }

    #[test]
    fn explicit_xsd_string_normalizes_to_plain_form() {
        let raw = "\"v\"^^<http://www.w3.org/2001/XMLSchema#string>";
        let ox = algebra_to_oxrdf(&Term::Literal(raw.to_owned())).unwrap();
        assert_eq!(oxrdf_to_algebra(&ox), Term::Literal("\"v\"".to_owned()));
    }
}
