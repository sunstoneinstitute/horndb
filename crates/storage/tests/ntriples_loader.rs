use horndb_storage::loader::ntriples::{load_ntriples_file, LoadStats};
use horndb_storage::{Store, TermKind};
use oxrdf::{Literal, NamedNode, Term, Triple};
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
fn load_triple_term_fixture() {
    // RDF 1.2 N-Triples: `<<( s p o )>>` in object position. The fixture
    // has 3 quads, two of which embed the *same* inner triple term — the
    // dictionary must dedupe them so only two distinct triple-term IDs
    // are minted overall.
    let store = Store::in_memory();
    let stats = load_ntriples_file(&store, &fixture("triple_term.nt")).unwrap();
    assert_eq!(stats.triples, 3);
    assert_eq!(store.triple_count(), 3);

    // The dictionary stores triple terms recursively: the inner
    // `Term::Triple` is one dictionary entry, the Bob/age/"30" inside it
    // are *not* separately interned (they live inside the Triple struct
    // and round-trip via `lookup` together). So the expected entries are
    // just the four outer URIs plus the two distinct triple terms:
    //   URIs:  Alice, Carol, Eve, claims          = 4
    //   Triple terms: <<( Bob age "30" )>>,
    //                 <<( Bob age "40" )>>        = 2
    // Total = 6.
    assert_eq!(store.dictionary().len(), 6);

    // The dedupe invariant: re-interning the same <<( Bob age "30" )>>
    // triple term must return the same TermId without growing the dict.
    let bob = NamedNode::new("http://example.org/Bob").unwrap();
    let age = NamedNode::new("http://example.org/age").unwrap();
    let thirty = Literal::new_simple_literal("30");
    let inner = Triple::new(bob, age, Term::Literal(thirty));
    let tt = Term::Triple(Box::new(inner));
    let before = store.dictionary().len();
    let tt_id = store.dictionary().intern(&tt).unwrap();
    assert_eq!(
        tt_id.kind(),
        TermKind::TripleTerm,
        "kind tag must reflect TripleTerm"
    );
    assert_eq!(
        store.dictionary().len(),
        before,
        "the triple term was already in the dict (matched by structural Eq)"
    );
    // Lookup round-trips back to the same Term::Triple structure.
    let back = store.dictionary().lookup(tt_id).unwrap();
    assert_eq!(back, tt);
}

#[test]
fn missing_file_returns_error() {
    let store = Store::in_memory();
    let err = load_ntriples_file(&store, &fixture("does-not-exist.nt"));
    assert!(err.is_err());
}
