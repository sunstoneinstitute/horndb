# ADR-0004: Compile OWL 2 RL rules ahead of time (Soufflé-style)

**Status:** Accepted

**Date:** 2026-06-02

**Source:** Extracted retrospectively from SPEC-00 (bet 5), SPEC-04, and `docs/architecture.md`.

## Context

The OWL 2 RL/RDF rule set is fixed and fully known at build time. A runtime rule interpreter would add per-tuple dispatch overhead in the hottest path of the engine, where rules fire repeatedly over large delta tables during materialization.

## Decision

Compile the rules ahead of time rather than interpreting them at runtime:

- Generate native Rust from `crates/owlrl/rules.toml` in `build.rs`, driven by the codegen pipeline in `codegen/`.
- Emit one `fire_<id>` function per rule and evaluate semi-naïvely with delta tables.
- Keep no rule interpreter in the hot path.
- Defer user-defined runtime Datalog rules to the Stage-2 Datalog frontend.

## Consequences

- `+` No dispatch overhead; rules monomorphize to native code.
- `+` `rules.toml` is the single editable source of rule truth.
- `−` Slower first build; editing rules triggers a codegen rebuild.
- `−` No runtime-defined rules until the Stage-2 Datalog frontend.

## Related

- Governing specs: `docs/specs/SPEC-00-vision.md` (bet 5), `docs/specs/SPEC-04-rule-engine.md`.
- Architecture: `docs/architecture.md` §6.
- Sibling ADRs: ADR-0005, ADR-0006, ADR-0007.
