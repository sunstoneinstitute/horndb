//! Named-graph SPARQL Update (SPEC-28 phase 4, S4, #267): quad data, pattern
//! updates that route templates per graph, the graph-management verbs under
//! D11 existence semantics, `WITH`/`USING`, `LOAD` destination routing, the
//! `SILENT` fidelity of `ADD`/`MOVE`/`COPY`, and the closed reserved
//! namespace. Exercised over both Stage-1 backends where relevant.

use horndb_sparql::algebra::Term;
use horndb_sparql::api::{execute_query, QueryAnswer};
use horndb_sparql::exec::horn::HornBackend;
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::exec::{AlgebraQuad, FullBackend};
use horndb_sparql::parser::parse_update;
use horndb_sparql::update::apply_update;
use spargebra::algebra::GraphTarget;

const RESERVED: &str = "https://horndb.io/graph/inferred";

fn run(u: &str, store: &mut impl FullBackend) -> Result<(), String> {
    let parsed = parse_update(u).map_err(|e| e.to_string())?;
    apply_update(&parsed, store).map_err(|e| e.to_string())
}

/// Number of triples in the default graph only (the unnamed graph). Reads the
/// store seam directly, so it is not affected by the query's union/strict mode.
fn count_default<B: FullBackend>(store: &B) -> usize {
    store
        .scan_graph_quads(&GraphTarget::DefaultGraph)
        .unwrap()
        .len()
}

/// Number of triples in a specific named graph (ground `GRAPH <g>`).
fn count_graph<B: FullBackend>(store: &B, g: &str) -> usize {
    let q = format!("SELECT ?s ?p ?o WHERE {{ GRAPH <{g}> {{ ?s ?p ?o }} }}");
    let QueryAnswer::Solutions { rows, .. } = execute_query(&q, store).unwrap() else {
        panic!("expected solutions");
    };
    rows.len()
}

fn iri(s: &str) -> Term {
    Term::Iri(s.to_owned())
}

/// Seed a quad straight through the store write seam (bypasses the update
/// policy layer, so it can seed reserved graphs a query is otherwise closed
/// to writing).
fn seed_quad<B: FullBackend>(store: &mut B, g: Option<&str>, s: &str, p: &str, o: &str) {
    let q: AlgebraQuad = (g.map(str::to_owned), iri(s), iri(p), iri(o));
    store.apply_quads(Vec::new(), vec![q]).unwrap();
}

// ── Quad data: INSERT DATA / DELETE DATA route by graph ──────────────────────

fn insert_delete_data_graph_blocks<B: FullBackend + Default>() {
    let mut store = B::default();
    // A default-graph quad and a named-graph quad in one INSERT DATA.
    run(
        "INSERT DATA { \
           <http://ex/a> <http://ex/p> <http://ex/d> . \
           GRAPH <http://g/1> { <http://ex/a> <http://ex/p> <http://ex/g1> } \
         }",
        &mut store,
    )
    .unwrap();
    assert_eq!(
        count_default(&store),
        1,
        "default-graph quad landed in default"
    );
    assert_eq!(
        count_graph(&store, "http://g/1"),
        1,
        "named quad landed in g1"
    );

    // DELETE DATA from the named graph leaves the default graph untouched.
    run(
        "DELETE DATA { GRAPH <http://g/1> { <http://ex/a> <http://ex/p> <http://ex/g1> } }",
        &mut store,
    )
    .unwrap();
    assert_eq!(count_graph(&store, "http://g/1"), 0, "named quad retracted");
    assert_eq!(
        count_default(&store),
        1,
        "default graph untouched by named DELETE"
    );
    assert!(
        !store.graph_exists("http://g/1"),
        "D11: emptied graph ceases to exist"
    );
}

#[test]
fn insert_delete_data_graph_blocks_mem() {
    insert_delete_data_graph_blocks::<MemStore>();
}
#[test]
fn insert_delete_data_graph_blocks_horn() {
    insert_delete_data_graph_blocks::<HornBackend>();
}

/// One store batch per operation, applied in request order, never collapsed:
/// `DELETE DATA{q};INSERT DATA{q};DELETE DATA{q}` ends absent, and the mirror
/// `INSERT;DELETE;INSERT` ends present — the sequence, not a collapsed net.
fn one_batch_per_operation<B: FullBackend + Default>() {
    let q = "<http://ex/a> <http://ex/p> <http://ex/b>";
    let mut store = B::default();
    run(
        &format!("DELETE DATA {{ {q} }} ; INSERT DATA {{ {q} }} ; DELETE DATA {{ {q} }}"),
        &mut store,
    )
    .unwrap();
    assert_eq!(count_default(&store), 0, "del;ins;del ends absent");

    let mut store2 = B::default();
    run(
        &format!("INSERT DATA {{ {q} }} ; DELETE DATA {{ {q} }} ; INSERT DATA {{ {q} }}"),
        &mut store2,
    )
    .unwrap();
    assert_eq!(
        count_default(&store2),
        1,
        "ins;del;ins ends present (no collapse)"
    );
}

#[test]
fn one_batch_per_operation_mem() {
    one_batch_per_operation::<MemStore>();
}
#[test]
fn one_batch_per_operation_horn() {
    one_batch_per_operation::<HornBackend>();
}

// ── Pattern updates: templates route per GraphNamePattern ─────────────────────

fn pattern_update_named_template<B: FullBackend + Default>() {
    let mut store = B::default();
    seed_quad(
        &mut store,
        None,
        "http://ex/a",
        "http://ex/p",
        "http://ex/b",
    );
    seed_quad(
        &mut store,
        None,
        "http://ex/c",
        "http://ex/p",
        "http://ex/d",
    );
    // Copy every default-graph triple into a named graph via a template.
    run(
        "INSERT { GRAPH <http://g/1> { ?s ?p ?o } } WHERE { ?s ?p ?o }",
        &mut store,
    )
    .unwrap();
    assert_eq!(
        count_graph(&store, "http://g/1"),
        2,
        "template routed to g1"
    );
    assert_eq!(
        count_default(&store),
        2,
        "INSERT does not remove the source rows"
    );
}

#[test]
fn pattern_update_named_template_mem() {
    pattern_update_named_template::<MemStore>();
}
#[test]
fn pattern_update_named_template_horn() {
    pattern_update_named_template::<HornBackend>();
}

/// `WITH <g>` scopes both templates and the WHERE clause to `<g>`. spargebra
/// 0.4.6 expresses the WHERE-side scope through `USING <g>` (not a
/// `GraphPattern::Graph` wrapper — see `update.rs`), so this pins that both
/// sides resolve to `<g>` end to end.
fn with_scopes_both_sides<B: FullBackend + Default>() {
    let mut store = B::default();
    seed_quad(
        &mut store,
        Some("http://g/1"),
        "http://ex/a",
        "http://ex/p",
        "http://ex/b",
    );
    // Read from g1 (WHERE), write a derived triple back into g1 (template).
    run(
        "WITH <http://g/1> INSERT { ?s ?p <http://ex/seen> } WHERE { ?s ?p ?o }",
        &mut store,
    )
    .unwrap();
    assert_eq!(
        count_graph(&store, "http://g/1"),
        2,
        "WITH read g1 and wrote back to g1"
    );
    assert_eq!(
        count_default(&store),
        0,
        "the default graph is untouched by WITH <g1>"
    );
}

#[test]
fn with_scopes_both_sides_mem() {
    with_scopes_both_sides::<MemStore>();
}
#[test]
fn with_scopes_both_sides_horn() {
    with_scopes_both_sides::<HornBackend>();
}

/// `USING <g>` builds the WHERE dataset (phase-3 `DatasetSpec` machinery): the
/// WHERE reads `<g>` as its default graph while the DELETE/INSERT template
/// (unscoped) writes the default graph.
fn using_builds_where_dataset<B: FullBackend + Default>() {
    let mut store = B::default();
    seed_quad(
        &mut store,
        Some("http://g/1"),
        "http://ex/a",
        "http://ex/p",
        "http://ex/b",
    );
    run(
        "INSERT { ?s ?p ?o } USING <http://g/1> WHERE { ?s ?p ?o }",
        &mut store,
    )
    .unwrap();
    assert_eq!(
        count_default(&store),
        1,
        "USING read g1; template wrote the default graph"
    );
    assert_eq!(count_graph(&store, "http://g/1"), 1, "g1 unchanged");
}

#[test]
fn using_builds_where_dataset_mem() {
    using_builds_where_dataset::<MemStore>();
}
#[test]
fn using_builds_where_dataset_horn() {
    using_builds_where_dataset::<HornBackend>();
}

// ── CREATE / CLEAR / DROP existence (D11) ────────────────────────────────────

fn create_clear_drop_existence_semantics<B: FullBackend + Default>() {
    let mut store = B::default();

    // CREATE of an absent graph: succeeds, no-op (D11 has no empty graphs).
    run("CREATE GRAPH <http://g/1>", &mut store).unwrap();
    assert!(
        !store.graph_exists("http://g/1"),
        "CREATE cannot make an empty graph exist"
    );

    // Give g1 a quad so it exists.
    seed_quad(
        &mut store,
        Some("http://g/1"),
        "http://ex/a",
        "http://ex/p",
        "http://ex/b",
    );
    assert!(store.graph_exists("http://g/1"));

    // CREATE of an existing graph: error unless SILENT.
    assert!(
        run("CREATE GRAPH <http://g/1>", &mut store).is_err(),
        "CREATE of existing errors"
    );
    run("CREATE SILENT GRAPH <http://g/1>", &mut store).unwrap();
    assert_eq!(
        count_graph(&store, "http://g/1"),
        1,
        "SILENT CREATE left the data alone"
    );

    // CLEAR/DROP of an absent graph: error unless SILENT.
    assert!(run("CLEAR GRAPH <http://absent>", &mut store).is_err());
    run("CLEAR SILENT GRAPH <http://absent>", &mut store).unwrap();
    assert!(run("DROP GRAPH <http://absent>", &mut store).is_err());
    run("DROP SILENT GRAPH <http://absent>", &mut store).unwrap();

    // DROP of a present graph: retracts every quad; the graph then stops
    // existing (D11).
    run("DROP GRAPH <http://g/1>", &mut store).unwrap();
    assert!(
        !store.graph_exists("http://g/1"),
        "DROP emptied g1 → gone from graphs()"
    );
    assert!(store.graphs().is_empty());
}

#[test]
fn create_clear_drop_existence_semantics_mem() {
    create_clear_drop_existence_semantics::<MemStore>();
}
#[test]
fn create_clear_drop_existence_semantics_horn() {
    create_clear_drop_existence_semantics::<HornBackend>();
}

/// `DROP ALL` empties the default graph and every non-reserved named graph
/// quad by quad, but leaves reserved (`https://horndb.io/graph/…`) graphs
/// untouched — SPEC-30 owns the store-level reset.
fn drop_all_spares_reserved<B: FullBackend + Default>() {
    let mut store = B::default();
    seed_quad(
        &mut store,
        None,
        "http://ex/a",
        "http://ex/p",
        "http://ex/b",
    );
    seed_quad(
        &mut store,
        Some("http://g/1"),
        "http://ex/a",
        "http://ex/p",
        "http://ex/c",
    );
    seed_quad(
        &mut store,
        Some(RESERVED),
        "http://ex/a",
        "http://ex/p",
        "http://ex/inferred",
    );

    run("DROP ALL", &mut store).unwrap();

    assert_eq!(count_default(&store), 0, "default graph swept");
    assert!(
        !store.graph_exists("http://g/1"),
        "non-reserved named graph swept"
    );
    assert!(
        store.graph_exists(RESERVED),
        "reserved graph spared by DROP ALL"
    );
    assert_eq!(store.graphs(), vec![RESERVED.to_owned()]);
}

#[test]
fn drop_all_spares_reserved_mem() {
    drop_all_spares_reserved::<MemStore>();
}
#[test]
fn drop_all_spares_reserved_horn() {
    drop_all_spares_reserved::<HornBackend>();
}

/// CLEAR/DROP retract quad by quad through the store's delete path (no
/// structural unlink): the swept graph's exact quads leave, siblings are
/// untouched, and the graph re-accepts inserts afterward.
fn clear_drop_flow_through_delta_boundary<B: FullBackend + Default>() {
    let mut store = B::default();
    seed_quad(
        &mut store,
        Some("http://g/1"),
        "http://ex/a",
        "http://ex/p",
        "http://ex/1",
    );
    seed_quad(
        &mut store,
        Some("http://g/1"),
        "http://ex/a",
        "http://ex/p",
        "http://ex/2",
    );
    seed_quad(
        &mut store,
        Some("http://g/2"),
        "http://ex/a",
        "http://ex/p",
        "http://ex/3",
    );
    assert_eq!(count_graph(&store, "http://g/1"), 2);

    run("CLEAR GRAPH <http://g/1>", &mut store).unwrap();
    assert_eq!(
        count_graph(&store, "http://g/1"),
        0,
        "g1's two quads retracted"
    );
    assert_eq!(count_graph(&store, "http://g/2"), 1, "sibling g2 untouched");

    // The graph re-accepts inserts (delete path, not a corrupt structural drop).
    run(
        "INSERT DATA { GRAPH <http://g/1> { <http://ex/a> <http://ex/p> <http://ex/r> } }",
        &mut store,
    )
    .unwrap();
    assert_eq!(
        count_graph(&store, "http://g/1"),
        1,
        "re-insert after CLEAR works"
    );
}

#[test]
fn clear_drop_flow_through_delta_boundary_mem() {
    clear_drop_flow_through_delta_boundary::<MemStore>();
}
#[test]
fn clear_drop_flow_through_delta_boundary_horn() {
    clear_drop_flow_through_delta_boundary::<HornBackend>();
}

// ── LOAD routing ─────────────────────────────────────────────────────────────

fn write_tmp(name: &str, body: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let mut p = std::env::temp_dir();
    p.push(format!(
        "horndb_load_ng_{}_{seq}_{name}",
        std::process::id()
    ));
    std::fs::write(&p, body).unwrap();
    p
}

fn load_routes_to_destination<B: FullBackend + Default>() {
    // A triples format (.nt) with an INTO GRAPH destination routes every
    // triple into that named graph; the default graph stays empty.
    let path = write_tmp(
        "t.nt",
        "<http://ex/s> <http://ex/p> <http://ex/o> .\n<http://ex/s> <http://ex/p> <http://ex/o2> .\n",
    );
    let mut store = B::default();
    let u = format!("LOAD <file://{}> INTO GRAPH <http://g/dst>", path.display());
    run(&u, &mut store).unwrap();
    assert_eq!(
        count_graph(&store, "http://g/dst"),
        2,
        "triples routed into the destination"
    );
    assert_eq!(
        count_default(&store),
        0,
        "default graph untouched by INTO GRAPH"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn load_routes_to_destination_mem() {
    load_routes_to_destination::<MemStore>();
}
#[test]
fn load_routes_to_destination_horn() {
    load_routes_to_destination::<HornBackend>();
}

fn load_nq_routes_quads_to_their_graphs<B: FullBackend + Default>() {
    // A dataset format (.nq) with a plain LOAD (no INTO) routes each quad to
    // its own named graph, matching the N-Quads loader.
    let path = write_tmp(
        "d.nq",
        "<http://ex/s> <http://ex/p> <http://ex/o> <http://g/a> .\n\
         <http://ex/s> <http://ex/p> <http://ex/o2> <http://g/b> .\n\
         <http://ex/s> <http://ex/p> <http://ex/o3> .\n",
    );
    let mut store = B::default();
    let u = format!("LOAD <file://{}>", path.display());
    run(&u, &mut store).unwrap();
    assert_eq!(count_graph(&store, "http://g/a"), 1, "quad routed to g/a");
    assert_eq!(count_graph(&store, "http://g/b"), 1, "quad routed to g/b");
    assert_eq!(
        count_default(&store),
        1,
        "the default-graph quad stayed in the default graph"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn load_nq_routes_quads_to_their_graphs_mem() {
    load_nq_routes_quads_to_their_graphs::<MemStore>();
}
#[test]
fn load_nq_routes_quads_to_their_graphs_horn() {
    load_nq_routes_quads_to_their_graphs::<HornBackend>();
}

#[test]
fn load_nq_into_graph_errors() {
    // Redirecting a quad source into one graph is undefined; a dataset format
    // with INTO GRAPH is an error naming the reason.
    let path = write_tmp(
        "e.nq",
        "<http://ex/s> <http://ex/p> <http://ex/o> <http://g/a> .\n",
    );
    let mut store = MemStore::default();
    let u = format!("LOAD <file://{}> INTO GRAPH <http://g/dst>", path.display());
    let err = run(&u, &mut store).unwrap_err();
    assert!(err.to_lowercase().contains("dataset"), "{err}");
    assert_eq!(count_default(&store), 0, "no partial load on the error");
    std::fs::remove_file(&path).ok();
}

// ── Reserved namespace closed to writes (S4) ─────────────────────────────────

fn reserved_namespace_closed_to_writes<B: FullBackend + Default>() {
    // Every write form must be refused, with AND without SILENT — the reserved
    // check is a permission error, not suppressible by SILENT.
    let r = RESERVED;
    let writes: &[String] = &[
        format!("INSERT DATA {{ GRAPH <{r}> {{ <http://ex/a> <http://ex/p> <http://ex/b> }} }}"),
        format!("DELETE DATA {{ GRAPH <{r}> {{ <http://ex/a> <http://ex/p> <http://ex/b> }} }}"),
        format!("INSERT {{ GRAPH <{r}> {{ ?s ?p ?o }} }} WHERE {{ ?s ?p ?o }}"),
        format!("CREATE GRAPH <{r}>"),
        format!("CREATE SILENT GRAPH <{r}>"),
        format!("CLEAR GRAPH <{r}>"),
        format!("CLEAR SILENT GRAPH <{r}>"),
        format!("DROP GRAPH <{r}>"),
        format!("DROP SILENT GRAPH <{r}>"),
        format!("LOAD <file:///tmp/x.nt> INTO GRAPH <{r}>"),
        format!("LOAD SILENT <file:///tmp/x.nt> INTO GRAPH <{r}>"),
        format!("ADD <http://g/1> TO <{r}>"),
        format!("ADD SILENT <http://g/1> TO <{r}>"),
        format!("COPY <http://g/1> TO <{r}>"),
        format!("MOVE <http://g/1> TO <{r}>"),
    ];
    for w in writes {
        let mut store = B::default();
        seed_quad(
            &mut store,
            None,
            "http://ex/x",
            "http://ex/p",
            "http://ex/y",
        );
        seed_quad(
            &mut store,
            Some("http://g/1"),
            "http://ex/a",
            "http://ex/p",
            "http://ex/b",
        );
        let err = run(w, &mut store).unwrap_err();
        assert!(
            err.to_lowercase().contains("reserved"),
            "reserved write must error naming the namespace: `{w}` -> `{err}`"
        );
    }
}

#[test]
fn reserved_namespace_closed_to_writes_mem() {
    reserved_namespace_closed_to_writes::<MemStore>();
}
#[test]
fn reserved_namespace_closed_to_writes_horn() {
    reserved_namespace_closed_to_writes::<HornBackend>();
}

#[test]
fn reserved_graph_reads_are_allowed() {
    // Reads of a reserved graph stay allowed (only writes are closed).
    let mut store = MemStore::default();
    seed_quad(
        &mut store,
        Some(RESERVED),
        "http://ex/a",
        "http://ex/p",
        "http://ex/b",
    );
    assert_eq!(
        count_graph(&store, RESERVED),
        1,
        "ground GRAPH read of a reserved graph works"
    );
}

// ── ADD / MOVE / COPY between named graphs + SILENT recovery ─────────────────

fn add_move_copy_between_named_graphs<B: FullBackend + Default>() {
    let mut store = B::default();
    seed_quad(
        &mut store,
        Some("http://g/src"),
        "http://ex/a",
        "http://ex/p",
        "http://ex/b",
    );

    // ADD copies src → dst, leaving src intact.
    run("ADD <http://g/src> TO <http://g/add>", &mut store).unwrap();
    assert_eq!(
        count_graph(&store, "http://g/add"),
        1,
        "ADD copied into dst"
    );
    assert_eq!(
        count_graph(&store, "http://g/src"),
        1,
        "ADD kept the source"
    );

    // COPY overwrites the destination, leaving src intact.
    seed_quad(
        &mut store,
        Some("http://g/copy"),
        "http://ex/z",
        "http://ex/p",
        "http://ex/old",
    );
    run("COPY <http://g/src> TO <http://g/copy>", &mut store).unwrap();
    assert_eq!(
        count_graph(&store, "http://g/copy"),
        1,
        "COPY overwrote the destination"
    );
    assert!(count_graph(&store, "http://g/copy") == 1);
    assert_eq!(
        count_graph(&store, "http://g/src"),
        1,
        "COPY kept the source"
    );

    // MOVE moves src → mv and empties src.
    run("MOVE <http://g/src> TO <http://g/mv>", &mut store).unwrap();
    assert_eq!(
        count_graph(&store, "http://g/mv"),
        1,
        "MOVE wrote the destination"
    );
    assert!(
        !store.graph_exists("http://g/src"),
        "MOVE emptied the source"
    );
}

#[test]
fn add_move_copy_between_named_graphs_mem() {
    add_move_copy_between_named_graphs::<MemStore>();
}
#[test]
fn add_move_copy_between_named_graphs_horn() {
    add_move_copy_between_named_graphs::<HornBackend>();
}

/// `ADD SILENT <absent> TO <g>` is a no-op (the SILENT-recovery pin): the
/// tokenizer recovers the SILENT flag spargebra drops, so a missing source is
/// swallowed. Its non-silent form errors — see `add_missing_source_errors`.
fn add_silent_missing_source_is_noop<B: FullBackend + Default>() {
    let mut store = B::default();
    seed_quad(
        &mut store,
        None,
        "http://ex/a",
        "http://ex/p",
        "http://ex/b",
    );
    run("ADD SILENT <http://g/absent> TO <http://g/dst>", &mut store).unwrap();
    assert_eq!(
        count_default(&store),
        1,
        "SILENT ADD of an absent source is a no-op"
    );
    assert!(!store.graph_exists("http://g/dst"));
}

#[test]
fn add_silent_missing_source_is_noop_mem() {
    add_silent_missing_source_is_noop::<MemStore>();
}
#[test]
fn add_silent_missing_source_is_noop_horn() {
    add_silent_missing_source_is_noop::<HornBackend>();
}

/// Non-silent `ADD`/`COPY` of an absent named source errors (SPARQL 1.1
/// §3.2.3/§3.2.5), recovered via the SILENT tokenizer + source-existence
/// check. No destructive pre-step runs (atomicity preflight).
fn add_missing_source_errors<B: FullBackend + Default>() {
    let mut store = B::default();
    seed_quad(
        &mut store,
        None,
        "http://ex/a",
        "http://ex/p",
        "http://ex/b",
    );
    let err = run("ADD <http://g/absent> TO <http://g/dst>", &mut store).unwrap_err();
    assert!(err.to_lowercase().contains("does not exist"), "{err}");

    let err = run("COPY <http://g/absent> TO DEFAULT", &mut store).unwrap_err();
    assert!(err.to_lowercase().contains("does not exist"), "{err}");
    assert_eq!(
        count_default(&store),
        1,
        "COPY's destructive pre-DROP never ran"
    );
}

#[test]
fn add_missing_source_errors_mem() {
    add_missing_source_errors::<MemStore>();
}
#[test]
fn add_missing_source_errors_horn() {
    add_missing_source_errors::<HornBackend>();
}

/// Regression (whole-branch review): a non-silent `COPY <absent> TO <dest>` on
/// the **ambiguous** SILENT-alignment path must error and leave `<dest>`
/// intact. COPY desugars to `Drop(<dest>)` + a source-reading `DeleteInsert`
/// with no source `Drop`, so its ONLY absent-source guard is the recovered
/// SILENT check. A silent-equivalent fallback would run `Drop(<dest>)`, read
/// zero rows, and wipe `<dest>` with no error (SPARQL 1.1 §3.2.4 forbids this).
///
/// The ambiguity is forced deterministically: an identity `ADD <g> TO <g>`
/// (one source token, zero desugared ops) co-occurs with the COPY, so the hint
/// count (2) ≠ the copy-op count (1) — the ambiguous branch — which now falls
/// back to non-silent, so the absent source errors in preflight before the
/// destructive `Drop` runs.
fn copy_absent_source_ambiguous_alignment_errors_no_wipe<B: FullBackend + Default>() {
    let mut store = B::default();
    // Seed the destination so a wrongful Drop would be observable.
    seed_quad(
        &mut store,
        Some("http://g/dest"),
        "http://ex/keep",
        "http://ex/p",
        "http://ex/v",
    );
    assert_eq!(count_graph(&store, "http://g/dest"), 1);

    let err = run(
        "ADD <http://g/x> TO <http://g/x> ; \
         COPY <http://g/absent> TO <http://g/dest>",
        &mut store,
    )
    .unwrap_err();
    assert!(err.to_lowercase().contains("does not exist"), "{err}");
    assert_eq!(
        count_graph(&store, "http://g/dest"),
        1,
        "the destination must survive: the failed COPY's Drop(<dest>) never ran"
    );
}

#[test]
fn copy_absent_source_ambiguous_alignment_errors_no_wipe_mem() {
    copy_absent_source_ambiguous_alignment_errors_no_wipe::<MemStore>();
}
#[test]
fn copy_absent_source_ambiguous_alignment_errors_no_wipe_horn() {
    copy_absent_source_ambiguous_alignment_errors_no_wipe::<HornBackend>();
}
