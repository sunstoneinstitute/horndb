# ADR-0016: HornDB is consumed as an embeddable reasoning view over an external system-of-record

**Status:** Proposed

**Date:** 2026-06-27

**Source:** Downstream constraint from the Sunstone data-platform decision `data-platform/docs/adr/0002-graph-as-canonical-metadata-store.md`. This ADR records what that decision asks of HornDB; it does not change HornDB's vision or non-goals (see ADR-0001).

## Context

The data-platform makes the unified knowledge graph the **canonical source of truth** and demotes Iceberg/Nessie, Oxigraph, and HornDB to **derived materializations**. Crucially, HornDB is chosen as a **reasoning view, not the system of record** — durability, branches/tags, transactions, and persistence stay in the SoR (a Postgres versioned quad store + transactional outbox). This keeps HornDB aligned with its existing non-goals (it is a reasoner, not a versioned DBMS) and its in-memory, history-collapsing design.

Reasoning runs in two tiers:

- **Central** — a HornDB over the full canonical graph is authoritative for inferred facts.
- **Local** — a developer runs an embedded HornDB hydrated with only the named graphs their work touches, reasoning over `subset ∪ local-delta`. OWL 2 RL fact derivation is monotonic, so this is sound but possibly incomplete — the right trade for iteration speed.

## Decision

Support being embedded as a reasoning view fed by an external SoR. Four capabilities, mapped to current status:

1. **Embeddable** (in-process, not only the axum SPARQL server). → SPEC-10 (Python/rdflib API) is the seam; currently *partial, off-workspace*.
2. **Pluggable named-graph hydration** — load named graphs on demand from an external source (the SoR / its changefeed), lazily. → *gap*; today loading is file-bulk (`loader/`), with N-Quads→named-graph routing but no pull source.
3. **Overlay delta + incremental closure** over a hydrated subset, queryable as the union of base + delta. → builds on existing Z-set/DBSP machinery (SPEC-06, ADR-0008); the multi-tenant overlay surface is new.
4. **Named-graph-scoped delta export** — emit a branch's local changes for push back to the SoR. → *gap*; snapshot export (SPEC-02 F9) is default-graph only; named-graph snapshots are explicitly deferred.

Explicitly **not** asked of HornDB: durability, git-like branches/tags, transactions, time-travel. Those remain SoR-owned.

## Consequences

- Hydration is always `touched-graphs + spine` (TBox + identity graph), because named graphs are a provenance unit, not a reasoning-locality boundary. The spine is the closure subset HornDB already materializes.
- Capabilities 2 and 4 (pluggable hydration, named-graph delta export) are new SPEC/TASKS work; 1 and 3 extend in-flight surfaces.
- No conflict with ADR-0001 non-goals: the versioned-store role is declined by design, not deferred.
- RDF 1.2 stays optional upstream (claims are n-ary nodes in RDF 1.1); consistent with ADR-0014, HornDB's RDF 1.2 readiness is a bonus, not a requirement the SoR depends on.
