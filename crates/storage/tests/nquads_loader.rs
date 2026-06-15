use horndb_storage::loader::nquads::{load_nquads_file, load_nquads_reader};
use horndb_storage::{GraphId, MemoryTier, Store, DEFAULT_GRAPH};
use oxrdf::{NamedNode, Term};
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures");
    p.push(name);
    p
}

fn iri(s: &str) -> Term {
    Term::NamedNode(NamedNode::new(s).unwrap())
}

/// Count triples stored under `(graph, predicate)` by scanning the predicate
/// partition directly. `intern_graph_uri` is idempotent, so resolving the graph
/// label here returns the id minted during load without growing the dictionary.
fn count_in_graph(store: &Store, g: GraphId, predicate: &Term) -> usize {
    let p_id = match store.dictionary().get(predicate) {
        Some(id) => id,
        None => return 0,
    };
    let mt = store
        .tier()
        .as_any()
        .downcast_ref::<MemoryTier>()
        .expect("Stage-1 store wraps MemoryTier");
    mt.with_predicate(g, p_id, |part| part.scan().count())
        .unwrap_or(0)
}

#[test]
fn nquads_route_to_named_graphs() {
    let store = Store::in_memory();
    let stats = load_nquads_file(&store, &fixture("named_graphs.nq")).unwrap();
    assert_eq!(stats.triples, 4);
    assert_eq!(store.triple_count(), 4);
    assert!(stats.bytes_read > 0);

    // Three graphs: the default graph plus g1 and g2.
    assert_eq!(store.stats().graphs, 3);
    assert!(store.has_named_graph_data());

    let g1 = store
        .intern_graph_uri(&iri("http://example.org/g1"))
        .unwrap();
    let g2 = store
        .intern_graph_uri(&iri("http://example.org/g2"))
        .unwrap();
    let knows = iri("http://example.org/knows");
    let age = iri("http://example.org/age");

    // Default graph: only `Alice knows Bob`.
    assert_eq!(count_in_graph(&store, DEFAULT_GRAPH, &knows), 1);
    assert_eq!(count_in_graph(&store, DEFAULT_GRAPH, &age), 0);
    // g1: the two `knows` quads share the same graph label.
    assert_eq!(count_in_graph(&store, g1, &knows), 2);
    // g2: the single `age` quad.
    assert_eq!(count_in_graph(&store, g2, &age), 1);
    assert_eq!(count_in_graph(&store, g2, &knows), 0);
}

#[test]
fn triples_without_graph_term_land_in_default_graph() {
    // N-Quads lines with no fourth term are default-graph triples; no named
    // graph data is created.
    let store = Store::in_memory();
    let src = concat!(
        "<http://example.org/a> <http://example.org/p> <http://example.org/b> .\n",
        "<http://example.org/c> <http://example.org/p> <http://example.org/d> .\n",
    );
    let stats = load_nquads_reader(&store, src.as_bytes()).unwrap();
    assert_eq!(stats.triples, 2);
    assert_eq!(store.triple_count(), 2);
    assert!(!store.has_named_graph_data());
    assert_eq!(store.stats().graphs, 1);
    assert_eq!(
        count_in_graph(&store, DEFAULT_GRAPH, &iri("http://example.org/p")),
        2
    );
}

#[test]
fn load_is_idempotent() {
    let store = Store::in_memory();
    load_nquads_file(&store, &fixture("named_graphs.nq")).unwrap();
    load_nquads_file(&store, &fixture("named_graphs.nq")).unwrap();
    assert_eq!(
        store.triple_count(),
        4,
        "duplicate quads must collapse per graph"
    );
}

#[test]
fn missing_file_returns_error() {
    let store = Store::in_memory();
    let err = load_nquads_file(&store, &fixture("does-not-exist.nq"));
    assert!(err.is_err());
}
