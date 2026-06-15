//! List-walking OWL 2 RL rules — `prp-spo2`, `prp-key`, `cls-int1`,
//! `cls-uni`, `cax-adc`, `eq-diff2`/`eq-diff3`.
//!
//! These rules can't be expressed in the `rules.toml` schema because each
//! axiom declaration determines a *different* rule arity (the chain length,
//! key length, intersection arity, ... depends on the ontology). So the
//! codegen pipeline in `crates/owlrl/codegen` only handles fixed-shape
//! rules; the list-axiom rules live here as hand-written Rust.
//!
//! Per SPEC-04 F2 ("no runtime rule interpreter") the layout still
//! satisfies the AOT discipline: list resolution is a one-shot, load-time
//! computation ([`resolve`]) that builds a [`SchemaAxioms`] holding pre-walked
//! `Vec<TermId>` chains. The hot path inside each `fire_*` is a hand-written
//! nested loop — no dynamic dispatch on rule shape.
//!
//! Per SPEC-04 F3 (semi-naïve evaluation) and F5 (no serial scans over
//! `rdf:type`) each rule advertises the predicate-IDs its body reads
//! through [`SchemaAxioms::body_predicates`] and skips when none of those
//! are dirty. `cls-int1` / `cls-uni` iterate the resolved chain per-`c_i`
//! via `store.probe(None, rdf_type, Some(c_i))` rather than scanning the
//! entire `rdf:type` partition.
//!
//! Per RDFox's ISWC 2015 paper this is also how production engines handle
//! list-shaped rules; the engine internalises the ontology lists at load
//! time and applies the resulting tabular rules in the same semi-naïve
//! loop as the static ones.
//!
//! ## Future work
//!
//! - Stage-2 SPEC-02 will give the store a `(p, o) → s` index. Today,
//!   `store.probe(None, p, Some(o))` filters the full `(p, *)` partition,
//!   which is the canonical `rdf:type`-skew failure mode (SPEC-04 F5).
//! - List axioms are computed once per `Engine::load` (Stage-1 is
//!   insertion-only — SPEC-06). A Stage-2 delta-aware refresh is a
//!   separate follow-up.

use rustc_hash::FxHashSet;
use smallvec::smallvec;

use crate::delta::Delta;
use crate::provenance::Provenance;
use crate::store::TripleStore;
use crate::types::{MaxCardRestriction, TermId, Triple};
use crate::vocab::Vocabulary;

/// All list-axiom shapes resolved at load time. One `Vec` per W3C rule kind.
#[derive(Debug, Default, Clone)]
pub struct SchemaAxioms {
    /// `(p, [p1, p2, ..., pn])` — one entry per `?p owl:propertyChainAxiom ?list`.
    pub property_chains: Vec<(TermId, Vec<TermId>)>,
    /// `(c, [p1, ..., pn])` — one entry per `?c owl:hasKey ?list`.
    pub keys: Vec<(TermId, Vec<TermId>)>,
    /// `(c, [c1, ..., cn])` — one entry per `?c owl:intersectionOf ?list`.
    pub intersections: Vec<(TermId, Vec<TermId>)>,
    /// `(c, [c1, ..., cn])` — one entry per `?c owl:unionOf ?list`.
    pub unions: Vec<(TermId, Vec<TermId>)>,
    /// `[c1, ..., cn]` — one entry per `?adc rdf:type owl:AllDisjointClasses`
    /// with an `owl:members` list.
    pub all_disjoint_classes: Vec<Vec<TermId>>,
    /// `[x1, ..., xn]` — one entry per `?ad rdf:type owl:AllDifferent` with
    /// an `owl:members` or `owl:distinctMembers` list.
    pub all_different: Vec<Vec<TermId>>,
    /// Resolved unqualified max-cardinality restrictions (`cls-maxc1`/`cls-maxc2`).
    /// Resolved at load time (`integration.rs`) and carried on the store; copied
    /// here by `resolve` so they ride the same semi-naïve dirty-prune path.
    pub max_card_restrictions: Vec<MaxCardRestriction>,
}

impl SchemaAxioms {
    /// Union of every body predicate every list rule reads. Used by the
    /// semi-naïve driver's dirty-predicate prune.
    pub fn body_predicates(&self, vocab: &Vocabulary) -> FxHashSet<TermId> {
        let mut s: FxHashSet<TermId> = FxHashSet::default();
        // prp-spo2: each p_i in every chain plus the head predicate
        // (a self-chain `(p, p) ⇒ p` is essentially transitivity and the
        // second round must still see p as dirty).
        for (head, chain) in &self.property_chains {
            s.insert(*head);
            for p in chain {
                s.insert(*p);
            }
        }
        // prp-key reads each p_i and rdf:type.
        if !self.keys.is_empty() {
            s.insert(vocab.rdf_type);
            for (_c, ps) in &self.keys {
                for p in ps {
                    s.insert(*p);
                }
            }
        }
        // cls-int1, cls-uni, cax-adc all read rdf:type.
        if !self.intersections.is_empty()
            || !self.unions.is_empty()
            || !self.all_disjoint_classes.is_empty()
        {
            s.insert(vocab.rdf_type);
        }
        // eq-diff2/3 emits differentFrom; downstream eq-diff1 (compiled)
        // chains off that + owl:sameAs.
        if !self.all_different.is_empty() {
            s.insert(vocab.owl_same_as);
            s.insert(vocab.owl_different_from);
        }
        // cls-maxc1/cls-maxc2 read rdf:type (for ?u : ?x) and each restricted
        // property (for ?u ?p ?y). Re-fire whenever either becomes dirty.
        if !self.max_card_restrictions.is_empty() {
            s.insert(vocab.rdf_type);
            for r in &self.max_card_restrictions {
                s.insert(r.property);
            }
        }
        s
    }

    pub fn is_empty(&self) -> bool {
        self.property_chains.is_empty()
            && self.keys.is_empty()
            && self.intersections.is_empty()
            && self.unions.is_empty()
            && self.all_disjoint_classes.is_empty()
            && self.all_different.is_empty()
            && self.max_card_restrictions.is_empty()
    }
}

/// Walk every list-declaring schema partition and resolve each `rdf:List`
/// head into a `Vec<TermId>`. One-shot per `Engine::load` — Stage-1 is
/// insertion-only (SPEC-06) so the resolved chains do not change across
/// semi-naïve rounds.
pub fn resolve(store: &dyn TripleStore, vocab: &Vocabulary) -> SchemaAxioms {
    let mut out = SchemaAxioms::default();

    // ?p owl:propertyChainAxiom ?head
    for t in store.scan_predicate(vocab.owl_property_chain_axiom) {
        if let Some(chain) = walk_list(store, vocab, t.o) {
            if !chain.is_empty() {
                out.property_chains.push((t.s, chain));
            }
        }
    }

    // ?c owl:hasKey ?head
    for t in store.scan_predicate(vocab.owl_has_key) {
        if let Some(ps) = walk_list(store, vocab, t.o) {
            if !ps.is_empty() {
                out.keys.push((t.s, ps));
            }
        }
    }

    // ?c owl:intersectionOf ?head
    for t in store.scan_predicate(vocab.owl_intersection_of) {
        if let Some(cs) = walk_list(store, vocab, t.o) {
            if !cs.is_empty() {
                out.intersections.push((t.s, cs));
            }
        }
    }

    // ?c owl:unionOf ?head
    for t in store.scan_predicate(vocab.owl_union_of) {
        if let Some(cs) = walk_list(store, vocab, t.o) {
            if !cs.is_empty() {
                out.unions.push((t.s, cs));
            }
        }
    }

    // ?adc rdf:type owl:AllDisjointClasses + ?adc owl:members ?head.
    {
        let adcs: Vec<TermId> = store
            .probe(None, vocab.rdf_type, Some(vocab.owl_all_disjoint_classes))
            .map(|t| t.s)
            .collect();
        for adc in adcs {
            let head = first_object(store, adc, vocab.owl_members)
                .or_else(|| first_object(store, adc, vocab.owl_distinct_members));
            if let Some(head) = head {
                if let Some(cs) = walk_list(store, vocab, head) {
                    if cs.len() >= 2 {
                        out.all_disjoint_classes.push(cs);
                    }
                }
            }
        }
    }

    // ?ad rdf:type owl:AllDifferent + ?ad owl:distinctMembers ?head (or
    // owl:members; both spellings appear in W3C test data).
    {
        let ads: Vec<TermId> = store
            .probe(None, vocab.rdf_type, Some(vocab.owl_all_different))
            .map(|t| t.s)
            .collect();
        for ad in ads {
            let head = first_object(store, ad, vocab.owl_distinct_members)
                .or_else(|| first_object(store, ad, vocab.owl_members));
            if let Some(head) = head {
                if let Some(xs) = walk_list(store, vocab, head) {
                    if xs.len() >= 2 {
                        out.all_different.push(xs);
                    }
                }
            }
        }
    }

    // Max-cardinality restrictions are classified at load time (integration.rs),
    // where the dictionary can parse the literal value; carry them through.
    out.max_card_restrictions = store.card_restrictions().to_vec();

    out
}

/// Walk an `rdf:first` / `rdf:rest` chain from `head` to `rdf:nil`. Returns
/// `None` on a malformed list (cycle, missing `rdf:first`, ...).
fn walk_list(store: &dyn TripleStore, vocab: &Vocabulary, head: TermId) -> Option<Vec<TermId>> {
    let mut out = Vec::new();
    let mut visited: FxHashSet<TermId> = FxHashSet::default();
    let mut cur = head;
    while cur != vocab.rdf_nil {
        if !visited.insert(cur) {
            // Cycle — malformed list.
            return None;
        }
        let first = first_object(store, cur, vocab.rdf_first)?;
        out.push(first);
        cur = first_object(store, cur, vocab.rdf_rest)?;
    }
    Some(out)
}

/// Return the first object of `(s, p, ?o)` in the store, or `None`.
fn first_object(store: &dyn TripleStore, s: TermId, p: TermId) -> Option<TermId> {
    store.probe(Some(s), p, None).next().map(|t| t.o)
}

/// Fire every list rule whose body predicates intersect `dirty` (or all of
/// them if `dirty` is `None`, signalling the first round).
pub fn fire_all(
    store: &dyn TripleStore,
    axioms: &SchemaAxioms,
    vocab: &Vocabulary,
    dirty: Option<&FxHashSet<TermId>>,
) -> Delta {
    let mut out = Delta::new();
    if axioms.is_empty() {
        return out;
    }

    // prp-spo2
    if !axioms.property_chains.is_empty() && any_dirty_for_chains(axioms, dirty) {
        for (head_pred, chain) in &axioms.property_chains {
            fire_prp_spo2(store, *head_pred, chain, &mut out);
        }
    }

    // prp-key — body reads each p_i and rdf:type for ?x and ?y.
    if !axioms.keys.is_empty() && any_dirty_for_keys(axioms, vocab, dirty) {
        for (c, ps) in &axioms.keys {
            fire_prp_key(store, vocab, *c, ps, &mut out);
        }
    }

    // cls-int1 — body reads rdf:type for each c_i.
    if !axioms.intersections.is_empty() && is_dirty(dirty, vocab.rdf_type) {
        for (c, cs) in &axioms.intersections {
            fire_cls_int1(store, vocab, *c, cs, &mut out);
        }
    }

    // scm-int — schema-only: each intersection class is a subclass of every
    // member of its list. Output is fully determined by the resolved schema
    // so we fire once, on the first round.
    if !axioms.intersections.is_empty() && dirty.is_none() {
        for (c, cs) in &axioms.intersections {
            fire_scm_int(store, vocab, *c, cs, &mut out);
        }
    }

    // cls-uni — body reads rdf:type for each c_i.
    if !axioms.unions.is_empty() && is_dirty(dirty, vocab.rdf_type) {
        for (c, cs) in &axioms.unions {
            fire_cls_uni(store, vocab, *c, cs, &mut out);
        }
    }

    // cax-adc — body reads rdf:type for each c_i.
    if !axioms.all_disjoint_classes.is_empty() && is_dirty(dirty, vocab.rdf_type) {
        for cs in &axioms.all_disjoint_classes {
            fire_cax_adc(store, vocab, cs, &mut out);
        }
    }

    // eq-diff2 / eq-diff3: assert pairwise differentFrom from the resolved
    // list. The differentFrom triples are schema-derived and constant
    // across rounds, so we fire on the first round only — subsequent
    // applies are no-ops via `store.contains`.
    if !axioms.all_different.is_empty() && dirty.is_none() {
        for xs in &axioms.all_different {
            fire_eq_diff_list(vocab, xs, &mut out);
        }
    }

    // cls-maxc1 — maxCardinality 0 restriction with any p-value ⇒ inconsistency.
    // cls-maxc2 — maxCardinality 1 restriction with two distinct p-values ⇒ sameAs.
    if !axioms.max_card_restrictions.is_empty() && is_dirty(dirty, vocab.rdf_type) {
        for r in &axioms.max_card_restrictions {
            match r.max {
                0 => fire_cls_maxc1(store, vocab, r.class, r.property, &mut out),
                1 => fire_cls_maxc2(store, vocab, r.class, r.property, &mut out),
                _ => {}
            }
        }
    } else if !axioms.max_card_restrictions.is_empty() {
        // rdf:type not dirty, but a restricted property might be.
        for r in &axioms.max_card_restrictions {
            if is_dirty(dirty, r.property) {
                match r.max {
                    0 => fire_cls_maxc1(store, vocab, r.class, r.property, &mut out),
                    1 => fire_cls_maxc2(store, vocab, r.class, r.property, &mut out),
                    _ => {}
                }
            }
        }
    }

    out
}

fn any_dirty_for_chains(axioms: &SchemaAxioms, dirty: Option<&FxHashSet<TermId>>) -> bool {
    let Some(dirty) = dirty else {
        return true;
    };
    for (head, chain) in &axioms.property_chains {
        if dirty.contains(head) {
            return true;
        }
        for p in chain {
            if dirty.contains(p) {
                return true;
            }
        }
    }
    false
}

fn any_dirty_for_keys(
    axioms: &SchemaAxioms,
    vocab: &Vocabulary,
    dirty: Option<&FxHashSet<TermId>>,
) -> bool {
    let Some(dirty) = dirty else {
        return true;
    };
    if dirty.contains(&vocab.rdf_type) {
        return true;
    }
    for (_c, ps) in &axioms.keys {
        for p in ps {
            if dirty.contains(p) {
                return true;
            }
        }
    }
    false
}

fn is_dirty(dirty: Option<&FxHashSet<TermId>>, p: TermId) -> bool {
    match dirty {
        None => true,
        Some(d) => d.contains(&p),
    }
}

// ---------------------------------------------------------------------------
// prp-spo2 — `?p owl:propertyChainAxiom (p1 ... pn) ∧ ?u0 p1 ?u1 ∧
// ?u1 p2 ?u2 ∧ ... ∧ ?u(n-1) pn ?un ⇒ ?u0 ?p ?un`
//
// Implementation: leading scan on `scan_predicate(p1)` to enumerate the
// `(?u0, ?u1)` frontier, then for each remaining `p_i` extend each
// `(u0, u_mid)` pair via `store.probe(u_mid, p_i, None)`. The chain is
// resolved at load time so this loop has no list-walking overhead.
// ---------------------------------------------------------------------------
fn fire_prp_spo2(store: &dyn TripleStore, head_pred: TermId, chain: &[TermId], out: &mut Delta) {
    debug_assert!(!chain.is_empty());

    let mut frontier: Vec<(TermId, TermId)> = Vec::new();
    for t in store.scan_predicate(chain[0]) {
        frontier.push((t.s, t.o));
    }
    if chain.len() == 1 {
        // Chain of length 1 is just sub-property propagation.
        for (u0, un) in frontier {
            emit_pair(out, "prp-spo2", u0, head_pred, un);
        }
        return;
    }
    let mut next: Vec<(TermId, TermId)> = Vec::new();
    for &p_i in &chain[1..] {
        next.clear();
        for &(u0, u_mid) in &frontier {
            for t in store.probe(Some(u_mid), p_i, None) {
                next.push((u0, t.o));
            }
        }
        std::mem::swap(&mut frontier, &mut next);
        if frontier.is_empty() {
            return;
        }
    }
    for (u0, un) in frontier {
        emit_pair(out, "prp-spo2", u0, head_pred, un);
    }
}

fn emit_pair(out: &mut Delta, rule_id: &'static str, s: TermId, p: TermId, o: TermId) {
    let head = Triple::new(s, p, o);
    if !out.contains(&head) {
        out.insert(
            head,
            Provenance {
                rule_id,
                // Premises are runtime-dependent on the resolved schema
                // list; recording the head suffices for Stage-1
                // provenance (full proof tree reconstruction is Stage-2
                // per SPEC-04 F4).
                premises: smallvec![],
            },
        );
    }
}

// ---------------------------------------------------------------------------
// prp-key — `?c owl:hasKey (p1 ... pn) ∧ ?x rdf:type ?c ∧ ?y rdf:type ?c ∧
// ?x pi ?zi ∧ ?y pi ?zi (for all i) ⇒ ?x owl:sameAs ?y`
//
// Stage-1 strategy: enumerate ?x : c and for each (?x, p1, z1) value find
// candidate ?y : c with (?y, p1, z1). Then filter candidates by matching
// every other p_i. Keys in the W3C suite are short (n=1) so the nested
// match is cheap.
// ---------------------------------------------------------------------------
fn fire_prp_key(
    store: &dyn TripleStore,
    vocab: &Vocabulary,
    c: TermId,
    ps: &[TermId],
    out: &mut Delta,
) {
    debug_assert!(!ps.is_empty());

    let xs: Vec<TermId> = store
        .probe(None, vocab.rdf_type, Some(c))
        .map(|t| t.s)
        .collect();
    if xs.len() < 2 {
        return;
    }
    for &x in &xs {
        for first_t in store.probe(Some(x), ps[0], None) {
            let z0 = first_t.o;
            let candidates: Vec<TermId> = store
                .probe(None, ps[0], Some(z0))
                .map(|t| t.s)
                .filter(|&y| y != x && store.contains(&Triple::new(y, vocab.rdf_type, c)))
                .collect();
            if candidates.is_empty() {
                continue;
            }
            let x_zs = cartesian_zs(store, x, &ps[1..]);
            if x_zs.is_empty() {
                continue;
            }
            for x_choice in &x_zs {
                let mut survivors = candidates.clone();
                for (i, &z_i) in x_choice.iter().enumerate() {
                    let p_i = ps[1 + i];
                    survivors.retain(|&y| store.contains(&Triple::new(y, p_i, z_i)));
                    if survivors.is_empty() {
                        break;
                    }
                }
                for y in survivors {
                    let head = Triple::new(x, vocab.owl_same_as, y);
                    if !out.contains(&head) && !store.contains(&head) {
                        out.insert(
                            head,
                            Provenance {
                                rule_id: "prp-key",
                                premises: smallvec![],
                            },
                        );
                    }
                }
            }
        }
    }
}

/// Cartesian product of `(x, p_i, ?z)` values for every `p_i` in `rest`.
/// Returns an empty Vec if any `p_i` has no value for `x`. Returns
/// `vec![Vec::new()]` when `rest` is empty (vacuous match).
fn cartesian_zs(store: &dyn TripleStore, x: TermId, rest: &[TermId]) -> Vec<Vec<TermId>> {
    if rest.is_empty() {
        return vec![Vec::new()];
    }
    let mut acc: Vec<Vec<TermId>> = vec![Vec::new()];
    for &p_i in rest {
        let values: Vec<TermId> = store.probe(Some(x), p_i, None).map(|t| t.o).collect();
        if values.is_empty() {
            return Vec::new();
        }
        let mut next = Vec::with_capacity(acc.len() * values.len());
        for prefix in &acc {
            for &v in &values {
                let mut row = prefix.clone();
                row.push(v);
                next.push(row);
            }
        }
        acc = next;
    }
    acc
}

// ---------------------------------------------------------------------------
// cls-int1 — `?c owl:intersectionOf (c1 ... cn) ∧ ?x rdf:type c1 ∧ ... ∧
// ?x rdf:type cn ⇒ ?x rdf:type ?c`
//
// Stage-1 seeds the iteration on `c1` and filters by the rest. Stage-2
// (SPEC-04 F5) should pre-sort by partition size.
// ---------------------------------------------------------------------------
fn fire_cls_int1(
    store: &dyn TripleStore,
    vocab: &Vocabulary,
    c: TermId,
    cs: &[TermId],
    out: &mut Delta,
) {
    debug_assert!(!cs.is_empty());
    let xs: Vec<TermId> = store
        .probe(None, vocab.rdf_type, Some(cs[0]))
        .map(|t| t.s)
        .collect();
    for x in xs {
        if cs[1..]
            .iter()
            .all(|&c_i| store.contains(&Triple::new(x, vocab.rdf_type, c_i)))
        {
            let head = Triple::new(x, vocab.rdf_type, c);
            if !out.contains(&head) && !store.contains(&head) {
                out.insert(
                    head,
                    Provenance {
                        rule_id: "cls-int1",
                        premises: smallvec![],
                    },
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// scm-int — `?c owl:intersectionOf (c1 ... cn) ⇒ ?c rdfs:subClassOf ci`
//
// Schema-derived: the resolved intersections are constant across semi-naïve
// rounds (Stage-1 insertion-only — SPEC-06), so we emit on the first round
// only. Downstream `cax-sco` / `scm-sco` then propagate to instance-level
// type triples, which is how the description-logic-1xx-incons cases close
// in combination with `cls-com`.
// ---------------------------------------------------------------------------
fn fire_scm_int(
    store: &dyn TripleStore,
    vocab: &Vocabulary,
    c: TermId,
    cs: &[TermId],
    out: &mut Delta,
) {
    for &ci in cs {
        let head = Triple::new(c, vocab.rdfs_sub_class_of, ci);
        if !out.contains(&head) && !store.contains(&head) {
            out.insert(
                head,
                Provenance {
                    rule_id: "scm-int",
                    premises: smallvec![],
                },
            );
        }
    }
}

// ---------------------------------------------------------------------------
// cls-uni — `?c owl:unionOf (c1 ... cn) ∧ ∃i. ?x rdf:type ci ⇒ ?x rdf:type ?c`
// ---------------------------------------------------------------------------
fn fire_cls_uni(
    store: &dyn TripleStore,
    vocab: &Vocabulary,
    c: TermId,
    cs: &[TermId],
    out: &mut Delta,
) {
    for &c_i in cs {
        for t in store.probe(None, vocab.rdf_type, Some(c_i)) {
            let head = Triple::new(t.s, vocab.rdf_type, c);
            if !out.contains(&head) && !store.contains(&head) {
                out.insert(
                    head,
                    Provenance {
                        rule_id: "cls-uni",
                        premises: smallvec![],
                    },
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// cax-adc — `_:adc rdf:type owl:AllDisjointClasses ∧ _:adc owl:members
// (c1 ... cn) ∧ ?x rdf:type ci ∧ ?x rdf:type cj (i ≠ j) ⇒ ?x rdf:type
// owl:Nothing`
//
// Implementation: enumerate every pair (i, j) with i < j; for each pair,
// reuse the cax-dw shape on the (c_i, c_j) sub-extents.
// ---------------------------------------------------------------------------
fn fire_cax_adc(store: &dyn TripleStore, vocab: &Vocabulary, cs: &[TermId], out: &mut Delta) {
    for i in 0..cs.len() {
        let xs_i: Vec<TermId> = store
            .probe(None, vocab.rdf_type, Some(cs[i]))
            .map(|t| t.s)
            .collect();
        if xs_i.is_empty() {
            continue;
        }
        for &cj in cs.iter().skip(i + 1) {
            for &x in &xs_i {
                if store.contains(&Triple::new(x, vocab.rdf_type, cj)) {
                    let head = Triple::new(x, vocab.rdf_type, vocab.owl_nothing);
                    if !out.contains(&head) && !store.contains(&head) {
                        out.insert(
                            head,
                            Provenance {
                                rule_id: "cax-adc",
                                premises: smallvec![],
                            },
                        );
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// cls-maxc1 — `?x owl:maxCardinality "0" ∧ ?x owl:onProperty ?p ∧
// ?u rdf:type ?x ∧ ?u ?p ?y ⇒ false`. Materialised, like every other OWL 2 RL
// `false`-rule in this engine, as `?u rdf:type owl:Nothing` so
// `Engine::is_consistent()` reports the inconsistency.
// ---------------------------------------------------------------------------
fn fire_cls_maxc1(
    store: &dyn TripleStore,
    vocab: &Vocabulary,
    class: TermId,
    property: TermId,
    out: &mut Delta,
) {
    let us: Vec<TermId> = store
        .probe(None, vocab.rdf_type, Some(class))
        .map(|t| t.s)
        .collect();
    for u in us {
        // Any single value on the restricted property violates max 0.
        if store.probe(Some(u), property, None).next().is_some() {
            let head = Triple::new(u, vocab.rdf_type, vocab.owl_nothing);
            if !out.contains(&head) && !store.contains(&head) {
                out.insert(
                    head,
                    Provenance {
                        rule_id: "cls-maxc1",
                        premises: smallvec![],
                    },
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// cls-maxc2 — `?x owl:maxCardinality "1" ∧ ?x owl:onProperty ?p ∧
// ?u rdf:type ?x ∧ ?u ?p ?y1 ∧ ?u ?p ?y2 ⇒ ?y1 owl:sameAs ?y2`.
//
// For each ?u typed as the restriction class, gather every value on the
// restricted property and emit `owl:sameAs` for each ordered distinct pair.
// `owl:sameAs` symmetry/transitivity is closed downstream by the closure
// backend (eq-sym/eq-trans), so emitting distinct pairs suffices; the
// reflexive case (y == y) is skipped as trivial.
// ---------------------------------------------------------------------------
fn fire_cls_maxc2(
    store: &dyn TripleStore,
    vocab: &Vocabulary,
    class: TermId,
    property: TermId,
    out: &mut Delta,
) {
    let us: Vec<TermId> = store
        .probe(None, vocab.rdf_type, Some(class))
        .map(|t| t.s)
        .collect();
    for u in us {
        let ys: Vec<TermId> = store.probe(Some(u), property, None).map(|t| t.o).collect();
        if ys.len() < 2 {
            continue;
        }
        for (i, &y1) in ys.iter().enumerate() {
            for &y2 in &ys[i + 1..] {
                if y1 == y2 {
                    continue;
                }
                for (a, b) in [(y1, y2), (y2, y1)] {
                    let head = Triple::new(a, vocab.owl_same_as, b);
                    if !out.contains(&head) && !store.contains(&head) {
                        out.insert(
                            head,
                            Provenance {
                                rule_id: "cls-maxc2",
                                premises: smallvec![],
                            },
                        );
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// eq-diff2 / eq-diff3 — `_:ad rdf:type owl:AllDifferent ∧
// _:ad owl:members|owl:distinctMembers (x1 ... xn) ⇒ ?xi owl:differentFrom ?xj`
// for every i ≠ j. The downstream `eq-diff1` (compiled) reports
// inconsistency if any ?xi owl:sameAs ?xj also holds.
// ---------------------------------------------------------------------------
fn fire_eq_diff_list(vocab: &Vocabulary, xs: &[TermId], out: &mut Delta) {
    for i in 0..xs.len() {
        for j in 0..xs.len() {
            if i == j {
                continue;
            }
            let head = Triple::new(xs[i], vocab.owl_different_from, xs[j]);
            if !out.contains(&head) {
                out.insert(
                    head,
                    Provenance {
                        rule_id: "eq-diff2",
                        premises: smallvec![],
                    },
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemStore;

    fn t(s: u64, p: u64, o: u64) -> Triple {
        Triple::new(TermId(s), TermId(p), TermId(o))
    }

    fn fresh() -> (MemStore, Vocabulary) {
        let v = Vocabulary::synthetic(10_000);
        (MemStore::new(v), v)
    }

    fn assert_list(s: &mut MemStore, v: &Vocabulary, head: u64, items: &[u64]) {
        // Caller passes a sequence of item IDs; each list cell's TermId is
        // computed as `head + idx`.
        for (idx, &item) in items.iter().enumerate() {
            let cell = head + idx as u64;
            s.assert(t(cell, v.rdf_first.0, item));
            let next = if idx + 1 == items.len() {
                v.rdf_nil.0
            } else {
                head + (idx as u64) + 1
            };
            s.assert(t(cell, v.rdf_rest.0, next));
        }
    }

    #[test]
    fn walk_list_chain() {
        let (mut s, v) = fresh();
        assert_list(&mut s, &v, 1000, &[42, 43, 44]);
        let got = walk_list(&s, &v, TermId(1000)).unwrap();
        assert_eq!(got, vec![TermId(42), TermId(43), TermId(44)]);
    }

    #[test]
    fn walk_list_empty_is_nil() {
        let (s, v) = fresh();
        let got = walk_list(&s, &v, v.rdf_nil).unwrap();
        assert!(got.is_empty());
    }

    #[test]
    fn resolve_property_chain() {
        let (mut s, v) = fresh();
        let p = TermId(50);
        let p1 = TermId(51);
        let p2 = TermId(52);
        s.assert(t(p.0, v.owl_property_chain_axiom.0, 1000));
        assert_list(&mut s, &v, 1000, &[p1.0, p2.0]);
        let ax = resolve(&s, &v);
        assert_eq!(ax.property_chains.len(), 1);
        assert_eq!(ax.property_chains[0].0, p);
        assert_eq!(ax.property_chains[0].1, vec![p1, p2]);
    }

    #[test]
    fn prp_spo2_two_step() {
        let (mut s, v) = fresh();
        let p = TermId(50);
        let p1 = TermId(51);
        let p2 = TermId(52);
        let a = TermId(100);
        let b = TermId(101);
        let c = TermId(102);
        s.assert(t(p.0, v.owl_property_chain_axiom.0, 1000));
        assert_list(&mut s, &v, 1000, &[p1.0, p2.0]);
        s.assert(t(a.0, p1.0, b.0));
        s.assert(t(b.0, p2.0, c.0));
        let ax = resolve(&s, &v);
        let d = fire_all(&s, &ax, &v, None);
        assert!(d.contains(&t(a.0, p.0, c.0)), "prp-spo2: a-p->c");
    }

    #[test]
    fn prp_spo2_self_chain_dirty_includes_head() {
        // chain2trans1 case: `(p, p) ⇒ p` synthesises transitivity. The
        // dirty-predicate prune must include the head predicate `p` so the
        // second semi-naïve round still fires after `p` itself becomes
        // dirty.
        let (mut s, v) = fresh();
        let p = TermId(50);
        s.assert(t(p.0, v.owl_property_chain_axiom.0, 1000));
        assert_list(&mut s, &v, 1000, &[p.0, p.0]);
        let ax = resolve(&s, &v);
        let body_preds = ax.body_predicates(&v);
        assert!(
            body_preds.contains(&p),
            "self-chain dirty set must contain the head predicate"
        );
    }

    #[test]
    fn prp_key_single() {
        let (mut s, v) = fresh();
        let c = TermId(50);
        let p = TermId(51);
        let x = TermId(100);
        let y = TermId(101);
        let z = TermId(200);
        s.assert(t(c.0, v.owl_has_key.0, 1000));
        assert_list(&mut s, &v, 1000, &[p.0]);
        s.assert(t(x.0, v.rdf_type.0, c.0));
        s.assert(t(y.0, v.rdf_type.0, c.0));
        s.assert(t(x.0, p.0, z.0));
        s.assert(t(y.0, p.0, z.0));
        let ax = resolve(&s, &v);
        let d = fire_all(&s, &ax, &v, None);
        assert!(
            d.contains(&t(x.0, v.owl_same_as.0, y.0)) || d.contains(&t(y.0, v.owl_same_as.0, x.0)),
            "prp-key: x sameAs y (or reverse)"
        );
    }

    #[test]
    fn cls_int1_two_member() {
        let (mut s, v) = fresh();
        let c = TermId(50);
        let c1 = TermId(51);
        let c2 = TermId(52);
        let x = TermId(100);
        s.assert(t(c.0, v.owl_intersection_of.0, 1000));
        assert_list(&mut s, &v, 1000, &[c1.0, c2.0]);
        s.assert(t(x.0, v.rdf_type.0, c1.0));
        s.assert(t(x.0, v.rdf_type.0, c2.0));
        let ax = resolve(&s, &v);
        let d = fire_all(&s, &ax, &v, None);
        assert!(d.contains(&t(x.0, v.rdf_type.0, c.0)), "cls-int1");
    }

    #[test]
    fn scm_int_emits_sub_class_of_for_each_member() {
        let (mut s, v) = fresh();
        let c = TermId(50);
        let c1 = TermId(51);
        let c2 = TermId(52);
        s.assert(t(c.0, v.owl_intersection_of.0, 1000));
        assert_list(&mut s, &v, 1000, &[c1.0, c2.0]);
        let ax = resolve(&s, &v);
        let d = fire_all(&s, &ax, &v, None);
        assert!(
            d.contains(&t(c.0, v.rdfs_sub_class_of.0, c1.0)),
            "scm-int: c ⊑ c1"
        );
        assert!(
            d.contains(&t(c.0, v.rdfs_sub_class_of.0, c2.0)),
            "scm-int: c ⊑ c2"
        );
    }

    #[test]
    fn cls_uni_either_member() {
        let (mut s, v) = fresh();
        let c = TermId(50);
        let c1 = TermId(51);
        let c2 = TermId(52);
        let x = TermId(100);
        let y = TermId(101);
        s.assert(t(c.0, v.owl_union_of.0, 1000));
        assert_list(&mut s, &v, 1000, &[c1.0, c2.0]);
        s.assert(t(x.0, v.rdf_type.0, c1.0));
        s.assert(t(y.0, v.rdf_type.0, c2.0));
        let ax = resolve(&s, &v);
        let d = fire_all(&s, &ax, &v, None);
        assert!(d.contains(&t(x.0, v.rdf_type.0, c.0)), "cls-uni via c1");
        assert!(d.contains(&t(y.0, v.rdf_type.0, c.0)), "cls-uni via c2");
    }

    #[test]
    fn cax_adc_pair_violation() {
        let (mut s, v) = fresh();
        let adc = TermId(50);
        let c1 = TermId(51);
        let c2 = TermId(52);
        let c3 = TermId(53);
        let x = TermId(100);
        s.assert(t(adc.0, v.rdf_type.0, v.owl_all_disjoint_classes.0));
        s.assert(t(adc.0, v.owl_members.0, 1000));
        assert_list(&mut s, &v, 1000, &[c1.0, c2.0, c3.0]);
        s.assert(t(x.0, v.rdf_type.0, c1.0));
        s.assert(t(x.0, v.rdf_type.0, c2.0));
        let ax = resolve(&s, &v);
        let d = fire_all(&s, &ax, &v, None);
        assert!(
            d.contains(&t(x.0, v.rdf_type.0, v.owl_nothing.0)),
            "cax-adc: x : c1 ∧ x : c2 with AllDisjointClasses ⇒ x : owl:Nothing"
        );
    }

    #[test]
    fn cls_maxc1_zero_cardinality_violation() {
        use crate::types::MaxCardRestriction;
        let (mut s, v) = fresh();
        let x = TermId(50); // restriction class
        let p = TermId(51); // onProperty
        let u = TermId(100);
        let y = TermId(101);
        // u : x, u p y  with maxCardinality 0 on p ⇒ inconsistency.
        s.assert(t(u.0, v.rdf_type.0, x.0));
        s.assert(t(u.0, p.0, y.0));
        s.set_card_restrictions(vec![MaxCardRestriction {
            class: x,
            property: p,
            max: 0,
        }]);
        let ax = resolve(&s, &v);
        let d = fire_all(&s, &ax, &v, None);
        assert!(
            d.contains(&t(u.0, v.rdf_type.0, v.owl_nothing.0)),
            "cls-maxc1: maxCard 0 with a p-value ⇒ u : owl:Nothing"
        );
    }

    #[test]
    fn cls_maxc1_no_value_is_consistent() {
        use crate::types::MaxCardRestriction;
        let (mut s, v) = fresh();
        let x = TermId(50);
        let p = TermId(51);
        let u = TermId(100);
        // u : x but NO u p ? — no violation.
        s.assert(t(u.0, v.rdf_type.0, x.0));
        s.set_card_restrictions(vec![MaxCardRestriction {
            class: x,
            property: p,
            max: 0,
        }]);
        let ax = resolve(&s, &v);
        let d = fire_all(&s, &ax, &v, None);
        assert!(
            !d.contains(&t(u.0, v.rdf_type.0, v.owl_nothing.0)),
            "cls-maxc1: no p-value ⇒ no inconsistency"
        );
    }

    #[test]
    fn cls_maxc2_one_cardinality_merges() {
        use crate::types::MaxCardRestriction;
        let (mut s, v) = fresh();
        let x = TermId(50);
        let p = TermId(51);
        let u = TermId(100);
        let y1 = TermId(101);
        let y2 = TermId(102);
        // u : x, u p y1, u p y2  with maxCardinality 1 on p ⇒ y1 sameAs y2.
        s.assert(t(u.0, v.rdf_type.0, x.0));
        s.assert(t(u.0, p.0, y1.0));
        s.assert(t(u.0, p.0, y2.0));
        s.set_card_restrictions(vec![MaxCardRestriction {
            class: x,
            property: p,
            max: 1,
        }]);
        let ax = resolve(&s, &v);
        let d = fire_all(&s, &ax, &v, None);
        assert!(
            d.contains(&t(y1.0, v.owl_same_as.0, y2.0))
                || d.contains(&t(y2.0, v.owl_same_as.0, y1.0)),
            "cls-maxc2: two p-values under maxCard 1 ⇒ y1 sameAs y2"
        );
    }

    #[test]
    fn cls_maxc2_single_value_no_merge() {
        use crate::types::MaxCardRestriction;
        let (mut s, v) = fresh();
        let x = TermId(50);
        let p = TermId(51);
        let u = TermId(100);
        let y1 = TermId(101);
        s.assert(t(u.0, v.rdf_type.0, x.0));
        s.assert(t(u.0, p.0, y1.0));
        s.set_card_restrictions(vec![MaxCardRestriction {
            class: x,
            property: p,
            max: 1,
        }]);
        let ax = resolve(&s, &v);
        let d = fire_all(&s, &ax, &v, None);
        // No second value → no sameAs, and certainly no reflexive sameAs.
        assert!(!d.contains(&t(y1.0, v.owl_same_as.0, y1.0)));
        assert_eq!(
            d.iter().filter(|(tr, _)| tr.p == v.owl_same_as).count(),
            0,
            "cls-maxc2: a single p-value derives no sameAs"
        );
    }

    #[test]
    fn eq_diff_list_asserts_pairs() {
        let (mut s, v) = fresh();
        let ad = TermId(50);
        let a = TermId(100);
        let b = TermId(101);
        s.assert(t(ad.0, v.rdf_type.0, v.owl_all_different.0));
        s.assert(t(ad.0, v.owl_distinct_members.0, 1000));
        assert_list(&mut s, &v, 1000, &[a.0, b.0]);
        let ax = resolve(&s, &v);
        let d = fire_all(&s, &ax, &v, None);
        assert!(d.contains(&t(a.0, v.owl_different_from.0, b.0)));
        assert!(d.contains(&t(b.0, v.owl_different_from.0, a.0)));
    }
}
