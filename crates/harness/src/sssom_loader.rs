//! SPEC-11 F9 — harness-only SSSOM/TSV reader (bench/standalone). NOT a
//! production surface: production mappings arrive as RDF via the changefeed.
//!
//! Parses the commented-YAML header (curie_map + propagatable defaults),
//! expands CURIEs, splits `|`-multivalue cells, and emits positive base
//! triples (negated rows -> internal horndb:notExactMatch predicate).

use std::collections::HashMap;

use anyhow::{anyhow, Result};
use oxrdf::{Dataset, GraphName, NamedNode, Quad, Term};

/// IRI for the internal negated-exact-match predicate (mirrors
/// `horndb-owlrl`'s `HORNDB_NOT_EXACT_MATCH`). Keep in sync.
const HORNDB_NOT_EXACT_MATCH: &str = "https://w3id.org/horndb/internal#notExactMatch";

/// Parse SSSOM/TSV text into a `Dataset` of positive base triples.
pub fn parse_sssom_tsv(text: &str) -> Result<Dataset> {
    let mut curie_map: HashMap<String, String> = HashMap::new();
    let mut lines = text.lines().peekable();

    // 1. Commented-YAML header: lines starting with '#'. We only need the
    //    curie_map block (key: "prefix: expansion" under "curie_map:").
    let mut in_curie_map = false;
    while let Some(line) = lines.peek() {
        let Some(stripped) = line.strip_prefix('#') else {
            break;
        };
        let content = stripped.trim_end();
        let trimmed = content.trim();
        if trimmed.starts_with("curie_map:") {
            in_curie_map = true;
        } else if in_curie_map {
            // Indented "  prefix: http://..." -> entry; dedent ends the block.
            let is_indented = content.starts_with("  ") || content.starts_with('\t');
            if is_indented && trimmed.contains(':') {
                let (prefix, exp) = trimmed.split_once(':').unwrap();
                curie_map.insert(
                    prefix.trim().to_string(),
                    exp.trim()
                        .trim_matches(|c| c == '"' || c == '\'')
                        .to_string(),
                );
            } else {
                in_curie_map = false;
            }
        }
        lines.next();
    }

    // 2. The column header row (first non-comment line).
    let header = lines
        .next()
        .ok_or_else(|| anyhow!("SSSOM TSV: missing column header row"))?;
    let cols: Vec<&str> = header.split('\t').collect();
    let col = |name: &str| cols.iter().position(|c| *c == name);
    let subj_i = col("subject_id").ok_or_else(|| anyhow!("missing subject_id column"))?;
    let pred_i = col("predicate_id").ok_or_else(|| anyhow!("missing predicate_id column"))?;
    let obj_i = col("object_id").ok_or_else(|| anyhow!("missing object_id column"))?;
    let modifier_i = col("predicate_modifier");

    // 3. Data rows.
    let mut ds = Dataset::new();
    for row in lines {
        if row.trim().is_empty() {
            continue;
        }
        let cells: Vec<&str> = row.split('\t').collect();
        let get = |i: usize| cells.get(i).copied().unwrap_or("").trim();
        let subjects = split_multi(get(subj_i));
        let predicate = get(pred_i);
        let objects = split_multi(get(obj_i));
        let negated = modifier_i.map(|i| get(i) == "Not").unwrap_or(false);

        let pred_iri = if negated {
            HORNDB_NOT_EXACT_MATCH.to_string()
        } else {
            expand_curie(predicate, &curie_map)?
        };
        let pred_node = NamedNode::new(pred_iri)?;
        for s in &subjects {
            for o in &objects {
                let s_node = NamedNode::new(expand_curie(s, &curie_map)?)?;
                let o_node = NamedNode::new(expand_curie(o, &curie_map)?)?;
                ds.insert(&Quad::new(
                    s_node,
                    pred_node.clone(),
                    Term::NamedNode(o_node),
                    GraphName::DefaultGraph,
                ));
            }
        }
    }
    Ok(ds)
}

/// Split a `|`-delimited SSSOM multivalue cell into trimmed non-empty parts.
fn split_multi(cell: &str) -> Vec<String> {
    cell.split('|')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Expand `prefix:local` to a full IRI via the curie_map; pass through
/// values that already look like full IRIs (contain "://").
fn expand_curie(value: &str, curie_map: &HashMap<String, String>) -> Result<String> {
    if value.contains("://") {
        return Ok(value.to_string());
    }
    let (prefix, local) = value
        .split_once(':')
        .ok_or_else(|| anyhow!("not a CURIE or IRI: {value}"))?;
    let base = curie_map
        .get(prefix)
        .ok_or_else(|| anyhow!("unknown CURIE prefix '{prefix}' in {value}"))?;
    Ok(format!("{base}{local}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
#curie_map:
#  A: http://example.org/a/
#  B: http://example.org/b/
#  skos: http://www.w3.org/2004/02/skos/core#
subject_id\tpredicate_id\tobject_id\tpredicate_modifier
A:1\tskos:exactMatch\tB:1\t
A:2\tskos:broadMatch\tB:2|B:3\t
A:9\tskos:exactMatch\tB:9\tNot
";

    #[test]
    fn parses_curie_map_and_rows() {
        let ds = parse_sssom_tsv(SAMPLE).unwrap();
        // 1 + 2 (multivalue object) + 1 negated = 4 triples
        assert_eq!(ds.len(), 4);
    }

    #[test]
    fn expands_curies_to_full_iris() {
        let ds = parse_sssom_tsv(SAMPLE).unwrap();
        let exact = NamedNode::new("http://www.w3.org/2004/02/skos/core#exactMatch").unwrap();
        let a1 = NamedNode::new("http://example.org/a/1").unwrap();
        let b1 = NamedNode::new("http://example.org/b/1").unwrap();
        assert!(ds.contains(&Quad::new(
            a1,
            exact,
            Term::NamedNode(b1),
            GraphName::DefaultGraph
        )));
    }

    #[test]
    fn multivalue_object_splits_on_pipe() {
        let ds = parse_sssom_tsv(SAMPLE).unwrap();
        let broad = NamedNode::new("http://www.w3.org/2004/02/skos/core#broadMatch").unwrap();
        let a2 = NamedNode::new("http://example.org/a/2").unwrap();
        for tgt in ["http://example.org/b/2", "http://example.org/b/3"] {
            let o = NamedNode::new(tgt).unwrap();
            assert!(ds.contains(&Quad::new(
                a2.clone(),
                broad.clone(),
                Term::NamedNode(o),
                GraphName::DefaultGraph
            )));
        }
    }

    #[test]
    fn negated_row_uses_internal_predicate() {
        let ds = parse_sssom_tsv(SAMPLE).unwrap();
        let not_exact = NamedNode::new(HORNDB_NOT_EXACT_MATCH).unwrap();
        let a9 = NamedNode::new("http://example.org/a/9").unwrap();
        let b9 = NamedNode::new("http://example.org/b/9").unwrap();
        assert!(ds.contains(&Quad::new(
            a9,
            not_exact,
            Term::NamedNode(b9),
            GraphName::DefaultGraph
        )));
    }
}
