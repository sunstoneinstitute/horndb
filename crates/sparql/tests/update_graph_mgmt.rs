//! Graph-management SPARQL Update verbs (SPEC-07 #52): `LOAD`, `CLEAR`,
//! `DROP`, `CREATE`, `ADD`, `MOVE`, `COPY`. Exercised over both Stage-1
//! backends where the verb is backend-relevant. Under the default-graph-only
//! Stage-1 model these verbs map onto the single merged graph and honour the
//! `SILENT` modifier (a SILENT op against an unrepresentable named graph is a
//! no-op; the same op non-silent is an error).

use horndb_sparql::algebra::Term;
use horndb_sparql::api::{execute_query, QueryAnswer};
use horndb_sparql::exec::horn::HornBackend;
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::exec::FullBackend;
use horndb_sparql::parser::parse_update;
use horndb_sparql::update::apply_update;

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
fn clear_named_graph_silent_is_noop_nonsilent_errors() {
    // No named graphs exist: a named target addresses nothing.
    let mut store: MemStore = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    run("CLEAR SILENT GRAPH <http://g/1>", &mut store).unwrap();
    assert_eq!(
        count_all(&store),
        1,
        "silent clear of absent graph is a no-op"
    );

    let err = run("CLEAR GRAPH <http://g/1>", &mut store).unwrap_err();
    assert!(
        err.to_lowercase().contains("named-graph"),
        "non-silent clear of named graph should error: {err}"
    );
}

#[test]
fn drop_named_graph_silent_is_noop_nonsilent_errors() {
    let mut store: MemStore = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    run("DROP SILENT GRAPH <http://g/1>", &mut store).unwrap();
    assert_eq!(count_all(&store), 1);
    let err = run("DROP GRAPH <http://g/1>", &mut store).unwrap_err();
    assert!(err.to_lowercase().contains("named-graph"), "{err}");
}

#[test]
fn clear_named_keyword_silent_is_noop() {
    let mut store: MemStore = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    run("CLEAR SILENT NAMED", &mut store).unwrap();
    assert_eq!(count_all(&store), 1);
    let err = run("CLEAR NAMED", &mut store).unwrap_err();
    assert!(err.to_lowercase().contains("named-graph"), "{err}");
}

// ── CREATE ──────────────────────────────────────────────────────────────────

#[test]
fn create_named_graph_silent_is_noop_nonsilent_errors() {
    let mut store: MemStore = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    run("CREATE SILENT GRAPH <http://g/1>", &mut store).unwrap();
    assert_eq!(
        count_all(&store),
        1,
        "silent create is a no-op, data untouched"
    );
    let err = run("CREATE GRAPH <http://g/1>", &mut store).unwrap_err();
    assert!(
        err.to_lowercase().contains("create"),
        "non-silent create of named graph should error: {err}"
    );
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
fn load_into_named_graph_silent_noop_nonsilent_errors() {
    let path = write_tmp("data2.nt", "<http://ex/s> <http://ex/p> <http://ex/o> .\n");
    let mut store = MemStore::default();
    let u = format!(
        "LOAD SILENT <file://{}> INTO GRAPH <http://g/1>",
        path.display()
    );
    run(&u, &mut store).unwrap();
    assert_eq!(
        count_all(&store),
        0,
        "silent LOAD into named graph is a no-op"
    );

    let u = format!("LOAD <file://{}> INTO GRAPH <http://g/1>", path.display());
    let err = run(&u, &mut store).unwrap_err();
    assert!(err.to_lowercase().contains("named graph"), "{err}");
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
fn add_named_operand_errors() {
    // A named-graph operand cannot be represented; the DeleteInsert that
    // ADD/COPY/MOVE desugar to reads/writes a named GRAPH and is rejected.
    let mut store: MemStore = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    let err = run("ADD <http://g/1> TO DEFAULT", &mut store).unwrap_err();
    assert!(
        err.to_lowercase().contains("graph"),
        "named operand should error: {err}"
    );
    // The store is left untouched (the rejection happens before any mutation).
    assert_eq!(count_all(&store), 1);
}

fn copy_named_to_default_errors_without_data_loss<B: FullBackend + Default>() {
    // `COPY <named> TO DEFAULT` desugars to `Drop{DEFAULT}` + a `DeleteInsert`
    // reading `GRAPH <named>`. Applying op-by-op would clear the default graph
    // and only then reject the named read, losing data on a failing update.
    // The atomicity preflight must reject the whole update before any mutation,
    // leaving the seeded triple intact.
    let mut store: B = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    let err = run("COPY <http://g/1> TO DEFAULT", &mut store).unwrap_err();
    assert!(err.to_lowercase().contains("graph"), "{err}");
    assert_eq!(count_all(&store), 1, "failed update must not lose data");
}

#[test]
fn copy_named_to_default_errors_without_data_loss_mem() {
    copy_named_to_default_errors_without_data_loss::<MemStore>();
}
#[test]
fn copy_named_to_default_errors_without_data_loss_horn() {
    copy_named_to_default_errors_without_data_loss::<HornBackend>();
}

#[test]
fn move_named_to_default_errors_without_data_loss() {
    let mut store: MemStore = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    let err = run("MOVE <http://g/1> TO DEFAULT", &mut store).unwrap_err();
    assert!(err.to_lowercase().contains("graph"), "{err}");
    assert_eq!(count_all(&store), 1, "failed update must not lose data");
}

#[test]
fn multi_op_failing_op_aborts_before_destructive_op() {
    // A destructive op followed by a failing op: the whole update is rejected
    // up front, so the destructive op never runs.
    let mut store: MemStore = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    let err = run("CLEAR DEFAULT ; CREATE GRAPH <http://g/1>", &mut store).unwrap_err();
    assert!(err.to_lowercase().contains("create"), "{err}");
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
fn add_named_operand_silent_still_errors() {
    // spargebra drops the SILENT flag when it desugars ADD/MOVE/COPY into a
    // DeleteInsert, so SILENT is not preserved for a named operand: it errors
    // like the non-silent form. Documented in update.rs. No data moves either
    // way (named graphs are unrepresentable), so the store is unchanged.
    let mut store: MemStore = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    let err = run("ADD SILENT <http://g/1> TO DEFAULT", &mut store).unwrap_err();
    assert!(err.to_lowercase().contains("graph"), "{err}");
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

// ── Named-graph data updates are rejected (default-graph only) ───────────────

#[test]
fn insert_data_named_graph_errors_without_mutation() {
    // `INSERT DATA { GRAPH <g> { … } }` must not silently write to the default
    // graph: it is rejected, and the store is unchanged.
    let mut store: MemStore = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    let err = run(
        "INSERT DATA { GRAPH <http://g/1> { <http://ex/s> <http://ex/p> <http://ex/o> } }",
        &mut store,
    )
    .unwrap_err();
    assert!(err.to_lowercase().contains("named-graph"), "{err}");
    assert_eq!(count_all(&store), 1);
}

#[test]
fn delete_data_named_graph_errors_without_mutation() {
    let mut store: MemStore = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    let err = run(
        "DELETE DATA { GRAPH <http://g/1> { <http://ex/a> <http://ex/p> <http://ex/b> } }",
        &mut store,
    )
    .unwrap_err();
    assert!(err.to_lowercase().contains("named-graph"), "{err}");
    // The default-graph triple must survive — it was never targeted.
    assert_eq!(count_all(&store), 1);
}

#[test]
fn multi_op_insert_then_named_delete_data_aborts() {
    // A default-graph INSERT DATA followed by a named-graph DELETE DATA: the
    // whole update is rejected up front, so the insert never applies.
    let mut store = MemStore::default();
    let err = run(
        "INSERT DATA { <http://ex/a> <http://ex/p> <http://ex/b> } ; \
         DELETE DATA { GRAPH <http://g/1> { <http://ex/a> <http://ex/p> <http://ex/b> } }",
        &mut store,
    )
    .unwrap_err();
    assert!(err.to_lowercase().contains("named-graph"), "{err}");
    assert_eq!(count_all(&store), 0, "no op applies when a later op fails");
}
