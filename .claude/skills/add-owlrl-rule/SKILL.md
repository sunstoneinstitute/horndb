---
name: add-owlrl-rule
description: This skill should be used when the user wants to add, edit, test, or debug an OWL 2 RL rule in crates/owlrl/rules.toml, add a vocabulary term to crates/owlrl/src/vocab.rs, inspect the compiled output of a rule, or understand how the owlrl codegen pipeline works. Triggers include phrases like "add a rule", "new OWL rule", "edit rules.toml", "add a vocab term", "rdf:type / rdfs:* / owl:* term", "delegate to closure", "show the compiled rule", "inspect generated_rules.rs", "fire_<id>".
---

# Adding or editing OWL 2 RL rules

The canonical reference is **`crates/owlrl/AGENTS.md`** — read it first
when in doubt. This skill is the operational shortcut.

## Files in play

- `crates/owlrl/rules.toml` — the rule source. Each `[[rule]]` block
  has `id`, `comment`, optional `delegate = "closure"`, `body`
  (array of `{ s, p, o }` patterns), `head` (single pattern).
- `crates/owlrl/src/vocab.rs` — the **single source of truth** for
  vocabulary. Each field carries a `///` QName doc comment in
  backticks (e.g. `` /// `rdf:type` ``).
- `crates/owlrl/codegen/{parse,plan,emit,vocab}.rs` — read-only
  unless the user is changing the codegen pipeline itself.
- Generated output: `$OUT_DIR/generated_rules.rs`. Inspect with
  `cargo run -p horndb-owlrl --bin show-rule -- <id>`.

## Workflow: add a new rule

1. Find a structurally similar existing `[[rule]]` (same arity, same
   `delegate` status). Copy it as the starting point — do not write
   from scratch.
2. Edit `id`, `comment`, `body`, `head`. Cite the W3C source in
   `comment` (e.g. `"Table 7 row cax-eqc1"`).
3. **If the rule references a vocab token (`prefix:name`) not
   already in `vocab.rs`:** follow the "add a vocab term" workflow
   below first.
4. Add a regression test under `crates/owlrl/tests/single_rule.rs`
   (or `tests/rule_<id>.rs`). Shape:
   ```rust
   let v = Vocabulary::synthetic(1000);
   let mut store = MemStore::new(v);
   store.assert(/* antecedent triples */);
   materialize(&mut store, &mut RuleFiringBackend::new());
   assert!(store.contains(&expected_head_triple));
   ```
5. Build & inspect:
   ```bash
   cargo build -p horndb-owlrl
   cargo run -p horndb-owlrl --bin show-rule -- <id>
   ```
   Confirm the emitted Rust matches your intent.
6. Test:
   ```bash
   cargo test -p horndb-owlrl
   ```

## Workflow: add a vocab term

Adding a vocab term is **a single-file edit** thanks to the
auto-derived QName map. Do NOT touch `codegen/parse.rs` — it picks
up new terms automatically.

1. Open `crates/owlrl/src/vocab.rs`.
2. Add a field to `struct Vocabulary` with a `///` QName doc:
   ```rust
   /// `owl:newTerm`
   pub owl_new_term: TermId,
   ```
3. Add a matching line in `Vocabulary::synthetic()`:
   ```rust
   owl_new_term: next(),
   ```
4. If the new term should also be seeded by `Engine::load` (it
   usually should, for IRI dictionary alignment), add:
   - A `const OWL_NEW_TERM: &str = "http://www.w3.org/.../newTerm";`
     in `integration.rs`.
   - A matching `(OWL_NEW_TERM, vocab.owl_new_term)` entry in
     `build_vocab()`.

That's it. Next build will see the new QName everywhere.

## Inspection tooling

```bash
# What rules exist? Which are delegated?
cargo run -p horndb-owlrl --bin show-rule -- --list

# Show one rule's compiled Rust.
cargo run -p horndb-owlrl --bin show-rule -- cax-sco

# Dump the whole generated_rules.rs.
cargo run -p horndb-owlrl --bin show-rule -- --all
```

## When a build fails

- `unknown vocabulary token "owl:foo"` → the QName is not in
  `vocab.rs`. The fix is in `vocab.rs`, never in `parse.rs`.
- `Vocabulary field X is missing its QName doc comment` → add a
  `///` doc comment with the QName in backticks above field X.
- `duplicate QName "foo:bar" on Vocabulary fields a and b` → two
  fields claim the same QName; pick one.

## What `delegate = "closure"` means

The rule is **not compiled**. Its `fire` function is a no-op stub.
Instead, the engine routes it to a `ClosureBackend`
implementation (`horndb-closure` in production,
`backend.rs::RuleFiringBackend` in Stage-1 tests). The backend must
implement the rule's semantics. **Do not** mark a rule
`delegate = "closure"` without ensuring the backend covers it —
the Stage-1 reference backend hard-codes the small set it knows
about (see `backend.rs`).

Currently delegated: `eq-ref`, `eq-sym`, `eq-trans`, `prp-trp`,
`scm-sco`, `scm-spo` (transitive-closure-shaped, faster via
GraphBLAS).

## Common pitfalls

- **Wildcard predicate + semi-naïve prune.** A rule with `p = "?p"`
  in any body pattern must be marked `wildcard_predicate: true` in
  the emitted `CompiledRule` (the emitter handles this
  automatically) — otherwise the engine's dirty-predicate prune
  under-fires it. See commit e0ca19a.
- **Leading pattern predicate must be a constant.** Stage-1's
  planner panics if the *first* body pattern has a variable
  predicate. Rules like `eq-rep-s`/`eq-rep-o` work because the
  variable predicate is in a *later* pattern.
- **`eq-rep-p` partition blowup.** It substitutes predicates across
  `owl:sameAs`. On adversarial inputs the `rdf:type` partition can
  explode — Stage-1 ships the literal W3C rule; a guard is
  Stage-2 work (see `rules.toml` comment).

## Verification before declaring done

```bash
cargo build -p horndb-owlrl
cargo test -p horndb-owlrl
cargo run -p horndb-owlrl --bin show-rule -- <new-rule-id>
cargo clippy -p horndb-owlrl --all-targets -- -D warnings
```

For confidence in the wider workspace (and what CI/pre-push runs):

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace
```
