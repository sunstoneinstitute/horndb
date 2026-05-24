//! Parser for W3C-style test manifests, expressed in Turtle.
//!
//! Real W3C manifests historically shipped as RDF/XML; the Stage-1
//! fetch script converts them to Turtle so this parser is the single
//! ingestion point. Vocabulary used (subset sufficient for Stage 0):
//!
//! * `mf:` <http://www.w3.org/2001/sw/DataAccess/tests/test-manifest#>
//! * `rdft:` <http://www.w3.org/ns/rdftest#>
//! * `qt:` <http://www.w3.org/2001/sw/DataAccess/tests/test-query#>
//!
//! We recognise the test types listed in SPEC-01 F1: positive/negative
//! entailment, consistency/inconsistency, plus a minimal SPARQL ASK
//! variant for SPARQL 1.1 manifests.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use oxrdf::{Graph, NamedNodeRef, Subject, SubjectRef, Term, TermRef};
use oxttl::TurtleParser;

use crate::testcase::{Suite, TestCase, TestKind};

const MF: &str = "http://www.w3.org/2001/sw/DataAccess/tests/test-manifest#";
#[allow(dead_code)]
const RDFT: &str = "http://www.w3.org/ns/rdftest#";
const QT: &str = "http://www.w3.org/2001/sw/DataAccess/tests/test-query#";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDF_FIRST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#first";
const RDF_REST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#rest";
const RDF_NIL: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#nil";

/// Parse a manifest from disk. `suite` is supplied externally because
/// the harness already knows which directory it is loading.
pub fn parse(path: &Path, suite: Suite) -> Result<Vec<TestCase>> {
    let bytes = fs::read(path).with_context(|| format!("reading manifest {}", path.display()))?;
    let base = path
        .parent()
        .ok_or_else(|| anyhow!("manifest has no parent dir"))?;
    let graph = parse_turtle(&bytes, &format!("file://{}", path.display()))?;
    extract_cases(&graph, base, suite)
}

fn parse_turtle(bytes: &[u8], base_iri: &str) -> Result<Graph> {
    let mut graph = Graph::new();
    let parser = TurtleParser::new()
        .with_base_iri(base_iri)?
        .for_slice(bytes);
    for triple in parser {
        let triple = triple?;
        graph.insert(&triple);
    }
    Ok(graph)
}

fn term_to_subject(t: &Term) -> Result<Subject> {
    match t {
        Term::NamedNode(n) => Ok(Subject::NamedNode(n.clone())),
        Term::BlankNode(b) => Ok(Subject::BlankNode(b.clone())),
        _ => bail!("expected resource, got literal"),
    }
}

fn subjectref_to_subject(s: SubjectRef<'_>) -> Result<Subject> {
    match s {
        SubjectRef::NamedNode(n) => Ok(Subject::NamedNode(n.into_owned())),
        SubjectRef::BlankNode(b) => Ok(Subject::BlankNode(b.into_owned())),
        other => bail!("unsupported subject shape: {other:?}"),
    }
}

fn extract_cases(graph: &Graph, base: &Path, suite: Suite) -> Result<Vec<TestCase>> {
    // 1. Find the manifest node (typed mf:Manifest).
    let manifest_iri = format!("{MF}Manifest");
    let manifest_type = NamedNodeRef::new(&manifest_iri)?;
    let rdf_type = NamedNodeRef::new(RDF_TYPE)?;
    let manifest_term_ref: TermRef<'_> = manifest_type.into();
    let manifest_subj_ref = graph
        .subjects_for_predicate_object(rdf_type, manifest_term_ref)
        .next()
        .ok_or_else(|| anyhow!("no mf:Manifest in {}", base.display()))?;
    let manifest_subj = subjectref_to_subject(manifest_subj_ref)?;

    // 2. Walk mf:entries list.
    let entries_iri = format!("{MF}entries");
    let entries_pred = NamedNodeRef::new(&entries_iri)?;
    let entry_head = graph
        .object_for_subject_predicate(manifest_subj.as_ref(), entries_pred)
        .ok_or_else(|| anyhow!("manifest has no mf:entries"))?
        .into_owned();
    let entries = read_rdf_list(graph, entry_head)?;

    // 3. Project each entry into a TestCase.
    let projector = EntryProjector::new()?;
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let entry_subj = term_to_subject(&entry)?;
        out.push(projector.project(graph, &entry_subj, base, suite)?);
    }
    Ok(out)
}

struct EntryProjector {
    name_iri: String,
    action_iri: String,
    result_iri: String,
    pe_iri: String,
    ne_iri: String,
    cons_iri: String,
    incons_iri: String,
    qet_iri: String,
    qt_query_iri: String,
    qt_data_iri: String,
}

impl EntryProjector {
    fn new() -> Result<Self> {
        Ok(Self {
            name_iri: format!("{MF}name"),
            action_iri: format!("{MF}action"),
            result_iri: format!("{MF}result"),
            pe_iri: format!("{MF}PositiveEntailmentTest"),
            ne_iri: format!("{MF}NegativeEntailmentTest"),
            cons_iri: format!("{MF}ConsistencyTest"),
            incons_iri: format!("{MF}InconsistencyTest"),
            qet_iri: format!("{MF}QueryEvaluationTest"),
            qt_query_iri: format!("{QT}query"),
            qt_data_iri: format!("{QT}data"),
        })
    }

    fn project(
        &self,
        graph: &Graph,
        entry: &Subject,
        base: &Path,
        suite: Suite,
    ) -> Result<TestCase> {
        project_entry(self, graph, entry, base, suite)
    }
}

fn read_rdf_list(graph: &Graph, head: Term) -> Result<Vec<Term>> {
    let first = NamedNodeRef::new(RDF_FIRST)?;
    let rest = NamedNodeRef::new(RDF_REST)?;
    let nil_iri = NamedNodeRef::new(RDF_NIL)?;
    let mut out = Vec::new();
    let mut cur = head;
    loop {
        if let Term::NamedNode(n) = &cur {
            if n.as_ref() == nil_iri {
                break;
            }
        }
        let cur_subj = term_to_subject(&cur)?;
        let item = graph
            .object_for_subject_predicate(cur_subj.as_ref(), first)
            .ok_or_else(|| anyhow!("malformed list (missing rdf:first)"))?
            .into_owned();
        out.push(item);
        cur = graph
            .object_for_subject_predicate(cur_subj.as_ref(), rest)
            .ok_or_else(|| anyhow!("malformed list (missing rdf:rest)"))?
            .into_owned();
    }
    Ok(out)
}

fn project_entry(
    p: &EntryProjector,
    graph: &Graph,
    entry: &Subject,
    base: &Path,
    suite: Suite,
) -> Result<TestCase> {
    let name_pred = NamedNodeRef::new(&p.name_iri)?;
    let action_pred = NamedNodeRef::new(&p.action_iri)?;
    let result_pred = NamedNodeRef::new(&p.result_iri)?;
    let rdf_type = NamedNodeRef::new(RDF_TYPE)?;

    let id = match entry {
        Subject::NamedNode(n) => n.as_str().to_string(),
        Subject::BlankNode(b) => format!("_:{}", b.as_str()),
        other => bail!("unsupported entry subject: {other:?}"),
    };

    let name = graph
        .object_for_subject_predicate(entry.as_ref(), name_pred)
        .and_then(|t| match t {
            TermRef::Literal(l) => Some(l.value().to_string()),
            _ => None,
        })
        .unwrap_or_else(|| id.clone());

    let kind_iri_term = graph
        .object_for_subject_predicate(entry.as_ref(), rdf_type)
        .ok_or_else(|| anyhow!("entry {id} has no rdf:type"))?
        .into_owned();
    let kind_iri = match kind_iri_term {
        Term::NamedNode(n) => n,
        _ => bail!("entry {id} rdf:type is not an IRI"),
    };

    let resolve = |t: Term| -> Result<PathBuf> {
        match t {
            Term::NamedNode(n) => resolve_file(n.as_str(), base),
            other => bail!("expected file IRI, got {other}"),
        }
    };

    let action = graph
        .object_for_subject_predicate(entry.as_ref(), action_pred)
        .map(|t| t.into_owned());
    let result = graph
        .object_for_subject_predicate(entry.as_ref(), result_pred)
        .map(|t| t.into_owned());

    let kind_str = kind_iri.as_str();
    let kind = if kind_str == p.pe_iri {
        TestKind::PositiveEntailment {
            premise: resolve(action.ok_or_else(|| anyhow!("missing mf:action"))?)?,
            conclusion: resolve(result.ok_or_else(|| anyhow!("missing mf:result"))?)?,
        }
    } else if kind_str == p.ne_iri {
        TestKind::NegativeEntailment {
            premise: resolve(action.ok_or_else(|| anyhow!("missing mf:action"))?)?,
            conclusion: resolve(result.ok_or_else(|| anyhow!("missing mf:result"))?)?,
        }
    } else if kind_str == p.cons_iri {
        TestKind::Consistency {
            premise: resolve(action.ok_or_else(|| anyhow!("missing mf:action"))?)?,
        }
    } else if kind_str == p.incons_iri {
        TestKind::Inconsistency {
            premise: resolve(action.ok_or_else(|| anyhow!("missing mf:action"))?)?,
        }
    } else if kind_str == p.qet_iri || kind_str.starts_with(QT) {
        // SPARQL ASK: action is a qt:QueryTest with qt:query + qt:data,
        // result is an SRX file we read here to extract the boolean.
        let action_node = action.ok_or_else(|| anyhow!("missing mf:action"))?;
        let action_subj =
            term_to_subject(&action_node).map_err(|_| anyhow!("qt action is not a resource"))?;
        let qt_query = NamedNodeRef::new(&p.qt_query_iri)?;
        let qt_data = NamedNodeRef::new(&p.qt_data_iri)?;
        let query = resolve(
            graph
                .object_for_subject_predicate(action_subj.as_ref(), qt_query)
                .ok_or_else(|| anyhow!("qt:query missing"))?
                .into_owned(),
        )?;
        let data = resolve(
            graph
                .object_for_subject_predicate(action_subj.as_ref(), qt_data)
                .ok_or_else(|| anyhow!("qt:data missing"))?
                .into_owned(),
        )?;
        let expected_path = resolve(result.ok_or_else(|| anyhow!("missing mf:result"))?)?;
        let srx = fs::read_to_string(&expected_path)
            .with_context(|| format!("reading SRX {}", expected_path.display()))?;
        let expected = srx.contains("<boolean>true</boolean>");
        TestKind::SparqlAsk {
            query,
            data,
            expected,
        }
    } else {
        bail!("unsupported test type for entry {id}: {kind_str}");
    };

    Ok(TestCase {
        id,
        suite,
        name,
        kind,
    })
}

fn resolve_file(iri: &str, base: &Path) -> Result<PathBuf> {
    // Manifests reference siblings either as relative paths or as
    // `file://` IRIs that the Turtle parser already resolved against
    // the manifest's base. Both shapes are accepted.
    if let Some(rel) = iri.strip_prefix("file://") {
        // The Turtle parser produces absolute file:// IRIs relative to
        // the manifest directory; strip the prefix back to a path.
        // Cope with both `file:///abs/...` and the simpler `file://`.
        let trimmed = rel.trim_start_matches('/');
        let candidate_abs = PathBuf::from(format!("/{trimmed}"));
        if candidate_abs.exists() {
            return Ok(candidate_abs);
        }
        return Ok(base.join(trimmed));
    }
    Ok(base.join(iri))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    fn write(dir: &Path, name: &str, content: &str) -> PathBuf {
        let p = dir.join(name);
        let mut f = fs::File::create(&p).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        p
    }

    #[test]
    fn parses_minimal_positive_entailment_manifest() {
        let d = tempdir().unwrap();
        write(d.path(), "premise.ttl", "");
        write(d.path(), "conclusion.ttl", "");
        let manifest = write(
            d.path(),
            "manifest.ttl",
            r#"
@prefix mf:   <http://www.w3.org/2001/sw/DataAccess/tests/test-manifest#> .
@prefix rdf:  <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .

<#manifest> a mf:Manifest ;
    mf:entries ( <#t-empty-entails-empty> ) .

<#t-empty-entails-empty> a mf:PositiveEntailmentTest ;
    mf:name "empty entails empty" ;
    mf:action <premise.ttl> ;
    mf:result <conclusion.ttl> .
"#,
        );
        let cases = parse(&manifest, Suite::Owl2).expect("parse ok");
        assert_eq!(cases.len(), 1);
        let c = &cases[0];
        assert_eq!(c.name, "empty entails empty");
        assert!(matches!(&c.kind, TestKind::PositiveEntailment { .. }));
        assert!(c.id.ends_with("#t-empty-entails-empty"));
    }

    #[test]
    fn rejects_manifest_with_no_mf_manifest() {
        let d = tempdir().unwrap();
        let manifest = write(d.path(), "manifest.ttl", "# empty\n");
        let err = parse(&manifest, Suite::Owl2).unwrap_err();
        assert!(err.to_string().contains("no mf:Manifest"));
    }
}
