//! Faithful RDF term <-> lexical-form conversion.
//!
//! The SPEC-07 `MemStore` keeps every triple as `(String, String, String)`
//! in N-Triples lexical form and recovers only the IRI-vs-literal distinction
//! on read (`exec::classify_lexical`). To present an rdflib-compatible surface
//! we need to round-trip the *kind* of every term — including blank nodes,
//! which the store would otherwise surface as IRIs.
//!
//! Strategy: encode each term into a canonical N-Triples lexical string when
//! writing to the store, and re-parse that exact lexical string back into a
//! [`RdfTerm`] when reading. Blank nodes are stored with their `_:` prefix so
//! they re-parse as blank nodes rather than IRIs; literals are stored in full
//! N-Triples form (`"v"`, `"v"@lang`, `"v"^^<dt>`) exactly as the SPARQL
//! update path already does. This module owns that codec and has no PyO3
//! dependency, so it is unit-testable with a plain `cargo test`.

use oxrdf::{Literal, NamedNode};

/// A faithful, kind-preserving RDF term used at the binding boundary.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RdfTerm {
    Iri(String),
    /// Blank-node *label* (without the `_:` prefix).
    Blank(String),
    Literal {
        value: String,
        /// `xsd:string` is represented as `None` to match rdflib, which omits
        /// the implicit datatype for plain string literals.
        datatype: Option<String>,
        language: Option<String>,
    },
}

/// The well-known IRI for `xsd:string`, rdflib's implicit plain-literal type.
pub const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
/// The well-known IRI for `rdf:langString`, the implicit type of a lang-tagged
/// literal.
pub const RDF_LANGSTRING: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#langString";

impl RdfTerm {
    pub fn iri(s: impl Into<String>) -> Self {
        RdfTerm::Iri(s.into())
    }

    pub fn blank(label: impl Into<String>) -> Self {
        RdfTerm::Blank(label.into())
    }

    /// A plain (xsd:string) literal.
    pub fn plain_literal(value: impl Into<String>) -> Self {
        RdfTerm::Literal {
            value: value.into(),
            datatype: None,
            language: None,
        }
    }

    /// Build a literal, normalising the rdflib conventions: an `xsd:string`
    /// datatype collapses to `None` (plain literal), and a language tag wins
    /// over an explicit datatype (the term is then an `rdf:langString`).
    pub fn literal(
        value: impl Into<String>,
        datatype: Option<String>,
        language: Option<String>,
    ) -> Self {
        let value = value.into();
        if let Some(lang) = language {
            return RdfTerm::Literal {
                value,
                datatype: None,
                language: Some(lang.to_ascii_lowercase()),
            };
        }
        let datatype = match datatype {
            Some(dt) if dt == XSD_STRING => None,
            other => other,
        };
        RdfTerm::Literal {
            value,
            datatype,
            language: None,
        }
    }

    /// The effective datatype IRI as rdflib reports it: `xsd:string` for a
    /// plain literal, `rdf:langString` for a language-tagged literal, or the
    /// explicit datatype otherwise. IRIs and blank nodes have no datatype.
    pub fn effective_datatype(&self) -> Option<&str> {
        match self {
            RdfTerm::Literal {
                language: Some(_), ..
            } => Some(RDF_LANGSTRING),
            RdfTerm::Literal {
                datatype: Some(dt), ..
            } => Some(dt),
            RdfTerm::Literal { .. } => Some(XSD_STRING),
            _ => None,
        }
    }

    /// Encode to the lexical form the SPEC-07 `MemStore` persists.
    ///
    /// This MUST match the SPARQL write path (`update::subject_to_term` etc.)
    /// so the binding and SPARQL queries/updates share one store coherently:
    ///   * IRIs are stored **bare** (no angle brackets), as `n.as_str()`;
    ///   * literals are stored in full N-Triples quoted form (`l.to_string()`);
    ///   * blank nodes are stored with a `_:` prefix. The SPARQL path stores a
    ///     bare blank label (which `exec::classify_lexical` then mis-reads as
    ///     an IRI); the binding adds the `_:` so `from_store_lexical` can
    ///     recover the blank-node kind faithfully (SPEC-10 F1). Blank nodes
    ///     written by the binding therefore round-trip; blank nodes arriving
    ///     via SPARQL `INSERT DATA` keep the legacy bare-label behaviour.
    pub fn to_store_lexical(&self) -> String {
        match self {
            // Bare IRI string — matches `NamedNode::as_str()` used by the
            // SPARQL update path. Angle brackets would break query matching.
            RdfTerm::Iri(iri) => iri.clone(),
            RdfTerm::Blank(label) => format!("_:{label}"),
            RdfTerm::Literal {
                value,
                datatype,
                language,
            } => {
                let lit = match (datatype, language) {
                    (_, Some(lang)) => Literal::new_language_tagged_literal(value, lang)
                        .unwrap_or_else(|_| Literal::new_simple_literal(value)),
                    (Some(dt), None) => match NamedNode::new(dt) {
                        Ok(nn) => Literal::new_typed_literal(value, nn),
                        Err(_) => Literal::new_simple_literal(value),
                    },
                    (None, None) => Literal::new_simple_literal(value),
                };
                lit.to_string()
            }
        }
    }

    /// Recover a term from the store's lexical form. The store distinguishes
    /// `"`-prefixed literals from everything else; we additionally recover
    /// blank nodes from a leading `_:` and parse the literal's datatype/lang.
    pub fn from_store_lexical(lex: &str) -> RdfTerm {
        if let Some(label) = lex.strip_prefix("_:") {
            return RdfTerm::Blank(label.to_string());
        }
        if lex.starts_with('"') {
            return parse_literal_lexical(lex);
        }
        // An IRI may be stored bare or in angle brackets depending on the
        // write path; normalise to the bare form rdflib expects.
        let iri = lex
            .strip_prefix('<')
            .and_then(|s| s.strip_suffix('>'))
            .unwrap_or(lex);
        RdfTerm::Iri(iri.to_string())
    }
}

/// Parse a literal in N-Triples lexical form (`"v"`, `"v"@en`, `"v"^^<dt>`).
fn parse_literal_lexical(lex: &str) -> RdfTerm {
    // Reuse oxrdf's N-Triples literal parser for correctness (escapes etc.).
    match Literal::from_str_ntriples(lex) {
        Ok(lit) => {
            let value = lit.value().to_string();
            let language = lit.language().map(|l| l.to_string());
            let datatype = lit.datatype().as_str().to_string();
            RdfTerm::literal(value, Some(datatype), language)
        }
        Err(_) => {
            // Best-effort fallback: strip the outer quotes if present.
            let inner = lex
                .strip_prefix('"')
                .and_then(|s| s.rsplit_once('"').map(|(v, _)| v))
                .unwrap_or(lex);
            RdfTerm::plain_literal(inner.to_string())
        }
    }
}

// oxrdf 0.3's `Literal` does not expose a public N-Triples parse helper, so we
// parse via the term parser. This thin trait keeps the call site readable and
// localises any future swap to a dedicated literal parser.
trait LiteralNtParse: Sized {
    fn from_str_ntriples(s: &str) -> Result<Literal, ()>;
}

impl LiteralNtParse for Literal {
    fn from_str_ntriples(s: &str) -> Result<Literal, ()> {
        // `oxrdf::Term`'s `FromStr` parses N-Triples terms; pull the literal
        // back out. We avoid a full N-Triples line parse for a single term.
        use std::str::FromStr;
        match oxrdf::Term::from_str(s) {
            Ok(oxrdf::Term::Literal(l)) => Ok(l),
            _ => Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iri_round_trips_bare() {
        let t = RdfTerm::iri("http://ex/s");
        let lex = t.to_store_lexical();
        // Bare form, matching the SPARQL store's `NamedNode::as_str()`.
        assert_eq!(lex, "http://ex/s");
        assert_eq!(RdfTerm::from_store_lexical(&lex), t);
    }

    #[test]
    fn angle_bracketed_iri_from_parser_normalises_to_bare() {
        // A parser write path may hand us `<iri>`; recover it as a bare IRI.
        assert_eq!(
            RdfTerm::from_store_lexical("<http://ex/s>"),
            RdfTerm::iri("http://ex/s")
        );
    }

    #[test]
    fn blank_round_trips_as_blank_not_iri() {
        let t = RdfTerm::blank("b0");
        let lex = t.to_store_lexical();
        assert_eq!(lex, "_:b0");
        // Critically NOT classified as an IRI.
        assert_eq!(
            RdfTerm::from_store_lexical(&lex),
            RdfTerm::Blank("b0".into())
        );
    }

    #[test]
    fn plain_literal_round_trips() {
        let t = RdfTerm::plain_literal("hello");
        let lex = t.to_store_lexical();
        assert_eq!(lex, "\"hello\"");
        assert_eq!(RdfTerm::from_store_lexical(&lex), t);
        assert_eq!(t.effective_datatype(), Some(XSD_STRING));
    }

    #[test]
    fn xsd_string_datatype_collapses_to_plain() {
        let t = RdfTerm::literal("hi", Some(XSD_STRING.to_string()), None);
        assert_eq!(t, RdfTerm::plain_literal("hi"));
    }

    #[test]
    fn lang_literal_round_trips_and_lowercases_tag() {
        let t = RdfTerm::literal("chat", None, Some("EN".to_string()));
        let lex = t.to_store_lexical();
        assert_eq!(lex, "\"chat\"@en");
        let back = RdfTerm::from_store_lexical(&lex);
        assert_eq!(back, RdfTerm::literal("chat", None, Some("en".to_string())));
        assert_eq!(back.effective_datatype(), Some(RDF_LANGSTRING));
    }

    #[test]
    fn typed_literal_round_trips() {
        let dt = "http://www.w3.org/2001/XMLSchema#integer";
        let t = RdfTerm::literal("42", Some(dt.to_string()), None);
        let lex = t.to_store_lexical();
        assert_eq!(lex, format!("\"42\"^^<{dt}>"));
        assert_eq!(RdfTerm::from_store_lexical(&lex), t);
        assert_eq!(t.effective_datatype(), Some(dt));
    }

    #[test]
    fn literal_with_quotes_and_escapes_round_trips() {
        let t = RdfTerm::plain_literal("a \"quoted\" word\nnewline");
        let lex = t.to_store_lexical();
        let back = RdfTerm::from_store_lexical(&lex);
        assert_eq!(back, t);
    }
}
