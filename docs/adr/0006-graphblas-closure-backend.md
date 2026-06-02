# ADR-0006: SuiteSparse:GraphBLAS for the closure subset

**Status:** Accepted

**Date:** 2026-06-02

**Source:** Extracted retrospectively from SPEC-00 (bet 4), SPEC-05, and `docs/architecture.md`.

## Context

Schema-level transitive closure — `rdfs:subClassOf`, `rdfs:subPropertyOf`, `owl:sameAs`, and transitive properties — is naturally expressed as linear algebra over a boolean semiring rather than as iterated rule firing. Computing deep closures by repeatedly firing rules in the rule engine is expensive, and no production OWL reasoner exploits the linear-algebra formulation today. A backend that treats closure as matrix multiplication can lean on a hyper-optimized library and opens a path to GPU acceleration later.

## Decision

Compute the closure subset as semiring matrix multiplication on SuiteSparse:GraphBLAS, in the `horndb-closure` crate.

- Express closure as iterated `GrB_mxm` with the `LOR_LAND_BOOL` semiring.
- Bridge dictionary IDs to matrix indices with a dense-renumbering cache.
- Write materialized results back to storage directly, without re-firing rules.

## Consequences

+ Closures are computed by a hyper-optimized library rather than by iterated rule firing.
+ Opens a GPU GraphBLAS path for Stage 3.
+ Makes valued / custom-semiring annotated reasoning reachable later.
− Adds a C-ABI dependency and build complexity (bindgen, pkg-config, `links = "graphblas"`).
− Requires dense renumbering to bridge dictionary IDs ↔ dense matrix indices.

## Related

- Governing specs: `docs/specs/SPEC-00-vision.md` (bet 4), `docs/specs/SPEC-05-closure-backend.md`.
- Architecture: `docs/architecture.md` §7.
- Siblings: ADR-0007, ADR-0015, ADR-0004.
