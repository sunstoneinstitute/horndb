use oxrdf::{BlankNode, Literal, NamedNode, Term};
use reasoner_storage::{Dictionary, TermKind};

fn uri(s: &str) -> Term {
    Term::NamedNode(NamedNode::new(s).unwrap())
}
fn bnode(s: &str) -> Term {
    Term::BlankNode(BlankNode::new(s).unwrap())
}
fn plain(s: &str) -> Term {
    Term::Literal(Literal::new_simple_literal(s))
}
fn lang(s: &str, t: &str) -> Term {
    Term::Literal(Literal::new_language_tagged_literal(s, t).unwrap())
}
fn typed(s: &str, dt: &str) -> Term {
    Term::Literal(Literal::new_typed_literal(s, NamedNode::new(dt).unwrap()))
}

#[test]
fn intern_uri_returns_uri_kind() {
    let dict = Dictionary::new();
    let id = dict.intern(&uri("http://example.org/Alice")).unwrap();
    assert_eq!(id.kind(), TermKind::Uri);
    assert_eq!(dict.lookup(id).unwrap(), uri("http://example.org/Alice"));
}

#[test]
fn intern_twice_returns_same_id() {
    let dict = Dictionary::new();
    let a = dict.intern(&uri("http://example.org/x")).unwrap();
    let b = dict.intern(&uri("http://example.org/x")).unwrap();
    assert_eq!(a, b);
    assert_eq!(dict.len(), 1);
}

#[test]
fn intern_distinguishes_kinds() {
    let dict = Dictionary::new();
    let u = dict.intern(&uri("http://example.org/x")).unwrap();
    let b = dict.intern(&bnode("x")).unwrap();
    let p = dict.intern(&plain("x")).unwrap();
    let l = dict.intern(&lang("x", "en")).unwrap();
    let t = dict.intern(&typed("x", "http://example.org/T")).unwrap();
    assert_eq!(u.kind(), TermKind::Uri);
    assert_eq!(b.kind(), TermKind::Blank);
    assert_eq!(p.kind(), TermKind::PlainLiteral);
    assert_eq!(l.kind(), TermKind::LangLiteral);
    assert_eq!(t.kind(), TermKind::TypedLiteral);
    // All distinct IDs.
    let ids = [u, b, p, l, t];
    for i in 0..ids.len() {
        for j in (i + 1)..ids.len() {
            assert_ne!(ids[i], ids[j]);
        }
    }
}

#[test]
fn small_xsd_integer_is_inlined() {
    let dict = Dictionary::new();
    let id = dict
        .intern(&typed("42", "http://www.w3.org/2001/XMLSchema#integer"))
        .unwrap();
    assert_eq!(id.kind(), TermKind::InlineInt);
    assert_eq!(id.as_inline_int(), Some(42));
    // No dictionary entry was created.
    assert_eq!(dict.len(), 0);
    assert_eq!(
        dict.lookup(id).unwrap(),
        typed("42", "http://www.w3.org/2001/XMLSchema#integer")
    );
}

#[test]
fn large_xsd_integer_falls_back_to_dictionary() {
    let dict = Dictionary::new();
    let big = format!("{}", i64::MAX);
    let id = dict
        .intern(&typed(&big, "http://www.w3.org/2001/XMLSchema#integer"))
        .unwrap();
    assert_eq!(id.kind(), TermKind::TypedLiteral);
    assert_eq!(dict.len(), 1);
}

#[test]
fn dictionary_indices_start_at_one() {
    let dict = Dictionary::new();
    let id = dict.intern(&uri("http://example.org/x")).unwrap();
    assert_eq!(id.payload(), 1, "first index must be 1, not 0");
}

#[test]
fn concurrent_intern_returns_same_id() {
    use std::sync::Arc;
    use std::thread;
    let dict = Arc::new(Dictionary::new());
    let mut handles = vec![];
    for _ in 0..8 {
        let d = dict.clone();
        handles.push(thread::spawn(move || {
            d.intern(&uri("http://example.org/shared")).unwrap()
        }));
    }
    let ids: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let first = ids[0];
    for id in &ids {
        assert_eq!(*id, first);
    }
    assert_eq!(dict.len(), 1);
}
