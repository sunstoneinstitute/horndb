# SPEC-04 Proof Recording (F4) + `proof(t)` API Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver SPEC-04 **F4** / acceptance criterion #5: every inferred triple records real premises, and a new `proof(t)` API returns a proof tree whose leaves are asserted triples and whose internal nodes are rule applications.

**Architecture:** Two halves. (1) A `ProofTree` recursive type plus a `MemStore::proof_tree` builder that walks the per-triple single-level `Provenance` already stored in `MemStore::proofs`, recursing into each premise until it reaches asserted triples (no proof entry = leaf), with a path-visited set to cut derivation cycles (e.g. `eq-sym` ↔ `eq-sym`). (2) Close the premise-recording gap: the 12 hand-written list rules in `list_rules.rs` currently record `premises: smallvec![]`; thread real body triples through each firing site. Compiled rules (`emit.rs`), the reference closure backend (`backend.rs`), and `eq_rep_p_opt.rs` already record real premises.

**Tech Stack:** Rust 1.90, `horndb-owlrl` crate, `smallvec`, `rustc_hash`. Reference backend `RuleFiringBackend` (records real premises) is used by all proof-tree tests.

---

## Background facts (verified against the worktree)

- `Provenance { rule_id: RuleId, premises: SmallVec<[Triple; 4]> }` — `src/provenance.rs`. `RuleId = &'static str` (`src/types.rs:76`).
- `MemStore` (`src/store.rs`) stores `proofs: FxHashMap<Triple, Provenance>` for **inferred** triples only; asserted triples have **no** proof entry. `MemStore::proof(t) -> Option<&Provenance>` exists (single level).
- Premise recording already correct in: generated rules (`codegen/emit.rs:318-320`), `src/backend.rs` (eq-trans/prp-trp etc. record 1–2 premises), `src/eq_rep_p_opt.rs` (records 2). `src/datatypes.rs` injects via `store.assert(...)` (base facts = correct leaves). `src/graphblas_backend.rs` records empty premises **best-effort by design** (documented at its top) — out of scope here.
- **Gap:** `src/list_rules.rs` records `premises: smallvec![]` at 12 sites. List structure is resolved away at load time into `SchemaAxioms` (`Vec<TermId>`), discarding the originating list-head term.
- `Engine` (`src/integration.rs`) owns `state.store: MemStore` and `state.dict: FxHashMap<String, TermId>`. Decoding pattern to mirror: `materialized_triples` (`src/integration.rs:280-302`) inverts the dict `TermId -> &str`.

---

## File Structure

- `src/provenance.rs` — add `ProofTree` enum + its module doc. (Modify)
- `src/store.rs` — add `MemStore::proof_tree` inherent method + unit tests. (Modify)
- `src/list_rules.rs` — extend `SchemaAxioms` to carry originating axiom triples for the schema-only rules; thread real premises through all 12 firing sites; add premise-assertion tests. (Modify)
- `src/integration.rs` — add `Engine::proof` returning a String-decoded proof tree + test. (Modify)
- `tests/proof_tree.rs` — new integration test file for multi-step + NF4 depth-≤10 proofs. (Create)
- `crates/owlrl/AGENTS.md` (§7) and `crates/owlrl/INTEGRATION-NOTES.md` — document the proof API + premise policy. (Modify)
- `docs/architecture.md` — flip the SPEC-04 proof-recording Status. (Modify)

---

## Premise policy (the decision the executor must follow)

For every derived triple, premises = the rule's body atoms that are **bound to concrete triples available at the firing site**:

- **Instance-level body triples** are always recorded (they exist in local scope at each site).
- **The originating list/axiom triple** (`?c owl:intersectionOf ?listhead`, `?ad owl:members ?listhead`, etc.) is recorded for the rules whose *only* antecedent is the schema list (`scm-int`, `eq-diff2/3`) so their proof nodes are non-empty and bottom out at the asserted axiom; and is included for the other list rules where the list head is cheaply available.
- **Restriction-declaration side conditions** (`?x owl:maxCardinality "n"`, `?x owl:onProperty ?p`, `?x owl:onClass ?c`) are an asserted schema side-condition. `MaxCardRestriction`/`QualMaxCardRestriction` do **not** carry the restriction-class node today, so for `cls-maxc1/2` and `cls-maxqc1-4` record the **instance-level** body triples only (`?u rdf:type ?class`, `?u ?p ?y`), and document that the restriction declaration is an elided side condition (a follow-up may carry the restriction node). This keeps the instance proof tree complete (leaves bottom out at asserted instance data).

Rationale to record in code/notes: the proof-tree property required by acceptance #5 is "leaves are asserted triples." Instance premises already satisfy that for the data rules; the originating axiom triple is needed only for the schema-only rules. This matches RDFox-style instance-derivation explanations.

---

### Task 1: `ProofTree` type + `MemStore::proof_tree` builder

**Files:**
- Modify: `crates/owlrl/src/provenance.rs`
- Modify: `crates/owlrl/src/store.rs`

- [ ] **Step 1: Write the failing test** (append to the `#[cfg(test)] mod tests` in `crates/owlrl/src/store.rs`)

```rust
    #[test]
    fn proof_tree_bottoms_out_at_asserted() {
        // a -p-> b asserted; rule "r1" derives c1 from [a p b];
        // rule "r2" derives c2 from [c1].  proof_tree(c2) =
        //   Derived c2 <- r2 [ Derived c1 <- r1 [ Asserted(a p b) ] ]
        let mut s = store();
        let ab = t(1, 2, 3);
        let c1 = t(1, 4, 5);
        let c2 = t(1, 6, 7);
        s.assert(ab);
        s.insert_inferred(c1, Provenance::new("r1", [ab]));
        s.insert_inferred(c2, Provenance::new("r2", [c1]));

        let tree = s.proof_tree(&c2);
        match tree {
            ProofTree::Derived { triple, rule_id, premises } => {
                assert_eq!(triple, c2);
                assert_eq!(rule_id, "r2");
                assert_eq!(premises.len(), 1);
                match &premises[0] {
                    ProofTree::Derived { triple, rule_id, premises } => {
                        assert_eq!(*triple, c1);
                        assert_eq!(*rule_id, "r1");
                        assert_eq!(premises.len(), 1);
                        assert_eq!(premises[0], ProofTree::Asserted(ab));
                    }
                    other => panic!("expected Derived c1, got {other:?}"),
                }
            }
            other => panic!("expected Derived c2, got {other:?}"),
        }
        // An asserted triple is a leaf.
        assert_eq!(s.proof_tree(&ab), ProofTree::Asserted(ab));
    }

    #[test]
    fn proof_tree_cuts_cycles() {
        // eq-sym-style mutual derivation: x derived from y and y derived from x.
        let mut s = store();
        let x = t(1, 2, 3);
        let y = t(3, 2, 1);
        s.insert_inferred(x, Provenance::new("eq-sym", [y]));
        s.insert_inferred(y, Provenance::new("eq-sym", [x]));
        let tree = s.proof_tree(&x);
        // Must terminate; the back-edge to x appears as a Cycle leaf.
        match tree {
            ProofTree::Derived { premises, .. } => match &premises[0] {
                ProofTree::Derived { premises, .. } => {
                    assert_eq!(premises[0], ProofTree::Cycle(x));
                }
                other => panic!("expected nested Derived, got {other:?}"),
            },
            other => panic!("expected Derived, got {other:?}"),
        }
    }
```

Add `use crate::provenance::{Provenance, ProofTree};` to the test module if needed (the module already imports `Provenance`; extend it).

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p horndb-owlrl --lib proof_tree`
Expected: FAIL — `ProofTree` not found / `proof_tree` method missing.

- [ ] **Step 3: Add the `ProofTree` type** to `crates/owlrl/src/provenance.rs`

```rust
/// A proof tree for a triple in the materialised store (SPEC-04 F4,
/// acceptance #5). Leaves are asserted (base) triples; internal nodes are
/// rule applications deriving the triple from its premises.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum ProofTree {
    /// A base (asserted) triple, or a triple with no recorded derivation —
    /// a leaf.
    Asserted(Triple),
    /// A derived triple: the rule that produced it and the proofs of its
    /// premises. `premises` is empty only for derivations whose backend
    /// records premises best-effort (e.g. the GraphBLAS closure backend).
    Derived {
        triple: Triple,
        rule_id: RuleId,
        premises: Vec<ProofTree>,
    },
    /// The triple is already being expanded higher in this branch — a
    /// derivation cycle (e.g. `eq-sym` ↔ `eq-sym`). Cut to keep the tree
    /// finite; the full single-level proof is still retrievable via
    /// [`crate::store::MemStore::proof`].
    Cycle(Triple),
}
```

- [ ] **Step 4: Add the builder** to `crates/owlrl/src/store.rs` (inherent `impl MemStore`, near the existing `proof` method). Add `ProofTree` to the `use crate::provenance::...` line at the top of the file.

```rust
    /// Build the full proof tree for `t` (SPEC-04 F4, acceptance #5).
    ///
    /// Recurses through the single-level [`Provenance`] recorded for each
    /// inferred triple. A triple with no proof entry is treated as asserted
    /// (a leaf). Derivation cycles are cut with a [`ProofTree::Cycle`] leaf.
    pub fn proof_tree(&self, t: &Triple) -> ProofTree {
        let mut path = FxHashSet::default();
        self.proof_tree_inner(t, &mut path)
    }

    fn proof_tree_inner(&self, t: &Triple, path: &mut FxHashSet<Triple>) -> ProofTree {
        let Some(prov) = self.proofs.get(t) else {
            // No recorded derivation -> asserted base fact (leaf).
            return ProofTree::Asserted(*t);
        };
        if !path.insert(*t) {
            // Already on the current branch: cut the cycle.
            return ProofTree::Cycle(*t);
        }
        let premises = prov
            .premises
            .iter()
            .map(|p| self.proof_tree_inner(p, path))
            .collect();
        path.remove(t);
        ProofTree::Derived {
            triple: *t,
            rule_id: prov.rule_id,
            premises,
        }
    }
```

Ensure `ProofTree` is re-exported: add `pub use provenance::ProofTree;` (or include it in an existing `pub use provenance::...`) in `crates/owlrl/src/lib.rs` next to the other public re-exports.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p horndb-owlrl --lib proof_tree`
Expected: PASS (both tests).

- [ ] **Step 6: Commit**

```bash
git add crates/owlrl/src/provenance.rs crates/owlrl/src/store.rs crates/owlrl/src/lib.rs
git commit -m "feat(owlrl): ProofTree type + MemStore::proof_tree builder (SPEC-04 F4)"
```

---

### Task 2: Record real premises in the 12 list-rule firing sites

**Files:**
- Modify: `crates/owlrl/src/list_rules.rs`

The originating list head is discarded by `resolve`. Carry it so the schema-only rules can name their axiom triple. Minimal change: extend the `SchemaAxioms` vecs that feed schema-only or list rules to carry the originating `(subject, list_head)` where needed, and record instance premises everywhere.

- [ ] **Step 1: Write failing premise-assertion tests** — new `#[cfg(test)] mod premise_tests` appended to `crates/owlrl/src/list_rules.rs`. Cover the representative rules. Use `MemStore` + `resolve` + the public `fire_all` path, then read `store.proof(&head).premises`.

```rust
#[cfg(test)]
mod premise_tests {
    use super::*;
    use crate::store::MemStore;

    fn ids(base: u64, n: u64) -> Vec<TermId> {
        (0..n).map(|i| TermId(base + i)).collect()
    }

    // cls-int1: x:c derived from [x:c1, x:c2] (plus the intersection axiom).
    #[test]
    fn cls_int1_records_instance_premises() {
        let v = Vocabulary::synthetic(1);
        let mut store = MemStore::new(v.clone());
        let c = TermId(100);
        let c1 = TermId(101);
        let c2 = TermId(102);
        let x = TermId(200);
        // c owl:intersectionOf (c1 c2) — assert a 2-element rdf:List.
        let l0 = TermId(300);
        let l1 = TermId(301);
        store.assert(Triple::new(c, v.owl_intersection_of, l0));
        store.assert(Triple::new(l0, v.rdf_first, c1));
        store.assert(Triple::new(l0, v.rdf_rest, l1));
        store.assert(Triple::new(l1, v.rdf_first, c2));
        store.assert(Triple::new(l1, v.rdf_rest, v.rdf_nil));
        store.assert(Triple::new(x, v.rdf_type, c1));
        store.assert(Triple::new(x, v.rdf_type, c2));

        let axioms = resolve(&store, &v);
        let dirty = None;
        let delta = fire_all(&store, &axioms, &v, dirty);
        let head = Triple::new(x, v.rdf_type, c);
        let (_t, prov) = delta.iter().find(|(t, _)| **t == head).expect("cls-int1 head");
        assert_eq!(prov.rule_id, "cls-int1");
        assert!(prov.premises.contains(&Triple::new(x, v.rdf_type, c1)));
        assert!(prov.premises.contains(&Triple::new(x, v.rdf_type, c2)));
        assert!(!prov.premises.is_empty());
    }

    // scm-int is schema-only: its sole premise is the intersection axiom.
    #[test]
    fn scm_int_records_axiom_premise() {
        let v = Vocabulary::synthetic(1);
        let mut store = MemStore::new(v.clone());
        let c = TermId(100);
        let c1 = TermId(101);
        let l0 = TermId(300);
        store.assert(Triple::new(c, v.owl_intersection_of, l0));
        store.assert(Triple::new(l0, v.rdf_first, c1));
        store.assert(Triple::new(l0, v.rdf_rest, v.rdf_nil));

        let axioms = resolve(&store, &v);
        let delta = fire_all(&store, &axioms, &v, None);
        let head = Triple::new(c, v.rdfs_sub_class_of, c1);
        let (_t, prov) = delta.iter().find(|(t, _)| **t == head).expect("scm-int head");
        assert_eq!(prov.rule_id, "scm-int");
        assert!(!prov.premises.is_empty(), "scm-int must record the intersection axiom");
        assert!(prov.premises.contains(&Triple::new(c, v.owl_intersection_of, l0)));
    }
}
```

> **Executor note:** confirm the exact `fire_all` signature and the vocab field names (`rdf_first`, `rdf_rest`, `rdf_nil`, `owl_intersection_of`, `rdfs_sub_class_of`) by reading `src/list_rules.rs` and `src/vocab.rs`. Adjust the test calls to match. If `fire_all` takes the dirty set as `Option<&FxHashSet<TermId>>`, pass `None`.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p horndb-owlrl --lib premise_tests`
Expected: FAIL — `cls-int1` records empty premises; `scm-int` records empty premises.

- [ ] **Step 3: Extend `SchemaAxioms` to carry originating axiom triples**

In `crates/owlrl/src/list_rules.rs`, change the entries that need the originating list head so the fire fns can name the axiom. Carry the **list head** alongside each resolved chain. Concretely change these fields:

```rust
    /// `(c, listhead, [c1, ..., cn])` — one entry per `?c owl:intersectionOf ?listhead`.
    pub intersections: Vec<(TermId, TermId, Vec<TermId>)>,
    /// `(ad, listhead, [x1, ..., xn])` — one per AllDifferent members list.
    pub all_different: Vec<(TermId, TermId, Vec<TermId>)>,
```

(Leave the other vecs as-is unless the executor finds recording their axiom triple is needed to make tests pass; the instance premises suffice for the data rules. Keep churn minimal.)

Update `resolve` to push the list head: `out.intersections.push((t.s, t.o, cs));` and the AllDifferent block to capture the members-list head term and the `ad` node. Update `body_predicates` and any destructuring of these tuples (the compiler will point at every site — fix each).

- [ ] **Step 4: Thread premises through every firing site**

For each of the 12 `out.insert(head, Provenance { rule_id, premises: smallvec![] })` sites, replace `smallvec![]` with the real body triples. Build the premise list with `Provenance::new(rule_id, [..])` or an explicit `smallvec![..]`. Per-rule premise composition (instance triples are all in local scope at each site):

1. **prp-spo2** (`emit_pair`, ~L507): thread the chain. Change `frontier`/`next` to carry the accumulated path triples, e.g. `Vec<(TermId, TermId, SmallVec<[Triple; 4]>)>`. At the leading scan push `(t.s, t.o, smallvec![Triple::new(t.s, chain[0], t.o)])`; at each extension push `(u0, t.o, prev_path + Triple::new(u_mid, p_i, t.o))`. `emit_pair` takes the path and records it as premises. The chain-of-length-1 branch records the single `(u0, chain[0], un)` triple.
2. **prp-key** (~L580): `smallvec![Triple::new(x, vocab.rdf_type, c), Triple::new(y, vocab.rdf_type, c)]` plus, for the key property values, `Triple::new(x, ps[0], z0)` and `Triple::new(y, ps[0], z0)` and the per-`p_i` matched `(x, p_i, z_i)` / `(y, p_i, z_i)`. (`y` here is the survivor; `z_i` are the `x_choice` values; `survivors` were filtered on `(y, p_i, z_i)`.)
3. **cls-int1** (~L646): `(x, rdf_type, cs[0])` plus `(x, rdf_type, c_i)` for each `c_i` in `cs[1..]`.
4. **scm-int** (~L677): schema-only — the intersection axiom `Triple::new(c, vocab.owl_intersection_of, listhead)`. (Requires the `listhead` from Step 3.)
5. **cls-uni** (~L702): `(t.s, rdf_type, c_i)` — the single matched membership.
6. **cax-adc** (~L736): `(x, rdf_type, cs[i])` and `(x, rdf_type, cj)`.
7. **prp-adp** (~L782): `(u, pi, w)` and `(u, pj, w)`.
8. **cls-maxc1** (~L818): `(u, rdf_type, class)` and the violating value `(u, property, y)` — capture `y` from the `probe(...).next()` instead of discarding it.
9. **cls-maxc2** (~L864): `(u, rdf_type, class)`, `(u, property, y1)`, `(u, property, y2)`.
10. **eq-diff2/3** (`fire_eq_diff_list`, ~L892): schema-only — the AllDifferent axiom. With the `all_different` tuple now `(ad, listhead, xs)`, record `Triple::new(ad, vocab.owl_members, listhead)`. (Pass `ad`+`listhead` into `fire_eq_diff_list`; today it only takes `xs`.)
11. **cls-maxqc_zero** (~L935): `(u, rdf_type, r.class)` and the qualifying value `(u, r.property, y)`.
12. **cls-maxqc_one** (~L988): `(u, rdf_type, r.class)`, `(u, r.property, y1)`, `(u, r.property, y2)`.

Keep premise smallvecs within capacity 4 where possible; longer lists (prp-key, cls-int1 with arity >2) just spill to the heap — that is fine.

- [ ] **Step 5: Run to verify premise tests pass and nothing regressed**

Run: `cargo test -p horndb-owlrl`
Expected: PASS — `premise_tests` green, all existing list-rule + conformance tests still green.

- [ ] **Step 6: Commit**

```bash
git add crates/owlrl/src/list_rules.rs
git commit -m "feat(owlrl): record real premises in list-walking rules (SPEC-04 F4)"
```

---

### Task 3: Expose the proof tree through the `Engine` façade

**Files:**
- Modify: `crates/owlrl/src/integration.rs`

- [ ] **Step 1: Write the failing test** (append to the `#[cfg(test)] mod tests` in `crates/owlrl/src/integration.rs`; read the existing tests there for the `Dataset` construction idiom)

```rust
    #[test]
    fn engine_proof_decodes_a_two_step_derivation() {
        // c1 rdfs:subClassOf c2, c2 rdfs:subClassOf c3, x rdf:type c1.
        // cax-sco/scm-sco derive x rdf:type c3. Engine::proof returns a tree
        // whose leaves are the asserted triples (decoded to IRIs).
        let mut e = Engine::new();
        e.load(&dataset_from_ntriples(
            r#"<http://ex/c1> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <http://ex/c2> .
<http://ex/c2> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <http://ex/c3> .
<http://ex/x> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://ex/c1> ."#,
        ))
        .unwrap();

        let tree = e
            .proof(
                "http://ex/x",
                "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
                "http://ex/c3",
            )
            .expect("proof present");
        // Root is a derived node; recursively all leaves are Asserted.
        assert!(matches!(tree, StringProofTree::Derived { .. }));
        assert!(leaves_all_asserted(&tree));
    }
```

> **Executor note:** the helper `dataset_from_ntriples` and `leaves_all_asserted` must be written in the test module (or reuse an existing dataset-builder helper if `integration.rs` tests already have one — read first). `leaves_all_asserted` walks `StringProofTree` and returns false if any leaf is not `Asserted`/`Cycle`.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p horndb-owlrl --lib engine_proof`
Expected: FAIL — `Engine::proof` / `StringProofTree` missing.

- [ ] **Step 3: Add `StringProofTree` + `Engine::proof`** to `crates/owlrl/src/integration.rs`

```rust
/// A proof tree with terms decoded back to their lexical forms (the same
/// lexical convention as [`Engine::materialized_triples`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StringProofTree {
    Asserted((String, String, String)),
    Derived {
        triple: (String, String, String),
        rule_id: String,
        premises: Vec<StringProofTree>,
    },
    Cycle((String, String, String)),
}
```

```rust
    /// Proof tree for the materialised triple `(s, p, o)` (given as lexical
    /// keys per [`materialized_triples`](Self::materialized_triples)).
    /// `None` if nothing is loaded or the triple is not in the store.
    pub fn proof(&self, s: &str, p: &str, o: &str) -> Option<StringProofTree> {
        let state = self.state.as_ref()?;
        let (&sid, &pid, &oid) = (
            state.dict.get(s)?,
            state.dict.get(p)?,
            state.dict.get(o)?,
        );
        let triple = Triple::new(sid, pid, oid);
        if !state.store.contains(&triple) {
            return None;
        }
        // Invert the dict once: TermId -> lexical key.
        let mut rev: FxHashMap<TermId, &str> = FxHashMap::default();
        for (lex, &id) in &state.dict {
            rev.insert(id, lex.as_str());
        }
        Some(decode_proof(&state.store.proof_tree(&triple), &rev))
    }
```

Add a free helper `decode_proof(tree: &ProofTree, rev: &FxHashMap<TermId, &str>) -> StringProofTree` that maps each `Triple` via `rev` (skip/`?`-fallback a missing key defensively, mirroring `materialized_triples`) and recurses over premises. Import `ProofTree` and `StringProofTree` as needed; export `StringProofTree` from `lib.rs` next to `Engine`.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p horndb-owlrl --lib engine_proof`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/owlrl/src/integration.rs crates/owlrl/src/lib.rs
git commit -m "feat(owlrl): Engine::proof returns a decoded proof tree (SPEC-04 F4)"
```

---

### Task 4: NF4 depth sanity test + docs sync

**Files:**
- Create: `crates/owlrl/tests/proof_tree.rs`
- Modify: `crates/owlrl/AGENTS.md` (§7), `crates/owlrl/INTEGRATION-NOTES.md`, `docs/architecture.md`

- [ ] **Step 1: Write the multi-step + NF4 integration test** in `crates/owlrl/tests/proof_tree.rs`

```rust
//! Proof-tree integration tests (SPEC-04 F4, acceptance #5, NF4).
use horndb_owlrl::integration::{Engine, StringProofTree};

fn leaf_count_and_depth(t: &StringProofTree) -> (usize, usize) {
    match t {
        StringProofTree::Asserted(_) | StringProofTree::Cycle(_) => (1, 1),
        StringProofTree::Derived { premises, .. } => {
            if premises.is_empty() {
                return (1, 1);
            }
            let mut leaves = 0;
            let mut max_child = 0;
            for p in premises {
                let (l, d) = leaf_count_and_depth(p);
                leaves += l;
                max_child = max_child.max(d);
            }
            (leaves, max_child + 1)
        }
    }
}

#[test]
fn nf4_depth_chain_proof_is_correct_and_fast() {
    // Build a subClassOf chain c0 .. c10 plus x rdf:type c0. The closure
    // derives x rdf:type c10; its proof has depth >= 2 and bottoms out at
    // asserted triples. NF4: building it is well under 100 ms.
    let n = 11;
    let mut nt = String::new();
    let sco = "http://www.w3.org/2000/01/rdf-schema#subClassOf";
    let ty = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
    for i in 0..n - 1 {
        nt.push_str(&format!("<http://ex/c{i}> <{sco}> <http://ex/c{}> .\n", i + 1));
    }
    nt.push_str(&format!("<http://ex/x> <{ty}> <http://ex/c0> .\n"));

    let mut e = Engine::new();
    e.load(&horndb_owlrl::integration::dataset_from_ntriples(&nt)).unwrap();

    let start = std::time::Instant::now();
    let tree = e
        .proof("http://ex/x", ty, &format!("http://ex/c{}", n - 1))
        .expect("deep proof present");
    let elapsed = start.elapsed();

    let (leaves, depth) = leaf_count_and_depth(&tree);
    assert!(depth >= 2, "expected a multi-step proof, got depth {depth}");
    assert!(leaves >= 1);
    assert!(
        elapsed.as_millis() < 100,
        "NF4: proof build took {elapsed:?}, budget 100 ms"
    );
}
```

> **Executor note:** there may be no public `dataset_from_ntriples` on the crate. If the crate has no public N-Triples → `Dataset` helper, either (a) reuse whatever the existing `integration.rs` tests use to build a `Dataset` and expose a small `#[cfg(test)]`-free constructor, or (b) build the chain with `Engine`'s existing public load path used elsewhere in the harness. Read `crates/owlrl/src/integration.rs` and `crates/harness` for the established loading idiom before finalizing. Keep the assertion semantics (depth ≥ 2, < 100 ms).

- [ ] **Step 2: Run to verify it passes**

Run: `cargo test -p horndb-owlrl --test proof_tree`
Expected: PASS.

- [ ] **Step 3: Update crate docs** — `crates/owlrl/AGENTS.md` §7 (the symlinked `CLAUDE.md` updates automatically). Replace the F4 deferral language: proof recording now records real premises in the list rules and a `proof(t)`/`Engine::proof` API returns a proof tree. Note the documented elisions: GraphBLAS closure backend records best-effort empty premises; restriction-rule schema declarations are an elided side condition. Add the same to `crates/owlrl/INTEGRATION-NOTES.md`.

- [ ] **Step 4: Flip `docs/architecture.md` Status** for SPEC-04 proof recording (`planned` → `implemented`). Read the SPEC-04 section first; change only the proof/F4 row.

- [ ] **Step 5: Commit**

```bash
git add crates/owlrl/tests/proof_tree.rs crates/owlrl/AGENTS.md crates/owlrl/INTEGRATION-NOTES.md docs/architecture.md
git commit -m "test(owlrl): NF4 proof-depth sanity + docs sync (SPEC-04 F4)"
```

---

## Final verification (Phase 6 gate)

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

All green. The owlrl 50-case conformance subset must remain green (no regression): the premise changes are additive (they only populate a field that was empty) and must not change which triples are derived.

## Self-review notes

- **Spec coverage:** F4 premises (Task 2), `proof(t)` tree + leaves-asserted (Tasks 1, 3), acceptance #5 multi-step (Tasks 1, 3, 4), NF4 (Task 4). ✓
- **Type consistency:** `ProofTree` (TermId-level, Task 1) vs `StringProofTree` (decoded, Task 3) are distinct by design; `proof_tree` returns the former, `Engine::proof` the latter. `Provenance::new(rule_id, premises)` is the existing constructor.
- **Out of scope (documented, not silently dropped):** GraphBLAS-backend premise attribution; restriction-declaration schema premises; compressed side-table persistence; W3C explanation-test fixtures (gated on SPEC-01 harness wiring — note in INTEGRATION-NOTES).
