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
    assert_eq!(cases.len(), 3);
    assert!(cases
        .iter()
        .any(|c| matches!(c.kind, TestKind::PositiveEntailment { .. })));
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
