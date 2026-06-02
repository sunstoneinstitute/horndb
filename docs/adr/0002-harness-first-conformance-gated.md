# ADR-0002: Harness-first, conformance-gated development

**Status:** Accepted

**Date:** 2026-06-02

**Source:** Extracted retrospectively from `docs/specs/SPEC-00-vision.md`, `docs/specs/SPEC-01-*.md`, and `docs/architecture.md`.

## Context

Reasoners are correctness-critical and regressions are silent. Building the engine first and bolting on tests later invites un-gradeable "done" claims. The team needs an objective gate for every stage.

## Decision

Build the SPEC-01 conformance/benchmark harness *before* the engine.

- Every SPEC's acceptance criteria reference a concrete subset of the harness corpus; a SPEC is not satisfied until that subset is green in CI.
- New work may grow a subset but never bypass it.
- `harness/selected.toml` at the workspace root is the canonical selection.
- Stage gates are objective — e.g. the ≥50-case W3C OWL 2 RL subset must be green for Stage 1.

## Consequences

+ Every implementation commit lands in a repo that already knows how to grade it.
+ Stage go/no-go decisions are measurable, not subjective.
− Upfront cost: the bench exists before the features it grades.
− The harness pulls heavy transitive deps (`oxrocksdb-sys`, ~700 MB) into the build.

## Related

- Governing specs: `docs/specs/SPEC-00-vision.md` (process rule, harness-first ordering); `docs/specs/SPEC-01-*.md`.
- `docs/architecture.md` §2, §3.
- Siblings: ADR-0001.
