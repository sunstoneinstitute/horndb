//! Named-graph SPARQL Update (SPEC-28 S4/S6, PLAN-28-04, #267): quad-data
//! `GRAPH` blocks, named-graph pattern templates, D11 graph-management
//! existence semantics, `LOAD` routing, the closed reserved namespace, and
//! `ADD`/`MOVE`/`COPY` between named graphs with recovered `SILENT` fidelity.
//!
//! Assertions read per-graph contents through the quad-shaped `Store` seam
//! (`scan_graph_quads`/`graph_exists`/`named_graphs`), so they pin the exact
//! graph a quad lands in rather than a union count. Exercised over both
//! Stage-1 backends where the behaviour is backend-relevant.

use horndb_sparql::algebra::Term;
use horndb_sparql::exec::horn::HornBackend;
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::exec::{FullBackend, GraphNamedNode as NamedNode, Store, StoreGraphTarget};
use horndb_sparql::parser::parse_update;
use horndb_sparql::update::apply_update;

fn iri(s: &str) -> Term {
    Term::Iri(s.to_owned())
}

/// `GraphTarget::NamedNode(<iri>)`.
fn tgt(g: &str) -> StoreGraphTarget {
    StoreGraphTarget::NamedNode(NamedNode::new_unchecked(g))
}

fn run(u: &str, store: &mut impl FullBackend) -> Result<(), String> {
    let parsed = parse_update(u).map_err(|e| e.to_string())?;
    apply_update(&parsed, store).map_err(|e| e.to_string())
}

/// Number of quads in `g` (default graph or a named graph).
fn count(store: &impl FullBackend, g: &StoreGraphTarget) -> usize {
    store.scan_graph_quads(g).unwrap().len()
}

/// Seed a quad directly through the store seam (no reserved-namespace guard),
/// so tests can plant data in any graph — including a reserved one.
fn seed_quad(store: &mut impl FullBackend, g: Option<&str>, s: &str, p: &str, o: &str) {
    store
        .apply_quads(Vec::new(), vec![(g.map(iri), iri(s), iri(p), iri(o))])
        .unwrap();
}

// ── Quad data: INSERT DATA / DELETE DATA GRAPH blocks ────────────────────────

fn insert_delete_data_graph_blocks<B: FullBackend + Default>() {
    let mut store = B::default();
    // A default-graph quad and a named-graph quad in one INSERT DATA.
    run(
        "INSERT DATA { \
           <http://ex/s> <http://ex/p> <http://ex/o> . \
           GRAPH <http://g/1> { <http://ex/s> <http://ex/p> <http://ex/o2> } }",
        &mut store,
    )
    .unwrap();
    assert_eq!(count(&store, &StoreGraphTarget::DefaultGraph), 1);
    assert_eq!(count(&store, &tgt("http://g/1")), 1);
    assert!(store.graph_exists("http://g/1"));
    // The named quad is distinct from the default one (same s/p, different o).
    assert_eq!(
        store.scan_graph_quads(&tgt("http://g/1")).unwrap(),
        vec![(iri("http://ex/s"), iri("http://ex/p"), iri("http://ex/o2"))]
    );

    // DELETE DATA from the named graph leaves the default graph untouched and,
    // once the named graph is empty, it ceases to exist (D11).
    run(
        "DELETE DATA { GRAPH <http://g/1> { <http://ex/s> <http://ex/p> <http://ex/o2> } }",
        &mut store,
    )
    .unwrap();
    assert_eq!(count(&store, &tgt("http://g/1")), 0);
    assert!(!store.graph_exists("http://g/1"));
    assert_eq!(count(&store, &StoreGraphTarget::DefaultGraph), 1);
}

#[test]
fn insert_delete_data_graph_blocks_mem() {
    insert_delete_data_graph_blocks::<MemStore>();
}
#[test]
fn insert_delete_data_graph_blocks_horn() {
    insert_delete_data_graph_blocks::<HornBackend>();
}

// ── One store batch per operation (SPEC-28 S4) ───────────────────────────────

fn one_batch_per_operation_state<B: FullBackend + Default>() {
    // `DELETE DATA{q} ; INSERT DATA{q} ; DELETE DATA{q}` ends with q ABSENT.
    // A single collapsed dels-before-adds batch would end with q PRESENT, so
    // the final state distinguishes per-op batching from a wrong collapse.
    let mut store = B::default();
    run(
        "DELETE DATA { <http://ex/a> <http://ex/p> <http://ex/b> } ; \
         INSERT DATA { <http://ex/a> <http://ex/p> <http://ex/b> } ; \
         DELETE DATA { <http://ex/a> <http://ex/p> <http://ex/b> }",
        &mut store,
    )
    .unwrap();
    assert_eq!(count(&store, &StoreGraphTarget::DefaultGraph), 0);
}

#[test]
fn one_batch_per_operation_state_mem() {
    one_batch_per_operation_state::<MemStore>();
}
#[test]
fn one_batch_per_operation_state_horn() {
    one_batch_per_operation_state::<HornBackend>();
}

#[test]
fn one_batch_per_operation() {
    // Commit-version delta == number of *effective* ops (each net-effect op is
    // exactly one store batch/commit). The HornBackend's underlying store bumps
    // its version once per net-effect batch and never for a no-op batch.
    let mut store = HornBackend::default();
    let v0 = store.version();
    // Three distinct inserts: three effective ops -> three commits.
    run(
        "INSERT DATA { <http://ex/a> <http://ex/p> <http://ex/b> } ; \
         INSERT DATA { <http://ex/c> <http://ex/p> <http://ex/d> } ; \
         INSERT DATA { <http://ex/e> <http://ex/p> <http://ex/f> }",
        &mut store,
    )
    .unwrap();
    assert_eq!(store.version() - v0, 3, "one commit per effective op");

    // A leading no-op delete (absent quad) contributes zero commits.
    let v1 = store.version();
    run(
        "DELETE DATA { <http://ex/absent> <http://ex/p> <http://ex/x> } ; \
         INSERT DATA { <http://ex/g> <http://ex/p> <http://ex/h> } ; \
         DELETE DATA { <http://ex/g> <http://ex/p> <http://ex/h> }",
        &mut store,
    )
    .unwrap();
    assert_eq!(store.version() - v1, 2, "the no-op delete is not a commit");
}

// ── Pattern update with a named-graph template ───────────────────────────────

fn pattern_update_named_template<B: FullBackend + Default>() {
    let mut store = B::default();
    seed_quad(
        &mut store,
        Some("http://g/src"),
        "http://ex/a",
        "http://ex/p",
        "http://ex/b",
    );

    // Copy the source graph's contents into a destination graph via templates.
    run(
        "INSERT { GRAPH <http://g/dst> { ?s ?p ?o } } \
         WHERE { GRAPH <http://g/src> { ?s ?p ?o } }",
        &mut store,
    )
    .unwrap();
    assert_eq!(
        store.scan_graph_quads(&tgt("http://g/dst")).unwrap(),
        vec![(iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"))]
    );
    // Source is unchanged; default graph untouched.
    assert_eq!(count(&store, &tgt("http://g/src")), 1);
    assert_eq!(count(&store, &StoreGraphTarget::DefaultGraph), 0);
}

#[test]
fn pattern_update_named_template_mem() {
    pattern_update_named_template::<MemStore>();
}
#[test]
fn pattern_update_named_template_horn() {
    pattern_update_named_template::<HornBackend>();
}

// ── CREATE / CLEAR / DROP existence semantics (D11 matrix) ───────────────────

fn create_clear_drop_existence_semantics<B: FullBackend + Default>() {
    let mut store = B::default();

    // CREATE of an absent graph succeeds (no registry: the graph still does not
    // "exist" until it holds a quad — D11).
    run("CREATE GRAPH <http://g/1>", &mut store).unwrap();
    assert!(!store.graph_exists("http://g/1"));

    // Populate it, then CREATE of an existing graph errors unless SILENT.
    seed_quad(
        &mut store,
        Some("http://g/1"),
        "http://ex/a",
        "http://ex/p",
        "http://ex/b",
    );
    assert!(store.graph_exists("http://g/1"));
    let err = run("CREATE GRAPH <http://g/1>", &mut store).unwrap_err();
    assert!(err.to_lowercase().contains("exist"), "{err}");
    run("CREATE SILENT GRAPH <http://g/1>", &mut store).unwrap();
    assert_eq!(
        count(&store, &tgt("http://g/1")),
        1,
        "SILENT CREATE is a no-op"
    );

    // CLEAR / DROP of an ABSENT graph: error unless SILENT.
    let err = run("CLEAR GRAPH <http://g/absent>", &mut store).unwrap_err();
    assert!(err.to_lowercase().contains("exist"), "{err}");
    run("CLEAR SILENT GRAPH <http://g/absent>", &mut store).unwrap();
    let err = run("DROP GRAPH <http://g/absent>", &mut store).unwrap_err();
    assert!(err.to_lowercase().contains("exist"), "{err}");
    run("DROP SILENT GRAPH <http://g/absent>", &mut store).unwrap();

    // CLEAR of a present graph empties it; the graph then ceases to exist.
    run("CLEAR GRAPH <http://g/1>", &mut store).unwrap();
    assert!(!store.graph_exists("http://g/1"));
    assert!(!Store::named_graphs(&store).contains(&"http://g/1".to_owned()));

    // DROP of a present graph likewise removes it from graphs().
    seed_quad(
        &mut store,
        Some("http://g/2"),
        "http://ex/a",
        "http://ex/p",
        "http://ex/b",
    );
    run("DROP GRAPH <http://g/2>", &mut store).unwrap();
    assert!(!store.graph_exists("http://g/2"));
    assert!(!Store::named_graphs(&store).contains(&"http://g/2".to_owned()));
}

#[test]
fn create_clear_drop_existence_semantics_mem() {
    create_clear_drop_existence_semantics::<MemStore>();
}
#[test]
fn create_clear_drop_existence_semantics_horn() {
    create_clear_drop_existence_semantics::<HornBackend>();
}

// ── DROP ALL spares reserved graphs ──────────────────────────────────────────

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
        Some("http://g/user"),
        "http://ex/c",
        "http://ex/p",
        "http://ex/d",
    );
    // A reserved (HornDB-internal) graph, seeded through the store seam since
    // the update path is closed to writes on the reserved namespace.
    seed_quad(
        &mut store,
        Some("https://horndb.io/graph/inferred"),
        "http://ex/x",
        "http://ex/p",
        "http://ex/y",
    );

    run("DROP ALL", &mut store).unwrap();

    assert_eq!(
        count(&store, &StoreGraphTarget::DefaultGraph),
        0,
        "default dropped"
    );
    assert!(!store.graph_exists("http://g/user"), "user graph dropped");
    assert!(
        store.graph_exists("https://horndb.io/graph/inferred"),
        "DROP ALL must spare reserved graphs (SPEC-28 S4)"
    );
    assert_eq!(count(&store, &tgt("https://horndb.io/graph/inferred")), 1);
}

#[test]
fn drop_all_spares_reserved_mem() {
    drop_all_spares_reserved::<MemStore>();
}
#[test]
fn drop_all_spares_reserved_horn() {
    drop_all_spares_reserved::<HornBackend>();
}

// ── CLEAR/DROP flow through the delta boundary (no structural unlink) ─────────

fn clear_drop_flow_through_delta_boundary<B: FullBackend + Default>() {
    // `clear_graph` retracts every visible quad through `apply_quads`, so the
    // reported retraction count equals the graph's quad count — a structural
    // partition unlink would bypass the counted delta path (SPEC-28 S4).
    let mut store = B::default();
    for i in 0..3 {
        seed_quad(
            &mut store,
            Some("http://g/1"),
            &format!("http://ex/s{i}"),
            "http://ex/p",
            "http://ex/o",
        );
    }
    let retracted = store.clear_graph(&tgt("http://g/1")).unwrap();
    assert_eq!(retracted, 3, "clear_graph must report a quad-grain count");
    assert!(!store.graph_exists("http://g/1"));
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
    let path = write_tmp(
        "d.nt",
        "<http://ex/s> <http://ex/p> <http://ex/o> .\n<http://ex/s> <http://ex/p> <http://ex/o2> .\n",
    );

    // Triples format INTO GRAPH <g>: everything lands in g, default untouched.
    let mut store = B::default();
    run(
        &format!(
            "LOAD <file://{}> INTO GRAPH <http://g/dest>",
            path.display()
        ),
        &mut store,
    )
    .unwrap();
    assert_eq!(count(&store, &tgt("http://g/dest")), 2);
    assert_eq!(count(&store, &StoreGraphTarget::DefaultGraph), 0);

    // Plain LOAD (no INTO): the default graph.
    let mut store2 = B::default();
    run(&format!("LOAD <file://{}>", path.display()), &mut store2).unwrap();
    assert_eq!(count(&store2, &StoreGraphTarget::DefaultGraph), 2);

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
    // A plain LOAD of an N-Quads document routes each quad to its own graph,
    // matching the bulk N-Quads loader (SPEC-28 S4).
    let path = write_tmp(
        "d.nq",
        "<http://ex/s> <http://ex/p> <http://ex/o> <http://g/1> .\n\
         <http://ex/s> <http://ex/p> <http://ex/o2> <http://g/2> .\n\
         <http://ex/s> <http://ex/p> <http://ex/od> .\n",
    );
    let mut store = B::default();
    run(&format!("LOAD <file://{}>", path.display()), &mut store).unwrap();
    assert_eq!(count(&store, &tgt("http://g/1")), 1);
    assert_eq!(count(&store, &tgt("http://g/2")), 1);
    assert_eq!(count(&store, &StoreGraphTarget::DefaultGraph), 1);
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
    // Redirecting a dataset document's quads into a single graph is not defined
    // (W3C LOAD is a graph operation) — a non-silent error.
    let path = write_tmp(
        "d2.nq",
        "<http://ex/s> <http://ex/p> <http://ex/o> <http://g/1> .\n",
    );
    let mut store = MemStore::default();
    let err = run(
        &format!("LOAD <file://{}> INTO GRAPH <http://g/x>", path.display()),
        &mut store,
    )
    .unwrap_err();
    assert!(err.to_lowercase().contains("named graph"), "{err}");
    assert_eq!(count(&store, &tgt("http://g/x")), 0);
    std::fs::remove_file(&path).ok();
}

// ── The reserved namespace is closed to writes (not SILENT-suppressible) ─────

fn reserved_namespace_closed_to_writes<B: FullBackend + Default>() {
    const R: &str = "https://horndb.io/graph/secret";
    // Every write form, with AND without SILENT, must error and mutate nothing.
    // `{verb}` is substituted; each is a distinct write targeting R.
    let path = write_tmp("r.nt", "<http://ex/s> <http://ex/p> <http://ex/o> .\n");
    let load = format!("LOAD <file://{}> INTO GRAPH <{R}>", path.display());
    let load_silent = format!("LOAD SILENT <file://{}> INTO GRAPH <{R}>", path.display());
    let forms: Vec<String> = vec![
        format!("INSERT DATA {{ GRAPH <{R}> {{ <http://ex/s> <http://ex/p> <http://ex/o> }} }}"),
        format!("DELETE DATA {{ GRAPH <{R}> {{ <http://ex/s> <http://ex/p> <http://ex/o> }} }}"),
        format!("INSERT {{ GRAPH <{R}> {{ ?s ?p ?o }} }} WHERE {{ ?s ?p ?o }}"),
        format!("CREATE GRAPH <{R}>"),
        format!("CREATE SILENT GRAPH <{R}>"),
        format!("CLEAR GRAPH <{R}>"),
        format!("CLEAR SILENT GRAPH <{R}>"),
        format!("DROP GRAPH <{R}>"),
        format!("DROP SILENT GRAPH <{R}>"),
        load,
        load_silent,
        format!("ADD <http://g/src> TO <{R}>"),
        format!("MOVE <http://g/src> TO <{R}>"),
        format!("COPY <http://g/src> TO <{R}>"),
    ];
    for form in &forms {
        let mut store = B::default();
        // A source graph for the ADD/MOVE/COPY forms; a default-graph triple so
        // the pattern-INSERT form has a row to (attempt to) copy.
        seed_quad(
            &mut store,
            Some("http://g/src"),
            "http://ex/a",
            "http://ex/p",
            "http://ex/b",
        );
        seed_quad(
            &mut store,
            None,
            "http://ex/s",
            "http://ex/p",
            "http://ex/o",
        );
        let err = run(form, &mut store).unwrap_err();
        assert!(
            err.to_lowercase().contains("reserved"),
            "form `{form}` should be a reserved-namespace error, got: {err}"
        );
        // Reads of a reserved graph stay allowed and nothing was written to R.
        assert_eq!(count(&store, &tgt(R)), 0, "form `{form}` must not write R");
    }
    std::fs::remove_file(&path).ok();
}

#[test]
fn reserved_namespace_closed_to_writes_mem() {
    reserved_namespace_closed_to_writes::<MemStore>();
}
#[test]
fn reserved_namespace_closed_to_writes_horn() {
    reserved_namespace_closed_to_writes::<HornBackend>();
}

fn reserved_namespace_closed_to_runtime_bound_graph_var<B: FullBackend + Default>() {
    // The reserved-namespace closure must hold even when the target graph is a
    // template variable that binds to a reserved IRI only at runtime (VALUES,
    // BIND, or USING NAMED enumeration) — otherwise a `GRAPH ?g` template is a
    // hole straight into the reserved namespace (SPEC-28 S4). The error is
    // permission-shaped and raised before the operation's `apply_quads`, so the
    // operation writes nothing.
    const R: &str = "https://horndb.io/graph/secret";

    // INSERT with `?g` bound to a reserved IRI via VALUES.
    let mut store = B::default();
    let err = run(
        &format!(
            "INSERT {{ GRAPH ?g {{ <http://ex/s> <http://ex/p> <http://ex/o> }} }} \
             WHERE {{ VALUES ?g {{ <{R}> }} }}"
        ),
        &mut store,
    )
    .unwrap_err();
    assert!(
        err.to_lowercase().contains("reserved"),
        "VALUES route: {err}"
    );
    assert_eq!(
        count(&store, &tgt(R)),
        0,
        "nothing may land in the reserved graph"
    );

    // INSERT with `?g` bound to a reserved IRI via BIND.
    let mut store = B::default();
    let err = run(
        &format!(
            "INSERT {{ GRAPH ?g {{ <http://ex/s> <http://ex/p> <http://ex/o> }} }} \
             WHERE {{ BIND(<{R}> AS ?g) }}"
        ),
        &mut store,
    )
    .unwrap_err();
    assert!(err.to_lowercase().contains("reserved"), "BIND route: {err}");
    assert_eq!(count(&store, &tgt(R)), 0);

    // DELETE into a reserved graph is a write too — seed the reserved graph and
    // confirm a reserved-bound DELETE template errors and removes nothing.
    let mut store = B::default();
    seed_quad(
        &mut store,
        Some(R),
        "http://ex/s",
        "http://ex/p",
        "http://ex/o",
    );
    let err = run(
        &format!(
            "DELETE {{ GRAPH ?g {{ <http://ex/s> <http://ex/p> <http://ex/o> }} }} \
             WHERE {{ VALUES ?g {{ <{R}> }} }}"
        ),
        &mut store,
    )
    .unwrap_err();
    assert!(
        err.to_lowercase().contains("reserved"),
        "DELETE route: {err}"
    );
    assert_eq!(count(&store, &tgt(R)), 1, "the reserved quad must survive");

    // USING NAMED <reserved> enumerates the reserved graph into `?g`; copying it
    // into `GRAPH ?g` (itself) is still a write to the reserved namespace.
    let mut store = B::default();
    seed_quad(
        &mut store,
        Some(R),
        "http://ex/s",
        "http://ex/p",
        "http://ex/o",
    );
    let err = run(
        &format!(
            "INSERT {{ GRAPH ?g {{ ?s ?p ?o }} }} USING NAMED <{R}> \
             WHERE {{ GRAPH ?g {{ ?s ?p ?o }} }}"
        ),
        &mut store,
    )
    .unwrap_err();
    assert!(
        err.to_lowercase().contains("reserved"),
        "USING NAMED route: {err}"
    );
    assert_eq!(count(&store, &tgt(R)), 1);

    // Regression guard: a `GRAPH ?g` template binding `?g` to a NON-reserved IRI
    // still writes normally.
    let mut store = B::default();
    run(
        "INSERT { GRAPH ?g { <http://ex/s> <http://ex/p> <http://ex/o> } } \
         WHERE { VALUES ?g { <http://g/ok> } }",
        &mut store,
    )
    .unwrap();
    assert_eq!(
        count(&store, &tgt("http://g/ok")),
        1,
        "non-reserved ?g still works"
    );
}

#[test]
fn reserved_namespace_closed_to_runtime_bound_graph_var_mem() {
    reserved_namespace_closed_to_runtime_bound_graph_var::<MemStore>();
}
#[test]
fn reserved_namespace_closed_to_runtime_bound_graph_var_horn() {
    reserved_namespace_closed_to_runtime_bound_graph_var::<HornBackend>();
}

// ── ADD / MOVE / COPY between named graphs ───────────────────────────────────

fn add_move_copy_between_named_graphs<B: FullBackend + Default>() {
    // ADD merges the source into the destination, leaving the source intact.
    let mut store = B::default();
    seed_quad(
        &mut store,
        Some("http://g/1"),
        "http://ex/a",
        "http://ex/p",
        "http://ex/b",
    );
    seed_quad(
        &mut store,
        Some("http://g/2"),
        "http://ex/c",
        "http://ex/p",
        "http://ex/d",
    );
    run("ADD <http://g/1> TO <http://g/2>", &mut store).unwrap();
    assert_eq!(count(&store, &tgt("http://g/2")), 2, "ADD merges into dest");
    assert_eq!(
        count(&store, &tgt("http://g/1")),
        1,
        "ADD leaves source intact"
    );

    // COPY replaces the destination with the source's contents.
    let mut store = B::default();
    seed_quad(
        &mut store,
        Some("http://g/1"),
        "http://ex/a",
        "http://ex/p",
        "http://ex/b",
    );
    seed_quad(
        &mut store,
        Some("http://g/2"),
        "http://ex/c",
        "http://ex/p",
        "http://ex/d",
    );
    run("COPY <http://g/1> TO <http://g/2>", &mut store).unwrap();
    assert_eq!(
        store.scan_graph_quads(&tgt("http://g/2")).unwrap(),
        vec![(iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"))],
        "COPY replaces the destination"
    );
    assert_eq!(
        count(&store, &tgt("http://g/1")),
        1,
        "COPY leaves source intact"
    );

    // MOVE removes the source.
    let mut store = B::default();
    seed_quad(
        &mut store,
        Some("http://g/1"),
        "http://ex/a",
        "http://ex/p",
        "http://ex/b",
    );
    run("MOVE <http://g/1> TO <http://g/2>", &mut store).unwrap();
    assert_eq!(count(&store, &tgt("http://g/2")), 1);
    assert!(!store.graph_exists("http://g/1"), "MOVE removes the source");

    // Named graph <-> DEFAULT.
    let mut store = B::default();
    seed_quad(
        &mut store,
        Some("http://g/1"),
        "http://ex/a",
        "http://ex/p",
        "http://ex/b",
    );
    run("MOVE <http://g/1> TO DEFAULT", &mut store).unwrap();
    assert_eq!(count(&store, &StoreGraphTarget::DefaultGraph), 1);
    assert!(!store.graph_exists("http://g/1"));
}

#[test]
fn add_move_copy_between_named_graphs_mem() {
    add_move_copy_between_named_graphs::<MemStore>();
}
#[test]
fn add_move_copy_between_named_graphs_horn() {
    add_move_copy_between_named_graphs::<HornBackend>();
}

fn add_silent_missing_source_is_noop<B: FullBackend + Default>() {
    // SPEC-28 S4: SILENT ADD with a missing source is a true no-op (ADD has no
    // destination DROP in its desugaring); a non-silent ADD is an error. COPY
    // and MOVE are different — see `copy_silent_missing_source_clears_destination`
    // / `move_silent_missing_source_clears_destination` below. spargebra drops
    // the SILENT flag when it desugars these verbs, so the flag is recovered
    // from the source text (PLAN-28-04); without recovery both cases would be
    // silent no-ops.
    let mut store = B::default();
    seed_quad(
        &mut store,
        None,
        "http://ex/keep",
        "http://ex/p",
        "http://ex/v",
    );

    run("ADD SILENT <http://g/missing> TO DEFAULT", &mut store).unwrap();
    assert_eq!(
        count(&store, &StoreGraphTarget::DefaultGraph),
        1,
        "SILENT ADD noop"
    );

    let err = run("ADD <http://g/missing> TO DEFAULT", &mut store).unwrap_err();
    assert!(err.to_lowercase().contains("source"), "{err}");
    assert_eq!(
        count(&store, &StoreGraphTarget::DefaultGraph),
        1,
        "failed ADD keeps data"
    );
}

#[test]
fn add_silent_missing_source_is_noop_mem() {
    add_silent_missing_source_is_noop::<MemStore>();
}
#[test]
fn add_silent_missing_source_is_noop_horn() {
    add_silent_missing_source_is_noop::<HornBackend>();
}

fn copy_silent_missing_source_clears_destination<B: FullBackend + Default>() {
    // SPEC-28 S4: spargebra desugars COPY into a leading `Drop { silent: true,
    // graph: <to> }` followed by a copy from <from>. So a SILENT COPY with a
    // missing source is NOT a no-op: the destination is cleared by the leading
    // drop, then nothing is copied in, leaving the destination empty. Only ADD
    // (no leading drop in its desugaring) is a true no-op — see
    // `add_silent_missing_source_is_noop` above.
    let mut store = B::default();
    seed_quad(
        &mut store,
        Some("http://g/dest"),
        "http://ex/keep",
        "http://ex/p",
        "http://ex/v",
    );

    run(
        "COPY SILENT <http://g/missing> TO <http://g/dest>",
        &mut store,
    )
    .unwrap();
    assert_eq!(
        count(&store, &tgt("http://g/dest")),
        0,
        "SILENT COPY from a missing source clears the destination"
    );
    assert!(!store.graph_exists("http://g/dest"));
}

#[test]
fn copy_silent_missing_source_clears_destination_mem() {
    copy_silent_missing_source_clears_destination::<MemStore>();
}
#[test]
fn copy_silent_missing_source_clears_destination_horn() {
    copy_silent_missing_source_clears_destination::<HornBackend>();
}

fn move_silent_missing_source_clears_destination<B: FullBackend + Default>() {
    // SPEC-28 S4: spargebra desugars MOVE into `Drop { silent: true, graph:
    // <to> }`, then a copy from <from>, then `Drop { silent, graph: <from> }`.
    // The leading drop on the destination is unconditional (always silent),
    // so a SILENT MOVE with a missing source clears the destination the same
    // way COPY does — see `copy_silent_missing_source_clears_destination`.
    let mut store = B::default();
    seed_quad(
        &mut store,
        Some("http://g/dest"),
        "http://ex/keep",
        "http://ex/p",
        "http://ex/v",
    );

    run(
        "MOVE SILENT <http://g/missing> TO <http://g/dest>",
        &mut store,
    )
    .unwrap();
    assert_eq!(
        count(&store, &tgt("http://g/dest")),
        0,
        "SILENT MOVE from a missing source clears the destination"
    );
    assert!(!store.graph_exists("http://g/dest"));
}

#[test]
fn move_silent_missing_source_clears_destination_mem() {
    move_silent_missing_source_clears_destination::<MemStore>();
}
#[test]
fn move_silent_missing_source_clears_destination_horn() {
    move_silent_missing_source_clears_destination::<HornBackend>();
}
