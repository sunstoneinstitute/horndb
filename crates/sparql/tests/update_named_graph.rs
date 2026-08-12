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

/// Regression: an identity `ADD <g> TO <g>` (zero desugared ops, one verb
/// token) co-occurring with a non-silent `COPY <absent> TO <dest>` must error
/// and leave `<dest>` intact. `COPY` desugars to `Drop(<dest>)` + a
/// source-reading `DeleteInsert` with no source `Drop`, so its only
/// absent-source guard is the recovered-`SILENT` preflight. Per-occurrence text
/// recovery (`recover_amc_hints`) recognises the identity op (skips it) and the
/// non-silent absent-source `COPY` (errors it) independently, so the error is
/// raised before the destructive `Drop(<dest>)` runs (SPARQL 1.1 §3.2.4 forbids
/// wiping `<dest>` without an error).
fn copy_absent_source_with_identity_coop_errors_no_wipe<B: FullBackend + Default>() {
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
fn copy_absent_source_with_identity_coop_errors_no_wipe_mem() {
    copy_absent_source_with_identity_coop_errors_no_wipe::<MemStore>();
}
#[test]
fn copy_absent_source_with_identity_coop_errors_no_wipe_horn() {
    copy_absent_source_with_identity_coop_errors_no_wipe::<HornBackend>();
}

// ── SILENT recovery: an identity op co-occurring with a SILENT op ────────────
//
// An identity `ADD`/`MOVE`/`COPY <g> TO <g>` desugars to *zero* operations but
// still emits one verb token in the source text. A per-occurrence text recovery
// (`recover_amc_hints`) handles this: it recovers `(silent, source,
// is_identity)` for every verb occurrence and drives the missing-source
// preflight straight off those hints, with no token-count↔op-count alignment to
// go wrong. The identity op is recognised (`is_identity`) and skipped; a
// following `SILENT` op keeps its flag, so an absent source stays a no-op.

/// `ADD <g> TO <g> ; ADD SILENT <missing> TO <dst>` succeeds and changes
/// nothing: the identity `ADD` is zero ops, and the `SILENT ADD` of an absent
/// source is a no-op (SPARQL 1.1 §3.2.3).
fn add_identity_then_silent_missing_source_ok<B: FullBackend + Default>() {
    let mut store = B::default();
    seed_quad(
        &mut store,
        None,
        "http://ex/a",
        "http://ex/p",
        "http://ex/b",
    );
    run(
        "ADD <http://ex/g> TO <http://ex/g> ; \
         ADD SILENT <http://ex/missing> TO <http://ex/dst>",
        &mut store,
    )
    .unwrap();
    assert_eq!(
        count_default(&store),
        1,
        "the default triple survives unchanged"
    );
    assert!(
        !store.graph_exists("http://ex/dst"),
        "the SILENT ADD of an absent source wrote nothing"
    );
}

#[test]
fn add_identity_then_silent_missing_source_ok_mem() {
    add_identity_then_silent_missing_source_ok::<MemStore>();
}
#[test]
fn add_identity_then_silent_missing_source_ok_horn() {
    add_identity_then_silent_missing_source_ok::<HornBackend>();
}

/// `COPY <g> TO <g> ; COPY SILENT <missing> TO <dst>` succeeds and clears
/// `<dst>`: the identity `COPY` is zero ops; the `SILENT COPY` drops `<dst>`
/// first (§3.2.4) and then copies from an absent source, a silent no-op — so
/// `<dst>` ends empty.
fn copy_identity_then_silent_missing_source_clears_dst<B: FullBackend + Default>() {
    let mut store = B::default();
    seed_quad(
        &mut store,
        Some("http://ex/dst"),
        "http://ex/keep",
        "http://ex/p",
        "http://ex/v",
    );
    assert_eq!(count_graph(&store, "http://ex/dst"), 1);
    run(
        "COPY <http://ex/g> TO <http://ex/g> ; \
         COPY SILENT <http://ex/missing> TO <http://ex/dst>",
        &mut store,
    )
    .unwrap();
    assert_eq!(
        count_graph(&store, "http://ex/dst"),
        0,
        "COPY drops the destination before the (no-op) copy"
    );
    assert!(!store.graph_exists("http://ex/dst"));
}

#[test]
fn copy_identity_then_silent_missing_source_clears_dst_mem() {
    copy_identity_then_silent_missing_source_clears_dst::<MemStore>();
}
#[test]
fn copy_identity_then_silent_missing_source_clears_dst_horn() {
    copy_identity_then_silent_missing_source_clears_dst::<HornBackend>();
}

/// `MOVE <g> TO <g> ; MOVE SILENT <missing> TO <dst>` succeeds and clears
/// `<dst>` (§3.2.5): the identity `MOVE` is zero ops; the `SILENT MOVE` drops
/// `<dst>`, copies from an absent source (a no-op), and silently drops that
/// absent source — so `<dst>` ends empty.
fn move_identity_then_silent_missing_source_clears_dst<B: FullBackend + Default>() {
    let mut store = B::default();
    seed_quad(
        &mut store,
        Some("http://ex/dst"),
        "http://ex/keep",
        "http://ex/p",
        "http://ex/v",
    );
    assert_eq!(count_graph(&store, "http://ex/dst"), 1);
    run(
        "MOVE <http://ex/g> TO <http://ex/g> ; \
         MOVE SILENT <http://ex/missing> TO <http://ex/dst>",
        &mut store,
    )
    .unwrap();
    assert_eq!(
        count_graph(&store, "http://ex/dst"),
        0,
        "MOVE drops the destination before the (no-op) move"
    );
    assert!(!store.graph_exists("http://ex/dst"));
}

#[test]
fn move_identity_then_silent_missing_source_clears_dst_mem() {
    move_identity_then_silent_missing_source_clears_dst::<MemStore>();
}
#[test]
fn move_identity_then_silent_missing_source_clears_dst_horn() {
    move_identity_then_silent_missing_source_clears_dst::<HornBackend>();
}

/// Guard (must hold both before and after this fix): a genuinely non-silent
/// `COPY` of an absent source still errors, and the destination is not wiped —
/// the preflight rejects it before the destructive `Drop(<dst>)` runs.
fn copy_nonsilent_missing_source_errors_no_wipe<B: FullBackend + Default>() {
    let mut store = B::default();
    seed_quad(
        &mut store,
        Some("http://ex/dst"),
        "http://ex/keep",
        "http://ex/p",
        "http://ex/v",
    );
    let err = run("COPY <http://ex/missing> TO <http://ex/dst>", &mut store).unwrap_err();
    assert!(err.to_lowercase().contains("does not exist"), "{err}");
    assert_eq!(
        count_graph(&store, "http://ex/dst"),
        1,
        "a failed non-silent COPY must not wipe the destination"
    );
}

#[test]
fn copy_nonsilent_missing_source_errors_no_wipe_mem() {
    copy_nonsilent_missing_source_errors_no_wipe::<MemStore>();
}
#[test]
fn copy_nonsilent_missing_source_errors_no_wipe_horn() {
    copy_nonsilent_missing_source_errors_no_wipe::<HornBackend>();
}

// ── SILENT recovery: a prefixed-name source needs structural resolution ──────
//
// `recover_amc_hints` reads operands from the raw text, so a *prefixed* source
// (`ex:missing`) it cannot expand degrades to `AmcSource::Unknown` — the
// preflight then falls back to the desugared copy-op's fully-resolved source
// IRI (spargebra has already expanded the prefix). Without that fallback a
// non-silent `COPY`/`MOVE` of an absent prefixed source would skip the
// existence check and let the destructive `Drop(<dst>)` wipe the destination.

/// Non-silent `COPY` of an absent *prefixed* source errors and leaves `<dst>`
/// intact (I-1 regression — the source resolves structurally, not from text).
fn copy_prefixed_missing_source_errors_no_wipe<B: FullBackend + Default>() {
    let mut store = B::default();
    seed_quad(
        &mut store,
        Some("http://ex/dst"),
        "http://ex/keep",
        "http://ex/p",
        "http://ex/v",
    );
    assert_eq!(count_graph(&store, "http://ex/dst"), 1);
    let err = run(
        "PREFIX ex: <http://ex/> COPY ex:missing TO <http://ex/dst>",
        &mut store,
    )
    .unwrap_err();
    assert!(err.to_lowercase().contains("does not exist"), "{err}");
    assert_eq!(
        count_graph(&store, "http://ex/dst"),
        1,
        "a failed COPY of a prefixed absent source must not wipe the destination"
    );
}

#[test]
fn copy_prefixed_missing_source_errors_no_wipe_mem() {
    copy_prefixed_missing_source_errors_no_wipe::<MemStore>();
}
#[test]
fn copy_prefixed_missing_source_errors_no_wipe_horn() {
    copy_prefixed_missing_source_errors_no_wipe::<HornBackend>();
}

/// Non-silent `MOVE` of an absent *prefixed* source errors and leaves `<dst>`
/// intact (I-1 regression, MOVE variant).
fn move_prefixed_missing_source_errors_no_wipe<B: FullBackend + Default>() {
    let mut store = B::default();
    seed_quad(
        &mut store,
        Some("http://ex/dst"),
        "http://ex/keep",
        "http://ex/p",
        "http://ex/v",
    );
    assert_eq!(count_graph(&store, "http://ex/dst"), 1);
    let err = run(
        "PREFIX ex: <http://ex/> MOVE ex:missing TO <http://ex/dst>",
        &mut store,
    )
    .unwrap_err();
    assert!(err.to_lowercase().contains("does not exist"), "{err}");
    assert_eq!(
        count_graph(&store, "http://ex/dst"),
        1,
        "a failed MOVE of a prefixed absent source must not wipe the destination"
    );
}

#[test]
fn move_prefixed_missing_source_errors_no_wipe_mem() {
    move_prefixed_missing_source_errors_no_wipe::<MemStore>();
}
#[test]
fn move_prefixed_missing_source_errors_no_wipe_horn() {
    move_prefixed_missing_source_errors_no_wipe::<HornBackend>();
}

/// Positive guard: a `SILENT COPY` of an absent *prefixed* source is a no-op
/// that still clears `<dst>` (§3.2.4) — the structural fallback errors only in
/// the non-silent case, so `SILENT` is still honoured for prefixed operands.
fn copy_silent_prefixed_missing_source_clears_dst<B: FullBackend + Default>() {
    let mut store = B::default();
    seed_quad(
        &mut store,
        Some("http://ex/dst"),
        "http://ex/keep",
        "http://ex/p",
        "http://ex/v",
    );
    assert_eq!(count_graph(&store, "http://ex/dst"), 1);
    run(
        "PREFIX ex: <http://ex/> COPY SILENT ex:missing TO <http://ex/dst>",
        &mut store,
    )
    .unwrap();
    assert_eq!(
        count_graph(&store, "http://ex/dst"),
        0,
        "SILENT COPY still drops the destination before the (no-op) copy"
    );
    assert!(!store.graph_exists("http://ex/dst"));
}

#[test]
fn copy_silent_prefixed_missing_source_clears_dst_mem() {
    copy_silent_prefixed_missing_source_clears_dst::<MemStore>();
}
#[test]
fn copy_silent_prefixed_missing_source_clears_dst_horn() {
    copy_silent_prefixed_missing_source_clears_dst::<HornBackend>();
}

// ── SILENT recovery: prologue-resolved, no op-inspection ─────────────────────
//
// Operands are resolved to absolute IRIs from the update's own PREFIX/BASE
// prologue, and the missing-source preflight reads only those recovered hints —
// never the desugared ops. So a user-written `{?s ?p ?o}` DeleteInsert can't be
// mistaken for a synthetic copy-op (Hole A), and a prefixed identity op is
// recognised and excluded like any other (Hole B).

/// Rows a `SELECT` returns (used to inspect exact graph contents).
fn select_rows<B: FullBackend>(store: &B, query: &str) -> usize {
    let QueryAnswer::Solutions { rows, .. } = execute_query(query, store).unwrap() else {
        panic!("expected solutions");
    };
    rows.len()
}

/// Hole A: a user `INSERT {?s ?p ?o} WHERE { GRAPH ex:absent {…} }` sharing the
/// s/p/o var names spargebra emits must NOT be treated as an AMC copy-op. The
/// request succeeds; the following `COPY ex:present TO <dst>` copies normally,
/// so `<dst>` ends holding ex:present's triple (neither falsely rejected nor
/// wiped by a phantom source-existence check).
fn user_deleteinsert_not_mistaken_for_copyop<B: FullBackend + Default>() {
    let mut store = B::default();
    seed_quad(
        &mut store,
        Some("http://ex/present"),
        "http://ex/a",
        "http://ex/p",
        "http://ex/b",
    );
    seed_quad(
        &mut store,
        Some("http://ex/dst"),
        "http://ex/x",
        "http://ex/p",
        "http://ex/y",
    );
    run(
        "PREFIX ex: <http://ex/> \
         INSERT { ?s ?p ?o } WHERE { GRAPH ex:absent { ?s ?p ?o } } ; \
         COPY ex:present TO <http://ex/dst>",
        &mut store,
    )
    .unwrap();
    // dst was cleared then took ex:present's single triple.
    assert_eq!(
        count_graph(&store, "http://ex/dst"),
        1,
        "dst holds one triple"
    );
    assert_eq!(
        select_rows(
            &store,
            "SELECT ?x WHERE { GRAPH <http://ex/dst> { <http://ex/a> <http://ex/p> <http://ex/b> } }",
        ),
        1,
        "dst now holds ex:present's triple",
    );
    assert_eq!(
        select_rows(
            &store,
            "SELECT ?x WHERE { GRAPH <http://ex/dst> { <http://ex/x> <http://ex/p> <http://ex/y> } }",
        ),
        0,
        "dst's original triple was overwritten by COPY, not kept",
    );
    assert_eq!(
        count_graph(&store, "http://ex/present"),
        1,
        "COPY left the source intact",
    );
}

#[test]
fn user_deleteinsert_not_mistaken_for_copyop_mem() {
    user_deleteinsert_not_mistaken_for_copyop::<MemStore>();
}
#[test]
fn user_deleteinsert_not_mistaken_for_copyop_horn() {
    user_deleteinsert_not_mistaken_for_copyop::<HornBackend>();
}

/// Hole B: a prefixed identity op (`COPY ex:g TO ex:g`, zero desugared ops)
/// co-occurring with a SILENT op and a non-silent absent-source op. Prologue
/// resolution recognises the identity (excluded) and honours the SILENT flag, so
/// the real non-silent `COPY ex:missing TO <dC>` is checked and errors — `<dC>`
/// is not silently wiped, and nothing else applies (atomicity).
fn prefixed_identity_coop_does_not_drop_real_hint<B: FullBackend + Default>() {
    let mut store = B::default();
    seed_quad(
        &mut store,
        Some("http://ex/present"),
        "http://ex/a",
        "http://ex/p",
        "http://ex/b",
    );
    seed_quad(
        &mut store,
        Some("http://ex/dC"),
        "http://ex/keep",
        "http://ex/p",
        "http://ex/v",
    );
    let err = run(
        "PREFIX ex: <http://ex/> \
         COPY ex:g TO ex:g ; \
         COPY SILENT ex:present TO <http://ex/dB> ; \
         COPY ex:missing TO <http://ex/dC>",
        &mut store,
    )
    .unwrap_err();
    assert!(
        err.contains("http://ex/missing") && err.to_lowercase().contains("does not exist"),
        "the real non-silent COPY of the absent source must error: {err}"
    );
    assert_eq!(
        count_graph(&store, "http://ex/dC"),
        1,
        "dC must not be wiped — the whole request aborts in preflight"
    );
    assert_eq!(
        count_graph(&store, "http://ex/dB"),
        0,
        "atomicity: the SILENT COPY never ran either",
    );
}

#[test]
fn prefixed_identity_coop_does_not_drop_real_hint_mem() {
    prefixed_identity_coop_does_not_drop_real_hint::<MemStore>();
}
#[test]
fn prefixed_identity_coop_does_not_drop_real_hint_horn() {
    prefixed_identity_coop_does_not_drop_real_hint::<HornBackend>();
}

/// BASE/relative: a non-silent `COPY <missing> TO <dst>` under `BASE
/// <http://ex/>` resolves the source to `http://ex/missing`; absent, so it
/// errors (naming the resolved IRI) and leaves `<dst>` intact.
fn base_relative_missing_source_errors_no_wipe<B: FullBackend + Default>() {
    let mut store = B::default();
    seed_quad(
        &mut store,
        Some("http://ex/dst"),
        "http://ex/keep",
        "http://ex/p",
        "http://ex/v",
    );
    let err = run("BASE <http://ex/> COPY <missing> TO <dst>", &mut store).unwrap_err();
    assert!(
        err.contains("http://ex/missing"),
        "the error must name the base-resolved source IRI: {err}"
    );
    assert_eq!(
        count_graph(&store, "http://ex/dst"),
        1,
        "a failed COPY must not wipe the base-resolved destination"
    );
}

#[test]
fn base_relative_missing_source_errors_no_wipe_mem() {
    base_relative_missing_source_errors_no_wipe::<MemStore>();
}
#[test]
fn base_relative_missing_source_errors_no_wipe_horn() {
    base_relative_missing_source_errors_no_wipe::<HornBackend>();
}
