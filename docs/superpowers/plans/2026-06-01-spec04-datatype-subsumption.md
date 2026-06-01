# Plan — SPEC-04 datatype subsumption (dt-type1/dt-type2) + scm-eqc-rev (issue #34)

First shippable increment of the SPEC-04 rule-completeness epic (#4). Clears the
three tractable cases in the "datatype subsumption" red bucket of
`harness/KNOWN-MANIFEST-BUGS.md`.

## Background (validated by baseline probe)

The 5 W3C cases filed under "datatype subsumption" split three ways:

- **WebOnt-I5.8-011-pe** — empty premise ⇒ `xsd:integer`/`xsd:string a
  rdfs:Datatype`. Needs **dt-type1** (datatype declarations).
- **WebOnt-I5.8-006-pe** — `p rdfs:range xsd:byte` ⇒ `p rdfs:range xsd:short`.
  Needs **dt-type2** as the XSD subsumption lattice (`xsd:byte rdfs:subClassOf
  xsd:short`) + the already-implemented **`scm-rng1`** (range propagates along
  subClassOf of the range class — confirmed in `rules.toml`; note: `scm-rng1`,
  NOT `scm-rng2` which is the subPropertyOf variant).
- **WebOnt-equivalentClass-003-pe** — pure two-way `rdfs:subClassOf`
  (`Car`/`Automobile`, NO datatype — KNOWN-MANIFEST-BUGS mis-files it under
  datatype) ⇒ `owl:equivalentClass`. Needs **`scm-eqc-rev`**, the class analogue
  of the existing `scm-eqp-rev`. Baseline probe confirmed this is currently
  `false`.

Deferred (NOT in this increment): WebOnt-I5.8-008/009 (datatype value-space
**intersection** narrowing — genuine interval reasoning, not subsumption); and
the literal-value rules dt-eq/dt-diff/dt-not-type (issue #40).

Architecture facts:
- `crates/owlrl/src/store.rs::TripleStore` is purely `TermId`-based. Literals are
  interned as opaque string keys in `integration.rs`. dt-type1/dt-type2 reason
  over **datatype IRIs as ordinary terms**, NOT literal values, so they need NO
  literal introspection.
- Precedent for load-time axiom injection:
  `integration.rs::infer_owl_thing_from_named_individuals` asserts derived base
  triples before `reset_and_materialize`. The datatype lattice follows the same
  pattern.
- `Engine` (`crates/owlrl/src/integration.rs`) owns the `String → TermId` dict.
  Datatype injection must happen there (the `MemStore`/`materialize` layer has no
  dictionary). `scm-eqc-rev` is a pure codegen rule and needs no dict.

## Task 1 — `scm-eqc-rev` codegen rule

**Files:** `crates/owlrl/rules.toml`, `crates/owlrl/tests/single_rule.rs`.

TDD:
1. Add a failing test `scm_eqc_rev` to `tests/single_rule.rs` (MemStore +
   synthetic vocab, mirrors the existing tests there): assert
   `c1 rdfs:subClassOf c2` and `c2 rdfs:subClassOf c1`; after `materialize`,
   expect `c1 owl:equivalentClass c2` AND `c2 owl:equivalentClass c1`.
2. Add the rule to `rules.toml` immediately after `scm-eqp-rev`, mirroring it:
   ```toml
   [[rule]]
   id = "scm-eqc-rev"
   comment = "Two-way rdfs:subClassOf implies owl:equivalentClass (class analogue of scm-eqp-rev; W3C WebOnt-equivalentClass-003-pe)."
   body = [
     { s = "?c1", p = "rdfs:subClassOf", o = "?c2" },
     { s = "?c2", p = "rdfs:subClassOf", o = "?c1" },
   ]
   head = { s = "?c1", p = "owl:equivalentClass", o = "?c2" }
   ```
3. `cargo build -p horndb-owlrl` (regenerates), then
   `cargo run -p horndb-owlrl --bin show-rule -- scm-eqc-rev` to confirm the
   generated `fire_scm_eqc_rev` matches intent.
4. `cargo test -p horndb-owlrl` green.

Acceptance: new test passes; no existing owlrl test regresses.

## Task 2 — datatype lattice module + load-time injection

**Files:** new `crates/owlrl/src/datatypes.rs`; `crates/owlrl/src/lib.rs` (add
`pub mod datatypes;`); `crates/owlrl/src/integration.rs`; new
`crates/owlrl/tests/datatype_subsumption.rs`.

Design `crates/owlrl/src/datatypes.rs`:
- `pub const RDFS_DATATYPE: &str = "http://www.w3.org/2000/01/rdf-schema#Datatype";`
- A faithful OWL 2 RL datatype set as `&str` IRI constants / a `pub const
  XSD_DATATYPES: &[&str]` for dt-type1. Include at minimum: `xsd:string`,
  `xsd:boolean`, `xsd:decimal`, `xsd:integer`, `xsd:dateTime`, and the full
  integer tower used by the lattice below. (`xsd:` =
  `http://www.w3.org/2001/XMLSchema#`.)
- `pub const XSD_SUBCLASS_EDGES: &[(&str, &str)]` — directed `(sub, super)`
  `rdfs:subClassOf` edges of the XSD datatype hierarchy:
  ```
  integer ⊑ decimal
  long ⊑ integer,  int ⊑ long,  short ⊑ int,  byte ⊑ short
  nonNegativeInteger ⊑ integer
  positiveInteger ⊑ nonNegativeInteger
  unsignedLong ⊑ nonNegativeInteger
  unsignedInt ⊑ unsignedLong,  unsignedShort ⊑ unsignedInt,  unsignedByte ⊑ unsignedShort
  nonPositiveInteger ⊑ integer,  negativeInteger ⊑ nonPositiveInteger
  ```
  (Do NOT add `unsigned* ⊑ short/int/...` cross-edges — the unsigned tower is a
  separate branch under nonNegativeInteger. No intersection edges.)
- `pub fn inject(store, vocab, intern)` where `intern: &mut dyn FnMut(&str) ->
  TermId` resolves an IRI to its `TermId` via the caller's dict. It asserts, as
  **base** triples (`store.assert`, like the owl:Thing helper):
  - dt-type1: `D rdf:type rdfs:Datatype` for each `D` in the datatype set (use
    `vocab.rdf_type` and the interned `RDFS_DATATYPE`).
  - dt-type2: `sub rdfs:subClassOf super` for each edge (use
    `vocab.rdfs_sub_class_of`).
  Keep the function signature simple and unit-testable; the store and vocab types
  are `crate::store::MemStore` / `crate::vocab::Vocabulary` (or `&mut dyn
  TripleStore` — choose whichever keeps it testable without the dict; a thin
  wrapper in integration.rs supplies `intern`).

Wire into `integration.rs::load()`: after the existing
`infer_owl_thing_from_named_individuals(...)` call and before
`reset_and_materialize`, call the datatype injection, interning each IRI through
`state.dict` (reuse the existing `intern_named`-style logic; the dict + next_id
live in `state`). Injection is **unconditional** — required because I5.8-011 has
an empty premise yet expects the datatype declarations.

TDD (`crates/owlrl/tests/datatype_subsumption.rs`, using the public
`horndb_owlrl::integration::Engine`):
1. **dt-type1**: empty premise (`Dataset::new()`); after `load`, `entails`
   `xsd:integer rdf:type rdfs:Datatype` and `xsd:string rdf:type rdfs:Datatype`
   → true. (Mirrors I5.8-011.)
2. **dt-type2 + scm-rng1**: premise `p rdf:type owl:DatatypeProperty`,
   `p rdfs:range xsd:byte`; after `load`, `entails` `p rdfs:range xsd:short`
   → true. (Mirrors I5.8-006.)
3. **transitivity**: same premise also entails `p rdfs:range xsd:int` and
   `p rdfs:range xsd:integer` (lattice closed transitively by scm-sco).
4. **negative guard**: `p rdfs:range xsd:byte` does NOT entail `p rdfs:range
   xsd:string` (unrelated branch) → false.

Acceptance: all four tests pass. `cargo test -p horndb-owlrl` green; no
regression.

## Task 3 — harness selection + docs + bookkeeping

**Files:** `harness/selected.toml`, `harness/KNOWN-MANIFEST-BUGS.md`,
`docs/architecture.md`, `TASKS.md`.

1. Add to the `[suites.owl2-w3c-rl]` `include = [...]` list (keep it sorted as
   the file is): `"#WebOnt-I5.8-006-pe"`, `"#WebOnt-I5.8-011-pe"`,
   `"#WebOnt-equivalentClass-003-pe"`.
2. **Verify the conformance gate (harness-first):** build the harness with the
   real engine and run the owl2-w3c-rl suite; confirm the three new `-pe` cases
   are green and nothing regressed:
   ```
   CARGO_TARGET_DIR=/Users/stig/git/sunstone/horndb/target \
     cargo run -p horndb-harness --bin harness --features real-engine -- \
     --engine owlrl run
   ```
   (Fixtures are checked in under `crates/harness/tests/fixtures/owl2-w3c-rl/`;
   no fetch needed. If the full `run` is impractical, run the owl2-w3c-rl suite
   specifically / the `crates/owlrl` + harness tests that exercise the subset and
   record the real output.) Do not mark done on red.
3. `KNOWN-MANIFEST-BUGS.md`: remove `#WebOnt-I5.8-006-pe`, `#WebOnt-I5.8-011-pe`,
   `#WebOnt-equivalentClass-003-pe` from the red list; correct the
   equivalentClass-003 entry (it is not a datatype case — it needed
   `scm-eqc-rev`); re-document I5.8-008/009 accurately as datatype value-space
   **intersection** reasoning (still deferred, issue #4); update the bucket
   counts.
4. `docs/architecture.md`: flip the SPEC-04 datatype-rules Status note to reflect
   that dt-type1/dt-type2 subsumption + scm-eqc-rev are implemented (intersection
   + literal-value rules still deferred). Find the SPEC-04 row/section first.
5. `TASKS.md`: in the #4 epic-breakdown note, mark increment #34 delivered
   (datatype subsumption + scm-eqc-rev); the parent task stays `[v]` (increments
   #35–#40 remain). Do NOT flip the parent to `[x]`.

Acceptance: harness owl2-w3c-rl suite green incl. the 3 new cases; docs consistent.

## Out of scope / do not do
- No literal value parsing/introspection (issue #40).
- No I5.8-008/009 intersection reasoning.
- No changes to the `TripleStore` trait surface.
- Do not flip parent task #4 to done; do not close any issue (merge-gated).
