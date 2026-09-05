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

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use oxrdf::{Graph, NamedNodeRef, NamedOrBlankNode, NamedOrBlankNodeRef, Term, TermRef};
use oxttl::TurtleParser;

use crate::testcase::{GspRequest, Suite, TestCase, TestKind};

const MF: &str = "http://www.w3.org/2001/sw/DataAccess/tests/test-manifest#";
const RDFT: &str = "http://www.w3.org/ns/rdftest#";
const QT: &str = "http://www.w3.org/2001/sw/DataAccess/tests/test-query#";
/// SPARQL 1.1 Update test vocabulary — `ut:request` / `ut:data` /
/// `ut:graphData` / `ut:graph`, used by `mf:UpdateEvaluationTest`.
const UT: &str = "http://www.w3.org/2009/sparql/tests/test-update#";
/// W3C HTTP-in-RDF vocabulary, used by the Graph Store Protocol manifests to
/// spell out each request/response pair.
const HT: &str = "http://www.w3.org/2011/http#";
/// Status-code IRIs (`hts:OK`, `hts:Created`, ...) named by `mf:expectedStatus`.
const HTS: &str = "http://www.w3.org/2011/http-statusCodes#";
/// "Representing Content in RDF" — `cnt:chars` holds a request/response body.
const CNT: &str = "http://www.w3.org/2011/content#";
const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDF_FIRST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#first";
const RDF_REST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#rest";
const RDF_NIL: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#nil";

/// Parse a manifest from disk. `suite` is supplied externally because
/// the harness already knows which directory it is loading.
///
/// `mf:include`d sub-manifests are followed depth-first, so pointing at the
/// upstream `manifest-all.ttl` yields every case in the tree. A manifest is
/// read at most once per call, so an include cycle terminates.
pub fn parse(path: &Path, suite: Suite) -> Result<Vec<TestCase>> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    parse_into(path, suite, &mut out, &mut seen)?;
    Ok(out)
}

fn parse_into(
    path: &Path,
    suite: Suite,
    out: &mut Vec<TestCase>,
    seen: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    let key = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if !seen.insert(key) {
        return Ok(());
    }
    let bytes = fs::read(path).with_context(|| format!("reading manifest {}", path.display()))?;
    let base = path
        .parent()
        .ok_or_else(|| anyhow!("manifest has no parent dir"))?;
    let graph = parse_turtle(&bytes, &format!("file://{}", path.display()))?;
    let (cases, includes) = extract_cases(&graph, base, suite)?;
    out.extend(cases);
    for inc in includes {
        parse_into(&inc, suite, out, seen)
            .with_context(|| format!("via mf:include from {}", path.display()))?;
    }
    Ok(())
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

fn term_to_subject(t: &Term) -> Result<NamedOrBlankNode> {
    // W3C test manifests are RDF 1.1 documents per SPEC-01 — they never
    // carry triple-term subjects. The literal arm catches the same class
    // of "wrong term shape" errors as a triple-term arm would, and the
    // bail on `Term::Triple` is explicit so the failure mode is loud.
    match t {
        Term::NamedNode(n) => Ok(NamedOrBlankNode::NamedNode(n.clone())),
        Term::BlankNode(b) => Ok(NamedOrBlankNode::BlankNode(b.clone())),
        Term::Literal(_) => bail!("expected resource, got literal"),
        Term::Triple(_) => bail!("RDF 1.2 triple terms are not valid in W3C test manifests"),
    }
}

fn subjectref_to_subject(s: NamedOrBlankNodeRef<'_>) -> Result<NamedOrBlankNode> {
    // RDF 1.2 keeps subjects as `NamedOrBlankNodeRef` — exhaustive
    // without a triple-term arm. See `term_to_subject` for the
    // manifest-only invariant on `Term::Triple`.
    match s {
        NamedOrBlankNodeRef::NamedNode(n) => Ok(NamedOrBlankNode::NamedNode(n.into_owned())),
        NamedOrBlankNodeRef::BlankNode(b) => Ok(NamedOrBlankNode::BlankNode(b.into_owned())),
    }
}

/// Project one manifest document into its own cases plus the paths of the
/// sub-manifests it `mf:include`s.
fn extract_cases(
    graph: &Graph,
    base: &Path,
    suite: Suite,
) -> Result<(Vec<TestCase>, Vec<PathBuf>)> {
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

    // 2. Walk mf:entries list. An aggregate manifest (upstream
    // `manifest-all.ttl`) has only `mf:include`, no entries of its own.
    let entries_iri = format!("{MF}entries");
    let entries_pred = NamedNodeRef::new(&entries_iri)?;
    let entries = match graph.object_for_subject_predicate(manifest_subj.as_ref(), entries_pred) {
        Some(head) => read_rdf_list(graph, head.into_owned())?,
        None => Vec::new(),
    };

    // 3. Project each entry into a TestCase.
    let projector = EntryProjector::new()?;
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let entry_subj = term_to_subject(&entry)?;
        out.push(projector.project(graph, &entry_subj, base, suite)?);
    }

    // 4. Collect mf:include'd sub-manifests, if any.
    let include_iri = format!("{MF}include");
    let include_pred = NamedNodeRef::new(&include_iri)?;
    let mut includes = Vec::new();
    if let Some(head) = graph.object_for_subject_predicate(manifest_subj.as_ref(), include_pred) {
        for item in read_rdf_list(graph, head.into_owned())? {
            match item {
                Term::NamedNode(n) => includes.push(resolve_file(n.as_str(), base)?),
                other => bail!("mf:include member is not an IRI: {other}"),
            }
        }
    }
    Ok((out, includes))
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
    uet_iri: String,
    qt_query_iri: String,
    qt_data_iri: String,
    qt_graph_data_iri: String,
    ut_request_iri: String,
    ut_data_iri: String,
    ut_graph_data_iri: String,
    ut_graph_iri: String,
    syntax_pos_iri: String,
    syntax_neg_iri: String,
    sparql_query_pos_iri: String,
    sparql_query_neg_iri: String,
    sparql_update_pos_iri: String,
    sparql_update_neg_iri: String,
    gsp_iri: String,
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
            uet_iri: format!("{MF}UpdateEvaluationTest"),
            qt_query_iri: format!("{QT}query"),
            qt_data_iri: format!("{QT}data"),
            qt_graph_data_iri: format!("{QT}graphData"),
            ut_request_iri: format!("{UT}request"),
            ut_data_iri: format!("{UT}data"),
            ut_graph_data_iri: format!("{UT}graphData"),
            ut_graph_iri: format!("{UT}graph"),
            // W3C RDF 1.2 N-Triples syntax tests use the rdft: vocabulary
            // rather than mf:*. The syntax-only tests have only an
            // `mf:action`; no `mf:result`.
            syntax_pos_iri: format!("{RDFT}TestNTriplesPositiveSyntax"),
            syntax_neg_iri: format!("{RDFT}TestNTriplesNegativeSyntax"),
            // W3C SPARQL 1.1 *syntax* tests. The mf:action points directly at
            // the `.rq` (query) / `.ru` (update) file — no qt:QueryTest blank
            // node, no data, no result. Graded by spargebra accept/reject.
            sparql_query_pos_iri: format!("{MF}PositiveSyntaxTest11"),
            sparql_query_neg_iri: format!("{MF}NegativeSyntaxTest11"),
            sparql_update_pos_iri: format!("{MF}PositiveUpdateSyntaxTest11"),
            sparql_update_neg_iri: format!("{MF}NegativeUpdateSyntaxTest11"),
            // W3C SPARQL 1.1 Graph Store Protocol test: `mf:action` is an
            // `ht:Connection` holding an ordered `ht:requests` list.
            gsp_iri: format!("{MF}GraphStoreProtocolTest"),
        })
    }

    fn project(
        &self,
        graph: &Graph,
        entry: &NamedOrBlankNode,
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
    entry: &NamedOrBlankNode,
    base: &Path,
    suite: Suite,
) -> Result<TestCase> {
    let name_pred = NamedNodeRef::new(&p.name_iri)?;
    let action_pred = NamedNodeRef::new(&p.action_iri)?;
    let result_pred = NamedNodeRef::new(&p.result_iri)?;
    let rdf_type = NamedNodeRef::new(RDF_TYPE)?;

    // Entries are subjects in the manifest — RDF 1.2 keeps subjects as
    // `NamedOrBlankNode`, so the match is exhaustive without an explicit
    // triple-term arm. See SPEC-01 / `term_to_subject` for the manifest
    // shape rationale.
    let id = match entry {
        NamedOrBlankNode::NamedNode(n) => n.as_str().to_string(),
        NamedOrBlankNode::BlankNode(b) => format!("_:{}", b.as_str()),
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

    // Graph Store Protocol cases are HTTP sequences, not files. Gated on the
    // suite so no other manifest's `mf:GraphStoreProtocolTest` changes shape.
    if suite == Suite::Sparql11Gsp && kind_str == p.gsp_iri {
        let action_node = action.ok_or_else(|| anyhow!("missing mf:action"))?;
        let requests = gsp_requests(graph, &term_to_subject(&action_node)?)
            .with_context(|| format!("entry {id}"))?;
        return Ok(TestCase {
            id,
            suite,
            name,
            kind: TestKind::GraphStoreProtocol { requests },
        });
    }

    // The whole-manifest SPARQL 1.1 *evaluation* suite grades the two
    // evaluation types itself; every other suite keeps the curated Stage-1
    // projections below (notably `mf:QueryEvaluationTest` -> SparqlAsk).
    if suite == Suite::Sparql11Eval && (kind_str == p.qet_iri || kind_str == p.uet_iri) {
        let action_node = action.ok_or_else(|| anyhow!("missing mf:action"))?;
        let action_subj = term_to_subject(&action_node)?;
        let kind = if kind_str == p.qet_iri {
            TestKind::SparqlQueryEval {
                query: resolve(
                    graph
                        .object_for_subject_predicate(
                            action_subj.as_ref(),
                            NamedNodeRef::new(&p.qt_query_iri)?,
                        )
                        .ok_or_else(|| anyhow!("qt:query missing"))?
                        .into_owned(),
                )?,
                data: opt_file(graph, &action_subj, &p.qt_data_iri, base)?,
                graph_data: files(graph, &action_subj, &p.qt_graph_data_iri, base)?,
                result: resolve(result.ok_or_else(|| anyhow!("missing mf:result"))?)?,
            }
        } else {
            let result_subj =
                term_to_subject(&result.ok_or_else(|| anyhow!("missing mf:result"))?)?;
            TestKind::SparqlUpdateEval {
                request: resolve(
                    graph
                        .object_for_subject_predicate(
                            action_subj.as_ref(),
                            NamedNodeRef::new(&p.ut_request_iri)?,
                        )
                        .ok_or_else(|| anyhow!("ut:request missing"))?
                        .into_owned(),
                )?,
                data: opt_file(graph, &action_subj, &p.ut_data_iri, base)?,
                graph_data: labelled_graphs(graph, &action_subj, p, base)?,
                result_data: opt_file(graph, &result_subj, &p.ut_data_iri, base)?,
                result_graph_data: labelled_graphs(graph, &result_subj, p, base)?,
            }
        };
        return Ok(TestCase {
            id,
            suite,
            name,
            kind,
        });
    }

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
    } else if kind_str == p.syntax_pos_iri {
        TestKind::SyntaxPositive {
            input: resolve(action.ok_or_else(|| anyhow!("missing mf:action"))?)?,
        }
    } else if kind_str == p.syntax_neg_iri {
        TestKind::SyntaxNegative {
            input: resolve(action.ok_or_else(|| anyhow!("missing mf:action"))?)?,
        }
    } else if kind_str == p.sparql_query_pos_iri {
        TestKind::SparqlSyntaxPositive {
            input: resolve(action.ok_or_else(|| anyhow!("missing mf:action"))?)?,
            update: false,
        }
    } else if kind_str == p.sparql_query_neg_iri {
        TestKind::SparqlSyntaxNegative {
            input: resolve(action.ok_or_else(|| anyhow!("missing mf:action"))?)?,
            update: false,
        }
    } else if kind_str == p.sparql_update_pos_iri {
        TestKind::SparqlSyntaxPositive {
            input: resolve(action.ok_or_else(|| anyhow!("missing mf:action"))?)?,
            update: true,
        }
    } else if kind_str == p.sparql_update_neg_iri {
        TestKind::SparqlSyntaxNegative {
            input: resolve(action.ok_or_else(|| anyhow!("missing mf:action"))?)?,
            update: true,
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
        // Not a type this harness grades (e.g. `mf:ProtocolTest`). Kept as a
        // case so a whole-manifest selection still loads; the runner reports
        // it Skipped with the type IRI, which keeps the gap visible instead of
        // failing the whole manifest load.
        TestKind::Unsupported {
            type_iri: kind_str.to_string(),
        }
    };

    Ok(TestCase {
        id,
        suite,
        name,
        kind,
    })
}

/// Project an `ht:Connection`'s `ht:requests` list into [`GspRequest`]s.
///
/// Shape (see the upstream `graph-store-protocol/manifest.ttl` comment):
/// each `ht:Request` carries `ht:methodName`, `ht:absolutePath`, an optional
/// `ht:headers` list of `[ht:fieldName ; ht:fieldValue]`, an optional
/// `ht:body [ cnt:chars "…" ]`, and an `ht:resp` whose `mf:expectedStatus`
/// names the acceptable status codes.
fn gsp_requests(graph: &Graph, conn: &NamedOrBlankNode) -> Result<Vec<GspRequest>> {
    let head = graph
        .object_for_subject_predicate(conn.as_ref(), NamedNodeRef::new(&format!("{HT}requests"))?)
        .ok_or_else(|| anyhow!("ht:Connection has no ht:requests"))?
        .into_owned();
    let mut out = Vec::new();
    for item in read_rdf_list(graph, head)? {
        let req = term_to_subject(&item)?;
        let resp = term_to_subject(
            &graph
                .object_for_subject_predicate(
                    req.as_ref(),
                    NamedNodeRef::new(&format!("{HT}resp"))?,
                )
                .ok_or_else(|| anyhow!("ht:Request has no ht:resp"))?
                .into_owned(),
        )?;
        let mut expected_status = Vec::new();
        for o in graph.objects_for_subject_predicate(
            resp.as_ref(),
            NamedNodeRef::new(&format!("{MF}expectedStatus"))?,
        ) {
            match o {
                TermRef::NamedNode(n) => expected_status.push(status_code(n.as_str())?),
                other => bail!("mf:expectedStatus is not an IRI: {other}"),
            }
        }
        if expected_status.is_empty() {
            bail!("ht:Response has no mf:expectedStatus");
        }
        let expected_body = gsp_body(graph, &resp)?;
        let expected_body_type = expected_body
            .is_some()
            .then(|| {
                gsp_headers(graph, &resp).map(|hs| {
                    hs.into_iter()
                        .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
                        .map(|(_, v)| v)
                })
            })
            .transpose()?
            .flatten();
        out.push(GspRequest {
            method: gsp_literal(graph, &req, &format!("{HT}methodName"))?
                .ok_or_else(|| anyhow!("ht:Request has no ht:methodName"))?,
            path: gsp_literal(graph, &req, &format!("{HT}absolutePath"))?
                .ok_or_else(|| anyhow!("ht:Request has no ht:absolutePath"))?,
            headers: gsp_headers(graph, &req)?,
            body: gsp_body(graph, &req)?,
            expected_status,
            expected_body,
            expected_body_type,
        });
    }
    Ok(out)
}

/// `hts:` status-code IRI to its numeric code. Unknown names are a hard
/// error: silently dropping one would make a case pass on any status.
fn status_code(iri: &str) -> Result<u16> {
    match iri.strip_prefix(HTS) {
        Some("OK") => Ok(200),
        Some("Created") => Ok(201),
        Some("NoContent") => Ok(204),
        Some("NotFound") => Ok(404),
        _ => bail!("unrecognised mf:expectedStatus {iri}"),
    }
}

fn gsp_literal(graph: &Graph, subj: &NamedOrBlankNode, pred: &str) -> Result<Option<String>> {
    match graph.object_for_subject_predicate(subj.as_ref(), NamedNodeRef::new(pred)?) {
        Some(TermRef::Literal(l)) => Ok(Some(l.value().to_string())),
        Some(other) => bail!("{pred} is not a literal: {other}"),
        None => Ok(None),
    }
}

/// `ht:headers ( [ ht:fieldName "…" ; ht:fieldValue "…" ] … )`.
fn gsp_headers(graph: &Graph, subj: &NamedOrBlankNode) -> Result<Vec<(String, String)>> {
    let Some(head) = graph
        .object_for_subject_predicate(subj.as_ref(), NamedNodeRef::new(&format!("{HT}headers"))?)
    else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for item in read_rdf_list(graph, head.into_owned())? {
        let h = term_to_subject(&item)?;
        let name = gsp_literal(graph, &h, &format!("{HT}fieldName"))?
            .ok_or_else(|| anyhow!("header has no ht:fieldName"))?;
        let value = gsp_literal(graph, &h, &format!("{HT}fieldValue"))?
            .ok_or_else(|| anyhow!("header has no ht:fieldValue"))?;
        out.push((name, value));
    }
    Ok(out)
}

/// `ht:body [ cnt:chars "…" ]`, absent when the message carries no payload.
fn gsp_body(graph: &Graph, subj: &NamedOrBlankNode) -> Result<Option<String>> {
    let Some(body) =
        graph.object_for_subject_predicate(subj.as_ref(), NamedNodeRef::new(&format!("{HT}body"))?)
    else {
        return Ok(None);
    };
    gsp_literal(
        graph,
        &term_to_subject(&body.into_owned())?,
        &format!("{CNT}chars"),
    )
}

/// Optional single-file object (`qt:data`, `ut:data`).
fn opt_file(
    graph: &Graph,
    subj: &NamedOrBlankNode,
    pred_iri: &str,
    base: &Path,
) -> Result<Option<PathBuf>> {
    match graph.object_for_subject_predicate(subj.as_ref(), NamedNodeRef::new(pred_iri)?) {
        Some(TermRef::NamedNode(n)) => Ok(Some(resolve_file(n.as_str(), base)?)),
        Some(other) => bail!("{pred_iri} is not a file IRI: {other}"),
        None => Ok(None),
    }
}

/// Zero or more plain file objects (`qt:graphData`). The graph each file lands
/// in is named by the file's own IRI — the convention the query suite's
/// `GRAPH <relative.ttl>` patterns rely on.
fn files(
    graph: &Graph,
    subj: &NamedOrBlankNode,
    pred_iri: &str,
    base: &Path,
) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for o in graph.objects_for_subject_predicate(subj.as_ref(), NamedNodeRef::new(pred_iri)?) {
        match o {
            TermRef::NamedNode(n) => out.push(resolve_file(n.as_str(), base)?),
            other => bail!("{pred_iri} is not a file IRI: {other}"),
        }
    }
    Ok(out)
}

/// `ut:graphData [ ut:graph <file> ; rdfs:label "<graph IRI>" ]` — the update
/// suite's explicitly-named graphs.
fn labelled_graphs(
    graph: &Graph,
    subj: &NamedOrBlankNode,
    p: &EntryProjector,
    base: &Path,
) -> Result<Vec<(PathBuf, String)>> {
    let gd = NamedNodeRef::new(&p.ut_graph_data_iri)?;
    let g = NamedNodeRef::new(&p.ut_graph_iri)?;
    let label = NamedNodeRef::new(RDFS_LABEL)?;
    let mut out = Vec::new();
    let entries: Vec<_> = graph
        .objects_for_subject_predicate(subj.as_ref(), gd)
        .map(|t| t.into_owned())
        .collect();
    for e in entries {
        let e = term_to_subject(&e)?;
        let file = match graph.object_for_subject_predicate(e.as_ref(), g) {
            Some(TermRef::NamedNode(n)) => resolve_file(n.as_str(), base)?,
            _ => bail!("ut:graphData entry has no ut:graph file IRI"),
        };
        let name = match graph.object_for_subject_predicate(e.as_ref(), label) {
            Some(TermRef::Literal(l)) => l.value().to_string(),
            Some(TermRef::NamedNode(n)) => n.as_str().to_string(),
            _ => bail!("ut:graphData entry has no rdfs:label graph name"),
        };
        out.push((file, name));
    }
    Ok(out)
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
