use reasoner_sparql::algebra::{Term, TriplePattern, Var};
use reasoner_sparql::exec::mem::MemStore;
use reasoner_sparql::exec::{Bindings, Executor};

fn t(s: &str, p: &str, o: &str) -> (String, String, String) {
    (s.into(), p.into(), o.into())
}

fn pat_var(s: &str) -> Term {
    Term::Var(Var::new(s))
}
fn pat_iri(s: &str) -> Term {
    Term::Iri(s.into())
}

#[test]
fn mem_executor_matches_single_pattern() {
    let mut s = MemStore::default();
    s.insert(t("http://ex/a", "http://ex/p", "http://ex/b"));
    s.insert(t("http://ex/a", "http://ex/p", "http://ex/c"));
    s.insert(t("http://ex/x", "http://ex/q", "http://ex/y"));

    let pat = TriplePattern {
        subject: pat_iri("http://ex/a"),
        predicate: pat_iri("http://ex/p"),
        object: pat_var("o"),
    };
    let result: Vec<Bindings> = s
        .scan_bgp(std::slice::from_ref(&pat))
        .expect("scan")
        .collect();
    assert_eq!(result.len(), 2);
    let mut got: Vec<String> = result
        .iter()
        .map(|b| match b.get("o").unwrap() {
            Term::Iri(s) => s.clone(),
            other => panic!("unexpected term: {other:?}"),
        })
        .collect();
    got.sort();
    assert_eq!(
        got,
        vec!["http://ex/b".to_owned(), "http://ex/c".to_owned()]
    );
}

#[test]
fn mem_executor_joins_two_patterns_on_shared_var() {
    let mut s = MemStore::default();
    s.insert(t("http://ex/a", "http://ex/p", "http://ex/b"));
    s.insert(t("http://ex/b", "http://ex/q", "http://ex/c"));
    s.insert(t("http://ex/z", "http://ex/q", "http://ex/c"));

    let p1 = TriplePattern {
        subject: pat_iri("http://ex/a"),
        predicate: pat_iri("http://ex/p"),
        object: pat_var("o"),
    };
    let p2 = TriplePattern {
        subject: pat_var("o"),
        predicate: pat_iri("http://ex/q"),
        object: pat_var("z"),
    };

    let result: Vec<Bindings> = s.scan_bgp(&[p1, p2]).expect("scan").collect();
    assert_eq!(result.len(), 1);
    let b = &result[0];
    assert_eq!(b.get("o").unwrap(), &Term::Iri("http://ex/b".into()));
    assert_eq!(b.get("z").unwrap(), &Term::Iri("http://ex/c".into()));
}
