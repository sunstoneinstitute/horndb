---
status: executed
date: 2026-05-24
scope: "SPEC-08 ML/LLM Integration Boundary — Stage 0/1"
---

# SPEC-08 ML/LLM Integration Boundary — Stage 0/1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Establish the ML integration boundary as a set of pluggable Rust traits with reference no-op implementations, a provenance-source enum that other crates can attach to triples, a library-level audit log, and a global `ml.enabled` feature flag — such that with ML disabled the engine behaves bit-identically to a non-ML build.

**Architecture:** SPEC-08 is deliberately small and cross-cutting. We define traits (`CandidateGenerator`, `PlanAdvisor`, `HotSetAdvisor`) and supporting value types (`Confidence`, `TripleSubject`, `SubplanShape`, `PlanAdvice`, `MlProvenance`) inside `horndb-ml`. We ship no-op `Disabled*` implementations. We expose an `MlConfig` with `enabled: bool` plus a thread-safe `MlRegistry` that returns either the registered impl or the no-op fallback. We add an in-memory `MlAuditLog` (library API now; HTTP in Stage 2). We do **not** modify any other crate; instead, the plan ends by emitting a `INTEGRATION-NOTES.md` per consuming crate that records which trait method to call from which integration point.

**Tech Stack:** Rust 1.93+, workspace crate `horndb-ml`. No new external crates required for Stage 0/1 beyond `anyhow`, `thiserror`, and the std-lib (`Arc`, `RwLock`, `Mutex`). LLM endpoint, FAISS-backed `CandidateGenerator`, HTTP audit endpoint, cost reporting, and training-data leakage controls are deferred to Stage 2.

**Stage 0/1 Scope:**
- F1 `CandidateGenerator` trait + `DisabledCandidateGenerator` no-op.
- F2 `PlanAdvisor` trait + `DisabledPlanAdvisor` no-op.
- F4 `HotSetAdvisor` trait + `DisabledHotSetAdvisor` no-op.
- F5 `MlProvenance` annotation type (an enum source-tag) that storage crates will attach as an optional column; we ship the enum and the schema-level constants — wiring into the storage column belongs to SPEC-02's plan.
- F6 `MlAuditLog` library API — record + paginate. HTTP endpoint deferred.
- NF1 `MlConfig { enabled: bool }` and `MlRegistry` with hot-reload via `RwLock`; with `enabled=false`, every accessor returns the no-op impl.
- Integration coordination: write `INTEGRATION-NOTES.md` files under each consuming crate's directory describing the exact call site and signature (no code changes there).

**Deferred to Stage 2+ (Future Work):**
- F3 LLM → SPARQL HTTP endpoint (`POST /nl-query`).
- Real FAISS-backed `CandidateGenerator`.
- HTTP `GET /ml-audit?since=` endpoint (Stage 1 provides the library-level log).
- Training-data leakage / privacy controls.
- Per-query LLM cost reporting.
- Confidence-calibration policy (Stage 1 default: "always queue for review" — no auto-commit policy yet).

---

## File Structure

We create one new crate body — `horndb-ml` — split into focused modules. Files that change together live together.

```
crates/ml/
├── Cargo.toml                       # update — add anyhow, thiserror, chrono
├── src/
│   ├── lib.rs                       # re-exports + crate-level docs
│   ├── config.rs                    # MlConfig, MlConfigError
│   ├── types.rs                     # Confidence, TripleSubject, SubplanShape, PlanAdvice, ModelId
│   ├── provenance.rs                # MlProvenance enum + serialization constants
│   ├── candidate.rs                 # CandidateGenerator trait + DisabledCandidateGenerator
│   ├── planner.rs                   # PlanAdvisor trait + DisabledPlanAdvisor
│   ├── hotset.rs                    # HotSetAdvisor trait + DisabledHotSetAdvisor
│   ├── audit.rs                     # MlAuditLog + MlAuditEntry + AuditPage
│   └── registry.rs                  # MlRegistry — central accessor, hot-reload
└── tests/
    ├── disabled_is_identity.rs      # NF1: with enabled=false everything no-ops
    ├── registry_hot_reload.rs       # acceptance #5: runtime enable/disable
    └── audit_pagination.rs          # F6: audit log returns paginated entries

crates/storage/INTEGRATION-NOTES.md       # new — call site: MlProvenance column
crates/wcoj/INTEGRATION-NOTES.md          # new — call site: PlanAdvisor before plan
crates/owlrl/INTEGRATION-NOTES.md         # new — call site: CandidateGenerator staging
crates/closure/INTEGRATION-NOTES.md       # new — call site: candidate sameAs re-verify
crates/sparql/INTEGRATION-NOTES.md        # new — call site: PlanAdvisor + LLM endpoint
```

**Boundary discipline:** No edits land in other crates. The `INTEGRATION-NOTES.md` files are markdown only — they describe where future SPEC-02/03/04/05/07 plans will call our APIs.

---

## Task 1: Update `horndb-ml` Cargo.toml with minimal deps

**Files:**
- Modify: `crates/ml/Cargo.toml`

- [ ] **Step 1: Inspect the current Cargo.toml**

Run: `cat crates/ml/Cargo.toml`
Expected output:
```
[package]
name = "horndb-ml"
version = "0.0.0"
edition.workspace = true
license.workspace = true
publish = false

[dependencies]
```

- [ ] **Step 2: Replace with the Stage 0/1 deps**

Overwrite `crates/ml/Cargo.toml` with:

```toml
[package]
name = "horndb-ml"
version = "0.0.0"
edition.workspace = true
license.workspace = true
publish = false

[dependencies]
anyhow = { workspace = true }
thiserror = { workspace = true }
chrono = { version = "0.4", default-features = false, features = ["clock", "serde"] }

[dev-dependencies]
# Tests use only std + chrono; nothing extra needed at Stage 1.
```

Note: `chrono` is needed for `MlAuditEntry.timestamp`. We disable default features to keep the dep surface tight.

- [ ] **Step 3: Verify it builds**

Run: `cargo build -p horndb-ml`
Expected: `Compiling horndb-ml v0.0.0` then `Finished ... profile [unoptimized + debuginfo]`. No errors.

- [ ] **Step 4: Commit**

```bash
git add crates/ml/Cargo.toml
git commit -m "$(cat <<'EOF'
ml: add minimal deps (anyhow, thiserror, chrono) for SPEC-08 stage 0/1

Trait definitions and no-op implementations only — no HTTP, no FAISS,
no LLM client at this stage. chrono is for MlAuditEntry timestamps.
EOF
)"
```

---

## Task 2: Define `Confidence` newtype with TDD

**Files:**
- Create: `crates/ml/src/types.rs`
- Modify: `crates/ml/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/ml/src/types.rs` with only the test module first:

```rust
//! Shared value types for the ML integration boundary.
//!
//! Kept dependency-free so consuming crates can construct these
//! without dragging in anything ML-specific.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confidence_clamps_to_unit_interval() {
        assert_eq!(Confidence::new(0.5).value(), 0.5);
        assert_eq!(Confidence::new(-0.1).value(), 0.0);
        assert_eq!(Confidence::new(1.7).value(), 1.0);
        assert_eq!(Confidence::new(f64::NAN).value(), 0.0);
    }

    #[test]
    fn confidence_zero_and_one_helpers() {
        assert_eq!(Confidence::zero().value(), 0.0);
        assert_eq!(Confidence::one().value(), 1.0);
    }

    #[test]
    fn confidence_is_ordered() {
        assert!(Confidence::new(0.1) < Confidence::new(0.9));
    }
}
```

- [ ] **Step 2: Wire types module into lib.rs and run test to verify failure**

Overwrite `crates/ml/src/lib.rs`:

```rust
//! horndb-ml — ML/LLM integration boundary (SPEC-08).
//!
//! The symbolic reasoner is the source of truth; this crate's traits
//! exist so external ML systems can *propose* facts (re-verified
//! symbolically) and *advise* the planner. With `MlConfig.enabled =
//! false` the engine behaves bit-identically to a non-ML build.

pub mod types;
```

Run: `cargo test -p horndb-ml --lib types::tests`
Expected: FAIL with errors like `cannot find type 'Confidence' in this scope`.

- [ ] **Step 3: Implement `Confidence`**

Replace `crates/ml/src/types.rs` with:

```rust
//! Shared value types for the ML integration boundary.
//!
//! Kept dependency-free so consuming crates can construct these
//! without dragging in anything ML-specific.

/// A confidence score in the closed interval [0.0, 1.0].
///
/// Constructed via [`Confidence::new`], which clamps out-of-range
/// and NaN inputs. We deliberately do *not* implement `Eq`; use
/// `PartialOrd` / `partial_cmp` for comparisons.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Confidence(f64);

impl Confidence {
    pub fn new(v: f64) -> Self {
        if v.is_nan() {
            Confidence(0.0)
        } else if v < 0.0 {
            Confidence(0.0)
        } else if v > 1.0 {
            Confidence(1.0)
        } else {
            Confidence(v)
        }
    }

    pub fn zero() -> Self {
        Confidence(0.0)
    }

    pub fn one() -> Self {
        Confidence(1.0)
    }

    pub fn value(self) -> f64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confidence_clamps_to_unit_interval() {
        assert_eq!(Confidence::new(0.5).value(), 0.5);
        assert_eq!(Confidence::new(-0.1).value(), 0.0);
        assert_eq!(Confidence::new(1.7).value(), 1.0);
        assert_eq!(Confidence::new(f64::NAN).value(), 0.0);
    }

    #[test]
    fn confidence_zero_and_one_helpers() {
        assert_eq!(Confidence::zero().value(), 0.0);
        assert_eq!(Confidence::one().value(), 1.0);
    }

    #[test]
    fn confidence_is_ordered() {
        assert!(Confidence::new(0.1) < Confidence::new(0.9));
    }
}
```

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test -p horndb-ml --lib types::tests`
Expected: `running 3 tests ... test result: ok. 3 passed; 0 failed`.

- [ ] **Step 5: Commit**

```bash
git add crates/ml/src/lib.rs crates/ml/src/types.rs
git commit -m "$(cat <<'EOF'
ml: add Confidence newtype with clamping constructor

Confidence is the shared currency between CandidateGenerator and
the symbolic re-verification path. Clamping in the constructor lets
trait implementors hand back raw model outputs without pre-validation.
EOF
)"
```

---

## Task 3: Add `TripleSubject`, `ModelId`, `SubplanShape`, `PlanAdvice` value types

**Files:**
- Modify: `crates/ml/src/types.rs`

- [ ] **Step 1: Write the failing test cases**

Append to `crates/ml/src/types.rs` (inside the existing `#[cfg(test)] mod tests` block, before its closing `}`):

```rust
    #[test]
    fn triple_subject_variants() {
        let iri = TripleSubject::Iri("http://example.org/a".into());
        let bnode = TripleSubject::BlankNode("b1".into());
        assert_ne!(iri, bnode);
        // Round-trip via clone.
        assert_eq!(iri.clone(), iri);
    }

    #[test]
    fn model_id_string_roundtrip() {
        let m = ModelId::new("faiss-mini-lm-v6");
        assert_eq!(m.as_str(), "faiss-mini-lm-v6");
    }

    #[test]
    fn subplan_shape_constructs() {
        let shape = SubplanShape {
            n_patterns: 4,
            n_vars: 3,
            bound_vars: 1,
        };
        assert_eq!(shape.n_patterns, 4);
    }

    #[test]
    fn plan_advice_default_is_unadvised() {
        let a = PlanAdvice::unadvised();
        assert!(a.estimated_cardinality.is_none());
        assert!(a.suggested_join_order.is_empty());
    }
```

- [ ] **Step 2: Run tests to verify failure**

Run: `cargo test -p horndb-ml --lib types::tests`
Expected: FAIL with `cannot find type 'TripleSubject'`, `cannot find type 'ModelId'`, etc.

- [ ] **Step 3: Implement the new types**

Insert the following before the `#[cfg(test)] mod tests` block in `crates/ml/src/types.rs`:

```rust
/// Identity of an RDF subject as seen at the ML boundary.
///
/// We intentionally model this with owned `String`s rather than
/// dictionary IDs so this crate stays independent of SPEC-02 storage.
/// Consuming crates resolve to/from their dictionary at the call site.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TripleSubject {
    Iri(String),
    BlankNode(String),
}

/// Stable identity of an ML model contributing to the store.
///
/// Used both for provenance tagging (F5) and audit-log records (F6).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModelId(String);

impl ModelId {
    pub fn new(s: impl Into<String>) -> Self {
        ModelId(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Coarse-grained shape of a subplan offered to the [`PlanAdvisor`].
///
/// We deliberately keep this opaque: the advisor sees structural
/// numbers but not the actual triple patterns. This lets us evolve
/// the planner's internal representation (SPEC-03 / SPEC-07) without
/// breaking ML plugins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubplanShape {
    pub n_patterns: usize,
    pub n_vars: usize,
    pub bound_vars: usize,
}

/// Advice returned by a [`PlanAdvisor`].
///
/// Every field is optional: the planner treats it as a hint and
/// always falls back to its own histograms when missing or implausible.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlanAdvice {
    pub estimated_cardinality: Option<u64>,
    pub suggested_index: Option<String>,
    /// Variable indices in the suggested binding order; empty = no opinion.
    pub suggested_join_order: Vec<usize>,
}

impl PlanAdvice {
    pub fn unadvised() -> Self {
        Self::default()
    }
}
```

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test -p horndb-ml --lib types::tests`
Expected: `running 7 tests ... test result: ok. 7 passed; 0 failed`.

- [ ] **Step 5: Commit**

```bash
git add crates/ml/src/types.rs
git commit -m "$(cat <<'EOF'
ml: add TripleSubject, ModelId, SubplanShape, PlanAdvice value types

TripleSubject uses owned strings on purpose — keeps horndb-ml free
of any storage-crate dependency. SubplanShape is intentionally opaque
so the planner can evolve its internal IR without breaking advisors.
EOF
)"
```

---

## Task 4: Add `MlProvenance` enum (F5 — provenance annotation column)

**Files:**
- Create: `crates/ml/src/provenance.rs`
- Modify: `crates/ml/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/ml/src/provenance.rs`:

```rust
//! Provenance annotation attached to ML-derived triples (SPEC-08 F5).
//!
//! Storage (SPEC-02) is expected to materialize this as an optional
//! column on the triple partition; this crate owns the schema.

use crate::types::{Confidence, ModelId};

/// Where a triple came from.
///
/// `Symbolic` is the default for triples derived by rule firing or
/// closure; SPARQL planners (SPEC-07) can filter on this for audit.
#[derive(Debug, Clone, PartialEq)]
pub enum MlProvenance {
    Symbolic,
    MlDerived {
        model: ModelId,
        confidence: Confidence,
    },
}

impl MlProvenance {
    pub fn is_ml_derived(&self) -> bool {
        matches!(self, MlProvenance::MlDerived { .. })
    }

    /// Discriminant byte used by SPEC-02 when packing the provenance
    /// column. Stable across crate versions — appending a new variant
    /// must keep existing bytes intact.
    pub const SYMBOLIC_TAG: u8 = 0x00;
    pub const ML_DERIVED_TAG: u8 = 0x01;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbolic_is_not_ml_derived() {
        assert!(!MlProvenance::Symbolic.is_ml_derived());
    }

    #[test]
    fn ml_derived_is_ml_derived() {
        let p = MlProvenance::MlDerived {
            model: ModelId::new("test-model"),
            confidence: Confidence::new(0.42),
        };
        assert!(p.is_ml_derived());
    }

    #[test]
    fn tag_bytes_are_stable() {
        // SPEC-02 will pack these into a storage column — must be stable.
        assert_eq!(MlProvenance::SYMBOLIC_TAG, 0x00);
        assert_eq!(MlProvenance::ML_DERIVED_TAG, 0x01);
    }
}
```

- [ ] **Step 2: Add `provenance` module to lib.rs**

Edit `crates/ml/src/lib.rs` — replace its contents with:

```rust
//! horndb-ml — ML/LLM integration boundary (SPEC-08).
//!
//! The symbolic reasoner is the source of truth; this crate's traits
//! exist so external ML systems can *propose* facts (re-verified
//! symbolically) and *advise* the planner. With `MlConfig.enabled =
//! false` the engine behaves bit-identically to a non-ML build.

pub mod provenance;
pub mod types;
```

- [ ] **Step 3: Run tests to verify pass**

Run: `cargo test -p horndb-ml --lib provenance::tests`
Expected: `running 3 tests ... test result: ok. 3 passed; 0 failed`.

- [ ] **Step 4: Commit**

```bash
git add crates/ml/src/lib.rs crates/ml/src/provenance.rs
git commit -m "$(cat <<'EOF'
ml: add MlProvenance enum for SPEC-08 F5 (annotation column)

Stable discriminant bytes are part of the SPEC-02 storage contract;
appending variants must preserve existing tag values. SPARQL planner
filters on is_ml_derived() to support audit queries.
EOF
)"
```

---

## Task 5: Define `CandidateGenerator` trait (F1)

**Files:**
- Create: `crates/ml/src/candidate.rs`
- Modify: `crates/ml/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/ml/src/candidate.rs`:

```rust
//! Candidate-link generation (SPEC-08 F1).
//!
//! ML systems propose candidate `owl:sameAs` links between subjects.
//! Every proposal is a *hypothesis* — the engine must re-verify
//! symbolically before committing. This crate ships only the trait
//! and a no-op implementation; real implementations (e.g. FAISS) are
//! Stage 2 deliverables.

use crate::types::{Confidence, ModelId, TripleSubject};

pub trait CandidateGenerator: Send + Sync {
    /// Identity of the underlying model (stable across calls).
    fn model_id(&self) -> ModelId;

    /// Propose how confident we are that `left` and `right` denote
    /// the same entity. `Confidence::zero()` means "no opinion."
    fn propose_sameas(&self, left: &TripleSubject, right: &TripleSubject) -> Confidence;
}

/// No-op implementation used when ML is disabled (NF1).
///
/// Always returns `Confidence::zero()` — the engine will never act on
/// its proposals, and re-verification trivially rejects.
#[derive(Debug, Default)]
pub struct DisabledCandidateGenerator;

impl DisabledCandidateGenerator {
    pub const MODEL_ID: &'static str = "disabled-candidate-generator";
}

impl CandidateGenerator for DisabledCandidateGenerator {
    fn model_id(&self) -> ModelId {
        ModelId::new(Self::MODEL_ID)
    }

    fn propose_sameas(&self, _left: &TripleSubject, _right: &TripleSubject) -> Confidence {
        Confidence::zero()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_returns_zero_for_any_pair() {
        let g = DisabledCandidateGenerator;
        let a = TripleSubject::Iri("http://x/a".into());
        let b = TripleSubject::Iri("http://x/b".into());
        assert_eq!(g.propose_sameas(&a, &b).value(), 0.0);
    }

    #[test]
    fn disabled_reports_stable_model_id() {
        let g = DisabledCandidateGenerator;
        assert_eq!(g.model_id().as_str(), "disabled-candidate-generator");
    }

    /// Marker test — the trait must be object-safe so we can store
    /// `Arc<dyn CandidateGenerator>` in the registry (Task 9).
    #[test]
    fn trait_is_object_safe() {
        let _: Box<dyn CandidateGenerator> = Box::new(DisabledCandidateGenerator);
    }
}
```

- [ ] **Step 2: Wire module into lib.rs**

Edit `crates/ml/src/lib.rs` — append `pub mod candidate;` so the file reads:

```rust
//! horndb-ml — ML/LLM integration boundary (SPEC-08).
//!
//! The symbolic reasoner is the source of truth; this crate's traits
//! exist so external ML systems can *propose* facts (re-verified
//! symbolically) and *advise* the planner. With `MlConfig.enabled =
//! false` the engine behaves bit-identically to a non-ML build.

pub mod candidate;
pub mod provenance;
pub mod types;
```

- [ ] **Step 3: Run tests to verify pass**

Run: `cargo test -p horndb-ml --lib candidate::tests`
Expected: `running 3 tests ... test result: ok. 3 passed; 0 failed`.

- [ ] **Step 4: Commit**

```bash
git add crates/ml/src/lib.rs crates/ml/src/candidate.rs
git commit -m "$(cat <<'EOF'
ml: add CandidateGenerator trait + DisabledCandidateGenerator no-op (F1)

Trait is object-safe so the registry can store Arc<dyn CandidateGenerator>.
DisabledCandidateGenerator always returns Confidence::zero() — guarantees
the symbolic re-verification path sees nothing when ML is off.
EOF
)"
```

---

## Task 6: Define `PlanAdvisor` trait (F2)

**Files:**
- Create: `crates/ml/src/planner.rs`
- Modify: `crates/ml/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/ml/src/planner.rs`:

```rust
//! Planner-advice plugin (SPEC-08 F2).
//!
//! The planner (SPEC-03 / SPEC-07) consults this for cardinality
//! hints and join-order suggestions but always validates against
//! its own histograms and falls back if the advice is implausible.

use crate::types::{ModelId, PlanAdvice, SubplanShape};

pub trait PlanAdvisor: Send + Sync {
    fn model_id(&self) -> ModelId;
    fn advise(&self, shape: &SubplanShape) -> PlanAdvice;
}

/// No-op implementation used when ML is disabled.
///
/// Returns `PlanAdvice::unadvised()` — the planner uses its own
/// histograms exclusively.
#[derive(Debug, Default)]
pub struct DisabledPlanAdvisor;

impl DisabledPlanAdvisor {
    pub const MODEL_ID: &'static str = "disabled-plan-advisor";
}

impl PlanAdvisor for DisabledPlanAdvisor {
    fn model_id(&self) -> ModelId {
        ModelId::new(Self::MODEL_ID)
    }
    fn advise(&self, _shape: &SubplanShape) -> PlanAdvice {
        PlanAdvice::unadvised()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_returns_unadvised() {
        let a = DisabledPlanAdvisor.advise(&SubplanShape {
            n_patterns: 5,
            n_vars: 4,
            bound_vars: 2,
        });
        assert_eq!(a, PlanAdvice::unadvised());
    }

    #[test]
    fn disabled_reports_stable_model_id() {
        assert_eq!(
            DisabledPlanAdvisor.model_id().as_str(),
            "disabled-plan-advisor"
        );
    }

    #[test]
    fn trait_is_object_safe() {
        let _: Box<dyn PlanAdvisor> = Box::new(DisabledPlanAdvisor);
    }
}
```

- [ ] **Step 2: Wire module into lib.rs**

Edit `crates/ml/src/lib.rs` to add `pub mod planner;` (alphabetical order). Final contents:

```rust
//! horndb-ml — ML/LLM integration boundary (SPEC-08).
//!
//! The symbolic reasoner is the source of truth; this crate's traits
//! exist so external ML systems can *propose* facts (re-verified
//! symbolically) and *advise* the planner. With `MlConfig.enabled =
//! false` the engine behaves bit-identically to a non-ML build.

pub mod candidate;
pub mod planner;
pub mod provenance;
pub mod types;
```

- [ ] **Step 3: Run tests to verify pass**

Run: `cargo test -p horndb-ml --lib planner::tests`
Expected: `running 3 tests ... test result: ok. 3 passed; 0 failed`.

- [ ] **Step 4: Commit**

```bash
git add crates/ml/src/lib.rs crates/ml/src/planner.rs
git commit -m "$(cat <<'EOF'
ml: add PlanAdvisor trait + DisabledPlanAdvisor no-op (F2)

Trait takes only the opaque SubplanShape so the planner's internal IR
can evolve without breaking advisor implementations. Disabled variant
returns PlanAdvice::unadvised() — planner uses its histograms only.
EOF
)"
```

---

## Task 7: Define `HotSetAdvisor` trait (F4)

**Files:**
- Create: `crates/ml/src/hotset.rs`
- Modify: `crates/ml/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/ml/src/hotset.rs`:

```rust
//! Hot-set advisor for tier placement (SPEC-08 F4).
//!
//! Predicts which triples will be queried frequently in the next
//! window. SPEC-02's tiering uses this as one input to placement
//! (alongside actual recent-access statistics).
//!
//! Triple IDs are opaque `u64`s here — SPEC-02 owns their meaning;
//! we just shuttle them.

use crate::types::ModelId;

/// Opaque triple identifier from SPEC-02 storage.
///
/// Defined here as a type alias so consumers don't need to import a
/// storage type just to implement this trait.
pub type TripleId = u64;

pub trait HotSetAdvisor: Send + Sync {
    fn model_id(&self) -> ModelId;

    /// Return up to `max` triple IDs predicted to be hot in the
    /// upcoming window. May return fewer; may return an empty Vec.
    fn predict_hot(&self, max: usize) -> Vec<TripleId>;
}

#[derive(Debug, Default)]
pub struct DisabledHotSetAdvisor;

impl DisabledHotSetAdvisor {
    pub const MODEL_ID: &'static str = "disabled-hotset-advisor";
}

impl HotSetAdvisor for DisabledHotSetAdvisor {
    fn model_id(&self) -> ModelId {
        ModelId::new(Self::MODEL_ID)
    }
    fn predict_hot(&self, _max: usize) -> Vec<TripleId> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_returns_empty() {
        assert!(DisabledHotSetAdvisor.predict_hot(1000).is_empty());
    }

    #[test]
    fn disabled_reports_stable_model_id() {
        assert_eq!(
            DisabledHotSetAdvisor.model_id().as_str(),
            "disabled-hotset-advisor"
        );
    }

    #[test]
    fn trait_is_object_safe() {
        let _: Box<dyn HotSetAdvisor> = Box::new(DisabledHotSetAdvisor);
    }
}
```

- [ ] **Step 2: Wire module into lib.rs**

Edit `crates/ml/src/lib.rs` adding `pub mod hotset;`:

```rust
//! horndb-ml — ML/LLM integration boundary (SPEC-08).
//!
//! The symbolic reasoner is the source of truth; this crate's traits
//! exist so external ML systems can *propose* facts (re-verified
//! symbolically) and *advise* the planner. With `MlConfig.enabled =
//! false` the engine behaves bit-identically to a non-ML build.

pub mod candidate;
pub mod hotset;
pub mod planner;
pub mod provenance;
pub mod types;
```

- [ ] **Step 3: Run tests to verify pass**

Run: `cargo test -p horndb-ml --lib hotset::tests`
Expected: `running 3 tests ... test result: ok. 3 passed; 0 failed`.

- [ ] **Step 4: Commit**

```bash
git add crates/ml/src/lib.rs crates/ml/src/hotset.rs
git commit -m "$(cat <<'EOF'
ml: add HotSetAdvisor trait + DisabledHotSetAdvisor no-op (F4)

TripleId is a u64 alias defined locally so trait implementors don't
import a storage type. Disabled returns empty Vec — tiering falls
back to recent-access statistics only.
EOF
)"
```

---

## Task 8: Add `MlAuditLog` (F6 library API)

**Files:**
- Create: `crates/ml/src/audit.rs`
- Modify: `crates/ml/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/ml/src/audit.rs`:

```rust
//! Audit log for ML-derived facts (SPEC-08 F6, library form).
//!
//! Stage 0/1 exposes only the in-memory log + paginated query as a
//! library API. The HTTP `GET /ml-audit?since=` endpoint is Stage 2
//! and will simply wrap this type.

use crate::types::{Confidence, ModelId, TripleSubject};
use chrono::{DateTime, Utc};
use std::sync::Mutex;

#[derive(Debug, Clone)]
pub struct MlAuditEntry {
    pub timestamp: DateTime<Utc>,
    pub model: ModelId,
    pub confidence: Confidence,
    /// `(subject, predicate_iri, object_subject_or_literal)` — kept
    /// loose at Stage 1 since SPEC-02 hasn't fixed its term model yet.
    pub triple: (TripleSubject, String, TripleSubject),
}

#[derive(Debug, Clone)]
pub struct AuditPage {
    pub entries: Vec<MlAuditEntry>,
    /// Token to pass to the next call to continue paginating. `None`
    /// means no more entries.
    pub next_offset: Option<usize>,
}

#[derive(Debug, Default)]
pub struct MlAuditLog {
    inner: Mutex<Vec<MlAuditEntry>>,
}

impl MlAuditLog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&self, entry: MlAuditEntry) {
        self.inner.lock().expect("audit-log mutex poisoned").push(entry);
    }

    pub fn len(&self) -> usize {
        self.inner.lock().expect("audit-log mutex poisoned").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Return entries with timestamp >= `since`, paginated.
    ///
    /// `offset` is the index into the filtered result, not the raw
    /// log — so a caller can keep paginating with the returned token
    /// even as new entries arrive.
    pub fn query_since(
        &self,
        since: DateTime<Utc>,
        offset: usize,
        limit: usize,
    ) -> AuditPage {
        let guard = self.inner.lock().expect("audit-log mutex poisoned");
        let filtered: Vec<MlAuditEntry> = guard
            .iter()
            .filter(|e| e.timestamp >= since)
            .cloned()
            .collect();
        let end = (offset + limit).min(filtered.len());
        let entries = if offset >= filtered.len() {
            Vec::new()
        } else {
            filtered[offset..end].to_vec()
        };
        let next_offset = if end < filtered.len() { Some(end) } else { None };
        AuditPage { entries, next_offset }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(ts: DateTime<Utc>, model: &str) -> MlAuditEntry {
        MlAuditEntry {
            timestamp: ts,
            model: ModelId::new(model),
            confidence: Confidence::new(0.9),
            triple: (
                TripleSubject::Iri("http://x/a".into()),
                "http://www.w3.org/2002/07/owl#sameAs".into(),
                TripleSubject::Iri("http://x/b".into()),
            ),
        }
    }

    #[test]
    fn empty_log_returns_empty_page() {
        let log = MlAuditLog::new();
        let p = log.query_since(Utc::now() - chrono::Duration::hours(1), 0, 10);
        assert!(p.entries.is_empty());
        assert!(p.next_offset.is_none());
    }

    #[test]
    fn record_then_query() {
        let log = MlAuditLog::new();
        let t = Utc::now();
        log.record(make_entry(t, "m1"));
        let p = log.query_since(t - chrono::Duration::seconds(1), 0, 10);
        assert_eq!(p.entries.len(), 1);
        assert_eq!(p.entries[0].model.as_str(), "m1");
    }

    #[test]
    fn since_filter_excludes_older() {
        let log = MlAuditLog::new();
        let old = Utc::now() - chrono::Duration::hours(2);
        let new = Utc::now();
        log.record(make_entry(old, "old"));
        log.record(make_entry(new, "new"));
        let p = log.query_since(Utc::now() - chrono::Duration::hours(1), 0, 10);
        assert_eq!(p.entries.len(), 1);
        assert_eq!(p.entries[0].model.as_str(), "new");
    }

    #[test]
    fn pagination_returns_next_offset_when_more_available() {
        let log = MlAuditLog::new();
        let base = Utc::now();
        for i in 0..5 {
            log.record(make_entry(base + chrono::Duration::seconds(i), "m"));
        }
        let p1 = log.query_since(base - chrono::Duration::seconds(1), 0, 2);
        assert_eq!(p1.entries.len(), 2);
        assert_eq!(p1.next_offset, Some(2));

        let p2 = log.query_since(base - chrono::Duration::seconds(1), 2, 2);
        assert_eq!(p2.entries.len(), 2);
        assert_eq!(p2.next_offset, Some(4));

        let p3 = log.query_since(base - chrono::Duration::seconds(1), 4, 2);
        assert_eq!(p3.entries.len(), 1);
        assert_eq!(p3.next_offset, None);
    }
}
```

- [ ] **Step 2: Wire module into lib.rs**

Edit `crates/ml/src/lib.rs` (final form):

```rust
//! horndb-ml — ML/LLM integration boundary (SPEC-08).
//!
//! The symbolic reasoner is the source of truth; this crate's traits
//! exist so external ML systems can *propose* facts (re-verified
//! symbolically) and *advise* the planner. With `MlConfig.enabled =
//! false` the engine behaves bit-identically to a non-ML build.

pub mod audit;
pub mod candidate;
pub mod hotset;
pub mod planner;
pub mod provenance;
pub mod types;
```

- [ ] **Step 3: Run tests to verify pass**

Run: `cargo test -p horndb-ml --lib audit::tests`
Expected: `running 4 tests ... test result: ok. 4 passed; 0 failed`.

- [ ] **Step 4: Commit**

```bash
git add crates/ml/src/lib.rs crates/ml/src/audit.rs
git commit -m "$(cat <<'EOF'
ml: add MlAuditLog library API for SPEC-08 F6

Stage 0/1 ships the in-memory log + paginated query_since(). The
Stage 2 HTTP endpoint GET /ml-audit will wrap this type — keeping
the data path identical between library and HTTP callers.
EOF
)"
```

---

## Task 9: Define `MlConfig` and `MlConfigError`

**Files:**
- Create: `crates/ml/src/config.rs`
- Modify: `crates/ml/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/ml/src/config.rs`:

```rust
//! Configuration for the ML integration boundary (SPEC-08 NF1).
//!
//! The `enabled` flag is the master switch. With `enabled = false`,
//! the [`crate::registry::MlRegistry`] hands out the `Disabled*`
//! implementations and the engine behaves bit-identically to a
//! non-ML build.

#[derive(Debug, Clone, PartialEq)]
pub struct MlConfig {
    pub enabled: bool,
}

impl MlConfig {
    pub fn disabled() -> Self {
        MlConfig { enabled: false }
    }
    pub fn enabled() -> Self {
        MlConfig { enabled: true }
    }
}

impl Default for MlConfig {
    /// Default is **disabled** — opt-in by design (SPEC-08 NF1).
    fn default() -> Self {
        Self::disabled()
    }
}

/// Errors raised by configuration / registration operations.
///
/// Reserved for future use — Stage 0/1 has no failure modes on
/// `MlRegistry::register_*` because registration is allowed
/// regardless of `enabled`, and the enabled flag only gates
/// *accessors*. Kept here so consumers can `use horndb_ml::MlConfigError`
/// without breakage when Stage 2 adds e.g. an "invalid model id"
/// variant.
#[derive(Debug, thiserror::Error)]
pub enum MlConfigError {
    /// Placeholder — never returned in Stage 0/1. Documented here so
    /// the enum is non-empty and `match` exhaustiveness compiles.
    #[error("ml-config: unspecified configuration error")]
    Unspecified,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_disabled() {
        assert!(!MlConfig::default().enabled);
    }

    #[test]
    fn explicit_constructors() {
        assert!(MlConfig::enabled().enabled);
        assert!(!MlConfig::disabled().enabled);
    }

    #[test]
    fn error_type_is_constructible() {
        // Lock the public surface so adding new variants doesn't
        // accidentally remove this one.
        let _e = MlConfigError::Unspecified;
    }
}
```

- [ ] **Step 2: Wire module into lib.rs**

Edit `crates/ml/src/lib.rs`:

```rust
//! horndb-ml — ML/LLM integration boundary (SPEC-08).
//!
//! The symbolic reasoner is the source of truth; this crate's traits
//! exist so external ML systems can *propose* facts (re-verified
//! symbolically) and *advise* the planner. With `MlConfig.enabled =
//! false` the engine behaves bit-identically to a non-ML build.

pub mod audit;
pub mod candidate;
pub mod config;
pub mod hotset;
pub mod planner;
pub mod provenance;
pub mod types;
```

- [ ] **Step 3: Run tests to verify pass**

Run: `cargo test -p horndb-ml --lib config::tests`
Expected: `running 3 tests ... test result: ok. 3 passed; 0 failed`.

- [ ] **Step 4: Commit**

```bash
git add crates/ml/src/lib.rs crates/ml/src/config.rs
git commit -m "$(cat <<'EOF'
ml: add MlConfig with disabled-by-default semantics (NF1)

Default is disabled — opt-in by design. MlConfigError::RegisterWhileDisabled
will be raised by MlRegistry::register_* in the next task when callers
try to wire a real plugin against a disabled config.
EOF
)"
```

---

## Task 10: Add `MlRegistry` with hot-reload (acceptance #5)

**Files:**
- Create: `crates/ml/src/registry.rs`
- Modify: `crates/ml/src/lib.rs`

- [ ] **Step 1: Write the failing test (registry basics)**

Create `crates/ml/src/registry.rs`:

```rust
//! Central accessor for ML plugins (SPEC-08).
//!
//! The engine asks the registry for a plugin instance; the registry
//! returns either the registered impl or a `Disabled*` no-op,
//! depending on the current [`MlConfig`]. Configuration is
//! hot-reloadable via [`MlRegistry::reload_config`] — acceptance #5.
//!
//! Thread safety: all accessors are read-only on the hot path and
//! held under an `RwLock`. Reloads acquire a write lock for the
//! duration of a single swap.

use crate::audit::MlAuditLog;
use crate::candidate::{CandidateGenerator, DisabledCandidateGenerator};
use crate::config::MlConfig;
use crate::hotset::{DisabledHotSetAdvisor, HotSetAdvisor};
use crate::planner::{DisabledPlanAdvisor, PlanAdvisor};
use std::sync::{Arc, RwLock};

pub struct MlRegistry {
    inner: RwLock<RegistryInner>,
    audit: Arc<MlAuditLog>,
}

struct RegistryInner {
    config: MlConfig,
    candidate: Option<Arc<dyn CandidateGenerator>>,
    planner: Option<Arc<dyn PlanAdvisor>>,
    hotset: Option<Arc<dyn HotSetAdvisor>>,

    // Cached no-op fallbacks so the disabled hot path returns the
    // same Arc instance every time (no allocation per call).
    disabled_candidate: Arc<dyn CandidateGenerator>,
    disabled_planner: Arc<dyn PlanAdvisor>,
    disabled_hotset: Arc<dyn HotSetAdvisor>,
}

impl MlRegistry {
    pub fn new(config: MlConfig) -> Self {
        Self {
            inner: RwLock::new(RegistryInner {
                config,
                candidate: None,
                planner: None,
                hotset: None,
                disabled_candidate: Arc::new(DisabledCandidateGenerator),
                disabled_planner: Arc::new(DisabledPlanAdvisor),
                disabled_hotset: Arc::new(DisabledHotSetAdvisor),
            }),
            audit: Arc::new(MlAuditLog::new()),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.inner.read().expect("registry rwlock poisoned").config.enabled
    }

    /// Hot-reload the config (acceptance #5 — no restart).
    ///
    /// Switching from enabled to disabled keeps registered plugins
    /// in place but accessor methods return the `Disabled*` no-ops
    /// until re-enabled.
    pub fn reload_config(&self, config: MlConfig) {
        let mut guard = self.inner.write().expect("registry rwlock poisoned");
        guard.config = config;
    }

    pub fn register_candidate(&self, g: Arc<dyn CandidateGenerator>) {
        let mut guard = self.inner.write().expect("registry rwlock poisoned");
        guard.candidate = Some(g);
    }

    pub fn register_planner(&self, p: Arc<dyn PlanAdvisor>) {
        let mut guard = self.inner.write().expect("registry rwlock poisoned");
        guard.planner = Some(p);
    }

    pub fn register_hotset(&self, h: Arc<dyn HotSetAdvisor>) {
        let mut guard = self.inner.write().expect("registry rwlock poisoned");
        guard.hotset = Some(h);
    }

    pub fn candidate_generator(&self) -> Arc<dyn CandidateGenerator> {
        let guard = self.inner.read().expect("registry rwlock poisoned");
        if guard.config.enabled {
            guard
                .candidate
                .as_ref()
                .cloned()
                .unwrap_or_else(|| guard.disabled_candidate.clone())
        } else {
            guard.disabled_candidate.clone()
        }
    }

    pub fn plan_advisor(&self) -> Arc<dyn PlanAdvisor> {
        let guard = self.inner.read().expect("registry rwlock poisoned");
        if guard.config.enabled {
            guard
                .planner
                .as_ref()
                .cloned()
                .unwrap_or_else(|| guard.disabled_planner.clone())
        } else {
            guard.disabled_planner.clone()
        }
    }

    pub fn hotset_advisor(&self) -> Arc<dyn HotSetAdvisor> {
        let guard = self.inner.read().expect("registry rwlock poisoned");
        if guard.config.enabled {
            guard
                .hotset
                .as_ref()
                .cloned()
                .unwrap_or_else(|| guard.disabled_hotset.clone())
        } else {
            guard.disabled_hotset.clone()
        }
    }

    pub fn audit_log(&self) -> Arc<MlAuditLog> {
        self.audit.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ModelId, TripleSubject};

    #[test]
    fn disabled_returns_no_op_candidate() {
        let r = MlRegistry::new(MlConfig::disabled());
        let g = r.candidate_generator();
        assert_eq!(
            g.model_id().as_str(),
            DisabledCandidateGenerator::MODEL_ID
        );
    }

    #[test]
    fn enabled_without_registration_returns_no_op() {
        let r = MlRegistry::new(MlConfig::enabled());
        let g = r.candidate_generator();
        // Enabled but nothing registered: still no-op.
        assert_eq!(
            g.model_id().as_str(),
            DisabledCandidateGenerator::MODEL_ID
        );
    }

    struct FakeCandidate;
    impl CandidateGenerator for FakeCandidate {
        fn model_id(&self) -> ModelId {
            ModelId::new("fake")
        }
        fn propose_sameas(
            &self,
            _left: &TripleSubject,
            _right: &TripleSubject,
        ) -> crate::types::Confidence {
            crate::types::Confidence::new(0.99)
        }
    }

    #[test]
    fn enabled_with_registered_returns_registered() {
        let r = MlRegistry::new(MlConfig::enabled());
        r.register_candidate(Arc::new(FakeCandidate));
        let g = r.candidate_generator();
        assert_eq!(g.model_id().as_str(), "fake");
    }

    #[test]
    fn registered_but_disabled_returns_no_op() {
        let r = MlRegistry::new(MlConfig::enabled());
        r.register_candidate(Arc::new(FakeCandidate));
        r.reload_config(MlConfig::disabled());
        let g = r.candidate_generator();
        // The registered plugin is still in the registry, but the
        // config switch routes us back to the no-op.
        assert_eq!(
            g.model_id().as_str(),
            DisabledCandidateGenerator::MODEL_ID
        );
    }

    #[test]
    fn re_enable_restores_registered() {
        let r = MlRegistry::new(MlConfig::enabled());
        r.register_candidate(Arc::new(FakeCandidate));
        r.reload_config(MlConfig::disabled());
        r.reload_config(MlConfig::enabled());
        assert_eq!(r.candidate_generator().model_id().as_str(), "fake");
    }
}
```

- [ ] **Step 2: Wire module into lib.rs (final form)**

Edit `crates/ml/src/lib.rs` to its final shape:

```rust
//! horndb-ml — ML/LLM integration boundary (SPEC-08).
//!
//! The symbolic reasoner is the source of truth; this crate's traits
//! exist so external ML systems can *propose* facts (re-verified
//! symbolically) and *advise* the planner. With `MlConfig.enabled =
//! false` the engine behaves bit-identically to a non-ML build.

pub mod audit;
pub mod candidate;
pub mod config;
pub mod hotset;
pub mod planner;
pub mod provenance;
pub mod registry;
pub mod types;

pub use config::{MlConfig, MlConfigError};
pub use registry::MlRegistry;
```

- [ ] **Step 3: Run tests to verify pass**

Run: `cargo test -p horndb-ml --lib registry::tests`
Expected: `running 5 tests ... test result: ok. 5 passed; 0 failed`.

- [ ] **Step 4: Run full crate test suite as a checkpoint**

Run: `cargo test -p horndb-ml`
Expected: All unit tests across all modules pass (config 3 + types 7 + provenance 3 + candidate 3 + planner 3 + hotset 3 + audit 4 + registry 5 = 31 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/ml/src/lib.rs crates/ml/src/registry.rs
git commit -m "$(cat <<'EOF'
ml: add MlRegistry with hot-reload support (SPEC-08 acceptance #5)

Registry caches Arc'd no-op fallbacks so the disabled hot path is
allocation-free. Hot-reload via reload_config swaps the MlConfig under
a write lock; switching disabled->enabled restores any previously
registered plugin without re-registration.
EOF
)"
```

---

## Task 11: Integration test — "disabled is bit-identical" (NF1, acceptance #1)

**Files:**
- Create: `crates/ml/tests/disabled_is_identity.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/ml/tests/disabled_is_identity.rs`:

```rust
//! NF1 / acceptance #1: with `ml.enabled = false`, all accessor
//! methods return the canonical `Disabled*` no-ops — proving that
//! downstream callers see identical behaviour to a build with no
//! ML plugins compiled in.
//!
//! This is the test that protects the "ML cannot affect correctness"
//! guarantee at the boundary itself. SPEC-01's conformance harness
//! adds the *engine-wide* version of this check; here we lock the
//! boundary down.

use horndb_ml::candidate::DisabledCandidateGenerator;
use horndb_ml::hotset::DisabledHotSetAdvisor;
use horndb_ml::planner::DisabledPlanAdvisor;
use horndb_ml::types::{Confidence, PlanAdvice, SubplanShape, TripleSubject};
use horndb_ml::{MlConfig, MlRegistry};

#[test]
fn disabled_candidate_returns_zero_confidence() {
    let r = MlRegistry::new(MlConfig::disabled());
    let g = r.candidate_generator();
    let a = TripleSubject::Iri("http://x/a".into());
    let b = TripleSubject::Iri("http://x/b".into());
    assert_eq!(g.propose_sameas(&a, &b), Confidence::zero());
    assert_eq!(g.model_id().as_str(), DisabledCandidateGenerator::MODEL_ID);
}

#[test]
fn disabled_planner_returns_unadvised() {
    let r = MlRegistry::new(MlConfig::disabled());
    let p = r.plan_advisor();
    let shape = SubplanShape { n_patterns: 4, n_vars: 3, bound_vars: 1 };
    assert_eq!(p.advise(&shape), PlanAdvice::unadvised());
    assert_eq!(p.model_id().as_str(), DisabledPlanAdvisor::MODEL_ID);
}

#[test]
fn disabled_hotset_returns_empty() {
    let r = MlRegistry::new(MlConfig::disabled());
    let h = r.hotset_advisor();
    assert!(h.predict_hot(1000).is_empty());
    assert_eq!(h.model_id().as_str(), DisabledHotSetAdvisor::MODEL_ID);
}

#[test]
fn audit_log_is_empty_under_disabled_config() {
    // The audit log itself exists, but with no plugins firing it
    // stays empty for the lifetime of the registry.
    let r = MlRegistry::new(MlConfig::disabled());
    assert!(r.audit_log().is_empty());
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p horndb-ml --test disabled_is_identity`
Expected: `running 4 tests ... test result: ok. 4 passed; 0 failed`.

- [ ] **Step 3: Commit**

```bash
git add crates/ml/tests/disabled_is_identity.rs
git commit -m "$(cat <<'EOF'
ml: integration test for SPEC-08 NF1 — disabled-is-bit-identical

Locks the boundary down: with MlConfig::disabled() every accessor
returns the canonical Disabled* no-op. SPEC-01's engine-wide
conformance test will provide the cross-cutting version once the
harness exists.
EOF
)"
```

---

## Task 12: Integration test — runtime hot-reload (acceptance #5)

**Files:**
- Create: `crates/ml/tests/registry_hot_reload.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/ml/tests/registry_hot_reload.rs`:

```rust
//! Acceptance #5: enabling/disabling ML via configuration reload
//! requires no engine restart. We simulate the "engine" by stashing
//! the registry in an `Arc` and calling accessors before and after
//! reload from a second thread, confirming the post-reload
//! behaviour without recreating any state.

use horndb_ml::candidate::{CandidateGenerator, DisabledCandidateGenerator};
use horndb_ml::types::{Confidence, ModelId, TripleSubject};
use horndb_ml::{MlConfig, MlRegistry};
use std::sync::Arc;
use std::thread;

struct AlwaysHigh;
impl CandidateGenerator for AlwaysHigh {
    fn model_id(&self) -> ModelId {
        ModelId::new("always-high")
    }
    fn propose_sameas(
        &self,
        _left: &TripleSubject,
        _right: &TripleSubject,
    ) -> Confidence {
        Confidence::new(0.99)
    }
}

#[test]
fn hot_reload_round_trip_without_restart() {
    let r = Arc::new(MlRegistry::new(MlConfig::enabled()));
    r.register_candidate(Arc::new(AlwaysHigh));

    // Initially enabled: registered plugin is in effect.
    let a = TripleSubject::Iri("http://x/a".into());
    let b = TripleSubject::Iri("http://x/b".into());
    assert_eq!(
        r.candidate_generator().propose_sameas(&a, &b),
        Confidence::new(0.99)
    );

    // Disable from a worker thread — same registry instance.
    {
        let r2 = r.clone();
        thread::spawn(move || r2.reload_config(MlConfig::disabled()))
            .join()
            .unwrap();
    }
    assert!(!r.is_enabled());
    assert_eq!(
        r.candidate_generator().model_id().as_str(),
        DisabledCandidateGenerator::MODEL_ID
    );

    // Re-enable — registered plugin comes back without re-registration.
    r.reload_config(MlConfig::enabled());
    assert_eq!(
        r.candidate_generator().propose_sameas(&a, &b),
        Confidence::new(0.99)
    );
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p horndb-ml --test registry_hot_reload`
Expected: `running 1 test ... test result: ok. 1 passed; 0 failed`.

- [ ] **Step 3: Commit**

```bash
git add crates/ml/tests/registry_hot_reload.rs
git commit -m "$(cat <<'EOF'
ml: integration test for SPEC-08 acceptance #5 — runtime reload

Confirms a single MlRegistry instance can be flipped enabled <-> disabled
across threads without losing registered plugins or requiring an engine
restart. Reload from worker thread proves the RwLock isn't held by the
caller during the swap.
EOF
)"
```

---

## Task 13: Integration test — audit log pagination across threads

**Files:**
- Create: `crates/ml/tests/audit_pagination.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/ml/tests/audit_pagination.rs`:

```rust
//! F6: the audit log records ML-derived facts and supports
//! `since`-windowed paginated reads.

use chrono::{Duration, Utc};
use horndb_ml::audit::MlAuditEntry;
use horndb_ml::types::{Confidence, ModelId, TripleSubject};
use horndb_ml::{MlConfig, MlRegistry};
use std::sync::Arc;
use std::thread;

#[test]
fn concurrent_writers_then_paginated_read() {
    let r = Arc::new(MlRegistry::new(MlConfig::enabled()));
    let log = r.audit_log();
    let base = Utc::now();

    let handles: Vec<_> = (0..4u64)
        .map(|tid| {
            let log = log.clone();
            thread::spawn(move || {
                for i in 0..25u64 {
                    log.record(MlAuditEntry {
                        timestamp: base + Duration::milliseconds((tid * 25 + i) as i64),
                        model: ModelId::new(format!("model-{tid}")),
                        confidence: Confidence::new(0.5),
                        triple: (
                            TripleSubject::Iri(format!("http://x/s{i}")),
                            "http://www.w3.org/2002/07/owl#sameAs".into(),
                            TripleSubject::Iri(format!("http://x/o{i}")),
                        ),
                    });
                }
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }

    assert_eq!(log.len(), 100);

    // Paginate from the beginning of the window.
    let since = base - Duration::seconds(1);
    let mut seen = 0usize;
    let mut offset = 0usize;
    loop {
        let page = log.query_since(since, offset, 30);
        seen += page.entries.len();
        match page.next_offset {
            Some(next) => offset = next,
            None => break,
        }
    }
    assert_eq!(seen, 100);
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p horndb-ml --test audit_pagination`
Expected: `running 1 test ... test result: ok. 1 passed; 0 failed`.

- [ ] **Step 3: Commit**

```bash
git add crates/ml/tests/audit_pagination.rs
git commit -m "$(cat <<'EOF'
ml: integration test for SPEC-08 F6 — concurrent audit log writes

Four threads record 25 entries each; the paginated read returns all
100 entries across 4 pages of 30. Validates the Mutex-backed log
under contention and the next_offset chaining semantics.
EOF
)"
```

---

## Task 14: Run the whole crate's tests as a checkpoint

**Files:** none — verification only.

- [ ] **Step 1: Run every horndb-ml test**

Run: `cargo test -p horndb-ml`
Expected: tail of output reads
```
test result: ok. <N> passed; 0 failed
```
where the total is 31 unit + 4 + 1 + 1 = 37 tests across the crate.

- [ ] **Step 2: Run clippy across the crate**

Run: `cargo clippy -p horndb-ml --all-targets -- -D warnings`
Expected: `Checking horndb-ml v0.0.0` then no warnings or errors. If a warning fires, fix it before proceeding (typical: missing `#[must_use]` or visibility nits).

- [ ] **Step 3: Format**

Run: `cargo fmt -p horndb-ml`
Expected: no output.

- [ ] **Step 4: Commit any formatting changes**

Run: `git diff --stat` — if non-empty:

```bash
git add crates/ml/
git commit -m "$(cat <<'EOF'
ml: cargo fmt pass over SPEC-08 stage 0/1 sources
EOF
)"
```

If the diff is empty, skip this step.

---

## Task 15: Emit `INTEGRATION-NOTES.md` for `storage` (F5 wiring point)

**Files:**
- Create: `crates/storage/INTEGRATION-NOTES.md`

This file is **documentation only** — no code changes to the storage crate. SPEC-02's own plan will read this and wire the integration.

- [ ] **Step 1: Write the integration note**

Create `crates/storage/INTEGRATION-NOTES.md`:

````markdown
# SPEC-08 Integration Notes for `horndb-storage`

These notes describe call sites that **SPEC-02's plan** is responsible
for implementing. Nothing in this file modifies `horndb-storage`
directly; it records the contract `horndb-ml` exposes for SPEC-02
to consume.

## F5 — Provenance annotation column

`horndb-ml::provenance::MlProvenance` is the value type to store
on each inferred triple. SPEC-02 should:

1. Add an optional column `provenance: MlProvenance` to each
   predicate-partition's inferred-triples view.
2. Pack on disk via the stable discriminant bytes:
   - `MlProvenance::SYMBOLIC_TAG = 0x00`
   - `MlProvenance::ML_DERIVED_TAG = 0x01`
3. Triples written by SPEC-04 / SPEC-05 default to `Symbolic`.
4. The bulk-insert writeback from `MlRegistry::candidate_generator()`
   (called by SPEC-04 / SPEC-05) supplies `MlDerived { model, confidence }`.

The append-only discriminant rule is part of the SPEC-08 contract:
future variants must take new bytes, never reuse `0x00` or `0x01`.

## F4 — Hot-set advisor input to tiering

`horndb-ml::hotset::HotSetAdvisor::predict_hot(max)` returns
`Vec<TripleId>`. SPEC-02's tier-placement policy should:

1. Hold an `Arc<MlRegistry>` provided at construction time.
2. Periodically call `registry.hotset_advisor().predict_hot(window_size)`.
3. Bias placement toward the returned IDs **alongside** actual
   recent-access statistics (never instead of).

With `ml.enabled = false` the call returns an empty `Vec` (no-op);
tier placement therefore uses recent-access stats only — bit-identical
to a build with no advisor wired.
````

- [ ] **Step 2: Commit**

```bash
git add crates/storage/INTEGRATION-NOTES.md
git commit -m "$(cat <<'EOF'
ml: storage integration note for SPEC-08 F4/F5

Documents the provenance column contract (stable discriminant bytes)
and the HotSetAdvisor call pattern. SPEC-02's plan will read this and
wire the call sites; we make no changes to horndb-storage here.
EOF
)"
```

---

## Task 16: Emit `INTEGRATION-NOTES.md` for `wcoj` (F2 wiring point)

**Files:**
- Create: `crates/wcoj/INTEGRATION-NOTES.md`

- [ ] **Step 1: Write the integration note**

Create `crates/wcoj/INTEGRATION-NOTES.md`:

````markdown
# SPEC-08 Integration Notes for `horndb-wcoj`

These notes describe call sites that **SPEC-03's plan** is responsible
for implementing.

## F2 — PlanAdvisor consultation

Before finalising a join order, the WCOJ planner should:

1. Construct a `horndb_ml::types::SubplanShape { n_patterns,
   n_vars, bound_vars }` from the candidate subplan.
2. Call `registry.plan_advisor().advise(&shape)` to obtain a
   `PlanAdvice`.
3. Treat every advice field as a **hint**: validate against the
   planner's own histograms before applying. If `estimated_cardinality`
   disagrees with the histogram by more than configured tolerance,
   discard the advice and use the histogram value.
4. NF2: if the advise call exceeds 1 ms p99 (measure via a rolling
   histogram), skip the advisor for that query and log a warning.

With `ml.enabled = false`, `advise()` returns `PlanAdvice::unadvised()`
and the planner uses histograms exclusively — bit-identical to a
no-ML build.
````

- [ ] **Step 2: Commit**

```bash
git add crates/wcoj/INTEGRATION-NOTES.md
git commit -m "$(cat <<'EOF'
ml: wcoj integration note for SPEC-08 F2 (planner advisor)

Documents the call shape and the NF2 latency budget (1 ms p99 skip
threshold). SPEC-03's plan owns the actual wiring.
EOF
)"
```

---

## Task 17: Emit `INTEGRATION-NOTES.md` for `owlrl` (F1 staging-graph hook)

**Files:**
- Create: `crates/owlrl/INTEGRATION-NOTES.md`

- [ ] **Step 1: Write the integration note**

Create `crates/owlrl/INTEGRATION-NOTES.md`:

````markdown
# SPEC-08 Integration Notes for `horndb-owlrl`

These notes describe call sites that **SPEC-04's plan** is responsible
for implementing.

## F1 — Candidate `sameAs` re-verification

When the rule engine processes pending candidate `owl:sameAs`
assertions (held in the `staging.sameAs` named graph per SPEC-08), it
should:

1. For each candidate pair `(a, b)` with `MlProvenance::MlDerived
   { model, confidence }`, run the standard `eq-*` rule body
   symbolically — exactly as for any other assertion.
2. If symbolic re-verification holds (or if the proposal is admitted
   under the policy in step 3), record an `MlAuditEntry` via
   `registry.audit_log().record(...)`.
3. Stage 1 policy is **always queue for human review** — no
   auto-commit. The rule engine writes the candidate into
   `staging.sameAs`, never directly into the live store. Auto-commit
   thresholds are Stage 2.

## Provenance for derived triples

Triples derived from an admitted ML candidate must be written with
`MlProvenance::MlDerived { model, confidence }` (the confidence is
propagated from the originating candidate). Triples derived purely
from asserted facts keep `MlProvenance::Symbolic`.

With `ml.enabled = false`, the registry's candidate generator
returns `Confidence::zero()` for every pair, so no candidates ever
enter staging — the rule engine sees the asserted base only.
````

- [ ] **Step 2: Commit**

```bash
git add crates/owlrl/INTEGRATION-NOTES.md
git commit -m "$(cat <<'EOF'
ml: owlrl integration note for SPEC-08 F1 (sameAs re-verification)

Stage 1 policy is "always queue for review" — never auto-commit.
Auto-commit thresholds + confidence calibration are explicit Stage 2
work per SPEC-08 risks section.
EOF
)"
```

---

## Task 18: Emit `INTEGRATION-NOTES.md` for `closure` (cascade verification)

**Files:**
- Create: `crates/closure/INTEGRATION-NOTES.md`

- [ ] **Step 1: Write the integration note**

Create `crates/closure/INTEGRATION-NOTES.md`:

````markdown
# SPEC-08 Integration Notes for `horndb-closure`

These notes describe call sites that **SPEC-05's plan** is responsible
for implementing.

## F1 cascade — `sameAs` equivalence-class merge

When SPEC-04 admits a candidate `owl:sameAs(a, b)` from the staging
graph, SPEC-05's `EQREL` structure must:

1. Compute the implied equivalence-class consequences (union of the
   two classes, transitive over all property assertions touching
   either class).
2. Tag every newly-derived triple with the originating
   `MlProvenance::MlDerived { model, confidence }` so the audit
   trail (F6) can attribute the cascade back to the candidate.
3. Per SPEC-08's "sameAs cascade" risk: this is expensive to roll
   back. Stage 1's "always queue for review" policy keeps the cascade
   in the staging graph until accepted; the commit step then bulk-
   inserts via the writeback path described in SPEC-05 F5.

No `horndb-closure` API needs to change for Stage 0/1 — this
integration is a SPEC-05 plan task that calls into `horndb-ml`'s
existing types only.
````

- [ ] **Step 2: Commit**

```bash
git add crates/closure/INTEGRATION-NOTES.md
git commit -m "$(cat <<'EOF'
ml: closure integration note for SPEC-08 F1 sameAs cascade

Documents the staging-graph-first policy that defends against
SPEC-08's "expensive rollback" risk. SPEC-05's plan owns the wiring.
EOF
)"
```

---

## Task 19: Emit `INTEGRATION-NOTES.md` for `sparql` (F2 + future F3)

**Files:**
- Create: `crates/sparql/INTEGRATION-NOTES.md`

- [ ] **Step 1: Write the integration note**

Create `crates/sparql/INTEGRATION-NOTES.md`:

````markdown
# SPEC-08 Integration Notes for `horndb-sparql`

These notes describe call sites that **SPEC-07's plan** is responsible
for implementing.

## F2 — PlanAdvisor at the SPARQL planner

Same contract as `wcoj/INTEGRATION-NOTES.md` — the SPARQL planner
constructs a `SubplanShape` from its algebra tree, calls
`registry.plan_advisor().advise(&shape)`, validates against its own
histograms, and falls back if implausible. NF2's 1 ms p99 budget
applies here too.

## F5 — Filtering by provenance in SPARQL

SPARQL queries should be able to filter on the provenance column
exposed by SPEC-02. SPEC-07's plan should:

1. Recognise the (engine-specific) predicate
   `<https://horndb.io/prov/source>` in `FILTER`
   clauses.
2. Map literal values `"symbolic"` and `"ml-derived"` onto the
   `MlProvenance` discriminants from SPEC-02's storage column.
3. Allow audit queries of the form:
   ```sparql
   SELECT ?s ?p ?o ?model WHERE {
     ?s ?p ?o .
     ?s <https://horndb.io/prov/source> "ml-derived" .
     ?s <https://horndb.io/prov/model>  ?model .
   }
   ```

## F3 — LLM → SPARQL endpoint (STAGE 2 — DEFERRED)

`POST /nl-query` is **not** part of Stage 0/1. When SPEC-07's plan
adds it, the implementation should:

1. Live in a new module (`crates/sparql/src/nl.rs`).
2. Take an injected `Arc<dyn LlmClient>` (trait to be defined in
   `horndb-ml` Stage 2) so the LLM provider is pluggable and the
   handler is testable without network.
3. Always return the generated SPARQL alongside the results (per
   SPEC-08 risks: "LLM SPARQL quality").
4. Defer cost reporting and training-data leakage controls to
   Stage 2+ per SPEC-08.

For Stage 0/1 the file remains absent — `horndb-ml` ships only
the boundary; the LLM client trait will land with the Stage 2 plan.
````

- [ ] **Step 2: Commit**

```bash
git add crates/sparql/INTEGRATION-NOTES.md
git commit -m "$(cat <<'EOF'
ml: sparql integration note for SPEC-08 F2/F5 + F3 deferral

Documents F2 planner advice, F5 SPARQL filtering on the provenance
column, and explicitly defers F3 (LLM->SPARQL endpoint) to Stage 2
with a sketch of where the future module lands.
EOF
)"
```

---

## Task 20: Workspace-wide build + test sanity check

**Files:** none — verification only.

- [ ] **Step 1: Workspace build**

Run: `cargo build --workspace`
Expected: every crate (harness, storage, wcoj, owlrl, closure, incremental, sparql, ml, hardware-ext) compiles. The other crates are still placeholder shells, so this should be fast and clean.

- [ ] **Step 2: Workspace test**

Run: `cargo test --workspace`
Expected: `horndb-ml` runs 37 tests passing; other crates run 0 tests (they're placeholders). No failures.

- [ ] **Step 3: Workspace clippy**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings or errors. (Other crates have no real code yet; only `horndb-ml` could fail this gate.)

- [ ] **Step 4: Verify INTEGRATION-NOTES files are committed**

Run: `git ls-files 'crates/*/INTEGRATION-NOTES.md'`
Expected output:
```
crates/closure/INTEGRATION-NOTES.md
crates/owlrl/INTEGRATION-NOTES.md
crates/sparql/INTEGRATION-NOTES.md
crates/storage/INTEGRATION-NOTES.md
crates/wcoj/INTEGRATION-NOTES.md
```

If anything is missing, `git status` will show it untracked — add and commit before proceeding.

---

## Task 21: Update top-level docs / spec status (lightweight)

**Files:** none unless a status file exists.

- [ ] **Step 1: Check for a roadmap or status file**

Run: `ls /Users/stig/git/sunstone/reasoner/ | grep -iE 'roadmap|status|readme'`

- [ ] **Step 2: If a status file exists, update it**

If a `README.md` or `ROADMAP.md` exists, add a line under the SPEC-08 row stating "Stage 0/1 complete — traits + no-op impls + integration notes shipped." Commit:

```bash
git add <the-file>
git commit -m "$(cat <<'EOF'
docs: mark SPEC-08 stage 0/1 complete (traits + no-ops)
EOF
)"
```

If no such file exists, skip this task — do not create one. The PR will speak for itself.

---

## Self-review checklist (executed before claiming done)

- [ ] Every spec functional requirement F1, F2, F4, F5, F6 in scope is implemented or has an INTEGRATION-NOTES entry.
- [ ] F3 is explicitly deferred and called out in `sparql/INTEGRATION-NOTES.md`.
- [ ] NF1 (disabled-is-identity) has a dedicated integration test.
- [ ] Acceptance #5 (runtime reload) has a dedicated integration test.
- [ ] No edits to any crate other than `horndb-ml` and the `INTEGRATION-NOTES.md` docs.
- [ ] All commits are atomic and pass `cargo test -p horndb-ml` at HEAD.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` is clean.
- [ ] No `Co-Authored-By:` lines in commit messages.

## Plan completion criteria

The plan is **done** when:

1. `cargo test --workspace` is green.
2. `cargo clippy --workspace --all-targets -- -D warnings` is clean.
3. All five `INTEGRATION-NOTES.md` files are committed.
4. The disabled-is-identity test and the hot-reload test are committed and passing.
5. The Stage 0/1 deliverables from SPEC-08 (F1, F2, F4, F5 enum, F6 library, NF1) all have backing code.
6. The deferred items (F3 endpoint, FAISS impl, HTTP audit endpoint, training-data leakage controls, cost reporting) are documented as Future Work in this plan and in the `sparql/INTEGRATION-NOTES.md`.
