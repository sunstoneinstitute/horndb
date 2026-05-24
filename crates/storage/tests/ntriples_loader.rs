use reasoner_storage::loader::ntriples::{load_ntriples_file, LoadStats};
use reasoner_storage::Store;
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures");
    p.push(name);
    p
}

#[test]
fn load_tiny_fixture() {
    let store = Store::in_memory();
    let stats: LoadStats = load_ntriples_file(&store, &fixture("tiny.nt")).unwrap();
    assert_eq!(stats.triples, 6);
    assert_eq!(store.triple_count(), 6);
    // 6 distinct URIs (Alice, Bob, Carol, knows, age, plus none — wait, count.)
    //   subjects: Alice, Bob, Carol           = 3
    //   predicates: knows, age                = 2
    //   objects (URIs): Alice, Bob, Carol     = 0 new (all already counted)
    //   ages 29/30/31 are inline ints, not dict entries.
    // → 5 dictionary entries.
    assert_eq!(store.dictionary().len(), 5);
}

#[test]
fn load_with_literals_fixture() {
    let store = Store::in_memory();
    let stats = load_ntriples_file(&store, &fixture("with_literals.nt")).unwrap();
    assert_eq!(stats.triples, 5);
    assert_eq!(store.triple_count(), 5);
    // Distinct dictionary entries (excluding inline-int "42"):
    //   URIs: s1, s2, s3, s4, name, age, score, p, o          = 9
    //   Literals: "Alice" (plain), "Bob"@en (lang), 3.14 (decimal) = 3
    //   Blank nodes: _:b0                                      = 1
    // Total = 13.
    assert_eq!(store.dictionary().len(), 13);
}

#[test]
fn load_is_idempotent() {
    let store = Store::in_memory();
    load_ntriples_file(&store, &fixture("tiny.nt")).unwrap();
    load_ntriples_file(&store, &fixture("tiny.nt")).unwrap();
    assert_eq!(store.triple_count(), 6, "duplicate triples must collapse");
}

#[test]
fn missing_file_returns_error() {
    let store = Store::in_memory();
    let err = load_ntriples_file(&store, &fixture("does-not-exist.nt"));
    assert!(err.is_err());
}
