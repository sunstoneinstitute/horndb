//! Pattern-based SPARQL Update (`INSERT`/`DELETE … WHERE`) over both
//! Stage-1 backends. Each test applies an update, then queries the store
//! to assert the resulting triples (SPARQL Update has no result set).

use horndb_sparql::api::{execute_query, QueryAnswer};
use horndb_sparql::exec::horn::HornBackend;
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::exec::FullBackend;
use horndb_sparql::parser::parse_update;
use horndb_sparql::update::apply_update;

fn seed<B: FullBackend + Default>(triples: &[(&str, &str, &str)]) -> B {
    use horndb_sparql::algebra::Term;
    let mut b = B::default();
    for (s, p, o) in triples {
        b.insert_triple(
            Term::Iri((*s).to_owned()),
            Term::Iri((*p).to_owned()),
            Term::Iri((*o).to_owned()),
        );
    }
    b
}

/// Return the set of `?o` IRIs for `<subj> <pred> ?o` as sorted strings.
fn objects_of<B: FullBackend>(store: &B, subj: &str, pred: &str) -> Vec<String> {
    let q = format!("SELECT ?o WHERE {{ <{subj}> <{pred}> ?o }}");
    let QueryAnswer::Solutions { rows, .. } = execute_query(&q, store).unwrap() else {
        panic!("expected solutions");
    };
    let mut out: Vec<String> = rows
        .iter()
        .filter_map(|r| match r.get("o") {
            Some(horndb_sparql::algebra::Term::Iri(s)) => Some(s.clone()),
            _ => None,
        })
        .collect();
    out.sort();
    out
}

fn insert_where<B: FullBackend + Default>() {
    let mut store: B = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    let u = parse_update("INSERT { ?s <http://ex/q> ?o } WHERE { ?s <http://ex/p> ?o }").unwrap();
    apply_update(&u, &mut store).unwrap();
    assert_eq!(
        objects_of(&store, "http://ex/a", "http://ex/q"),
        vec!["http://ex/b"]
    );
    // original triple untouched
    assert_eq!(
        objects_of(&store, "http://ex/a", "http://ex/p"),
        vec!["http://ex/b"]
    );
}

fn delete_where<B: FullBackend + Default>() {
    let mut store: B = seed(&[
        ("http://ex/a", "http://ex/p", "http://ex/b"),
        ("http://ex/a", "http://ex/p", "http://ex/c"),
        ("http://ex/a", "http://ex/keep", "http://ex/d"),
    ]);
    let u = parse_update("DELETE WHERE { <http://ex/a> <http://ex/p> ?o }").unwrap();
    apply_update(&u, &mut store).unwrap();
    assert!(objects_of(&store, "http://ex/a", "http://ex/p").is_empty());
    assert_eq!(
        objects_of(&store, "http://ex/a", "http://ex/keep"),
        vec!["http://ex/d"]
    );
}

fn delete_insert_where<B: FullBackend + Default>() {
    let mut store: B = seed(&[("http://ex/a", "http://ex/old", "http://ex/b")]);
    let u = parse_update(
        "DELETE { ?s <http://ex/old> ?o } INSERT { ?s <http://ex/new> ?o } \
         WHERE { ?s <http://ex/old> ?o }",
    )
    .unwrap();
    apply_update(&u, &mut store).unwrap();
    assert!(objects_of(&store, "http://ex/a", "http://ex/old").is_empty());
    assert_eq!(
        objects_of(&store, "http://ex/a", "http://ex/new"),
        vec!["http://ex/b"]
    );
}

/// A template slot bound to nothing (var not in WHERE) drops that triple,
/// not the whole update.
fn ground_safety_drops_unbound<B: FullBackend + Default>() {
    let mut store: B = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    let u = parse_update(
        "INSERT { ?s <http://ex/q> ?missing . ?s <http://ex/r> ?o } \
         WHERE { ?s <http://ex/p> ?o }",
    )
    .unwrap();
    apply_update(&u, &mut store).unwrap();
    // ?missing is unbound -> first template triple dropped
    assert!(objects_of(&store, "http://ex/a", "http://ex/q").is_empty());
    // second template triple is fully ground -> inserted
    assert_eq!(
        objects_of(&store, "http://ex/a", "http://ex/r"),
        vec!["http://ex/b"]
    );
}

fn named_graph_template_rejected<B: FullBackend + Default>() {
    let mut store: B = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    let u = parse_update(
        "INSERT { GRAPH <http://ex/g> { ?s <http://ex/q> ?o } } \
         WHERE { ?s <http://ex/p> ?o }",
    )
    .unwrap();
    let err = apply_update(&u, &mut store).unwrap_err();
    assert!(format!("{err}").to_lowercase().contains("graph"));
}

#[test]
fn mem_insert_where() {
    insert_where::<MemStore>()
}
#[test]
fn horn_insert_where() {
    insert_where::<HornBackend>()
}
#[test]
fn mem_delete_where() {
    delete_where::<MemStore>()
}
#[test]
fn horn_delete_where() {
    delete_where::<HornBackend>()
}
#[test]
fn mem_delete_insert_where() {
    delete_insert_where::<MemStore>()
}
#[test]
fn horn_delete_insert_where() {
    delete_insert_where::<HornBackend>()
}
#[test]
fn mem_ground_safety() {
    ground_safety_drops_unbound::<MemStore>()
}
#[test]
fn horn_ground_safety() {
    ground_safety_drops_unbound::<HornBackend>()
}
#[test]
fn mem_named_graph_rejected() {
    named_graph_template_rejected::<MemStore>()
}
#[test]
fn horn_named_graph_rejected() {
    named_graph_template_rejected::<HornBackend>()
}
