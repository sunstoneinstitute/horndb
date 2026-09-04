//! Drives the W3C SPARQL 1.1 **Update** evaluation subset committed in
//! `crates/harness/tests/fixtures/sparql11/update_subset/`. For each selected
//! case it loads the initial dataset (`data.trig`), applies the update
//! (`request.ru`), and asserts **quad-set equality** of the resulting store
//! against the expected final dataset (`expected.trig`). The case list mirrors
//! `harness/selected.toml`'s `[sparql_update]` section.
//!
//! This is the update analog of `w3c_suite.rs` (the W3C query subset). Two
//! backends are exercised, so a divergence in either is caught:
//! * [`MemStore`] — the Stage-1 hash-set backend.
//! * [`HornBackend`] — the storage/WCOJ backend.
//!
//! **Comparison is D11-shaped (SPEC-28).** The final state is the *set of
//! visible quads* across the default graph and every named graph the store
//! reports (`graphs()`), decoded to algebra terms. A named graph exists iff it
//! holds ≥1 quad, so an empty-but-existing graph is indistinguishable from an
//! absent one — both contribute zero quads. W3C `clear/` cases whose expected
//! result declares an *empty-but-existing* named graph therefore cannot be
//! graded faithfully by quad-set equality; they are excluded (not selected) and
//! documented in `harness/KNOWN-MANIFEST-BUGS.md` with the D11 rationale, rather
//! than passed for the wrong reason.
//!
//! Both the mutated store and the expected store are read back through the same
//! `scan_graph_quads` seam of the same backend, so their term representations
//! are directly comparable — the update write path and the `data.trig` seed
//! path both go through each backend's storage layer.

use horndb_sparql::algebra::Term;
use horndb_sparql::exec::horn::HornBackend;
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::exec::FullBackend;
use horndb_sparql::parser::parse_update;
use horndb_sparql::update::apply_update;
use spargebra::algebra::GraphTarget;
use spargebra::term::NamedNode;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

fn fixtures_root() -> PathBuf {
    // tests live in crates/sparql/tests/, fixtures in crates/harness/
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop(); // crates/
    p.push("harness/tests/fixtures/sparql11/update_subset");
    p
}

// ── Seeding the initial / expected dataset from `data.trig` ──────────────────
//
// Mirrors `w3c_suite.rs`: the write trait (`exec::Store`) is quad-shaped since
// SPEC-28 phase 4, but seeding named graphs from a `.trig` still goes through
// each backend's storage seam (not the SPARQL Update policy layer), so a fixture
// can plant quads in any graph directly.

trait QuadSeed {
    fn seed_quad(&mut self, graph: Option<&oxrdf::NamedNode>, q: &oxrdf::Quad);

    /// Register the circuit rules a case's optional `rules.txt` names
    /// (SPEC-24 S4, HDB-51): one `transitive <iri>` per line. Only the
    /// `HornBackend` leg has a circuit, so such cases are listed on that leg
    /// alone.
    fn attach_rules(&mut self, rules: &str) {
        panic!("rules.txt is only supported on the HornBackend leg: {rules}");
    }
}

impl QuadSeed for MemStore {
    fn seed_quad(&mut self, graph: Option<&oxrdf::NamedNode>, q: &oxrdf::Quad) {
        // `MemStore` keeps terms as their N-Triples lexical form, IRIs and
        // blank-node labels *bare* (see `exec/mem.rs`).
        self.insert_quad(
            graph.map(oxrdf::NamedNode::as_str),
            (
                lex(&q.subject.clone().into()),
                lex(&q.predicate.clone().into()),
                lex(&q.object),
            ),
        );
    }
}

impl QuadSeed for HornBackend {
    #[cfg(feature = "incremental")]
    fn attach_rules(&mut self, rules: &str) {
        use horndb_incremental::{Circuit, TransitiveClosureRule};
        let mut circuit = Circuit::new();
        for line in rules.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let iri = line
                .strip_prefix("transitive <")
                .and_then(|r| r.strip_suffix('>'))
                .unwrap_or_else(|| panic!("rules.txt: unknown rule line {line:?}"));
            let p = self
                .intern_term(&oxrdf::Term::NamedNode(oxrdf::NamedNode::new_unchecked(
                    iri,
                )))
                .unwrap();
            circuit.add_closure_plan(Box::new(TransitiveClosureRule::new(p)));
        }
        self.attach_circuit(circuit).unwrap();
    }

    fn seed_quad(&mut self, graph: Option<&oxrdf::NamedNode>, q: &oxrdf::Quad) {
        let s = oxrdf::Term::from(q.subject.clone());
        let p = oxrdf::Term::from(q.predicate.clone());
        match graph {
            None => {
                self.insert_oxrdf(&s, &p, &q.object).unwrap();
            }
            Some(g) => {
                let g = oxrdf::Term::from(g.clone());
                self.insert_oxrdf_in_named_graph(&g, &s, &p, &q.object)
                    .unwrap();
            }
        }
    }
}

/// `MemStore`'s lexical form of an oxrdf term: bare IRI / bare blank-node
/// label / N-Triples literal.
fn lex(t: &oxrdf::Term) -> String {
    match t {
        oxrdf::Term::NamedNode(n) => n.as_str().to_owned(),
        oxrdf::Term::BlankNode(b) => b.as_str().to_owned(),
        other => other.to_string(),
    }
}

fn load_trig<S: QuadSeed + Default>(path: &Path) -> S {
    let mut s = S::default();
    let bytes = std::fs::read(path).expect("read .trig");
    for q in oxttl::TriGParser::new().for_slice(&bytes) {
        let q = q.expect("parse .trig");
        let graph = match &q.graph_name {
            oxrdf::GraphName::DefaultGraph => None,
            oxrdf::GraphName::NamedNode(g) => Some(g.clone()),
            oxrdf::GraphName::BlankNode(_) => panic!("blank-node graph names are not used"),
        };
        s.seed_quad(graph.as_ref(), &q);
    }
    s
}

// ── The final-state dump: every visible quad, keyed by graph ─────────────────

/// A visible quad in a store: the graph key (`None` = default graph) plus the
/// decoded subject/predicate/object. Comparable across two independently-built
/// stores of the same backend, since both decode through `scan_graph_quads`.
type QuadKey = (Option<String>, Term, Term, Term);

/// Snapshot the whole store as a set of visible quads (D11 view: only graphs
/// holding ≥1 quad appear).
fn dump<B: FullBackend>(store: &B) -> HashSet<QuadKey> {
    let mut out = HashSet::new();
    for (s, p, o) in store.scan_graph_quads(&GraphTarget::DefaultGraph).unwrap() {
        out.insert((None, s, p, o));
    }
    for g in store.graphs() {
        let target = GraphTarget::NamedNode(NamedNode::new_unchecked(&g));
        for (s, p, o) in store.scan_graph_quads(&target).unwrap() {
            out.insert((Some(g.clone()), s, p, o));
        }
    }
    out
}

fn run_one<B: FullBackend + QuadSeed + Default>(name: &str) {
    let dir = fixtures_root().join(name);

    // Initial state → (circuit rules, if the case has any) → apply the update
    // → final state. Rules attach after the seed so they see the whole base.
    let mut store: B = load_trig(&dir.join("data.trig"));
    if let Ok(rules) = std::fs::read_to_string(dir.join("rules.txt")) {
        store.attach_rules(&rules);
    }
    let request = std::fs::read_to_string(dir.join("request.ru")).expect("read request.ru");
    let parsed = parse_update(&request).unwrap_or_else(|e| panic!("{name}: parse: {e}"));
    apply_update(&parsed, &mut store).unwrap_or_else(|e| panic!("{name}: apply: {e}"));

    // Expected final state, loaded into a fresh store of the same backend.
    let expected: B = load_trig(&dir.join("expected.trig"));

    let got = dump(&store);
    let want = dump(&expected);
    assert_eq!(
        got, want,
        "{name}: final store state differs from expected.trig\n  only in store:    {:?}\n  only in expected: {:?}",
        got.difference(&want).collect::<Vec<_>>(),
        want.difference(&got).collect::<Vec<_>>()
    );
}

// ── MemStore leg ─────────────────────────────────────────────────────────────

macro_rules! update_case {
    ($name:ident, $dir:expr) => {
        #[test]
        fn $name() {
            run_one::<MemStore>($dir);
        }
    };
}

// W3C `add/` — ADD copies a graph's contents into another, source kept.
update_case!(add_01, "add-01");
update_case!(add_02, "add-02");
update_case!(add_03, "add-03");
update_case!(add_04, "add-04");
update_case!(add_05, "add-05");
update_case!(add_06, "add-06");
update_case!(add_07, "add-07");
update_case!(add_08, "add-08");

// W3C `copy/` — COPY overwrites the destination, source kept.
update_case!(copy_01, "copy-01");
update_case!(copy_02, "copy-02");
update_case!(copy_03, "copy-03");
update_case!(copy_04, "copy-04");
update_case!(copy_06, "copy-06");
update_case!(copy_07, "copy-07");

// W3C `move/` — MOVE overwrites the destination and empties the source.
update_case!(move_01, "move-01");
update_case!(move_02, "move-02");
update_case!(move_03, "move-03");
update_case!(move_04, "move-04");
update_case!(move_06, "move-06");
update_case!(move_07, "move-07");

// W3C `clear/` — only the cases that do not distinguish an empty-but-existing
// graph from an absent one are selected (D11). See KNOWN-MANIFEST-BUGS.md.
update_case!(clear_default_01, "clear-default-01");

// W3C `drop/` — DROP removes graphs, which matches D11 (an emptied graph ceases
// to exist), so all four cases grade faithfully.
update_case!(drop_default_01, "drop-default-01");
update_case!(drop_graph_01, "drop-graph-01");
update_case!(drop_named_01, "drop-named-01");
update_case!(drop_all_01, "drop-all-01");

// W3C `delete-insert/` — pattern-based DELETE/INSERT … WHERE on the default
// graph. `-01`/`-01b`/`-01c` pin single-op vs multi-op ordering (the request
// grain that Task 5's replay differential also pins).
update_case!(delete_insert_01, "delete-insert-01");
update_case!(delete_insert_01b, "delete-insert-01b");
update_case!(delete_insert_01c, "delete-insert-01c");
update_case!(delete_insert_02, "delete-insert-02");
// `-04` deletes via a DISTINCT subquery that includes `GRAPH ?g` arms (the
// graph variable is projected away), so it also confirms the phase-3 graph
// path inside a DELETE WHERE.
update_case!(delete_insert_04, "delete-insert-04");
update_case!(delete_insert_04b, "delete-insert-04b");
update_case!(delete_insert_05b, "delete-insert-05b");
update_case!(delete_insert_06b, "delete-insert-06b");

// W3C `delete/` — the two `dawg-delete-with-*` cases that pin SPARQL 1.1 Update
// §3.1.2: a bare `WITH <g>` sets only the default graph, so a ground
// `GRAPH <other>` inside WHERE still sees `<other>` (#281). The rest of the
// upstream `delete/` family is not mirrored yet.
update_case!(delete_with_02, "delete-with-02");
update_case!(delete_with_06, "delete-with-06");

// ── HornBackend leg ──────────────────────────────────────────────────────────

macro_rules! update_case_horn {
    ($name:ident, $dir:expr) => {
        #[test]
        fn $name() {
            run_one::<HornBackend>($dir);
        }
    };
}

update_case_horn!(add_01_hornbackend, "add-01");
update_case_horn!(add_02_hornbackend, "add-02");
update_case_horn!(add_03_hornbackend, "add-03");
update_case_horn!(add_04_hornbackend, "add-04");
update_case_horn!(add_05_hornbackend, "add-05");
update_case_horn!(add_06_hornbackend, "add-06");
update_case_horn!(add_07_hornbackend, "add-07");
update_case_horn!(add_08_hornbackend, "add-08");

update_case_horn!(copy_01_hornbackend, "copy-01");
update_case_horn!(copy_02_hornbackend, "copy-02");
update_case_horn!(copy_03_hornbackend, "copy-03");
update_case_horn!(copy_04_hornbackend, "copy-04");
update_case_horn!(copy_06_hornbackend, "copy-06");
update_case_horn!(copy_07_hornbackend, "copy-07");

update_case_horn!(move_01_hornbackend, "move-01");
update_case_horn!(move_02_hornbackend, "move-02");
update_case_horn!(move_03_hornbackend, "move-03");
update_case_horn!(move_04_hornbackend, "move-04");
update_case_horn!(move_06_hornbackend, "move-06");
update_case_horn!(move_07_hornbackend, "move-07");

update_case_horn!(clear_default_01_hornbackend, "clear-default-01");

update_case_horn!(drop_default_01_hornbackend, "drop-default-01");
update_case_horn!(drop_graph_01_hornbackend, "drop-graph-01");
update_case_horn!(drop_named_01_hornbackend, "drop-named-01");
update_case_horn!(drop_all_01_hornbackend, "drop-all-01");

update_case_horn!(delete_insert_01_hornbackend, "delete-insert-01");
update_case_horn!(delete_insert_01b_hornbackend, "delete-insert-01b");
update_case_horn!(delete_insert_01c_hornbackend, "delete-insert-01c");
update_case_horn!(delete_insert_02_hornbackend, "delete-insert-02");
update_case_horn!(delete_insert_04_hornbackend, "delete-insert-04");
update_case_horn!(delete_insert_04b_hornbackend, "delete-insert-04b");
update_case_horn!(delete_insert_05b_hornbackend, "delete-insert-05b");
update_case_horn!(delete_insert_06b_hornbackend, "delete-insert-06b");

update_case_horn!(delete_with_02_hornbackend, "delete-with-02");
update_case_horn!(delete_with_06_hornbackend, "delete-with-06");

// SPEC-24 S4 (HDB-51), not a W3C case: `DELETE DATA` against a store with a
// registered circuit rule withdraws the derived consequences. Needs the
// circuit, so HornBackend leg only, under the `incremental` feature.
#[cfg(feature = "incremental")]
update_case_horn!(circuit_delete_01_hornbackend, "circuit-delete-01");
