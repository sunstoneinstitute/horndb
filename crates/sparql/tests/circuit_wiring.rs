//! SPEC-24 S4 (HDB-51): SPARQL Update -> circuit tick -> derived rows visible
//! to the next query, and `DELETE DATA` withdrawing them — observed through
//! both `/query` and the change feed. Every test is state-driven: the tick and
//! its feed drain run synchronously on the updating thread, so there is
//! nothing to wait for and no sleeps anywhere.

#![cfg(all(feature = "incremental", feature = "server"))]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use horndb_incremental::{
    BilinearRule, Circuit, DeltaRecord, DerivationKind, NaryPlan, RuleId, TransitiveClosureRule,
    TripleId, Zset,
};
use horndb_sparql::algebra::Term;
use horndb_sparql::api::{execute_query, execute_update, QueryAnswer};
use horndb_sparql::exec::circuit::DERIVED_GRAPH;
use horndb_sparql::exec::horn::HornBackend;
use horndb_sparql::exec::Store;
use horndb_sparql::server::{build_router, AppState};
use horndb_storage::Store as ColumnStore;
use oxrdf::{NamedNode, Term as OxTerm};
use parking_lot::RwLock;
use spargebra::algebra::GraphTarget;
use std::collections::BTreeSet;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tower::ServiceExt;

const P: &str = "http://ex/p";

fn ox(iri: &str) -> OxTerm {
    OxTerm::NamedNode(NamedNode::new_unchecked(iri))
}

fn id(b: &HornBackend, iri: &str) -> u64 {
    b.intern_term(&ox(iri)).unwrap()
}

fn triple(b: &HornBackend, s: &str, p: &str, o: &str) -> TripleId {
    (id(b, s), id(b, p), id(b, o))
}

/// A backend whose circuit closes `<p>` transitively (the `add_closure_plan`
/// seam), attached with `capacity` feed slots.
fn transitive_backend(capacity: usize) -> HornBackend {
    let mut b = HornBackend::new();
    let mut c = Circuit::new();
    c.add_closure_plan(Box::new(TransitiveClosureRule::new(id(&b, P))));
    b.attach_circuit_with_feed_capacity(c, capacity).unwrap();
    b
}

/// `SELECT ?s ?o WHERE { ?s <p> ?o }` over the default union, as
/// `(s, o)` IRI pairs.
fn edges(b: &HornBackend) -> BTreeSet<(String, String)> {
    let q = format!("SELECT ?s ?o WHERE {{ ?s <{P}> ?o }}");
    let QueryAnswer::Solutions { rows, .. } = execute_query(&q, b).unwrap() else {
        panic!("not a SELECT");
    };
    rows.into_iter()
        .map(|r| {
            let Some(Term::Iri(s)) = r.get("s").cloned() else {
                panic!()
            };
            let Some(Term::Iri(o)) = r.get("o").cloned() else {
                panic!()
            };
            (s, o)
        })
        .collect()
}

fn pair(s: &str, o: &str) -> (String, String) {
    (s.to_owned(), o.to_owned())
}

/// The invariant the wiring maintains: the derived graph holds exactly the
/// circuit's derived base (`mult > 0`).
fn assert_derived_graph_mirrors_circuit(b: &mut HornBackend) {
    let target = GraphTarget::NamedNode(NamedNode::new_unchecked(DERIVED_GRAPH));
    let in_graph: BTreeSet<TripleId> = b
        .scan_graph_quads(&target)
        .unwrap()
        .into_iter()
        .map(|(s, p, o)| {
            let iri = |t: Term| match t {
                Term::Iri(i) => i,
                other => panic!("non-IRI derived term {other:?}"),
            };
            triple(b, &iri(s), &iri(p), &iri(o))
        })
        .collect();
    let in_circuit: BTreeSet<TripleId> = b
        .circuit()
        .unwrap()
        .derived_base()
        .iter()
        .filter(|(_, m)| *m > 0)
        .map(|(t, _)| *t)
        .collect();
    assert_eq!(
        in_graph, in_circuit,
        "derived graph != circuit derived base"
    );
}

fn drain(rx: &horndb_incremental::ChangeFeedRx) -> Vec<DeltaRecord> {
    std::iter::from_fn(|| rx.try_recv().ok()).collect()
}

// ── HTTP end to end ──────────────────────────────────────────────────────────

async fn post(app: &axum::Router, path: &str, ctype: &str, body: &str) -> (StatusCode, Vec<u8>) {
    let req = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", ctype)
        .header("accept", "application/sparql-results+json")
        .body(Body::from(body.to_owned()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let body = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, body.to_vec())
}

async fn update(app: &axum::Router, sparql: &str) {
    let (status, body) = post(app, "/update", "application/sparql-update", sparql).await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "{}",
        String::from_utf8_lossy(&body)
    );
}

async fn select_pairs(app: &axum::Router) -> BTreeSet<(String, String)> {
    let q = format!("SELECT ?s ?o WHERE {{ ?s <{P}> ?o }}");
    let (status, body) = post(app, "/query", "application/sparql-query", &q).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    v["results"]["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["s"]["value"].as_str().unwrap().to_owned(),
                r["o"]["value"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

/// SPEC-24 acceptance 4: `INSERT DATA` derives through the circuit and the
/// next `/query` sees it; `DELETE DATA` against the store with a registered
/// rule withdraws the consequence, observable through `/query` and through
/// an audit subscription on the change feed.
#[tokio::test]
async fn http_update_ticks_and_query_sees_derived_then_withdrawn() {
    let mut b = transitive_backend(1 << 10);
    let audit = b.circuit().unwrap().subscribe();
    let a_p_c = triple(&b, "http://ex/a", P, "http://ex/c");
    let state = AppState {
        store: Arc::new(RwLock::new(b)),
        config: Default::default(),
        ready: Arc::new(AtomicBool::new(true)),
        admission: Default::default(),
    };
    let store = Arc::clone(&state.store);
    let app = build_router(state);

    update(
        &app,
        "INSERT DATA { <http://ex/a> <http://ex/p> <http://ex/b> . <http://ex/b> <http://ex/p> <http://ex/c> }",
    )
    .await;
    assert_eq!(
        select_pairs(&app).await,
        BTreeSet::from([
            pair("http://ex/a", "http://ex/b"),
            pair("http://ex/b", "http://ex/c"),
            pair("http://ex/a", "http://ex/c"), // derived
        ])
    );
    let recs = drain(&audit);
    assert!(
        recs.iter()
            .any(|r| r.triple == a_p_c && r.mult == 1 && r.kind == DerivationKind::ClosureInferred),
        "feed did not publish the derivation: {recs:?}"
    );

    update(
        &app,
        "DELETE DATA { <http://ex/b> <http://ex/p> <http://ex/c> }",
    )
    .await;
    assert_eq!(
        select_pairs(&app).await,
        BTreeSet::from([pair("http://ex/a", "http://ex/b")]),
        "derived <a p c> must be withdrawn with its support"
    );
    let recs = drain(&audit);
    let net: i64 = recs
        .iter()
        .filter(|r| r.triple == a_p_c && r.kind != DerivationKind::Asserted)
        .map(|r| r.mult)
        .sum();
    assert!(net < 0, "feed did not publish the withdrawal: {recs:?}");
    assert_derived_graph_mirrors_circuit(&mut store.write());
}

// ── Crate-level paths ────────────────────────────────────────────────────────

/// The `add_plan` (bilinear rule) seam: `(x type c) ∧ (c sub d) → (x type d)`
/// registered over dictionary ids, derived on insert, withdrawn on delete.
#[test]
fn bilinear_rule_seam_derives_and_withdraws() {
    struct CaxSco {
        ty: u64,
        sub: u64,
    }
    impl BilinearRule for CaxSco {
        fn id(&self) -> RuleId {
            7
        }
        fn apply_full(&self, a: &Zset<TripleId>, b: &Zset<TripleId>) -> Zset<TripleId> {
            let mut out = Zset::new();
            for ((x, xp, c), ma) in a.iter().filter(|((_, p, _), _)| *p == self.ty) {
                for ((_, _, d), mb) in b.iter().filter(|((s, p, _), _)| *p == self.sub && s == c) {
                    out.add((*x, *xp, *d), ma * mb);
                }
            }
            out
        }
        fn apply_delta(
            &self,
            a: &Zset<TripleId>,
            b: &Zset<TripleId>,
            da: &Zset<TripleId>,
            db: &Zset<TripleId>,
        ) -> Zset<TripleId> {
            let mut out = self.apply_full(da, b);
            out.add_assign(&self.apply_full(a, db));
            out.add_assign(&self.apply_full(da, db));
            out
        }
        fn body_predicates(&self) -> [Option<u64>; 2] {
            [Some(self.ty), Some(self.sub)]
        }
    }
    let mut b = HornBackend::new();
    let rule = CaxSco {
        ty: id(&b, "http://ex/type"),
        sub: id(&b, "http://ex/sub"),
    };
    let mut plan = NaryPlan::new();
    plan.push_join(Box::new(rule));
    let mut c = Circuit::new();
    c.add_plan(plan, 7);
    b.attach_circuit(c).unwrap();

    let types = |b: &HornBackend| {
        let QueryAnswer::Solutions { rows, .. } =
            execute_query("SELECT ?c WHERE { <http://ex/x> <http://ex/type> ?c }", b).unwrap()
        else {
            panic!()
        };
        rows.into_iter()
            .map(|r| match r.get("c").cloned() {
                Some(Term::Iri(i)) => i,
                _ => panic!(),
            })
            .collect::<BTreeSet<_>>()
    };
    execute_update(
        "INSERT DATA { <http://ex/x> <http://ex/type> <http://ex/C> . <http://ex/C> <http://ex/sub> <http://ex/D> }",
        &mut b,
    )
    .unwrap();
    assert_eq!(
        types(&b),
        BTreeSet::from(["http://ex/C".to_owned(), "http://ex/D".to_owned()])
    );
    execute_update(
        "DELETE DATA { <http://ex/C> <http://ex/sub> <http://ex/D> }",
        &mut b,
    )
    .unwrap();
    assert_eq!(types(&b), BTreeSet::from(["http://ex/C".to_owned()]));
    assert_derived_graph_mirrors_circuit(&mut b);
}

/// A tick that publishes more records than the feed holds drops the engine's
/// subscription (`LagPolicy::DisconnectSlow`); the wiring resubscribes and
/// rebuilds the derived graph from the circuit, and later ticks keep working.
#[test]
fn feed_overflow_resubscribes_and_resyncs() {
    let mut b = transitive_backend(2);
    // 4 asserted + 6 derived records: far past capacity 2.
    execute_update(
        "INSERT DATA { <http://ex/a> <http://ex/p> <http://ex/b> . <http://ex/b> <http://ex/p> <http://ex/c> . \
         <http://ex/c> <http://ex/p> <http://ex/d> . <http://ex/d> <http://ex/p> <http://ex/e> }",
        &mut b,
    )
    .unwrap();
    assert_eq!(b.circuit_resyncs(), 1);
    assert_eq!(edges(&b).len(), 4 + 6);
    assert_derived_graph_mirrors_circuit(&mut b);

    // 1 asserted + 4 withdrawn records: overflows again, resyncs again.
    execute_update(
        "DELETE DATA { <http://ex/c> <http://ex/p> <http://ex/d> }",
        &mut b,
    )
    .unwrap();
    assert_eq!(b.circuit_resyncs(), 2);
    assert_eq!(
        edges(&b),
        BTreeSet::from([
            pair("http://ex/a", "http://ex/b"),
            pair("http://ex/b", "http://ex/c"),
            pair("http://ex/d", "http://ex/e"),
            pair("http://ex/a", "http://ex/c"),
        ])
    );
    assert_derived_graph_mirrors_circuit(&mut b);

    // A tick that fits (1 asserted + 1 derived) takes the ordinary drain path.
    execute_update(
        "INSERT DATA { <http://ex/e> <http://ex/p> <http://ex/f> }",
        &mut b,
    )
    .unwrap();
    assert_eq!(b.circuit_resyncs(), 2);
    assert!(edges(&b).contains(&pair("http://ex/d", "http://ex/f")));
    assert_derived_graph_mirrors_circuit(&mut b);
}

/// Rows loaded below the write funnel (bulk load) are asserted into the
/// circuit when it is attached, so rules see the full base.
#[test]
fn attach_after_bulk_load_seeds_the_base() {
    let mut b = HornBackend::new();
    b.insert_oxrdf(&ox("http://ex/a"), &ox(P), &ox("http://ex/b"))
        .unwrap();
    b.insert_oxrdf(&ox("http://ex/b"), &ox(P), &ox("http://ex/c"))
        .unwrap();
    let mut c = Circuit::new();
    c.add_closure_plan(Box::new(TransitiveClosureRule::new(id(&b, P))));
    b.attach_circuit(c).unwrap();
    assert!(edges(&b).contains(&pair("http://ex/a", "http://ex/c")));
    assert_eq!(b.circuit().unwrap().asserted_base().len(), 2);
    assert_derived_graph_mirrors_circuit(&mut b);
}

/// Storage no-ops (re-insert of a live row, delete of an absent one,
/// delete+insert of one row in a batch, named-graph writes) reach the circuit
/// as nothing, so its asserted base stays a set that matches the default graph.
#[test]
fn storage_no_ops_do_not_reach_the_circuit() {
    let mut b = transitive_backend(1 << 10);
    let a_p_b = triple(&b, "http://ex/a", P, "http://ex/b");
    execute_update(
        "INSERT DATA { <http://ex/a> <http://ex/p> <http://ex/b> }",
        &mut b,
    )
    .unwrap();
    execute_update(
        "INSERT DATA { <http://ex/a> <http://ex/p> <http://ex/b> }",
        &mut b,
    )
    .unwrap();
    execute_update(
        "DELETE DATA { <http://ex/z> <http://ex/p> <http://ex/z> }",
        &mut b,
    )
    .unwrap();
    execute_update(
        "DELETE { <http://ex/a> <http://ex/p> <http://ex/b> } INSERT { <http://ex/a> <http://ex/p> <http://ex/b> } WHERE {}",
        &mut b,
    )
    .unwrap();
    execute_update(
        "INSERT DATA { GRAPH <http://ex/g> { <http://ex/b> <http://ex/p> <http://ex/c> } }",
        &mut b,
    )
    .unwrap();
    let c = b.circuit().unwrap();
    assert_eq!(c.asserted_base().get(&a_p_b), 1);
    assert_eq!(
        c.asserted_base().len(),
        1,
        "named-graph write must not enter the circuit"
    );

    execute_update(
        "DELETE DATA { <http://ex/a> <http://ex/p> <http://ex/b> }",
        &mut b,
    )
    .unwrap();
    assert_eq!(b.circuit().unwrap().asserted_base().get(&a_p_b), 0);
    // Only the named-graph row is left (the union default graph shows it);
    // nothing derived, because <b p c> never entered the circuit.
    assert_eq!(
        edges(&b),
        BTreeSet::from([pair("http://ex/b", "http://ex/c")])
    );
}

/// `CLEAR DEFAULT` reaches the circuit: derived rows go with their support,
/// and a later insert derives nothing from the cleared triples.
#[test]
fn clear_default_withdraws_derived_and_does_not_resurrect() {
    let mut b = transitive_backend(1 << 10);
    execute_update(
        "INSERT DATA { <http://ex/a> <http://ex/p> <http://ex/b> . <http://ex/b> <http://ex/p> <http://ex/c> }",
        &mut b,
    )
    .unwrap();
    assert!(edges(&b).contains(&pair("http://ex/a", "http://ex/c")));
    execute_update("CLEAR DEFAULT", &mut b).unwrap();
    assert!(
        edges(&b).is_empty(),
        "derived rows must go with their support"
    );
    assert!(b.circuit().unwrap().asserted_base().is_empty());
    execute_update(
        "INSERT DATA { <http://ex/c> <http://ex/p> <http://ex/d> }",
        &mut b,
    )
    .unwrap();
    assert_eq!(
        edges(&b),
        BTreeSet::from([pair("http://ex/c", "http://ex/d")]),
        "cleared triples must not feed new derivations"
    );
    assert_derived_graph_mirrors_circuit(&mut b);
}

/// SPEC-29 D7: an update that derives something still reports its touched
/// graphs to the view router.
#[test]
fn derivation_producing_update_marks_touched_graphs() {
    let mut b = transitive_backend(1 << 10);
    execute_update(
        "INSERT DATA { <http://ex/a> <http://ex/p> <http://ex/b> . <http://ex/b> <http://ex/p> <http://ex/c> }",
        &mut b,
    )
    .unwrap();
    assert!(
        edges(&b).contains(&pair("http://ex/a", "http://ex/c")),
        "precondition: derived"
    );
    assert_eq!(b.take_touched_graphs(), vec![None]);
}

/// A store reopened from its WAL still holds the derived rows the previous
/// circuit mirrored; attaching a circuit that derives less must remove them.
#[test]
fn reopen_from_wal_and_reattach_drops_stale_derived_rows() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut b = HornBackend::with_store(ColumnStore::open(dir.path()).unwrap());
        let mut c = Circuit::new();
        c.add_closure_plan(Box::new(TransitiveClosureRule::new(id(&b, P))));
        b.attach_circuit(c).unwrap();
        execute_update(
            "INSERT DATA { <http://ex/a> <http://ex/p> <http://ex/b> . <http://ex/b> <http://ex/p> <http://ex/c> }",
            &mut b,
        )
        .unwrap();
        assert!(edges(&b).contains(&pair("http://ex/a", "http://ex/c")));
    }
    let mut b = HornBackend::with_store(ColumnStore::open(dir.path()).unwrap());
    b.attach_circuit(Circuit::new()).unwrap(); // no rules: derives nothing
    assert_eq!(
        edges(&b),
        BTreeSet::from([
            pair("http://ex/a", "http://ex/b"),
            pair("http://ex/b", "http://ex/c")
        ]),
        "stale derived row survived the reattach"
    );
    assert!(b.circuit().unwrap().derived_base().is_empty());
    assert_derived_graph_mirrors_circuit(&mut b);
}
