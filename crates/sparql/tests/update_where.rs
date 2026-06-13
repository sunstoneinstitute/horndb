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
/// A `USING <named-graph>` clause redefines the dataset the WHERE reads
/// from; Stage-1 is default-graph only, so it must be rejected up front
/// (not silently ignored, which would delete from the default graph).
fn using_named_graph_rejected<B: FullBackend + Default>() {
    let mut store: B = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    let u = parse_update(
        "DELETE { ?s <http://ex/p> ?o } USING <http://ex/g> \
         WHERE { ?s <http://ex/p> ?o }",
    )
    .unwrap();
    let err = apply_update(&u, &mut store).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.to_lowercase().contains("graph"));
    // Also assert the message identifies the USING path, so a future swap
    // of the two rejection error paths (USING vs. named-graph template) is
    // caught here. `using_named_graph_unsupported()` contains "USING".
    assert!(msg.contains("USING"));
    // The default-graph triple must be intact (USING was rejected, not
    // silently applied against the default graph).
    assert_eq!(
        objects_of(&store, "http://ex/a", "http://ex/p"),
        vec!["http://ex/b"]
    );
}

/// A `WITH <named-graph>` clause on a combined DELETE/INSERT … WHERE must
/// be rejected at Stage 1. spargebra desugars `WITH <g>` into both the
/// quad graph names *and* `using.default`, so it trips the named-graph
/// template / USING guards (Stage-1 is default-graph only). A *positive*
/// `WITH` test is impossible at Stage 1: any `WITH <iri>` names a
/// non-default graph by construction, so there is no accepted form to
/// assert against.
fn with_named_graph_rejected<B: FullBackend + Default>() {
    let mut store: B = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    let u = parse_update(
        "WITH <http://ex/g> DELETE { ?s <http://ex/p> ?o } \
         INSERT { ?s <http://ex/q> ?o } WHERE { ?s <http://ex/p> ?o }",
    )
    .unwrap();
    let err = apply_update(&u, &mut store).unwrap_err();
    assert!(format!("{err}").to_lowercase().contains("graph"));
    // The default-graph triple must be intact (WITH was rejected up front,
    // not partially applied).
    assert_eq!(
        objects_of(&store, "http://ex/a", "http://ex/p"),
        vec!["http://ex/b"]
    );
}

/// A `GRAPH` pattern in the WHERE clause must be rejected before any
/// mutation. The query translator lowers `GraphPattern::Graph { name, inner }`
/// to its inner pattern over the single default graph — fine for a read
/// query, but for a mutating update it would delete default-graph triples
/// even though the named graph isn't represented (data corruption).
fn graph_in_where_rejected<B: FullBackend + Default>() {
    let mut store: B = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    let u = parse_update(
        "DELETE { ?s <http://ex/p> ?o } \
         WHERE { GRAPH <http://ex/g> { ?s <http://ex/p> ?o } }",
    )
    .unwrap();
    let err = apply_update(&u, &mut store).unwrap_err();
    assert!(format!("{err}").to_lowercase().contains("graph"));
    // The default-graph triple must be intact (GRAPH-in-WHERE was rejected
    // up front, not silently applied against the default graph).
    assert_eq!(
        objects_of(&store, "http://ex/a", "http://ex/p"),
        vec!["http://ex/b"]
    );
}

/// A triple-term slot in an INSERT/DELETE template must be rejected before
/// any mutation (the Stage-1 store has no triple-term slot). Silently
/// dropping the triple while reporting success would be inconsistent with
/// INSERT DATA / DELETE DATA.
fn triple_term_template_rejected<B: FullBackend + Default>() {
    let mut store: B = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    let u = parse_update(
        "INSERT { <<( ?s <http://ex/p> ?o )>> <http://ex/r> ?o } \
         WHERE { ?s <http://ex/p> ?o }",
    )
    .unwrap();
    let err = apply_update(&u, &mut store).unwrap_err();
    assert!(format!("{err}").to_lowercase().contains("triple term"));
    // The original triple must be intact and no bogus triple added.
    assert_eq!(
        objects_of(&store, "http://ex/a", "http://ex/p"),
        vec!["http://ex/b"]
    );
    assert!(objects_of(&store, "http://ex/a", "http://ex/r").is_empty());
}

#[test]
fn mem_graph_in_where_rejected() {
    graph_in_where_rejected::<MemStore>()
}
#[test]
fn horn_graph_in_where_rejected() {
    graph_in_where_rejected::<HornBackend>()
}
#[test]
fn mem_triple_term_template_rejected() {
    triple_term_template_rejected::<MemStore>()
}
#[test]
fn horn_triple_term_template_rejected() {
    triple_term_template_rejected::<HornBackend>()
}

#[test]
fn mem_named_graph_rejected() {
    named_graph_template_rejected::<MemStore>()
}
#[test]
fn horn_named_graph_rejected() {
    named_graph_template_rejected::<HornBackend>()
}
#[test]
fn mem_using_named_graph_rejected() {
    using_named_graph_rejected::<MemStore>()
}
#[test]
fn horn_using_named_graph_rejected() {
    using_named_graph_rejected::<HornBackend>()
}
#[test]
fn mem_with_named_graph_rejected() {
    with_named_graph_rejected::<MemStore>()
}
#[test]
fn horn_with_named_graph_rejected() {
    with_named_graph_rejected::<HornBackend>()
}
