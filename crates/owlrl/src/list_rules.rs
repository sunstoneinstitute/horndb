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
//! Per SPEC-04 F5 (partition `rdf:type` work by class id and parallelise),
//! the four `rdf:type`-driven rules — `cls-int1`, `cls-uni`, `cax-adc`,
//! `prp-key` — run their per-subject filtering across rayon's pool when the
//! class extent exceeds [`PAR_TYPE_THRESHOLD`], gated by
//! [`crate::engine::ParallelStrategy`] (`Auto` default; `Serial` is the
//! differential-test oracle). The store reads in a materialise round are
//! immutable (the round delta applies only at round end — see `engine.rs`),
//! so the per-subject work is data-parallel; the helpers [`map_subjects`] /
//! [`extend_delta`] keep the parallel and serial paths' output identical.
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

use rayon::prelude::*;
use rustc_hash::FxHashSet;
use smallvec::{smallvec, SmallVec};

use crate::delta::Delta;
use crate::engine::ParallelStrategy;
use crate::provenance::Provenance;
use crate::store::TripleStore;
use crate::types::{MaxCardRestriction, QualMaxCardRestriction, TermId, Triple};
use crate::vocab::Vocabulary;

/// All list-axiom shapes resolved at load time. One `Vec` per W3C rule kind.
#[derive(Debug, Default, Clone)]
pub struct SchemaAxioms {
    /// `(p, [p1, p2, ..., pn])` — one entry per `?p owl:propertyChainAxiom ?list`.
    pub property_chains: Vec<(TermId, Vec<TermId>)>,
    /// `(c, [p1, ..., pn])` — one entry per `?c owl:hasKey ?list`.
    pub keys: Vec<(TermId, Vec<TermId>)>,
    /// `(c, listhead, [c1, ..., cn])` — one entry per `?c owl:intersectionOf
    /// ?list`. `listhead` is the `rdf:List` head term of the originating axiom
    /// triple `?c owl:intersectionOf listhead`; it is recorded as the premise
    /// of the schema-only `scm-int` rule so its proof bottoms out at the
    /// asserted axiom.
    pub intersections: Vec<(TermId, TermId, Vec<TermId>)>,
    /// `(c, [c1, ..., cn])` — one entry per `?c owl:unionOf ?list`.
    pub unions: Vec<(TermId, Vec<TermId>)>,
    /// `[c1, ..., cn]` — one entry per `?adc rdf:type owl:AllDisjointClasses`
    /// with an `owl:members` list.
    pub all_disjoint_classes: Vec<Vec<TermId>>,
    /// `[p1, ..., pn]` — one entry per `?adp rdf:type owl:AllDisjointProperties`
    /// with an `owl:members` list. Drives `prp-adp` (the list-walking analogue
    /// of the pairwise `prp-pdw`).
    pub all_disjoint_properties: Vec<Vec<TermId>>,
    /// `(ad, members_pred, listhead, [x1, ..., xn])` — one entry per
    /// `?ad rdf:type owl:AllDifferent` with an `owl:members` or
    /// `owl:distinctMembers` list. `members_pred` is the predicate (`owl:members`
    /// or `owl:distinctMembers`) this list came from and `listhead` is the
    /// `rdf:List` head; together they form the originating axiom triple
    /// `?ad members_pred listhead`, recorded as the premise of the schema-only
    /// `eq-diff2`/`eq-diff3` derivations so their proofs bottom out at the
    /// asserted axiom.
    pub all_different: Vec<(TermId, TermId, TermId, Vec<TermId>)>,
    /// Resolved unqualified max-cardinality restrictions (`cls-maxc1`/`cls-maxc2`).
    /// Resolved at load time (`integration.rs`) and carried on the store; copied
    /// here by `resolve` so they ride the same semi-naïve dirty-prune path.
    pub max_card_restrictions: Vec<MaxCardRestriction>,
    /// Resolved qualified max-cardinality restrictions (`cls-maxqc1`–`cls-maxqc4`).
    /// Resolved at load time (`integration.rs`); copied here by `resolve` so they
    /// ride the same semi-naïve dirty-prune path.
    pub qual_max_card_restrictions: Vec<crate::types::QualMaxCardRestriction>,
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
        // prp-adp reads each disjoint property `p_i` (the `?u p_i ?w` body
        // patterns) — not rdf:type. Re-fire whenever any becomes dirty.
        for props in &self.all_disjoint_properties {
            for p in props {
                s.insert(*p);
            }
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
        // cls-maxqc1..4 read rdf:type (for ?u : ?x and ?y : ?filler) and each
        // restricted property (for ?u ?p ?y). Re-fire whenever any becomes dirty.
        if !self.qual_max_card_restrictions.is_empty() {
            s.insert(vocab.rdf_type);
            for r in &self.qual_max_card_restrictions {
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
            && self.all_disjoint_properties.is_empty()
            && self.all_different.is_empty()
            && self.max_card_restrictions.is_empty()
            && self.qual_max_card_restrictions.is_empty()
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
                out.intersections.push((t.s, t.o, cs));
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

    // ?adp rdf:type owl:AllDisjointProperties + ?adp owl:members ?head.
    {
        let adps: Vec<TermId> = store
            .probe(
                None,
                vocab.rdf_type,
                Some(vocab.owl_all_disjoint_properties),
            )
            .map(|t| t.s)
            .collect();
        for adp in adps {
            if let Some(head) = first_object(store, adp, vocab.owl_members) {
                if let Some(ps) = walk_list(store, vocab, head) {
                    if ps.len() >= 2 {
                        out.all_disjoint_properties.push(ps);
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
            // Record which predicate carried this members list so the
            // eq-diff2/3 premise names the axiom triple correctly.
            let head_pred = first_object(store, ad, vocab.owl_distinct_members)
                .map(|h| (vocab.owl_distinct_members, h))
                .or_else(|| {
                    first_object(store, ad, vocab.owl_members).map(|h| (vocab.owl_members, h))
                });
            if let Some((members_pred, head)) = head_pred {
                if let Some(xs) = walk_list(store, vocab, head) {
                    if xs.len() >= 2 {
                        out.all_different.push((ad, members_pred, head, xs));
                    }
                }
            }
        }
    }

    // Max-cardinality restrictions are classified at load time (integration.rs),
    // where the dictionary can parse the literal value; carry them through.
    out.max_card_restrictions = store.card_restrictions().to_vec();
    out.qual_max_card_restrictions = store.qual_card_restrictions().to_vec();

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

/// Subject-count threshold above which the `rdf:type`-driven list rules
/// (`cls-int1`, `cls-uni`, `cax-adc`, `prp-key`) parallelise their per-subject
/// filtering across rayon's pool (SPEC-04 F5). Below it the rayon split +
/// merge overhead outweighs the work, so the original sequential loop runs.
/// Tuned by the `rdf_type_skew` bench; small enough that the differential test's
/// skewed inputs still exercise the parallel path.
pub(crate) const PAR_TYPE_THRESHOLD: usize = 256;

/// Fire every list rule whose body predicates intersect `dirty` (or all of
/// them if `dirty` is `None`, signalling the first round).
///
/// `store: &(dyn TripleStore + Sync)` so the SPEC-04 F5 parallel path can share
/// it across rayon worker threads. `parallel` selects the per-subject
/// parallelisation strategy for the `rdf:type`-driven rules.
pub fn fire_all(
    store: &(dyn TripleStore + Sync),
    axioms: &SchemaAxioms,
    vocab: &Vocabulary,
    dirty: Option<&FxHashSet<TermId>>,
    parallel: ParallelStrategy,
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
            fire_prp_key(store, vocab, *c, ps, parallel, &mut out);
        }
    }

    // cls-int1 — body reads rdf:type for each c_i.
    if !axioms.intersections.is_empty() && is_dirty(dirty, vocab.rdf_type) {
        for (c, _listhead, cs) in &axioms.intersections {
            fire_cls_int1(store, vocab, *c, cs, parallel, &mut out);
        }
    }

    // scm-int — schema-only: each intersection class is a subclass of every
    // member of its list. Output is fully determined by the resolved schema
    // so we fire once, on the first round.
    if !axioms.intersections.is_empty() && dirty.is_none() {
        for (c, listhead, cs) in &axioms.intersections {
            fire_scm_int(store, vocab, *c, *listhead, cs, &mut out);
        }
    }

    // cls-uni — body reads rdf:type for each c_i.
    if !axioms.unions.is_empty() && is_dirty(dirty, vocab.rdf_type) {
        for (c, cs) in &axioms.unions {
            fire_cls_uni(store, vocab, *c, cs, parallel, &mut out);
        }
    }

    // cax-adc — body reads rdf:type for each c_i.
    if !axioms.all_disjoint_classes.is_empty() && is_dirty(dirty, vocab.rdf_type) {
        for cs in &axioms.all_disjoint_classes {
            fire_cax_adc(store, vocab, cs, parallel, &mut out);
        }
    }

    // prp-adp — body reads each disjoint property `p_i` (`?u p_i ?w`).
    if !axioms.all_disjoint_properties.is_empty()
        && any_dirty_for_disjoint_properties(axioms, dirty)
    {
        for props in &axioms.all_disjoint_properties {
            fire_prp_adp(store, vocab, props, &mut out);
        }
    }

    // eq-diff2 / eq-diff3: assert pairwise differentFrom from the resolved
    // list. The differentFrom triples are schema-derived and constant
    // across rounds, so we fire on the first round only — subsequent
    // applies are no-ops via `store.contains`.
    if !axioms.all_different.is_empty() && dirty.is_none() {
        for (ad, members_pred, listhead, xs) in &axioms.all_different {
            fire_eq_diff_list(vocab, *ad, *members_pred, *listhead, xs, &mut out);
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

    // cls-maxqc1/maxqc2 — maxQualifiedCardinality 0 ⇒ inconsistency.
    // cls-maxqc3/maxqc4 — maxQualifiedCardinality 1 ⇒ sameAs.
    if !axioms.qual_max_card_restrictions.is_empty() {
        let type_dirty = is_dirty(dirty, vocab.rdf_type);
        for r in &axioms.qual_max_card_restrictions {
            if !type_dirty && !is_dirty(dirty, r.property) {
                continue;
            }
            match r.max {
                0 => fire_cls_maxqc_zero(store, vocab, r, &mut out),
                1 => fire_cls_maxqc_one(store, vocab, r, &mut out),
                _ => {}
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

fn any_dirty_for_disjoint_properties(
    axioms: &SchemaAxioms,
    dirty: Option<&FxHashSet<TermId>>,
) -> bool {
    let Some(dirty) = dirty else {
        return true;
    };
    for props in &axioms.all_disjoint_properties {
        for p in props {
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
// SPEC-04 F5 — `rdf:type` skew parallelism helpers.
//
// The `rdf:type`-driven list rules (`cls-int1`, `cls-uni`, `cax-adc`,
// `prp-key`) gather a subject extent and run an independent per-subject test
// that emits at most one derived triple per subject. `map_subjects` runs that
// test in parallel across rayon's pool when the extent is large enough (and the
// strategy permits), and sequentially otherwise; `extend_delta` folds the
// produced candidates into the shared round delta (insert is idempotent, so the
// triple set is identical to the serial `!out.contains` guard regardless of
// the order rayon produces results in).
// ---------------------------------------------------------------------------

/// Apply `f` to every subject in `xs`, collecting the `Some` results. Uses
/// rayon when `parallel == Auto` and `xs` is above `PAR_TYPE_THRESHOLD`;
/// sequential otherwise. `f` must be a pure read over the (immutable) store.
fn map_subjects<F>(xs: &[TermId], parallel: ParallelStrategy, f: F) -> Vec<(Triple, Provenance)>
where
    F: Fn(TermId) -> Option<(Triple, Provenance)> + Sync + Send,
{
    if parallel == ParallelStrategy::Auto && xs.len() >= PAR_TYPE_THRESHOLD {
        xs.par_iter().filter_map(|&x| f(x)).collect()
    } else {
        xs.iter().filter_map(|&x| f(x)).collect()
    }
}

/// Fold rule-produced `(triple, provenance)` candidates into `out`. `insert`
/// dedups, so re-derivations and per-subject duplicates collapse to one entry.
fn extend_delta(out: &mut Delta, produced: Vec<(Triple, Provenance)>) {
    for (t, prov) in produced {
        out.insert(t, prov);
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

    // Each frontier entry carries `(u0, u_mid, path)` where `path` is the
    // accumulated body triples `?u(i-1) p_i ?ui` walked so far — recorded as
    // the premises for the derived `?u0 ?p ?un`.
    type Path = SmallVec<[Triple; 4]>;
    let mut frontier: Vec<(TermId, TermId, Path)> = Vec::new();
    for t in store.scan_predicate(chain[0]) {
        let mut path: Path = SmallVec::new();
        path.push(Triple::new(t.s, chain[0], t.o));
        frontier.push((t.s, t.o, path));
    }
    if chain.len() == 1 {
        // Chain of length 1 is just sub-property propagation: the single
        // `(u0, chain[0], un)` triple is the lone premise.
        for (u0, un, path) in frontier {
            emit_pair(out, "prp-spo2", u0, head_pred, un, path);
        }
        return;
    }
    let mut next: Vec<(TermId, TermId, Path)> = Vec::new();
    for &p_i in &chain[1..] {
        next.clear();
        for (u0, u_mid, prefix) in &frontier {
            for t in store.probe(Some(*u_mid), p_i, None) {
                let mut path = prefix.clone();
                path.push(Triple::new(*u_mid, p_i, t.o));
                next.push((*u0, t.o, path));
            }
        }
        std::mem::swap(&mut frontier, &mut next);
        if frontier.is_empty() {
            return;
        }
    }
    for (u0, un, path) in frontier {
        emit_pair(out, "prp-spo2", u0, head_pred, un, path);
    }
}

fn emit_pair(
    out: &mut Delta,
    rule_id: &'static str,
    s: TermId,
    p: TermId,
    o: TermId,
    premises: SmallVec<[Triple; 4]>,
) {
    let head = Triple::new(s, p, o);
    if !out.contains(&head) {
        out.insert(head, Provenance { rule_id, premises });
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
    store: &(dyn TripleStore + Sync),
    vocab: &Vocabulary,
    c: TermId,
    ps: &[TermId],
    parallel: ParallelStrategy,
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

    // All `?x owl:sameAs ?y` derivations for a given `?x` are independent of any
    // other `?x`, so the outer subject loop parallelises over the (skewed) keyed
    // class extent. Each `?x` produces its own candidate list, flattened and
    // deduped into the round delta by `extend_delta`.
    let emit_for_x = |x: TermId| -> Vec<(Triple, Provenance)> {
        let mut produced: Vec<(Triple, Provenance)> = Vec::new();
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
                    if !store.contains(&head) {
                        // Body atoms: ?x : c, ?y : c, the shared ps[0] value,
                        // and the matched ?z_i on every remaining key property
                        // (the `x_choice` values, which survivors were filtered
                        // against on the `(y, p_i, z_i)` side).
                        let mut premises: SmallVec<[Triple; 4]> = smallvec![
                            Triple::new(x, vocab.rdf_type, c),
                            Triple::new(y, vocab.rdf_type, c),
                            Triple::new(x, ps[0], z0),
                            Triple::new(y, ps[0], z0),
                        ];
                        for (i, &z_i) in x_choice.iter().enumerate() {
                            let p_i = ps[1 + i];
                            premises.push(Triple::new(x, p_i, z_i));
                            premises.push(Triple::new(y, p_i, z_i));
                        }
                        produced.push((
                            head,
                            Provenance {
                                rule_id: "prp-key",
                                premises,
                            },
                        ));
                    }
                }
            }
        }
        produced
    };

    let produced: Vec<(Triple, Provenance)> =
        if parallel == ParallelStrategy::Auto && xs.len() >= PAR_TYPE_THRESHOLD {
            xs.par_iter().flat_map_iter(|&x| emit_for_x(x)).collect()
        } else {
            xs.iter().flat_map(|&x| emit_for_x(x)).collect()
        };
    extend_delta(out, produced);
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
    store: &(dyn TripleStore + Sync),
    vocab: &Vocabulary,
    c: TermId,
    cs: &[TermId],
    parallel: ParallelStrategy,
    out: &mut Delta,
) {
    debug_assert!(!cs.is_empty());
    // Seed on the first class extent (SPEC-04 F5: this is the skewed
    // `rdf:type` partition the rule scans). The per-subject membership check
    // against the remaining classes is independent across subjects, so it
    // parallelises by class id once the extent is large enough.
    let xs: Vec<TermId> = store
        .probe(None, vocab.rdf_type, Some(cs[0]))
        .map(|t| t.s)
        .collect();

    let emit = |x: TermId| -> Option<(Triple, Provenance)> {
        if !cs[1..]
            .iter()
            .all(|&c_i| store.contains(&Triple::new(x, vocab.rdf_type, c_i)))
        {
            return None;
        }
        let head = Triple::new(x, vocab.rdf_type, c);
        if store.contains(&head) {
            return None;
        }
        // Body atoms: ?x rdf:type ci for every member ci of the list.
        let mut premises: SmallVec<[Triple; 4]> = smallvec![Triple::new(x, vocab.rdf_type, cs[0])];
        for &c_i in &cs[1..] {
            premises.push(Triple::new(x, vocab.rdf_type, c_i));
        }
        Some((
            head,
            Provenance {
                rule_id: "cls-int1",
                premises,
            },
        ))
    };

    extend_delta(out, map_subjects(&xs, parallel, emit));
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
    listhead: TermId,
    cs: &[TermId],
    out: &mut Delta,
) {
    // Schema-only: the sole antecedent is the axiom triple `?c owl:intersectionOf
    // listhead`. Record it so the proof bottoms out at the asserted axiom.
    let axiom = Triple::new(c, vocab.owl_intersection_of, listhead);
    for &ci in cs {
        let head = Triple::new(c, vocab.rdfs_sub_class_of, ci);
        if !out.contains(&head) && !store.contains(&head) {
            out.insert(
                head,
                Provenance {
                    rule_id: "scm-int",
                    premises: smallvec![axiom],
                },
            );
        }
    }
}

// ---------------------------------------------------------------------------
// cls-uni — `?c owl:unionOf (c1 ... cn) ∧ ∃i. ?x rdf:type ci ⇒ ?x rdf:type ?c`
// ---------------------------------------------------------------------------
fn fire_cls_uni(
    store: &(dyn TripleStore + Sync),
    vocab: &Vocabulary,
    c: TermId,
    cs: &[TermId],
    parallel: ParallelStrategy,
    out: &mut Delta,
) {
    // Gather every `(subject, member-class)` membership across the union list
    // into an owned vec, then map per-membership. The member class is carried
    // alongside the subject so each derived triple records the exact body atom
    // (`?x rdf:type ci`) that fired it — identical to the serial path.
    let memberships: Vec<(TermId, TermId)> = cs
        .iter()
        .flat_map(|&c_i| {
            store
                .probe(None, vocab.rdf_type, Some(c_i))
                .map(move |t| (t.s, c_i))
        })
        .collect();

    let emit = |(x, c_i): (TermId, TermId)| -> Option<(Triple, Provenance)> {
        let head = Triple::new(x, vocab.rdf_type, c);
        if store.contains(&head) {
            return None;
        }
        // Body atom: the single matched membership ?x rdf:type ci.
        Some((
            head,
            Provenance {
                rule_id: "cls-uni",
                premises: smallvec![Triple::new(x, vocab.rdf_type, c_i)],
            },
        ))
    };

    let produced = if parallel == ParallelStrategy::Auto && memberships.len() >= PAR_TYPE_THRESHOLD
    {
        memberships.par_iter().filter_map(|&m| emit(m)).collect()
    } else {
        memberships.iter().filter_map(|&m| emit(m)).collect()
    };
    extend_delta(out, produced);
}

// ---------------------------------------------------------------------------
// cax-adc — `_:adc rdf:type owl:AllDisjointClasses ∧ _:adc owl:members
// (c1 ... cn) ∧ ?x rdf:type ci ∧ ?x rdf:type cj (i ≠ j) ⇒ ?x rdf:type
// owl:Nothing`
//
// Implementation: enumerate every pair (i, j) with i < j; for each pair,
// reuse the cax-dw shape on the (c_i, c_j) sub-extents.
// ---------------------------------------------------------------------------
fn fire_cax_adc(
    store: &(dyn TripleStore + Sync),
    vocab: &Vocabulary,
    cs: &[TermId],
    parallel: ParallelStrategy,
    out: &mut Delta,
) {
    for i in 0..cs.len() {
        let xs_i: Vec<TermId> = store
            .probe(None, vocab.rdf_type, Some(cs[i]))
            .map(|t| t.s)
            .collect();
        if xs_i.is_empty() {
            continue;
        }
        let ci = cs[i];
        for &cj in cs.iter().skip(i + 1) {
            // For this disjoint pair (ci, cj), each subject of ci that is also a
            // cj is an inconsistency. The per-subject test is independent, so it
            // parallelises over the (skewed) ci extent.
            let emit = |x: TermId| -> Option<(Triple, Provenance)> {
                if !store.contains(&Triple::new(x, vocab.rdf_type, cj)) {
                    return None;
                }
                let head = Triple::new(x, vocab.rdf_type, vocab.owl_nothing);
                if store.contains(&head) {
                    return None;
                }
                // Body atoms: ?x rdf:type ci and ?x rdf:type cj.
                Some((
                    head,
                    Provenance {
                        rule_id: "cax-adc",
                        premises: smallvec![
                            Triple::new(x, vocab.rdf_type, ci),
                            Triple::new(x, vocab.rdf_type, cj),
                        ],
                    },
                ))
            };
            extend_delta(out, map_subjects(&xs_i, parallel, emit));
        }
    }
}

// ---------------------------------------------------------------------------
// prp-adp — `_:adp rdf:type owl:AllDisjointProperties ∧ _:adp owl:members
// (p1 ... pn) ∧ ?u pi ?w ∧ ?u pj ?w (i ≠ j) ⇒ false`. Materialised, like
// every other OWL 2 RL `false`-rule in this engine, as `?u rdf:type
// owl:Nothing` so `Engine::is_consistent()` reports the inconsistency. This
// is the list-walking analogue of the pairwise `prp-pdw` (which keys off
// `owl:propertyDisjointWith`); both head `?u rdf:type owl:Nothing`.
//
// Implementation: enumerate every pair (i, j) with i < j; scan `p_i` to
// collect each `(u, w)` pair, then check the store for `u p_j w`. A shared
// `(u, w)` across two distinct list members violates disjointness.
// ---------------------------------------------------------------------------
fn fire_prp_adp(store: &dyn TripleStore, vocab: &Vocabulary, props: &[TermId], out: &mut Delta) {
    for i in 0..props.len() {
        let pi = props[i];
        let pairs: Vec<(TermId, TermId)> = store.scan_predicate(pi).map(|t| (t.s, t.o)).collect();
        if pairs.is_empty() {
            continue;
        }
        for &pj in props.iter().skip(i + 1) {
            // W3C `prp-adp` ranges `i < j` over *list positions*, not property
            // identity — so a list that repeats a property, e.g. `(:p :p)`,
            // declares `:p` disjoint with itself, and any single `?u :p ?w`
            // matches both body atoms (`?u pi ?w ∧ ?u pj ?w`) and is therefore
            // inconsistent. We intentionally do *not* skip `pi == pj`: when the
            // two positions carry the same property, `store.contains(u, pj, w)`
            // is trivially the just-scanned triple, which is the correct
            // self-disjoint behaviour (and matches the compiled `prp-pdw`).
            for &(u, w) in &pairs {
                if store.contains(&Triple::new(u, pj, w)) {
                    let head = Triple::new(u, vocab.rdf_type, vocab.owl_nothing);
                    if !out.contains(&head) && !store.contains(&head) {
                        // Body atoms: ?u pi ?w and ?u pj ?w (the shared pair
                        // across two disjoint list members).
                        out.insert(
                            head,
                            Provenance {
                                rule_id: "prp-adp",
                                premises: smallvec![Triple::new(u, pi, w), Triple::new(u, pj, w),],
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
        if let Some(viol) = store.probe(Some(u), property, None).next() {
            let head = Triple::new(u, vocab.rdf_type, vocab.owl_nothing);
            if !out.contains(&head) && !store.contains(&head) {
                // Instance-level body atoms: ?u rdf:type class and the violating
                // value ?u property ?y. The restriction declaration
                // (owl:maxCardinality / owl:onProperty) is an asserted schema
                // side condition the resolved `MaxCardRestriction` does not carry
                // the restriction-class node for — elided here; a follow-up may
                // record it.
                out.insert(
                    head,
                    Provenance {
                        rule_id: "cls-maxc1",
                        premises: smallvec![
                            Triple::new(u, vocab.rdf_type, class),
                            Triple::new(u, property, viol.o),
                        ],
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
                        // Instance-level body atoms: ?u rdf:type class and the two
                        // distinct values ?u property y1/y2. The restriction
                        // declaration (owl:maxCardinality / owl:onProperty) is an
                        // asserted schema side condition elided here (the resolved
                        // `MaxCardRestriction` does not carry the restriction node);
                        // a follow-up may record it.
                        out.insert(
                            head,
                            Provenance {
                                rule_id: "cls-maxc2",
                                premises: smallvec![
                                    Triple::new(u, vocab.rdf_type, class),
                                    Triple::new(u, property, y1),
                                    Triple::new(u, property, y2),
                                ],
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
fn fire_eq_diff_list(
    vocab: &Vocabulary,
    ad: TermId,
    members_pred: TermId,
    listhead: TermId,
    xs: &[TermId],
    out: &mut Delta,
) {
    // Schema-only: the sole antecedent is the axiom triple `?ad members_pred
    // listhead` (members_pred is whichever of owl:members / owl:distinctMembers
    // this list came from). Record it so the proof bottoms out at the asserted
    // axiom.
    let axiom = Triple::new(ad, members_pred, listhead);
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
                        premises: smallvec![axiom],
                    },
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// cls-maxqc1 / cls-maxqc2 — `?x owl:maxQualifiedCardinality "0" ∧
// ?x owl:onProperty ?p ∧ ?x owl:onClass ?c ∧ ?u rdf:type ?x ∧ ?u ?p ?y ∧
// (?y rdf:type ?c, or ?c = owl:Thing) ⇒ false`. Materialised as
// `?u rdf:type owl:Nothing` so `Engine::is_consistent()` reports it.
// ---------------------------------------------------------------------------
fn fire_cls_maxqc_zero(
    store: &dyn TripleStore,
    vocab: &Vocabulary,
    r: &QualMaxCardRestriction,
    out: &mut Delta,
) {
    let us: Vec<TermId> = store
        .probe(None, vocab.rdf_type, Some(r.class))
        .map(|t| t.s)
        .collect();
    // The owl:Thing-filler variant is cls-maxqc2; the class-filler variant is
    // cls-maxqc1. Both materialise `?u rdf:type owl:Nothing`; only the recorded
    // provenance rule id differs (consumed by SPEC-08 / the proof API).
    let rule_id = if r.filler == vocab.owl_thing {
        "cls-maxqc2"
    } else {
        "cls-maxqc1"
    };
    for u in us {
        let qualifying_value = store
            .probe(Some(u), r.property, None)
            .find(|t| qualifies(store, vocab, t.o, r.filler))
            .map(|t| t.o);
        if let Some(y) = qualifying_value {
            let head = Triple::new(u, vocab.rdf_type, vocab.owl_nothing);
            if !out.contains(&head) && !store.contains(&head) {
                // Instance-level body atoms: ?u rdf:type r.class and the
                // qualifying value ?u r.property ?y. The restriction declaration
                // (owl:maxQualifiedCardinality / owl:onProperty / owl:onClass) is
                // an asserted schema side condition the resolved
                // `QualMaxCardRestriction` does not carry the restriction-class
                // node for — elided here; a follow-up may record it.
                out.insert(
                    head,
                    Provenance {
                        rule_id,
                        premises: smallvec![
                            Triple::new(u, vocab.rdf_type, r.class),
                            Triple::new(u, r.property, y),
                        ],
                    },
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// cls-maxqc3 / cls-maxqc4 — `?x owl:maxQualifiedCardinality "1" ∧
// ?x owl:onProperty ?p ∧ ?x owl:onClass ?c ∧ ?u rdf:type ?x ∧
// ?u ?p ?y1 ∧ ?u ?p ?y2 ∧ (?yi rdf:type ?c, or ?c = owl:Thing)
// ⇒ ?y1 owl:sameAs ?y2`. sameAs symmetry/transitivity is closed downstream.
// ---------------------------------------------------------------------------
fn fire_cls_maxqc_one(
    store: &dyn TripleStore,
    vocab: &Vocabulary,
    r: &QualMaxCardRestriction,
    out: &mut Delta,
) {
    // The owl:Thing-filler variant is cls-maxqc4; the class-filler variant is
    // cls-maxqc3. Both emit `owl:sameAs`; only the recorded provenance rule id
    // differs (consumed by SPEC-08 / the proof API).
    let rule_id = if r.filler == vocab.owl_thing {
        "cls-maxqc4"
    } else {
        "cls-maxqc3"
    };
    let us: Vec<TermId> = store
        .probe(None, vocab.rdf_type, Some(r.class))
        .map(|t| t.s)
        .collect();
    for u in us {
        let ys: Vec<TermId> = store
            .probe(Some(u), r.property, None)
            .map(|t| t.o)
            .filter(|&y| qualifies(store, vocab, y, r.filler))
            .collect();
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
                        // Instance-level body atoms: ?u rdf:type r.class and the
                        // two qualifying values ?u r.property y1/y2. The
                        // restriction declaration (owl:maxQualifiedCardinality /
                        // owl:onProperty / owl:onClass) is an asserted schema side
                        // condition elided here (the resolved
                        // `QualMaxCardRestriction` does not carry the restriction
                        // node); a follow-up may record it.
                        out.insert(
                            head,
                            Provenance {
                                rule_id,
                                premises: smallvec![
                                    Triple::new(u, vocab.rdf_type, r.class),
                                    Triple::new(u, r.property, y1),
                                    Triple::new(u, r.property, y2),
                                ],
                            },
                        );
                    }
                }
            }
        }
    }
}

/// A property value `y` counts toward a qualified-cardinality restriction iff
/// the filler is `owl:Thing` (count every value — cls-maxqc2/maxqc4) or `y` is
/// typed as the filler class (cls-maxqc1/maxqc3).
fn qualifies(store: &dyn TripleStore, vocab: &Vocabulary, y: TermId, filler: TermId) -> bool {
    if filler == vocab.owl_thing {
        return true;
    }
    store
        .probe(Some(y), vocab.rdf_type, Some(filler))
        .next()
        .is_some()
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
        let d = fire_all(&s, &ax, &v, None, ParallelStrategy::Auto);
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
        let d = fire_all(&s, &ax, &v, None, ParallelStrategy::Auto);
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
        let d = fire_all(&s, &ax, &v, None, ParallelStrategy::Auto);
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
        let d = fire_all(&s, &ax, &v, None, ParallelStrategy::Auto);
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
        let d = fire_all(&s, &ax, &v, None, ParallelStrategy::Auto);
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
        let d = fire_all(&s, &ax, &v, None, ParallelStrategy::Auto);
        assert!(
            d.contains(&t(x.0, v.rdf_type.0, v.owl_nothing.0)),
            "cax-adc: x : c1 ∧ x : c2 with AllDisjointClasses ⇒ x : owl:Nothing"
        );
    }

    #[test]
    fn resolve_all_disjoint_properties() {
        let (mut s, v) = fresh();
        let adp = TermId(50);
        let p1 = TermId(51);
        let p2 = TermId(52);
        let p3 = TermId(53);
        s.assert(t(adp.0, v.rdf_type.0, v.owl_all_disjoint_properties.0));
        s.assert(t(adp.0, v.owl_members.0, 1000));
        assert_list(&mut s, &v, 1000, &[p1.0, p2.0, p3.0]);
        let ax = resolve(&s, &v);
        assert_eq!(ax.all_disjoint_properties.len(), 1);
        assert_eq!(ax.all_disjoint_properties[0], vec![p1, p2, p3]);
    }

    #[test]
    fn prp_adp_shared_pair_violation() {
        // _:adp a owl:AllDisjointProperties ; owl:members (p1 p2 p3) .
        // u p1 w ∧ u p2 w  ⇒  u : owl:Nothing  (shared (u, w) across two
        // disjoint members).
        let (mut s, v) = fresh();
        let adp = TermId(50);
        let p1 = TermId(51);
        let p2 = TermId(52);
        let p3 = TermId(53);
        let u = TermId(100);
        let w = TermId(101);
        s.assert(t(adp.0, v.rdf_type.0, v.owl_all_disjoint_properties.0));
        s.assert(t(adp.0, v.owl_members.0, 1000));
        assert_list(&mut s, &v, 1000, &[p1.0, p2.0, p3.0]);
        s.assert(t(u.0, p1.0, w.0));
        s.assert(t(u.0, p2.0, w.0));
        let ax = resolve(&s, &v);
        let d = fire_all(&s, &ax, &v, None, ParallelStrategy::Auto);
        assert!(
            d.contains(&t(u.0, v.rdf_type.0, v.owl_nothing.0)),
            "prp-adp: u p1 w ∧ u p2 w with AllDisjointProperties ⇒ u : owl:Nothing"
        );
    }

    #[test]
    fn prp_adp_distinct_objects_are_consistent() {
        // u p1 w1 ∧ u p2 w2 (w1 ≠ w2): no shared (u, w) pair, so disjointness
        // is not violated. This is the shape of the W3C
        // `New-Feature-DisjointObjectProperties-*-cons` cases, which must stay
        // green: Stewie hasFather Peter, hasMother Lois — different objects.
        let (mut s, v) = fresh();
        let adp = TermId(50);
        let p1 = TermId(51);
        let p2 = TermId(52);
        let u = TermId(100);
        let w1 = TermId(101);
        let w2 = TermId(102);
        s.assert(t(adp.0, v.rdf_type.0, v.owl_all_disjoint_properties.0));
        s.assert(t(adp.0, v.owl_members.0, 1000));
        assert_list(&mut s, &v, 1000, &[p1.0, p2.0]);
        s.assert(t(u.0, p1.0, w1.0));
        s.assert(t(u.0, p2.0, w2.0));
        let ax = resolve(&s, &v);
        let d = fire_all(&s, &ax, &v, None, ParallelStrategy::Auto);
        assert!(
            !d.contains(&t(u.0, v.rdf_type.0, v.owl_nothing.0)),
            "prp-adp: distinct objects across disjoint properties ⇒ consistent"
        );
    }

    #[test]
    fn prp_adp_non_adjacent_members_violate() {
        // The violating pair is p1/p3 (non-adjacent list members). The pairwise
        // i<j walk must catch every pair, not just neighbours.
        let (mut s, v) = fresh();
        let adp = TermId(50);
        let p1 = TermId(51);
        let p2 = TermId(52);
        let p3 = TermId(53);
        let u = TermId(100);
        let w = TermId(101);
        s.assert(t(adp.0, v.rdf_type.0, v.owl_all_disjoint_properties.0));
        s.assert(t(adp.0, v.owl_members.0, 1000));
        assert_list(&mut s, &v, 1000, &[p1.0, p2.0, p3.0]);
        s.assert(t(u.0, p1.0, w.0));
        s.assert(t(u.0, p3.0, w.0));
        let ax = resolve(&s, &v);
        let d = fire_all(&s, &ax, &v, None, ParallelStrategy::Auto);
        assert!(
            d.contains(&t(u.0, v.rdf_type.0, v.owl_nothing.0)),
            "prp-adp: must check all i<j pairs, including non-adjacent p1/p3"
        );
    }

    #[test]
    fn prp_adp_self_disjoint_member_violates() {
        // A list that repeats the same property — `(p1 p1)` — declares p1
        // disjoint with itself. W3C `prp-adp` ranges i<j over list *positions*,
        // so a single `u p1 w` satisfies both body atoms ⇒ inconsistency.
        let (mut s, v) = fresh();
        let adp = TermId(50);
        let p1 = TermId(51);
        let u = TermId(100);
        let w = TermId(101);
        s.assert(t(adp.0, v.rdf_type.0, v.owl_all_disjoint_properties.0));
        s.assert(t(adp.0, v.owl_members.0, 1000));
        assert_list(&mut s, &v, 1000, &[p1.0, p1.0]);
        s.assert(t(u.0, p1.0, w.0));
        let ax = resolve(&s, &v);
        let d = fire_all(&s, &ax, &v, None, ParallelStrategy::Auto);
        assert!(
            d.contains(&t(u.0, v.rdf_type.0, v.owl_nothing.0)),
            "prp-adp: a property listed twice is disjoint with itself ⇒ any \
             assertion on it is inconsistent"
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
        let d = fire_all(&s, &ax, &v, None, ParallelStrategy::Auto);
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
        let d = fire_all(&s, &ax, &v, None, ParallelStrategy::Auto);
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
        let d = fire_all(&s, &ax, &v, None, ParallelStrategy::Auto);
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
        let d = fire_all(&s, &ax, &v, None, ParallelStrategy::Auto);
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
        let d = fire_all(&s, &ax, &v, None, ParallelStrategy::Auto);
        assert!(d.contains(&t(a.0, v.owl_different_from.0, b.0)));
        assert!(d.contains(&t(b.0, v.owl_different_from.0, a.0)));
    }

    #[test]
    fn cls_maxqc_zero_class_filler_violation() {
        use crate::types::QualMaxCardRestriction;
        let (mut s, v) = fresh();
        let x = TermId(50);
        let p = TermId(51);
        let c = TermId(52);
        let u = TermId(100);
        let y = TermId(101);
        s.assert(t(u.0, v.rdf_type.0, x.0));
        s.assert(t(u.0, p.0, y.0));
        s.assert(t(y.0, v.rdf_type.0, c.0));
        s.set_qual_card_restrictions(vec![QualMaxCardRestriction {
            class: x,
            property: p,
            filler: c,
            max: 0,
        }]);
        let ax = resolve(&s, &v);
        let d = fire_all(&s, &ax, &v, None, ParallelStrategy::Auto);
        assert!(
            d.contains(&t(u.0, v.rdf_type.0, v.owl_nothing.0)),
            "cls-maxqc1: maxQC 0 with a filler-typed value ⇒ u : owl:Nothing"
        );
    }

    #[test]
    fn cls_maxqc_zero_non_filler_value_is_consistent() {
        use crate::types::QualMaxCardRestriction;
        let (mut s, v) = fresh();
        let x = TermId(50);
        let p = TermId(51);
        let c = TermId(52);
        let u = TermId(100);
        let y = TermId(101);
        s.assert(t(u.0, v.rdf_type.0, x.0));
        s.assert(t(u.0, p.0, y.0));
        s.set_qual_card_restrictions(vec![QualMaxCardRestriction {
            class: x,
            property: p,
            filler: c,
            max: 0,
        }]);
        let ax = resolve(&s, &v);
        let d = fire_all(&s, &ax, &v, None, ParallelStrategy::Auto);
        assert!(
            !d.contains(&t(u.0, v.rdf_type.0, v.owl_nothing.0)),
            "cls-maxqc1: value not of filler class ⇒ no inconsistency"
        );
    }

    #[test]
    fn cls_maxqc_one_class_filler_merges() {
        use crate::types::QualMaxCardRestriction;
        let (mut s, v) = fresh();
        let x = TermId(50);
        let p = TermId(51);
        let c = TermId(52);
        let u = TermId(100);
        let y1 = TermId(101);
        let y2 = TermId(102);
        s.assert(t(u.0, v.rdf_type.0, x.0));
        s.assert(t(u.0, p.0, y1.0));
        s.assert(t(u.0, p.0, y2.0));
        s.assert(t(y1.0, v.rdf_type.0, c.0));
        s.assert(t(y2.0, v.rdf_type.0, c.0));
        s.set_qual_card_restrictions(vec![QualMaxCardRestriction {
            class: x,
            property: p,
            filler: c,
            max: 1,
        }]);
        let ax = resolve(&s, &v);
        let d = fire_all(&s, &ax, &v, None, ParallelStrategy::Auto);
        assert!(
            d.contains(&t(y1.0, v.owl_same_as.0, y2.0))
                || d.contains(&t(y2.0, v.owl_same_as.0, y1.0)),
            "cls-maxqc3: two filler-typed values under maxQC 1 ⇒ sameAs"
        );
    }

    #[test]
    fn cls_maxqc_one_only_one_qualifies_no_merge() {
        use crate::types::QualMaxCardRestriction;
        let (mut s, v) = fresh();
        let x = TermId(50);
        let p = TermId(51);
        let c = TermId(52);
        let u = TermId(100);
        let y1 = TermId(101);
        let y2 = TermId(102);
        s.assert(t(u.0, v.rdf_type.0, x.0));
        s.assert(t(u.0, p.0, y1.0));
        s.assert(t(u.0, p.0, y2.0));
        s.assert(t(y1.0, v.rdf_type.0, c.0));
        s.set_qual_card_restrictions(vec![QualMaxCardRestriction {
            class: x,
            property: p,
            filler: c,
            max: 1,
        }]);
        let ax = resolve(&s, &v);
        let d = fire_all(&s, &ax, &v, None, ParallelStrategy::Auto);
        assert_eq!(
            d.iter().filter(|(tr, _)| tr.p == v.owl_same_as).count(),
            0,
            "cls-maxqc3: only one qualifying value ⇒ no sameAs"
        );
    }

    #[test]
    fn cls_maxqc_one_thing_filler_merges_any_value() {
        use crate::types::QualMaxCardRestriction;
        let (mut s, v) = fresh();
        let x = TermId(50);
        let p = TermId(51);
        let u = TermId(100);
        let y1 = TermId(101);
        let y2 = TermId(102);
        s.assert(t(u.0, v.rdf_type.0, x.0));
        s.assert(t(u.0, p.0, y1.0));
        s.assert(t(u.0, p.0, y2.0));
        s.set_qual_card_restrictions(vec![QualMaxCardRestriction {
            class: x,
            property: p,
            filler: v.owl_thing,
            max: 1,
        }]);
        let ax = resolve(&s, &v);
        let d = fire_all(&s, &ax, &v, None, ParallelStrategy::Auto);
        assert!(
            d.contains(&t(y1.0, v.owl_same_as.0, y2.0))
                || d.contains(&t(y2.0, v.owl_same_as.0, y1.0)),
            "cls-maxqc4: owl:Thing filler ⇒ sameAs over any two values"
        );
        // The owl:Thing-filler variant must record cls-maxqc4 provenance, not
        // cls-maxqc3 (which is the class-filler variant).
        assert!(
            d.iter()
                .filter(|(tr, _)| tr.p == v.owl_same_as)
                .all(|(_, prov)| prov.rule_id == "cls-maxqc4"),
            "owl:Thing filler under maxQC 1 ⇒ provenance rule id is cls-maxqc4"
        );
    }
}

#[cfg(test)]
mod premise_tests {
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
    fn cls_int1_records_instance_premises() {
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
        let d = fire_all(&s, &ax, &v, None, ParallelStrategy::Auto);

        let head = Triple::new(x, v.rdf_type, c);
        let prov = d
            .iter()
            .find(|(tr, _)| **tr == head)
            .map(|(_, p)| p)
            .expect("cls-int1 should derive x rdf:type c");
        assert_eq!(prov.rule_id, "cls-int1");
        assert!(
            !prov.premises.is_empty(),
            "cls-int1 premises must be non-empty"
        );
        assert!(
            prov.premises.contains(&Triple::new(x, v.rdf_type, c1)),
            "cls-int1 premises must contain (x rdf:type c1)"
        );
        assert!(
            prov.premises.contains(&Triple::new(x, v.rdf_type, c2)),
            "cls-int1 premises must contain (x rdf:type c2)"
        );
    }

    #[test]
    fn scm_int_records_axiom_premise() {
        let (mut s, v) = fresh();
        let c = TermId(50);
        let c1 = TermId(51);
        let listhead = TermId(1000);
        s.assert(t(c.0, v.owl_intersection_of.0, listhead.0));
        assert_list(&mut s, &v, listhead.0, &[c1.0]);
        let ax = resolve(&s, &v);
        let d = fire_all(&s, &ax, &v, None, ParallelStrategy::Auto);

        let head = Triple::new(c, v.rdfs_sub_class_of, c1);
        let prov = d
            .iter()
            .find(|(tr, _)| **tr == head)
            .map(|(_, p)| p)
            .expect("scm-int should derive c rdfs:subClassOf c1");
        assert_eq!(prov.rule_id, "scm-int");
        assert!(
            !prov.premises.is_empty(),
            "scm-int premises must be non-empty"
        );
        assert!(
            prov.premises
                .contains(&Triple::new(c, v.owl_intersection_of, listhead)),
            "scm-int premises must contain the originating axiom triple"
        );
    }
}
