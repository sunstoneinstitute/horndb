# SPEC-08 Integration Notes for `reasoner-owlrl`

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
