//! Graph-management SPARQL Update verbs (SPEC-07 #52, SPEC-28 S4/S6): `LOAD`,
//! `CLEAR`, `DROP`, `CREATE`, `ADD`, `MOVE`, `COPY`. Exercised over both
//! Stage-1 backends where the verb is backend-relevant. Named graphs are
//! first-class (PLAN-28-04): these verbs operate on real named graphs with
//! SPARQL 1.1 §3.2 D11 existence semantics, and `SILENT` covers the
//! existence errors only.

use horndb_sparql::algebra::Term;
use horndb_sparql::api::{execute_query, QueryAnswer};
use horndb_sparql::exec::horn::HornBackend;
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::exec::StoreTestExt;
use horndb_sparql::exec::{FullBackend, GraphNamedNode as NamedNode, Store, StoreGraphTarget};
use horndb_sparql::parser::parse_update;
use horndb_sparql::update::apply_update;

/// `GraphTarget::NamedNode(<iri>)`, for per-graph store assertions.
fn tgt(g: &str) -> StoreGraphTarget {
    StoreGraphTarget::NamedNode(NamedNode::new_unchecked(g))
}

/// Number of quads in a named graph.
fn count_named<B: FullBackend>(store: &B, g: &str) -> usize {
    store.scan_graph_quads(&tgt(g)).unwrap().len()
}

fn seed<B: FullBackend + Default>(triples: &[(&str, &str, &str)]) -> B {
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

/// Total number of triples visible to a query (`SELECT * WHERE { ?s ?p ?o }`).
fn count_all<B: FullBackend>(store: &B) -> usize {
    let QueryAnswer::Solutions { rows, .. } =
        execute_query("SELECT ?s ?p ?o WHERE { ?s ?p ?o }", store).unwrap()
    else {
        panic!("expected solutions");
    };
    rows.len()
}

fn run(u: &str, store: &mut impl FullBackend) -> Result<(), String> {
    let parsed = parse_update(u).map_err(|e| e.to_string())?;
    apply_update(&parsed, store).map_err(|e| e.to_string())
}

// ── CLEAR / DROP ────────────────────────────────────────────────────────────

fn clear_default_empties<B: FullBackend + Default>() {
    let mut store: B = seed(&[
        ("http://ex/a", "http://ex/p", "http://ex/b"),
        ("http://ex/a", "http://ex/p", "http://ex/c"),
    ]);
    assert_eq!(count_all(&store), 2);
    run("CLEAR DEFAULT", &mut store).unwrap();
    assert_eq!(count_all(&store), 0);
}

#[test]
fn clear_default_empties_mem() {
    clear_default_empties::<MemStore>();
}
#[test]
fn clear_default_empties_horn() {
    clear_default_empties::<HornBackend>();
}

fn clear_all_and_drop_all_empty<B: FullBackend + Default>() {
    for verb in ["CLEAR ALL", "DROP ALL", "DROP DEFAULT"] {
        let mut store: B = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
        run(verb, &mut store).unwrap_or_else(|e| panic!("{verb}: {e}"));
        assert_eq!(count_all(&store), 0, "{verb} should empty the store");
    }
}

#[test]
fn clear_all_and_drop_all_empty_mem() {
    clear_all_and_drop_all_empty::<MemStore>();
}
#[test]
fn clear_all_and_drop_all_empty_horn() {
    clear_all_and_drop_all_empty::<HornBackend>();
}

fn clear_after_insert_then_reinsert<B: FullBackend + Default>() {
    // Re-inserting after a CLEAR must resurrect the triple (covers the
    // HornBackend native-retraction path used by clear_all, SPEC-25 S1).
    let mut store: B = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    run("CLEAR DEFAULT", &mut store).unwrap();
    assert_eq!(count_all(&store), 0);
    run(
        "INSERT DATA { <http://ex/a> <http://ex/p> <http://ex/b> }",
        &mut store,
    )
    .unwrap();
    assert_eq!(count_all(&store), 1);
}

#[test]
fn clear_after_insert_then_reinsert_mem() {
    clear_after_insert_then_reinsert::<MemStore>();
}
#[test]
fn clear_after_insert_then_reinsert_horn() {
    clear_after_insert_then_reinsert::<HornBackend>();
}

#[test]
fn clear_named_graph_absent_error_present_empties() {
    // PLAN-28-04 (inverts the Stage-1 "named graph unrepresentable" pin): D11
    // existence. CLEAR of an ABSENT named graph is an error unless SILENT; of a
    // PRESENT one it empties the graph.
    let mut store = MemStore::default();
    store
        .apply_quads(
            Vec::new(),
            vec![(
                Some(Term::Iri("http://g/1".into())),
                Term::Iri("http://ex/a".into()),
                Term::Iri("http://ex/p".into()),
                Term::Iri("http://ex/b".into()),
            )],
        )
        .unwrap();

    run("CLEAR SILENT GRAPH <http://g/absent>", &mut store).unwrap();
    let err = run("CLEAR GRAPH <http://g/absent>", &mut store).unwrap_err();
    assert!(err.to_lowercase().contains("exist"), "{err}");

    run("CLEAR GRAPH <http://g/1>", &mut store).unwrap();
    assert!(!store.graph_exists("http://g/1"), "CLEAR empties the graph");
}

#[test]
fn drop_named_graph_absent_error_present_empties() {
    // PLAN-28-04 (inverts the Stage-1 pin): D11 existence for DROP.
    let mut store = MemStore::default();
    store
        .apply_quads(
            Vec::new(),
            vec![(
                Some(Term::Iri("http://g/1".into())),
                Term::Iri("http://ex/a".into()),
                Term::Iri("http://ex/p".into()),
                Term::Iri("http://ex/b".into()),
            )],
        )
        .unwrap();
    run("DROP SILENT GRAPH <http://g/absent>", &mut store).unwrap();
    let err = run("DROP GRAPH <http://g/absent>", &mut store).unwrap_err();
    assert!(err.to_lowercase().contains("exist"), "{err}");
    run("DROP GRAPH <http://g/1>", &mut store).unwrap();
    assert!(!store.graph_exists("http://g/1"));
}

#[test]
fn clear_named_keyword_clears_all_named() {
    // PLAN-28-04 (inverts the Stage-1 pin): CLEAR NAMED empties every named
    // graph and never errors on an empty named set. The default graph survives.
    let mut store: MemStore = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    store
        .apply_quads(
            Vec::new(),
            vec![(
                Some(Term::Iri("http://g/1".into())),
                Term::Iri("http://ex/c".into()),
                Term::Iri("http://ex/p".into()),
                Term::Iri("http://ex/d".into()),
            )],
        )
        .unwrap();
    run("CLEAR NAMED", &mut store).unwrap();
    assert!(
        !store.graph_exists("http://g/1"),
        "CLEAR NAMED empties named graphs"
    );
    assert_eq!(count_all(&store), 1, "default graph survives CLEAR NAMED");
}

// ── CREATE ──────────────────────────────────────────────────────────────────

#[test]
fn create_named_graph_absent_ok_present_errors() {
    // PLAN-28-04 (inverts the Stage-1 pin): D11 CREATE. An absent graph is a
    // successful no-op (no registry); an existing one errors unless SILENT.
    let mut store = MemStore::default();
    run("CREATE GRAPH <http://g/1>", &mut store).unwrap();
    // No registry: the graph does not "exist" until it holds a quad.
    assert!(!store.graph_exists("http://g/1"));

    store
        .apply_quads(
            Vec::new(),
            vec![(
                Some(Term::Iri("http://g/1".into())),
                Term::Iri("http://ex/a".into()),
                Term::Iri("http://ex/p".into()),
                Term::Iri("http://ex/b".into()),
            )],
        )
        .unwrap();
    let err = run("CREATE GRAPH <http://g/1>", &mut store).unwrap_err();
    assert!(err.to_lowercase().contains("exist"), "{err}");
    run("CREATE SILENT GRAPH <http://g/1>", &mut store).unwrap();
}

// ── LOAD ────────────────────────────────────────────────────────────────────

fn write_tmp(name: &str, body: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    // The default harness runs tests in parallel; a process-unique counter keeps
    // two tests (e.g. the MemStore and HornBackend legs sharing `name`) from
    // racing on one path.
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!(
        "horndb_load_test_{}_{seq}_{name}",
        std::process::id()
    ));
    std::fs::write(&p, body).unwrap();
    p
}

fn load_file_into_default<B: FullBackend + Default>() {
    let path = write_tmp(
        "data.nt",
        "<http://ex/s> <http://ex/p> <http://ex/o> .\n<http://ex/s> <http://ex/p> <http://ex/o2> .\n",
    );
    let mut store: B = B::default();
    let u = format!("LOAD <file://{}>", path.display());
    run(&u, &mut store).unwrap();
    assert_eq!(count_all(&store), 2);
    std::fs::remove_file(&path).ok();
}

#[test]
fn load_file_into_default_mem() {
    load_file_into_default::<MemStore>();
}
#[test]
fn load_file_into_default_horn() {
    load_file_into_default::<HornBackend>();
}

#[test]
fn load_turtle_file() {
    let path = write_tmp(
        "data.ttl",
        "@prefix ex: <http://ex/> .\nex:s ex:p ex:o, ex:o2 .\n",
    );
    let mut store = MemStore::default();
    let u = format!("LOAD <file://{}>", path.display());
    run(&u, &mut store).unwrap();
    assert_eq!(count_all(&store), 2);
    std::fs::remove_file(&path).ok();
}

#[test]
fn load_turtle_relative_iris_resolve_against_source() {
    // Turtle with relative IRIs must resolve against the document (LOAD source)
    // IRI; without a base the parse would fail.
    let path = write_tmp("rel.ttl", "<s> <p> <o> .\n");
    let mut store = MemStore::default();
    let u = format!("LOAD <file://{}>", path.display());
    run(&u, &mut store).unwrap();
    assert_eq!(count_all(&store), 1);
    std::fs::remove_file(&path).ok();
}

#[test]
fn load_file_localhost_authority() {
    // `file://localhost/...` is a valid local file IRI.
    let path = write_tmp("auth.nt", "<http://ex/s> <http://ex/p> <http://ex/o> .\n");
    let mut store = MemStore::default();
    // path already begins with `/`, so `file://localhost` + path is well-formed.
    let u = format!("LOAD <file://localhost{}>", path.display());
    run(&u, &mut store).unwrap();
    assert_eq!(count_all(&store), 1);
    std::fs::remove_file(&path).ok();
}

#[test]
fn load_file_remote_authority_errors() {
    // A non-local authority is rejected (no remote fetch).
    let mut store = MemStore::default();
    let err = run("LOAD <file://remote.example.org/tmp/data.nt>", &mut store).unwrap_err();
    assert!(err.to_lowercase().contains("authority"), "{err}");
}

#[test]
fn load_percent_encoded_path() {
    // A file IRI percent-encodes reserved characters; LOAD must decode the path
    // back to the real filesystem name before reading it.
    let mut dir = std::env::temp_dir();
    dir.push(format!("horndb load dir {}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("a b.nt");
    std::fs::write(&path, "<http://ex/s> <http://ex/p> <http://ex/o> .\n").unwrap();

    let mut store = MemStore::default();
    // Encode spaces as %20 in the IRI.
    let encoded = path.display().to_string().replace(' ', "%20");
    let u = format!("LOAD <file://{encoded}>");
    run(&u, &mut store).unwrap();
    assert_eq!(count_all(&store), 1);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn load_triples_into_named_graph_routes_to_it() {
    // PLAN-28-04 (inverts the Stage-1 "LOAD INTO named unsupported" pin): a
    // triples document loads into the named destination graph.
    let path = write_tmp(
        "data2.nt",
        "<http://ex/s> <http://ex/p> <http://ex/o> .\n<http://ex/s> <http://ex/p> <http://ex/o2> .\n",
    );
    let mut store = MemStore::default();
    let u = format!("LOAD <file://{}> INTO GRAPH <http://g/1>", path.display());
    run(&u, &mut store).unwrap();
    assert_eq!(count_named(&store, "http://g/1"), 2);
    assert_eq!(
        count_all(&store),
        2,
        "the union sees the loaded named graph"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn load_remote_source_silent_noop_nonsilent_errors() {
    let mut store = MemStore::default();
    run("LOAD SILENT <http://example.org/data.ttl>", &mut store).unwrap();
    assert_eq!(count_all(&store), 0);
    let err = run("LOAD <http://example.org/data.ttl>", &mut store).unwrap_err();
    assert!(err.to_lowercase().contains("file:"), "{err}");
}

#[test]
fn load_missing_file_silent_noop_nonsilent_errors() {
    let mut store = MemStore::default();
    run(
        "LOAD SILENT <file:///nonexistent/horndb/missing.nt>",
        &mut store,
    )
    .unwrap();
    assert_eq!(count_all(&store), 0);
    let err = run("LOAD <file:///nonexistent/horndb/missing.nt>", &mut store).unwrap_err();
    assert!(err.to_lowercase().contains("load"), "{err}");
}

// ── ADD / MOVE / COPY (spargebra desugars these) ────────────────────────────

#[test]
fn add_default_to_default_is_identity() {
    // ADD/MOVE/COPY where source == destination is the W3C identity case:
    // spargebra rewrites it to zero operations, so it is a valid no-op and the
    // data is untouched.
    let mut store: MemStore = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    run("ADD DEFAULT TO DEFAULT", &mut store).unwrap();
    assert_eq!(count_all(&store), 1);
}

#[test]
fn copy_default_to_default_is_identity() {
    let mut store: MemStore = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    run("COPY DEFAULT TO DEFAULT", &mut store).unwrap();
    assert_eq!(count_all(&store), 1);
}

#[test]
fn move_default_to_default_is_identity() {
    let mut store: MemStore = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    run("MOVE DEFAULT TO DEFAULT", &mut store).unwrap();
    assert_eq!(count_all(&store), 1);
}

#[test]
fn add_named_operand_executes() {
    // PLAN-28-04 (inverts the Stage-1 "named operand unrepresentable" pin): a
    // named-graph operand now executes — ADD merges the source graph into the
    // destination (here DEFAULT), leaving the source intact.
    let mut store = MemStore::default();
    store
        .apply_quads(
            Vec::new(),
            vec![(
                Some(Term::Iri("http://g/1".into())),
                Term::Iri("http://ex/a".into()),
                Term::Iri("http://ex/p".into()),
                Term::Iri("http://ex/b".into()),
            )],
        )
        .unwrap();
    run("ADD <http://g/1> TO DEFAULT", &mut store).unwrap();
    assert_eq!(
        store
            .scan_graph_quads(&StoreGraphTarget::DefaultGraph)
            .unwrap()
            .len(),
        1,
        "ADD merged the source into the default graph"
    );
    assert_eq!(count_named(&store, "http://g/1"), 1, "ADD keeps the source");
}

fn copy_missing_source_errors_without_data_loss<B: FullBackend + Default>() {
    // PLAN-28-04 (inverts the Stage-1 pin, same atomicity intent): `COPY
    // <missing> TO DEFAULT` desugars to `Drop{DEFAULT}` + a `DeleteInsert`
    // reading `GRAPH <missing>`. A non-silent missing source is an error
    // (SPEC-28 S4); the preflight rejects the whole update before the
    // destructive `Drop{DEFAULT}` runs, so the default graph is intact.
    let mut store: B = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    let err = run("COPY <http://g/missing> TO DEFAULT", &mut store).unwrap_err();
    assert!(err.to_lowercase().contains("source"), "{err}");
    assert_eq!(count_all(&store), 1, "failed update must not lose data");
}

#[test]
fn copy_missing_source_errors_without_data_loss_mem() {
    copy_missing_source_errors_without_data_loss::<MemStore>();
}
#[test]
fn copy_missing_source_errors_without_data_loss_horn() {
    copy_missing_source_errors_without_data_loss::<HornBackend>();
}

#[test]
fn move_missing_source_errors_without_data_loss() {
    // PLAN-28-04 (inverts the Stage-1 pin): non-silent MOVE of a missing source.
    let mut store: MemStore = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    let err = run("MOVE <http://g/missing> TO DEFAULT", &mut store).unwrap_err();
    assert!(err.to_lowercase().contains("source"), "{err}");
    assert_eq!(count_all(&store), 1, "failed update must not lose data");
}

#[test]
fn multi_op_failing_op_aborts_before_destructive_op() {
    // A destructive op followed by a failing op: the whole update is rejected
    // up front, so the destructive op never runs. PLAN-28-04: the failing op is
    // now a D11 error (DROP of an absent graph, non-silent) — CREATE of an
    // absent graph is a success under the new semantics.
    let mut store: MemStore = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    let err = run("CLEAR DEFAULT ; DROP GRAPH <http://g/absent>", &mut store).unwrap_err();
    assert!(err.to_lowercase().contains("exist"), "{err}");
    assert_eq!(
        count_all(&store),
        1,
        "CLEAR must not run when a later op fails"
    );
}

#[test]
fn multi_op_clear_then_unsupported_where_aborts() {
    // A CLEAR followed by a DELETE WHERE whose WHERE uses an unsupported algebra
    // construct (MINUS) must abort before the CLEAR mutates — the preflight
    // translates/plans the WHERE, so the translation failure is caught up front.
    let mut store: MemStore = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    let err = run(
        "CLEAR DEFAULT ; DELETE { ?s ?p ?o } WHERE { ?s ?p ?o MINUS { ?s ?p ?o } }",
        &mut store,
    )
    .unwrap_err();
    assert!(!err.is_empty(), "expected an error");
    assert_eq!(
        count_all(&store),
        1,
        "CLEAR must not run when a later WHERE fails to translate"
    );
}

#[test]
fn add_silent_missing_source_is_noop() {
    // PLAN-28-04 (inverts the Stage-1 pin): spargebra drops the SILENT flag when
    // it desugars ADD/MOVE/COPY, so update.rs recovers it from the source text
    // (`recover_amc_hints`). With the flag recovered, a SILENT ADD of a missing
    // source is a no-op (not an error), and a non-silent one is an error.
    let mut store: MemStore = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    run("ADD SILENT <http://g/missing> TO DEFAULT", &mut store).unwrap();
    assert_eq!(
        count_all(&store),
        1,
        "SILENT ADD of a missing source is a no-op"
    );

    let err = run("ADD <http://g/missing> TO DEFAULT", &mut store).unwrap_err();
    assert!(err.to_lowercase().contains("source"), "{err}");
    assert_eq!(count_all(&store), 1);
}

// ── Multi-operation update ──────────────────────────────────────────────────

#[test]
fn multi_op_update_applies_in_order() {
    let mut store = MemStore::default();
    run(
        "INSERT DATA { <http://ex/a> <http://ex/p> <http://ex/b> } ; \
         CLEAR DEFAULT ; \
         INSERT DATA { <http://ex/c> <http://ex/p> <http://ex/d> }",
        &mut store,
    )
    .unwrap();
    // First insert, then clear wipes it, then second insert: exactly one triple.
    assert_eq!(count_all(&store), 1);
    let QueryAnswer::Solutions { rows, .. } =
        execute_query("SELECT ?s WHERE { ?s <http://ex/p> ?o }", &store).unwrap()
    else {
        panic!("expected solutions");
    };
    assert_eq!(rows.len(), 1);
    assert!(matches!(rows[0].get("s"), Some(Term::Iri(s)) if s == "http://ex/c"));
}

// ── Named-graph data updates route to the named graph (PLAN-28-04) ───────────

#[test]
fn insert_data_named_graph_routes_to_it() {
    // PLAN-28-04 (inverts the Stage-1 rejection pin): `INSERT DATA { GRAPH <g>
    // { … } }` routes to the named graph, leaving the default graph untouched.
    let mut store: MemStore = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    run(
        "INSERT DATA { GRAPH <http://g/1> { <http://ex/s> <http://ex/p> <http://ex/o> } }",
        &mut store,
    )
    .unwrap();
    assert_eq!(count_named(&store, "http://g/1"), 1);
    assert_eq!(
        store
            .scan_graph_quads(&StoreGraphTarget::DefaultGraph)
            .unwrap()
            .len(),
        1,
        "default graph untouched"
    );
}

#[test]
fn delete_data_named_graph_routes_to_it() {
    // PLAN-28-04 (inverts the Stage-1 pin): `DELETE DATA { GRAPH <g> { … } }`
    // removes from the named graph; the default graph is never targeted.
    let mut store: MemStore = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    run(
        "INSERT DATA { GRAPH <http://g/1> { <http://ex/a> <http://ex/p> <http://ex/b> } }",
        &mut store,
    )
    .unwrap();
    run(
        "DELETE DATA { GRAPH <http://g/1> { <http://ex/a> <http://ex/p> <http://ex/b> } }",
        &mut store,
    )
    .unwrap();
    assert!(!store.graph_exists("http://g/1"));
    assert_eq!(count_all(&store), 1, "the default-graph triple survives");
}

#[test]
fn multi_op_insert_then_named_delete_data_both_apply() {
    // PLAN-28-04 (inverts the Stage-1 abort pin): a default-graph INSERT DATA
    // followed by a named-graph DELETE DATA both apply — the named DELETE of an
    // absent quad is a counted no-op, not an error.
    let mut store = MemStore::default();
    run(
        "INSERT DATA { <http://ex/a> <http://ex/p> <http://ex/b> } ; \
         DELETE DATA { GRAPH <http://g/1> { <http://ex/a> <http://ex/p> <http://ex/b> } }",
        &mut store,
    )
    .unwrap();
    assert_eq!(count_all(&store), 1, "the default-graph insert applied");
}
