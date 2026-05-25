//! Parse `rules.toml` into typed rule specs.
//!
//! QName resolution (`rdf:type` → `rdf_type`) is driven by a map auto-derived
//! from `src/vocab.rs` by [`crate::codegen::vocab::extract_qname_map`]. The
//! parser does not maintain its own copy of the vocabulary — adding a vocab
//! term is therefore a single edit in `src/vocab.rs`.

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
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

/// Parse `rules.toml`, resolving QNames against the vocab declared in
/// `src/vocab.rs`. This is the only entry point `build.rs` needs.
pub fn parse_file(rules_path: &Path, vocab_path: &Path) -> Result<Vec<RuleSpec>> {
    let text = std::fs::read_to_string(rules_path)
        .with_context(|| format!("reading {}", rules_path.display()))?;
    let qname_map = crate::codegen::vocab::extract_qname_map(vocab_path)?;
    parse_str(&text, &qname_map)
}

/// Parse `rules.toml` text given a precomputed QName map. The map is
/// usually obtained from [`crate::codegen::vocab::extract_qname_map`], but
/// callers (e.g. tests) may build a small map directly.
pub fn parse_str(text: &str, qname_map: &HashMap<String, String>) -> Result<Vec<RuleSpec>> {
    let doc: Document = toml::from_str(text).context("parsing rules.toml")?;
    doc.rule
        .into_iter()
        .map(|r| parse_rule(r, qname_map))
        .collect()
}

fn parse_rule(raw: RawRule, qname_map: &HashMap<String, String>) -> Result<RuleSpec> {
    let body = raw
        .body
        .into_iter()
        .map(|p| parse_pattern(p, qname_map))
        .collect::<Result<Vec<_>>>()?;
    let head = parse_pattern(raw.head, qname_map)?;
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

fn parse_pattern(raw: RawPattern, qname_map: &HashMap<String, String>) -> Result<Pattern> {
    Ok(Pattern {
        s: parse_slot(&raw.s, qname_map)?,
        p: parse_slot(&raw.p, qname_map)?,
        o: parse_slot(&raw.o, qname_map)?,
    })
}

fn parse_slot(s: &str, qname_map: &HashMap<String, String>) -> Result<Slot> {
    if let Some(rest) = s.strip_prefix('?') {
        if rest.is_empty() {
            bail!("empty variable name");
        }
        Ok(Slot::Var(rest.to_string()))
    } else {
        Ok(Slot::Vocab(vocab_term(s, qname_map)?))
    }
}

fn vocab_term(token: &str, qname_map: &HashMap<String, String>) -> Result<VocabTerm> {
    let field = qname_map.get(token).ok_or_else(|| {
        anyhow!(
            "unknown vocabulary token {token:?}; add it to crates/owlrl/src/vocab.rs \
             (struct field + matching `synthetic()` init + `/// `{token}`` doc comment)"
        )
    })?;
    // The codegen needs `&'static str`s in the generated source. Leak the
    // (small, build-time, finite) field-name strings to satisfy that — never
    // freed, but `build.rs` exits long before this matters.
    let field: &'static str = Box::leak(field.clone().into_boxed_str());
    Ok(VocabTerm { field })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_map() -> HashMap<String, String> {
        [
            ("rdf:type", "rdf_type"),
            ("rdfs:subClassOf", "rdfs_sub_class_of"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
    }

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
        let rules = parse_str(src, &fixture_map()).unwrap();
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
        let rules = parse_str(src, &fixture_map()).unwrap();
        assert!(rules[0].delegate);
    }

    #[test]
    fn unknown_vocab_token_errors_with_actionable_message() {
        let src = r#"
            [[rule]]
            id = "bogus"
            body = []
            head = { s = "?x", p = "foo:bar", o = "?y" }
        "#;
        let err = parse_str(src, &fixture_map()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("foo:bar"),
            "error should name the offending token, got: {msg}"
        );
        assert!(
            msg.contains("src/vocab.rs"),
            "error should point at vocab.rs as the fix location, got: {msg}"
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
        assert!(parse_str(src, &fixture_map()).is_err());
    }
}
