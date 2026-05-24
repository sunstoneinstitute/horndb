# SPEC-08 Integration Notes for `reasoner-closure`

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

No `reasoner-closure` API needs to change for Stage 0/1 — this
integration is a SPEC-05 plan task that calls into `reasoner-ml`'s
existing types only.
