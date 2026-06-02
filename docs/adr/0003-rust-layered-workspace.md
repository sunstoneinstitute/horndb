# ADR-0003: Rust implementation on a layered nine-crate workspace

**Status:** Accepted

**Date:** 2026-06-02

**Source:** Extracted retrospectively from `docs/specs/SPEC-00-vision.md` and `docs/architecture.md`.

## Context

The engine is performance- and memory-safety-critical. It needs zero-cost abstractions, mature Apache Arrow interop, and clean C-ABI FFI to SuiteSparse:GraphBLAS. Nemo (a Rust reasoner) is precedent. Clear module boundaries are needed so subsystems can be built and reviewed in parallel.

## Decision

Implement in Rust (edition 2021, pinned to 1.88.0 via `rust-toolchain.toml`).

- Nine crates under `crates/`, all `publish = false`.
- Strict dependency order: `storage → wcoj → {owlrl, closure} → incremental → sparql`, with `harness` and `ml` on top.
- Apache Arrow is the columnar in-memory exchange format.
- Shared dependencies live in the root `[workspace.dependencies]`.

## Consequences

+ Memory safety with no GC pauses; mature FFI for the GraphBLAS C ABI.
+ Crate boundaries enforce layering and enable parallel (multi-agent) development.
− Rust build times, dominated by GraphBLAS and rocksdb.
− The pinned toolchain needs periodic deliberate bumps.

## Related

- Governing spec: `docs/specs/SPEC-00-vision.md` (implementation language and dependencies).
- `docs/architecture.md` §2; root `CLAUDE.md` (workspace layout).
- Siblings: ADR-0006, ADR-0009.
