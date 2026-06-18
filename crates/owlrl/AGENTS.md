# `horndb-owlrl` — Agent / Contributor Guide

> This document is the canonical description of the **what** and **why** of
> `horndb-owlrl`. It is written so that a future agent (human or model)
> could rewrite the codegen pipeline in a different language — say, port
> it to TypeScript or generate C++ instead of Rust — using only this
> file plus the W3C OWL 2 RL spec and the SPEC-04 design doc.
>
> `crates/owlrl/CLAUDE.md` is a symlink to this file: keep them in sync
> by editing only `AGENTS.md`.

---

## 1. Mission

`horndb-owlrl` is the **OWL 2 RL forward-chaining rule engine** of HornDB
(SPEC-04). Given an RDF graph, it derives every triple that the OWL 2 RL
profile entails from the asserted base, then exposes the closure plus
per-triple proofs to higher layers.

Two design commitments shape every decision below:

1. **The rule set is fixed at compile time.** OWL 2 RL is a closed
   W3C-standardized rule list. We compile each rule to a dedicated Rust
   function (`fire_<id>`) at build time and dispatch them through a
   static `RULES: &[CompiledRule]` table. There is **no rule
   interpreter** in this crate. The motivation is performance (no
   indirection in the hot loop, predicate-specialized iteration) and
   transparency (the generated source is human-readable Rust that can be
   audited).

2. **The crate is harness-graded against the W3C OWL 2 RL conformance
   suite.** Correctness is gated by the curated 50-case subset in
   `harness/curation/owl2-rl-50.md`. Any change that makes a previously
   passing case fail is rejected — see SPEC-00.

This document is structured as: §2 the compile-time pipeline (input
→ generated Rust), §3 the runtime pipeline (generated Rust + driver),
§4 design rationale for the non-obvious choices, §5 the tactical
add-a-rule / add-a-vocab-term workflows, §6 inspection tooling, §7
known caveats and Stage-2 deferrals.

---

## 2. Compile-time pipeline

```
                          rules.toml                src/vocab.rs
                              │                         │
                              ▼                         ▼
                  ┌─────────────────────┐   ┌──────────────────────┐
                  │ codegen/parse.rs    │◄──┤ codegen/vocab.rs     │
                  │  Document → RuleSpec│   │ extract_qname_map    │
                  └─────────┬───────────┘   │  (syn-walk vocab.rs) │
                            │               └──────────────────────┘
                            ▼
                  ┌──────────────────┐
                  │ codegen/plan.rs  │   classify each body slot:
                  │ plan_rule()      │     Bound | Fresh | SameAsEarlierSlot
                  └─────────┬────────┘
                            ▼
                  ┌──────────────────┐
                  │ codegen/emit.rs  │   emit_all() → proc_macro2 TokenStream
                  │ emit_all()       │
                  └─────────┬────────┘
                            ▼
                  ┌──────────────────┐
                  │ build.rs         │   prettyplease::unparse → write
                  │ orchestrator     │   $OUT_DIR/generated_rules.rs
                  └──────────────────┘
                            │
                            ▼
              src/lib.rs `pub mod generated { include!(…) }`
```

### 2.1 `rules.toml` — the rule source

A flat list of `[[rule]]` blocks. The schema (one rule):

| Field      | Type      | Required? | Meaning                                                                                                                                       |
|------------|-----------|-----------|-----------------------------------------------------------------------------------------------------------------------------------------------|
| `id`       | string    | yes       | W3C rule ID (e.g. `"cax-sco"`). Used as both the table key and the generated function suffix (with `-`/`:` → `_`). MUST be unique.            |
| `comment`  | string    | no        | Human description; usually cites the W3C OWL 2 Profiles table.                                                                                |
| `delegate` | string    | no        | If `"closure"`, this rule is **not compiled** — it is routed to a `ClosureBackend` at runtime. The body/head are still listed for documentation and to keep the table complete. |
| `body`     | array     | yes       | List of triple patterns (`{ s, p, o }`). Empty list `[]` means the rule has no premises (a tautology generator like `scm-cls`).               |
| `head`     | table     | yes       | Single triple pattern.                                                                                                                        |

A pattern's `s`/`p`/`o` is either:
- A **variable**: a string starting with `?` (e.g. `"?x"`, `"?c1"`).
  Same name across patterns = same value.
- A **vocab token**: a QName like `"rdf:type"` or `"owl:sameAs"`. The
  set of valid QNames is the union of `///` doc comments on
  `src/vocab.rs:struct Vocabulary` — see §2.3.

The rule semantics are:
> for every binding of body variables that satisfies all body patterns
> in the current store, derive the corresponding `head` triple.

### 2.2 `src/vocab.rs` — the vocabulary registry

A plain `struct Vocabulary { … }` with one field per RDF/RDFS/OWL term
the rule set needs. Each field carries:

- A snake_case Rust name (e.g. `rdfs_sub_class_of`).
- A `TermId` value (the dictionary ID, populated at runtime by the
  caller — typically the SPEC-02 storage layer).
- A `///` doc comment whose text contains the canonical QName in
  backticks: `` /// `rdfs:subClassOf` ``.

The doc comment is **load-bearing**: `build.rs` reads it to build the
QName → field-name map (§2.3). The rule parser uses that map to
resolve every QName in `rules.toml`.

The struct also has a `synthetic(base)` constructor used by tests to
allocate consecutive `TermId`s starting at `base`.

### 2.3 `codegen/vocab.rs` — QName map extraction

Function `extract_qname_map(vocab_path) -> HashMap<String, String>`:

1. Read `src/vocab.rs` as text.
2. Parse it with `syn::parse_file`.
3. Find `struct Vocabulary`. For each field, walk attributes for
   `#[doc = "..."]`, extract a `prefix:name` token from the text (the
   recommended form is `` `prefix:name` ``).
4. Return `{qname → field_name}`. Errors loudly if any field lacks its
   QName doc, if any QName is duplicated, or if `Vocabulary` is absent.

This is the **single edit point** when adding a vocabulary term — the
rule parser inherits the new entry automatically.

### 2.4 `codegen/parse.rs` — `rules.toml` → `Vec<RuleSpec>`

`parse_file(rules_path, vocab_path)` is the only entry point
`build.rs` needs. It:

1. Reads `rules.toml` with `toml::from_str` into a `Document` (serde).
2. Calls `vocab::extract_qname_map(vocab_path)` for QName resolution.
3. For each raw rule, resolves slots:
   - A leading `?` → `Slot::Var(name)`.
   - Otherwise → `Slot::Vocab(VocabTerm { field })` where `field` is
     the `Vocabulary` field name from the QName map. Unknown QNames
     produce an actionable error message pointing at `src/vocab.rs`.
4. Returns `Vec<RuleSpec>` with body patterns, head pattern, and the
   `delegate` flag.

The QName-resolved field name is leaked as `&'static str` because
downstream codegen (proc-macro2) wants static literals. This leak is
build-time only and bounded by the vocabulary size; `build.rs` exits
long before it matters.

### 2.5 `codegen/plan.rs` — body iteration plan

A trivial nested-loop planner. Stage-1 plan shape is *"iterate the
leading pattern, probe the rest"*; SPEC-03 (WCOJ) will replace this
with leapfrog triejoin in Stage 2.

For each body pattern, the planner classifies each slot (s, p, o) as
one of:

- `Bound(BoundSource::Vocab(field))` — the slot must equal a vocab
  term. Emitted as `Some(v.<field>)` in probes.
- `Bound(BoundSource::Var(name))` — a previously-introduced variable.
  Emitted as `Some(<name>)`.
- `Fresh(name)` — the slot introduces a new variable. Emitted as
  `None` in the probe; the variable is then bound from the iterated
  triple.
- `SameAsEarlierSlot { earlier, … }` — same variable appearing twice
  *within the same pattern* (e.g. `?x ?p ?x` in `prp-irp`). Emitted as
  `None` in the probe plus a post-probe equality filter against the
  earlier slot.

The leading pattern (depth 0) is iterated via
`store.scan_predicate(v.<pred>)`. **Stage-1 invariant**: the leading
pattern's predicate must be a vocab term, not a variable. Any
variable-predicate leading pattern would require scanning every
predicate partition — `eq-rep-s`/`eq-rep-o` push the variable
predicate to a *later* pattern where the special
`probe_any_predicate` path handles it. `prp-trp` is the only rule
that genuinely needs leading variable-predicate iteration; it is
`delegate = "closure"` and never reaches the planner.

### 2.6 `codegen/emit.rs` — Rust source generation

`emit_all(rules)` produces a `proc_macro2::TokenStream` containing:

- `pub type FireFn` — alias for `fn(&dyn TripleStore, &Delta) -> Delta`.
- `pub type PredAccessor` — alias for `fn(&Vocabulary) -> TermId`.
- `pub struct CompiledRule { id, delegated, fire, body_predicates, wildcard_predicate }`.
- `pub const RULE_COUNT: usize`, `pub const RULE_IDS: &[&str]`.
- One `pub fn fire_<sanitized_id>(...)` per rule.
- `pub const RULES: &[CompiledRule]` linking everything together.

For a non-delegated rule the function body is a nested-loop expansion
of the plan:

```rust
// fire_cax_sco — illustrative
pub fn fire_cax_sco(store: &dyn TripleStore, _delta: &Delta) -> Delta {
    let v = store.vocab();
    let mut out = Delta::new();
    for __t0 in store.scan_predicate(v.rdfs_sub_class_of) {
        let c1 = __t0.s; let c2 = __t0.o;
        for __t1 in store.probe(None, v.rdf_type, Some(c1)) {
            let x = __t1.s;
            let head = Triple::new(x, v.rdf_type, c2);
            if !store.contains(&head) && !out.contains(&head) {
                out.insert(head, Provenance { rule_id: "cax-sco", premises: … });
            }
        }
    }
    out
}
```

For a delegated rule the function body returns `Delta::new()` — the
runtime calls a `ClosureBackend` instead.

**Provenance**: every emitted `out.insert` records a `Provenance {
rule_id, premises }` where `premises: SmallVec<[Triple; 4]>` lists
the body triples that fired the rule. The premise capacity 4 is
chosen because no Stage-1 compiled rule has more than 4 body
patterns.

**`wildcard_predicate` flag**: set to `true` iff any body pattern's
predicate slot is a variable. The engine uses this to disable the
dirty-predicate prune for such rules (§3.4) — without it, semi-naïve
evaluation under-fires when only the predicate changed.

### 2.7 `build.rs` — orchestrator

1. Emit `cargo:rerun-if-changed=…` for every input.
2. Call `codegen::parse::parse_file(rules.toml, src/vocab.rs)`.
3. Call `codegen::emit::emit_all(...)` → TokenStream.
4. Parse the TokenStream back into `syn::File` (sanity check) and
   pretty-print with `prettyplease`.
5. Write `$OUT_DIR/generated_rules.rs`.

If any step fails, `build.rs` prints the cause to stderr and exits
non-zero — the build halts. Errors point at the file the contributor
needs to edit.

### 2.8 Output: `generated_rules.rs`

Lives at `$OUT_DIR/generated_rules.rs`. Brought into the library by:

```rust
// src/lib.rs
pub mod generated {
    include!(concat!(env!("OUT_DIR"), "/generated_rules.rs"));
}

pub const COMPILED_RULES_SOURCE: &str =
    include_str!(concat!(env!("OUT_DIR"), "/generated_rules.rs"));
```

`generated::RULES` is the runtime dispatch table; `COMPILED_RULES_SOURCE`
is the same file as a string, used by the `show-rule` dev binary (§6).

---

## 3. Runtime pipeline

### 3.1 `Vocabulary` and `TermId`

`TermId(u64)` is a dictionary-encoded RDF term ID, opaque to this
crate. The SPEC-02 storage layer is the authoritative dictionary; in
tests, `Vocabulary::synthetic(base)` allocates consecutive IDs.

The caller populates a `Vocabulary` struct and passes it into the
store. The generated rule code does `let v = store.vocab()` once on
entry and reads vocab fields by name.

### 3.2 `TripleStore` trait (`src/store.rs`)

The contract the generated rule code reads against. Required methods:

- `vocab(&self) -> &Vocabulary`
- `contains(&self, t: &Triple) -> bool`
- `scan_predicate(&self, p: TermId) -> TripleIter<'_>`
- `probe(&self, s: Option<TermId>, p: TermId, o: Option<TermId>) -> TripleIter<'_>`
- `probe_any_predicate(&self, s: Option<TermId>, o: Option<TermId>) -> TripleIter<'_>` — for variable-predicate probes (`eq-rep-s`/`eq-rep-o`).
- `insert_inferred(&mut self, t: Triple, prov: Provenance) -> bool`
- `clear_inferred(&mut self)`
- `all_triples(&self) -> FxHashSet<Triple>`

The Stage-1 `MemStore` impl is a `HashMap<TermId, HashSet<(s,o)>>`
keyed by predicate. SPEC-02 will ship a production tiered/columnar
backend behind the same trait.

### 3.3 `Delta` (`src/delta.rs`)

A per-round set of derived triples + their proofs. Each rule's `fire`
function returns a `Delta`; the engine merges all per-rule deltas and
attempts to apply them to the store. Fresh deltas (those producing
genuinely new triples) drive the next round; an empty applied delta
terminates the loop.

### 3.4 The semi-naïve driver (`src/engine.rs`)

`materialize(store, backend)` runs rounds until fixpoint:

1. For each rule in `RULES`:
   - Skip if `delegated` (the closure backend handles it).
   - Skip if `rule_relevant(rule, dirty_set) == false`.
   - Call `rule.fire(store, &Delta::new())`, merge into round delta.
2. Call `backend.close(store)` once per round, merge into round delta.
3. Apply the round delta to the store via `insert_inferred`. Track
   the *applied* (genuinely new) triples.
4. Compute `dirty_predicates` from the applied delta; loop until
   applied is empty.

**Dirty-predicate prune** (the "semi-naïve" trick): A rule is
relevant on subsequent rounds iff at least one of its body predicates
appears in the dirty set. Exception: if the rule has
`wildcard_predicate = true`, it must always re-fire while the dirty
set is non-empty, because its body can be re-satisfied by any new
predicate.

**Reset semantics**: `reset_and_materialize` calls
`store.clear_inferred()` first — drops all derived triples, keeps
asserted ones, then re-runs `materialize`. This is the public path
for re-deriving (SPEC-04 F7).

### 3.5 `ClosureBackend` trait (`src/backend.rs`)

A subset of rules in OWL 2 RL are transitive-closure-shaped:
`eq-sym`, `eq-trans`, `eq-ref`, `prp-trp`, `scm-sco`, `scm-spo`.
These are *much* faster computed with a sparse matrix algorithm than
with rule firing. In production, `horndb-closure` (SPEC-05) implements
this trait against SuiteSparse:GraphBLAS. In tests and Stage-1 smoke
runs, the in-crate `RuleFiringBackend` runs the same closure as
ordinary nested loops — slow but obviously correct.

Rules with `delegate = "closure"` in `rules.toml` are listed in
`RULES` with `delegated: true` and a no-op `fire` function. The
engine routes them to the backend via `backend.close(store)`.

### 3.6 `Provenance` and the engine façade

Every derived triple carries a `Provenance { rule_id: &'static str,
premises: SmallVec<[Triple; 4]> }`. The premises are the body triples
that fired the derivation. The provenance is consumed by SPEC-08
(ML/LLM boundary) — derivations from ML-admitted facts are tagged
`MlDerived`, symbolic ones are `Symbolic`. See
`INTEGRATION-NOTES.md` and `provenance.rs`.

Provenance composes into a **proof tree** (SPEC-04 F4): `MemStore::proof_tree`
recursively expands each derived triple's premises down to asserted base
triples (cutting derivation cycles), and `Engine::proof(s, p, o)` returns the
same tree decoded to lexical IRIs as a `StringProofTree`. See §7 for the two
intentional elisions (GraphBLAS-closure empty premises; restriction-rule
schema side conditions).

`integration.rs` exposes an `Engine` façade that owns a
`MemStore`, a dictionary (`String → TermId`), and a vocabulary, and
exposes `load(&Dataset)`, `entails(...)`, `is_consistent()`, etc.
It is the entry point the harness uses.

---

## 4. Design rationale (the WHYs)

These are the non-obvious choices a re-implementer needs to preserve.

### 4.1 Why compile-time codegen, not interpretation

A Soufflé-style interpreter would loop over an array of `Rule` structs
at runtime, dispatching pattern-matching dynamically. We compile each
rule to a hand-shaped function because:

- The hot inner loop becomes a fixed sequence of `scan_predicate` /
  `probe` calls — the CPU can prefetch and inline.
- The `rdf:type` field accesses are direct field reads (`v.rdf_type`),
  not table lookups.
- The generated code is *readable Rust*. A contributor or auditor can
  open `target/.../generated_rules.rs` (or run `show-rule <id>`) and
  see exactly what fires. There is no "what would the interpreter
  do" indirection.

The cost is build-time complexity (this crate has a 4-file codegen
pipeline) and the inability to load new rules at runtime — which is
fine because OWL 2 RL is a closed, W3C-standardized rule list.

### 4.2 Why TOML for `rules.toml`

The rules look RDF-shaped (triple patterns), which suggests Turtle /
N3 / SWRL / SPIN. We use TOML anyway because:

- Per-rule metadata (`id`, `comment`, `delegate = "closure"`) has no
  natural RDF encoding without named graphs or reification.
- The QName tokens (`rdf:type`, `owl:sameAs`) are still recognizable
  to RDF people; variables (`?x`) are SPARQL-style.
- TOML parse errors have clean line numbers and are friendly.
- Adding `toml` as a build-dep is far cheaper than an N3 parser.
- The W3C → field-name mapping (`owl:Class` → `owl_class`) has to
  exist somewhere regardless of input syntax — using Turtle would
  not eliminate it, just move it.

If the rule set ever grows past a few hundred entries or needs to be
imported from external SWRL/SPIN ontologies, revisit this — but for
the ~50-rule Stage-1 set, TOML is the right tool.

### 4.3 Why auto-derive the QName map (not hand-maintain it)

Previously, adding a vocab term required edits to three files:
`rules.toml` (use it), `src/vocab.rs` (declare it), AND
`codegen/parse.rs` (a hardcoded `match` mapping the QName to the
field name). The third edit was easy to forget, and the build error
("unknown vocabulary token") didn't point at the missing entry.

Driving the map from a single source — the `///` doc comment on each
`Vocabulary` field — collapses the workflow to a one-file edit.
Contributors edit `vocab.rs` (or just `rules.toml` if the term
already exists); the rules parser picks it up via `build.rs`.

The doc-comment form (`` /// `rdf:type` ``) was chosen over
algorithmic derivation (e.g. snake_case → camelCase) because the
class-vs-property casing is ambiguous (`owl:Class` is PascalCase,
`owl:sameAs` is camelCase) and an explicit doc is grep-friendly and
IDE-friendly.

### 4.4 Why semi-naïve with a *predicate-level* dirty set

A naïve evaluator re-fires every rule on every round until fixpoint.
A standard semi-naïve evaluator re-fires only rules that "could"
produce new derivations — typically gated on a per-relation dirty set.

Our dirty set is at the **predicate level**, not the per-relation
level, because everything in this crate lives in one big partitioned
"by-predicate" map (`store::MemStore::by_pred`). A rule is relevant
iff one of its body-pattern predicates is in the dirty set from the
previous round.

The exception is rules with a variable predicate (`eq-rep-s/p/o`):
they read *every* predicate partition, so any dirty predicate could
expose a new match. The `wildcard_predicate` flag in `CompiledRule`
handles this. **This invariant is easy to violate** — see commit
`e0ca19a` for a fix where a wildcard-predicate rule was being
incorrectly skipped by the prune.

### 4.5 Why the `ClosureBackend` is a separate trait

Transitive-closure-shaped rules (`scm-sco`, `eq-trans`, etc.) are
asymptotically faster computed with a sparse matrix multiplication
algorithm (SuiteSparse:GraphBLAS) than with nested-loop rule firing.
On a graph with `n` nodes and a chain of length `k`, naïve closure is
O(n·k); GraphBLAS hits O(n + edges).

Rather than special-case those rules in the engine, we lift them into
a trait (`ClosureBackend::close(store) -> Delta`) and let SPEC-05
implement the fast version. Tests and Stage-1 smoke runs use
`RuleFiringBackend`, which runs the same closure as ordinary rule
firing — fully self-contained, slow, obviously correct.

This separation is also why the delegated rules still appear in
`rules.toml`: keeping them listed makes the rule table
self-documenting, even though they have a no-op compiled body.

### 4.6 Why insertion-only at Stage 1

Real OWL retraction requires either (a) re-materializing from
scratch (cheap, used today via `reset_and_materialize`) or (b)
Z-set / DBSP-style incremental deletion (SPEC-06, Stage 2). We do
*not* attempt incremental deletion in this crate. `insert_inferred`
is the only state-mutation path; clearing happens only via
`clear_inferred`, which drops *all* derived triples wholesale.

### 4.7 Why `Box::leak` the field-name strings in `parse.rs`

The codegen emits literal `&'static str`s like `v.rdfs_sub_class_of`
into generated code. To stitch the field name into a
`proc_macro2::TokenStream` via `format_ident!`, the string needs a
`'static` lifetime. The QName map is built at runtime (build time),
so we `Box::leak` each field-name string. The leak is bounded by the
vocabulary size (~40 entries) and `build.rs` exits long before any
of that matters. Not a memory leak that affects the final binary.

---

## 5. Tactical workflows

### 5.1 Adding a new rule

1. Open `crates/owlrl/rules.toml`. Find a structurally similar
   existing `[[rule]]` block (same arity, same `delegate` status).
   Copy it.
2. Edit `id`, `comment`, `body`, `head`. Cite the W3C source in
   `comment` (e.g. "Table 7 row cax-eqc1").
3. If the new rule references a vocab token (`prefix:name`) that
   isn't already in `src/vocab.rs`, follow §5.2 first.
4. Add a regression test under `crates/owlrl/tests/single_rule.rs`
   (or a new `tests/rule_<id>.rs`). The shape is:
   ```rust
   let v = Vocabulary::synthetic(1000);
   let mut store = MemStore::new(v);
   store.assert(/* triples from antecedent */);
   materialize(&mut store, &mut RuleFiringBackend::new());
   assert!(store.contains(&expected_head_triple));
   ```
5. `cargo build -p horndb-owlrl` — regenerates `generated_rules.rs`.
6. `cargo run -p horndb-owlrl --bin show-rule -- <id>` — inspect the
   generated Rust to confirm it matches your intent (§6).
7. `cargo test -p horndb-owlrl`.

### 5.2 Adding a new vocabulary term

1. Open `crates/owlrl/src/vocab.rs`.
2. Add a field to `struct Vocabulary` with a `///` QName doc:
   ```rust
   /// `owl:newTerm`
   pub owl_new_term: TermId,
   ```
3. Add a matching line to `Vocabulary::synthetic()`:
   ```rust
   owl_new_term: next(),
   ```
4. If your code reads `Engine` (`integration.rs`), you'll also need
   to seed the IRI → TermId mapping there (look for the
   `const OWL_… : &str = "http://…"` block and the `build_vocab`
   helper). This is a third file only if you go through the
   `Engine` façade — pure rule additions do not require it.
5. **Do not edit `codegen/parse.rs`.** The QName map is
   auto-derived; the parser picks up your new term on the next
   build.

### 5.3 Adding a delegated (closure) rule

Same as §5.1 but set `delegate = "closure"` on the rule. The rule
appears in `RULES` with a no-op `fire` and `delegated: true`. You
**also** need a real implementation in `horndb-closure` (SPEC-05) —
without one, the rule does nothing at runtime. The Stage-1
`RuleFiringBackend` in `backend.rs` hard-codes the small set of
delegated rules it knows about (`scm-sco`, `scm-spo`, `eq-sym`,
`eq-trans`, `prp-trp`); adding a new delegated rule means extending
that backend too.

---

## 6. Inspection tooling

```bash
# List every rule and whether it's compiled or delegated.
cargo run -p horndb-owlrl --bin show-rule -- --list

# Print the generated Rust for one rule.
cargo run -p horndb-owlrl --bin show-rule -- cax-sco

# Dump the entire generated_rules.rs to stdout (useful for diffing
# before/after a codegen change).
cargo run -p horndb-owlrl --bin show-rule -- --all
```

The binary reads from `COMPILED_RULES_SOURCE` (a `pub const &str` in
`src/lib.rs`) — a copy of `generated_rules.rs` embedded at compile
time. It does not re-run the codegen; you get exactly what the
library is using.

---

## 7. Known caveats and Stage-2 deferrals

- **`rdf-12` feature is on workspace-wide** after PR2 of the RDF 1.2
  migration. The Stage-1 OWL 2 RL engine still rejects triple-term
  inputs — `intern_term` and `triple_entailed` bail explicitly when a
  premise or conclusion quad carries a `TermRef::Triple` object (see
  `crates/owlrl/src/integration.rs`). Real triple-term entailment
  (reified rules, `sameTerm` over triple terms) is Stage-2 territory;
  the bail keeps tests loud rather than silently dropping data.
- **`eq-rep-p` skew is mitigated by a class-canonical path.** `eq-rep-p`
  substitutes predicates across `owl:sameAs`; the materialised output (each
  predicate in an `owl:sameAs` class carries the class's union extent) is
  semantically required and irreducible. The *work* is not: the engine
  evaluates `eq-rep-p` via `src/eq_rep_p_opt.rs` (union-find over
  `owl:sameAs`, union extent computed once per class) instead of the
  generated `O(k²)` nested-loop fire. `EqRepPStrategy::Naive` selects the
  generated path; `tests/eq_rep_p_differential.rs` proves the two reach the
  identical closure. The remaining downstream cost — `cls-*`/`cax-*` rules
  scanning a large materialised `rdf:type` partition (SPEC-04 F5
  partition-by-class-id) — is addressed for the **hand-written list rules**:
  `cls-int1`/`cls-uni`/`cax-adc`/`prp-key` partition their per-subject filtering
  by class id and parallelise it across rayon above `PAR_TYPE_THRESHOLD`,
  selected by `MaterializeOpts::parallel` (`ParallelStrategy::Auto` default;
  `Serial` is the differential-test oracle in
  `tests/rdf_type_skew_differential.rs`, benched in `benches/rdf_type_skew.rs`).
  The **compiled** `rules.toml` rules (`cax-sco`-style) are *not* yet
  parallelised — that needs a `FireFn` signature change and is Stage-2.
  See TASKS.md #2/#39.
- **Proof recording is implemented (SPEC-04 F4, acceptance #5, NF4).**
  Every compiled rule and every `list_rules.rs` rule records its real body
  triples as `Provenance.premises` on each derived triple.
  `MemStore::proof_tree` walks those premises recursively into a full
  `ProofTree` (leaves are asserted base triples; cycles are cut to keep the
  tree finite), and `Engine::proof(s, p, o)` returns the same tree decoded
  back to lexical IRIs (`StringProofTree`). A deep derivation (e.g. an
  N-step `rdfs:subClassOf` chain) yields a correspondingly deep proof in
  well under the NF4 100 ms budget — see `tests/proof_tree.rs`. Two
  intentional elisions remain: (a) the GraphBLAS closure backend
  (`graphblas_backend.rs`) records best-effort *empty* premises by design,
  so a closure-derived node is a `Derived` leaf rather than expanding
  further; (b) the restriction-rule schema declarations (`owl:maxCardinality`/
  `owl:onProperty`/`owl:onClass` for `cls-maxc*`/`cls-maxqc*`) are an elided
  side condition — the *instance-level* premises are still recorded, so the
  instance proof tree bottoms out at asserted instance data. The deferred
  part is production *persistence*: a compressed side-table with on-demand
  rederivation (Stage 2); today's premises live in-memory only.
- **No incremental deletion.** `reset_and_materialize` is the only
  re-derivation path (SPEC-04 F7); SPEC-06 / Stage 2 will add Z-set
  incremental updates.
- **`#[cfg(test)]` blocks inside `codegen/*.rs` are dead code.** The
  codegen modules are only `#[path]`-included by `build.rs`, which
  is compiled separately from the test runner. The tests in
  `codegen/parse.rs`, `codegen/plan.rs`, and `codegen/vocab.rs` never
  execute under `cargo test`. They serve as documentation of intent.
  Stage-2 should either move codegen behind a feature so it can be
  tested directly, or migrate these to real integration tests.
- **Stage-1 leading pattern must have a constant predicate.** See
  §2.5; relaxing this is gated on SPEC-03 (WCOJ).
- **List-walking rules (`scm-int`, `cls-int1`, `cls-uni`, `cax-adc`,
  `prp-adp`, `prp-spo2`, `prp-key`, `eq-diff2/3`) are implemented** in `src/list_rules.rs`
  (resolved once per `load`, fired in the semi-naïve loop — see `engine.rs`).
  The remaining gap is `cls-svf*` / `cls-avf*` (someValuesFrom / allValuesFrom
  restrictions), still Stage-2. The `dt-type1`/`dt-type2` datatype base is
  injected at load time by `src/datatypes.rs`. (These three families — list
  rules, the datatype base, and the `owl:Thing`-from-`NamedIndividual` pass —
  live outside `rules.toml`, which is why the RDFox A/B harness needs
  `scripts/bench/gen_schema_closure.py` to compare like-for-like; see #59.)
- **Literal-value datatype rules (`dt-eq`, `dt-diff`, `dt-not-type`) are
  implemented** in `src/datatype_literals.rs` (pure value-space parsing +
  classification) and wired by the load-time `inject_datatype_literal_axioms`
  pass in `integration.rs`. They reason over the *values* literals denote
  (unlike `datatypes.rs`, which reasons over datatype IRIs): value-equal
  literals across lexical forms / the integer tower ⇒ `owl:sameAs` (`dt-eq`);
  value-distinct comparable literals ⇒ `owl:differentFrom` (`dt-diff`); a
  lexical form outside its datatype's value space ⇒ `owl:Nothing`
  (`dt-not-type`). The conclusions are base axioms the compiled `eq-diff1` /
  `eq-rep-*` rules then propagate. `dt-not-type` also runs a **post-fixpoint**
  pass (`validate_derived_datatype_memberships`) that re-checks literals typed
  into a narrower datatype during materialisation (`prp-rng`/`prp-dom`), e.g.
  `"999"^^xsd:integer` typed `xsd:byte` via a range axiom ⇒ inconsistency.
  Unbounded integer types (`xsd:integer`, `(non)?(Positive|Negative)Integer`)
  are validated by arbitrary-precision string canonicalisation, so a literal
  larger than `i128` is **not** falsely flagged ill-typed. Stage-1 scope is the
  XSD integer tower,
  `xsd:string`/`boolean`, and plain/lang literals; other datatypes
  (`xsd:dateTime`, `xsd:decimal`, user types) stay **opaque** — never
  cross-compared, so no false `sameAs`/`differentFrom`. Full value-space
  *intersection* narrowing (`I5.8-008/009-pe`) remains deferred (#4). See #40.
  The pairwise comparison is O(k²) in distinct object literals `k`; a
  value-space-bucketed pass is a Stage-2 optimisation if `k` grows large.
- **Generated function bodies are O(|body|) nested loops.** This is
  fine for ~50 rules with bodies of length ≤ 4. SPEC-03 will replace
  the plan with leapfrog triejoin for arbitrary body sizes.

---

## 8. If you are porting the codegen to another language

You need to produce, given the same `rules.toml` and a vocabulary
declaration in the target language:

1. **A QName resolver.** Map every QName in the rule file to the
   target-language identifier for the corresponding vocabulary
   member. Single source of truth — do not maintain a parallel
   table.
2. **For each non-delegated rule**: a function with the contract
   "given a `TripleStore` (read-only) and an unused `Delta`, return
   a `Delta` of newly derivable triples this round". The function
   must:
   - Iterate the leading pattern via the store's
     `scan_predicate(<vocab-term>)`.
   - For each subsequent pattern, call `probe(s_opt, p, o_opt)` (or
     `probe_any_predicate` if the predicate is a variable not yet
     bound).
   - For each fully-bound assignment, construct the head triple and
     emit it iff it is not already in the store or the round delta.
   - Record `(rule_id, premises)` for every emitted triple.
3. **A rule dispatch table** (`RULES`) with one entry per rule
   carrying `id`, `delegated`, the fire function, the body
   predicates as accessor closures, and the `wildcard_predicate`
   flag.
4. **A semi-naïve driver** matching §3.4: per-round delta merging,
   apply to store, recompute dirty predicates from applied delta,
   loop. Honour the `wildcard_predicate` exception in the
   dirty-prune.
5. **A `ClosureBackend` hook** invoked once per round for delegated
   rules.

The W3C OWL 2 Profiles document is the authoritative source for the
rule semantics themselves. This file describes only the engineering
shape that makes those semantics fast and auditable.
