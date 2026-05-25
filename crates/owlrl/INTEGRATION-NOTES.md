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

## List-axiom rules live in `list_rules.rs`, not `rules.toml`

The W3C OWL 2 RL rules that walk an `rdf:List` from the ontology — namely
`prp-spo2` (property chains), `prp-key` (`owl:hasKey`), `cls-int1`
(`owl:intersectionOf`), `cls-uni` (`owl:unionOf`), `cax-adc`
(`owl:AllDisjointClasses`), and `eq-diff2`/`eq-diff3`
(`owl:AllDifferent`) — *cannot* be expressed in the `rules.toml`
fixed-shape schema. Each axiom declaration in the ontology determines a
different rule arity (the chain length, key length, intersection arity,
... depends on the ontology, not the rule). The codegen pipeline in
`crates/owlrl/codegen` is built for fixed-shape rules and would have to
become a Datalog interpreter to handle variable-arity bodies — which
SPEC-04 F2 explicitly forbids.

The chosen architecture (RDFox, ISWC 2015 §4.2) is therefore:

1. At load time, walk the schema partitions for each list-declaring
   predicate (`owl:propertyChainAxiom`, `owl:hasKey`, ...) and resolve
   every `rdf:first`/`rdf:rest` chain into a `Vec<TermId>`. The result
   lives on a `SchemaAxioms` struct held by `engine::materialize`. This
   is a one-shot, schema-level computation per `Engine::load`.
2. Each list-axiom rule lives as a hand-written `fn fire_*` in
   `list_rules.rs`. The hot path is a hand-rolled nested loop reading
   the resolved `Vec<TermId>` — no dynamic dispatch on rule shape, no
   list-walking inside the inner join.
3. The `engine::materialize` driver calls `list_rules::fire_all` once per
   semi-naïve round, *between* the compiled-rule sweep and the closure
   backend. Each list rule advertises the predicate-IDs its body reads
   (via `SchemaAxioms::body_predicates`) so the same dirty-predicate
   prune that protects compiled rules also protects the list ones.

The companion **auto-`owl:Thing`** load-time pass
(`integration::infer_owl_thing_from_named_individuals`) asserts
`?x rdf:type owl:Thing` for every `?x rdf:type owl:NamedIndividual` —
required for `prp-rfp` to fire against ontologies that type their
individuals with `owl:NamedIndividual` (which is most of the W3C test
suite).

Stage-1 is insertion-only (SPEC-06 limitation) so the resolved
`SchemaAxioms` are stable across all semi-naïve rounds inside one
`materialize` call. Stage-2 will revisit this when retraction lands.
