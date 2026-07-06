# ADR-0015: Vendor SuiteSparse:GraphBLAS as a static git submodule

**Status:** Accepted

**Date:** 2026-06-02

**Source:** Extracted retrospectively from SPEC-05, `docs/specs/SPEC-13-shared-graphblas-build.md`, and `docs/architecture.md`.

## Context

`horndb-closure` links GraphBLAS over the C ABI. Depending on a system-installed `graphblas` makes builds and CI nondeterministic and version-fragile. The multi-worktree Stage-1 workflow compounded the cost: each worktree recompiled GraphBLAS independently, a 1–3 minute cold compile and hundreds of MB of artifact every time.

## Decision

Vendor GraphBLAS as a statically linked git submodule with checked-in bindings, built once per `(target, version)` and shared across worktrees.

- Vendor GraphBLAS `v10.3.0` as a git submodule under `crates/closure/vendor/GraphBLAS`, statically linked, with checked-in bindings.
- Make `vendored` + `openmp` default Cargo features; keep `regen-bindings` optional.
- Build once per `(target, version)` into a flock-guarded shared directory (`vendor/.shared-build/<target>/<version>/`, anchored at the main worktree), reused across git worktrees.
- Cache that directory in CI, keyed on the submodule SHA.

## Consequences

+ Reproducible, hermetic builds with no system dependency.
+ The worktree pool shares a single GraphBLAS build instead of N.
− Large submodule; the first build is slow.
− Bindings must be regenerated (`regen-bindings`) on a version bump.
− This narrows the remaining multi-worktree disk-pressure concern to rocksdb.

## Related

- Governing spec: `docs/specs/SPEC-05-closure-backend.md`.
- Design: `docs/specs/SPEC-13-shared-graphblas-build.md`.
- Architecture: `docs/architecture.md` §7.
- Integration notes: `crates/closure/INTEGRATION-NOTES.md`.
- Siblings: ADR-0006.
