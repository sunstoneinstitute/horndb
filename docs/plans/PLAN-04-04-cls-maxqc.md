---
status: executed
date: 2026-06-16
scope: "cls-maxqc1–cls-maxqc4 (Qualified Max-Cardinality)"
---

# cls-maxqc1–cls-maxqc4 (Qualified Max-Cardinality) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the four OWL 2 RL qualified max-cardinality class rules (`cls-maxqc1`–`cls-maxqc4`, W3C Table 7) in `horndb-owlrl`, with unit + integration test coverage.

**Architecture:** Follows the existing unqualified `cls-maxc1`/`cls-maxc2` pattern exactly: a `QualMaxCardRestriction` is resolved once at load time in `integration.rs` (where the dictionary can parse the literal cardinality value), carried on the `MemStore`, and fired in the semi-naïve loop by `list_rules.rs`. The `owl:onClass` filler distinguishes the four rules; `filler == owl:Thing` means "count every value" (maxqc2/maxqc4), otherwise only `?y rdf:type ?filler` values count (maxqc1/maxqc3). `max == 0` → inconsistency (`?u rdf:type owl:Nothing`); `max == 1` → `owl:sameAs` over distinct qualifying pairs.

**Tech Stack:** Rust 1.90, `horndb-owlrl` crate, `rustc_hash`, `smallvec`.

---

## Scope note — ObjectQCR-002-pe does NOT flip green

Issue #36's "Done when" says the `New-Feature-ObjectQCR-002-pe` W3C case flips
green and joins `harness/selected.toml`. **It cannot, and this plan deliberately
does not attempt it.** The conclusion graph asserts `Stewie a [owl:complementOf
Woman]` — a contrapositive derivation that requires generating a *fresh*
`owl:complementOf` partner class (a tuple-generating dependency). OWL 2 RL
disclaims TGDs, and `cls-maxqc1..4` only emit `owl:sameAs` / `owl:Nothing`. This
is the identical blocker already documented for `DisjointClasses-001/003-pe`.

This mirrors how unqualified increment #35 was handled: the rules land with
unit/integration coverage, but no `selected.toml` entry is added because no W3C
case in the synthesised `owl2-w3c-rl` suite is gated *purely* on these rules.
The doc deliverable is therefore to **reclassify** `ObjectQCR-002-pe` in
`KNOWN-MANIFEST-BUGS.md` from the "cls-maxqc1..4" bucket into the existing
"fresh-bnode complementOf generation" bucket — not to flip it green.

---

## File structure

- `crates/owlrl/src/types.rs` — **already done:** `QualMaxCardRestriction` struct.
- `crates/owlrl/src/vocab.rs` — **already done:** `owl_max_qualified_cardinality`, `owl_on_class`.
- `crates/owlrl/src/store.rs` — add `qual_card_restrictions()` trait method (default `&[]`) + `MemStore` storage + `set_qual_card_restrictions()`.
- `crates/owlrl/src/list_rules.rs` — carry `qual_max_card_restrictions` in `SchemaAxioms`, advertise body predicates, fire `cls-maxqc1..4`, unit tests.
- `crates/owlrl/src/integration.rs` — resolve qualified restrictions at load time (parse literal, join onProperty + onClass), seed new vocab IRIs, integration tests.
- `crates/owlrl/KNOWN-MANIFEST-BUGS.md` (symlink → `harness/KNOWN-MANIFEST-BUGS.md`) — reclassify ObjectQCR-002-pe.
- `docs/architecture.md` — flip SPEC-04 cls-maxqc status.

---

### Task 1: Store trait method for qualified restrictions

**Files:**
- Modify: `crates/owlrl/src/store.rs`

- [ ] **Step 1: Add trait method (default empty) to `TripleStore`**

In `crates/owlrl/src/store.rs`, after the existing `card_restrictions` default method:

```rust
    /// Resolved qualified max-cardinality restrictions (`cls-maxqc1`–`cls-maxqc4`).
    /// Populated at load time by the embedder (`integration.rs`); empty for
    /// stores built directly without restriction resolution.
    fn qual_card_restrictions(&self) -> &[crate::types::QualMaxCardRestriction] {
        &[]
    }
```

Update the `use` line to import `QualMaxCardRestriction`:
`use crate::types::{MaxCardRestriction, QualMaxCardRestriction, TermId, Triple};`

- [ ] **Step 2: Add field + setter + impl to `MemStore`**

Add field to the `MemStore` struct:

```rust
    /// Resolved qualified max-cardinality restrictions (see `TripleStore::qual_card_restrictions`).
    qual_card_restrictions: Vec<QualMaxCardRestriction>,
```

Initialise it in `MemStore::new` (`qual_card_restrictions: Vec::new(),`). Add a setter next to `set_card_restrictions`:

```rust
    /// Set the resolved qualified max-cardinality restrictions. Called once at
    /// load time (`integration.rs`) or directly by tests.
    pub fn set_qual_card_restrictions(&mut self, restrictions: Vec<QualMaxCardRestriction>) {
        self.qual_card_restrictions = restrictions;
    }
```

Implement the trait method on `MemStore` next to `card_restrictions`:

```rust
    fn qual_card_restrictions(&self) -> &[QualMaxCardRestriction] {
        &self.qual_card_restrictions
    }
```

- [ ] **Step 3: Round-trip unit test**

Add to `store.rs` `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn qual_card_restrictions_round_trip() {
        use crate::types::QualMaxCardRestriction;
        let mut s = store();
        assert!(s.qual_card_restrictions().is_empty());
        s.set_qual_card_restrictions(vec![QualMaxCardRestriction {
            class: TermId(1),
            property: TermId(2),
            filler: TermId(3),
            max: 1,
        }]);
        assert_eq!(s.qual_card_restrictions().len(), 1);
        assert_eq!(s.qual_card_restrictions()[0].filler, TermId(3));
    }
```

- [ ] **Step 4: Build + test**

Run: `cargo test -p horndb-owlrl --lib store::`
Expected: PASS (including new round-trip test).

- [ ] **Step 5: Commit**

```bash
git add crates/owlrl/src/store.rs
git commit -m "feat(owlrl): store qualified max-cardinality restrictions (SPEC-04 #36)"
```

---

### Task 2: Fire cls-maxqc1–cls-maxqc4 in list_rules.rs

**Files:**
- Modify: `crates/owlrl/src/list_rules.rs`

- [ ] **Step 1: Carry restrictions in `SchemaAxioms`**

Add field to `SchemaAxioms` (after `max_card_restrictions`):

```rust
    /// Resolved qualified max-cardinality restrictions (`cls-maxqc1`–`cls-maxqc4`).
    /// Resolved at load time (`integration.rs`); copied here by `resolve` so they
    /// ride the same semi-naïve dirty-prune path.
    pub qual_max_card_restrictions: Vec<crate::types::QualMaxCardRestriction>,
```

Update `import`: change `use crate::types::{MaxCardRestriction, TermId, Triple};` to
`use crate::types::{MaxCardRestriction, QualMaxCardRestriction, TermId, Triple};`.

- [ ] **Step 2: Advertise body predicates + extend `is_empty`**

In `body_predicates`, after the unqualified `max_card_restrictions` block:

```rust
        // cls-maxqc1..4 read rdf:type (for ?u : ?x and ?y : ?filler) and each
        // restricted property (for ?u ?p ?y). Re-fire whenever any becomes dirty.
        if !self.qual_max_card_restrictions.is_empty() {
            s.insert(vocab.rdf_type);
            for r in &self.qual_max_card_restrictions {
                s.insert(r.property);
            }
        }
```

In `is_empty`, add `&& self.qual_max_card_restrictions.is_empty()` to the chain.

- [ ] **Step 3: Resolve onto axioms**

In `resolve`, next to `out.max_card_restrictions = store.card_restrictions().to_vec();`:

```rust
    out.qual_max_card_restrictions = store.qual_card_restrictions().to_vec();
```

- [ ] **Step 4: Fire in `fire_all`**

After the unqualified `cls-maxc` block in `fire_all`, add a parallel block. Note
the dirty gate: fire when `rdf:type` is dirty OR any restricted property is dirty
(a new `?y rdf:type ?filler` or a new `?u ?p ?y` can both expose a match):

```rust
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
```

- [ ] **Step 5: Implement the two fire functions**

Add at the end of the firing-function section (before `#[cfg(test)]`). The
`qualifies` helper centralises the `owl:Thing`-filler ("any value") vs
class-filler distinction:

```rust
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
    for u in us {
        let has_qualifying_value = store
            .probe(Some(u), r.property, None)
            .any(|t| qualifies(store, vocab, t.o, r.filler));
        if has_qualifying_value {
            let head = Triple::new(u, vocab.rdf_type, vocab.owl_nothing);
            if !out.contains(&head) && !store.contains(&head) {
                out.insert(
                    head,
                    Provenance {
                        rule_id: "cls-maxqc1",
                        premises: smallvec![],
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
                        out.insert(
                            head,
                            Provenance {
                                rule_id: "cls-maxqc3",
                                premises: smallvec![],
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
```

- [ ] **Step 6: Unit tests**

Add to `list_rules.rs` `#[cfg(test)] mod tests`. Cover: maxqc1 (class filler, qualifying value ⇒ Nothing), maxqc1 negative (value not of filler class ⇒ consistent), maxqc3 (class filler, two qualifying values ⇒ sameAs), maxqc3 negative (only one qualifies ⇒ no sameAs), maxqc4 (Thing filler ⇒ sameAs regardless of type):

```rust
    #[test]
    fn cls_maxqc_zero_class_filler_violation() {
        use crate::types::QualMaxCardRestriction;
        let (mut s, v) = fresh();
        let x = TermId(50); // restriction class
        let p = TermId(51); // onProperty
        let c = TermId(52); // onClass filler
        let u = TermId(100);
        let y = TermId(101);
        s.assert(t(u.0, v.rdf_type.0, x.0));
        s.assert(t(u.0, p.0, y.0));
        s.assert(t(y.0, v.rdf_type.0, c.0)); // y IS of filler class
        s.set_qual_card_restrictions(vec![QualMaxCardRestriction {
            class: x,
            property: p,
            filler: c,
            max: 0,
        }]);
        let ax = resolve(&s, &v);
        let d = fire_all(&s, &ax, &v, None);
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
        s.assert(t(u.0, p.0, y.0)); // y is NOT typed as c
        s.set_qual_card_restrictions(vec![QualMaxCardRestriction {
            class: x,
            property: p,
            filler: c,
            max: 0,
        }]);
        let ax = resolve(&s, &v);
        let d = fire_all(&s, &ax, &v, None);
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
        let d = fire_all(&s, &ax, &v, None);
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
        s.assert(t(y1.0, v.rdf_type.0, c.0)); // only y1 is of filler class
        s.set_qual_card_restrictions(vec![QualMaxCardRestriction {
            class: x,
            property: p,
            filler: c,
            max: 1,
        }]);
        let ax = resolve(&s, &v);
        let d = fire_all(&s, &ax, &v, None);
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
        // No rdf:type on y1/y2 — owl:Thing filler counts every value.
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
        let d = fire_all(&s, &ax, &v, None);
        assert!(
            d.contains(&t(y1.0, v.owl_same_as.0, y2.0))
                || d.contains(&t(y2.0, v.owl_same_as.0, y1.0)),
            "cls-maxqc4: owl:Thing filler ⇒ sameAs over any two values"
        );
    }
```

- [ ] **Step 7: Build + test**

Run: `cargo test -p horndb-owlrl --lib list_rules::`
Expected: PASS (all new maxqc tests green).

- [ ] **Step 8: Commit**

```bash
git add crates/owlrl/src/list_rules.rs
git commit -m "feat(owlrl): fire cls-maxqc1-4 qualified max-cardinality (SPEC-04 #36)"
```

---

### Task 3: Resolve qualified restrictions at load time

**Files:**
- Modify: `crates/owlrl/src/integration.rs`

- [ ] **Step 1: Seed the new vocab IRIs**

Find the `const OWL_…: &str` block and the `build_vocab` helper (search for
`owl_max_cardinality` and `OWL_MAX_CARDINALITY`). Add IRI constants:

```rust
const OWL_MAX_QUALIFIED_CARDINALITY: &str =
    "http://www.w3.org/2002/07/owl#maxQualifiedCardinality";
const OWL_ON_CLASS: &str = "http://www.w3.org/2002/07/owl#onClass";
```

And in the `build_vocab` allocation block (where `owl_max_cardinality: alloc(OWL_MAX_CARDINALITY, …)` lives), add matching lines:

```rust
        owl_max_qualified_cardinality: alloc(OWL_MAX_QUALIFIED_CARDINALITY, &mut id, &mut dict),
        owl_on_class: alloc(OWL_ON_CLASS, &mut id, &mut dict),
```

(Match the exact arg shape of the surrounding `alloc(...)` calls — confirm by reading the block first.)

- [ ] **Step 2: Resolution helper**

Add next to `resolve_max_card_restrictions`:

```rust
/// Resolve qualified max-cardinality restrictions for `cls-maxqc1`–`cls-maxqc4`.
///
/// Scans `?x owl:maxQualifiedCardinality ?n`, parses the literal value (reusing
/// `parse_card_literal`; only `0` and `1` have OWL 2 RL rules), then joins with
/// `?x owl:onProperty ?p` and `?x owl:onClass ?c`. The `owl:Thing` filler
/// (cls-maxqc2/maxqc4) is carried through as `filler == vocab.owl_thing`.
fn resolve_qual_max_card_restrictions(
    store: &MemStore,
    vocab: &Vocabulary,
    dict: &FxHashMap<String, TermId>,
) -> Vec<QualMaxCardRestriction> {
    let mut rev: FxHashMap<TermId, &str> = FxHashMap::default();
    for (lex, &id) in dict {
        rev.insert(id, lex.as_str());
    }
    let mut out = Vec::new();
    for card in store.scan_predicate(vocab.owl_max_qualified_cardinality) {
        let class = card.s;
        let Some(max) = rev.get(&card.o).and_then(|lex| parse_card_literal(lex)) else {
            continue;
        };
        if max > 1 {
            continue;
        }
        for op in store.probe(Some(class), vocab.owl_on_property, None) {
            for oc in store.probe(Some(class), vocab.owl_on_class, None) {
                out.push(QualMaxCardRestriction {
                    class,
                    property: op.o,
                    filler: oc.o,
                    max,
                });
            }
        }
    }
    out
}
```

Add `QualMaxCardRestriction` to the `crate::types` import at the top of `integration.rs`.

- [ ] **Step 3: Wire into `load`**

Next to the existing `let restrictions = resolve_max_card_restrictions(...)` /
`state.store.set_card_restrictions(restrictions);` lines:

```rust
        let qual_restrictions =
            resolve_qual_max_card_restrictions(&state.store, &self.vocab, &state.dict);
        state.store.set_qual_card_restrictions(qual_restrictions);
```

- [ ] **Step 4: Integration tests via the Engine façade**

Add to `integration.rs` `#[cfg(test)] mod tests`. Build premise Turtle/N-Triples
matching the existing `cls_maxc*_via_engine` tests' style. Cover one
inconsistency (maxqc1, max 0) and one sameAs entailment (maxqc3, max 1). Example
(adapt the dataset-construction helper to the one those tests already use):

```rust
    #[test]
    fn cls_maxqc3_entails_sameas_via_engine() {
        // x: maxQualifiedCardinality 1 on p, onClass c.
        // u : x, u p y1, u p y2, y1 : c, y2 : c  ⇒  y1 owl:sameAs y2.
        let ttl = r#"
            @prefix owl: <http://www.w3.org/2002/07/owl#> .
            @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
            @prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
            @prefix ex: <http://ex/> .
            ex:x a owl:Restriction ;
                 owl:onProperty ex:p ;
                 owl:maxQualifiedCardinality "1"^^xsd:nonNegativeInteger ;
                 owl:onClass ex:c .
            ex:u a ex:x ; ex:p ex:y1, ex:y2 .
            ex:y1 a ex:c .
            ex:y2 a ex:c .
        "#;
        let mut engine = Engine::new();
        engine.load(&parse_ttl(ttl)).unwrap();
        let concl = parse_ttl("@prefix owl: <http://www.w3.org/2002/07/owl#> . @prefix ex: <http://ex/> . ex:y1 owl:sameAs ex:y2 .");
        assert!(
            engine.entails(&concl).unwrap(),
            "maxQualifiedCardinality 1 with two filler-typed values ⇒ y1 sameAs y2 (cls-maxqc3)"
        );
    }

    #[test]
    fn cls_maxqc1_makes_inconsistent_via_engine() {
        let ttl = r#"
            @prefix owl: <http://www.w3.org/2002/07/owl#> .
            @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
            @prefix ex: <http://ex/> .
            ex:x a owl:Restriction ;
                 owl:onProperty ex:p ;
                 owl:maxQualifiedCardinality "0"^^xsd:nonNegativeInteger ;
                 owl:onClass ex:c .
            ex:u a ex:x ; ex:p ex:y .
            ex:y a ex:c .
        "#;
        let mut engine = Engine::new();
        engine.load(&parse_ttl(ttl)).unwrap();
        assert!(
            !engine.is_consistent().unwrap(),
            "maxQualifiedCardinality 0 with a filler-typed value ⇒ inconsistent (cls-maxqc1)"
        );
    }
```

> If `integration.rs` tests build datasets with a different helper than a
> `parse_ttl`, match the existing convention (read the nearby `cls_maxc*` tests
> first and reuse their exact dataset-construction approach).

- [ ] **Step 5: Build + test**

Run: `cargo test -p horndb-owlrl`
Expected: PASS (all crate tests, including the two new integration tests).

- [ ] **Step 6: Commit**

```bash
git add crates/owlrl/src/integration.rs
git commit -m "feat(owlrl): resolve qualified max-cardinality restrictions at load (SPEC-04 #36)"
```

---

### Task 4: Reclassify ObjectQCR-002-pe + flip architecture status

**Files:**
- Modify: `harness/KNOWN-MANIFEST-BUGS.md` (the `crates/owlrl/KNOWN-MANIFEST-BUGS.md` symlink follows it)
- Modify: `docs/architecture.md`

- [ ] **Step 1: Reclassify in KNOWN-MANIFEST-BUGS.md**

Remove the standalone `### Object qualified cardinality (cls-maxqc1..4)` section
(and its `#New-Feature-ObjectQCR-002-pe` entry). Move `ObjectQCR-002-pe` into the
existing `### Fresh-bnode generation of owl:complementOf partner classes`
section, with a one-line note: the conclusion asserts `Stewie a [owl:complementOf
Woman]`, a contrapositive derivation needing a fresh complement class (TGD) —
`cls-maxqc1..4` are now implemented but only emit `owl:sameAs`/`owl:Nothing`, so
this case stays red on the fresh-bnode gap. Update the summary table row +
total-count prose accordingly (the cls-maxqc row goes away; the fresh-bnode
complementOf row's count goes from 2 → 3; grand total stays 19). Add a dated note
in the style of the existing `2026-06-16 — unqualified max-cardinality` note
recording that cls-maxqc1..4 landed (issue #36) and why no `selected.toml` entry
is added.

- [ ] **Step 2: Flip architecture.md status**

In `docs/architecture.md`, find the SPEC-04 row/line covering qualified
max-cardinality (or the cls-max* cardinality entry) and update its **Status** to
reflect cls-maxqc1–4 implemented (planned → implemented). Match the surrounding
table/line format exactly.

- [ ] **Step 3: Verify docs consistency**

Run: `grep -rn "maxqc\|ObjectQCR" harness/KNOWN-MANIFEST-BUGS.md docs/architecture.md`
Expected: ObjectQCR-002-pe appears only under the fresh-bnode complementOf
bucket; architecture shows cls-maxqc implemented.

- [ ] **Step 4: Commit**

```bash
git add harness/KNOWN-MANIFEST-BUGS.md docs/architecture.md
git commit -m "docs(owlrl): record cls-maxqc1-4; reclassify ObjectQCR-002-pe (SPEC-04 #36)"
```

---

## Final verification (Phase 6 gates)

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
# Confirm the harness still passes (and ObjectQCR-002-pe is NOT spuriously
# added/expected). Real-engine owl2-w3c-rl run should be unchanged in pass count:
cargo run -p horndb-harness --bin harness --features real-engine -- --engine owlrl run
```

All must be green. The cls-maxqc rules are gated by the new unit + integration
tests (no new W3C selection entry, per the scope note above).

---

## Self-review

- **Spec coverage:** All four rules — maxqc1 (max 0, class), maxqc2 (max 0,
  Thing), maxqc3 (max 1, class), maxqc4 (max 1, Thing) — are covered by the two
  fire functions keyed on `max` × the `qualifies` Thing/class split. Unit tests
  cover class-filler zero (+negative), class-filler one (+negative), and
  Thing-filler one. ✓
- **Placeholders:** none — all code shown. The integration-test dataset helper is
  the one caveat (match existing convention); flagged explicitly. ✓
- **Type consistency:** `QualMaxCardRestriction { class, property, filler, max }`
  used identically across types.rs, store.rs, list_rules.rs, integration.rs;
  `qualifies`/`fire_cls_maxqc_zero`/`fire_cls_maxqc_one` signatures consistent. ✓
