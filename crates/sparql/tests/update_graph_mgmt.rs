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
    // HornBackend tombstone path used by clear_all).
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
    let mut p = std::env::temp_dir();
    p.push(format!("horndb_load_test_{}_{name}", std::process::id()));
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
