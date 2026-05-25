//! Auto-derive the QName→field map by parsing `src/vocab.rs` with `syn`.
//!
//! The single source of truth for the vocabulary is the `struct Vocabulary`
//! declaration in `crates/owlrl/src/vocab.rs`. Each field carries a `///`
//! doc comment whose text contains the canonical QName in backticks
//! (e.g. `` `rdf:type` ``). This module reads that source file at build
//! time and produces the `qname → field_name` map the rules parser uses.
//!
//! Adding a vocabulary term is therefore a single edit in `vocab.rs`. The
//! rules parser picks it up automatically — no other file needs changing.

use anyhow::{anyhow, Context, Result};
use std::collections::HashMap;
use std::path::Path;

/// Parse `src/vocab.rs` and return the QName→field-name map.
///
/// Errors if `Vocabulary` is missing, a field lacks its QName doc comment,
/// or two fields share the same QName.
pub fn extract_qname_map(vocab_path: &Path) -> Result<HashMap<String, String>> {
    let src = std::fs::read_to_string(vocab_path)
        .with_context(|| format!("reading {}", vocab_path.display()))?;
    extract_qname_map_from_source(&src)
}

pub fn extract_qname_map_from_source(src: &str) -> Result<HashMap<String, String>> {
    let file: syn::File = syn::parse_str(src).context("parsing src/vocab.rs as Rust")?;

    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut found_struct = false;
    for item in &file.items {
        let syn::Item::Struct(s) = item else { continue };
        if s.ident != "Vocabulary" {
            continue;
        }
        found_struct = true;
        for field in &s.fields {
            let field_name = field
                .ident
                .as_ref()
                .ok_or_else(|| anyhow!("Vocabulary has an unnamed field — not supported"))?
                .to_string();
            let qname = extract_qname(&field.attrs).ok_or_else(|| {
                anyhow!(
                    "Vocabulary field `{field_name}` is missing its QName doc \
                     comment. Add a single-line doc comment containing the \
                     canonical QName in backticks (e.g. ``` /// `rdf:type` ```) \
                     directly above the field. See crates/owlrl/AGENTS.md."
                )
            })?;
            pairs.push((qname, field_name));
        }
    }
    if !found_struct {
        return Err(anyhow!(
            "src/vocab.rs does not contain a `struct Vocabulary` definition"
        ));
    }

    let mut map: HashMap<String, String> = HashMap::with_capacity(pairs.len());
    for (qname, field) in pairs {
        if let Some(prev) = map.insert(qname.clone(), field.clone()) {
            return Err(anyhow!(
                "duplicate QName `{qname}` on Vocabulary fields `{prev}` and `{field}`"
            ));
        }
    }
    Ok(map)
}

fn extract_qname(attrs: &[syn::Attribute]) -> Option<String> {
    for attr in attrs {
        if !attr.path().is_ident("doc") {
            continue;
        }
        let syn::Meta::NameValue(nv) = &attr.meta else {
            continue;
        };
        let syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(s),
            ..
        }) = &nv.value
        else {
            continue;
        };
        if let Some(q) = parse_qname_from_doc(&s.value()) {
            return Some(q);
        }
    }
    None
}

/// Extract a `prefix:name` QName from a doc-comment string. The recommended
/// form is `` `prefix:name` ``; a bare `prefix:name` line is also accepted.
fn parse_qname_from_doc(line: &str) -> Option<String> {
    let t = line.trim();
    let inner = if let (Some(start), Some(end)) = (t.find('`'), t.rfind('`')) {
        if start < end {
            t[start + 1..end].trim()
        } else {
            t
        }
    } else {
        t
    };
    if inner.contains(':')
        && !inner.contains(char::is_whitespace)
        && inner
            .chars()
            .all(|c| c.is_alphanumeric() || c == ':' || c == '_' || c == '-')
    {
        Some(inner.to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_qnames_from_doc_comments() {
        let src = r#"
            pub struct Vocabulary {
                /// `rdf:type`
                pub rdf_type: TermId,
                /// `owl:sameAs`
                pub owl_same_as: TermId,
            }
        "#;
        let map = extract_qname_map_from_source(src).unwrap();
        assert_eq!(map.get("rdf:type").map(|s| s.as_str()), Some("rdf_type"));
        assert_eq!(
            map.get("owl:sameAs").map(|s| s.as_str()),
            Some("owl_same_as")
        );
    }

    #[test]
    fn missing_qname_doc_errors_pointing_at_field() {
        let src = r#"
            pub struct Vocabulary {
                /// A property without its QName tag.
                pub rdf_type: TermId,
            }
        "#;
        let err = extract_qname_map_from_source(src).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("rdf_type"),
            "expected error to name the offending field, got: {msg}"
        );
    }

    #[test]
    fn duplicate_qname_errors() {
        let src = r#"
            pub struct Vocabulary {
                /// `rdf:type`
                pub a: TermId,
                /// `rdf:type`
                pub b: TermId,
            }
        "#;
        let err = extract_qname_map_from_source(src).unwrap_err();
        assert!(format!("{err:#}").contains("duplicate"));
    }
}
