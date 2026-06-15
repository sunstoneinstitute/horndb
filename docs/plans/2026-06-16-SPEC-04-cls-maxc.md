# SPEC-04 `cls-maxc1` / `cls-maxc2` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the two unqualified max-cardinality OWL 2 RL class rules `cls-maxc1` (max 0 ⇒ inconsistency) and `cls-maxc2` (max 1 ⇒ `owl:sameAs`) as hand-written restriction rules in `horndb-owlrl`, with unit and integration tests.

**Architecture:** These two rules cannot be expressed in `rules.toml` codegen because they (a) match a typed literal value (`owl:maxCardinality "0"`/`"1"`), (b) use a variable predicate bound from `owl:onProperty`, and (c) `cls-maxc1` derives inconsistency. They join the existing hand-written list-rule family in `src/list_rules.rs`. The literal value (0/1) is recognised at **load time** in `integration.rs` (where the dictionary maps `TermId → lexical form`), producing a `Vec<MaxCardRestriction>` that rides on the `MemStore` and is read back by the semi-naïve firing loop. Inconsistency is materialised the same way every other OWL 2 RL `false`-rule does it here: `?u rdf:type owl:Nothing` (so `Engine::is_consistent()` returns `false`).

**Tech Stack:** Rust (`horndb-owlrl`), the existing `TripleStore`/`MemStore`/`SchemaAxioms`/`Delta` machinery, oxrdf dictionary in `integration.rs`.

---

## Background (read before starting)

W3C OWL 2 RL/RDF rules (Profiles document, Table 8):

- **`cls-maxc1`** — `T(?x, owl:maxCardinality, "0"^^xsd:nonNegativeInteger)`, `T(?x, owl:onProperty, ?p)`, `T(?u, rdf:type, ?x)`, `T(?u, ?p, ?y)` ⇒ `false`. Materialised here as `?u rdf:type owl:Nothing`.
- **`cls-maxc2`** — `T(?x, owl:maxCardinality, "1"^^xsd:nonNegativeInteger)`, `T(?x, owl:onProperty, ?p)`, `T(?u, rdf:type, ?x)`, `T(?u, ?p, ?y1)`, `T(?u, ?p, ?y2)` ⇒ `T(?y1, owl:sameAs, ?y2)`.

Key facts established during research:

- Hand-written rules live in `src/list_rules.rs`; `cax-adc` (also produces `owl:Nothing`) and `cls-int1` are the closest structural models. They are resolved once per `materialize` via `list_rules::resolve(store, vocab)` into a `SchemaAxioms`, then fired in the semi-naïve loop by `list_rules::fire_all`, gated by `SchemaAxioms::body_predicates` against the dirty-predicate set.
- `rules.toml` does **not** list hand-written rules — do not add anything there.
- Inconsistency is represented as `?u rdf:type owl:Nothing`; `Engine::is_consistent()` probes `(None, rdf:type, owl:Nothing)`.
- `owl:maxCardinality` and `owl:onProperty` already exist in `Vocabulary` (`owl_max_cardinality`, `owl_on_property`) and are seeded in `integration.rs::build_vocab`.
- The dictionary key for a typed literal is `"<value>"^^<<datatype-iri>>` (see `integration.rs::intern_literal`), e.g. `"0"^^<http://www.w3.org/2001/XMLSchema#nonNegativeInteger>`.
- No **unqualified** max-cardinality case exists in the synthesised W3C suite (`crates/harness/tests/fixtures/owl2-w3c-rl/`); only the qualified `New-Feature-ObjectQCR-002` (which is `cls-maxqc*`, issue #36, deferred). So this increment adds **no** new `[suites.owl2-w3c-rl]` entry — the "flip green" criterion is vacuously satisfied. `KNOWN-MANIFEST-BUGS.md` is updated to record that unqualified max-cardinality is now implemented and that the only remaining cardinality gap is the qualified family.

## File Structure

- `crates/owlrl/src/types.rs` — add `MaxCardRestriction { class, property, max }` newtype-ish struct.
- `crates/owlrl/src/store.rs` — `MemStore` carries `Vec<MaxCardRestriction>`; `TripleStore` gains a defaulted `card_restrictions()` accessor; `MemStore` gains `set_card_restrictions`.
- `crates/owlrl/src/list_rules.rs` — `SchemaAxioms` gains `max_card_restrictions`; `resolve`/`is_empty`/`body_predicates`/`fire_all` updated; two new `fire_cls_maxc1`/`fire_cls_maxc2` functions; unit tests.
- `crates/owlrl/src/integration.rs` — at load time, classify `owl:maxCardinality` restrictions (robust integer-literal parse via the reverse dictionary), join with `owl:onProperty`, set on the store; integration tests.
- `docs/architecture.md` — flip the SPEC-04 row mentioning cardinality.
- `harness/KNOWN-MANIFEST-BUGS.md` — note unqualified max-cardinality now implemented.

---

### Task 1: `MaxCardRestriction` type

**Files:**
- Modify: `crates/owlrl/src/types.rs`
- Test: `crates/owlrl/src/types.rs` (inline `#[cfg(test)]`)

- [ ] **Step 1: Add the type**

In `crates/owlrl/src/types.rs`, after the `Triple` impl block, add:

```rust
/// A resolved unqualified max-cardinality restriction (`cls-maxc1`/`cls-maxc2`).
///
/// `class` is the restriction class `?x` (`T(?x, owl:maxCardinality, n)` and
/// `T(?x, owl:onProperty, property)`); `max` is the cardinality value, which
/// the rules only act on for `0` and `1`. Resolved at load time in
/// `integration.rs` (where the dictionary can parse the literal value) and
/// fired by `list_rules.rs` in the semi-naïve loop.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct MaxCardRestriction {
    pub class: TermId,
    pub property: TermId,
    pub max: u8,
}
```

- [ ] **Step 2: Add a trivial construction test**

In the `#[cfg(test)] mod tests` block of `types.rs`, add:

```rust
    #[test]
    fn max_card_restriction_fields() {
        let r = MaxCardRestriction {
            class: TermId(1),
            property: TermId(2),
            max: 1,
        };
        assert_eq!(r.max, 1);
        assert_ne!(r.class, r.property);
    }
```

- [ ] **Step 3: Build + test**

Run: `cargo test -p horndb-owlrl --lib types::`
Expected: PASS (the new test plus the existing two).

- [ ] **Step 4: Commit**

```bash
git add crates/owlrl/src/types.rs
git commit -m "feat(owlrl): add MaxCardRestriction type (SPEC-04 cls-maxc)"
```

---

### Task 2: Store carries resolved restrictions

**Files:**
- Modify: `crates/owlrl/src/store.rs`
- Test: `crates/owlrl/src/store.rs` (inline `#[cfg(test)]`)

- [ ] **Step 1: Write the failing test**

In the `#[cfg(test)] mod tests` of `store.rs`, add:

```rust
    #[test]
    fn card_restrictions_round_trip() {
        use crate::types::MaxCardRestriction;
        let mut s = store();
        assert!(s.card_restrictions().is_empty());
        s.set_card_restrictions(vec![MaxCardRestriction {
            class: TermId(1),
            property: TermId(2),
            max: 1,
        }]);
        assert_eq!(s.card_restrictions().len(), 1);
        assert_eq!(s.card_restrictions()[0].max, 1);
    }
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo test -p horndb-owlrl --lib store::tests::card_restrictions_round_trip`
Expected: FAIL to compile — `set_card_restrictions` / `card_restrictions` not found.

- [ ] **Step 3: Add the trait method, the field, and the accessors**

In `store.rs`, add the import at the top (next to the existing `use crate::types::{TermId, Triple};`):

```rust
use crate::types::{MaxCardRestriction, TermId, Triple};
```

Add a defaulted method to the `TripleStore` trait (after `all_triples`):

```rust
    /// Resolved unqualified max-cardinality restrictions (`cls-maxc1`/`cls-maxc2`).
    /// Populated at load time by the embedder (`integration.rs`); empty for
    /// stores built directly without restriction resolution.
    fn card_restrictions(&self) -> &[MaxCardRestriction] {
        &[]
    }
```

Add the field to `MemStore`:

```rust
    /// Resolved max-cardinality restrictions (see `TripleStore::card_restrictions`).
    card_restrictions: Vec<MaxCardRestriction>,
```

Initialise it in `MemStore::new`:

```rust
            card_restrictions: Vec::new(),
```

Add a setter in the inherent `impl MemStore` block (near `assert_all`):

```rust
    /// Set the resolved max-cardinality restrictions. Called once at load
    /// time (`integration.rs`) or directly by tests.
    pub fn set_card_restrictions(&mut self, restrictions: Vec<MaxCardRestriction>) {
        self.card_restrictions = restrictions;
    }
```

Override the trait accessor in `impl TripleStore for MemStore` (after `all_triples`):

```rust
    fn card_restrictions(&self) -> &[MaxCardRestriction] {
        &self.card_restrictions
    }
```

- [ ] **Step 4: Run the test to confirm it passes**

Run: `cargo test -p horndb-owlrl --lib store::tests::card_restrictions_round_trip`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/owlrl/src/store.rs
git commit -m "feat(owlrl): MemStore carries resolved max-card restrictions (SPEC-04 cls-maxc)"
```

---

### Task 3: `cls-maxc1` firing (max 0 ⇒ owl:Nothing)

**Files:**
- Modify: `crates/owlrl/src/list_rules.rs`
- Test: `crates/owlrl/src/list_rules.rs` (inline `#[cfg(test)]`)

- [ ] **Step 1: Write the failing test**

In `list_rules.rs` `#[cfg(test)] mod tests`, add:

```rust
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
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p horndb-owlrl --lib list_rules::tests::cls_maxc1`
Expected: FAIL to compile — `SchemaAxioms` has no `max_card_restrictions`, `resolve` doesn't populate it, `fire_all` doesn't fire it.

- [ ] **Step 3: Wire the restriction list into `SchemaAxioms` + `resolve` + `fire_all`**

Add the field to `struct SchemaAxioms` (in `list_rules.rs`), and import the type. At the top, change the import:

```rust
use crate::types::{MaxCardRestriction, TermId, Triple};
```

Add to `SchemaAxioms`:

```rust
    /// Resolved unqualified max-cardinality restrictions (`cls-maxc1`/`cls-maxc2`).
    /// Resolved at load time (`integration.rs`) and carried on the store; copied
    /// here by `resolve` so they ride the same semi-naïve dirty-prune path.
    pub max_card_restrictions: Vec<MaxCardRestriction>,
```

In `SchemaAxioms::is_empty`, add the conjunct:

```rust
            && self.max_card_restrictions.is_empty()
```

In `SchemaAxioms::body_predicates`, after the `all_different` block, add:

```rust
        // cls-maxc1/cls-maxc2 read rdf:type (for ?u : ?x) and each restricted
        // property (for ?u ?p ?y). Re-fire whenever either becomes dirty.
        if !self.max_card_restrictions.is_empty() {
            s.insert(vocab.rdf_type);
            for r in &self.max_card_restrictions {
                s.insert(r.property);
            }
        }
```

In `resolve`, just before `out` is returned, copy the store's restrictions in:

```rust
    // Max-cardinality restrictions are classified at load time (integration.rs),
    // where the dictionary can parse the literal value; carry them through.
    out.max_card_restrictions = store.card_restrictions().to_vec();

    out
```

In `fire_all`, after the `eq-diff2 / eq-diff3` block and before `out` is returned, add:

```rust
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
```

Add the `fire_cls_maxc1` function (place it after `fire_cax_adc`):

```rust
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
```

(The `fire_cls_maxc2` function is added in Task 4; this task's `fire_all` references it, so add a minimal stub now to compile, then flesh it out in Task 4. Add this stub immediately after `fire_cls_maxc1`:)

```rust
fn fire_cls_maxc2(
    _store: &dyn TripleStore,
    _vocab: &Vocabulary,
    _class: TermId,
    _property: TermId,
    _out: &mut Delta,
) {
    // Implemented in Task 4.
}
```

- [ ] **Step 4: Run the cls-maxc1 tests**

Run: `cargo test -p horndb-owlrl --lib list_rules::tests::cls_maxc1`
Expected: PASS (both `cls_maxc1_*` tests).

- [ ] **Step 5: Run the whole crate to confirm no regressions**

Run: `cargo test -p horndb-owlrl --lib`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/owlrl/src/list_rules.rs
git commit -m "feat(owlrl): cls-maxc1 max-0 inconsistency rule (SPEC-04)"
```

---

### Task 4: `cls-maxc2` firing (max 1 ⇒ owl:sameAs)

**Files:**
- Modify: `crates/owlrl/src/list_rules.rs`
- Test: `crates/owlrl/src/list_rules.rs` (inline `#[cfg(test)]`)

- [ ] **Step 1: Write the failing test**

Add to `list_rules.rs` tests:

```rust
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
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p horndb-owlrl --lib list_rules::tests::cls_maxc2`
Expected: FAIL — the stub derives nothing.

- [ ] **Step 3: Replace the `fire_cls_maxc2` stub with the real implementation**

Replace the stub body added in Task 3 with:

```rust
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
```

- [ ] **Step 4: Run the cls-maxc2 tests**

Run: `cargo test -p horndb-owlrl --lib list_rules::tests::cls_maxc2`
Expected: PASS.

- [ ] **Step 5: Run the whole crate**

Run: `cargo test -p horndb-owlrl --lib`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/owlrl/src/list_rules.rs
git commit -m "feat(owlrl): cls-maxc2 max-1 sameAs rule (SPEC-04)"
```

---

### Task 5: Load-time restriction classification in `integration.rs`

**Files:**
- Modify: `crates/owlrl/src/integration.rs`
- Test: `crates/owlrl/src/integration.rs` (inline `#[cfg(test)]`)

- [ ] **Step 1: Write the failing integration tests**

Add a helper `nq_lit` and two tests to the `#[cfg(test)] mod tests` of `integration.rs`. Put the helper next to the existing `nq` helper:

```rust
    const XSD_NNI: &str = "http://www.w3.org/2001/XMLSchema#nonNegativeInteger";
    const OWL_MAX_CARDINALITY_IRI: &str = "http://www.w3.org/2002/07/owl#maxCardinality";
    const OWL_ON_PROPERTY_IRI: &str = "http://www.w3.org/2002/07/owl#onProperty";

    fn nq_card(s: &str, value: &str) -> Quad {
        Quad::new(
            NamedOrBlankNode::NamedNode(NamedNode::new(s).unwrap()),
            NamedNode::new(OWL_MAX_CARDINALITY_IRI).unwrap(),
            Literal::new_typed_literal(value, NamedNode::new(XSD_NNI).unwrap()),
            GraphName::DefaultGraph,
        )
    }

    fn nq_on_prop(s: &str, p: &str) -> Quad {
        Quad::new(
            NamedOrBlankNode::NamedNode(NamedNode::new(s).unwrap()),
            NamedNode::new(OWL_ON_PROPERTY_IRI).unwrap(),
            NamedNode::new(p).unwrap(),
            GraphName::DefaultGraph,
        )
    }
```

```rust
    #[test]
    fn cls_maxc1_makes_inconsistent_via_engine() {
        let mut engine = Engine::new();
        let mut data = Dataset::new();
        // :R maxCardinality 0 onProperty :p ; :u a :R ; :u :p :y
        data.insert(&nq_card("http://ex/R", "0"));
        data.insert(&nq_on_prop("http://ex/R", "http://ex/p"));
        data.insert(&nq("http://ex/u", RDF_TYPE, "http://ex/R"));
        data.insert(&nq("http://ex/u", "http://ex/p", "http://ex/y"));
        engine.load(&data).unwrap();
        assert!(
            !engine.is_consistent().unwrap(),
            "maxCardinality 0 with a value ⇒ inconsistent (cls-maxc1)"
        );
    }

    #[test]
    fn cls_maxc2_entails_sameas_via_engine() {
        let mut engine = Engine::new();
        let mut data = Dataset::new();
        // :R maxCardinality 1 onProperty :p ; :u a :R ; :u :p :y1 ; :u :p :y2
        data.insert(&nq_card("http://ex/R", "1"));
        data.insert(&nq_on_prop("http://ex/R", "http://ex/p"));
        data.insert(&nq("http://ex/u", RDF_TYPE, "http://ex/R"));
        data.insert(&nq("http://ex/u", "http://ex/p", "http://ex/y1"));
        data.insert(&nq("http://ex/u", "http://ex/p", "http://ex/y2"));
        engine.load(&data).unwrap();
        let mut concl = Dataset::new();
        concl.insert(&nq("http://ex/y1", OWL_SAME_AS, "http://ex/y2"));
        assert!(
            engine.entails(&concl).unwrap(),
            "maxCardinality 1 with two values ⇒ y1 owl:sameAs y2 (cls-maxc2)"
        );
    }

    #[test]
    fn max_cardinality_two_is_ignored() {
        // Only 0 and 1 are acted on; maxCardinality 2 is a no-op in Stage-1.
        let mut engine = Engine::new();
        let mut data = Dataset::new();
        data.insert(&nq_card("http://ex/R", "2"));
        data.insert(&nq_on_prop("http://ex/R", "http://ex/p"));
        data.insert(&nq("http://ex/u", RDF_TYPE, "http://ex/R"));
        data.insert(&nq("http://ex/u", "http://ex/p", "http://ex/y"));
        engine.load(&data).unwrap();
        assert!(engine.is_consistent().unwrap());
    }
```

Also add `Literal` to the test imports — change the test `use` line to:

```rust
    use oxrdf::{BlankNode, GraphName, Literal, NamedNode, NamedOrBlankNode, Quad};
```

(If `Literal` is already imported, leave it.)

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p horndb-owlrl --lib integration::tests::cls_maxc`
Expected: FAIL — restrictions are never resolved, so nothing fires.

- [ ] **Step 3: Classify restrictions at load time**

In `integration.rs`, add a free function (place it near `infer_owl_thing_from_named_individuals`):

```rust
/// Resolve unqualified max-cardinality restrictions for `cls-maxc1`/`cls-maxc2`.
///
/// Scans `?x owl:maxCardinality ?n`, parses the literal value of `?n` (any
/// XSD integer datatype — the OWL 2 RL/RDF rules write
/// `"0"^^xsd:nonNegativeInteger`, but we accept other integer spellings of
/// the same value), and joins with `?x owl:onProperty ?p`. Only values `0`
/// and `1` are retained — higher cardinalities have no OWL 2 RL rule.
///
/// Runs at load time because the literal value is only recoverable from the
/// dictionary (`TermId → lexical key`); the resolved list then rides on the
/// store through `TripleStore::card_restrictions`.
fn resolve_max_card_restrictions(
    store: &MemStore,
    vocab: &Vocabulary,
    dict: &FxHashMap<String, TermId>,
) -> Vec<MaxCardRestriction> {
    // Invert the dictionary once: TermId → lexical key.
    let mut rev: FxHashMap<TermId, &str> = FxHashMap::default();
    for (lex, &id) in dict {
        rev.insert(id, lex.as_str());
    }
    let mut out = Vec::new();
    for card in store.scan_predicate(vocab.owl_max_cardinality) {
        let class = card.s;
        let Some(max) = rev.get(&card.o).and_then(|lex| parse_card_literal(lex)) else {
            continue;
        };
        if max > 1 {
            continue;
        }
        // Join with onProperty (there should be exactly one per restriction).
        for op in store.probe(Some(class), vocab.owl_on_property, None) {
            out.push(MaxCardRestriction {
                class,
                property: op.o,
                max,
            });
        }
    }
    out
}

/// Parse the integer value out of a dictionary literal key of the form
/// `"<value>"^^<<datatype>>` (see `intern_literal`). Returns `None` for
/// non-literals, language-tagged literals, or non-integer lexical values.
fn parse_card_literal(lex: &str) -> Option<u8> {
    let rest = lex.strip_prefix('"')?;
    let close = rest.find("\"^^<")?;
    let value = &rest[..close];
    value.parse::<u8>().ok()
}
```

Add the imports at the top of `integration.rs` (extend the existing `use crate::types::...` line):

```rust
use crate::types::{MaxCardRestriction, TermId, Triple};
```

In `Engine::load`, after the `infer_owl_thing_from_named_individuals(...)` call and after the datatype-axiom injection block (i.e. just before computing `stats`), add:

```rust
        // cls-maxc1/cls-maxc2: classify unqualified max-cardinality
        // restrictions now, while the dictionary can still parse the literal
        // value. The resolved list rides on the store for the firing loop.
        let restrictions = resolve_max_card_restrictions(&state.store, &self.vocab, &state.dict);
        state.store.set_card_restrictions(restrictions);
```

- [ ] **Step 4: Run the integration tests**

Run: `cargo test -p horndb-owlrl --lib integration::tests::cls_maxc`
Expected: PASS.

Run: `cargo test -p horndb-owlrl --lib integration::tests::max_cardinality_two_is_ignored`
Expected: PASS.

- [ ] **Step 5: Run the whole crate**

Run: `cargo test -p horndb-owlrl --lib`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/owlrl/src/integration.rs
git commit -m "feat(owlrl): load-time cls-maxc restriction classification (SPEC-04)"
```

---

### Task 6: `parse_card_literal` unit coverage

**Files:**
- Modify: `crates/owlrl/src/integration.rs` (inline `#[cfg(test)]`)

- [ ] **Step 1: Add focused parser tests**

```rust
    #[test]
    fn parse_card_literal_handles_integer_spellings() {
        assert_eq!(
            super::parse_card_literal(
                "\"0\"^^<http://www.w3.org/2001/XMLSchema#nonNegativeInteger>"
            ),
            Some(0)
        );
        assert_eq!(
            super::parse_card_literal("\"1\"^^<http://www.w3.org/2001/XMLSchema#integer>"),
            Some(1)
        );
        assert_eq!(
            super::parse_card_literal("\"2\"^^<http://www.w3.org/2001/XMLSchema#integer>"),
            Some(2)
        );
        // Not a literal key.
        assert_eq!(super::parse_card_literal("http://ex/x"), None);
        // Language-tagged literal — no `^^<…>` suffix.
        assert_eq!(super::parse_card_literal("\"hi\"@en"), None);
        // Non-integer lexical value.
        assert_eq!(
            super::parse_card_literal("\"x\"^^<http://www.w3.org/2001/XMLSchema#string>"),
            None
        );
    }
```

- [ ] **Step 2: Run**

Run: `cargo test -p horndb-owlrl --lib integration::tests::parse_card_literal`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/owlrl/src/integration.rs
git commit -m "test(owlrl): parse_card_literal datatype-spelling coverage (SPEC-04)"
```

---

### Task 7: Docs sync — architecture.md + KNOWN-MANIFEST-BUGS.md

**Files:**
- Modify: `docs/architecture.md`
- Modify: `harness/KNOWN-MANIFEST-BUGS.md`

> Note: `TASKS.md` is intentionally **not** touched on the feature branch — its `[v]`→`[ ]` release for this epic increment is a locked commit on `main` after merge (per `/next-task` Phase 11).

- [ ] **Step 1: Update `docs/architecture.md`**

In the SPEC-04 table (around line 203), the row currently reads:

```
| Datatype value-space intersection (`I5.8-008/009-pe`), literal-value rules (`dt-eq`/`dt-diff`/`dt-not-type`), `cls-int*` / `cls-uni*` list-walking rules | **deferred** | Intersection narrowing tracked under issue #4; literal-value rules carved out as issue #40. |
```

Add a new row directly **above** it:

```
| Unqualified max-cardinality (`cls-maxc1`/`cls-maxc2`) | **implemented** | Hand-written in `list_rules.rs`; restriction literals (`owl:maxCardinality "0"`/`"1"`) classified at load time in `integration.rs`. `cls-maxc1` → `owl:Nothing` (inconsistency), `cls-maxc2` → `owl:sameAs`. Qualified `cls-maxqc1..4` remain deferred ([#36](https://github.com/sunstoneinstitute/horndb/issues/36)). |
```

- [ ] **Step 2: Update `harness/KNOWN-MANIFEST-BUGS.md`**

In the capability table, the qualified-cardinality row currently reads:

```
| `cls-maxqc1..4` (qualified cardinality, `ObjectQCR-002-pe`) | 1 |
```

Leave that row as-is (it is still deferred), but add a short note under the table (after the "Total: **19 cases**." line) recording the new state:

```

> **2026-06-16 — unqualified max-cardinality implemented (`cls-maxc1`/`cls-maxc2`, issue #35).**
> No W3C case in the synthesised `owl2-w3c-rl` suite is gated on *unqualified*
> max-cardinality (the only cardinality case, `New-Feature-ObjectQCR-002`, is
> *qualified* — `owl:maxQualifiedCardinality` + `owl:onClass` — and remains
> blocked on `cls-maxqc1..4`). So this batch adds no `selected.toml` entry; the
> rules are covered by unit + integration tests in `crates/owlrl`. The total
> above is unchanged.
```

- [ ] **Step 3: Commit**

```bash
git add docs/architecture.md harness/KNOWN-MANIFEST-BUGS.md
git commit -m "docs(owlrl): record cls-maxc1/cls-maxc2 as implemented (SPEC-04)"
```

---

### Task 8: Full verification gate

**Files:** none (verification only)

- [ ] **Step 1: Format**

Run: `cargo fmt --all`
Then: `cargo fmt --all -- --check`
Expected: clean.

- [ ] **Step 2: Clippy (workspace, all targets)**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings. (First run rebuilds rocksdb via the harness — slow but expected.)

- [ ] **Step 3: Tests (workspace)**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 4: Inspect that no new `rules.toml` rule was accidentally added**

Run: `cargo run -p horndb-owlrl --bin show-rule -- --list | grep -i maxc || echo "no compiled maxc rule (expected — hand-written)"`
Expected: prints the "no compiled maxc rule" line (cls-maxc is hand-written, not in `rules.toml`).

- [ ] **Step 5: Commit any formatting-only changes**

```bash
git add -A
git commit -m "style(owlrl): cargo fmt after cls-maxc (SPEC-04)" || echo "nothing to format"
```

---

## Self-Review notes

- **Spec coverage:** `cls-maxc1` (Task 3), `cls-maxc2` (Task 4), load-time literal classification (Task 5), unit + integration + parser tests (Tasks 1–6), harness selection note + KNOWN-MANIFEST-BUGS (Task 7). The issue's "flip green any gated W3C case" criterion is vacuously satisfied (no unqualified case exists) and documented (Task 7).
- **Type consistency:** `MaxCardRestriction { class, property, max }` defined in Task 1 is used identically in store (Task 2), list_rules (Tasks 3–4), and integration (Task 5). `fire_cls_maxc1`/`fire_cls_maxc2` signatures match between their definition and the `fire_all` call site.
- **No placeholders:** every step carries the literal code/commands.
