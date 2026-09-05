//! Graph-management SPARQL Update verbs (SPEC-07 #52): `LOAD`, `CLEAR`,
//! `DROP`, `CREATE`, `ADD`, `MOVE`, `COPY`. Exercised over both Stage-1
//! backends where the verb is backend-relevant. Named graphs are first-class
//! (SPEC-28 phase 4, #267), so these verbs operate on real named graphs under
//! D11 existence semantics (a graph exists iff it holds ≥1 visible quad). The
//! named-graph routing, WITH/USING, LOAD routing, SILENT recovery, and closed
//! reserved namespace live in `update_named_graph.rs`; this file keeps the
//! default-graph and multi-op coverage plus the graph-management pins.

use horndb_sparql::algebra::Term;
use horndb_sparql::api::{execute_query, QueryAnswer};
use horndb_sparql::exec::horn::HornBackend;
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::exec::{FullBackend, Store};
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

// D11 existence (SPEC-28 phase 4, #267): a named CLEAR/DROP of an *absent*
// graph is a silent no-op / non-silent error; of a graph *with data* it
// succeeds and retracts. The full existence matrix (incl. present-graph sweep)
// is in `update_named_graph.rs`; here we pin the absent-graph sub-case plus the
// with-data sweep for a MemStore.
#[test]
fn clear_named_graph_absent_is_noop_or_errors_present_sweeps() {
    // Absent graph: SILENT no-op, non-silent error naming existence.
    let mut store: MemStore = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    run("CLEAR SILENT GRAPH <http://g/1>", &mut store).unwrap();
    assert_eq!(
        count_all(&store),
        1,
        "silent clear of absent graph is a no-op"
    );
    let err = run("CLEAR GRAPH <http://g/1>", &mut store).unwrap_err();
    assert!(
        err.to_lowercase().contains("does not exist"),
        "non-silent clear of an absent graph should error: {err}"
    );

    // With data: CLEAR of a present named graph succeeds and retracts it.
    run(
        "INSERT DATA { GRAPH <http://g/1> { <http://ex/a> <http://ex/p> <http://ex/b> } }",
        &mut store,
    )
    .unwrap();
    run("CLEAR GRAPH <http://g/1>", &mut store).unwrap();
    assert!(
        !store.graph_exists("http://g/1"),
        "present graph swept to empty (D11)"
    );
}

#[test]
fn drop_named_graph_absent_is_noop_or_errors() {
    let mut store: MemStore = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    run("DROP SILENT GRAPH <http://g/1>", &mut store).unwrap();
    assert_eq!(count_all(&store), 1);
    let err = run("DROP GRAPH <http://g/1>", &mut store).unwrap_err();
    assert!(err.to_lowercase().contains("does not exist"), "{err}");
}

#[test]
fn clear_named_keyword_sweeps_named_graphs() {
    // CLEAR NAMED clears every non-reserved named graph and never errors; the
    // default graph is left untouched (SPEC-28 phase 4).
    let mut store: MemStore = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    run(
        "INSERT DATA { GRAPH <http://g/1> { <http://ex/a> <http://ex/p> <http://ex/n> } }",
        &mut store,
    )
    .unwrap();
    run("CLEAR NAMED", &mut store).unwrap();
    assert!(!store.graph_exists("http://g/1"), "named graph cleared");
    assert_eq!(
        count_all(&store),
        1,
        "default graph untouched by CLEAR NAMED"
    );
    // SILENT is accepted and equivalent here.
    run("CLEAR SILENT NAMED", &mut store).unwrap();
}

// ── CREATE ──────────────────────────────────────────────────────────────────

#[test]
fn create_named_graph_absent_ok_existing_errors_unless_silent() {
    // D11 (SPEC-28 phase 4): CREATE of an *absent* graph succeeds (no registry,
    // so no empty graph appears); of an *existing* graph it errors unless
    // SILENT.
    let mut store: MemStore = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    run("CREATE GRAPH <http://g/1>", &mut store).unwrap();
    assert!(
        !store.graph_exists("http://g/1"),
        "CREATE cannot make an empty graph exist"
    );
    assert_eq!(
        count_all(&store),
        1,
        "CREATE of an absent graph left the data alone"
    );

    // Populate it, then CREATE again: error unless SILENT.
    run(
        "INSERT DATA { GRAPH <http://g/1> { <http://ex/a> <http://ex/p> <http://ex/n> } }",
        &mut store,
    )
    .unwrap();
    let err = run("CREATE GRAPH <http://g/1>", &mut store).unwrap_err();
    assert!(
        err.to_lowercase().contains("already exists"),
        "CREATE of an existing graph should error: {err}"
    );
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
fn load_into_named_graph_routes_triples() {
    // SPEC-28 phase 4: LOAD of a triples format INTO GRAPH <g> now routes every
    // triple into that named graph (it was previously rejected). The default
    // graph stays empty.
    let path = write_tmp("data2.nt", "<http://ex/s> <http://ex/p> <http://ex/o> .\n");
    let mut store = MemStore::default();
    let u = format!("LOAD <file://{}> INTO GRAPH <http://g/1>", path.display());
    run(&u, &mut store).unwrap();
    // `count_all` is a union query, so the one triple it sees is the one now in
    // g/1 — proving nothing leaked into the default graph.
    assert_eq!(
        count_all(&store),
        1,
        "exactly the one loaded triple, in g/1"
    );
    assert!(
        store.graph_exists("http://g/1"),
        "the triple landed in the named graph"
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
fn add_named_operand_missing_source_errors() {
    // SPEC-28 phase 4: a named operand is representable now, so `ADD <g> TO
    // DEFAULT` executes. `<http://g/1>` is absent, so SPARQL 1.1 §3.2.3 makes a
    // non-silent ADD an error naming the missing source (recovered via the
    // SILENT tokenizer). The store is left untouched (caught in preflight).
    let mut store: MemStore = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    let err = run("ADD <http://g/1> TO DEFAULT", &mut store).unwrap_err();
    assert!(
        err.to_lowercase().contains("does not exist"),
        "absent source should error: {err}"
    );
    assert_eq!(count_all(&store), 1);
}

fn copy_named_to_default_missing_source_errors_without_data_loss<B: FullBackend + Default>() {
    // `COPY <absent> TO DEFAULT` desugars to `Drop{DEFAULT}` + a `DeleteInsert`
    // reading `GRAPH <absent>`. The atomicity preflight rejects the whole
    // update (absent source) before the destructive `Drop{DEFAULT}` runs, so
    // the seeded default-graph triple survives.
    let mut store: B = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    let err = run("COPY <http://g/1> TO DEFAULT", &mut store).unwrap_err();
    assert!(err.to_lowercase().contains("does not exist"), "{err}");
    assert_eq!(count_all(&store), 1, "failed update must not lose data");
}

#[test]
fn copy_named_to_default_missing_source_errors_without_data_loss_mem() {
    copy_named_to_default_missing_source_errors_without_data_loss::<MemStore>();
}
#[test]
fn copy_named_to_default_missing_source_errors_without_data_loss_horn() {
    copy_named_to_default_missing_source_errors_without_data_loss::<HornBackend>();
}

#[test]
fn move_named_to_default_missing_source_errors_without_data_loss() {
    // `MOVE <absent> TO DEFAULT` errors on its source-`Drop` (the one desugared
    // op that keeps its SILENT flag) before the destructive `Drop{DEFAULT}`
    // commits — atomicity preflight, no data loss.
    let mut store: MemStore = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    let err = run("MOVE <http://g/1> TO DEFAULT", &mut store).unwrap_err();
    assert!(err.to_lowercase().contains("does not exist"), "{err}");
    assert_eq!(count_all(&store), 1, "failed update must not lose data");
}

#[test]
fn multi_op_failing_op_aborts_before_destructive_op() {
    // A destructive op followed by a failing op: the whole update is rejected
    // up front, so the destructive op never runs. The later op writes the
    // reserved namespace (SPEC-28 S4), a state-independent error the preflight
    // catches before the CLEAR mutates.
    let mut store: MemStore = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    let err = run(
        "CLEAR DEFAULT ; INSERT DATA { GRAPH <https://horndb.io/graph/x> \
         { <http://ex/a> <http://ex/p> <http://ex/b> } }",
        &mut store,
    )
    .unwrap_err();
    assert!(err.to_lowercase().contains("reserved"), "{err}");
    assert_eq!(
        count_all(&store),
        1,
        "CLEAR must not run when a later op fails"
    );
}

#[test]
fn multi_op_clear_then_unsupported_where_aborts() {
    // A CLEAR followed by a DELETE WHERE whose WHERE uses an unsupported algebra
    // construct (SERVICE, HDB-133: MINUS is supported now) must abort before the
    // CLEAR mutates — the preflight translates/plans the WHERE, so the
    // translation failure is caught up front.
    let mut store: MemStore = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    let err = run(
        "CLEAR DEFAULT ; DELETE { ?s ?p ?o } WHERE { SERVICE <http://ex/svc> { ?s ?p ?o } }",
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

// The SILENT-recovery counterpart — `ADD SILENT <absent> TO …` is a no-op —
// lives in `update_named_graph.rs::add_silent_missing_source_is_noop` (it
// inverts the pre-phase-4 `add_named_operand_silent_still_errors`).

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

/// Every `?s` bound by `?s <http://ex/p> ?o`, sorted — enough to tell "the
/// store is back to its seed" apart from "a coincidentally equal triple count".
fn subjects<B: FullBackend>(store: &B) -> Vec<String> {
    let QueryAnswer::Solutions { rows, .. } =
        execute_query("SELECT ?s WHERE { ?s <http://ex/p> ?o }", store).unwrap()
    else {
        panic!("expected solutions");
    };
    let mut out: Vec<String> = rows
        .iter()
        .map(|r| match r.get("s") {
            Some(Term::Iri(s)) => s.clone(),
            other => panic!("expected an IRI subject, got {other:?}"),
        })
        .collect();
    out.sort();
    out
}

fn multi_op_failure_rolls_back_earlier_ops<B: FullBackend + Default>() {
    // The preflight cannot catch this one: `CREATE GRAPH <http://g/1>` is legal
    // against the pre-update store (the graph is absent there) and fails only
    // because the first op created it. SPARQL 1.1 §3.1.3 makes the whole
    // request atomic, so both earlier ops — an insert into a new named graph
    // and a delete of a pre-existing default-graph triple — must be undone.
    let mut store: B = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    let err = run(
        "INSERT DATA { GRAPH <http://g/1> { <http://ex/x> <http://ex/p> <http://ex/y> } } ; \
         DELETE WHERE { <http://ex/a> <http://ex/p> ?o } ; \
         CREATE GRAPH <http://g/1>",
        &mut store,
    )
    .unwrap_err();
    assert!(err.to_lowercase().contains("already exists"), "{err}");
    assert_eq!(
        subjects(&store),
        vec!["http://ex/a".to_owned()],
        "a failing op must roll the whole request back to its pre-update state"
    );
    assert!(
        !store.graph_exists("http://g/1"),
        "the rolled-back insert must leave no named graph behind (D11)"
    );
}

#[test]
fn multi_op_failure_rolls_back_earlier_ops_mem() {
    multi_op_failure_rolls_back_earlier_ops::<MemStore>();
}
#[test]
fn multi_op_failure_rolls_back_earlier_ops_horn() {
    multi_op_failure_rolls_back_earlier_ops::<HornBackend>();
}

// ── Named-graph data updates route to their graph (SPEC-28 phase 4) ──────────

#[test]
fn insert_data_named_graph_lands_in_graph() {
    // `INSERT DATA { GRAPH <g> { … } }` now writes the named graph, not the
    // default one. The seeded default triple and the new named triple are both
    // visible under union (two distinct triples).
    let mut store: MemStore = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    run(
        "INSERT DATA { GRAPH <http://g/1> { <http://ex/s> <http://ex/p> <http://ex/o> } }",
        &mut store,
    )
    .unwrap();
    assert!(
        store.graph_exists("http://g/1"),
        "the quad landed in the named graph"
    );
    assert_eq!(
        count_all(&store),
        2,
        "default triple + named triple both visible"
    );
}

#[test]
fn delete_data_absent_named_graph_is_noop() {
    // `DELETE DATA { GRAPH <absent> { … } }` deletes from an absent named graph:
    // a no-op that succeeds, leaving the default graph untouched.
    let mut store: MemStore = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    run(
        "DELETE DATA { GRAPH <http://g/1> { <http://ex/a> <http://ex/p> <http://ex/b> } }",
        &mut store,
    )
    .unwrap();
    assert_eq!(count_all(&store), 1, "the default-graph triple survives");
    assert!(!store.graph_exists("http://g/1"));
}

#[test]
fn multi_op_insert_then_named_delete_data_both_apply() {
    // A default-graph INSERT DATA followed by a named-graph DELETE DATA: both
    // are valid now, so both apply in order. The named DELETE (of an absent
    // graph) is a no-op, leaving the inserted default triple.
    let mut store = MemStore::default();
    run(
        "INSERT DATA { <http://ex/a> <http://ex/p> <http://ex/b> } ; \
         DELETE DATA { GRAPH <http://g/1> { <http://ex/a> <http://ex/p> <http://ex/b> } }",
        &mut store,
    )
    .unwrap();
    assert_eq!(
        count_all(&store),
        1,
        "the default INSERT applied; the named DELETE was a no-op"
    );
}
