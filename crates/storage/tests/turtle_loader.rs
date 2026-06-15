use horndb_storage::loader::turtle::{
    load_turtle_file, load_turtle_reader, load_turtle_reader_with_base,
};
use horndb_storage::Store;
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures");
    p.push(name);
    p
}

#[test]
fn load_tiny_turtle_matches_ntriples_counts() {
    // tiny.ttl expands (via prefixes, `;`, `,`, bare integers) to the same
    // six triples / five dictionary entries as tiny.nt — bare `30`/`31`/`29`
    // are canonical xsd:integer literals and inline, not dictionary entries.
    let store = Store::in_memory();
    let stats = load_turtle_file(&store, &fixture("tiny.ttl")).unwrap();
    assert_eq!(stats.triples, 6);
    assert_eq!(store.triple_count(), 6);
    assert_eq!(store.dictionary().len(), 5);
    assert!(stats.bytes_read > 0, "file loader records bytes read");
}

#[test]
fn load_with_literals_turtle() {
    // Mirrors with_literals.nt: 5 triples, 13 dictionary entries (the "42"
    // xsd:integer is inline, the decimal / lang / plain literals are not).
    let store = Store::in_memory();
    let stats = load_turtle_file(&store, &fixture("with_literals.ttl")).unwrap();
    assert_eq!(stats.triples, 5);
    assert_eq!(store.triple_count(), 5);
    assert_eq!(store.dictionary().len(), 13);
}

#[test]
fn load_is_idempotent() {
    let store = Store::in_memory();
    load_turtle_file(&store, &fixture("tiny.ttl")).unwrap();
    load_turtle_file(&store, &fixture("tiny.ttl")).unwrap();
    assert_eq!(store.triple_count(), 6, "duplicate triples must collapse");
}

#[test]
fn malformed_turtle_returns_parse_error() {
    let store = Store::in_memory();
    let err = load_turtle_file(&store, &fixture("bad.ttl"));
    assert!(err.is_err(), "truncated turtle must surface a parse error");
}

#[test]
fn missing_file_returns_error() {
    let store = Store::in_memory();
    let err = load_turtle_file(&store, &fixture("does-not-exist.ttl"));
    assert!(err.is_err());
}

#[test]
fn reader_api_loads_inline_source() {
    let store = Store::in_memory();
    let src = "@prefix ex: <http://example.org/> .\nex:a ex:p ex:b .\n";
    let stats = load_turtle_reader(&store, src.as_bytes()).unwrap();
    assert_eq!(stats.triples, 1);
    assert_eq!(store.triple_count(), 1);
}

#[test]
fn relative_iris_resolve_against_file_base() {
    // relative.ttl uses document-relative IRIs (`<#alice>`). The file loader
    // derives a `file://` base from the path, so these resolve and the file
    // loads cleanly.
    let store = Store::in_memory();
    let stats = load_turtle_file(&store, &fixture("relative.ttl")).unwrap();
    assert_eq!(stats.triples, 2);
    assert_eq!(store.triple_count(), 2);
}

#[test]
fn relative_iris_without_base_error() {
    // The same content via the base-less reader API has nothing to resolve the
    // relative IRIs against, so the parser rejects it — confirming the file
    // loader's base is what makes the file load above succeed.
    let store = Store::in_memory();
    let src = "@prefix ex: <http://example.org/> .\n<#alice> ex:knows <#bob> .\n";
    let err = load_turtle_reader(&store, src.as_bytes());
    assert!(err.is_err(), "relative IRIs need a base IRI");
}

#[test]
fn explicit_base_resolves_relative_iris_via_reader() {
    let store = Store::in_memory();
    let src = "@prefix ex: <http://example.org/> .\n<#alice> ex:knows <#bob> .\n";
    let stats =
        load_turtle_reader_with_base(&store, src.as_bytes(), Some("http://example.org/doc"))
            .unwrap();
    assert_eq!(stats.triples, 1);
    assert_eq!(store.triple_count(), 1);
}

#[test]
fn explicit_invalid_base_is_rejected() {
    let store = Store::in_memory();
    let src = "@prefix ex: <http://example.org/> .\nex:a ex:p ex:b .\n";
    let err = load_turtle_reader_with_base(&store, src.as_bytes(), Some("not a valid iri"));
    assert!(
        err.is_err(),
        "an explicit invalid base must be a hard error"
    );
}
