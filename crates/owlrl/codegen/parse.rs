//! Parse `rules.toml` into typed rule specs.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::Path;

/// Raw TOML shape — mirrors `rules.toml` literally.
#[derive(Debug, Deserialize)]
struct Document {
    rule: Vec<RawRule>,
}

#[derive(Debug, Deserialize)]
struct RawRule {
    id: String,
    #[serde(default)]
    comment: String,
    #[serde(default)]
    delegate: Option<String>,
    body: Vec<RawPattern>,
    head: RawPattern,
}

#[derive(Debug, Deserialize)]
struct RawPattern {
    s: String,
    p: String,
    o: String,
}

/// Parsed slot: variable (by name) or a vocabulary token.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum Slot {
    Var(String),
    Vocab(VocabTerm),
}

/// A reference to one of the fields on `crate::vocab::Vocabulary`. The
/// `field` is the literal Rust field name on `Vocabulary`.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct VocabTerm {
    pub field: &'static str,
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Pattern {
    pub s: Slot,
    pub p: Slot,
    pub o: Slot,
}

#[derive(Debug, Clone)]
pub struct RuleSpec {
    pub id: String,
    #[allow(dead_code)]
    pub comment: String,
    pub delegate: bool,
    pub body: Vec<Pattern>,
    pub head: Pattern,
}

pub fn parse_file(path: &Path) -> Result<Vec<RuleSpec>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    parse_str(&text)
}

pub fn parse_str(text: &str) -> Result<Vec<RuleSpec>> {
    let doc: Document = toml::from_str(text).context("parsing rules.toml")?;
    doc.rule.into_iter().map(parse_rule).collect()
}

fn parse_rule(raw: RawRule) -> Result<RuleSpec> {
    let body = raw
        .body
        .into_iter()
        .map(parse_pattern)
        .collect::<Result<Vec<_>>>()?;
    let head = parse_pattern(raw.head)?;
    let delegate = match raw.delegate.as_deref() {
        None => false,
        Some("closure") => true,
        Some(other) => bail!("unknown delegate target {:?} for rule {}", other, raw.id),
    };
    Ok(RuleSpec {
        id: raw.id,
        comment: raw.comment,
        delegate,
        body,
        head,
    })
}

fn parse_pattern(raw: RawPattern) -> Result<Pattern> {
    Ok(Pattern {
        s: parse_slot(&raw.s)?,
        p: parse_slot(&raw.p)?,
        o: parse_slot(&raw.o)?,
    })
}

fn parse_slot(s: &str) -> Result<Slot> {
    if let Some(rest) = s.strip_prefix('?') {
        if rest.is_empty() {
            bail!("empty variable name");
        }
        Ok(Slot::Var(rest.to_string()))
    } else {
        Ok(Slot::Vocab(vocab_term(s)?))
    }
}

fn vocab_term(token: &str) -> Result<VocabTerm> {
    // Map QName-style vocab token → field on `crate::vocab::Vocabulary`.
    let field: &'static str = match token {
        "rdf:type" => "rdf_type",
        "rdf:first" => "rdf_first",
        "rdf:rest" => "rdf_rest",
        "rdf:nil" => "rdf_nil",
        "rdfs:subClassOf" => "rdfs_sub_class_of",
        "rdfs:subPropertyOf" => "rdfs_sub_property_of",
        "rdfs:domain" => "rdfs_domain",
        "rdfs:range" => "rdfs_range",
        "owl:Class" => "owl_class",
        "owl:Thing" => "owl_thing",
        "owl:Nothing" => "owl_nothing",
        "owl:sameAs" => "owl_same_as",
        "owl:differentFrom" => "owl_different_from",
        "owl:equivalentClass" => "owl_equivalent_class",
        "owl:equivalentProperty" => "owl_equivalent_property",
        "owl:inverseOf" => "owl_inverse_of",
        "owl:FunctionalProperty" => "owl_functional_property",
        "owl:InverseFunctionalProperty" => "owl_inverse_functional_property",
        "owl:SymmetricProperty" => "owl_symmetric_property",
        "owl:TransitiveProperty" => "owl_transitive_property",
        "owl:IrreflexiveProperty" => "owl_irreflexive_property",
        "owl:ReflexiveProperty" => "owl_reflexive_property",
        "owl:AsymmetricProperty" => "owl_asymmetric_property",
        "owl:propertyDisjointWith" => "owl_property_disjoint_with",
        "owl:disjointWith" => "owl_disjoint_with",
        "owl:complementOf" => "owl_complement_of",
        "owl:intersectionOf" => "owl_intersection_of",
        "owl:unionOf" => "owl_union_of",
        "owl:someValuesFrom" => "owl_some_values_from",
        "owl:allValuesFrom" => "owl_all_values_from",
        "owl:hasValue" => "owl_has_value",
        "owl:onProperty" => "owl_on_property",
        "owl:maxCardinality" => "owl_max_cardinality",
        "owl:sourceIndividual" => "owl_source_individual",
        "owl:assertionProperty" => "owl_assertion_property",
        "owl:targetIndividual" => "owl_target_individual",
        "owl:targetValue" => "owl_target_value",
        "owl:ObjectProperty" => "owl_object_property",
        other => bail!("unknown vocabulary token {other:?}"),
    };
    Ok(VocabTerm { field })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_rule() {
        let src = r#"
            [[rule]]
            id = "cax-sco"
            body = [
              { s = "?c1", p = "rdfs:subClassOf", o = "?c2" },
              { s = "?x", p = "rdf:type", o = "?c1" },
            ]
            head = { s = "?x", p = "rdf:type", o = "?c2" }
        "#;
        let rules = parse_str(src).unwrap();
        assert_eq!(rules.len(), 1);
        let r = &rules[0];
        assert_eq!(r.id, "cax-sco");
        assert!(!r.delegate);
        assert_eq!(r.body.len(), 2);
    }

    #[test]
    fn delegate_closure_recognized() {
        let src = r#"
            [[rule]]
            id = "scm-sco"
            delegate = "closure"
            body = [
              { s = "?a", p = "rdfs:subClassOf", o = "?b" },
              { s = "?b", p = "rdfs:subClassOf", o = "?c" },
            ]
            head = { s = "?a", p = "rdfs:subClassOf", o = "?c" }
        "#;
        let rules = parse_str(src).unwrap();
        assert!(rules[0].delegate);
    }

    #[test]
    fn unknown_vocab_token_errors() {
        let src = r#"
            [[rule]]
            id = "bogus"
            body = []
            head = { s = "?x", p = "foo:bar", o = "?y" }
        "#;
        let err = parse_str(src).unwrap_err();
        assert!(
            err.to_string().contains("foo:bar")
                || err.chain().any(|c| c.to_string().contains("foo:bar"))
        );
    }

    #[test]
    fn variable_must_have_name() {
        let src = r#"
            [[rule]]
            id = "bogus"
            body = []
            head = { s = "?", p = "rdf:type", o = "?y" }
        "#;
        assert!(parse_str(src).is_err());
    }
}
