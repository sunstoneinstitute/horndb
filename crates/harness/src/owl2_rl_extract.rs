//! Ingestion of the W3C OWL 2 RL profile test cases.
//!
//! The canonical source for the OWL 2 RL test subset is the aggregate
//! manifest at <https://www.w3.org/2009/11/owl-test/profile-RL.rdf>.
//! Every `<test:TestCase>` inside that file carries its premise and
//! conclusion ontologies as **embedded RDF/XML strings** (literals of
//! type `xsd:string`), not as file references — so the in-tree
//! manifest parser (`manifest.rs`, which expects `mf:action`/`mf:result`
//! file IRIs) cannot read the W3C file directly.
//!
//! This module turns the W3C aggregate into the shape the harness
//! already understands:
//!
//! * each embedded premise / conclusion ontology is decoded, re-parsed
//!   as RDF/XML, and re-serialized as Turtle into a sibling
//!   `<id>.premise.ttl` / `<id>.conclusion.ttl` file;
//! * a single synthesised `manifest.ttl` is written next to those
//!   files, with one `mf:PositiveEntailmentTest` /
//!   `mf:NegativeEntailmentTest` / `mf:ConsistencyTest` /
//!   `mf:InconsistencyTest` entry per `(TestCase, kind)` pair (a
//!   W3C case typed as both `PositiveEntailmentTest` and
//!   `ConsistencyTest` becomes two entries).
//!
//! The W3C file's `<!DOCTYPE>` declares four entities (`&rdf;`,
//! `&rdfs;`, `&owl;`, `&test;`) using **single-quoted** values; oxrdfio
//! and oxttl reject those, and even quick-xml needs a hint. Rather than
//! teach the parsers, the extractor pre-substitutes those four entities
//! in-memory before parsing (the four names do not collide with the XML
//! built-ins `&lt;` / `&gt;` / `&amp;` / `&quot;` / `&apos;`, which are
//! left intact for quick-xml to decode normally).
//!
//! Stage-1 scope: the extractor honours `test:rdfXmlPremiseOntology` /
//! `test:rdfXmlConclusionOntology` only. The functional-syntax variants
//! (`test:fs*Ontology`) and the OWL/XML variants are out of scope until
//! we ship parsers for them.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use oxrdfio::{RdfFormat, RdfParser, RdfSerializer};
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

/// Namespace for the W3C OWL 2 test ontology.
const TEST_NS: &str = "http://www.w3.org/2007/OWL/testOntology#";

/// Substitute the four DOCTYPE-defined entities in `profile-RL.rdf` with
/// their expansions. Done before parsing because (a) oxrdfio rejects
/// single-quoted ENTITY values and (b) we strip the DOCTYPE entirely
/// downstream so the entities would otherwise be undefined.
fn expand_doctype_entities(input: &str) -> String {
    input
        .replace("&test;", TEST_NS)
        .replace("&rdfs;", "http://www.w3.org/2000/01/rdf-schema#")
        .replace("&owl;", "http://www.w3.org/2002/07/owl#")
        .replace("&rdf;", "http://www.w3.org/1999/02/22-rdf-syntax-ns#")
}

/// One emitted entry in the synthesised manifest.
#[derive(Debug, Clone)]
struct ManifestEntry {
    /// Stable, file-system-safe id used as both the manifest fragment
    /// (`<#id>`) and as the filename stem of the sibling .ttl files.
    id: String,
    /// Human-readable name. Pulled from `test:identifier` plus a
    /// kind-specific suffix.
    name: String,
    /// Which `mf:` type to emit.
    kind: ManifestKind,
    /// File system path (relative to the manifest) of the premise.
    premise: PathBuf,
    /// File system path (relative to the manifest) of the conclusion.
    /// Only populated for entailment-flavoured kinds.
    conclusion: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy)]
enum ManifestKind {
    PositiveEntailment,
    NegativeEntailment,
    Consistency,
    Inconsistency,
}

impl ManifestKind {
    fn mf_type(self) -> &'static str {
        match self {
            ManifestKind::PositiveEntailment => "mf:PositiveEntailmentTest",
            ManifestKind::NegativeEntailment => "mf:NegativeEntailmentTest",
            ManifestKind::Consistency => "mf:ConsistencyTest",
            ManifestKind::Inconsistency => "mf:InconsistencyTest",
        }
    }

    fn id_suffix(self) -> &'static str {
        match self {
            ManifestKind::PositiveEntailment => "pe",
            ManifestKind::NegativeEntailment => "ne",
            ManifestKind::Consistency => "cons",
            ManifestKind::Inconsistency => "incons",
        }
    }
}

/// Accumulator for a single `<test:TestCase>` element.
#[derive(Default)]
struct TestCaseDraft {
    /// `test:identifier` literal (human-readable).
    identifier: Option<String>,
    /// Expanded IRIs from each `<rdf:type rdf:resource="..."/>`.
    types: Vec<String>,
    /// Decoded body of `<test:rdfXmlPremiseOntology>`, if present.
    premise: Option<String>,
    /// Decoded body of `<test:rdfXmlConclusionOntology>`, if present.
    conclusion: Option<String>,
}

/// Result counters returned by [`extract`].
#[derive(Debug, Default)]
pub struct ExtractStats {
    pub cases_scanned: usize,
    pub entries_emitted: usize,
    pub turtle_files_written: usize,
    pub skipped_no_payload: usize,
}

/// Read `source` (the W3C `profile-RL.rdf` aggregate), produce sibling
/// `<id>.{premise,conclusion}.ttl` files under `out_dir`, and write a
/// synthesised `manifest.ttl` to `out_dir/manifest.ttl`.
pub fn extract(source: &Path, out_dir: &Path) -> Result<ExtractStats> {
    fs::create_dir_all(out_dir)
        .with_context(|| format!("creating out dir {}", out_dir.display()))?;
    let raw = fs::read_to_string(source)
        .with_context(|| format!("reading source {}", source.display()))?;
    let expanded = expand_doctype_entities(&raw);
    let drafts = parse_test_cases(&expanded)
        .with_context(|| format!("parsing test cases from {}", source.display()))?;

    let mut stats = ExtractStats {
        cases_scanned: drafts.len(),
        ..ExtractStats::default()
    };
    let mut entries = Vec::new();
    // We materialise each (identifier, premise-payload) pair only once
    // even when the same TestCase emits more than one entry (e.g. a
    // PositiveEntailment+Consistency W3C case becomes a -pe and a -cons
    // entry that share the premise).
    let mut premise_written: BTreeMap<String, PathBuf> = BTreeMap::new();
    let mut conclusion_written: BTreeMap<String, PathBuf> = BTreeMap::new();
    for (case_idx, draft) in drafts.into_iter().enumerate() {
        let raw_id = draft
            .identifier
            .clone()
            .unwrap_or_else(|| format!("case-{case_idx}"));
        let stem = sanitise_id(&raw_id);
        let kinds = pick_kinds(&draft.types);
        if kinds.is_empty() {
            stats.skipped_no_payload += 1;
            continue;
        }
        // Materialise premise / conclusion once per stem.
        if !premise_written.contains_key(&stem) {
            let Some(premise_xml) = draft.premise.as_deref() else {
                stats.skipped_no_payload += 1;
                continue;
            };
            let path = out_dir.join(format!("{stem}.premise.ttl"));
            write_turtle(premise_xml, &path)
                .with_context(|| format!("converting premise for {raw_id}"))?;
            stats.turtle_files_written += 1;
            premise_written.insert(stem.clone(), path);
        }
        if let Some(conclusion_xml) = draft.conclusion.as_deref() {
            if !conclusion_written.contains_key(&stem) {
                let path = out_dir.join(format!("{stem}.conclusion.ttl"));
                write_turtle(conclusion_xml, &path)
                    .with_context(|| format!("converting conclusion for {raw_id}"))?;
                stats.turtle_files_written += 1;
                conclusion_written.insert(stem.clone(), path);
            }
        }
        for kind in kinds {
            // PositiveEntailment / NegativeEntailment require a
            // conclusion; skip if the W3C case is missing one (the
            // file does occasionally type a case as
            // PositiveEntailmentTest while omitting the conclusion
            // body, in which case the case is unusable).
            let needs_conclusion = matches!(
                kind,
                ManifestKind::PositiveEntailment | ManifestKind::NegativeEntailment
            );
            if needs_conclusion && !conclusion_written.contains_key(&stem) {
                stats.skipped_no_payload += 1;
                continue;
            }
            let entry_id = format!("{stem}-{}", kind.id_suffix());
            let entry_name = format!("{raw_id} ({})", kind.id_suffix());
            entries.push(ManifestEntry {
                id: entry_id,
                name: entry_name,
                kind,
                premise: PathBuf::from(format!("{stem}.premise.ttl")),
                conclusion: if needs_conclusion {
                    Some(PathBuf::from(format!("{stem}.conclusion.ttl")))
                } else {
                    None
                },
            });
            stats.entries_emitted += 1;
        }
    }

    let manifest = render_manifest(&entries);
    let manifest_path = out_dir.join("manifest.ttl");
    fs::write(&manifest_path, manifest)
        .with_context(|| format!("writing manifest {}", manifest_path.display()))?;
    Ok(stats)
}

/// Sanitise `id` for use as both a filename stem and a Turtle relative
/// IRI fragment. Drops anything outside `[A-Za-z0-9._-]`.
fn sanitise_id(id: &str) -> String {
    let mut out = String::with_capacity(id.len());
    for ch in id.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    // Collapse runs of underscores so e.g. `FS2RDF--ar` stays readable.
    let mut squashed = String::with_capacity(out.len());
    let mut prev_us = false;
    for ch in out.chars() {
        if ch == '_' {
            if !prev_us {
                squashed.push('_');
            }
            prev_us = true;
        } else {
            squashed.push(ch);
            prev_us = false;
        }
    }
    let trimmed = squashed.trim_matches('_').to_string();
    if trimmed.is_empty() {
        "case".to_string()
    } else {
        trimmed
    }
}

/// Decide which `mf:` entries a W3C case should produce, based on the
/// `test:*Test` rdf:type values it carries. `ProfileIdentificationTest`
/// is metadata and is ignored.
fn pick_kinds(types: &[String]) -> Vec<ManifestKind> {
    let pe_iri = format!("{TEST_NS}PositiveEntailmentTest");
    let ne_iri = format!("{TEST_NS}NegativeEntailmentTest");
    let cons_iri = format!("{TEST_NS}ConsistencyTest");
    let incons_iri = format!("{TEST_NS}InconsistencyTest");
    let mut out = Vec::new();
    if types.iter().any(|t| t == &pe_iri) {
        out.push(ManifestKind::PositiveEntailment);
    }
    if types.iter().any(|t| t == &ne_iri) {
        out.push(ManifestKind::NegativeEntailment);
    }
    if types.iter().any(|t| t == &cons_iri) {
        out.push(ManifestKind::Consistency);
    }
    if types.iter().any(|t| t == &incons_iri) {
        out.push(ManifestKind::Inconsistency);
    }
    out
}

/// Parse the (entity-expanded) profile-RL.rdf body into one draft per
/// `<test:TestCase>` element.
fn parse_test_cases(xml: &str) -> Result<Vec<TestCaseDraft>> {
    let mut reader = Reader::from_str(xml);
    let config = reader.config_mut();
    config.trim_text(false);
    config.expand_empty_elements = false;

    let mut buf = Vec::new();
    let mut drafts: Vec<TestCaseDraft> = Vec::new();
    let mut current: Option<TestCaseDraft> = None;
    // While inside a TestCase, track which leaf literal element we are
    // collecting so we can attribute the next Text event correctly.
    let mut text_target: Option<LiteralField> = None;
    // Accumulate text across CDATA / multi-event payloads.
    let mut text_buf = String::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Err(e) => bail!("XML parse error at {}: {e}", reader.buffer_position()),
            Ok(Event::Eof) => break,
            Ok(Event::Start(ref e)) => {
                let q = e.name();
                let name = local_name_bytes(q.as_ref());
                if name == "TestCase" && current.is_none() {
                    current = Some(TestCaseDraft::default());
                    continue;
                }
                if current.is_some() {
                    text_target = literal_target_for(name);
                    text_buf.clear();
                }
            }
            Ok(Event::Empty(ref e)) => {
                if let Some(draft) = current.as_mut() {
                    let q = e.name();
                    let name = local_name_bytes(q.as_ref());
                    if name == "type" {
                        if let Some(res) = attr_value(e, "resource")? {
                            draft.types.push(res);
                        }
                    }
                }
            }
            Ok(Event::End(ref e)) => {
                let name_owned = e.name();
                let name = local_name_bytes(name_owned.as_ref());
                if name == "TestCase" {
                    if let Some(d) = current.take() {
                        drafts.push(d);
                    }
                    text_target = None;
                    text_buf.clear();
                    continue;
                }
                if let (Some(draft), Some(target)) = (current.as_mut(), text_target.take()) {
                    let value = std::mem::take(&mut text_buf);
                    match target {
                        LiteralField::Identifier => draft.identifier = Some(value),
                        LiteralField::Premise => draft.premise = Some(value),
                        LiteralField::Conclusion => draft.conclusion = Some(value),
                    }
                }
            }
            Ok(Event::Text(t)) => {
                if text_target.is_some() {
                    text_buf.push_str(&t.unescape()?);
                }
            }
            Ok(Event::CData(t)) => {
                if text_target.is_some() {
                    text_buf.push_str(&String::from_utf8_lossy(&t));
                }
            }
            Ok(_) => {}
        }
        buf.clear();
    }
    Ok(drafts)
}

#[derive(Debug, Clone, Copy)]
enum LiteralField {
    Identifier,
    Premise,
    Conclusion,
}

fn literal_target_for(local: &str) -> Option<LiteralField> {
    match local {
        "identifier" => Some(LiteralField::Identifier),
        "rdfXmlPremiseOntology" => Some(LiteralField::Premise),
        "rdfXmlConclusionOntology" => Some(LiteralField::Conclusion),
        _ => None,
    }
}

fn local_name_bytes(name: &[u8]) -> &str {
    let full = std::str::from_utf8(name).unwrap_or("");
    full.rsplit(':').next().unwrap_or(full)
}

fn attr_value(e: &BytesStart<'_>, want: &str) -> Result<Option<String>> {
    for attr in e.attributes() {
        let attr = attr.map_err(|err| anyhow!("attribute parse error: {err}"))?;
        let key = std::str::from_utf8(attr.key.as_ref()).unwrap_or("");
        let local = key.rsplit(':').next().unwrap_or(key);
        if local == want {
            let v = attr.unescape_value().map_err(|err| anyhow!("{err}"))?;
            return Ok(Some(v.into_owned()));
        }
    }
    Ok(None)
}

/// Re-parse `body` (an RDF/XML document text) and serialise the result
/// as Turtle to `dest`.
fn write_turtle(body: &str, dest: &Path) -> Result<()> {
    let base_iri = format!("file://{}", dest.display());
    let parser = RdfParser::from_format(RdfFormat::RdfXml).with_base_iri(&base_iri)?;
    let mut serializer = RdfSerializer::from_format(RdfFormat::Turtle).for_writer(Vec::<u8>::new());
    for quad in parser.for_slice(body.as_bytes()) {
        let quad = quad.with_context(|| {
            format!("re-parsing embedded RDF/XML targeted at {}", dest.display())
        })?;
        serializer.serialize_quad(&quad)?;
    }
    let out = serializer
        .finish()
        .with_context(|| format!("finalising turtle {}", dest.display()))?;
    fs::write(dest, out).with_context(|| format!("writing turtle {}", dest.display()))?;
    Ok(())
}

/// Render the synthesised mf:Manifest as Turtle.
fn render_manifest(entries: &[ManifestEntry]) -> String {
    let mut out = String::new();
    out.push_str("@prefix mf:  <http://www.w3.org/2001/sw/DataAccess/tests/test-manifest#> .\n");
    out.push_str("@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .\n");
    out.push_str("@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n\n");
    out.push_str("# Synthesised from the W3C OWL 2 RL profile aggregate\n");
    out.push_str("# (https://www.w3.org/2009/11/owl-test/profile-RL.rdf) by the\n");
    out.push_str("# horndb-harness `extract-owl2-rl` subcommand. Each TestCase in\n");
    out.push_str("# the W3C file produces one entry per applicable kind\n");
    out.push_str("# (PositiveEntailmentTest, NegativeEntailmentTest, ConsistencyTest,\n");
    out.push_str("# InconsistencyTest). Premises and conclusions live in sibling\n");
    out.push_str("# `<id>.premise.ttl` / `<id>.conclusion.ttl` files; the original\n");
    out.push_str("# embedded RDF/XML payloads were re-serialized as Turtle here.\n\n");
    out.push_str("<#manifest> a mf:Manifest ;\n");
    out.push_str("    mf:entries (\n");
    for entry in entries {
        out.push_str(&format!("        <#{}>\n", entry.id));
    }
    out.push_str("    ) .\n\n");
    for entry in entries {
        out.push_str(&format!(
            "<#{id}> a {ty} ;\n    mf:name {name} ;\n    mf:action <{premise}>",
            id = entry.id,
            ty = entry.kind.mf_type(),
            name = turtle_literal(&entry.name),
            premise = entry.premise.display(),
        ));
        if let Some(c) = &entry.conclusion {
            out.push_str(&format!(" ;\n    mf:result <{}>", c.display()));
        }
        out.push_str(" .\n\n");
    }
    out
}

fn turtle_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    const SAMPLE: &str = r##"<?xml version="1.0"?>
<!DOCTYPE rdf:RDF[
    <!ENTITY rdf 'http://www.w3.org/1999/02/22-rdf-syntax-ns#'>
    <!ENTITY test 'http://www.w3.org/2007/OWL/testOntology#'>
    <!ENTITY owl 'http://www.w3.org/2002/07/owl#'>
]>
<rdf:RDF xmlns:rdf="&rdf;" xmlns:test="&test;" xmlns:owl="&owl;">
    <test:TestCase rdf:about="http://owl.semanticweb.org/id/Sample-PE">
        <rdf:type rdf:resource="&test;ProfileIdentificationTest"/>
        <rdf:type rdf:resource="&test;PositiveEntailmentTest"/>
        <rdf:type rdf:resource="&test;ConsistencyTest"/>
        <test:identifier rdf:datatype="http://www.w3.org/2001/XMLSchema#string">sample-pe</test:identifier>
        <test:profile rdf:resource="&test;RL"/>
        <test:rdfXmlPremiseOntology rdf:datatype="http://www.w3.org/2001/XMLSchema#string">&lt;rdf:RDF xml:base="http://example.org/" xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:owl="http://www.w3.org/2002/07/owl#" xmlns:rdfs="http://www.w3.org/2000/01/rdf-schema#"&gt;
            &lt;owl:Ontology rdf:about=""/&gt;
            &lt;owl:Class rdf:about="#A"&gt;&lt;rdfs:subClassOf rdf:resource="#B"/&gt;&lt;/owl:Class&gt;
            &lt;rdf:Description rdf:about="#x"&gt;&lt;rdf:type rdf:resource="#A"/&gt;&lt;/rdf:Description&gt;
        &lt;/rdf:RDF&gt;</test:rdfXmlPremiseOntology>
        <test:rdfXmlConclusionOntology rdf:datatype="http://www.w3.org/2001/XMLSchema#string">&lt;rdf:RDF xml:base="http://example.org/" xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"&gt;
            &lt;rdf:Description rdf:about="#x"&gt;&lt;rdf:type rdf:resource="#B"/&gt;&lt;/rdf:Description&gt;
        &lt;/rdf:RDF&gt;</test:rdfXmlConclusionOntology>
    </test:TestCase>
    <test:TestCase rdf:about="http://owl.semanticweb.org/id/Sample-INC">
        <rdf:type rdf:resource="&test;ProfileIdentificationTest"/>
        <rdf:type rdf:resource="&test;InconsistencyTest"/>
        <test:identifier rdf:datatype="http://www.w3.org/2001/XMLSchema#string">sample-incons</test:identifier>
        <test:rdfXmlPremiseOntology rdf:datatype="http://www.w3.org/2001/XMLSchema#string">&lt;rdf:RDF xml:base="http://example.org/" xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:owl="http://www.w3.org/2002/07/owl#"&gt;
            &lt;owl:Ontology rdf:about=""/&gt;
            &lt;rdf:Description rdf:about="#x"&gt;&lt;rdf:type rdf:resource="http://www.w3.org/2002/07/owl#Nothing"/&gt;&lt;/rdf:Description&gt;
        &lt;/rdf:RDF&gt;</test:rdfXmlPremiseOntology>
    </test:TestCase>
</rdf:RDF>
"##;

    #[test]
    fn sanitise_id_strips_unsafe_chars() {
        assert_eq!(
            sanitise_id("FS2RDF-disjoint-classes-2-ar"),
            "FS2RDF-disjoint-classes-2-ar"
        );
        assert_eq!(sanitise_id("foo/bar baz"), "foo_bar_baz");
        assert_eq!(sanitise_id("  "), "case");
    }

    #[test]
    fn pick_kinds_selects_all_applicable() {
        let types = vec![
            format!("{TEST_NS}ProfileIdentificationTest"),
            format!("{TEST_NS}PositiveEntailmentTest"),
            format!("{TEST_NS}ConsistencyTest"),
        ];
        let kinds = pick_kinds(&types);
        assert_eq!(kinds.len(), 2);
    }

    #[test]
    fn parse_sample_extracts_two_cases() {
        let expanded = expand_doctype_entities(SAMPLE);
        let drafts = parse_test_cases(&expanded).unwrap();
        assert_eq!(drafts.len(), 2);
        let first = &drafts[0];
        assert_eq!(first.identifier.as_deref(), Some("sample-pe"));
        assert!(first
            .types
            .iter()
            .any(|t| t == &format!("{TEST_NS}PositiveEntailmentTest")));
        assert!(first.premise.is_some());
        assert!(first.conclusion.is_some());
        let second = &drafts[1];
        assert_eq!(second.identifier.as_deref(), Some("sample-incons"));
        assert!(second
            .types
            .iter()
            .any(|t| t == &format!("{TEST_NS}InconsistencyTest")));
        assert!(second.premise.is_some());
        assert!(second.conclusion.is_none());
    }

    #[test]
    fn extract_writes_manifest_and_turtles() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("profile-RL.rdf");
        std::fs::File::create(&src)
            .unwrap()
            .write_all(SAMPLE.as_bytes())
            .unwrap();
        let stats = extract(&src, dir.path()).unwrap();
        assert_eq!(stats.cases_scanned, 2);
        // sample-pe → -pe + -cons; sample-incons → -incons
        assert_eq!(stats.entries_emitted, 3);
        // sample-pe shares its premise.ttl across two entries, so we
        // wrote only one premise + one conclusion for it, plus a
        // premise for sample-incons. Total = 3 .ttl files.
        assert_eq!(stats.turtle_files_written, 3);
        let manifest = std::fs::read_to_string(dir.path().join("manifest.ttl")).unwrap();
        assert!(manifest.contains("mf:PositiveEntailmentTest"));
        assert!(manifest.contains("mf:ConsistencyTest"));
        assert!(manifest.contains("mf:InconsistencyTest"));
        assert!(manifest.contains("<#sample-pe-pe>"));
        assert!(manifest.contains("<#sample-pe-cons>"));
        assert!(manifest.contains("<#sample-incons-incons>"));
        let premise = std::fs::read_to_string(dir.path().join("sample-pe.premise.ttl")).unwrap();
        // The premise contains both the subClassOf axiom and the rdf:type
        // assertion, re-serialised as Turtle.
        assert!(premise.contains("subClassOf"));
        assert!(premise.contains("#x"));
    }
}
