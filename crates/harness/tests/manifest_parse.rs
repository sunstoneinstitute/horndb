use std::path::PathBuf;

use horndb_harness::manifest;
use horndb_harness::testcase::{Suite, TestKind};

fn fixture(rel: &str) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures");
    p.push(rel);
    p
}

#[test]
fn parses_owl2_fixture_manifest() {
    let cases = manifest::parse(&fixture("owl2/manifest.ttl"), Suite::Owl2).unwrap();
    // Three Stage-0 smoke fixtures plus the rule-coverage expansion
    // (see harness/selected.toml). Bumping this count is intentional
    // when fixtures are added.
    assert!(
        cases.len() >= 16,
        "expected at least 16 owl2 fixtures, got {}",
        cases.len(),
    );
    assert!(cases
        .iter()
        .any(|c| matches!(c.kind, TestKind::PositiveEntailment { .. })));
    assert!(cases
        .iter()
        .any(|c| matches!(c.kind, TestKind::NegativeEntailment { .. })));
    assert!(cases
        .iter()
        .any(|c| matches!(c.kind, TestKind::Inconsistency { .. })));
}

#[test]
fn parses_sparql11_fixture_manifest() {
    let cases = manifest::parse(&fixture("sparql11/manifest.ttl"), Suite::Sparql11).unwrap();
    assert_eq!(cases.len(), 1);
    match &cases[0].kind {
        TestKind::SparqlAsk { expected, .. } => assert!(*expected),
        other => panic!("expected SparqlAsk, got {other:?}"),
    }
}

#[test]
fn parses_rdf12_ntriples_syntax_manifest() {
    // Verbatim mirror of the upstream W3C manifest (see
    // `crates/harness/scripts/fetch-w3c-suites.sh`). The manifest lists
    // more cases than we currently select in `harness/selected.toml`;
    // this test asserts the manifest *parses* and produces the right
    // mix of TestKind::SyntaxPositive / SyntaxNegative entries.
    let cases = manifest::parse(
        &fixture("rdf12-n-triples/manifest.ttl"),
        Suite::Rdf12NTriples,
    )
    .unwrap();
    let pos = cases
        .iter()
        .filter(|c| matches!(c.kind, TestKind::SyntaxPositive { .. }))
        .count();
    let neg = cases
        .iter()
        .filter(|c| matches!(c.kind, TestKind::SyntaxNegative { .. }))
        .count();
    assert!(
        pos >= 4,
        "expected at least 4 SyntaxPositive cases, got {pos}"
    );
    assert!(
        neg >= 6,
        "expected at least 6 SyntaxNegative cases, got {neg}"
    );
    // No other kinds should appear in a syntax-only manifest.
    assert_eq!(pos + neg, cases.len(), "unexpected non-syntax cases");
}

#[test]
fn parses_sparql11_syntax_manifest() {
    // Curated subset of the W3C SPARQL 1.1 syntax sub-suites (issue #110).
    // Asserts the manifest parses and yields only SparqlSyntax{Positive,
    // Negative} entries, covering both the query and update forms.
    let cases = manifest::parse(
        &fixture("sparql11-syntax/manifest.ttl"),
        Suite::Sparql11Syntax,
    )
    .unwrap();
    let pos = cases
        .iter()
        .filter(|c| matches!(c.kind, TestKind::SparqlSyntaxPositive { .. }))
        .count();
    let neg = cases
        .iter()
        .filter(|c| matches!(c.kind, TestKind::SparqlSyntaxNegative { .. }))
        .count();
    let updates = cases
        .iter()
        .filter(|c| {
            matches!(
                c.kind,
                TestKind::SparqlSyntaxPositive { update: true, .. }
                    | TestKind::SparqlSyntaxNegative { update: true, .. }
            )
        })
        .count();
    assert_eq!(pos, 10, "expected 10 positive syntax cases, got {pos}");
    assert_eq!(neg, 5, "expected 5 negative syntax cases, got {neg}");
    assert_eq!(updates, 5, "expected 5 update-form cases, got {updates}");
    // Syntax-only manifest: nothing else should appear.
    assert_eq!(pos + neg, cases.len(), "unexpected non-syntax cases");
}
