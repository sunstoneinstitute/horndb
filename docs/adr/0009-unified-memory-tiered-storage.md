# ADR-0009: Unified-memory hardware as a first-class target (tiered columnar storage)

**Status:** Accepted

**Date:** 2026-06-02

**Source:** Extracted retrospectively from `docs/specs/SPEC-00-vision.md` (bet 2), `docs/specs/SPEC-02-storage.md`, and `docs/architecture.md`.

## Context

Modern nodes expose a memory hierarchy — HBM (~5 TB/s), DDR5, CXL-attached DRAM, NVMe. RDFox's main-memory shared-everything design wastes HBM and CXL bandwidth. Sunstone targets multi-billion-triple scale on single MI300A/GH200-class nodes, so the storage layout must be ready for bandwidth-tiered placement from the start.

## Decision

Treat tiered unified memory as a first-class target. `horndb-storage` is:

- Predicate-partitioned and columnar.
- Dictionary-encoded with stable 64-bit IDs (term kind in the high bits, small literals inlined).
- Organised behind a tier API (HBM hot / DDR5 warm / CXL+NVMe cold).
- Shipping a single warm tier plus tiering scaffolding at Stage 1; GPU/CXL/NVMe placement is SPEC-09 (Stage 3).

## Consequences

+ Data layout is ready for bandwidth-tiered placement and Apache Arrow interop.
+ Dictionary encoding shrinks the working set.
− Full tiering, MVCC, the HDT cold tier, and a persistent on-disk dictionary are deferred to Stage 2+.
− The tier API is scaffolding until Stage 3 fills it.

## Related

- Governing spec: `docs/specs/SPEC-02-storage.md`; vision bet 2 in `docs/specs/SPEC-00-vision.md`.
- Current state: `docs/architecture.md` §4.
- Siblings: ADR-0003, ADR-0010.
