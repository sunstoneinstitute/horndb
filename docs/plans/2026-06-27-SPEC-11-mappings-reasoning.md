# SPEC-11 SSSOM Mappings — Reasoning Slice Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make HornDB reason over SSSOM ontology crosswalks — materialize mapping facts, fire the SSSOM chaining rules (T1 / RCE / RI / RG) plus monotone negative chaining, tag inferred mappings with the right `semapv:*` justification and `derived_from` provenance, and grade it all green against a curated conformance subset loaded via a harness-only SSSOM/TSV reader.

**Architecture:** This is the *reasoning* half of SPEC-11 (F1–F4, F7, F8, F9 + conformance). It rides the existing `horndb-owlrl` codegen pipeline (`rules.toml` → `build.rs` → `generated_rules.rs`) and the existing `ClosureBackend` for transitive chaining — **no new subsystem**. The *serving* half (F5 compact crosswalk index, F6 spine) is a separate follow-up plan and is explicitly **out of scope here**. Per ADR-0017, `skos:exactMatch` is a crosswalk edge, **never** OWL identity: the SSSOM rules compose mapping predicates *within the mapping layer* and never bridge to `owl:sameAs`/`eq-rep-*`.

**Tech Stack:** Rust 1.90 (workspace-pinned), `horndb-owlrl` (TOML-driven rule codegen via `syn`/`proc-macro2`/`prettyplease`), `horndb-harness` (oxrdf/oxttl loaders, manifest-driven runner), `cargo nextest`.

---

## Background an implementer needs before starting

You are editing a forward-chaining OWL 2 RL reasoner. Read `crates/owlrl/CLAUDE.md` (a.k.a. `AGENTS.md`) §2 and §5 once — it is the canonical pipeline description. The three facts that matter most:

1. **Adding a vocabulary term is a multi-file edit, but mechanical.** Add a field to `struct Vocabulary` in `crates/owlrl/src/vocab.rs` with a `///`-doc'd QName in backticks, add a matching line in `Vocabulary::synthetic()`, and — *only if the term flows through the `Engine` façade* (it does for us) — add a `const FOO: &str = "<IRI>"` and a `foo: alloc(FOO)` line in `crates/owlrl/src/integration.rs::build_vocab()`, then bump `USER_TERMS_BASE`. **Never edit `codegen/parse.rs`** — the QName→field map is auto-derived from the doc comments. A `debug_assert_eq!(id, USER_TERMS_BASE)` at the end of `build_vocab` will panic at runtime if your count is off — that is your safety net.

2. **Adding a rule is a `rules.toml` edit.** A `[[rule]]` block has `id`, `comment`, `body` (list of `{ s, p, o }` patterns), `head` (one pattern). A slot is either a variable (`"?x"`) or a vocab QName (`"skos:exactMatch"`). `cargo build -p horndb-owlrl` regenerates the Rust; `cargo run -p horndb-owlrl --bin show-rule -- <id>` prints the generated function so you can eyeball it. The **Stage-1 invariant** (still in force): the *leading* body pattern's predicate must be a constant vocab term, not a variable.

3. **Transitive ("closure-shaped") rules are different.** They are marked `delegate = "closure"` in `rules.toml` (compiled to a no-op `fire`) and are actually computed by a `ClosureBackend`. The harness uses the always-available `RuleFiringBackend` (in `crates/owlrl/src/backend.rs`), which hard-codes *which* predicates it closes. To add transitive closure over a new predicate you must (a) add the `delegate = "closure"` rule block for documentation/table-completeness, **and** (b) extend `RuleFiringBackend::close()` to actually close it.

### Rule semantics decisions locked for this plan (with rationale)

SPEC-11 §F3 names the rule families but defers exact predicate scoping to SSSOM's `inference.md`. Two scoping choices are forced by ADR-0017 and the Stage-1 invariant; they are locked here so every task is unambiguous. If `inference.md` later contradicts one, fix it in a follow-up — do **not** improvise mid-plan.

- **RCE (role chains) are instantiated per mapping predicate, not over a wildcard predicate.** SPEC-11 writes RCE1 as `A -[exactMatch]-> B -[p]-> C ⟹ A -[p]-> C`. Taking `p` as a *free* predicate would make `exactMatch` substitute a subject across **arbitrary** triples — exactly the `eq-rep-s`-style identity behaviour ADR-0017 forbids for crosswalk edges. So we restrict `p` to the SSSOM mapping predicates and emit one constant-predicate rule per `(leading, p)` pair. This also satisfies the Stage-1 constant-leading-predicate invariant.
- **T1 transitivity only *adds* closure for the SKOS mapping predicates** `skos:exactMatch`, `skos:broadMatch`, `skos:narrowMatch`. The other predicates SPEC-11 lists under T1 — `subClassOf`, `subPropertyOf`, `sameAs`, `equivalentClass`, `equivalentProperty` — are already transitively closed by the existing OWL machinery (`scm-sco`, `scm-spo`, `eq-trans`, and the `scm-eqc*`/`scm-eqp*` schema rules). Re-closing them in the mapping layer would be redundant and, for `equivalentClass`/`sameAs`, would risk the very identity-bridging ADR-0017 bans. We do **not** re-add them.
- **`skos:exactMatch` symmetry is *not* added.** SPEC-11 §F3 lists symmetry only for the `narrowMatch ↔ broadMatch` inverse pair (RI) and the cross-species inverses — not for `exactMatch`. We implement exactly the rules the spec enumerates and add nothing more.

### Namespaces used (memorize these IRIs — they appear verbatim in Task 1)

| Prefix | Namespace IRI |
|---|---|
| `skos:` | `http://www.w3.org/2004/02/skos/core#` |
| `sssom:` | `https://w3id.org/sssom/` |
| `semapv:` | `https://w3id.org/semapv/vocab/` |
| `horndb:` (internal) | `https://w3id.org/horndb/internal#` |

---

## File structure (what gets created / modified)

**owlrl crate (rules + vocab + justification + confidence):**
- Modify: `crates/owlrl/src/vocab.rs` — add mapping-predicate / SSSOM-node / internal-negated vocab fields.
- Modify: `crates/owlrl/src/integration.rs` — IRI consts + `build_vocab()` lines + `USER_TERMS_BASE`.
- Modify: `crates/owlrl/rules.toml` — RG / RI / RCE compiled rules; T1 + negative `delegate="closure"` blocks.
- Modify: `crates/owlrl/src/backend.rs` — extend `RuleFiringBackend::close()` for the new transitive + negative-chaining predicates.
- Create: `crates/owlrl/src/sssom.rs` — `rule_justification(rule_id) -> Semapv` map, `MappingNode` builder (F2), `combine_confidence` (F7).
- Modify: `crates/owlrl/src/lib.rs` — `pub mod sssom;`.
- Create/modify tests: `crates/owlrl/tests/sssom_rules.rs`, `crates/owlrl/tests/sssom_negative.rs`, `crates/owlrl/tests/sssom_identity_isolation.rs`.

**harness crate (loader + conformance):**
- Create: `crates/harness/src/sssom_loader.rs` — SSSOM/TSV → `oxrdf::Dataset` (F9).
- Modify: `crates/harness/src/lib.rs` (or `bin/harness.rs` module wiring) — expose the loader.
- Create: `crates/harness/tests/fixtures/sssom-mappings/manifest.ttl` + premise/conclusion `.ttl` fixtures + one real `.sssom.tsv` slice.
- Modify: `crates/harness/src/testcase.rs` + `src/runner.rs` — `Suite::SssomMappings` key + dispatch.
- Modify: `harness/selected.toml` — `[suites.sssom-mappings]`.
- Create: `harness/curation/sssom-mappings.md` — the curated-subset rationale doc.

**docs sync (same commits as the code):**
- Modify: `docs/architecture.md` §13 (flip Status fields), `BENCHMARKS.md` (NF rows), `TASKS.md` (#130), `docs/index.md` (if a new doc is linked).

---

## Task 1: Mapping-predicate, SSSOM-node, and internal-negated vocabulary (F1)

**Files:**
- Modify: `crates/owlrl/src/vocab.rs` (struct fields ~line 126; `synthetic()` ~line 188)
- Modify: `crates/owlrl/src/integration.rs` (IRI consts ~line 72; `build_vocab()` ~line 764; `USER_TERMS_BASE` line 74)
- Test: `crates/owlrl/src/vocab.rs` (`mod tests`, ~line 197)

We add **19** vocab terms in one task because they are interdependent (the rules in later tasks reference them) and the `debug_assert_eq!` self-check forces them to be added together.

The 19 terms:
- SKOS mapping relations (5): `skos:exactMatch`, `skos:closeMatch`, `skos:broadMatch`, `skos:narrowMatch`, `skos:relatedMatch`
- semapv cross-species (3): `semapv:crossSpeciesExactMatch`, `semapv:crossSpeciesNarrowMatch`, `semapv:crossSpeciesBroadMatch`
- semapv justifications (2): `semapv:MappingChaining`, `semapv:MappingInversion`
- SSSOM n-ary node (8): `sssom:Mapping` (class), `sssom:subject_id`, `sssom:predicate_id`, `sssom:object_id`, `sssom:mapping_justification`, `sssom:confidence`, `sssom:predicate_modifier`, `sssom:derived_from`
- internal negated predicate (1): `horndb:notExactMatch`

(`owl:equivalentClass`, `owl:equivalentProperty`, `owl:sameAs`, `rdfs:subClassOf`, `rdfs:subPropertyOf` already exist — do not re-add.)

- [ ] **Step 1: Write the failing test** — extend the vocab unit test to assert the new fields exist and are distinct.

In `crates/owlrl/src/vocab.rs`, inside `mod tests`, add:

```rust
    #[test]
    fn sssom_terms_present_and_distinct() {
        let v = Vocabulary::synthetic(100);
        // mapping predicates
        assert_ne!(v.skos_exact_match, v.skos_broad_match);
        assert_ne!(v.skos_narrow_match, v.skos_close_match);
        assert_ne!(v.skos_related_match, v.skos_exact_match);
        // cross-species
        assert_ne!(v.semapv_cross_species_exact_match, v.semapv_cross_species_narrow_match);
        assert_ne!(v.semapv_cross_species_broad_match, v.semapv_cross_species_exact_match);
        // justifications
        assert_ne!(v.semapv_mapping_chaining, v.semapv_mapping_inversion);
        // n-ary node slots
        assert_ne!(v.sssom_mapping, v.sssom_subject_id);
        assert_ne!(v.sssom_predicate_id, v.sssom_object_id);
        assert_ne!(v.sssom_mapping_justification, v.sssom_confidence);
        assert_ne!(v.sssom_predicate_modifier, v.sssom_derived_from);
        // internal negated
        assert_ne!(v.horndb_not_exact_match, v.skos_exact_match);
    }
```

- [ ] **Step 2: Run the test to verify it fails to compile**

Run: `cargo test -p horndb-owlrl --lib vocab::tests::sssom_terms_present_and_distinct`
Expected: FAIL — `error[E0609]: no field 'skos_exact_match' on type 'Vocabulary'`.

- [ ] **Step 3: Add the 19 struct fields**

In `crates/owlrl/src/vocab.rs`, immediately before the closing `}` of `struct Vocabulary` (after `owl_named_individual`, ~line 126), add:

```rust
    // --- SPEC-11 SSSOM mapping predicates (F1) ---
    /// `skos:exactMatch`
    pub skos_exact_match: TermId,
    /// `skos:closeMatch`
    pub skos_close_match: TermId,
    /// `skos:broadMatch`
    pub skos_broad_match: TermId,
    /// `skos:narrowMatch`
    pub skos_narrow_match: TermId,
    /// `skos:relatedMatch`
    pub skos_related_match: TermId,
    /// `semapv:crossSpeciesExactMatch`
    pub semapv_cross_species_exact_match: TermId,
    /// `semapv:crossSpeciesNarrowMatch`
    pub semapv_cross_species_narrow_match: TermId,
    /// `semapv:crossSpeciesBroadMatch`
    pub semapv_cross_species_broad_match: TermId,
    /// `semapv:MappingChaining`
    pub semapv_mapping_chaining: TermId,
    /// `semapv:MappingInversion`
    pub semapv_mapping_inversion: TermId,
    // SSSOM n-ary mapping node (F2)
    /// `sssom:Mapping`
    pub sssom_mapping: TermId,
    /// `sssom:subject_id`
    pub sssom_subject_id: TermId,
    /// `sssom:predicate_id`
    pub sssom_predicate_id: TermId,
    /// `sssom:object_id`
    pub sssom_object_id: TermId,
    /// `sssom:mapping_justification`
    pub sssom_mapping_justification: TermId,
    /// `sssom:confidence`
    pub sssom_confidence: TermId,
    /// `sssom:predicate_modifier`
    pub sssom_predicate_modifier: TermId,
    /// `sssom:derived_from`
    pub sssom_derived_from: TermId,
    // Internal negated mapping predicate (F4) — NOT a public IRI for chaining.
    /// `horndb:notExactMatch`
    pub horndb_not_exact_match: TermId,
```

- [ ] **Step 4: Add 19 matching lines to `synthetic()`**

In `crates/owlrl/src/vocab.rs`, immediately before the closing `}` of the `Self { … }` literal in `synthetic()` (after `owl_named_individual: next(),`, ~line 188), add — **in the same order as the struct fields**:

```rust
            skos_exact_match: next(),
            skos_close_match: next(),
            skos_broad_match: next(),
            skos_narrow_match: next(),
            skos_related_match: next(),
            semapv_cross_species_exact_match: next(),
            semapv_cross_species_narrow_match: next(),
            semapv_cross_species_broad_match: next(),
            semapv_mapping_chaining: next(),
            semapv_mapping_inversion: next(),
            sssom_mapping: next(),
            sssom_subject_id: next(),
            sssom_predicate_id: next(),
            sssom_object_id: next(),
            sssom_mapping_justification: next(),
            sssom_confidence: next(),
            sssom_predicate_modifier: next(),
            sssom_derived_from: next(),
            horndb_not_exact_match: next(),
```

- [ ] **Step 5: Add IRI consts + `build_vocab()` lines + bump `USER_TERMS_BASE` in `integration.rs`**

In `crates/owlrl/src/integration.rs`, after the last `const OWL_NAMED_INDIVIDUAL` (~line 72), add:

```rust
// SPEC-11 SSSOM mapping vocabulary (F1).
const SKOS_EXACT_MATCH: &str = "http://www.w3.org/2004/02/skos/core#exactMatch";
const SKOS_CLOSE_MATCH: &str = "http://www.w3.org/2004/02/skos/core#closeMatch";
const SKOS_BROAD_MATCH: &str = "http://www.w3.org/2004/02/skos/core#broadMatch";
const SKOS_NARROW_MATCH: &str = "http://www.w3.org/2004/02/skos/core#narrowMatch";
const SKOS_RELATED_MATCH: &str = "http://www.w3.org/2004/02/skos/core#relatedMatch";
const SEMAPV_CROSS_SPECIES_EXACT_MATCH: &str = "https://w3id.org/semapv/vocab/crossSpeciesExactMatch";
const SEMAPV_CROSS_SPECIES_NARROW_MATCH: &str = "https://w3id.org/semapv/vocab/crossSpeciesNarrowMatch";
const SEMAPV_CROSS_SPECIES_BROAD_MATCH: &str = "https://w3id.org/semapv/vocab/crossSpeciesBroadMatch";
const SEMAPV_MAPPING_CHAINING: &str = "https://w3id.org/semapv/vocab/MappingChaining";
const SEMAPV_MAPPING_INVERSION: &str = "https://w3id.org/semapv/vocab/MappingInversion";
const SSSOM_MAPPING: &str = "https://w3id.org/sssom/Mapping";
const SSSOM_SUBJECT_ID: &str = "https://w3id.org/sssom/subject_id";
const SSSOM_PREDICATE_ID: &str = "https://w3id.org/sssom/predicate_id";
const SSSOM_OBJECT_ID: &str = "https://w3id.org/sssom/object_id";
const SSSOM_MAPPING_JUSTIFICATION: &str = "https://w3id.org/sssom/mapping_justification";
const SSSOM_CONFIDENCE: &str = "https://w3id.org/sssom/confidence";
const SSSOM_PREDICATE_MODIFIER: &str = "https://w3id.org/sssom/predicate_modifier";
const SSSOM_DERIVED_FROM: &str = "https://w3id.org/sssom/derived_from";
const HORNDB_NOT_EXACT_MATCH: &str = "https://w3id.org/horndb/internal#notExactMatch";
```

In `build_vocab()`, immediately before the closing `}` of the `Vocabulary { … }` literal (after `owl_named_individual: alloc(OWL_NAMED_INDIVIDUAL),`, ~line 764), add — **same order**:

```rust
        skos_exact_match: alloc(SKOS_EXACT_MATCH),
        skos_close_match: alloc(SKOS_CLOSE_MATCH),
        skos_broad_match: alloc(SKOS_BROAD_MATCH),
        skos_narrow_match: alloc(SKOS_NARROW_MATCH),
        skos_related_match: alloc(SKOS_RELATED_MATCH),
        semapv_cross_species_exact_match: alloc(SEMAPV_CROSS_SPECIES_EXACT_MATCH),
        semapv_cross_species_narrow_match: alloc(SEMAPV_CROSS_SPECIES_NARROW_MATCH),
        semapv_cross_species_broad_match: alloc(SEMAPV_CROSS_SPECIES_BROAD_MATCH),
        semapv_mapping_chaining: alloc(SEMAPV_MAPPING_CHAINING),
        semapv_mapping_inversion: alloc(SEMAPV_MAPPING_INVERSION),
        sssom_mapping: alloc(SSSOM_MAPPING),
        sssom_subject_id: alloc(SSSOM_SUBJECT_ID),
        sssom_predicate_id: alloc(SSSOM_PREDICATE_ID),
        sssom_object_id: alloc(SSSOM_OBJECT_ID),
        sssom_mapping_justification: alloc(SSSOM_MAPPING_JUSTIFICATION),
        sssom_confidence: alloc(SSSOM_CONFIDENCE),
        sssom_predicate_modifier: alloc(SSSOM_PREDICATE_MODIFIER),
        sssom_derived_from: alloc(SSSOM_DERIVED_FROM),
        horndb_not_exact_match: alloc(HORNDB_NOT_EXACT_MATCH),
```

Then change `USER_TERMS_BASE` (line 74) from `49` to `68` (48 existing terms + 19 new = 67 vocab IDs occupying `1..=67`, so the first user term is `68`), and update its doc comment to `Vocabulary terms occupy 1..=67.`. The `debug_assert_eq!(id, USER_TERMS_BASE)` at the end of `build_vocab` validates this.

- [ ] **Step 6: Run the vocab test + a build to confirm the count is right**

Run: `cargo build -p horndb-owlrl && cargo test -p horndb-owlrl --lib vocab::`
Expected: PASS. (If `build_vocab`'s `debug_assert_eq!` panics in any `Engine`-using test, you miscounted `USER_TERMS_BASE` — recount.)

- [ ] **Step 7: Commit**

```bash
git add crates/owlrl/src/vocab.rs crates/owlrl/src/integration.rs
git commit -m 'feat(owlrl): SPEC-11 F1 SSSOM mapping vocabulary'
```

---

## Task 2: RG1/RG2 generalisation rules (F3)

**Files:**
- Modify: `crates/owlrl/rules.toml` (append at end, before EOF)
- Test: `crates/owlrl/tests/sssom_rules.rs` (new file)

RG1: `A owl:equivalentClass B ⟹ A skos:exactMatch B`. RG2: `A rdfs:subClassOf B ⟹ A skos:broadMatch B`. Both are single-body compiled rules (deliberate weakening when mixing OWL- and SKOS-strength mappings).

- [ ] **Step 1: Write the failing test** — create `crates/owlrl/tests/sssom_rules.rs`:

```rust
//! SPEC-11 F3 — SSSOM chaining rule conformance (RG / RI / RCE).
use horndb_owlrl::backend::RuleFiringBackend;
use horndb_owlrl::engine::materialize;
use horndb_owlrl::store::MemStore;
use horndb_owlrl::types::{TermId, Triple};
use horndb_owlrl::vocab::Vocabulary;

fn t(s: TermId, p: TermId, o: TermId) -> Triple {
    Triple::new(s, p, o)
}

fn run(setup: impl FnOnce(&mut MemStore, &Vocabulary)) -> (MemStore, Vocabulary) {
    let v = Vocabulary::synthetic(10_000);
    let mut s = MemStore::new(v);
    setup(&mut s, &v);
    materialize(&mut s, &mut RuleFiringBackend::new());
    (s, v)
}

#[test]
fn rg1_equivalent_class_generalises_to_exact_match() {
    let a = TermId(1);
    let b = TermId(2);
    let (s, v) = run(|s, v| s.assert(t(a, v.owl_equivalent_class, b)));
    assert!(s.contains(&t(a, v.skos_exact_match, b)));
}

#[test]
fn rg2_subclass_generalises_to_broad_match() {
    let a = TermId(1);
    let b = TermId(2);
    let (s, v) = run(|s, v| s.assert(t(a, v.rdfs_sub_class_of, b)));
    assert!(s.contains(&t(a, v.skos_broad_match, b)));
}
```

> NOTE: confirm the exact public paths (`horndb_owlrl::engine::materialize`, `store::MemStore`, `types::{TermId, Triple}`, `vocab::Vocabulary`, `backend::RuleFiringBackend`) against `crates/owlrl/src/lib.rs` — adjust the `use` lines if a module is re-exported under a different path. The harness uses these same types, so they are public.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p horndb-owlrl --test sssom_rules rg1`
Expected: FAIL — the derived `skos:exactMatch` triple is absent.

- [ ] **Step 3: Add RG1/RG2 to `rules.toml`**

Append to `crates/owlrl/rules.toml`:

```toml
# --- SPEC-11 F3: SSSOM generalisation rules (RG1, RG2) ---
# Deliberate weakening: OWL-strength axioms generalise to SKOS-strength
# crosswalk edges so OWL- and SKOS-sourced mappings compose in one layer.
[[rule]]
id = "sssom-rg1"
comment = "SSSOM RG1: owl:equivalentClass generalises to skos:exactMatch."
body = [
  { s = "?a", p = "owl:equivalentClass", o = "?b" },
]
head = { s = "?a", p = "skos:exactMatch", o = "?b" }

[[rule]]
id = "sssom-rg2"
comment = "SSSOM RG2: rdfs:subClassOf generalises to skos:broadMatch."
body = [
  { s = "?a", p = "rdfs:subClassOf", o = "?b" },
]
head = { s = "?a", p = "skos:broadMatch", o = "?b" }
```

- [ ] **Step 4: Build, inspect, and run the test**

```bash
cargo run -p horndb-owlrl --bin show-rule -- sssom-rg1   # eyeball generated fire_sssom_rg1
cargo test -p horndb-owlrl --test sssom_rules rg
```
Expected: both `rg1`/`rg2` PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/owlrl/rules.toml crates/owlrl/tests/sssom_rules.rs
git commit -m 'feat(owlrl): SPEC-11 F3 RG1/RG2 generalisation rules'
```

---

## Task 3: RI1–5 inverse rules (F3)

**Files:**
- Modify: `crates/owlrl/rules.toml`
- Test: `crates/owlrl/tests/sssom_rules.rs`

The inverse pairs (each direction is one rule): `narrowMatch ↔ broadMatch`; `crossSpeciesNarrowMatch ↔ crossSpeciesBroadMatch`; and `crossSpeciesExactMatch` is its own inverse (symmetric). That is 5 rules total (RI1–5), all tagged `semapv:MappingInversion` at justification time (Task 8).

- [ ] **Step 1: Write the failing tests** — append to `crates/owlrl/tests/sssom_rules.rs`:

```rust
#[test]
fn ri_narrow_inverts_to_broad() {
    let a = TermId(1);
    let b = TermId(2);
    let (s, v) = run(|s, v| s.assert(t(a, v.skos_narrow_match, b)));
    assert!(s.contains(&t(b, v.skos_broad_match, a)));
}

#[test]
fn ri_broad_inverts_to_narrow() {
    let a = TermId(1);
    let b = TermId(2);
    let (s, v) = run(|s, v| s.assert(t(a, v.skos_broad_match, b)));
    assert!(s.contains(&t(b, v.skos_narrow_match, a)));
}

#[test]
fn ri_cross_species_exact_is_symmetric() {
    let a = TermId(1);
    let b = TermId(2);
    let (s, v) = run(|s, v| s.assert(t(a, v.semapv_cross_species_exact_match, b)));
    assert!(s.contains(&t(b, v.semapv_cross_species_exact_match, a)));
}

#[test]
fn ri_cross_species_narrow_inverts_to_broad() {
    let a = TermId(1);
    let b = TermId(2);
    let (s, v) = run(|s, v| s.assert(t(a, v.semapv_cross_species_narrow_match, b)));
    assert!(s.contains(&t(b, v.semapv_cross_species_broad_match, a)));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p horndb-owlrl --test sssom_rules ri_`
Expected: FAIL.

- [ ] **Step 3: Add RI1–5 to `rules.toml`**

```toml
# --- SPEC-11 F3: SSSOM inversion rules (RI1-5) — tagged semapv:MappingInversion ---
[[rule]]
id = "sssom-ri1"
comment = "SSSOM RI1: skos:narrowMatch inverts to skos:broadMatch."
body = [
  { s = "?a", p = "skos:narrowMatch", o = "?b" },
]
head = { s = "?b", p = "skos:broadMatch", o = "?a" }

[[rule]]
id = "sssom-ri2"
comment = "SSSOM RI2: skos:broadMatch inverts to skos:narrowMatch."
body = [
  { s = "?a", p = "skos:broadMatch", o = "?b" },
]
head = { s = "?b", p = "skos:narrowMatch", o = "?a" }

[[rule]]
id = "sssom-ri3"
comment = "SSSOM RI3: semapv:crossSpeciesExactMatch is symmetric (self-inverse)."
body = [
  { s = "?a", p = "semapv:crossSpeciesExactMatch", o = "?b" },
]
head = { s = "?b", p = "semapv:crossSpeciesExactMatch", o = "?a" }

[[rule]]
id = "sssom-ri4"
comment = "SSSOM RI4: semapv:crossSpeciesNarrowMatch inverts to crossSpeciesBroadMatch."
body = [
  { s = "?a", p = "semapv:crossSpeciesNarrowMatch", o = "?b" },
]
head = { s = "?b", p = "semapv:crossSpeciesBroadMatch", o = "?a" }

[[rule]]
id = "sssom-ri5"
comment = "SSSOM RI5: semapv:crossSpeciesBroadMatch inverts to crossSpeciesNarrowMatch."
body = [
  { s = "?a", p = "semapv:crossSpeciesBroadMatch", o = "?b" },
]
head = { s = "?b", p = "semapv:crossSpeciesNarrowMatch", o = "?a" }
```

- [ ] **Step 4: Build + test**

```bash
cargo test -p horndb-owlrl --test sssom_rules ri_
```
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/owlrl/rules.toml crates/owlrl/tests/sssom_rules.rs
git commit -m 'feat(owlrl): SPEC-11 F3 RI1-5 inversion rules'
```

---

## Task 4: RCE1/RCE2 role-chain rules (F3)

**Files:**
- Modify: `crates/owlrl/rules.toml`
- Test: `crates/owlrl/tests/sssom_rules.rs`

RCE composes an identity-ish leading edge (`skos:exactMatch` or `owl:equivalentClass`) with a *following mapping edge* `p`, propagating `p`. Per the locked decision, `p` ranges over the SSSOM mapping predicates only — so we instantiate one rule per `(leading, p)`. To keep the conformance subset tractable we cover the representative set the acceptance subset exercises: leading `skos:exactMatch` composed with `{broadMatch, narrowMatch, exactMatch}` (RCE1 family), and the mirror — a mapping edge `p` followed by a trailing `skos:exactMatch` (RCE2 family).

- [ ] **Step 1: Write the failing tests** — append to `crates/owlrl/tests/sssom_rules.rs`:

```rust
#[test]
fn rce1_exact_then_broad_propagates_broad() {
    // A exactMatch B, B broadMatch C  =>  A broadMatch C
    let a = TermId(1);
    let b = TermId(2);
    let c = TermId(3);
    let (s, v) = run(|s, v| {
        s.assert(t(a, v.skos_exact_match, b));
        s.assert(t(b, v.skos_broad_match, c));
    });
    assert!(s.contains(&t(a, v.skos_broad_match, c)));
}

#[test]
fn rce2_broad_then_exact_propagates_broad() {
    // A broadMatch B, B exactMatch C  =>  A broadMatch C
    let a = TermId(1);
    let b = TermId(2);
    let c = TermId(3);
    let (s, v) = run(|s, v| {
        s.assert(t(a, v.skos_broad_match, b));
        s.assert(t(b, v.skos_exact_match, c));
    });
    assert!(s.contains(&t(a, v.skos_broad_match, c)));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p horndb-owlrl --test sssom_rules rce`
Expected: FAIL.

- [ ] **Step 3: Add the RCE rules to `rules.toml`**

```toml
# --- SPEC-11 F3: SSSOM role-chain rules (RCE1/RCE2) ---
# RCE1: exactMatch o p  => p   (leading exactMatch, p in {broad,narrow,exact}).
# RCE2: p o exactMatch  => p   (trailing exactMatch mirror).
# p is restricted to mapping predicates (ADR-0017): exactMatch is a crosswalk
# edge, never an identity that substitutes across arbitrary triples.
[[rule]]
id = "sssom-rce1-broad"
comment = "SSSOM RCE1: exactMatch o broadMatch => broadMatch."
body = [
  { s = "?a", p = "skos:exactMatch", o = "?b" },
  { s = "?b", p = "skos:broadMatch", o = "?c" },
]
head = { s = "?a", p = "skos:broadMatch", o = "?c" }

[[rule]]
id = "sssom-rce1-narrow"
comment = "SSSOM RCE1: exactMatch o narrowMatch => narrowMatch."
body = [
  { s = "?a", p = "skos:exactMatch", o = "?b" },
  { s = "?b", p = "skos:narrowMatch", o = "?c" },
]
head = { s = "?a", p = "skos:narrowMatch", o = "?c" }

[[rule]]
id = "sssom-rce1-exact"
comment = "SSSOM RCE1: exactMatch o exactMatch => exactMatch."
body = [
  { s = "?a", p = "skos:exactMatch", o = "?b" },
  { s = "?b", p = "skos:exactMatch", o = "?c" },
]
head = { s = "?a", p = "skos:exactMatch", o = "?c" }

[[rule]]
id = "sssom-rce2-broad"
comment = "SSSOM RCE2: broadMatch o exactMatch => broadMatch."
body = [
  { s = "?a", p = "skos:broadMatch", o = "?b" },
  { s = "?b", p = "skos:exactMatch", o = "?c" },
]
head = { s = "?a", p = "skos:broadMatch", o = "?c" }

[[rule]]
id = "sssom-rce2-narrow"
comment = "SSSOM RCE2: narrowMatch o exactMatch => narrowMatch."
body = [
  { s = "?a", p = "skos:narrowMatch", o = "?b" },
  { s = "?b", p = "skos:exactMatch", o = "?c" },
]
head = { s = "?a", p = "skos:narrowMatch", o = "?c" }
```

> NOTE: `sssom-rce1-exact` makes `skos:exactMatch` transitive purely through compiled rule firing. Task 5 *also* delegates `skos:exactMatch` transitivity to the closure backend (the faster path). Having both is harmless — each derived triple is novelty-checked (`!store.contains && !out.contains`) before insertion — but if `show-rule`/profiling later flags redundant work, the canonical home for `exactMatch` transitivity is the closure delegation; drop `sssom-rce1-exact` then. Keep it for now so this task is independently green without Task 5.

- [ ] **Step 4: Build + test**

```bash
cargo test -p horndb-owlrl --test sssom_rules rce
```
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/owlrl/rules.toml crates/owlrl/tests/sssom_rules.rs
git commit -m 'feat(owlrl): SPEC-11 F3 RCE1/RCE2 role-chain rules'
```

---

## Task 5: T1 transitivity via closure delegation (F3)

**Files:**
- Modify: `crates/owlrl/rules.toml` (delegated rule blocks)
- Modify: `crates/owlrl/src/backend.rs` (`RuleFiringBackend::close()`)
- Test: `crates/owlrl/tests/sssom_rules.rs`

We add transitive closure for `skos:exactMatch`, `skos:broadMatch`, `skos:narrowMatch`. Each gets a `delegate = "closure"` block (table completeness/documentation) **and** a line in `RuleFiringBackend::close()` (the actual computation, since the harness uses this backend).

- [ ] **Step 1: Write the failing test** — append to `crates/owlrl/tests/sssom_rules.rs`:

```rust
#[test]
fn t1_broad_match_is_transitive() {
    // A broadMatch B broadMatch C  =>  A broadMatch C
    let a = TermId(1);
    let b = TermId(2);
    let c = TermId(3);
    let (s, v) = run(|s, v| {
        s.assert(t(a, v.skos_broad_match, b));
        s.assert(t(b, v.skos_broad_match, c));
    });
    assert!(s.contains(&t(a, v.skos_broad_match, c)));
}

#[test]
fn t1_narrow_match_is_transitive() {
    let a = TermId(1);
    let b = TermId(2);
    let c = TermId(3);
    let (s, v) = run(|s, v| {
        s.assert(t(a, v.skos_narrow_match, b));
        s.assert(t(b, v.skos_narrow_match, c));
    });
    assert!(s.contains(&t(a, v.skos_narrow_match, c)));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p horndb-owlrl --test sssom_rules t1_`
Expected: FAIL — transitive `broadMatch`/`narrowMatch` not derived (RCE only chains *through* `exactMatch`, not same-predicate transitivity).

- [ ] **Step 3: Add `delegate="closure"` blocks to `rules.toml`**

```toml
# --- SPEC-11 F3: SSSOM transitivity (T1) — delegated to the closure backend ---
# Only the SKOS mapping predicates are added here. subClassOf/subPropertyOf/
# sameAs/equivalentClass/equivalentProperty are ALREADY closed by the existing
# OWL machinery (scm-sco, scm-spo, eq-trans, scm-eqc*/scm-eqp*) and are not
# re-added (ADR-0017: do not re-close identity-strength predicates here).
[[rule]]
id = "sssom-t1-exact"
comment = "SSSOM T1: skos:exactMatch transitivity (closure-delegated)."
delegate = "closure"
body = [
  { s = "?a", p = "skos:exactMatch", o = "?b" },
  { s = "?b", p = "skos:exactMatch", o = "?c" },
]
head = { s = "?a", p = "skos:exactMatch", o = "?c" }

[[rule]]
id = "sssom-t1-broad"
comment = "SSSOM T1: skos:broadMatch transitivity (closure-delegated)."
delegate = "closure"
body = [
  { s = "?a", p = "skos:broadMatch", o = "?b" },
  { s = "?b", p = "skos:broadMatch", o = "?c" },
]
head = { s = "?a", p = "skos:broadMatch", o = "?c" }

[[rule]]
id = "sssom-t1-narrow"
comment = "SSSOM T1: skos:narrowMatch transitivity (closure-delegated)."
delegate = "closure"
body = [
  { s = "?a", p = "skos:narrowMatch", o = "?b" },
  { s = "?b", p = "skos:narrowMatch", o = "?c" },
]
head = { s = "?a", p = "skos:narrowMatch", o = "?c" }
```

- [ ] **Step 4: Extend `RuleFiringBackend::close()` in `backend.rs`**

In `crates/owlrl/src/backend.rs`, inside the `loop` in `close()`, after the existing `close_transitive(store, v.owl_same_as, "eq-trans", &mut out);` line (line 55), add:

```rust
            // SPEC-11 T1: SSSOM mapping-predicate transitivity.
            close_transitive(store, v.skos_exact_match, "sssom-t1-exact", &mut out);
            close_transitive(store, v.skos_broad_match, "sssom-t1-broad", &mut out);
            close_transitive(store, v.skos_narrow_match, "sssom-t1-narrow", &mut out);
```

(`close_transitive` is the existing helper at line 67 — it already does the right `?a p ?b ∧ ?b p ?c ⟹ ?a p ?c` chain-close with provenance. No new helper needed.)

- [ ] **Step 5: Build + test (full sssom_rules suite)**

```bash
cargo test -p horndb-owlrl --test sssom_rules
```
Expected: every test (RG, RI, RCE, T1) PASS.

> NOTE — production GraphBLAS backend: the feature-gated `GraphBlas` backend (`crates/owlrl/src/graphblas_backend.rs`, behind `--features graphblas-backend`) has its own hard-coded predicate list and does **not** yet close these three. Bringing it to parity is tracked as a SPEC-11/SPEC-05 follow-up (and is covered by the existing `closure_backend_differential.rs` pattern). It is **out of scope** for this reasoning-slice plan because the harness runs the default `RuleFiringBackend`. Add a one-line TODO comment referencing TASKS.md #130 in `graphblas_backend.rs` next to its predicate list so the gap is discoverable.

- [ ] **Step 6: Commit**

```bash
git add crates/owlrl/rules.toml crates/owlrl/src/backend.rs crates/owlrl/tests/sssom_rules.rs crates/owlrl/src/graphblas_backend.rs
git commit -m 'feat(owlrl): SPEC-11 F3 T1 mapping-predicate transitivity via closure'
```

---

## Task 6: Negative mapping chaining (F4)

**Files:**
- Modify: `crates/owlrl/rules.toml`
- Modify: `crates/owlrl/src/backend.rs`
- Test: `crates/owlrl/tests/sssom_negative.rs` (new file)

Per F4, a negated mapping `A exactMatch[Not] B` is modelled as a **positive** fact over the internal predicate `horndb:notExactMatch` (added in Task 1). Negative-chaining is then ordinary monotone Datalog with **no negation-as-failure**: `exactMatch(A,B) ∧ notExactMatch(B,C) ⟹ notExactMatch(A,C)`. The `inference.md` xanthene example (positive ∘ Not ⟹ Not) must derive `notExactMatch`, and crucially must **not** derive a positive `exactMatch` across the negative link.

- [ ] **Step 1: Write the failing test** — create `crates/owlrl/tests/sssom_negative.rs`:

```rust
//! SPEC-11 F4 — monotone negative-mapping chaining (the inference.md xanthene case).
use horndb_owlrl::backend::RuleFiringBackend;
use horndb_owlrl::engine::materialize;
use horndb_owlrl::store::MemStore;
use horndb_owlrl::types::{TermId, Triple};
use horndb_owlrl::vocab::Vocabulary;

fn t(s: TermId, p: TermId, o: TermId) -> Triple {
    Triple::new(s, p, o)
}

#[test]
fn positive_then_negative_yields_negative_not_positive() {
    let v = Vocabulary::synthetic(10_000);
    let mut s = MemStore::new(v);
    let a = TermId(1); // e.g. xanthene-A
    let b = TermId(2);
    let c = TermId(3);
    // A exactMatch B (positive),  B exactMatch[Not] C (negated).
    s.assert(t(a, v.skos_exact_match, b));
    s.assert(t(b, v.horndb_not_exact_match, c));
    materialize(&mut s, &mut RuleFiringBackend::new());

    // Derives the negated A-C mapping...
    assert!(s.contains(&t(a, v.horndb_not_exact_match, c)));
    // ...and must NOT derive a positive exactMatch across the negative link.
    assert!(!s.contains(&t(a, v.skos_exact_match, c)));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p horndb-owlrl --test sssom_negative`
Expected: FAIL — `notExactMatch(a,c)` absent.

- [ ] **Step 3: Add the negative-chaining rule to `rules.toml`**

```toml
# --- SPEC-11 F4: monotone negative-mapping chaining (no negation-as-failure) ---
# A negated exactMatch is a positive fact over horndb:notExactMatch. Composing a
# positive exactMatch with a negated one yields a negated one. The negated
# predicate is excluded from the positive crosswalk index (F5, follow-up plan).
[[rule]]
id = "sssom-neg-exact"
comment = "SSSOM F4: exactMatch o notExactMatch => notExactMatch (monotone)."
body = [
  { s = "?a", p = "skos:exactMatch", o = "?b" },
  { s = "?b", p = "horndb:notExactMatch", o = "?c" },
]
head = { s = "?a", p = "horndb:notExactMatch", o = "?c" }
```

- [ ] **Step 4: Build + test**

```bash
cargo run -p horndb-owlrl --bin show-rule -- sssom-neg-exact
cargo test -p horndb-owlrl --test sssom_negative
```
Expected: PASS.

> NOTE: this rule is a *compiled* (non-delegated) rule with a constant leading predicate, so it needs no backend change. `horndb:notExactMatch` is intentionally not transitive on its own (a chain of two negatives does not entail a negative) — do not add a `notExactMatch ∘ notExactMatch` closure.

- [ ] **Step 5: Commit**

```bash
git add crates/owlrl/rules.toml crates/owlrl/tests/sssom_negative.rs
git commit -m 'feat(owlrl): SPEC-11 F4 monotone negative-mapping chaining'
```

---

## Task 7: Identity isolation differential test (ADR-0017, acceptance #4)

**Files:**
- Test: `crates/owlrl/tests/sssom_identity_isolation.rs` (new file)

This task adds **no production code** — it is a guard test proving the ADR-0017 invariant holds: `skos:exactMatch` never yields `owl:sameAs` entailment (no `eq-rep-*` firing on mapping edges), while genuine `owl:sameAs`/`owl:equivalentClass` still reach identity.

- [ ] **Step 1: Write the test** — create `crates/owlrl/tests/sssom_identity_isolation.rs`:

```rust
//! ADR-0017 — skos:exactMatch is a crosswalk edge, NEVER OWL identity.
use horndb_owlrl::backend::RuleFiringBackend;
use horndb_owlrl::engine::materialize;
use horndb_owlrl::store::MemStore;
use horndb_owlrl::types::{TermId, Triple};
use horndb_owlrl::vocab::Vocabulary;

fn t(s: TermId, p: TermId, o: TermId) -> Triple {
    Triple::new(s, p, o)
}

#[test]
fn exact_match_never_becomes_sameas() {
    let v = Vocabulary::synthetic(10_000);
    let mut s = MemStore::new(v);
    let a = TermId(1);
    let b = TermId(2);
    let p = TermId(3);
    let o = TermId(4);
    // A exactMatch B, plus a triple about A.
    s.assert(t(a, v.skos_exact_match, b));
    s.assert(t(a, p, o));
    materialize(&mut s, &mut RuleFiringBackend::new());

    // exactMatch must NOT create owl:sameAs identity...
    assert!(!s.contains(&t(a, v.owl_same_as, b)));
    // ...and must NOT substitute A's triples onto B (no eq-rep-* over a crosswalk).
    assert!(!s.contains(&t(b, p, o)));
}

#[test]
fn sameas_still_reaches_identity() {
    let v = Vocabulary::synthetic(10_000);
    let mut s = MemStore::new(v);
    let a = TermId(1);
    let b = TermId(2);
    let p = TermId(3);
    let o = TermId(4);
    s.assert(t(a, v.owl_same_as, b));
    s.assert(t(a, p, o));
    materialize(&mut s, &mut RuleFiringBackend::new());

    // Genuine owl:sameAs DOES substitute (eq-rep-s) and is symmetric (eq-sym).
    assert!(s.contains(&t(b, p, o)));
    assert!(s.contains(&t(b, v.owl_same_as, a)));
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p horndb-owlrl --test sssom_identity_isolation`
Expected: PASS immediately — no mapping rule bridges to `owl:sameAs`, and `eq-rep-*` only fires on `owl:sameAs` edges. If `exact_match_never_becomes_sameas` *fails*, a rule added in Tasks 2–6 wrongly references `owl:sameAs` in a head — find and fix it before proceeding.

- [ ] **Step 3: Commit**

```bash
git add crates/owlrl/tests/sssom_identity_isolation.rs
git commit -m 'test(owlrl): SPEC-11 ADR-0017 identity-isolation guard'
```

---

## Task 8: Justification tagging + mapping-node representation + confidence (F2, F7, F8)

**Files:**
- Create: `crates/owlrl/src/sssom.rs`
- Modify: `crates/owlrl/src/lib.rs` (add `pub mod sssom;`)
- Test: inline `#[cfg(test)]` in `crates/owlrl/src/sssom.rs`

This task adds the SSSOM-specific representation glue that the rule engine itself does not need but the spec requires: (F8/F2) map each chaining rule id to its `semapv:*` justification; (F2) build the n-ary `sssom:Mapping` node triples for an inferred mapping, including `sssom:derived_from` links to the premise mappings; (F7) combine premise confidences by product with a 1.0 default. Provenance (`rule_id` + `premises`) already exists from the compiled rules — this module *interprets* it into SSSOM terms.

- [ ] **Step 1: Write the failing tests** — create `crates/owlrl/src/sssom.rs`:

```rust
//! SPEC-11 F2/F7/F8 — SSSOM representation glue over the rule engine's
//! provenance: justification tagging, n-ary mapping-node construction, and
//! confidence combination. The chaining itself lives in `rules.toml`; this
//! module turns a derived mapping + its provenance into SSSOM-shaped facts.

use crate::types::{TermId, Triple};
use crate::vocab::Vocabulary;

/// The `semapv:*` justification a derived mapping carries, per SPEC-11 F3.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Justification {
    /// Derived by transitivity / role-chain / generalisation (T1, RCE, RG).
    Chaining,
    /// Derived by inversion (RI1-5).
    Inversion,
}

impl Justification {
    /// The vocab `TermId` for this justification (a `semapv:` individual).
    pub fn term(self, v: &Vocabulary) -> TermId {
        match self {
            Justification::Chaining => v.semapv_mapping_chaining,
            Justification::Inversion => v.semapv_mapping_inversion,
        }
    }
}

/// Map a chaining rule id to its SSSOM justification. Returns `None` for
/// non-mapping (ordinary OWL-RL) rule ids.
pub fn rule_justification(rule_id: &str) -> Option<Justification> {
    match rule_id {
        id if id.starts_with("sssom-ri") => Some(Justification::Inversion),
        id if id.starts_with("sssom-rg")
            || id.starts_with("sssom-rce")
            || id.starts_with("sssom-t1")
            || id.starts_with("sssom-neg") => Some(Justification::Chaining),
        _ => None,
    }
}

/// Combine confidences along a chain. SPEC-11 F7: product (independent-
/// probability) by default; unspecified confidence defaults to 1.0.
pub fn combine_confidence(premise_confidences: &[f64]) -> f64 {
    premise_confidences.iter().copied().product::<f64>()
}

/// The triples of an n-ary `sssom:Mapping` node for an inferred mapping
/// (SPEC-11 F2). `node` is a fresh blank/IRI TermId minted by the caller;
/// `derived_from` are the mapping-node ids of the premises (F8).
pub fn mapping_node_triples(
    v: &Vocabulary,
    node: TermId,
    subject: TermId,
    predicate: TermId,
    object: TermId,
    justification: Justification,
    derived_from: &[TermId],
) -> Vec<Triple> {
    let mut out = vec![
        Triple::new(node, v.rdf_type, v.sssom_mapping),
        Triple::new(node, v.sssom_subject_id, subject),
        Triple::new(node, v.sssom_predicate_id, predicate),
        Triple::new(node, v.sssom_object_id, object),
        Triple::new(node, v.sssom_mapping_justification, justification.term(v)),
    ];
    for &df in derived_from {
        out.push(Triple::new(node, v.sssom_derived_from, df));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn justification_mapping_is_correct() {
        assert_eq!(rule_justification("sssom-ri3"), Some(Justification::Inversion));
        assert_eq!(rule_justification("sssom-rg1"), Some(Justification::Chaining));
        assert_eq!(rule_justification("sssom-rce1-broad"), Some(Justification::Chaining));
        assert_eq!(rule_justification("sssom-t1-exact"), Some(Justification::Chaining));
        assert_eq!(rule_justification("sssom-neg-exact"), Some(Justification::Chaining));
        assert_eq!(rule_justification("cax-sco"), None);
    }

    #[test]
    fn confidence_combines_by_product_with_unit_default() {
        assert_eq!(combine_confidence(&[]), 1.0);
        assert_eq!(combine_confidence(&[0.9]), 0.9);
        assert!((combine_confidence(&[0.9, 0.8]) - 0.72).abs() < 1e-12);
    }

    #[test]
    fn mapping_node_emits_canonical_shape() {
        let v = Vocabulary::synthetic(10_000);
        let node = TermId(1);
        let triples = mapping_node_triples(
            &v, node, TermId(2), v.skos_broad_match, TermId(3),
            Justification::Chaining, &[TermId(7), TermId(8)],
        );
        assert!(triples.contains(&Triple::new(node, v.rdf_type, v.sssom_mapping)));
        assert!(triples.contains(&Triple::new(node, v.sssom_subject_id, TermId(2))));
        assert!(triples.contains(&Triple::new(node, v.sssom_mapping_justification, v.semapv_mapping_chaining)));
        assert!(triples.contains(&Triple::new(node, v.sssom_derived_from, TermId(7))));
        assert!(triples.contains(&Triple::new(node, v.sssom_derived_from, TermId(8))));
        assert_eq!(triples.len(), 7); // type + 3 slots + justification + 2 derived_from
    }
}
```

- [ ] **Step 2: Wire the module** — in `crates/owlrl/src/lib.rs`, add alongside the other `pub mod` lines:

```rust
pub mod sssom;
```

- [ ] **Step 3: Run to verify failure then pass**

Run: `cargo test -p horndb-owlrl --lib sssom::`
Expected: first run FAILs to compile (module absent) — after Steps 1–2, PASS.

> NOTE on `derived_from` / proof: SPEC-11 F8 says the `derived_from` set *is* the rule's premise set, and the proof tree bottoms out at asserted mappings. The engine already records `Provenance { rule_id, premises }` per derived triple and exposes `Engine::proof(s,p,o) -> StringProofTree` (see `crates/owlrl/CLAUDE.md` §3.6 / `tests/proof_tree.rs`). So F8 for *base-triple* mappings is already satisfied by existing machinery — `mapping_node_triples` here is the F2 n-ary-node *representation* layered on top, and Task 11's conformance test asserts the proof path end-to-end. Do **not** rebuild proof recording.

- [ ] **Step 4: Commit**

```bash
git add crates/owlrl/src/sssom.rs crates/owlrl/src/lib.rs
git commit -m 'feat(owlrl): SPEC-11 F2/F7/F8 justification, mapping-node, confidence'
```

---

## Task 9: Harness SSSOM/TSV loader (F9)

**Files:**
- Create: `crates/harness/src/sssom_loader.rs`
- Modify: `crates/harness/src/lib.rs` (add `mod sssom_loader;` / `pub use`)
- Test: inline `#[cfg(test)]` in `crates/harness/src/sssom_loader.rs`

A **harness-only** SSSOM/TSV reader (not a production path — production mappings arrive via the changefeed). It parses the commented-YAML header (→ `curie_map` + set-level defaults), expands CURIEs to IRIs, splits `|`-delimited multivalue cells, and emits the positive base triple `subject predicate object` per row into an `oxrdf::Dataset` (the type the harness `Reasoner::load` consumes). Negated rows (`predicate_modifier == "Not"`) emit the internal `horndb:notExactMatch` predicate instead.

Use `oxrdf` types already in the harness (see `crates/harness/src/rdf.rs` for the `Dataset`/`Quad`/`NamedNode`/`GraphName` imports and the existing loader shape).

- [ ] **Step 1: Write the failing test** — create `crates/harness/src/sssom_loader.rs` with the parser signature and a unit test over an inline TSV:

```rust
//! SPEC-11 F9 — harness-only SSSOM/TSV reader (bench/standalone). NOT a
//! production surface: production mappings arrive as RDF via the changefeed.
//!
//! Parses the commented-YAML header (curie_map + propagatable defaults),
//! expands CURIEs, splits `|`-multivalue cells, and emits positive base
//! triples (negated rows -> internal horndb:notExactMatch predicate).

use std::collections::HashMap;

use anyhow::{anyhow, Result};
use oxrdf::{Dataset, GraphName, NamedNode, Quad, Term};

/// IRI for the internal negated-exact-match predicate (mirrors
/// `horndb-owlrl`'s `HORNDB_NOT_EXACT_MATCH`). Keep in sync.
const HORNDB_NOT_EXACT_MATCH: &str = "https://w3id.org/horndb/internal#notExactMatch";

/// Parse SSSOM/TSV text into a `Dataset` of positive base triples.
pub fn parse_sssom_tsv(text: &str) -> Result<Dataset> {
    let mut curie_map: HashMap<String, String> = HashMap::new();
    let mut lines = text.lines().peekable();

    // 1. Commented-YAML header: lines starting with '#'. We only need the
    //    curie_map block (key: "prefix: expansion" under "curie_map:").
    let mut in_curie_map = false;
    while let Some(line) = lines.peek() {
        let Some(stripped) = line.strip_prefix('#') else { break };
        let content = stripped.trim_end();
        let trimmed = content.trim();
        if trimmed.starts_with("curie_map:") {
            in_curie_map = true;
        } else if in_curie_map {
            // Indented "  prefix: http://..." -> entry; dedent ends the block.
            let is_indented = content.starts_with("  ") || content.starts_with('\t');
            if is_indented && trimmed.contains(':') {
                let (prefix, exp) = trimmed.split_once(':').unwrap();
                curie_map.insert(
                    prefix.trim().to_string(),
                    exp.trim().trim_matches(|c| c == '"' || c == '\'').to_string(),
                );
            } else {
                in_curie_map = false;
            }
        }
        lines.next();
    }

    // 2. The column header row (first non-comment line).
    let header = lines
        .next()
        .ok_or_else(|| anyhow!("SSSOM TSV: missing column header row"))?;
    let cols: Vec<&str> = header.split('\t').collect();
    let col = |name: &str| cols.iter().position(|c| *c == name);
    let subj_i = col("subject_id").ok_or_else(|| anyhow!("missing subject_id column"))?;
    let pred_i = col("predicate_id").ok_or_else(|| anyhow!("missing predicate_id column"))?;
    let obj_i = col("object_id").ok_or_else(|| anyhow!("missing object_id column"))?;
    let modifier_i = col("predicate_modifier");

    // 3. Data rows.
    let mut ds = Dataset::new();
    for row in lines {
        if row.trim().is_empty() {
            continue;
        }
        let cells: Vec<&str> = row.split('\t').collect();
        let get = |i: usize| cells.get(i).copied().unwrap_or("").trim();
        let subjects = split_multi(get(subj_i));
        let predicate = get(pred_i);
        let objects = split_multi(get(obj_i));
        let negated = modifier_i.map(|i| get(i) == "Not").unwrap_or(false);

        let pred_iri = if negated {
            HORNDB_NOT_EXACT_MATCH.to_string()
        } else {
            expand_curie(predicate, &curie_map)?
        };
        let pred_node = NamedNode::new(pred_iri)?;
        for s in &subjects {
            for o in &objects {
                let s_node = NamedNode::new(expand_curie(s, &curie_map)?)?;
                let o_node = NamedNode::new(expand_curie(o, &curie_map)?)?;
                ds.insert(&Quad::new(
                    s_node,
                    pred_node.clone(),
                    Term::NamedNode(o_node),
                    GraphName::DefaultGraph,
                ));
            }
        }
    }
    Ok(ds)
}

/// Split a `|`-delimited SSSOM multivalue cell into trimmed non-empty parts.
fn split_multi(cell: &str) -> Vec<String> {
    cell.split('|')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Expand `prefix:local` to a full IRI via the curie_map; pass through
/// values that already look like full IRIs (contain "://").
fn expand_curie(value: &str, curie_map: &HashMap<String, String>) -> Result<String> {
    if value.contains("://") {
        return Ok(value.to_string());
    }
    let (prefix, local) = value
        .split_once(':')
        .ok_or_else(|| anyhow!("not a CURIE or IRI: {value}"))?;
    let base = curie_map
        .get(prefix)
        .ok_or_else(|| anyhow!("unknown CURIE prefix '{prefix}' in {value}"))?;
    Ok(format!("{base}{local}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
#curie_map:
#  A: http://example.org/a/
#  B: http://example.org/b/
#  skos: http://www.w3.org/2004/02/skos/core#
subject_id\tpredicate_id\tobject_id\tpredicate_modifier
A:1\tskos:exactMatch\tB:1\t
A:2\tskos:broadMatch\tB:2|B:3\t
A:9\tskos:exactMatch\tB:9\tNot
";

    #[test]
    fn parses_curie_map_and_rows() {
        let ds = parse_sssom_tsv(SAMPLE).unwrap();
        // 1 + 2 (multivalue object) + 1 negated = 4 triples
        assert_eq!(ds.len(), 4);
    }

    #[test]
    fn expands_curies_to_full_iris() {
        let ds = parse_sssom_tsv(SAMPLE).unwrap();
        let exact = NamedNode::new("http://www.w3.org/2004/02/skos/core#exactMatch").unwrap();
        let a1 = NamedNode::new("http://example.org/a/1").unwrap();
        let b1 = NamedNode::new("http://example.org/b/1").unwrap();
        assert!(ds.contains(&Quad::new(a1, exact, Term::NamedNode(b1), GraphName::DefaultGraph)));
    }

    #[test]
    fn multivalue_object_splits_on_pipe() {
        let ds = parse_sssom_tsv(SAMPLE).unwrap();
        let broad = NamedNode::new("http://www.w3.org/2004/02/skos/core#broadMatch").unwrap();
        let a2 = NamedNode::new("http://example.org/a/2").unwrap();
        for tgt in ["http://example.org/b/2", "http://example.org/b/3"] {
            let o = NamedNode::new(tgt).unwrap();
            assert!(ds.contains(&Quad::new(a2.clone(), broad.clone(), Term::NamedNode(o), GraphName::DefaultGraph)));
        }
    }

    #[test]
    fn negated_row_uses_internal_predicate() {
        let ds = parse_sssom_tsv(SAMPLE).unwrap();
        let not_exact = NamedNode::new(HORNDB_NOT_EXACT_MATCH).unwrap();
        let a9 = NamedNode::new("http://example.org/a/9").unwrap();
        let b9 = NamedNode::new("http://example.org/b/9").unwrap();
        assert!(ds.contains(&Quad::new(a9, not_exact, Term::NamedNode(b9), GraphName::DefaultGraph)));
    }
}
```

> NOTE: confirm the `oxrdf` import paths and `Dataset` API (`::new`, `.insert`, `.len`, `.contains`) against `crates/harness/src/rdf.rs` — that file already constructs `Quad`/`GraphName::DefaultGraph` exactly this way, so mirror its imports. If `anyhow` is not already a harness dep, it is in the workspace deps (used widely in the harness binary) — reference it with `anyhow.workspace = true` if the crate's `Cargo.toml` lacks it.

- [ ] **Step 2: Wire the module** — in `crates/harness/src/lib.rs`, add:

```rust
pub mod sssom_loader;
```

- [ ] **Step 3: Run to verify failure then pass**

Run: `cargo test -p horndb-harness --lib sssom_loader::`
Expected: first FAILs to compile, then all four tests PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/harness/src/sssom_loader.rs crates/harness/src/lib.rs crates/harness/Cargo.toml
git commit -m 'feat(harness): SPEC-11 F9 SSSOM/TSV loader (bench/standalone only)'
```

---

## Task 10: Conformance fixtures + a real SSSOM slice (acceptance #1, #2, #3)

**Files:**
- Create: `crates/harness/tests/fixtures/sssom-mappings/manifest.ttl`
- Create: `crates/harness/tests/fixtures/sssom-mappings/*.ttl` (premise/conclusion pairs)
- Create: `crates/harness/tests/fixtures/sssom-mappings/mondo-slice.sssom.tsv` (small real slice)
- Create: `harness/curation/sssom-mappings.md`

The conformance fixtures are W3C-manifest-style positive-entailment cases (the same `TestKind::PositiveEntailment { premise, conclusion }` shape the `owl2` suite uses — see `crates/harness/src/testcase.rs`). Each premise `.ttl` asserts mapping triples; each conclusion `.ttl` asserts the expected inferred mapping. We add one case per rule family plus the negative and identity-isolation cases, and one case that loads the real `.sssom.tsv` slice.

- [ ] **Step 1: Create the curation doc** — `harness/curation/sssom-mappings.md`:

```markdown
# SSSOM mappings conformance subset (SPEC-11)

The mappings analogue of `owl2-rl-50.md`. Each fixture is one positive
entailment that isolates a SPEC-11 §F3/§F4 rule family, plus an
identity-isolation negative case (ADR-0017) and one end-to-end case loaded
from a real SSSOM/TSV slice via the §F9 harness loader.

| Fixture id | Exercises | SPEC-11 ref |
|---|---|---|
| `#sssom-rg1-exact` | owl:equivalentClass ⟹ skos:exactMatch | F3 RG1 |
| `#sssom-rg2-broad` | rdfs:subClassOf ⟹ skos:broadMatch | F3 RG2 |
| `#sssom-ri-narrow-broad` | narrowMatch ⟹ inverse broadMatch | F3 RI1/2 |
| `#sssom-rce1-broad` | exactMatch ∘ broadMatch ⟹ broadMatch | F3 RCE1 |
| `#sssom-t1-broad` | broadMatch transitivity | F3 T1 |
| `#sssom-neg-exact` | exactMatch ∘ Not ⟹ Not | F4 |
| `#sssom-mondo-slice` | real slice loads + chains | F9, acceptance #1 |

Source of the real slice: <record the Biomappings/Mondo URL, commit, and
licence here>. Keep the slice tiny (≤ a few hundred rows) — it is a
correctness fixture, not a benchmark corpus (benches run on hornbench).
```

- [ ] **Step 2: Create one premise/conclusion pair** (repeat the pattern for each row in the table). Example `rg2-broad-premise.ttl`:

```turtle
@prefix ex: <http://example.org/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
ex:Dog rdfs:subClassOf ex:Mammal .
```

`rg2-broad-conclusion.ttl`:

```turtle
@prefix ex: <http://example.org/> .
@prefix skos: <http://www.w3.org/2004/02/skos/core#> .
ex:Dog skos:broadMatch ex:Mammal .
```

And the negative case `neg-exact-premise.ttl`:

```turtle
@prefix ex: <http://example.org/> .
@prefix skos: <http://www.w3.org/2004/02/skos/core#> .
@prefix h: <https://w3id.org/horndb/internal#> .
ex:A skos:exactMatch ex:B .
ex:B h:notExactMatch ex:C .
```

`neg-exact-conclusion.ttl`:

```turtle
@prefix ex: <http://example.org/> .
@prefix h: <https://w3id.org/horndb/internal#> .
ex:A h:notExactMatch ex:C .
```

- [ ] **Step 3: Create the manifest** — `manifest.ttl`, following the exact vocabulary the existing `owl2/manifest.ttl` uses (open that file and copy its prefix block + `mf:Manifest` / `mf:entries` / `mf:action` / `mf:result` shape verbatim; the harness resolves on-disk filenames via `mf:action`). One entry per fixture, e.g.:

```turtle
# (prefixes copied from crates/harness/tests/fixtures/owl2/manifest.ttl)
:sssom-rg2-broad a mf:PositiveEntailmentTest ;
    mf:name "SSSOM RG2: subClassOf generalises to broadMatch" ;
    mf:action <rg2-broad-premise.ttl> ;
    mf:result <rg2-broad-conclusion.ttl> .
```

- [ ] **Step 4: Create the real slice** — `mondo-slice.sssom.tsv`: download a small Biomappings or Mondo SSSOM/TSV (record provenance in the curation doc), trim to ≤ a few hundred rows that include at least one chainable pair, and check it in. (If network access is unavailable during execution, hand-author a ~20-row representative TSV with a realistic `curie_map` header and note in the curation doc that it is a synthetic stand-in pending a vendored real slice — flag this as a TASKS.md follow-up.)

- [ ] **Step 5: Commit (fixtures only; wiring is Task 11)**

```bash
git add crates/harness/tests/fixtures/sssom-mappings/ harness/curation/sssom-mappings.md
git commit -m 'test(harness): SPEC-11 SSSOM conformance fixtures + curation doc'
```

---

## Task 11: Wire the `sssom-mappings` suite into the harness + `selected.toml` (acceptance #1)

**Files:**
- Modify: `crates/harness/src/testcase.rs` (`Suite` enum)
- Modify: `crates/harness/src/runner.rs` (suite-name → `Suite` dispatch; loader selection)
- Modify: `harness/selected.toml`
- Test: harness run of the new suite

The runner currently maps suite names like `"owl2"` to a `Suite` enum and loads premise/conclusion via `load_turtle_dataset`. We add a `Suite::SssomMappings` key. For the `#sssom-mondo-slice` case the action file ends in `.sssom.tsv`, so the loader must branch on extension: `.sssom.tsv` → `sssom_loader::parse_sssom_tsv`, else `load_turtle_dataset`.

- [ ] **Step 1: Add the `Suite` variant** — in `crates/harness/src/testcase.rs`, add `SssomMappings` to the `Suite` enum (find the enum that already has `Owl2`, `Sparql11`, etc. — the exact name/location is in that file; the agent report places suite keys in `runner.rs`/`testcase.rs`).

- [ ] **Step 2: Add the dispatch arm** — in `crates/harness/src/runner.rs`, in the `match suite_name.as_str()` block, add:

```rust
        "sssom-mappings" => Suite::SssomMappings,
```

- [ ] **Step 3: Branch the dataset loader on extension** — wherever the runner calls `load_turtle_dataset(action_path)` for entailment premise/conclusion, replace with a helper:

```rust
fn load_dataset_for(path: &std::path::Path) -> anyhow::Result<oxrdf::Dataset> {
    if path.to_string_lossy().ends_with(".sssom.tsv") {
        let text = std::fs::read_to_string(path)?;
        Ok(crate::sssom_loader::parse_sssom_tsv(&text)?)
    } else {
        load_turtle_dataset(path)
    }
}
```

and call `load_dataset_for(premise)` / `load_dataset_for(conclusion)` in the `PositiveEntailment` arm. (Conclusions are always `.ttl`, but routing both through the helper is harmless and keeps one path.)

- [ ] **Step 4: Add the suite to `selected.toml`** — append to `harness/selected.toml`:

```toml
[suites.sssom-mappings]
# SPEC-11 SSSOM chaining conformance. Premise/conclusion fixtures isolate each
# rule family (F3 RG/RI/RCE/T1, F4 negative); #sssom-mondo-slice loads a real
# slice via the §F9 harness SSSOM/TSV loader. Curation: harness/curation/sssom-mappings.md
manifest = "crates/harness/tests/fixtures/sssom-mappings/manifest.ttl"
include = [
    "#sssom-rg1-exact",
    "#sssom-rg2-broad",
    "#sssom-ri-narrow-broad",
    "#sssom-rce1-broad",
    "#sssom-t1-broad",
    "#sssom-neg-exact",
    "#sssom-mondo-slice",
]
```

- [ ] **Step 5: Run the harness suite with the real engine**

Run (the full-workspace path, since the harness pulls oxrocksdb-sys):

```bash
cargo nextest run -p horndb-harness
cargo run -p horndb-harness --features real-engine --bin harness -- run --engine owlrl --suite sssom-mappings
```

Expected: all 7 `sssom-mappings` cases report `Passed`. If `#sssom-mondo-slice` fails, inspect with `--engine owlrl` verbose output: the most likely cause is a CURIE in the slice that the `curie_map` header doesn't define (loader returns an error) — fix the slice or its header, not the engine.

> NOTE: confirm the exact harness CLI subcommand/flags against `crates/harness/src/bin/harness.rs` (the agent report shows `--engine` selecting `owlrl` under the `real-engine` feature). If the binary has no per-suite filter flag, run the whole selected set: `cargo run -p horndb-harness --features real-engine --bin harness -- run --engine owlrl` and confirm the `sssom-mappings` rows are green.

- [ ] **Step 6: Commit**

```bash
git add crates/harness/src/testcase.rs crates/harness/src/runner.rs harness/selected.toml
git commit -m 'feat(harness): wire SPEC-11 sssom-mappings conformance suite'
```

---

## Task 12: Documentation sync (CLAUDE.md mandate)

**Files:**
- Modify: `docs/architecture.md` (§13 Status fields)
- Modify: `BENCHMARKS.md` (SPEC-11 NF rows — targets only; numbers come from hornbench later)
- Modify: `TASKS.md` (#130 progress)
- Modify: `docs/index.md` (link the new curation doc if appropriate)

Per the project CLAUDE.md, `docs/architecture.md`, `TASKS.md`, and the SPECs are linked views — update them in the same change set as the code. This task does the doc reconciliation for everything Tasks 1–11 implemented.

- [ ] **Step 1: Flip Status fields in `docs/architecture.md` §13** — change these rows from **planned** to **implemented**: the vocabulary row (F1), the chaining-rules row (F3), the negative-chaining row (F4), the confidence row (F7), the provenance row (F8), and the harness-loader row (F9). Leave the mapping-representation row (F2) as **partial** (n-ary node builder exists; full materialization on inference is follow-up), and leave the compact-index (F5) and crosswalk-spine (F6) rows **planned** (separate plan). Update the "Overall status" line to **partial / in progress**.

- [ ] **Step 2: Add the SPEC-11 section to `BENCHMARKS.md`** — add a `### SPEC-11 — SSSOM mappings & crosswalk index` subsection with target rows (NF1 throughput TBD, NF2 ≤10 B/pair, NF3 full-closure vs OxO2 1.16M/17min baseline) marked "Measured: pending hornbench (F5/F6 follow-up)". Do **not** invent measured numbers — those are produced only on hornbench per the project rule.

- [ ] **Step 3: Update `TASKS.md` #130** — annotate the task body to record that the reasoning slice (F1–F4, F7, F8, F9 + conformance) is implemented and the serving slice (F5/F6, and GraphBLAS-backend T1 parity) remains. Do not check off the top-level box (the task isn't fully done until the index/spine land). Follow the claim/checkoff procedure in the `TASKS.md` header and mirror the note to GitHub issue #130.

- [ ] **Step 4: Update `docs/index.md`** if it indexes curation docs — add a one-line pointer to `harness/curation/sssom-mappings.md`.

- [ ] **Step 5: Run the full verification gate**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run -p horndb-owlrl
cargo nextest run -p horndb-harness
```
Expected: clean fmt, zero clippy warnings, all owlrl + harness tests green.

- [ ] **Step 6: Commit**

```bash
git add docs/architecture.md BENCHMARKS.md TASKS.md docs/index.md
git commit -m 'docs(spec): SPEC-11 reasoning slice — sync architecture/benchmarks/tasks'
```

---

## Self-review notes (for the executor)

- **Acceptance coverage (this slice):** #1 conformance subset green (Tasks 10–11); #2 chaining T1/RCE/RI/RG with justifications (Tasks 2–5, 8); #3 negative chaining xanthene case (Task 6); #4 identity isolation (Task 7); #8 provenance/`derived_from` (Task 8 + existing proof machinery). #5/#6/#7 (index correctness, size/throughput, crosswalk-in-every-query) belong to the **serving-slice follow-up plan** and are intentionally not covered here.
- **Type consistency:** vocab field names are snake_case mirrors of the QName (`skos_exact_match` ↔ `` `skos:exactMatch` ``); rule ids use the `sssom-<family>` prefix that `rule_justification` (Task 8) pattern-matches on — if you rename a rule id, update `rule_justification`. The internal negated IRI string appears in **two** places (`integration.rs::HORNDB_NOT_EXACT_MATCH` and `sssom_loader.rs::HORNDB_NOT_EXACT_MATCH`) — keep them byte-identical.
- **Path verification:** every `use horndb_owlrl::{engine, store, types, vocab, backend}::…` and `oxrdf::…` import is marked with a NOTE to confirm against `lib.rs`/`rdf.rs` because the exact re-export path is the one detail the exploration could not pin to a line. Confirm before assuming a compile error is a logic error.
- **No new dependencies** are introduced (the loader uses `oxrdf` + `anyhow`, both already in the harness). The compact-index crates (Elias-Fano/FoR) are deliberately *not* added here — they belong to the serving slice.
