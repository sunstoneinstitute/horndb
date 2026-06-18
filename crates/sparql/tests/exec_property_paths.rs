//! End-to-end coverage for the non-recursive property-path operators
//! added under SPEC-07 #49: alternative `|`, zero-or-one `?`, and the
//! negated property set `!`, plus their composition with `/` and `^`.

use horndb_sparql::algebra::translate::translate_query;
use horndb_sparql::algebra::Term;
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::exec::runtime::Runtime;
use horndb_sparql::exec::{Bindings, Store};
use horndb_sparql::parser::{parse_query, ParsedQuery};
use horndb_sparql::plan::planner;

fn iri(s: &str) -> Term {
    Term::Iri(s.into())
}

/// A small social graph:
///   alice -knows-> bob
///   alice -likes-> carol
///   bob   -knows-> dave
///   bob   -admires-> alice
fn make_store() -> MemStore {
    let mut s = MemStore::default();
    let edges = [
        ("alice", "knows", "bob"),
        ("alice", "likes", "carol"),
        ("bob", "knows", "dave"),
        ("bob", "admires", "alice"),
    ];
    for (su, p, o) in edges {
        s.insert_triple(
            iri(&format!("http://ex/{su}")),
            iri(&format!("http://ex/{p}")),
            iri(&format!("http://ex/{o}")),
        );
    }
    s
}

fn run(q: &str, store: &MemStore) -> Vec<Bindings> {
    let inner = match parse_query(q).unwrap() {
        ParsedQuery::Select { inner }
        | ParsedQuery::Ask { inner }
        | ParsedQuery::Construct { inner } => inner,
        ParsedQuery::Describe { .. } => panic!("describe"),
        ParsedQuery::Explain { .. } => panic!("explain"),
    };
    let alg = translate_query(&inner).unwrap();
    let plan = planner::plan(&alg).unwrap();
    Runtime::new(store).run(&plan).unwrap().collect()
}

fn run_err(q: &str) -> String {
    let inner = match parse_query(q).unwrap() {
        ParsedQuery::Select { inner }
        | ParsedQuery::Ask { inner }
        | ParsedQuery::Construct { inner } => inner,
        ParsedQuery::Describe { .. } => panic!("describe"),
        ParsedQuery::Explain { .. } => panic!("explain"),
    };
    translate_query(&inner).unwrap_err().to_string()
}

/// Collect the IRI suffix bound to `var` across all rows, sorted+deduped.
fn names(rows: &[Bindings], var: &str) -> Vec<String> {
    let mut v: Vec<String> = rows
        .iter()
        .filter_map(|b| b.get(var))
        .map(|t| match t {
            Term::Iri(s) => s.rsplit('/').next().unwrap().to_owned(),
            other => panic!("expected IRI, got {other:?}"),
        })
        .collect();
    v.sort();
    v.dedup();
    v
}

// ---- Alternative `|` -------------------------------------------------

#[test]
fn alternative_unions_both_predicates() {
    let s = make_store();
    let rows = run(
        "SELECT ?o WHERE { <http://ex/alice> (<http://ex/knows>|<http://ex/likes>) ?o }",
        &s,
    );
    assert_eq!(names(&rows, "o"), vec!["bob", "carol"]);
}

#[test]
fn alternative_is_distinct_from_join() {
    // alice has both knows->bob and likes->carol; a `|` must NOT require
    // both to hold simultaneously (which would yield zero rows).
    let s = make_store();
    let rows = run(
        "SELECT ?o WHERE { ?s (<http://ex/knows>|<http://ex/admires>) ?o }",
        &s,
    );
    // knows: bob, dave ; admires: alice
    assert_eq!(names(&rows, "o"), vec!["alice", "bob", "dave"]);
}

// ---- Zero-or-one `?` -------------------------------------------------

#[test]
fn optional_subject_bound_binds_self_and_step() {
    let s = make_store();
    let rows = run(
        "SELECT ?o WHERE { <http://ex/alice> <http://ex/knows>? ?o }",
        &s,
    );
    // zero-length: alice ; one step: bob
    assert_eq!(names(&rows, "o"), vec!["alice", "bob"]);
}

#[test]
fn optional_object_bound_binds_self_and_reverse_step() {
    let s = make_store();
    let rows = run(
        "SELECT ?ss WHERE { ?ss <http://ex/knows>? <http://ex/bob> }",
        &s,
    );
    // zero-length: bob ; one step (something knows bob): alice
    assert_eq!(names(&rows, "ss"), vec!["alice", "bob"]);
}

#[test]
fn optional_both_ground_equal_matches() {
    let s = make_store();
    // No alice-knows-alice edge, but zero-length still matches on equality.
    let rows = run(
        "ASK { <http://ex/alice> <http://ex/knows>? <http://ex/alice> }",
        &s,
    );
    assert_eq!(rows.len(), 1, "zero-length self-match expected");
}

#[test]
fn optional_both_ground_present_edge_matches_once_or_twice() {
    let s = make_store();
    // alice knows bob, and they differ, so only the one-step branch fires.
    let rows = run(
        "ASK { <http://ex/alice> <http://ex/knows>? <http://ex/bob> }",
        &s,
    );
    assert!(!rows.is_empty(), "one-step branch should match");
}

#[test]
fn optional_distinct_unbound_vars_rejected() {
    // `?s p? ?o` with two distinct unbound endpoints would have to range
    // over every node; we reject it rather than return wrong answers.
    let msg = run_err("SELECT ?s ?o WHERE { ?s <http://ex/knows>? ?o }");
    assert!(msg.contains("property-path"), "got: {msg}");
}

#[test]
fn optional_same_unbound_var_both_ends_rejected() {
    // `?x p? ?x` would bind ?x to every node via the zero-length branch;
    // emitting the unit (unbound-?x) row would be wrong, so we reject it.
    let msg = run_err("SELECT ?x WHERE { ?x <http://ex/knows>? ?x }");
    assert!(msg.contains("property-path"), "got: {msg}");
}

// ---- Negated property set `!` ----------------------------------------

#[test]
fn negated_set_excludes_listed_predicate() {
    let s = make_store();
    // From alice, everything except `knows`: only likes->carol.
    let rows = run(
        "SELECT ?o WHERE { <http://ex/alice> !(<http://ex/knows>) ?o }",
        &s,
    );
    assert_eq!(names(&rows, "o"), vec!["carol"]);
}

#[test]
fn negated_set_with_multiple_predicates() {
    let s = make_store();
    // From bob, exclude both knows and admires -> nothing left.
    let rows = run(
        "SELECT ?o WHERE { <http://ex/bob> !(<http://ex/knows>|<http://ex/admires>) ?o }",
        &s,
    );
    assert!(rows.is_empty(), "all bob edges excluded, got {rows:?}");
}

#[test]
fn negated_set_inverse_member_kept_edge() {
    let s = make_store();
    // `!(^:knows)` parses as Reverse(NegatedPropertySet([knows])). Between
    // alice (subject) and ?bb it lowers — via the Reverse swap — to
    // `?bb ?p alice` with ?p != knows: incoming edges to alice other than
    // knows. bob admires alice (kept) and nobody knows alice -> ?bb = bob.
    let rows = run(
        "SELECT ?bb WHERE { <http://ex/alice> !(^<http://ex/knows>) ?bb }",
        &s,
    );
    assert_eq!(names(&rows, "bb"), vec!["bob"]);
}

#[test]
fn negated_set_inverse_member_excluded_edge() {
    let s = make_store();
    // Same shape against bob: the only incoming edge to bob is
    // alice-knows-bob, which `^knows` excludes -> no rows.
    let rows = run(
        "SELECT ?bb WHERE { <http://ex/bob> !(^<http://ex/knows>) ?bb }",
        &s,
    );
    assert!(rows.is_empty(), "got {rows:?}");
}

#[test]
fn two_negated_sets_do_not_share_hidden_predicate_var() {
    // Regression for the reused-hidden-var bug: two `!` patterns in one
    // query must mint distinct hidden predicate variables, or the join
    // forces their (unrelated) matched predicates to be equal and drops
    // rows. Here alice's non-knows edge is `likes`, bob's non-knows edge
    // is `admires` — different predicates, so a shared hidden var would
    // yield zero rows. Correct answer binds both.
    let s = make_store();
    let rows = run(
        "SELECT ?a ?b WHERE { \
           <http://ex/alice> !(<http://ex/knows>) ?a . \
           <http://ex/bob>   !(<http://ex/knows>) ?b }",
        &s,
    );
    assert_eq!(names(&rows, "a"), vec!["carol"]);
    assert_eq!(names(&rows, "b"), vec!["alice"]);
}

#[test]
fn blank_node_endpoint_joins_with_enclosing_pattern() {
    // A query blank node shared between a path and an ordinary triple must
    // keep co-referring: `_:b (knows|likes) ?o . _:b admires ?x`. Only bob
    // both has an outgoing knows/likes AND an admires edge, so `_:b` = bob,
    // ?o ∈ {dave} (bob knows dave), ?x = alice (bob admires alice). The
    // path's set-valued projection must NOT drop the `_:b` join variable,
    // or this collapses into a Cartesian product / false positives.
    let s = make_store();
    let rows = run(
        "SELECT ?o ?x WHERE { \
           _:b (<http://ex/knows>|<http://ex/likes>) ?o . \
           _:b <http://ex/admires> ?x }",
        &s,
    );
    // bob knows dave (likes: none from bob); bob admires alice.
    assert_eq!(names(&rows, "o"), vec!["dave"]);
    assert_eq!(names(&rows, "x"), vec!["alice"]);
    assert_eq!(rows.len(), 1, "no Cartesian blow-up, got {rows:?}");
}

// ---- Composition -----------------------------------------------------

#[test]
fn alternative_composes_with_sequence() {
    let s = make_store();
    // alice -(knows|likes)-> ?mid -knows-> ?o
    //   alice knows bob -knows-> dave
    //   alice likes carol -knows-> (none)
    let rows = run(
        "SELECT ?o WHERE { <http://ex/alice> (<http://ex/knows>|<http://ex/likes>)/<http://ex/knows> ?o }",
        &s,
    );
    assert_eq!(names(&rows, "o"), vec!["dave"]);
}

#[test]
fn inverse_of_alternative() {
    let s = make_store();
    // `^(knows|admires)` between ?ss and alice == `(knows|admires)` between
    // alice and ?ss. alice knows bob; alice admires nobody -> ?ss = bob.
    let rows = run(
        "SELECT ?ss WHERE { ?ss ^(<http://ex/knows>|<http://ex/admires>) <http://ex/alice> }",
        &s,
    );
    assert_eq!(names(&rows, "ss"), vec!["bob"]);
}

#[test]
fn plain_sequence_joins_on_minted_blank_node() {
    // Regression: spargebra flattens `p1/p2` into two patterns joined by
    // a minted blank node. That node must behave as a join variable, or
    // the sequence yields nothing. alice -knows-> bob -knows-> dave.
    let s = make_store();
    let rows = run(
        "SELECT ?o WHERE { <http://ex/alice> <http://ex/knows>/<http://ex/knows> ?o }",
        &s,
    );
    assert_eq!(names(&rows, "o"), vec!["dave"]);
}

#[test]
fn inverse_sequence() {
    // `^knows/admires` from dave: dave <-knows- bob -admires-> alice.
    let s = make_store();
    let rows = run(
        "SELECT ?o WHERE { <http://ex/dave> ^<http://ex/knows>/<http://ex/admires> ?o }",
        &s,
    );
    assert_eq!(names(&rows, "o"), vec!["alice"]);
}

// ---- Recursive Kleene paths `+` / `*` (SPEC-07 #50) ------------------
//
// The base graph's `knows` relation is the acyclic chain
//   alice -knows-> bob -knows-> dave.

#[test]
fn plus_forward_from_ground() {
    // `alice knows+ ?x`: one-or-more knows-steps from alice → {bob, dave}.
    let s = make_store();
    let rows = run(
        "SELECT ?x WHERE { <http://ex/alice> <http://ex/knows>+ ?x }",
        &s,
    );
    assert_eq!(names(&rows, "x"), vec!["bob", "dave"]);
}

#[test]
fn plus_backward_to_ground() {
    // `?x knows+ dave`: nodes that reach dave via knows+ → {alice, bob}.
    let s = make_store();
    let rows = run(
        "SELECT ?x WHERE { ?x <http://ex/knows>+ <http://ex/dave> }",
        &s,
    );
    assert_eq!(names(&rows, "x"), vec!["alice", "bob"]);
}

#[test]
fn plus_both_variables_enumerates_all_reachable_pairs() {
    // `?x knows+ ?y`: alice→bob, alice→dave, bob→dave.
    let s = make_store();
    let rows = run("SELECT ?x ?y WHERE { ?x <http://ex/knows>+ ?y }", &s);
    let mut pairs: Vec<(String, String)> = rows
        .iter()
        .map(|b| {
            let suf = |t: &Term| match t {
                Term::Iri(s) => s.rsplit('/').next().unwrap().to_owned(),
                other => panic!("expected IRI, got {other:?}"),
            };
            (suf(b.get("x").unwrap()), suf(b.get("y").unwrap()))
        })
        .collect();
    pairs.sort();
    assert_eq!(
        pairs,
        vec![
            ("alice".to_owned(), "bob".to_owned()),
            ("alice".to_owned(), "dave".to_owned()),
            ("bob".to_owned(), "dave".to_owned()),
        ]
    );
}

#[test]
fn plus_ground_both_endpoints_is_existence_test() {
    // `alice knows+ dave` is reachable → exactly one (empty) solution.
    let s = make_store();
    let rows = run(
        "SELECT * WHERE { <http://ex/alice> <http://ex/knows>+ <http://ex/dave> }",
        &s,
    );
    assert_eq!(rows.len(), 1, "reachable existence test, got {rows:?}");
    // Not reachable in the other direction.
    let none = run(
        "SELECT * WHERE { <http://ex/dave> <http://ex/knows>+ <http://ex/alice> }",
        &s,
    );
    assert!(none.is_empty(), "dave does not reach alice, got {none:?}");
}

#[test]
fn star_adds_reflexive_pairs() {
    // `alice knows* ?x`: zero-or-more → {alice (reflexive), bob, dave}.
    let s = make_store();
    let rows = run(
        "SELECT ?x WHERE { <http://ex/alice> <http://ex/knows>* ?x }",
        &s,
    );
    assert_eq!(names(&rows, "x"), vec!["alice", "bob", "dave"]);
}

#[test]
fn star_reflexive_at_leaf() {
    // `dave knows* ?x`: dave has no outgoing knows → only the reflexive
    // {dave}.
    let s = make_store();
    let rows = run(
        "SELECT ?x WHERE { <http://ex/dave> <http://ex/knows>* ?x }",
        &s,
    );
    assert_eq!(names(&rows, "x"), vec!["dave"]);
}

#[test]
fn plus_terminates_on_a_cycle() {
    // Add bob -knows-> alice, creating the cycle alice→bob→alice plus
    // bob→dave. `alice knows+ ?x` must terminate and reach everyone,
    // including alice herself (alice→bob→alice).
    let mut s = make_store();
    s.insert_triple(
        iri("http://ex/bob"),
        iri("http://ex/knows"),
        iri("http://ex/alice"),
    );
    let rows = run(
        "SELECT ?x WHERE { <http://ex/alice> <http://ex/knows>+ ?x }",
        &s,
    );
    assert_eq!(names(&rows, "x"), vec!["alice", "bob", "dave"]);
}

#[test]
fn plus_over_alternative_inner_path() {
    // `(knows|admires)+` from alice. Base graph: alice-knows->bob,
    // bob-knows->dave, bob-admires->alice. From alice the reachable set
    // under either predicate is {bob, dave, alice}.
    let s = make_store();
    let rows = run(
        "SELECT ?x WHERE { <http://ex/alice> (<http://ex/knows>|<http://ex/admires>)+ ?x }",
        &s,
    );
    assert_eq!(names(&rows, "x"), vec!["alice", "bob", "dave"]);
}

#[test]
fn plus_over_inverse_inner_path() {
    // `^knows+` from dave: invert knows then close. dave <-knows- bob
    // <-knows- alice, so dave reaches {bob, alice}.
    let s = make_store();
    let rows = run(
        "SELECT ?x WHERE { <http://ex/dave> ^<http://ex/knows>+ ?x }",
        &s,
    );
    assert_eq!(names(&rows, "x"), vec!["alice", "bob"]);
}

#[test]
fn star_over_subclassof_chain_matches_acceptance_shape() {
    // SPEC-07 acceptance #7 shape: `?x rdfs:subClassOf* :Person`.
    // Cat ⊑ Mammal ⊑ Animal; Person ⊑ Animal. `?x subClassOf* Animal`
    // returns Animal (reflexive), Mammal, Cat, Person.
    let mut s = MemStore::default();
    let sco = "http://www.w3.org/2000/01/rdf-schema#subClassOf";
    for (sub, sup) in [
        ("Cat", "Mammal"),
        ("Mammal", "Animal"),
        ("Person", "Animal"),
    ] {
        s.insert_triple(
            iri(&format!("http://ex/{sub}")),
            iri(sco),
            iri(&format!("http://ex/{sup}")),
        );
    }
    let rows = run(
        "SELECT ?x WHERE { ?x <http://www.w3.org/2000/01/rdf-schema#subClassOf>* <http://ex/Animal> }",
        &s,
    );
    assert_eq!(names(&rows, "x"), vec!["Animal", "Cat", "Mammal", "Person"]);
}

// ---- Nested non-recursive paths (reach the in-crate Sequence arm) ----

#[test]
fn nested_optional_over_sequence() {
    // `(knows/knows)?` from alice: zero-length (alice) ∪ two-knows (dave).
    // This `Sequence` is nested under `?`, so spargebra does NOT pre-flatten
    // it — it reaches translate_path's Sequence arm, which must mint a valid
    // internal join variable.
    let s = make_store();
    let rows = run(
        "SELECT ?o WHERE { <http://ex/alice> (<http://ex/knows>/<http://ex/knows>)? ?o }",
        &s,
    );
    assert_eq!(names(&rows, "o"), vec!["alice", "dave"]);
}

#[test]
fn alternative_with_nested_sequence_branch() {
    // `knows | (knows/knows)` from alice: bob (one step) ∪ dave (two steps).
    let s = make_store();
    let rows = run(
        "SELECT ?o WHERE { <http://ex/alice> (<http://ex/knows>|(<http://ex/knows>/<http://ex/knows>)) ?o }",
        &s,
    );
    assert_eq!(names(&rows, "o"), vec!["bob", "dave"]);
}

// ---- Set-valued semantics: a single path matches each pair once --------

#[test]
fn alternative_dedups_shared_endpoint() {
    // Two predicates connect alice→eve; `(p1|p2)` must yield eve ONCE.
    let mut s = make_store();
    s.insert_triple(
        iri("http://ex/alice"),
        iri("http://ex/p1"),
        iri("http://ex/eve"),
    );
    s.insert_triple(
        iri("http://ex/alice"),
        iri("http://ex/p2"),
        iri("http://ex/eve"),
    );
    let rows = run(
        "SELECT ?o WHERE { <http://ex/alice> (<http://ex/p1>|<http://ex/p2>) <http://ex/eve> }",
        &s,
    );
    assert_eq!(rows.len(), 1, "shared endpoint must dedup, got {rows:?}");
}

#[test]
fn negated_set_dedups_shared_endpoint() {
    // alice→eve via two non-excluded predicates; `!(knows)` yields eve ONCE.
    let mut s = make_store();
    s.insert_triple(
        iri("http://ex/alice"),
        iri("http://ex/p1"),
        iri("http://ex/eve"),
    );
    s.insert_triple(
        iri("http://ex/alice"),
        iri("http://ex/p2"),
        iri("http://ex/eve"),
    );
    let rows = run(
        "SELECT ?o WHERE { <http://ex/alice> !(<http://ex/knows>) <http://ex/eve> }",
        &s,
    );
    assert_eq!(rows.len(), 1, "shared endpoint must dedup, got {rows:?}");
}

#[test]
fn optional_dedups_zero_and_one_overlap() {
    // Self-loop alice-knows-alice: `knows?` zero-length and one-step both
    // bind alice; the set-valued path returns alice exactly once.
    let mut s = make_store();
    s.insert_triple(
        iri("http://ex/alice"),
        iri("http://ex/knows"),
        iri("http://ex/alice"),
    );
    let rows = run(
        "SELECT ?o WHERE { <http://ex/alice> <http://ex/knows>? ?o }",
        &s,
    );
    // alice (zero ∪ one, deduped) + bob (one step from the base graph).
    assert_eq!(names(&rows, "o"), vec!["alice", "bob"]);
    assert_eq!(rows.len(), 2, "alice must not appear twice, got {rows:?}");
}

#[test]
fn user_var_named_like_old_hidden_var_does_not_collide() {
    // Hidden path/blank-node variables are user-unspellable now. A user
    // variable spelled exactly like a former hidden name (`__path_seq_0`)
    // alongside a sequence path must keep its own, independent binding —
    // the sequence join must not force it equal to the hidden mid node.
    // alice -knows-> bob -knows-> dave, and ?__path_seq_0 binds the open
    // `?z` triple independently (alice's two outgoing objects).
    let s = make_store();
    let rows = run(
        "SELECT ?o ?__path_seq_0 WHERE { \
           <http://ex/alice> <http://ex/knows>/<http://ex/knows> ?o . \
           <http://ex/alice> <http://ex/likes> ?__path_seq_0 }",
        &s,
    );
    // The sequence yields dave; the user var binds carol (alice likes carol).
    assert_eq!(names(&rows, "o"), vec!["dave"]);
    assert_eq!(names(&rows, "__path_seq_0"), vec!["carol"]);
}
