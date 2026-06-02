# ADR-0012: Symbolic reasoner is the source of truth; ML is an opt-in advisor

**Status:** Accepted

**Date:** 2026-06-02

**Source:** Extracted retrospectively from `docs/specs/SPEC-00-vision.md`, `docs/specs/SPEC-08-ml-integration.md`, and `docs/architecture.md`.

## Context

LLM- and embedding-based reasoning is probabilistic and non-auditable, which conflicts directly with HornDB's hard provenance requirement. At the same time, ML can genuinely help with query planning, `owl:sameAs` candidate generation, LLM-to-SPARQL translation, and hot-set / tier placement. The project needs the upside of ML without ever letting a model's output decide what is true.

## Decision

The symbolic reasoner is always the source of truth; ML may only propose or advise.

- ML lives in the `horndb-ml` crate and is exposed behind explicit traits: `CandidateGenerator`, `PlanAdvisor`, and `HotSetAdvisor`.
- The entire crate is opt-in via configuration (`ml.enabled`).
- Disabling all ML must be bit-identical for correctness (NF1): results do not change when ML is off.

## Consequences

- `+` Correctness never depends on a model; results stay reproducible and the ML layer is removable without changing answers.
- `+` ML is a pure force multiplier — it can only improve planning, candidate generation, and placement, never corrupt the answer.
- `−` ML-proposed facts (for example candidate `owl:sameAs` links) still require symbolic verification and provenance before they are trusted.

## Related

- `docs/specs/SPEC-00-vision.md` (non-goals)
- `docs/specs/SPEC-08-ml-integration.md`
- `docs/architecture.md` §10
- Sibling: ADR-0013
