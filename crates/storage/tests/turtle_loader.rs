use horndb_storage::loader::turtle::{
    load_turtle_file, load_turtle_reader, load_turtle_reader_with_base,
};
use horndb_storage::{Store, TermId};
use oxrdf::Term;
use std::io::Write;
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

#[test]
fn file_base_percent_encodes_reserved_path_chars() {
    // A path containing an IRI-reserved character (`#`) must be percent-encoded
    // before it becomes the document base, otherwise the raw `#` would be read
    // as a fragment delimiter and relative IRIs would resolve against a
    // truncated, wrong base. Write such a file and confirm the subject of the
    // relative-IRI triple resolves to the encoded form.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("doc#frag.ttl");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"<#alice> <http://example.org/p> <http://example.org/o> .\n")
            .unwrap();
    }

    let store = Store::in_memory();
    let stats = load_turtle_file(&store, &path).unwrap();
    assert_eq!(stats.triples, 1);

    // Collect the single subject IRI back out of the store.
    let triples = store.scan_all_term_ids();
    assert_eq!(triples.len(), 1);
    let s_id: TermId = triples[0].0;
    let subject = store.dictionary().lookup(s_id).unwrap();
    let iri = match subject {
        Term::NamedNode(n) => n.into_string(),
        other => panic!("expected a NamedNode subject, got {other:?}"),
    };

    // The `#` in the file name is encoded as %23; `#alice` is the document
    // fragment appended to the (encoded) base, so the resolved IRI ends with
    // `doc%23frag.ttl#alice` and never contains the raw `doc#frag.ttl`.
    assert!(
        iri.contains("doc%23frag.ttl#alice"),
        "subject IRI should resolve against the percent-encoded base: {iri}"
    );
    assert!(
        !iri.contains("doc#frag.ttl"),
        "raw `#` must not leak into the base: {iri}"
    );
}
