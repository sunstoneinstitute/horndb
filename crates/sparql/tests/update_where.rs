//! Pattern-based SPARQL Update (`INSERT`/`DELETE … WHERE`) over both
//! Stage-1 backends. Each test applies an update, then queries the store
//! to assert the resulting triples (SPARQL Update has no result set).

use horndb_sparql::api::{execute_query, execute_query_with, QueryAnswer};
use horndb_sparql::exec::horn::HornBackend;
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::exec::FullBackend;
use horndb_sparql::parser::parse_update;
use horndb_sparql::update::apply_update;
use horndb_sparql::{DefaultGraphMode, SparqlConfig};

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

/// Return the set of `?s` IRIs for `?s <pred> ?o` (any object) as sorted
/// strings — used to assert presence/absence of a predicate edge.
fn subjects_with_pred<B: FullBackend>(store: &B, pred: &str) -> Vec<String> {
    let q = format!("SELECT ?s WHERE {{ ?s <{pred}> ?o }}");
    let QueryAnswer::Solutions { rows, .. } = execute_query(&q, store).unwrap() else {
        panic!("expected solutions");
    };
    let mut out: Vec<String> = rows
        .iter()
        .filter_map(|r| match r.get("s") {
            Some(horndb_sparql::algebra::Term::Iri(s)) => Some(s.clone()),
            _ => None,
        })
        .collect();
    out.sort();
    out
}

/// Seed a single triple with a literal object, matching the N-Triples
/// lexical convention the store uses (`"x"`).
fn seed_literal_object<B: FullBackend + Default>(s: &str, p: &str, lit: &str) -> B {
    use horndb_sparql::algebra::Term;
    let mut b = B::default();
    b.insert_triple(
        Term::Iri(s.to_owned()),
        Term::Iri(p.to_owned()),
        Term::Literal(lit.to_owned()),
    );
    b
}

/// An INSERT template that would place a literal in subject position
/// produces an illegal RDF triple. Per the illegal-RDF-construct skip rule
/// (SPARQL 1.1 Update §4.1.4 / §10.2.1) it must be *silently skipped* — the
/// update still returns `Ok` and no `<…q…>` triple is created.
fn literal_subject_insert_skipped<B: FullBackend + Default>() {
    // `<a> <p> "x"` — ?o binds to the literal "x".
    let mut store: B = seed_literal_object("http://ex/a", "http://ex/p", "\"x\"");
    let u = parse_update(
        "INSERT { ?o <http://ex/q> <http://ex/z> } WHERE { <http://ex/a> <http://ex/p> ?o }",
    )
    .unwrap();
    // Must succeed, not error.
    apply_update(&u, &mut store).unwrap();
    // The illegal literal-subject triple was skipped: no <q> edge exists.
    assert!(
        subjects_with_pred(&store, "http://ex/q").is_empty(),
        "literal-subject triple must be skipped, not inserted"
    );
}

/// Control: in the *same* update, a valid template triple (literal in
/// object position is legal) is still inserted even though a sibling
/// triple in the same solution is skipped for an illegal literal subject.
fn literal_subject_insert_skips_only_illegal<B: FullBackend + Default>() {
    let mut store: B = seed_literal_object("http://ex/a", "http://ex/p", "\"x\"");
    let u = parse_update(
        "INSERT { ?o <http://ex/q> <http://ex/z> . <http://ex/a> <http://ex/r> ?o } \
         WHERE { <http://ex/a> <http://ex/p> ?o }",
    )
    .unwrap();
    apply_update(&u, &mut store).unwrap();
    // Illegal triple skipped.
    assert!(
        subjects_with_pred(&store, "http://ex/q").is_empty(),
        "literal-subject triple must be skipped"
    );
    // Legal triple (`<a> <r> "x"`, literal in object position) inserted.
    let QueryAnswer::Solutions { rows, .. } =
        execute_query("SELECT ?o WHERE { <http://ex/a> <http://ex/r> ?o }", &store).unwrap()
    else {
        panic!("expected solutions");
    };
    let objs: Vec<String> = rows
        .iter()
        .filter_map(|r| match r.get("o") {
            Some(horndb_sparql::algebra::Term::Literal(s)) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        objs,
        vec!["\"x\"".to_owned()],
        "valid literal-object triple must still be inserted"
    );
}

#[test]
fn mem_literal_subject_insert_skipped() {
    literal_subject_insert_skipped::<MemStore>()
}
#[test]
fn horn_literal_subject_insert_skipped() {
    literal_subject_insert_skipped::<HornBackend>()
}
#[test]
fn mem_literal_subject_insert_skips_only_illegal() {
    literal_subject_insert_skips_only_illegal::<MemStore>()
}
#[test]
fn horn_literal_subject_insert_skips_only_illegal() {
    literal_subject_insert_skips_only_illegal::<HornBackend>()
}

/// A named-graph template routes its instantiated triples into that graph
/// (SPEC-28 phase 4, #267 — previously rejected). The WHERE reads the default
/// graph; the derived triple lands in `<g>`, not the default graph.
fn named_graph_template_routes<B: FullBackend + Default>() {
    let mut store: B = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    let u = parse_update(
        "INSERT { GRAPH <http://ex/g> { ?s <http://ex/q> ?o } } \
         WHERE { ?s <http://ex/p> ?o }",
    )
    .unwrap();
    apply_update(&u, &mut store).unwrap();
    let QueryAnswer::Solutions { rows, .. } = execute_query(
        "SELECT ?s WHERE { GRAPH <http://ex/g> { ?s <http://ex/q> ?o } }",
        &store,
    )
    .unwrap() else {
        panic!("expected solutions");
    };
    assert_eq!(rows.len(), 1, "the template routed the triple into <g>");
    assert!(
        default_graph_subjects(&store, "SELECT ?s WHERE { ?s <http://ex/q> ?o }").is_empty(),
        "the derived triple must not land in the default graph"
    );
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
/// A `USING <named-graph>` clause builds the WHERE dataset (SPEC-28 phase 4 /
/// S3 `DatasetSpec` — previously rejected): the WHERE reads `<g>` as its
/// default graph, while the (unscoped) INSERT template writes the default
/// graph.
fn using_named_graph_reads_dataset<B: FullBackend + Default + SeedNamedGraph>() {
    let mut store = B::default();
    store.seed_named("http://ex/g", "http://ex/a", "http://ex/p", "http://ex/b");
    let u = parse_update(
        "INSERT { ?s <http://ex/q> ?o } USING <http://ex/g> \
         WHERE { ?s <http://ex/p> ?o }",
    )
    .unwrap();
    apply_update(&u, &mut store).unwrap();
    // The WHERE read <g> (else the default graph is empty and nothing inserts);
    // the template wrote the default graph.
    assert_eq!(
        default_graph_subjects(&store, "SELECT ?s WHERE { ?s <http://ex/q> ?o }"),
        vec!["http://ex/a"],
        "USING <g> made the WHERE read <g>"
    );
}

/// A `WITH <named-graph>` clause scopes both the templates and the WHERE clause
/// to `<g>` (SPEC-28 phase 4 / D10 — previously rejected). spargebra desugars
/// `WITH <g>` by injecting `<g>` into the templates and setting `USING <g>` for
/// the WHERE, so both sides resolve to `<g>`.
fn with_named_graph_scopes_both_sides<B: FullBackend + Default + SeedNamedGraph>() {
    let mut store = B::default();
    store.seed_named("http://ex/g", "http://ex/a", "http://ex/p", "http://ex/b");
    let u = parse_update(
        "WITH <http://ex/g> DELETE { ?s <http://ex/p> ?o } \
         INSERT { ?s <http://ex/q> ?o } WHERE { ?s <http://ex/p> ?o }",
    )
    .unwrap();
    apply_update(&u, &mut store).unwrap();
    let count_in_g = |pred: &str| {
        let q = format!("SELECT ?s WHERE {{ GRAPH <http://ex/g> {{ ?s <{pred}> ?o }} }}");
        let QueryAnswer::Solutions { rows, .. } = execute_query(&q, &store).unwrap() else {
            panic!("expected solutions");
        };
        rows.len()
    };
    assert_eq!(
        count_in_g("http://ex/p"),
        0,
        "WITH deleted the source triple in <g>"
    );
    assert_eq!(
        count_in_g("http://ex/q"),
        1,
        "WITH inserted the derived triple in <g>"
    );
    assert!(
        default_graph_subjects(&store, "SELECT ?s WHERE { ?s ?p ?o }").is_empty(),
        "the default graph is untouched by WITH <g>"
    );
}

/// A ground `GRAPH` pattern in the WHERE clause reads that named graph (SPEC-28
/// phase 4 routes the WHERE through the phase-3 query path — previously
/// rejected by the update layer's own scan). Here the WHERE reads `<g>`; the
/// (default-graph) INSERT template copies the bound row into the default graph.
fn graph_in_where_reads_named<B: FullBackend + Default + SeedNamedGraph>() {
    let mut store: B = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    store.seed_named("http://ex/g", "http://ex/n", "http://ex/p", "http://ex/c");
    let u = parse_update(
        "INSERT { ?s <http://ex/q> ?o } \
         WHERE { GRAPH <http://ex/g> { ?s <http://ex/p> ?o } }",
    )
    .unwrap();
    apply_update(&u, &mut store).unwrap();
    // `<http://ex/n>` lives only in <g>; its appearance here proves the WHERE
    // read <g>, not the default graph.
    assert_eq!(
        default_graph_subjects(&store, "SELECT ?s WHERE { ?s <http://ex/q> ?o }"),
        vec!["http://ex/n"],
        "GRAPH <g> in the WHERE read the named graph"
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

/// Seed one quad into a named graph, through each backend's storage seam.
/// The `Store` write trait is triple-shaped and default-graph only until
/// SPEC-28 phase 4 (#267), so these tests plant named-graph data directly.
trait SeedNamedGraph {
    fn seed_named(&mut self, graph: &str, s: &str, p: &str, o: &str);
}
impl SeedNamedGraph for MemStore {
    fn seed_named(&mut self, graph: &str, s: &str, p: &str, o: &str) {
        self.insert_quad(Some(graph), (s.to_owned(), p.to_owned(), o.to_owned()));
    }
}
impl SeedNamedGraph for HornBackend {
    fn seed_named(&mut self, graph: &str, s: &str, p: &str, o: &str) {
        let iri = |v: &str| oxrdf::Term::NamedNode(oxrdf::NamedNode::new_unchecked(v));
        self.insert_oxrdf_in_named_graph(&iri(graph), &iri(s), &iri(p), &iri(o))
            .unwrap();
    }
}

/// The `?s` bindings of `q`, sorted, evaluated against the **default graph
/// only** (`strict`) so named-graph rows cannot mask the assertion.
fn default_graph_subjects<B: FullBackend>(store: &B, q: &str) -> Vec<String> {
    let QueryAnswer::Solutions { rows, .. } = execute_query_with(
        q,
        store,
        &SparqlConfig {
            default_graph: DefaultGraphMode::Strict,
            ..SparqlConfig::default()
        },
    )
    .unwrap() else {
        panic!("expected solutions");
    };
    let mut out: Vec<String> = rows
        .iter()
        .filter_map(|r| match r.get("s") {
            Some(horndb_sparql::algebra::Term::Iri(s)) => Some(s.clone()),
            _ => None,
        })
        .collect();
    out.sort();
    out
}

/// An update's WHERE clause with no `WITH`/`USING` reads the **default graph
/// only** (strict), regardless of the query-side `default_graph` mode — so a
/// default-graph template writes exactly the rows the WHERE bound (SPEC-28
/// phase 4; `apply_delete_insert` pins the no-clause WHERE to `Strict`).
///
/// The discriminating shape is DELETE+INSERT. Were the WHERE run under a
/// `union` default graph it would also bind named-graph rows; the default-graph
/// DELETE could not remove them (a silent no-op), but the INSERT template would
/// still fire — **copying** each named-graph binding into the default graph. A
/// plain `DELETE WHERE` hides this: the delete no-ops either way, so the
/// named-graph row survives whether or not the WHERE is pinned. Assert on the
/// copy, not on the survival.
fn update_where_does_not_see_named_graph_data<B: FullBackend + Default + SeedNamedGraph>() {
    let mut store: B = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    store.seed_named("http://ex/g", "http://ex/n", "http://ex/p", "http://ex/c");

    let u = parse_update(
        "DELETE { ?s <http://ex/p> ?o } INSERT { ?s <http://ex/q> ?o } \
         WHERE { ?s <http://ex/p> ?o }",
    )
    .unwrap();
    apply_update(&u, &mut store).unwrap();

    // Only the default graph's own row was rewritten. `<http://ex/n>` lives
    // in <g>; if it shows up here, the WHERE read a graph the templates
    // cannot write.
    assert_eq!(
        default_graph_subjects(&store, "SELECT ?s WHERE { ?s <http://ex/q> ?o }"),
        vec!["http://ex/a"],
        "named-graph bindings must not be copied into the default graph"
    );
    assert!(
        default_graph_subjects(&store, "SELECT ?s WHERE { ?s <http://ex/p> ?o }").is_empty(),
        "the default graph's own row must still be deleted"
    );

    // The named graph is untouched — the WHERE never saw it.
    let QueryAnswer::Solutions { rows, .. } = execute_query(
        "SELECT ?s WHERE { GRAPH <http://ex/g> { ?s <http://ex/p> ?o } }",
        &store,
    )
    .unwrap() else {
        panic!("expected solutions");
    };
    assert_eq!(rows.len(), 1, "named-graph row must survive: {rows:?}");
}

#[test]
fn mem_update_where_does_not_see_named_graph_data() {
    update_where_does_not_see_named_graph_data::<MemStore>()
}
#[test]
fn horn_update_where_does_not_see_named_graph_data() {
    update_where_does_not_see_named_graph_data::<HornBackend>()
}

#[test]
fn mem_graph_in_where_reads_named() {
    graph_in_where_reads_named::<MemStore>()
}
#[test]
fn horn_graph_in_where_reads_named() {
    graph_in_where_reads_named::<HornBackend>()
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
fn mem_named_graph_template_routes() {
    named_graph_template_routes::<MemStore>()
}
#[test]
fn horn_named_graph_template_routes() {
    named_graph_template_routes::<HornBackend>()
}
#[test]
fn mem_using_named_graph_reads_dataset() {
    using_named_graph_reads_dataset::<MemStore>()
}
#[test]
fn horn_using_named_graph_reads_dataset() {
    using_named_graph_reads_dataset::<HornBackend>()
}
#[test]
fn mem_with_named_graph_scopes_both_sides() {
    with_named_graph_scopes_both_sides::<MemStore>()
}
#[test]
fn horn_with_named_graph_scopes_both_sides() {
    with_named_graph_scopes_both_sides::<HornBackend>()
}
