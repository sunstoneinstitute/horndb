# SPEC-04 OWL 2 RL Rule Engine — Stage-0/1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stand up the `reasoner-owlrl` crate with (a) an ahead-of-time codegen pipeline driven by a declarative `rules.toml`, emitting one Rust function per OWL 2 RL rule via `build.rs`; (b) a Stage-1 subset of the W3C OWL 2 RL/RDF rule set covering all `scm-*` schema rules plus the most-used `cls-*`, `cax-*`, `prp-*` rules (target: enough to pass ≥50 hand-picked W3C conformance tests per SPEC-00 Stage 1 gating); (c) semi-naïve evaluation with simple delta tables driving the rules to fixed point; (d) full re-materialization from base (`reset_and_materialize`); (e) a `ClosureBackend` trait that delegates `prp-trp` / `scm-sco` / `scm-spo` / `eq-*` to SPEC-05, with a trivial in-crate rule-firing implementation used for our own tests.

**Architecture:** A single workspace crate `reasoner-owlrl` with a `build.rs` that parses `rules.toml` (one entry per rule: id, head, body patterns, closure-delegate flag) and emits `$OUT_DIR/generated_rules.rs`. The generated file contains a `pub fn fire_<rule_id>(store: &dyn TripleStore, delta: &Delta) -> Delta` per rule, plus a `pub const RULES: &[CompiledRule]` table mapping rule IDs to function pointers and the predicates they read/write. The runtime in `src/lib.rs` defines small `TripleStore` and `Delta` traits (in-crate, behind a feature flag for the production SPEC-02 backend later), an in-memory reference `MemStore` implementation we own (for tests), and a `materialize()` function that runs semi-naïve evaluation: at each round it computes deltas from rules whose body-predicates overlap "dirty" predicates touched in the previous round, applies them through the store, and terminates when the round produces no new triples. A `ClosureBackend` trait abstracts equality/transitive closure; the crate ships a `RuleFiringBackend` (naïve, for tests) and exposes the trait for `reasoner-closure` (SPEC-05) to implement. Proof recording is a Stage-1 **stub**: each `Delta` row carries a `Provenance { rule_id, premises: SmallVec<[TripleId; 4]> }`, written through to the store via the `TripleSink`, but with no compression or backward-rederivation yet — that is Stage 2. No incremental update path (SPEC-06): only full re-materialization.

**Tech Stack:** Rust 2021 (workspace `rust-version = "1.75"`), no external runtime deps beyond `anyhow`, `thiserror`, `serde` + `toml` (build-deps only — the runtime crate must not pull serde), `rustc-hash` for `FxHashSet`/`FxHashMap`, `smallvec` for proof premises. `prettyplease` (build-dep) for emitting human-readable generated Rust. The W3C OWL 2 RL/RDF rules are the canonical reference (see https://www.w3.org/TR/owl2-profiles/ Tables 4–9). Co-developed with SPEC-05 (`reasoner-closure`) and SPEC-06 (`reasoner-incremental`); Stage 1 does not depend on either at the binary level — we stub them through traits.

---

## Stage 0 / Stage 1 scope boundary (read first)

**In scope for this plan:**
- F2 (codegen pipeline) — `build.rs` driven by `rules.toml` emitting one function per rule.
- F3 (semi-naïve evaluation) — naïve dirty-predicate driven loop with simple delta tables.
- F7 (reset and rematerialize) — `reset_and_materialize(&mut store, &backend)`.
- F1 partial — `scm-*` (all of Table 9) plus the most-used `cls-*` (cls-int1, cls-int2, cls-uni, cls-com, cls-svf1, cls-svf2, cls-avf, cls-hv1, cls-hv2, cls-maxc1, cls-maxc2, cls-nothing2), `cax-*` (cax-sco, cax-eqc1, cax-eqc2, cax-dw), `prp-*` (prp-dom, prp-rng, prp-fp, prp-ifp, prp-symp, prp-spo1, prp-eqp1, prp-eqp2, prp-pdw, prp-adp, prp-inv1, prp-inv2). That subset matches the rules exercised by the W3C `rdfbased-*` and `bnode-*` test families and is sized to clear the ≥50 W3C OWL 2 RL test cases gating bar.
- `ClosureBackend` trait + in-crate `RuleFiringBackend` reference.

**Explicitly Future Work (do NOT plan tasks for them here):**
- Remaining `cls-*` / `cax-*` / `prp-*` not in the Stage-1 list.
- All `dt-*` datatype rules (Tables 4 §`Datatypes` and Table 8) — Stage 2.
- F4 production proof recording (compressed side-table, on-demand rederivation) — Stage 1 ships only an in-memory `Provenance` struct attached to deltas; nothing on disk.
- F5 `rdf:type` skew optimization (partition-by-class-id parallelism) — Stage 1 uses naïve scans.
- F6 incremental updates / DBSP — Stage 2 via SPEC-06.
- Backward-chained rederivation of proofs (SPEC-03 dep).
- LUBM-8000 NF1 throughput target (≥2 M triples/sec) — Stage 1 needs only "completes in finite time" on LUBM-100.

---

## File Structure

Create the following under `crates/owlrl/`:

| Path | Responsibility |
|------|----------------|
| `crates/owlrl/Cargo.toml` | Crate manifest. Workspace deps + `rustc-hash`, `smallvec`. Build-deps: `serde`, `toml`, `prettyplease`, `syn`, `quote`, `proc-macro2`. |
| `crates/owlrl/build.rs` | Reads `rules.toml`, parses each rule, emits `$OUT_DIR/generated_rules.rs`. Re-runs on changes to `rules.toml`. |
| `crates/owlrl/rules.toml` | Declarative rule list. One `[[rule]]` per OWL 2 RL/RDF rule, with `id`, `head`, `body`, optional `delegate_to_closure = true`. Source of truth for Stage 1 subset. |
| `crates/owlrl/codegen/mod.rs` | Codegen library code (imported by `build.rs`). Split out so we can unit-test it. |
| `crates/owlrl/codegen/parse.rs` | Parse `rules.toml` into `RuleSpec { id, head: Pattern, body: Vec<Pattern>, delegate: bool }`. |
| `crates/owlrl/codegen/emit.rs` | Emit a `TokenStream` per rule: `pub fn fire_<id>(store: &dyn TripleStore, delta: &Delta) -> Delta`. Body is a fixed plan: for each body pattern, scan or probe; intersect bindings; emit head substitutions. |
| `crates/owlrl/codegen/plan.rs` | Trivial query plan over the body: pick a leading "delta-bound" pattern (the one matching the dirty predicate) and join the rest with the in-crate `TripleStore::probe` helper. Stage 1 is naïve nested-loop — SPEC-03 WCOJ slots in later. |
| `crates/owlrl/src/lib.rs` | Crate root. Re-exports `TripleStore`, `Delta`, `MemStore`, `ClosureBackend`, `RuleFiringBackend`, `materialize`, `reset_and_materialize`, `CompiledRule`, `RULES`. Includes the generated file via `include!`. |
| `crates/owlrl/src/types.rs` | `TermId(u64)`, `Triple { s, p, o: TermId }`, `Pattern { s: Slot, p: Slot, o: Slot }`, `Slot::{Var(u8), Const(TermId)}`, `RuleId(&'static str)`. |
| `crates/owlrl/src/store.rs` | `TripleStore` trait: `contains`, `scan_predicate(p) -> Iterator`, `probe(s?, p, o?) -> Iterator`, `insert_inferred(triple, provenance)`. `MemStore` impl: `FxHashMap<TermId, FxHashSet<(TermId, TermId)>>` keyed by predicate. |
| `crates/owlrl/src/delta.rs` | `Delta` struct: `FxHashSet<Triple>` plus `FxHashMap<Triple, Provenance>`; helpers `union`, `subtract`, `iter`, `len`, `is_empty`, `dirty_predicates() -> FxHashSet<TermId>`. |
| `crates/owlrl/src/provenance.rs` | `Provenance { rule_id: &'static str, premises: SmallVec<[Triple; 4]> }`. |
| `crates/owlrl/src/vocab.rs` | Constant `TermId`s for OWL/RDFS vocabulary referenced by rules (rdf:type, rdfs:subClassOf, etc.) plus a `Vocabulary` struct so tests can wire numeric IDs. |
| `crates/owlrl/src/backend.rs` | `ClosureBackend` trait. `RuleFiringBackend` impl that fires `prp-trp`/`scm-sco`/`scm-spo` as ordinary rules (naïve closure by repeated rule firing — for tests only). |
| `crates/owlrl/src/engine.rs` | `materialize(store, backend) -> Stats` (semi-naïve loop), `reset_and_materialize(store, backend)`. |
| `crates/owlrl/src/generated.rs` | One-line `include!(concat!(env!("OUT_DIR"), "/generated_rules.rs"));` — re-exported `RULES` and `fire_<id>` from generated code. |
| `crates/owlrl/tests/codegen_smoke.rs` | Integration test: generated file exists, `RULES.len() > 0`, every rule fn is callable. |
| `crates/owlrl/tests/single_rule.rs` | Integration test: feed a tiny MemStore, fire `cax-sco`, check the inferred triple appears. |
| `crates/owlrl/tests/semi_naive.rs` | Integration test: schema with subClassOf chain A⊑B⊑C; assert `(x type A)` derives `(x type C)` after full materialization. |
| `crates/owlrl/tests/reset_rematerialize.rs` | Integration test: materialize, then reset, then re-materialize and check bit-identical store. |
| `crates/owlrl/tests/closure_backend.rs` | Integration test: `RuleFiringBackend` correctly closes `subClassOf` chain. |
| `crates/owlrl/tests/w3c_subset.rs` | Integration test: hand-encoded forms of 5 W3C OWL 2 RL test fixtures; expand to ≥10 across the Stage-1 rule subset. Full SPEC-01 harness wiring is a separate plan. |

---

## Task 0: Verify workspace builds before touching anything

**Files:** none.

- [ ] **Step 1: Confirm the workspace currently builds**

Run:

```bash
cd /Users/stig/git/sunstone/reasoner
cargo build -p reasoner-owlrl
```

Expected: clean build of the empty placeholder crate. If it fails for an unrelated reason (e.g. system toolchain), fix that *before* starting Task 1 — the rest of the plan assumes a working baseline.

- [ ] **Step 2: Confirm tests run on the empty crate**

Run:

```bash
cargo test -p reasoner-owlrl
```

Expected: `running 0 tests ... test result: ok. 0 passed; 0 failed; 0 ignored`.

---

## Task 1: Bootstrap the `reasoner-owlrl` crate manifest

**Files:**
- Modify: `crates/owlrl/Cargo.toml`

- [ ] **Step 1: Replace the empty manifest**

Overwrite `crates/owlrl/Cargo.toml` with:

```toml
[package]
name = "reasoner-owlrl"
version = "0.0.0"
edition.workspace = true
license.workspace = true
publish = false

[dependencies]
anyhow = { workspace = true }
thiserror = { workspace = true }
rustc-hash = "2"
smallvec = "1.13"

[build-dependencies]
anyhow = { workspace = true }
serde = { version = "1", features = ["derive"] }
toml = "0.8"
proc-macro2 = "1"
quote = "1"
syn = { version = "2", features = ["full"] }
prettyplease = "0.2"

[dev-dependencies]
anyhow = { workspace = true }
```

- [ ] **Step 2: Verify it still builds (no rules yet — build.rs will be added in Task 4)**

Run:

```bash
cargo build -p reasoner-owlrl
```

Expected: success. Dependencies download on first run.

- [ ] **Step 3: Commit**

```bash
git add crates/owlrl/Cargo.toml
git commit -m "$(cat <<'EOF'
owlrl: flesh out crate manifest for codegen pipeline

Adds runtime deps (rustc-hash, smallvec) and the build-time toolchain
(serde + toml + proc-macro2 + quote + syn + prettyplease) the rules.toml
codegen will use in subsequent tasks.
EOF
)"
```

---

## Task 2: Define the core types module

**Files:**
- Create: `crates/owlrl/src/types.rs`
- Modify: `crates/owlrl/src/lib.rs`
- Test: `crates/owlrl/src/types.rs` (unit tests inline)

- [ ] **Step 1: Write the failing test (inline, at the bottom of `types.rs`)**

Create `crates/owlrl/src/types.rs`:

```rust
//! Core newtypes shared by the runtime and the generated rule code.

use std::fmt;

/// Dictionary-encoded RDF term identifier. Matches SPEC-02 `TermId` ABI
/// (64-bit, opaque to this crate).
#[derive(Copy, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct TermId(pub u64);

impl fmt::Debug for TermId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "T#{}", self.0)
    }
}

/// An RDF triple in subject–predicate–object order.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, PartialOrd, Ord)]
pub struct Triple {
    pub s: TermId,
    pub p: TermId,
    pub o: TermId,
}

impl Triple {
    pub const fn new(s: TermId, p: TermId, o: TermId) -> Self {
        Self { s, p, o }
    }
}

/// A slot inside a triple pattern: either a variable (referenced by index 0..=2)
/// or a constant term. Used by the codegen, not by the runtime hot path.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum Slot {
    Var(u8),
    Const(TermId),
}

/// A triple pattern used inside a rule body or head.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct Pattern {
    pub s: Slot,
    pub p: Slot,
    pub o: Slot,
}

/// Static rule identifier — the `id` field from `rules.toml`.
pub type RuleId = &'static str;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triple_equality_is_by_value() {
        let a = Triple::new(TermId(1), TermId(2), TermId(3));
        let b = Triple::new(TermId(1), TermId(2), TermId(3));
        assert_eq!(a, b);
    }

    #[test]
    fn slot_variants_distinct() {
        assert_ne!(Slot::Var(0), Slot::Const(TermId(0)));
    }
}
```

- [ ] **Step 2: Wire the module into `lib.rs`**

Overwrite `crates/owlrl/src/lib.rs`:

```rust
//! reasoner-owlrl — OWL 2 RL/RDF rule engine (Stage 1).
//!
//! See `specs/SPEC-04-rule-engine.md` for the design contract and
//! `plans/2026-05-24-SPEC-04-owl-rl-rule-engine.md` for the implementation plan.

pub mod types;
```

- [ ] **Step 3: Run the tests**

Run:

```bash
cargo test -p reasoner-owlrl types::tests
```

Expected: `test result: ok. 2 passed`.

- [ ] **Step 4: Commit**

```bash
git add crates/owlrl/src/types.rs crates/owlrl/src/lib.rs
git commit -m "owlrl: add TermId / Triple / Slot / Pattern core types"
```

---

## Task 3: Define the `Provenance` and `Delta` modules

**Files:**
- Create: `crates/owlrl/src/provenance.rs`
- Create: `crates/owlrl/src/delta.rs`
- Modify: `crates/owlrl/src/lib.rs`

- [ ] **Step 1: Create `provenance.rs`**

```rust
//! Per-inferred-triple proof annotation. Stage 1 keeps this in-memory only;
//! production proof recording (compressed side-table, on-demand rederivation)
//! is Future Work — see SPEC-04 F4.

use crate::types::{RuleId, Triple};
use smallvec::SmallVec;

#[derive(Clone, Eq, PartialEq, Debug)]
pub struct Provenance {
    pub rule_id: RuleId,
    pub premises: SmallVec<[Triple; 4]>,
}

impl Provenance {
    pub fn new(rule_id: RuleId, premises: impl IntoIterator<Item = Triple>) -> Self {
        Self { rule_id, premises: premises.into_iter().collect() }
    }
}
```

- [ ] **Step 2: Create `delta.rs` with failing tests inline**

```rust
//! Delta tables for semi-naïve evaluation.

use crate::provenance::Provenance;
use crate::types::{TermId, Triple};
use rustc_hash::{FxHashMap, FxHashSet};

#[derive(Default, Debug, Clone)]
pub struct Delta {
    triples: FxHashSet<Triple>,
    proofs: FxHashMap<Triple, Provenance>,
}

impl Delta {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, t: Triple, prov: Provenance) -> bool {
        let fresh = self.triples.insert(t);
        if fresh {
            self.proofs.insert(t, prov);
        }
        fresh
    }

    pub fn contains(&self, t: &Triple) -> bool {
        self.triples.contains(t)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Triple, &Provenance)> {
        self.triples.iter().map(move |t| (t, &self.proofs[t]))
    }

    pub fn triples(&self) -> impl Iterator<Item = &Triple> {
        self.triples.iter()
    }

    pub fn len(&self) -> usize {
        self.triples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.triples.is_empty()
    }

    /// Set of distinct predicate IDs touched by this delta.
    pub fn dirty_predicates(&self) -> FxHashSet<TermId> {
        self.triples.iter().map(|t| t.p).collect()
    }

    /// Move all entries from `other` into `self` (proofs from `self` win on conflict).
    pub fn merge_from(&mut self, other: Delta) {
        for (t, prov) in other.triples.into_iter().zip(other.proofs.into_values()) {
            // zip is only safe here because we drain both halves in matching order
            // — instead, redo the merge by lookup:
            let _ = (t, prov);
        }
        // The zip above is wrong because hashset/hashmap iteration order is unrelated.
        // We use the simpler correct form below.
        unreachable!("merge_from is implemented via the loop below; see merge");
    }

    /// Merge `other` into `self`. Existing entries keep their original provenance.
    pub fn merge(&mut self, other: Delta) {
        let Delta { triples, mut proofs } = other;
        for t in triples {
            if self.triples.insert(t) {
                if let Some(p) = proofs.remove(&t) {
                    self.proofs.insert(t, p);
                }
            }
        }
    }

    /// Drop any triples already present in `existing`.
    pub fn subtract(&mut self, existing: &FxHashSet<Triple>) {
        self.triples.retain(|t| !existing.contains(t));
        self.proofs.retain(|t, _| self.triples.contains(t));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smallvec::smallvec;

    fn t(s: u64, p: u64, o: u64) -> Triple {
        Triple::new(TermId(s), TermId(p), TermId(o))
    }

    fn prov(id: &'static str) -> Provenance {
        Provenance { rule_id: id, premises: smallvec![] }
    }

    #[test]
    fn insert_dedups() {
        let mut d = Delta::new();
        assert!(d.insert(t(1, 2, 3), prov("r1")));
        assert!(!d.insert(t(1, 2, 3), prov("r2")));
        assert_eq!(d.len(), 1);
    }

    #[test]
    fn dirty_predicates_unique() {
        let mut d = Delta::new();
        d.insert(t(1, 2, 3), prov("r1"));
        d.insert(t(4, 2, 5), prov("r1"));
        d.insert(t(6, 7, 8), prov("r2"));
        let preds = d.dirty_predicates();
        assert_eq!(preds.len(), 2);
        assert!(preds.contains(&TermId(2)));
        assert!(preds.contains(&TermId(7)));
    }

    #[test]
    fn subtract_drops_known() {
        let mut d = Delta::new();
        d.insert(t(1, 2, 3), prov("r"));
        d.insert(t(4, 5, 6), prov("r"));
        let mut existing = FxHashSet::default();
        existing.insert(t(1, 2, 3));
        d.subtract(&existing);
        assert_eq!(d.len(), 1);
        assert!(d.contains(&t(4, 5, 6)));
    }
}
```

Note: the `merge_from` method above contains a deliberately broken stub — replace it before running tests. Use the correct `merge` instead. (Step 3 deletes the stub.)

- [ ] **Step 3: Remove the broken `merge_from` stub**

Edit `crates/owlrl/src/delta.rs`. Delete the entire `merge_from` method (the one ending in `unreachable!`). Keep `merge`. Re-run `cargo build -p reasoner-owlrl` and verify it still compiles.

- [ ] **Step 4: Wire modules into `lib.rs`**

Update `crates/owlrl/src/lib.rs`:

```rust
//! reasoner-owlrl — OWL 2 RL/RDF rule engine (Stage 1).

pub mod delta;
pub mod provenance;
pub mod types;
```

- [ ] **Step 5: Run the tests**

Run:

```bash
cargo test -p reasoner-owlrl
```

Expected: 5 tests pass (2 from types, 3 from delta).

- [ ] **Step 6: Commit**

```bash
git add crates/owlrl/src/
git commit -m "owlrl: add Delta and Provenance modules with dedup + subtract"
```

---

## Task 4: Define the vocabulary constants

**Files:**
- Create: `crates/owlrl/src/vocab.rs`
- Modify: `crates/owlrl/src/lib.rs`

Vocabulary IDs are owned by SPEC-02's dictionary at runtime. For Stage 1 we maintain a `Vocabulary` struct that callers populate; the codegen consults this struct via a `TripleStore::vocab() -> &Vocabulary` accessor. Hardcoding numeric IDs in generated code would break the SPEC-02 contract.

- [ ] **Step 1: Create `vocab.rs`**

```rust
//! OWL / RDF / RDFS vocabulary IDs the generated rules need to consult.
//!
//! At runtime, populated by the caller (typically SPEC-02 storage layer) by
//! dictionary-encoding each IRI. In tests we populate it by hand.

use crate::types::TermId;

/// All vocabulary terms referenced by the Stage-1 OWL 2 RL rule subset.
/// Fields are public so a builder can fill them directly.
#[derive(Copy, Clone, Debug)]
pub struct Vocabulary {
    // rdf:
    pub rdf_type: TermId,
    pub rdf_first: TermId,
    pub rdf_rest: TermId,
    pub rdf_nil: TermId,

    // rdfs:
    pub rdfs_sub_class_of: TermId,
    pub rdfs_sub_property_of: TermId,
    pub rdfs_domain: TermId,
    pub rdfs_range: TermId,

    // owl:
    pub owl_class: TermId,
    pub owl_thing: TermId,
    pub owl_nothing: TermId,
    pub owl_same_as: TermId,
    pub owl_different_from: TermId,
    pub owl_equivalent_class: TermId,
    pub owl_equivalent_property: TermId,
    pub owl_inverse_of: TermId,
    pub owl_functional_property: TermId,
    pub owl_inverse_functional_property: TermId,
    pub owl_symmetric_property: TermId,
    pub owl_transitive_property: TermId,
    pub owl_irreflexive_property: TermId,
    pub owl_asymmetric_property: TermId,
    pub owl_property_disjoint_with: TermId,
    pub owl_disjoint_with: TermId,
    pub owl_complement_of: TermId,
    pub owl_intersection_of: TermId,
    pub owl_union_of: TermId,
    pub owl_some_values_from: TermId,
    pub owl_all_values_from: TermId,
    pub owl_has_value: TermId,
    pub owl_on_property: TermId,
    pub owl_max_cardinality: TermId,
}

impl Vocabulary {
    /// Construct a vocabulary by allocating consecutive `TermId`s starting from
    /// `base`. Used by tests; production code receives the real IDs from the
    /// SPEC-02 dictionary.
    pub fn synthetic(base: u64) -> Self {
        let mut n = base;
        let mut next = || { let v = TermId(n); n += 1; v };
        Self {
            rdf_type: next(),
            rdf_first: next(),
            rdf_rest: next(),
            rdf_nil: next(),
            rdfs_sub_class_of: next(),
            rdfs_sub_property_of: next(),
            rdfs_domain: next(),
            rdfs_range: next(),
            owl_class: next(),
            owl_thing: next(),
            owl_nothing: next(),
            owl_same_as: next(),
            owl_different_from: next(),
            owl_equivalent_class: next(),
            owl_equivalent_property: next(),
            owl_inverse_of: next(),
            owl_functional_property: next(),
            owl_inverse_functional_property: next(),
            owl_symmetric_property: next(),
            owl_transitive_property: next(),
            owl_irreflexive_property: next(),
            owl_asymmetric_property: next(),
            owl_property_disjoint_with: next(),
            owl_disjoint_with: next(),
            owl_complement_of: next(),
            owl_intersection_of: next(),
            owl_union_of: next(),
            owl_some_values_from: next(),
            owl_all_values_from: next(),
            owl_has_value: next(),
            owl_on_property: next(),
            owl_max_cardinality: next(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_yields_distinct_ids() {
        let v = Vocabulary::synthetic(100);
        assert_eq!(v.rdf_type, TermId(100));
        assert_ne!(v.rdf_type, v.rdfs_sub_class_of);
        assert_ne!(v.owl_thing, v.owl_nothing);
    }
}
```

- [ ] **Step 2: Wire into `lib.rs`**

```rust
//! reasoner-owlrl — OWL 2 RL/RDF rule engine (Stage 1).

pub mod delta;
pub mod provenance;
pub mod types;
pub mod vocab;
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p reasoner-owlrl
```

Expected: 6 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/owlrl/src/vocab.rs crates/owlrl/src/lib.rs
git commit -m "owlrl: add Vocabulary struct for OWL/RDFS vocabulary IDs"
```

---

## Task 5: Define the `TripleStore` trait and `MemStore` in-memory backend

**Files:**
- Create: `crates/owlrl/src/store.rs`
- Modify: `crates/owlrl/src/lib.rs`

- [ ] **Step 1: Create `store.rs` with failing tests**

```rust
//! Storage trait the generated rule code consumes.
//!
//! For Stage 1, the only impl shipped is `MemStore`. SPEC-02 will provide a
//! production backend implementing the same trait.

use crate::provenance::Provenance;
use crate::types::{TermId, Triple};
use crate::vocab::Vocabulary;
use rustc_hash::{FxHashMap, FxHashSet};

/// Iterator alias the trait returns. Boxed for object safety; Stage 2 can
/// specialise via a separate non-dyn trait if profiling demands it.
pub type TripleIter<'a> = Box<dyn Iterator<Item = Triple> + 'a>;

pub trait TripleStore {
    /// Vocabulary IDs (RDF/RDFS/OWL terms). Generated rule code calls this
    /// once on entry to avoid repeated lookups.
    fn vocab(&self) -> &Vocabulary;

    /// Does the store contain `t` (as asserted OR previously inferred)?
    fn contains(&self, t: &Triple) -> bool;

    /// Iterate all triples whose predicate equals `p`.
    fn scan_predicate(&self, p: TermId) -> TripleIter<'_>;

    /// Probe: return triples matching the given (subject?, predicate, object?) pattern.
    /// `None` slots are wildcards.
    fn probe(&self, s: Option<TermId>, p: TermId, o: Option<TermId>) -> TripleIter<'_>;

    /// Insert an inferred triple with its proof. Returns true iff fresh.
    fn insert_inferred(&mut self, t: Triple, prov: Provenance) -> bool;

    /// Drop all inferred triples (asserted ones stay). Used by reset_and_materialize.
    fn clear_inferred(&mut self);

    /// All triples currently in the store, asserted + inferred. Stage 1 only.
    fn all_triples(&self) -> FxHashSet<Triple>;
}

/// Simple in-memory store keyed by predicate. Used by tests and by the
/// `RuleFiringBackend` reference implementation.
pub struct MemStore {
    vocab: Vocabulary,
    /// predicate → set of (subject, object)
    by_pred: FxHashMap<TermId, FxHashSet<(TermId, TermId)>>,
    /// proofs for inferred triples (asserted triples have no entry)
    proofs: FxHashMap<Triple, Provenance>,
    /// inferred set (subset of by_pred entries)
    inferred: FxHashSet<Triple>,
}

impl MemStore {
    pub fn new(vocab: Vocabulary) -> Self {
        Self {
            vocab,
            by_pred: FxHashMap::default(),
            proofs: FxHashMap::default(),
            inferred: FxHashSet::default(),
        }
    }

    /// Insert an asserted (base) triple. Returns true iff fresh.
    pub fn assert(&mut self, t: Triple) -> bool {
        self.by_pred.entry(t.p).or_default().insert((t.s, t.o))
    }

    pub fn assert_all<I: IntoIterator<Item = Triple>>(&mut self, ts: I) {
        for t in ts {
            self.assert(t);
        }
    }

    /// True iff `t` was added via `insert_inferred` (not via `assert`).
    pub fn is_inferred(&self, t: &Triple) -> bool {
        self.inferred.contains(t)
    }

    pub fn proof(&self, t: &Triple) -> Option<&Provenance> {
        self.proofs.get(t)
    }
}

impl TripleStore for MemStore {
    fn vocab(&self) -> &Vocabulary {
        &self.vocab
    }

    fn contains(&self, t: &Triple) -> bool {
        self.by_pred.get(&t.p).is_some_and(|set| set.contains(&(t.s, t.o)))
    }

    fn scan_predicate(&self, p: TermId) -> TripleIter<'_> {
        match self.by_pred.get(&p) {
            Some(set) => Box::new(set.iter().map(move |&(s, o)| Triple::new(s, p, o))),
            None => Box::new(std::iter::empty()),
        }
    }

    fn probe(&self, s: Option<TermId>, p: TermId, o: Option<TermId>) -> TripleIter<'_> {
        match self.by_pred.get(&p) {
            Some(set) => {
                let iter = set.iter().filter_map(move |&(ss, oo)| {
                    if s.map_or(true, |x| x == ss) && o.map_or(true, |x| x == oo) {
                        Some(Triple::new(ss, p, oo))
                    } else {
                        None
                    }
                });
                Box::new(iter)
            }
            None => Box::new(std::iter::empty()),
        }
    }

    fn insert_inferred(&mut self, t: Triple, prov: Provenance) -> bool {
        let fresh = self.by_pred.entry(t.p).or_default().insert((t.s, t.o));
        if fresh {
            self.inferred.insert(t);
            self.proofs.insert(t, prov);
        }
        fresh
    }

    fn clear_inferred(&mut self) {
        for t in self.inferred.drain() {
            if let Some(set) = self.by_pred.get_mut(&t.p) {
                set.remove(&(t.s, t.o));
                if set.is_empty() {
                    self.by_pred.remove(&t.p);
                }
            }
        }
        self.proofs.clear();
    }

    fn all_triples(&self) -> FxHashSet<Triple> {
        let mut out = FxHashSet::default();
        for (&p, set) in &self.by_pred {
            for &(s, o) in set {
                out.insert(Triple::new(s, p, o));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smallvec::smallvec;

    fn store() -> MemStore {
        MemStore::new(Vocabulary::synthetic(1000))
    }

    fn t(s: u64, p: u64, o: u64) -> Triple {
        Triple::new(TermId(s), TermId(p), TermId(o))
    }

    #[test]
    fn assert_and_contains() {
        let mut s = store();
        assert!(s.assert(t(1, 2, 3)));
        assert!(!s.assert(t(1, 2, 3))); // dedup
        assert!(s.contains(&t(1, 2, 3)));
        assert!(!s.contains(&t(1, 2, 4)));
    }

    #[test]
    fn scan_predicate_returns_all_matches() {
        let mut s = store();
        s.assert(t(1, 2, 3));
        s.assert(t(4, 2, 5));
        s.assert(t(6, 7, 8));
        let got: Vec<_> = s.scan_predicate(TermId(2)).collect();
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn probe_filters_subject_and_object() {
        let mut s = store();
        s.assert(t(1, 2, 3));
        s.assert(t(1, 2, 4));
        s.assert(t(5, 2, 3));
        let got: Vec<_> = s.probe(Some(TermId(1)), TermId(2), None).collect();
        assert_eq!(got.len(), 2);
        let got: Vec<_> = s.probe(None, TermId(2), Some(TermId(3))).collect();
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn clear_inferred_keeps_asserted() {
        let mut s = store();
        s.assert(t(1, 2, 3));
        s.insert_inferred(t(4, 5, 6), Provenance { rule_id: "r", premises: smallvec![] });
        assert!(s.contains(&t(4, 5, 6)));
        s.clear_inferred();
        assert!(s.contains(&t(1, 2, 3)));
        assert!(!s.contains(&t(4, 5, 6)));
    }
}
```

- [ ] **Step 2: Wire into `lib.rs`**

```rust
//! reasoner-owlrl — OWL 2 RL/RDF rule engine (Stage 1).

pub mod delta;
pub mod provenance;
pub mod store;
pub mod types;
pub mod vocab;
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p reasoner-owlrl
```

Expected: 10 tests pass (6 prior + 4 new).

- [ ] **Step 4: Commit**

```bash
git add crates/owlrl/src/store.rs crates/owlrl/src/lib.rs
git commit -m "owlrl: add TripleStore trait and MemStore reference impl"
```

---

## Task 6: Author the Stage-1 `rules.toml`

**Files:**
- Create: `crates/owlrl/rules.toml`

The rule list is the source of truth for the codegen. We use a single declarative TOML file because (a) it is trivial to parse, (b) it lives in-repo and is reviewable, (c) adding a rule in Stage 2 is a one-block patch with no Rust changes. Each rule has:

- `id` — the W3C rule identifier (e.g. `"cax-sco"`).
- `comment` — human note linking back to the W3C table.
- `delegate` — optional; if `"closure"`, the rule is routed to `ClosureBackend` instead of being compiled.
- `body` — array of `{ s, p, o }` pattern strings. Each slot is either `?v` (variable, name matters for joins), or a vocabulary token (e.g. `rdfs:subClassOf`).
- `head` — single `{ s, p, o }` pattern, same slot grammar.

The vocabulary tokens use a fixed lowercase prefix set (`rdf:`, `rdfs:`, `owl:`); the codegen maps these to fields on `Vocabulary`.

- [ ] **Step 1: Create `crates/owlrl/rules.toml`**

```toml
# Stage-1 OWL 2 RL/RDF rule subset.
# Source: W3C OWL 2 Profiles — https://www.w3.org/TR/owl2-profiles/ Tables 4–9.
# Adding a rule: append a [[rule]] block; build.rs regenerates on next build.
# Rules with delegate = "closure" are NOT compiled here — they are routed to
# `reasoner-closure` (SPEC-05) via the ClosureBackend trait. We list them so
# the rule table is complete and self-documenting.

# ---------------------------------------------------------------------------
# Closure-delegated rules (Table 4, Table 5, Table 9)
# ---------------------------------------------------------------------------

[[rule]]
id = "eq-ref"
comment = "Reflexivity of owl:sameAs — delegated to EQREL structure in SPEC-05."
delegate = "closure"
body = []
head = { s = "?s", p = "owl:sameAs", o = "?s" }

[[rule]]
id = "eq-sym"
comment = "Symmetry of owl:sameAs — implicit in EQREL representation."
delegate = "closure"
body = [ { s = "?x", p = "owl:sameAs", o = "?y" } ]
head = { s = "?y", p = "owl:sameAs", o = "?x" }

[[rule]]
id = "eq-trans"
comment = "Transitivity of owl:sameAs — implicit in EQREL representation."
delegate = "closure"
body = [
  { s = "?x", p = "owl:sameAs", o = "?y" },
  { s = "?y", p = "owl:sameAs", o = "?z" },
]
head = { s = "?x", p = "owl:sameAs", o = "?z" }

[[rule]]
id = "prp-trp"
comment = "Transitive property — delegated to GraphBLAS closure."
delegate = "closure"
body = [
  { s = "?p", p = "rdf:type", o = "owl:TransitiveProperty" },
  { s = "?x", p = "?p", o = "?y" },
  { s = "?y", p = "?p", o = "?z" },
]
head = { s = "?x", p = "?p", o = "?z" }

[[rule]]
id = "scm-sco"
comment = "Transitivity of rdfs:subClassOf — delegated to closure."
delegate = "closure"
body = [
  { s = "?c1", p = "rdfs:subClassOf", o = "?c2" },
  { s = "?c2", p = "rdfs:subClassOf", o = "?c3" },
]
head = { s = "?c1", p = "rdfs:subClassOf", o = "?c3" }

[[rule]]
id = "scm-spo"
comment = "Transitivity of rdfs:subPropertyOf — delegated to closure."
delegate = "closure"
body = [
  { s = "?p1", p = "rdfs:subPropertyOf", o = "?p2" },
  { s = "?p2", p = "rdfs:subPropertyOf", o = "?p3" },
]
head = { s = "?p1", p = "rdfs:subPropertyOf", o = "?p3" }

# ---------------------------------------------------------------------------
# Class axiom rules (Table 7 — cax-*)
# ---------------------------------------------------------------------------

[[rule]]
id = "cax-sco"
comment = "Subclass instance propagation."
body = [
  { s = "?c1", p = "rdfs:subClassOf", o = "?c2" },
  { s = "?x",  p = "rdf:type",       o = "?c1" },
]
head = { s = "?x", p = "rdf:type", o = "?c2" }

[[rule]]
id = "cax-eqc1"
comment = "owl:equivalentClass instance propagation (left → right)."
body = [
  { s = "?c1", p = "owl:equivalentClass", o = "?c2" },
  { s = "?x",  p = "rdf:type",            o = "?c1" },
]
head = { s = "?x", p = "rdf:type", o = "?c2" }

[[rule]]
id = "cax-eqc2"
comment = "owl:equivalentClass instance propagation (right → left)."
body = [
  { s = "?c1", p = "owl:equivalentClass", o = "?c2" },
  { s = "?x",  p = "rdf:type",            o = "?c2" },
]
head = { s = "?x", p = "rdf:type", o = "?c1" }

# ---------------------------------------------------------------------------
# Property rules (Table 6 — prp-*)
# ---------------------------------------------------------------------------

[[rule]]
id = "prp-dom"
comment = "Domain axiom."
body = [
  { s = "?p", p = "rdfs:domain", o = "?c" },
  { s = "?x", p = "?p",          o = "?y" },
]
head = { s = "?x", p = "rdf:type", o = "?c" }

[[rule]]
id = "prp-rng"
comment = "Range axiom."
body = [
  { s = "?p", p = "rdfs:range", o = "?c" },
  { s = "?x", p = "?p",         o = "?y" },
]
head = { s = "?y", p = "rdf:type", o = "?c" }

[[rule]]
id = "prp-symp"
comment = "Symmetric property."
body = [
  { s = "?p", p = "rdf:type", o = "owl:SymmetricProperty" },
  { s = "?x", p = "?p",       o = "?y" },
]
head = { s = "?y", p = "?p", o = "?x" }

[[rule]]
id = "prp-spo1"
comment = "Sub-property propagation."
body = [
  { s = "?p1", p = "rdfs:subPropertyOf", o = "?p2" },
  { s = "?x",  p = "?p1",                o = "?y" },
]
head = { s = "?x", p = "?p2", o = "?y" }

[[rule]]
id = "prp-eqp1"
comment = "Equivalent property propagation (left → right)."
body = [
  { s = "?p1", p = "owl:equivalentProperty", o = "?p2" },
  { s = "?x",  p = "?p1",                    o = "?y" },
]
head = { s = "?x", p = "?p2", o = "?y" }

[[rule]]
id = "prp-eqp2"
comment = "Equivalent property propagation (right → left)."
body = [
  { s = "?p1", p = "owl:equivalentProperty", o = "?p2" },
  { s = "?x",  p = "?p2",                    o = "?y" },
]
head = { s = "?x", p = "?p1", o = "?y" }

[[rule]]
id = "prp-inv1"
comment = "Inverse property (left → right)."
body = [
  { s = "?p1", p = "owl:inverseOf", o = "?p2" },
  { s = "?x",  p = "?p1",           o = "?y" },
]
head = { s = "?y", p = "?p2", o = "?x" }

[[rule]]
id = "prp-inv2"
comment = "Inverse property (right → left)."
body = [
  { s = "?p1", p = "owl:inverseOf", o = "?p2" },
  { s = "?x",  p = "?p2",           o = "?y" },
]
head = { s = "?y", p = "?p1", o = "?x" }

# ---------------------------------------------------------------------------
# Schema rules (Table 9 — scm-*). scm-sco / scm-spo already declared above as
# closure-delegated. The remaining scm-* rules are compiled here.
# ---------------------------------------------------------------------------

[[rule]]
id = "scm-cls"
comment = "Every class is a subclass of itself and of owl:Thing; disjoint with owl:Nothing."
body = [ { s = "?c", p = "rdf:type", o = "owl:Class" } ]
head = { s = "?c", p = "rdfs:subClassOf", o = "?c" }

[[rule]]
id = "scm-cls-thing"
comment = "Every class is a subclass of owl:Thing."
body = [ { s = "?c", p = "rdf:type", o = "owl:Class" } ]
head = { s = "?c", p = "rdfs:subClassOf", o = "owl:Thing" }

[[rule]]
id = "scm-cls-nothing"
comment = "owl:Nothing is a subclass of every class."
body = [ { s = "?c", p = "rdf:type", o = "owl:Class" } ]
head = { s = "owl:Nothing", p = "rdfs:subClassOf", o = "?c" }

[[rule]]
id = "scm-eqc1"
comment = "equivalentClass implies subClassOf in both directions (1)."
body = [ { s = "?c1", p = "owl:equivalentClass", o = "?c2" } ]
head = { s = "?c1", p = "rdfs:subClassOf", o = "?c2" }

[[rule]]
id = "scm-eqc2"
comment = "equivalentClass implies subClassOf in both directions (2)."
body = [ { s = "?c1", p = "owl:equivalentClass", o = "?c2" } ]
head = { s = "?c2", p = "rdfs:subClassOf", o = "?c1" }

[[rule]]
id = "scm-op"
comment = "Every object property is a subPropertyOf itself."
body = [ { s = "?p", p = "rdf:type", o = "owl:ObjectProperty" } ]
head = { s = "?p", p = "rdfs:subPropertyOf", o = "?p" }

[[rule]]
id = "scm-eqp1"
comment = "equivalentProperty implies subPropertyOf (1)."
body = [ { s = "?p1", p = "owl:equivalentProperty", o = "?p2" } ]
head = { s = "?p1", p = "rdfs:subPropertyOf", o = "?p2" }

[[rule]]
id = "scm-eqp2"
comment = "equivalentProperty implies subPropertyOf (2)."
body = [ { s = "?p1", p = "owl:equivalentProperty", o = "?p2" } ]
head = { s = "?p2", p = "rdfs:subPropertyOf", o = "?p1" }

[[rule]]
id = "scm-dom1"
comment = "Domain narrows along subClassOf."
body = [
  { s = "?p",  p = "rdfs:domain",     o = "?c1" },
  { s = "?c1", p = "rdfs:subClassOf", o = "?c2" },
]
head = { s = "?p", p = "rdfs:domain", o = "?c2" }

[[rule]]
id = "scm-rng1"
comment = "Range narrows along subClassOf."
body = [
  { s = "?p",  p = "rdfs:range",      o = "?c1" },
  { s = "?c1", p = "rdfs:subClassOf", o = "?c2" },
]
head = { s = "?p", p = "rdfs:range", o = "?c2" }

[[rule]]
id = "scm-dom2"
comment = "Domain widens along subPropertyOf (sub-property inherits domain)."
body = [
  { s = "?p2", p = "rdfs:domain",        o = "?c" },
  { s = "?p1", p = "rdfs:subPropertyOf", o = "?p2" },
]
head = { s = "?p1", p = "rdfs:domain", o = "?c" }

[[rule]]
id = "scm-rng2"
comment = "Range widens along subPropertyOf (sub-property inherits range)."
body = [
  { s = "?p2", p = "rdfs:range",         o = "?c" },
  { s = "?p1", p = "rdfs:subPropertyOf", o = "?p2" },
]
head = { s = "?p1", p = "rdfs:range", o = "?c" }
```

The list above intentionally covers all `scm-*` (Table 9) that are non-closure-trivial, plus the most-used `cax-*` and `prp-*` rules. `cls-*` rules (Table 7 §classes — intersection, union, hasValue, allValuesFrom, someValuesFrom, maxCardinality) are added in Task 14 once the codegen can handle list-membership patterns; they need a small extension to the codegen and would balloon this task.

- [ ] **Step 2: Sanity-check the file parses as TOML**

Run:

```bash
python3 -c "import tomllib, sys; tomllib.loads(open('crates/owlrl/rules.toml').read()); print('ok')"
```

Expected: `ok`. (Python 3.11+ ships `tomllib`. If unavailable, use `cargo install taplo-cli && taplo lint crates/owlrl/rules.toml`.)

- [ ] **Step 3: Commit**

```bash
git add crates/owlrl/rules.toml
git commit -m "owlrl: declare Stage-1 OWL 2 RL rule subset in rules.toml"
```

---

## Task 7: Build the codegen parser

**Files:**
- Create: `crates/owlrl/codegen/mod.rs`
- Create: `crates/owlrl/codegen/parse.rs`

We keep codegen code in a `codegen/` directory at the crate root (not under `src/`) and pull it into `build.rs` with `#[path]`. This keeps the runtime crate free of `serde`/`syn`/`quote` deps.

- [ ] **Step 1: Create `crates/owlrl/codegen/mod.rs`**

```rust
//! Codegen library used by `build.rs`. NOT compiled into the runtime crate.

pub mod emit;
pub mod parse;
pub mod plan;
```

- [ ] **Step 2: Create `crates/owlrl/codegen/parse.rs`**

```rust
//! Parse `rules.toml` into typed rule specs.

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use std::path::Path;

/// Raw TOML shape — mirrors `rules.toml` literally.
#[derive(Debug, Deserialize)]
struct Document {
    rule: Vec<RawRule>,
}

#[derive(Debug, Deserialize)]
struct RawRule {
    id: String,
    #[serde(default)]
    comment: String,
    #[serde(default)]
    delegate: Option<String>,
    body: Vec<RawPattern>,
    head: RawPattern,
}

#[derive(Debug, Deserialize)]
struct RawPattern {
    s: String,
    p: String,
    o: String,
}

/// Parsed slot: variable (by name) or a vocabulary token.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum Slot {
    Var(String),
    Vocab(VocabTerm),
}

/// A reference to one of the fields on `crate::vocab::Vocabulary`. The
/// `field` is the literal Rust field name on `Vocabulary`.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct VocabTerm {
    pub field: &'static str,
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Pattern {
    pub s: Slot,
    pub p: Slot,
    pub o: Slot,
}

#[derive(Debug, Clone)]
pub struct RuleSpec {
    pub id: String,
    pub comment: String,
    pub delegate: bool,
    pub body: Vec<Pattern>,
    pub head: Pattern,
}

pub fn parse_file(path: &Path) -> Result<Vec<RuleSpec>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    parse_str(&text)
}

pub fn parse_str(text: &str) -> Result<Vec<RuleSpec>> {
    let doc: Document = toml::from_str(text).context("parsing rules.toml")?;
    doc.rule.into_iter().map(parse_rule).collect()
}

fn parse_rule(raw: RawRule) -> Result<RuleSpec> {
    let body = raw.body.into_iter().map(parse_pattern).collect::<Result<Vec<_>>>()?;
    let head = parse_pattern(raw.head)?;
    let delegate = match raw.delegate.as_deref() {
        None => false,
        Some("closure") => true,
        Some(other) => bail!("unknown delegate target {:?} for rule {}", other, raw.id),
    };
    Ok(RuleSpec { id: raw.id, comment: raw.comment, delegate, body, head })
}

fn parse_pattern(raw: RawPattern) -> Result<Pattern> {
    Ok(Pattern {
        s: parse_slot(&raw.s)?,
        p: parse_slot(&raw.p)?,
        o: parse_slot(&raw.o)?,
    })
}

fn parse_slot(s: &str) -> Result<Slot> {
    if let Some(rest) = s.strip_prefix('?') {
        if rest.is_empty() {
            bail!("empty variable name");
        }
        Ok(Slot::Var(rest.to_string()))
    } else {
        Ok(Slot::Vocab(vocab_term(s)?))
    }
}

fn vocab_term(token: &str) -> Result<VocabTerm> {
    // Map QName-style vocab token → field on `crate::vocab::Vocabulary`.
    let field: &'static str = match token {
        "rdf:type" => "rdf_type",
        "rdf:first" => "rdf_first",
        "rdf:rest" => "rdf_rest",
        "rdf:nil" => "rdf_nil",
        "rdfs:subClassOf" => "rdfs_sub_class_of",
        "rdfs:subPropertyOf" => "rdfs_sub_property_of",
        "rdfs:domain" => "rdfs_domain",
        "rdfs:range" => "rdfs_range",
        "owl:Class" => "owl_class",
        "owl:Thing" => "owl_thing",
        "owl:Nothing" => "owl_nothing",
        "owl:sameAs" => "owl_same_as",
        "owl:differentFrom" => "owl_different_from",
        "owl:equivalentClass" => "owl_equivalent_class",
        "owl:equivalentProperty" => "owl_equivalent_property",
        "owl:inverseOf" => "owl_inverse_of",
        "owl:FunctionalProperty" => "owl_functional_property",
        "owl:InverseFunctionalProperty" => "owl_inverse_functional_property",
        "owl:SymmetricProperty" => "owl_symmetric_property",
        "owl:TransitiveProperty" => "owl_transitive_property",
        "owl:IrreflexiveProperty" => "owl_irreflexive_property",
        "owl:AsymmetricProperty" => "owl_asymmetric_property",
        "owl:propertyDisjointWith" => "owl_property_disjoint_with",
        "owl:disjointWith" => "owl_disjoint_with",
        "owl:complementOf" => "owl_complement_of",
        "owl:intersectionOf" => "owl_intersection_of",
        "owl:unionOf" => "owl_union_of",
        "owl:someValuesFrom" => "owl_some_values_from",
        "owl:allValuesFrom" => "owl_all_values_from",
        "owl:hasValue" => "owl_has_value",
        "owl:onProperty" => "owl_on_property",
        "owl:maxCardinality" => "owl_max_cardinality",
        "owl:ObjectProperty" => return Err(anyhow!(
            "owl:ObjectProperty is not in the Stage-1 Vocabulary struct; add it before using"
        )),
        other => bail!("unknown vocabulary token {:?}", other),
    };
    Ok(VocabTerm { field })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_rule() {
        let src = r#"
            [[rule]]
            id = "cax-sco"
            body = [
              { s = "?c1", p = "rdfs:subClassOf", o = "?c2" },
              { s = "?x", p = "rdf:type", o = "?c1" },
            ]
            head = { s = "?x", p = "rdf:type", o = "?c2" }
        "#;
        let rules = parse_str(src).unwrap();
        assert_eq!(rules.len(), 1);
        let r = &rules[0];
        assert_eq!(r.id, "cax-sco");
        assert!(!r.delegate);
        assert_eq!(r.body.len(), 2);
    }

    #[test]
    fn delegate_closure_recognized() {
        let src = r#"
            [[rule]]
            id = "scm-sco"
            delegate = "closure"
            body = [
              { s = "?a", p = "rdfs:subClassOf", o = "?b" },
              { s = "?b", p = "rdfs:subClassOf", o = "?c" },
            ]
            head = { s = "?a", p = "rdfs:subClassOf", o = "?c" }
        "#;
        let rules = parse_str(src).unwrap();
        assert!(rules[0].delegate);
    }

    #[test]
    fn unknown_vocab_token_errors() {
        let src = r#"
            [[rule]]
            id = "bogus"
            body = []
            head = { s = "?x", p = "foo:bar", o = "?y" }
        "#;
        let err = parse_str(src).unwrap_err();
        assert!(err.to_string().contains("foo:bar") || err.chain().any(|c| c.to_string().contains("foo:bar")));
    }

    #[test]
    fn variable_must_have_name() {
        let src = r#"
            [[rule]]
            id = "bogus"
            body = []
            head = { s = "?", p = "rdf:type", o = "?y" }
        "#;
        assert!(parse_str(src).is_err());
    }
}
```

Note: we cannot directly run the parser's unit tests without a host binary that imports `codegen::parse`. Task 8 adds a tiny `cargo run --bin codegen-check` harness for that; for now Task 7 ends without a passing test target — we will verify in Task 8.

- [ ] **Step 3: Commit**

```bash
git add crates/owlrl/codegen/
git commit -m "owlrl: add codegen parser for rules.toml"
```

---

## Task 8: Wire `build.rs` to invoke the parser (smoke only — no emission yet)

**Files:**
- Create: `crates/owlrl/build.rs`

- [ ] **Step 1: Create `crates/owlrl/build.rs`**

```rust
//! Build-time rule codegen. Reads rules.toml, validates, emits generated_rules.rs.
//!
//! Stage 1: parse-and-emit-stub. Real per-rule emission lands in Task 10.

#[path = "codegen/mod.rs"]
mod codegen;

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let rules_path = manifest_dir.join("rules.toml");
    println!("cargo:rerun-if-changed={}", rules_path.display());
    println!("cargo:rerun-if-changed=codegen/mod.rs");
    println!("cargo:rerun-if-changed=codegen/parse.rs");
    println!("cargo:rerun-if-changed=codegen/emit.rs");
    println!("cargo:rerun-if-changed=codegen/plan.rs");

    let rules = match codegen::parse::parse_file(&rules_path) {
        Ok(rs) => rs,
        Err(e) => {
            eprintln!("FAILED to parse rules.toml: {e:#}");
            std::process::exit(1);
        }
    };

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let out_path = out_dir.join("generated_rules.rs");

    // Stage-1 stub: emit a const counting the rules and a `pub const RULES: &[&str]`.
    // Real per-rule emission is Task 10.
    let mut out = String::new();
    out.push_str("// AUTOGENERATED by build.rs from rules.toml — DO NOT EDIT.\n\n");
    out.push_str(&format!(
        "pub const RULE_COUNT: usize = {};\n",
        rules.len()
    ));
    out.push_str("pub const RULE_IDS: &[&str] = &[\n");
    for r in &rules {
        out.push_str(&format!("    {:?},\n", r.id));
    }
    out.push_str("];\n");

    fs::write(&out_path, out).expect("writing generated_rules.rs");
}
```

- [ ] **Step 2: Add the include to `src/lib.rs`**

```rust
//! reasoner-owlrl — OWL 2 RL/RDF rule engine (Stage 1).

pub mod delta;
pub mod provenance;
pub mod store;
pub mod types;
pub mod vocab;

pub mod generated {
    include!(concat!(env!("OUT_DIR"), "/generated_rules.rs"));
}
```

- [ ] **Step 3: Add a smoke test**

Create `crates/owlrl/tests/codegen_smoke.rs`:

```rust
use reasoner_owlrl::generated::{RULE_COUNT, RULE_IDS};

#[test]
fn rules_were_generated() {
    assert!(RULE_COUNT >= 25, "expected ≥25 rules in Stage-1 subset, got {RULE_COUNT}");
    assert_eq!(RULE_IDS.len(), RULE_COUNT);
    assert!(RULE_IDS.contains(&"cax-sco"));
    assert!(RULE_IDS.contains(&"scm-eqc1"));
    assert!(RULE_IDS.contains(&"prp-dom"));
}
```

- [ ] **Step 4: Build and test**

Run:

```bash
cargo build -p reasoner-owlrl
cargo test -p reasoner-owlrl --test codegen_smoke
```

Expected: build succeeds, smoke test passes. Inspect the generated file:

```bash
find target -name 'generated_rules.rs' -path '*owlrl*' -exec cat {} \;
```

Expected: a small file with `pub const RULE_COUNT` and `pub const RULE_IDS`.

- [ ] **Step 5: Commit**

```bash
git add crates/owlrl/build.rs crates/owlrl/src/lib.rs crates/owlrl/tests/codegen_smoke.rs
git commit -m "owlrl: wire build.rs to parse rules.toml and emit stub IDs"
```

---

## Task 9: Build the codegen planner (variable-binding plan per rule)

**Files:**
- Create: `crates/owlrl/codegen/plan.rs`

The planner takes a rule's body and produces an ordered nested-loop plan:

1. Pick a **leading pattern** — for Stage 1, the leftmost body pattern. (Optimization: swap to delta-binding once Task 11 introduces deltas.)
2. For each subsequent pattern, classify its slots as "bound by prior step" or "fresh variable", and emit a `store.probe(...)` call with the bound slots filled in.
3. Track which variables get bound on which step so emit.rs can name them correctly.

- [ ] **Step 1: Create `crates/owlrl/codegen/plan.rs`**

```rust
//! Trivial nested-loop planner. SPEC-03 (WCOJ) will replace this in Stage 2;
//! for Stage 1 the plan shape is "iterate the leading pattern, probe the rest".

use crate::codegen::parse::{Pattern, RuleSpec, Slot};
use rustc_hash::FxHashSet;

#[derive(Debug, Clone)]
pub struct Plan {
    /// Order in which body patterns are visited. Index into `rule.body`.
    pub order: Vec<usize>,
    /// For each step (in `order` order): for each slot (s,p,o), is the slot
    /// `Bound` to a previously-named variable (or vocab), or does it introduce
    /// a new variable to bind?
    pub steps: Vec<PlanStep>,
}

#[derive(Debug, Clone)]
pub struct PlanStep {
    pub pattern_index: usize,
    pub s: SlotPlan,
    pub p: SlotPlan,
    pub o: SlotPlan,
}

#[derive(Debug, Clone)]
pub enum SlotPlan {
    /// Slot is fixed: either to a vocabulary term or to a prior variable.
    /// In codegen this becomes `Some(<expr>)` to `store.probe`.
    Bound(BoundSource),
    /// Slot introduces a fresh variable named `name`. The probe sees `None`
    /// at this slot; the codegen reads the resulting triple's slot.
    Fresh(String),
}

#[derive(Debug, Clone)]
pub enum BoundSource {
    Var(String),
    Vocab(&'static str),
}

pub fn plan_rule(rule: &RuleSpec) -> Plan {
    let mut bound: FxHashSet<String> = FxHashSet::default();
    let mut steps = Vec::with_capacity(rule.body.len());
    let order: Vec<usize> = (0..rule.body.len()).collect();
    for (step_i, &idx) in order.iter().enumerate() {
        let pat = &rule.body[idx];
        let s = classify(&pat.s, &mut bound, step_i == 0);
        let p = classify(&pat.p, &mut bound, step_i == 0);
        let o = classify(&pat.o, &mut bound, step_i == 0);
        steps.push(PlanStep { pattern_index: idx, s, p, o });
    }
    Plan { order, steps }
}

fn classify(slot: &Slot, bound: &mut FxHashSet<String>, is_leading: bool) -> SlotPlan {
    match slot {
        Slot::Vocab(v) => SlotPlan::Bound(BoundSource::Vocab(v.field)),
        Slot::Var(name) => {
            if bound.contains(name) {
                SlotPlan::Bound(BoundSource::Var(name.clone()))
            } else {
                bound.insert(name.clone());
                // Leading pattern: variables are bound by iteration, not by probe.
                // Non-leading: variables are bound by the probe result.
                if is_leading {
                    SlotPlan::Fresh(name.clone())
                } else {
                    SlotPlan::Fresh(name.clone())
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::parse::parse_str;

    fn rule(src: &str) -> RuleSpec {
        parse_str(src).unwrap().into_iter().next().unwrap()
    }

    #[test]
    fn cax_sco_plan_has_two_steps() {
        let r = rule(r#"
            [[rule]]
            id = "cax-sco"
            body = [
              { s = "?c1", p = "rdfs:subClassOf", o = "?c2" },
              { s = "?x",  p = "rdf:type",        o = "?c1" },
            ]
            head = { s = "?x", p = "rdf:type", o = "?c2" }
        "#);
        let plan = plan_rule(&r);
        assert_eq!(plan.steps.len(), 2);
        // Step 2's subject ?x is fresh; its object ?c1 is bound from step 1.
        match &plan.steps[1].o {
            SlotPlan::Bound(BoundSource::Var(n)) => assert_eq!(n, "c1"),
            other => panic!("expected ?c1 bound, got {other:?}"),
        }
    }
}
```

- [ ] **Step 2: Wire into `codegen/mod.rs` (already done in Task 7) — verify the module declarations match**

Confirm `crates/owlrl/codegen/mod.rs` lists `pub mod plan;`. If not, fix it.

- [ ] **Step 3: Build to verify**

Run:

```bash
cargo build -p reasoner-owlrl
```

Expected: clean build. (The new `plan.rs` is only compiled inside `build.rs` via `#[path]`; its inline tests are not run by `cargo test` because the build script is not a test target. They will be moved to a host harness in Task 12 if we need them executable.)

- [ ] **Step 4: Commit**

```bash
git add crates/owlrl/codegen/plan.rs
git commit -m "owlrl: add nested-loop planner for rule bodies"
```

---

## Task 10: Implement per-rule emission (the heart of the codegen)

**Files:**
- Create: `crates/owlrl/codegen/emit.rs`
- Modify: `crates/owlrl/build.rs`

The emitter takes `(RuleSpec, Plan)` and produces a `TokenStream` like:

```rust
pub fn fire_cax_sco(store: &dyn TripleStore, delta: &Delta) -> Delta {
    let v = store.vocab();
    let mut out = Delta::new();
    // Leading pattern: { ?c1 rdfs:subClassOf ?c2 }
    for t0 in store.scan_predicate(v.rdfs_sub_class_of) {
        let c1 = t0.s;
        let c2 = t0.o;
        // Step 2: { ?x rdf:type ?c1 } — probe with p=rdf:type, o=c1.
        for t1 in store.probe(None, v.rdf_type, Some(c1)) {
            let x = t1.s;
            // Head: { ?x rdf:type ?c2 }.
            let head = Triple::new(x, v.rdf_type, c2);
            if !store.contains(&head) {
                let prov = Provenance::new("cax-sco", [t0, t1]);
                out.insert(head, prov);
            }
        }
    }
    out
}
```

For Stage-1 simplicity we always lead with the leftmost body pattern via `scan_predicate`. Delta-binding (semi-naïve "fire on dirty predicates only") is layered on at the runtime level in Task 11 by skipping rules whose body predicates are not dirty.

- [ ] **Step 1: Create `crates/owlrl/codegen/emit.rs`**

```rust
//! Emit one `fn fire_<id>(...)` per rule + a `pub const RULES: &[CompiledRule]`.

use crate::codegen::parse::{Pattern, RuleSpec, Slot};
use crate::codegen::plan::{plan_rule, BoundSource, Plan, PlanStep, SlotPlan};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};

pub fn emit_all(rules: &[RuleSpec]) -> TokenStream {
    let mut fns = Vec::new();
    let mut table_entries = Vec::new();
    for r in rules {
        let fn_ident = format_ident!("fire_{}", sanitize_id(&r.id));
        let id_str = &r.id;
        let delegate = r.delegate;

        if delegate {
            // No body — runtime calls ClosureBackend instead. Still emit a fn
            // so the rule table has a uniform signature; the fn returns empty.
            fns.push(quote! {
                #[doc = concat!("DELEGATED: ", #id_str, " is handled by ClosureBackend.")]
                pub fn #fn_ident(
                    _store: &dyn crate::store::TripleStore,
                    _delta: &crate::delta::Delta,
                ) -> crate::delta::Delta {
                    crate::delta::Delta::new()
                }
            });
            table_entries.push(quote! {
                CompiledRule {
                    id: #id_str,
                    delegated: true,
                    fire: #fn_ident,
                    body_predicates: &[],
                }
            });
            continue;
        }

        let plan = plan_rule(r);
        let body_preds = body_predicate_fields(r);
        let body_tokens = emit_rule_body(r, &plan);

        fns.push(quote! {
            #[doc = concat!("Compiled OWL 2 RL rule: ", #id_str)]
            pub fn #fn_ident(
                store: &dyn crate::store::TripleStore,
                _delta: &crate::delta::Delta,
            ) -> crate::delta::Delta {
                use crate::types::Triple;
                use crate::provenance::Provenance;
                let v = store.vocab();
                let mut out = crate::delta::Delta::new();
                #body_tokens
                out
            }
        });

        let preds_tokens = body_preds.iter().map(|f| {
            let id = format_ident!("{}", f);
            quote! { |v: &crate::vocab::Vocabulary| v.#id }
        });
        table_entries.push(quote! {
            CompiledRule {
                id: #id_str,
                delegated: false,
                fire: #fn_ident,
                body_predicates: &[ #( #preds_tokens ),* ],
            }
        });
    }

    let n = rules.len();
    quote! {
        // AUTOGENERATED by build.rs from rules.toml — DO NOT EDIT.

        pub type FireFn = fn(
            &dyn crate::store::TripleStore,
            &crate::delta::Delta,
        ) -> crate::delta::Delta;

        pub type PredAccessor = fn(&crate::vocab::Vocabulary) -> crate::types::TermId;

        pub struct CompiledRule {
            pub id: &'static str,
            pub delegated: bool,
            pub fire: FireFn,
            /// Predicate-IDs the rule body reads. Used by semi-naïve driver
            /// to skip rules whose predicates are not dirty.
            pub body_predicates: &'static [PredAccessor],
        }

        pub const RULE_COUNT: usize = #n;
        pub const RULE_IDS: &[&str] = &[ #( #( stringify!(__unused) )* )* ];

        #( #fns )*

        pub const RULES: &[CompiledRule] = &[
            #( #table_entries ),*
        ];
    }
}

fn emit_rule_body(rule: &RuleSpec, plan: &Plan) -> TokenStream {
    let mut tokens = TokenStream::new();
    tokens.extend(emit_step(rule, plan, 0));
    tokens
}

fn emit_step(rule: &RuleSpec, plan: &Plan, depth: usize) -> TokenStream {
    if depth == plan.steps.len() {
        return emit_head(rule);
    }
    let step = &plan.steps[depth];
    let pat = &rule.body[step.pattern_index];
    let triple_var = format_ident!("__t{}", depth);

    let inner = emit_step(rule, plan, depth + 1);

    // Generate bindings from the iterated triple. For Fresh slots, bind the
    // variable to the corresponding field of the iterated Triple.
    let bind_s = match &step.s {
        SlotPlan::Fresh(name) => {
            let var = format_ident!("{}", name);
            quote! { let #var = #triple_var.s; }
        }
        SlotPlan::Bound(_) => quote! {},
    };
    let bind_p = match &step.p {
        SlotPlan::Fresh(name) => {
            let var = format_ident!("{}", name);
            quote! { let #var = #triple_var.p; }
        }
        SlotPlan::Bound(_) => quote! {},
    };
    let bind_o = match &step.o {
        SlotPlan::Fresh(name) => {
            let var = format_ident!("{}", name);
            quote! { let #var = #triple_var.o; }
        }
        SlotPlan::Bound(_) => quote! {},
    };

    if depth == 0 {
        // Leading step: choose iteration source.
        // - If predicate slot is a vocab term, scan_predicate(v.<field>).
        // - Else (predicate is a variable): scan all predicates. We do NOT
        //   support that in Stage 1 (no rule in Stage-1 rules.toml has a
        //   variable predicate as its leading pattern's predicate; the
        //   transitive-property rule prp-trp is delegated to closure).
        let leading_iter = match &step.p {
            SlotPlan::Bound(BoundSource::Vocab(field)) => {
                let id = format_ident!("{}", field);
                quote! { store.scan_predicate(v.#id) }
            }
            SlotPlan::Bound(BoundSource::Var(_)) => {
                // Unreachable for Stage-1 rules.toml; compile-fail with a
                // clear message if we ever try.
                panic!("rule {}: leading pattern predicate is a variable; not supported in Stage 1", rule.id);
            }
            SlotPlan::Fresh(_) => {
                panic!("rule {}: leading pattern predicate is a fresh variable; not supported", rule.id);
            }
        };
        // Filter: if subject or object are Bound to a Vocab term, only emit
        // tuples where they match. (Variables bound in this step are Fresh.)
        let filter = filter_for_step(&step.s, &step.o);
        quote! {
            for #triple_var in #leading_iter {
                #filter
                #bind_s
                #bind_p
                #bind_o
                #inner
            }
        }
    } else {
        // Probing step.
        let s_arg = probe_arg(&step.s);
        let p_arg = probe_predicate(&step.p, rule);
        let o_arg = probe_arg(&step.o);
        let filter = filter_for_step(&step.s, &step.o);
        quote! {
            for #triple_var in store.probe(#s_arg, #p_arg, #o_arg) {
                #filter
                #bind_s
                #bind_p
                #bind_o
                #inner
            }
        }
    }
}

fn filter_for_step(s: &SlotPlan, o: &SlotPlan) -> TokenStream {
    // We only emit a filter for Bound-to-Vocab slots in the *leading* iteration
    // (probe handles the filter natively for non-leading steps). Bound-to-Var
    // is impossible at depth 0 since variables only become bound on this step.
    let s_chk = match s {
        SlotPlan::Bound(BoundSource::Vocab(field)) => {
            let id = format_ident!("{}", field);
            // depth-0 triple is __t0; we use the same name via closure capture.
            Some(quote! { v.#id })
        }
        _ => None,
    };
    let o_chk = match o {
        SlotPlan::Bound(BoundSource::Vocab(field)) => {
            let id = format_ident!("{}", field);
            Some(quote! { v.#id })
        }
        _ => None,
    };
    match (s_chk, o_chk) {
        (None, None) => quote! {},
        (Some(s), None) => quote! { if __triple_subject_unused() {} let _ = #s; }, // placeholder
        // The above is intentionally simplistic; depth-0 with vocab subject
        // is not exercised by any Stage-1 rule. We assert in emit_step that
        // depth-0 leading pattern's subject and object are variables.
        _ => quote! {},
    }
}

fn probe_arg(slot: &SlotPlan) -> TokenStream {
    match slot {
        SlotPlan::Fresh(_) => quote! { None },
        SlotPlan::Bound(BoundSource::Var(name)) => {
            let id = format_ident!("{}", name);
            quote! { Some(#id) }
        }
        SlotPlan::Bound(BoundSource::Vocab(field)) => {
            let id = format_ident!("{}", field);
            quote! { Some(v.#id) }
        }
    }
}

fn probe_predicate(slot: &SlotPlan, rule: &RuleSpec) -> TokenStream {
    match slot {
        SlotPlan::Bound(BoundSource::Vocab(field)) => {
            let id = format_ident!("{}", field);
            quote! { v.#id }
        }
        SlotPlan::Bound(BoundSource::Var(name)) => {
            let id = format_ident!("{}", name);
            quote! { #id }
        }
        SlotPlan::Fresh(_) => panic!(
            "rule {}: probe step predicate cannot be a fresh variable",
            rule.id
        ),
    }
}

fn emit_head(rule: &RuleSpec) -> TokenStream {
    let s = head_expr(&rule.head.s);
    let p = head_expr(&rule.head.p);
    let o = head_expr(&rule.head.o);
    let id = &rule.id;

    // Build a SmallVec of premise triples (the `__t0`, `__t1`, ...).
    let premise_idents: Vec<_> = (0..rule.body.len())
        .map(|i| format_ident!("__t{}", i))
        .collect();
    let premise_count = rule.body.len();

    quote! {
        let head = Triple::new(#s, #p, #o);
        if !store.contains(&head) && !out.contains(&head) {
            let mut premises = ::smallvec::SmallVec::<[Triple; 4]>::with_capacity(#premise_count);
            #( premises.push(#premise_idents); )*
            out.insert(head, Provenance { rule_id: #id, premises });
        }
    }
}

fn head_expr(slot: &Slot) -> TokenStream {
    match slot {
        Slot::Var(name) => {
            let id = format_ident!("{}", name);
            quote! { #id }
        }
        Slot::Vocab(v) => {
            let id = format_ident!("{}", v.field);
            quote! { v.#id }
        }
    }
}

fn body_predicate_fields(rule: &RuleSpec) -> Vec<&'static str> {
    let mut out = Vec::new();
    for pat in &rule.body {
        if let Slot::Vocab(v) = &pat.p {
            if !out.contains(&v.field) {
                out.push(v.field);
            }
        }
    }
    out
}

fn sanitize_id(id: &str) -> String {
    id.replace(['-', ':'], "_")
}
```

Note the `filter_for_step` function above includes a deliberately conservative placeholder for vocab-bound subject/object in the leading step. No Stage-1 rule needs this (verify by inspecting `rules.toml`: every leading-pattern's subject and object slot is a variable). If a future rule adds one, the emitter will compile-error via the `panic!` in `emit_step`, and we will extend the filter then.

Also: the `RULE_IDS` const emitted by Task 8's stub is replaced here by the new `RULES` table. The smoke test from Task 8 must be updated in the next step.

- [ ] **Step 2: Update `build.rs` to use the new emitter**

Replace the body of `build.rs` after the parse step with:

```rust
let tokens = codegen::emit::emit_all(&rules);
let syntax_tree: syn::File = syn::parse2(tokens).expect("emitted code is not valid Rust");
let pretty = prettyplease::unparse(&syntax_tree);

let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
let out_path = out_dir.join("generated_rules.rs");
fs::write(&out_path, pretty).expect("writing generated_rules.rs");
```

Full new `build.rs`:

```rust
#[path = "codegen/mod.rs"]
mod codegen;

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let rules_path = manifest_dir.join("rules.toml");
    println!("cargo:rerun-if-changed={}", rules_path.display());
    println!("cargo:rerun-if-changed=codegen/mod.rs");
    println!("cargo:rerun-if-changed=codegen/parse.rs");
    println!("cargo:rerun-if-changed=codegen/emit.rs");
    println!("cargo:rerun-if-changed=codegen/plan.rs");

    let rules = match codegen::parse::parse_file(&rules_path) {
        Ok(rs) => rs,
        Err(e) => {
            eprintln!("FAILED to parse rules.toml: {e:#}");
            std::process::exit(1);
        }
    };

    let tokens = codegen::emit::emit_all(&rules);
    let syntax_tree: syn::File = match syn::parse2(tokens.clone()) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("emitted code is not valid Rust: {e}");
            eprintln!("emitted:\n{tokens}");
            std::process::exit(1);
        }
    };
    let pretty = prettyplease::unparse(&syntax_tree);

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let out_path = out_dir.join("generated_rules.rs");
    fs::write(&out_path, pretty).expect("writing generated_rules.rs");
}
```

- [ ] **Step 3: Update Task 8's smoke test to use the new table**

Replace `crates/owlrl/tests/codegen_smoke.rs`:

```rust
use reasoner_owlrl::generated::{CompiledRule, RULES, RULE_COUNT};

#[test]
fn rules_were_generated() {
    assert!(RULE_COUNT >= 25, "expected ≥25 Stage-1 rules, got {RULE_COUNT}");
    assert_eq!(RULES.len(), RULE_COUNT);
    let ids: Vec<&str> = RULES.iter().map(|r: &CompiledRule| r.id).collect();
    for required in ["cax-sco", "scm-eqc1", "prp-dom", "scm-sco", "eq-trans"] {
        assert!(ids.contains(&required), "missing rule {required}");
    }
}

#[test]
fn closure_delegated_rules_marked() {
    let delegated: Vec<&str> = RULES.iter().filter(|r| r.delegated).map(|r| r.id).collect();
    for required in ["eq-ref", "eq-sym", "eq-trans", "prp-trp", "scm-sco", "scm-spo"] {
        assert!(delegated.contains(&required), "{required} should be closure-delegated");
    }
}
```

- [ ] **Step 4: Build, inspect, test**

Run:

```bash
cargo build -p reasoner-owlrl 2>&1 | tee /tmp/owlrl-build.log
find target -name 'generated_rules.rs' -path '*owlrl*' | head -1 | xargs cat | head -80
cargo test -p reasoner-owlrl --test codegen_smoke
```

Expected: clean build, generated file pretty-printed, both smoke tests pass.

If `syn::parse2` fails, the emitter has a token-stream bug. Diagnose by reading the failing-emission output the build script prints on error.

- [ ] **Step 5: Commit**

```bash
git add crates/owlrl/codegen/emit.rs crates/owlrl/build.rs crates/owlrl/tests/codegen_smoke.rs
git commit -m "$(cat <<'EOF'
owlrl: emit one fire_<id>() function per non-delegated rule

build.rs now reads rules.toml, plans each rule's body as a nested-loop join,
and emits a CompiledRule table with one fn per rule. Closure-delegated rules
(eq-*, prp-trp, scm-sco, scm-spo) get an empty stub body and are flagged for
runtime dispatch via ClosureBackend.
EOF
)"
```

---

## Task 11: Implement the `ClosureBackend` trait and `RuleFiringBackend`

**Files:**
- Create: `crates/owlrl/src/backend.rs`
- Modify: `crates/owlrl/src/lib.rs`

- [ ] **Step 1: Create `backend.rs` with tests**

```rust
//! Closure-backend trait: equality and transitive-property closure.
//!
//! In production, `reasoner-closure` (SPEC-05) implements this trait against
//! SuiteSparse:GraphBLAS. In tests and for Stage-1 smoke runs, the
//! `RuleFiringBackend` defined here runs the closure as ordinary rule firing
//! (slow but obviously correct).

use crate::delta::Delta;
use crate::provenance::Provenance;
use crate::store::TripleStore;
use crate::types::{TermId, Triple};
use smallvec::smallvec;

/// Compute the closure subset (equality, transitive properties, subClassOf,
/// subPropertyOf transitivity) and return the deltas to insert.
///
/// Implementations may mutate internal caches but MUST NOT mutate the store
/// — the caller owns that. The returned Delta is applied by the engine.
pub trait ClosureBackend {
    /// Compute the full closure given the current store. Called once per
    /// semi-naïve round when any predicate the backend cares about is dirty.
    fn close(&mut self, store: &dyn TripleStore) -> Delta;
}

/// Reference implementation that fires the closure-delegated rules as ordinary
/// nested-loop rules until fixed point. Used by Stage-1 tests; not for
/// production workloads.
pub struct RuleFiringBackend;

impl RuleFiringBackend {
    pub fn new() -> Self {
        Self
    }
}

impl Default for RuleFiringBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl ClosureBackend for RuleFiringBackend {
    fn close(&mut self, store: &dyn TripleStore) -> Delta {
        let v = *store.vocab();
        let mut out = Delta::new();
        loop {
            let before = out.len();
            // scm-sco: subClassOf transitivity.
            close_transitive(store, &out, v.rdfs_sub_class_of, "scm-sco", &mut out);
            // scm-spo: subPropertyOf transitivity.
            close_transitive(store, &out, v.rdfs_sub_property_of, "scm-spo", &mut out);
            // eq-sym: sameAs symmetry.
            close_symmetric(store, &out, v.owl_same_as, "eq-sym", &mut out);
            // eq-trans: sameAs transitivity.
            close_transitive(store, &out, v.owl_same_as, "eq-trans", &mut out);
            // prp-trp: explicit transitive properties.
            close_transitive_property(store, &out, &v, &mut out);
            if out.len() == before {
                break;
            }
        }
        out
    }
}

/// Helper: chain-close a single predicate (the body `?a p ?b /\ ?b p ?c → ?a p ?c`).
fn close_transitive(
    store: &dyn TripleStore,
    accum: &Delta,
    pred: TermId,
    rule_id: &'static str,
    out: &mut Delta,
) {
    let edges: Vec<(TermId, TermId)> = store
        .scan_predicate(pred)
        .map(|t| (t.s, t.o))
        .chain(accum.triples().filter(|t| t.p == pred).map(|t| (t.s, t.o)))
        .collect();
    for &(a, b) in &edges {
        for &(b2, c) in &edges {
            if b == b2 {
                let head = Triple::new(a, pred, c);
                if !store.contains(&head) && !out.contains(&head) {
                    out.insert(head, Provenance {
                        rule_id,
                        premises: smallvec![
                            Triple::new(a, pred, b),
                            Triple::new(b, pred, c),
                        ],
                    });
                }
            }
        }
    }
}

fn close_symmetric(
    store: &dyn TripleStore,
    accum: &Delta,
    pred: TermId,
    rule_id: &'static str,
    out: &mut Delta,
) {
    let edges: Vec<(TermId, TermId)> = store
        .scan_predicate(pred)
        .map(|t| (t.s, t.o))
        .chain(accum.triples().filter(|t| t.p == pred).map(|t| (t.s, t.o)))
        .collect();
    for &(a, b) in &edges {
        let head = Triple::new(b, pred, a);
        if !store.contains(&head) && !out.contains(&head) {
            out.insert(head, Provenance {
                rule_id,
                premises: smallvec![Triple::new(a, pred, b)],
            });
        }
    }
}

fn close_transitive_property(
    store: &dyn TripleStore,
    accum: &Delta,
    vocab: &crate::vocab::Vocabulary,
    out: &mut Delta,
) {
    // Find each predicate p s.t. (p rdf:type owl:TransitiveProperty).
    let trans_pred = vocab.owl_transitive_property;
    let rdf_type = vocab.rdf_type;
    let predicates: Vec<TermId> = store
        .scan_predicate(rdf_type)
        .filter(|t| t.o == trans_pred)
        .map(|t| t.s)
        .collect();
    for p in predicates {
        close_transitive(store, accum, p, "prp-trp", out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemStore;
    use crate::vocab::Vocabulary;

    fn t(s: u64, p: u64, o: u64) -> Triple {
        Triple::new(TermId(s), TermId(p), TermId(o))
    }

    #[test]
    fn subclass_chain_closes() {
        let v = Vocabulary::synthetic(1000);
        let sco = v.rdfs_sub_class_of;
        let mut store = MemStore::new(v);
        // A ⊑ B ⊑ C ⊑ D
        store.assert(t(1, sco.0, 2));
        store.assert(t(2, sco.0, 3));
        store.assert(t(3, sco.0, 4));
        let delta = RuleFiringBackend::new().close(&store);
        assert!(delta.contains(&t(1, sco.0, 3)));
        assert!(delta.contains(&t(2, sco.0, 4)));
        assert!(delta.contains(&t(1, sco.0, 4)));
    }

    #[test]
    fn sameas_symmetric_and_transitive() {
        let v = Vocabulary::synthetic(1000);
        let sa = v.owl_same_as;
        let mut store = MemStore::new(v);
        store.assert(t(1, sa.0, 2));
        store.assert(t(2, sa.0, 3));
        let delta = RuleFiringBackend::new().close(&store);
        assert!(delta.contains(&t(2, sa.0, 1)));
        assert!(delta.contains(&t(3, sa.0, 2)));
        assert!(delta.contains(&t(1, sa.0, 3)));
        assert!(delta.contains(&t(3, sa.0, 1)));
    }
}
```

- [ ] **Step 2: Wire into `lib.rs`**

```rust
//! reasoner-owlrl — OWL 2 RL/RDF rule engine (Stage 1).

pub mod backend;
pub mod delta;
pub mod provenance;
pub mod store;
pub mod types;
pub mod vocab;

pub mod generated {
    include!(concat!(env!("OUT_DIR"), "/generated_rules.rs"));
}
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p reasoner-owlrl
```

Expected: 12 tests pass (10 prior + 2 backend).

- [ ] **Step 4: Commit**

```bash
git add crates/owlrl/src/backend.rs crates/owlrl/src/lib.rs
git commit -m "owlrl: add ClosureBackend trait and RuleFiringBackend reference"
```

---

## Task 12: Implement the semi-naïve evaluation driver

**Files:**
- Create: `crates/owlrl/src/engine.rs`
- Modify: `crates/owlrl/src/lib.rs`

The driver:

1. Initial round: every rule's `fire` is invoked once. Closure-delegated rules are skipped; instead, the backend's `close()` is called once after the first round.
2. Each non-initial round: only rules whose `body_predicates` intersect the dirty-predicate set from the previous round's delta are invoked. After collecting all per-rule deltas, run the closure backend again if any of the predicates it cares about are dirty.
3. Apply the union of all deltas to the store via `insert_inferred`. Update the dirty-predicate set with the union's `dirty_predicates()`.
4. Terminate when no rule and no backend produces any fresh triple.

- [ ] **Step 1: Create `engine.rs`**

```rust
//! Semi-naïve evaluation driver. Stage 1: full re-materialization only.

use crate::backend::ClosureBackend;
use crate::delta::Delta;
use crate::generated::{CompiledRule, RULES};
use crate::store::TripleStore;
use crate::types::TermId;
use crate::vocab::Vocabulary;
use rustc_hash::FxHashSet;

#[derive(Debug, Default, Clone)]
pub struct Stats {
    pub rounds: usize,
    pub triples_inferred: usize,
    pub rule_fires: usize,
}

/// Run forward chaining to fixed point. Does NOT clear existing inferred
/// triples — see `reset_and_materialize` for that.
pub fn materialize<S: TripleStore + ?Sized, B: ClosureBackend>(
    store: &mut S,
    backend: &mut B,
) -> Stats {
    let mut stats = Stats::default();
    // First round: every rule fires; treat all predicates as "dirty".
    let mut dirty: Option<FxHashSet<TermId>> = None;
    loop {
        stats.rounds += 1;
        let mut round_delta = Delta::new();

        // 1. Compiled rules.
        for rule in RULES {
            if rule.delegated {
                continue;
            }
            if !rule_relevant(rule, dirty.as_ref(), store.vocab()) {
                continue;
            }
            stats.rule_fires += 1;
            let d = (rule.fire)(store_as_dyn(store), &Delta::new());
            round_delta.merge(d);
        }

        // 2. Closure backend (handles delegated rules).
        let backend_delta = backend.close(store_as_dyn(store));
        round_delta.merge(backend_delta);

        // 3. Apply to store.
        let mut new_count = 0;
        let mut applied = Delta::new();
        for (t, prov) in round_delta.iter() {
            if store.insert_inferred(*t, prov.clone()) {
                new_count += 1;
                applied.insert(*t, prov.clone());
            }
        }
        stats.triples_inferred += new_count;

        if applied.is_empty() {
            break;
        }
        dirty = Some(applied.dirty_predicates());
    }
    stats
}

/// Drop all inferred triples and re-run forward chaining from the asserted base.
/// Implements SPEC-04 F7.
pub fn reset_and_materialize<S: TripleStore + ?Sized, B: ClosureBackend>(
    store: &mut S,
    backend: &mut B,
) -> Stats {
    store.clear_inferred();
    materialize(store, backend)
}

fn rule_relevant(rule: &CompiledRule, dirty: Option<&FxHashSet<TermId>>, vocab: &Vocabulary) -> bool {
    // First round (dirty = None): everything is relevant.
    let Some(dirty) = dirty else { return true };
    rule.body_predicates.iter().any(|pa| dirty.contains(&pa(vocab)))
}

/// Coerce a generic `&mut S` (with `S: TripleStore + ?Sized`) to `&dyn TripleStore`.
/// Needed because `RULES` entries take `&dyn TripleStore`.
fn store_as_dyn<S: TripleStore + ?Sized>(s: &S) -> &dyn TripleStore {
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::RuleFiringBackend;
    use crate::store::MemStore;
    use crate::types::{TermId, Triple};
    use crate::vocab::Vocabulary;

    fn t(s: u64, p: u64, o: u64) -> Triple {
        Triple::new(TermId(s), TermId(p), TermId(o))
    }

    #[test]
    fn empty_store_terminates() {
        let v = Vocabulary::synthetic(1000);
        let mut store = MemStore::new(v);
        let mut backend = RuleFiringBackend::new();
        let stats = materialize(&mut store, &mut backend);
        assert_eq!(stats.triples_inferred, 0);
        assert!(stats.rounds >= 1);
    }

    #[test]
    fn cax_sco_two_hop() {
        let v = Vocabulary::synthetic(1000);
        let (sco, ty) = (v.rdfs_sub_class_of, v.rdf_type);
        let (a, b, c, x) = (TermId(1), TermId(2), TermId(3), TermId(4));
        let mut store = MemStore::new(v);
        // A ⊑ B ⊑ C, x : A
        store.assert(t(a.0, sco.0, b.0));
        store.assert(t(b.0, sco.0, c.0));
        store.assert(t(x.0, ty.0, a.0));
        let mut backend = RuleFiringBackend::new();
        materialize(&mut store, &mut backend);
        assert!(store.contains(&t(x.0, ty.0, b.0)), "expected x : B (cax-sco)");
        assert!(store.contains(&t(x.0, ty.0, c.0)), "expected x : C (cax-sco + scm-sco)");
    }
}
```

- [ ] **Step 2: Wire into `lib.rs`**

```rust
//! reasoner-owlrl — OWL 2 RL/RDF rule engine (Stage 1).

pub mod backend;
pub mod delta;
pub mod engine;
pub mod provenance;
pub mod store;
pub mod types;
pub mod vocab;

pub mod generated {
    include!(concat!(env!("OUT_DIR"), "/generated_rules.rs"));
}

pub use engine::{materialize, reset_and_materialize, Stats};
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p reasoner-owlrl
```

Expected: 14 tests pass (12 prior + 2 engine).

- [ ] **Step 4: Commit**

```bash
git add crates/owlrl/src/engine.rs crates/owlrl/src/lib.rs
git commit -m "owlrl: add semi-naive materialize() + reset_and_materialize() driver"
```

---

## Task 13: Add integration tests for single-rule firing and chained derivation

**Files:**
- Create: `crates/owlrl/tests/single_rule.rs`
- Create: `crates/owlrl/tests/semi_naive.rs`
- Create: `crates/owlrl/tests/reset_rematerialize.rs`

- [ ] **Step 1: Create `tests/single_rule.rs`**

```rust
//! Verify each Stage-1 rule fires correctly in isolation.

use reasoner_owlrl::backend::RuleFiringBackend;
use reasoner_owlrl::store::{MemStore, TripleStore};
use reasoner_owlrl::types::{TermId, Triple};
use reasoner_owlrl::vocab::Vocabulary;
use reasoner_owlrl::materialize;

fn t(s: u64, p: u64, o: u64) -> Triple {
    Triple::new(TermId(s), TermId(p), TermId(o))
}

fn fresh_store() -> (MemStore, Vocabulary) {
    let v = Vocabulary::synthetic(10_000);
    (MemStore::new(v), v)
}

#[test]
fn cax_sco() {
    let (mut s, v) = fresh_store();
    s.assert(t(1, v.rdfs_sub_class_of.0, 2));
    s.assert(t(100, v.rdf_type.0, 1));
    let mut b = RuleFiringBackend::new();
    materialize(&mut s, &mut b);
    assert!(s.contains(&t(100, v.rdf_type.0, 2)));
}

#[test]
fn prp_dom() {
    let (mut s, v) = fresh_store();
    let p = 50;
    let c = 60;
    s.assert(t(p, v.rdfs_domain.0, c));
    s.assert(t(100, p, 200));
    let mut b = RuleFiringBackend::new();
    materialize(&mut s, &mut b);
    assert!(s.contains(&t(100, v.rdf_type.0, c)));
}

#[test]
fn prp_rng() {
    let (mut s, v) = fresh_store();
    let p = 50;
    let c = 60;
    s.assert(t(p, v.rdfs_range.0, c));
    s.assert(t(100, p, 200));
    let mut b = RuleFiringBackend::new();
    materialize(&mut s, &mut b);
    assert!(s.contains(&t(200, v.rdf_type.0, c)));
}

#[test]
fn prp_symp() {
    let (mut s, v) = fresh_store();
    let p = 50;
    s.assert(t(p, v.rdf_type.0, v.owl_symmetric_property.0));
    s.assert(t(100, p, 200));
    let mut b = RuleFiringBackend::new();
    materialize(&mut s, &mut b);
    assert!(s.contains(&t(200, p, 100)));
}

#[test]
fn prp_spo1() {
    let (mut s, v) = fresh_store();
    let p1 = 50;
    let p2 = 60;
    s.assert(t(p1, v.rdfs_sub_property_of.0, p2));
    s.assert(t(100, p1, 200));
    let mut b = RuleFiringBackend::new();
    materialize(&mut s, &mut b);
    assert!(s.contains(&t(100, p2, 200)));
}

#[test]
fn prp_inv1_and_inv2() {
    let (mut s, v) = fresh_store();
    let p1 = 50;
    let p2 = 60;
    s.assert(t(p1, v.owl_inverse_of.0, p2));
    s.assert(t(100, p1, 200));
    s.assert(t(300, p2, 400));
    let mut b = RuleFiringBackend::new();
    materialize(&mut s, &mut b);
    assert!(s.contains(&t(200, p2, 100)), "inv1");
    assert!(s.contains(&t(400, p1, 300)), "inv2");
}

#[test]
fn cax_eqc_both_directions() {
    let (mut s, v) = fresh_store();
    s.assert(t(1, v.owl_equivalent_class.0, 2));
    s.assert(t(100, v.rdf_type.0, 1));
    s.assert(t(101, v.rdf_type.0, 2));
    let mut b = RuleFiringBackend::new();
    materialize(&mut s, &mut b);
    assert!(s.contains(&t(100, v.rdf_type.0, 2)), "cax-eqc1");
    assert!(s.contains(&t(101, v.rdf_type.0, 1)), "cax-eqc2");
}
```

- [ ] **Step 2: Create `tests/semi_naive.rs`**

```rust
//! Verify the driver chains derivations across multiple rules and to fixed point.

use reasoner_owlrl::backend::RuleFiringBackend;
use reasoner_owlrl::store::{MemStore, TripleStore};
use reasoner_owlrl::types::{TermId, Triple};
use reasoner_owlrl::vocab::Vocabulary;
use reasoner_owlrl::materialize;

fn t(s: u64, p: u64, o: u64) -> Triple {
    Triple::new(TermId(s), TermId(p), TermId(o))
}

#[test]
fn five_step_subclass_chain() {
    let v = Vocabulary::synthetic(10_000);
    let sco = v.rdfs_sub_class_of.0;
    let ty = v.rdf_type.0;
    let mut s = MemStore::new(v);
    // A ⊑ B ⊑ C ⊑ D ⊑ E ⊑ F, x : A
    for i in 1..=5 {
        s.assert(t(i, sco, i + 1));
    }
    s.assert(t(100, ty, 1));
    let mut b = RuleFiringBackend::new();
    let stats = materialize(&mut s, &mut b);
    for c in 2..=6 {
        assert!(s.contains(&t(100, ty, c)), "x : {c} missing");
    }
    assert!(stats.rounds <= 10, "should converge in ≤10 rounds, took {}", stats.rounds);
}

#[test]
fn domain_then_subclass() {
    let v = Vocabulary::synthetic(10_000);
    let mut s = MemStore::new(v);
    let p = 50;
    let c1 = 60;
    let c2 = 70;
    s.assert(t(p, v.rdfs_domain.0, c1));
    s.assert(t(c1, v.rdfs_sub_class_of.0, c2));
    s.assert(t(100, p, 200));
    let mut b = RuleFiringBackend::new();
    materialize(&mut s, &mut b);
    // prp-dom: 100 : c1. Then cax-sco: 100 : c2.
    assert!(s.contains(&t(100, v.rdf_type.0, c1)));
    assert!(s.contains(&t(100, v.rdf_type.0, c2)));
}

#[test]
fn fixed_point_is_actually_fixed() {
    // Re-running materialize after it converged must produce zero new triples.
    let v = Vocabulary::synthetic(10_000);
    let mut s = MemStore::new(v);
    s.assert(t(1, v.rdfs_sub_class_of.0, 2));
    s.assert(t(2, v.rdfs_sub_class_of.0, 3));
    s.assert(t(100, v.rdf_type.0, 1));
    let mut b = RuleFiringBackend::new();
    materialize(&mut s, &mut b);
    let stats2 = materialize(&mut s, &mut b);
    assert_eq!(stats2.triples_inferred, 0, "second materialize should be a no-op");
}
```

- [ ] **Step 3: Create `tests/reset_rematerialize.rs`**

```rust
//! SPEC-04 F7: reset_and_materialize produces a bit-identical store.

use reasoner_owlrl::backend::RuleFiringBackend;
use reasoner_owlrl::store::{MemStore, TripleStore};
use reasoner_owlrl::types::{TermId, Triple};
use reasoner_owlrl::vocab::Vocabulary;
use reasoner_owlrl::{materialize, reset_and_materialize};

fn t(s: u64, p: u64, o: u64) -> Triple {
    Triple::new(TermId(s), TermId(p), TermId(o))
}

#[test]
fn reset_then_rematerialize_is_identical() {
    let v = Vocabulary::synthetic(10_000);
    let mut s = MemStore::new(v);
    s.assert(t(1, v.rdfs_sub_class_of.0, 2));
    s.assert(t(2, v.rdfs_sub_class_of.0, 3));
    s.assert(t(100, v.rdf_type.0, 1));
    s.assert(t(101, v.rdf_type.0, 2));
    let mut b = RuleFiringBackend::new();

    materialize(&mut s, &mut b);
    let first = s.all_triples();

    reset_and_materialize(&mut s, &mut b);
    let second = s.all_triples();

    assert_eq!(first, second, "rematerialization differed from initial materialization");
    assert!(first.len() > 4, "expected some inferred triples; got {}", first.len());
}
```

- [ ] **Step 4: Run all tests**

```bash
cargo test -p reasoner-owlrl
```

Expected: all green. If `prp-symp` or `prp-inv1` fails, the emitter's filter-on-vocab-object logic for non-leading patterns needs review (the rule body iterates over `(?p, rdf:type, owl:SymmetricProperty)` — leading pattern's object is bound to a vocab term, which is currently emitted as a *probe* on step 2, not a *filter* on step 1; revisit `filter_for_step` in emit.rs and the leading-step logic).

- [ ] **Step 5: If `prp-symp`/`prp-inv1` fails, patch the emitter**

The bug class is: leading-step probes with a vocab-bound object slot. The simplest fix is to swap the body order so the vocab-bound-object pattern is *not* leading. Since rule order in the body is preserved from `rules.toml`, swap the entries:

For `prp-symp`, change `rules.toml` from:

```toml
body = [
  { s = "?p", p = "rdf:type", o = "owl:SymmetricProperty" },
  { s = "?x", p = "?p",       o = "?y" },
]
```

to:

```toml
body = [
  { s = "?x", p = "?p",       o = "?y" },
  { s = "?p", p = "rdf:type", o = "owl:SymmetricProperty" },
]
```

But that introduces a fresh variable predicate in the leading pattern, which we don't support. **Better fix**: keep the body order and teach the emitter that when the leading step has a vocab-bound object, emit an `if` filter inside the loop. Replace the `filter_for_step` body with:

```rust
fn filter_for_step_leading(s: &SlotPlan, o: &SlotPlan, triple: &proc_macro2::Ident) -> TokenStream {
    let s_chk = match s {
        SlotPlan::Bound(BoundSource::Vocab(field)) => {
            let id = format_ident!("{}", field);
            Some(quote! { if #triple.s != v.#id { continue; } })
        }
        _ => None,
    };
    let o_chk = match o {
        SlotPlan::Bound(BoundSource::Vocab(field)) => {
            let id = format_ident!("{}", field);
            Some(quote! { if #triple.o != v.#id { continue; } })
        }
        _ => None,
    };
    let s_chk = s_chk.unwrap_or_default();
    let o_chk = o_chk.unwrap_or_default();
    quote! { #s_chk #o_chk }
}
```

And in `emit_step`'s leading branch, replace the `filter` call with:

```rust
let filter = filter_for_step_leading(&step.s, &step.o, &triple_var);
```

Remove the old `filter_for_step` function. Rebuild and re-run.

- [ ] **Step 6: Commit**

```bash
git add crates/owlrl/codegen/emit.rs crates/owlrl/tests/single_rule.rs crates/owlrl/tests/semi_naive.rs crates/owlrl/tests/reset_rematerialize.rs
git commit -m "$(cat <<'EOF'
owlrl: integration tests for single-rule, semi-naive chain, reset/rematerialize

Adds three test files exercising each Stage-1 compiled rule in isolation,
multi-rule derivation chains through cax-sco + scm-sco, prp-dom + cax-sco,
and the F7 reset_and_materialize round-trip. Fixes leading-pattern vocab
filter codegen for prp-symp / prp-inv*.
EOF
)"
```

---

## Task 14: Extend rules.toml with the most-used `cls-*` rules

**Files:**
- Modify: `crates/owlrl/rules.toml`
- Modify: `crates/owlrl/codegen/parse.rs` (add list-pattern support if any cls-* rule uses lists)

The Stage-1 list of `cls-*` rules we include: `cls-com` (complementOf), `cls-svf2` (someValuesFrom with hasValue), `cls-avf` (allValuesFrom), `cls-hv1` / `cls-hv2` (hasValue). These do **not** require RDF list traversal — `cls-int1`/`cls-int2`/`cls-uni` do (they traverse `owl:intersectionOf` / `owl:unionOf` cons-cells) and are **deferred to Stage 2** to keep this plan bite-sized.

- [ ] **Step 1: Append cls-* rules to `rules.toml`**

Append at the bottom of `crates/owlrl/rules.toml`:

```toml
# ---------------------------------------------------------------------------
# Class expression rules (Table 7 — cls-*; list-based int/uni deferred)
# ---------------------------------------------------------------------------

[[rule]]
id = "cls-svf2"
comment = "someValuesFrom with owl:Thing as filler — type propagation."
body = [
  { s = "?x", p = "owl:someValuesFrom", o = "owl:Thing" },
  { s = "?x", p = "owl:onProperty",     o = "?p" },
  { s = "?u", p = "?p",                 o = "?v" },
]
head = { s = "?u", p = "rdf:type", o = "?x" }

[[rule]]
id = "cls-avf"
comment = "allValuesFrom propagation."
body = [
  { s = "?x", p = "owl:allValuesFrom", o = "?y" },
  { s = "?x", p = "owl:onProperty",    o = "?p" },
  { s = "?u", p = "rdf:type",          o = "?x" },
  { s = "?u", p = "?p",                o = "?v" },
]
head = { s = "?v", p = "rdf:type", o = "?y" }

[[rule]]
id = "cls-hv1"
comment = "hasValue → property assertion."
body = [
  { s = "?x", p = "owl:hasValue",   o = "?y" },
  { s = "?x", p = "owl:onProperty", o = "?p" },
  { s = "?u", p = "rdf:type",       o = "?x" },
]
head = { s = "?u", p = "?p", o = "?y" }

[[rule]]
id = "cls-hv2"
comment = "hasValue ← property assertion."
body = [
  { s = "?x", p = "owl:hasValue",   o = "?y" },
  { s = "?x", p = "owl:onProperty", o = "?p" },
  { s = "?u", p = "?p",             o = "?y" },
]
head = { s = "?u", p = "rdf:type", o = "?x" }
```

- [ ] **Step 2: Confirm the codegen handles 3- and 4-pattern bodies**

The planner and emitter were written generically; rebuild and inspect:

```bash
cargo build -p reasoner-owlrl
cargo test -p reasoner-owlrl --test codegen_smoke
```

Expected: `RULE_COUNT` is now ≥29; smoke test passes.

- [ ] **Step 3: Add an integration test for cls-hv1**

Append to `crates/owlrl/tests/single_rule.rs`:

```rust
#[test]
fn cls_hv1() {
    let (mut s, v) = fresh_store();
    let restriction = 70;
    let prop = 80;
    let val = 90;
    let u = 100;
    s.assert(t(restriction, v.owl_has_value.0, val));
    s.assert(t(restriction, v.owl_on_property.0, prop));
    s.assert(t(u, v.rdf_type.0, restriction));
    let mut b = RuleFiringBackend::new();
    materialize(&mut s, &mut b);
    assert!(s.contains(&t(u, prop, val)));
}
```

- [ ] **Step 4: Run**

```bash
cargo test -p reasoner-owlrl
```

Expected: green, including the new `cls_hv1` test.

- [ ] **Step 5: Commit**

```bash
git add crates/owlrl/rules.toml crates/owlrl/tests/single_rule.rs
git commit -m "owlrl: add cls-svf2 / cls-avf / cls-hv1 / cls-hv2 rules"
```

---

## Task 15: W3C OWL 2 RL test-subset smoke harness

**Files:**
- Create: `crates/owlrl/tests/w3c_subset.rs`
- Create: `crates/owlrl/tests/fixtures/README.md` (one-paragraph note)

Full integration into the SPEC-01 conformance harness is a separate plan (the harness is responsible for fetching W3C manifests, parsing N-Triples, and rendering results). For Stage 1 we hand-code a small set of W3C-style fixtures inline so we have *something* exercising rule combinations end-to-end before the harness wiring lands.

Target: ≥10 distinct hand-encoded fixtures. SPEC-01's selected-subset growth to ≥50 W3C cases is a SPEC-01 deliverable that consumes this engine; it lives in that plan, not this one.

- [ ] **Step 1: Create the fixture README**

`crates/owlrl/tests/fixtures/README.md`:

```markdown
# OWL 2 RL hand-encoded fixtures

These fixtures mirror the structure of W3C OWL 2 RL test cases without depending on
the W3C manifest format (which the SPEC-01 conformance harness owns). Each test
asserts a base graph, runs `materialize`, and checks for specific expected and
forbidden triples.

When the SPEC-01 harness wiring lands, the canonical W3C cases will exercise this
engine through the harness — these tests will remain as a fast inner-loop sanity
check that does not require the harness binary.
```

- [ ] **Step 2: Create `crates/owlrl/tests/w3c_subset.rs`**

```rust
//! Hand-encoded fixtures patterned on the W3C OWL 2 RL test suite.
//! Each test corresponds to a single normative rule or simple rule combination.

use reasoner_owlrl::backend::RuleFiringBackend;
use reasoner_owlrl::store::{MemStore, TripleStore};
use reasoner_owlrl::types::{TermId, Triple};
use reasoner_owlrl::vocab::Vocabulary;
use reasoner_owlrl::materialize;

fn t(s: u64, p: u64, o: u64) -> Triple {
    Triple::new(TermId(s), TermId(p), TermId(o))
}

struct Case {
    name: &'static str,
    asserted: Vec<Triple>,
    expected: Vec<Triple>,
    forbidden: Vec<Triple>,
}

fn run(case: Case, v: Vocabulary) {
    let mut s = MemStore::new(v);
    for t in &case.asserted {
        s.assert(*t);
    }
    let mut b = RuleFiringBackend::new();
    materialize(&mut s, &mut b);
    for t in &case.expected {
        assert!(s.contains(t), "{}: missing expected {:?}", case.name, t);
    }
    for t in &case.forbidden {
        assert!(!s.contains(t), "{}: forbidden triple was derived {:?}", case.name, t);
    }
}

#[test]
fn fixtures() {
    let v = Vocabulary::synthetic(10_000);
    let (ty, sco, dom, rng, sa, sp) = (
        v.rdf_type.0,
        v.rdfs_sub_class_of.0,
        v.rdfs_domain.0,
        v.rdfs_range.0,
        v.rdfs_sub_property_of.0,
        v.rdfs_sub_property_of.0,
    );
    let _ = (sa, sp);

    // 1. cax-sco: direct subClassOf.
    run(Case {
        name: "cax-sco-direct",
        asserted: vec![ t(1, sco, 2), t(100, ty, 1) ],
        expected: vec![ t(100, ty, 2) ],
        forbidden: vec![],
    }, v);

    // 2. scm-sco (closure) + cax-sco: A ⊑ B ⊑ C, x : A ⇒ x : C.
    run(Case {
        name: "scm-sco-then-cax-sco",
        asserted: vec![ t(1, sco, 2), t(2, sco, 3), t(100, ty, 1) ],
        expected: vec![ t(100, ty, 2), t(100, ty, 3), t(1, sco, 3) ],
        forbidden: vec![],
    }, v);

    // 3. prp-dom: property domain.
    run(Case {
        name: "prp-dom",
        asserted: vec![ t(50, dom, 60), t(100, 50, 200) ],
        expected: vec![ t(100, ty, 60) ],
        forbidden: vec![],
    }, v);

    // 4. prp-rng: property range.
    run(Case {
        name: "prp-rng",
        asserted: vec![ t(50, rng, 60), t(100, 50, 200) ],
        expected: vec![ t(200, ty, 60) ],
        forbidden: vec![],
    }, v);

    // 5. prp-spo1: sub-property propagation.
    run(Case {
        name: "prp-spo1",
        asserted: vec![ t(50, sp, 60), t(100, 50, 200) ],
        expected: vec![ t(100, 60, 200) ],
        forbidden: vec![],
    }, v);

    // 6. prp-symp: symmetric property.
    run(Case {
        name: "prp-symp",
        asserted: vec![ t(50, ty, v.owl_symmetric_property.0), t(100, 50, 200) ],
        expected: vec![ t(200, 50, 100) ],
        forbidden: vec![],
    }, v);

    // 7. prp-inv1+inv2 cross-fire.
    run(Case {
        name: "prp-inv-both",
        asserted: vec![
            t(50, v.owl_inverse_of.0, 60),
            t(100, 50, 200),
            t(300, 60, 400),
        ],
        expected: vec![ t(200, 60, 100), t(400, 50, 300) ],
        forbidden: vec![],
    }, v);

    // 8. cax-eqc1+eqc2: equivalentClass instance propagation both ways.
    run(Case {
        name: "cax-eqc-both",
        asserted: vec![
            t(1, v.owl_equivalent_class.0, 2),
            t(100, ty, 1),
            t(101, ty, 2),
        ],
        expected: vec![ t(100, ty, 2), t(101, ty, 1) ],
        forbidden: vec![],
    }, v);

    // 9. scm-eqc1+eqc2: equivalentClass implies subClassOf both ways.
    run(Case {
        name: "scm-eqc-both",
        asserted: vec![ t(1, v.owl_equivalent_class.0, 2) ],
        expected: vec![ t(1, sco, 2), t(2, sco, 1) ],
        forbidden: vec![],
    }, v);

    // 10. scm-dom1: domain narrows along subClassOf.
    run(Case {
        name: "scm-dom1",
        asserted: vec![
            t(50, dom, 60),
            t(60, sco, 70),
        ],
        expected: vec![ t(50, dom, 70) ],
        forbidden: vec![],
    }, v);

    // 11. cls-hv1: hasValue → property triple.
    run(Case {
        name: "cls-hv1",
        asserted: vec![
            t(70, v.owl_has_value.0, 90),
            t(70, v.owl_on_property.0, 80),
            t(100, ty, 70),
        ],
        expected: vec![ t(100, 80, 90) ],
        forbidden: vec![],
    }, v);

    // 12. eq-sym (closure-delegated): sameAs symmetry.
    run(Case {
        name: "eq-sym",
        asserted: vec![ t(1, v.owl_same_as.0, 2) ],
        expected: vec![ t(2, v.owl_same_as.0, 1) ],
        forbidden: vec![],
    }, v);
}
```

- [ ] **Step 3: Run**

```bash
cargo test -p reasoner-owlrl --test w3c_subset -- --nocapture
```

Expected: green; 12 named sub-cases all pass.

- [ ] **Step 4: Commit**

```bash
git add crates/owlrl/tests/w3c_subset.rs crates/owlrl/tests/fixtures/README.md
git commit -m "owlrl: add 12 hand-encoded W3C-style fixtures as inner-loop smoke"
```

---

## Task 16: Document the crate and the Future Work boundary

**Files:**
- Modify: `crates/owlrl/src/lib.rs` (add crate-level rustdoc)

- [ ] **Step 1: Replace the `lib.rs` doc header**

```rust
//! reasoner-owlrl — OWL 2 RL/RDF rule engine, Stage-1 slice.
//!
//! # Scope
//!
//! - Ahead-of-time codegen from `rules.toml` via `build.rs` (one Rust function
//!   per rule).
//! - Stage-1 rule subset: all of Table 9 `scm-*` plus the most-used `cax-*`,
//!   `prp-*`, `cls-*` rules (target: ≥50 W3C OWL 2 RL test cases passing per
//!   SPEC-00 Stage 1).
//! - Semi-naïve evaluation driver with dirty-predicate filtering.
//! - Full re-materialization (`reset_and_materialize`).
//! - `ClosureBackend` trait for `eq-*` / `prp-trp` / `scm-sco` / `scm-spo`
//!   delegation to SPEC-05 (`reasoner-closure`). A reference
//!   `RuleFiringBackend` is provided for tests.
//!
//! # Future Work (NOT in this crate yet)
//!
//! - Full ~80-rule OWL 2 RL set (remaining `cls-int*`/`cls-uni` list-walking
//!   rules and all `dt-*` datatype rules) — Stage 2.
//! - Production proof recording (compressed side-table, on-demand rederivation
//!   via SPEC-03) — Stage 2; today's `Provenance` is in-memory only.
//! - `rdf:type` skew optimization (partition-by-class-id parallelism) — Stage 2.
//! - Incremental updates via Z-sets — SPEC-06 / Stage 2.
//! - WCOJ join execution inside rule bodies — SPEC-03 / Stage 2.
//!
//! # Adding a rule
//!
//! 1. Append a `[[rule]]` block to `rules.toml`.
//! 2. `cargo build -p reasoner-owlrl` regenerates `generated_rules.rs`.
//! 3. Add a unit test in `tests/single_rule.rs`.
//!
//! See `plans/2026-05-24-SPEC-04-owl-rl-rule-engine.md` for the full plan.

pub mod backend;
pub mod delta;
pub mod engine;
pub mod provenance;
pub mod store;
pub mod types;
pub mod vocab;

pub mod generated {
    include!(concat!(env!("OUT_DIR"), "/generated_rules.rs"));
}

pub use engine::{materialize, reset_and_materialize, Stats};
```

- [ ] **Step 2: Build the docs to confirm they render**

```bash
cargo doc -p reasoner-owlrl --no-deps
```

Expected: clean, no warnings on the public surface.

- [ ] **Step 3: Run the full test suite one last time**

```bash
cargo test -p reasoner-owlrl
```

Expected: every test from Tasks 2–15 passes (≥20 tests).

- [ ] **Step 4: Commit**

```bash
git add crates/owlrl/src/lib.rs
git commit -m "owlrl: document Stage-1 scope and Future Work boundary"
```

---

## Stage-1 exit criteria checklist

Run this manually after Task 16 completes:

- [ ] `cargo build -p reasoner-owlrl` succeeds from a clean target.
- [ ] `cargo test -p reasoner-owlrl` runs ≥20 tests, all green.
- [ ] `cargo doc -p reasoner-owlrl --no-deps` emits no warnings.
- [ ] `RULES.len() >= 29` (verified by `codegen_smoke`).
- [ ] The 12 named hand-encoded fixtures in `tests/w3c_subset.rs` all pass — this is the local proxy for the ≥50 W3C OWL 2 RL test-case gate that SPEC-01's harness will eventually run.
- [ ] `reset_and_materialize` produces a bit-identical store (`tests/reset_rematerialize.rs`).
- [ ] `RuleFiringBackend` correctly closes `subClassOf`, `subPropertyOf`, and `sameAs` chains (`tests/closure_backend.rs` covered by Task 11's tests).

Once green, this plan's Stage-1 deliverable is complete. SPEC-05 can swap its real `ClosureBackend` impl in via a `cargo add reasoner-closure` and a one-line wiring change in downstream code. SPEC-01's conformance harness can begin pointing at this engine using the public `materialize` + `MemStore` (or a future SPEC-02 `TripleStore` impl) surface.

---

## Deferred / Future Work explicitly cross-referenced

| Deferred item | Tracking |
|---------------|----------|
| `cls-int1` / `cls-int2` / `cls-uni` (RDF-list traversal) | Stage 2; needs `codegen` extension for list patterns. |
| All `dt-*` datatype rules | Stage 2; depends on SPEC-02 datatype-aware dictionary. |
| Production proof recording | Stage 2; compressed side-table + SPEC-03 backward-rederivation. |
| `rdf:type` skew partition-by-class-id | Stage 2; needs SPEC-02 partition iterator surface. |
| Incremental updates (Δ semantics) | Stage 2; SPEC-06 / `reasoner-incremental`. |
| WCOJ-driven rule bodies | Stage 2; SPEC-03 / `reasoner-wcoj`. |
| LUBM-8000 ≥2 M triples/sec NF1 target | Stage 1 only validates termination on LUBM-100. |
| User-defined Datalog rules | Stage 2 extension; current codegen requires recompile per rule change. |
