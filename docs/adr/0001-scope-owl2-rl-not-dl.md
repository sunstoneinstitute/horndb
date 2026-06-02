# ADR-0001: Scope to OWL 2 RL, not OWL 2 DL; RDF + SPARQL only

**Status:** Accepted

**Date:** 2026-06-02

**Source:** Extracted retrospectively from `docs/specs/SPEC-00-vision.md` and `docs/architecture.md`.

## Context

Sunstone needs OWL reasoning at multi-billion-triple scale without lock-in to RDFox (Samsung) or GraphDB (Graphwise). The scope must be bounded to ship on a small-team budget and to be gradeable against a fixed conformance corpus.

- OWL 2 DL completeness (HermiT, Pellet, Konclude) is a separate, harder problem class — not winnable at this budget.
- Property-graph databases (Neo4j) are a different data model entirely.
- A bounded scope is what makes the downstream architectural bets (GraphBLAS closure, ahead-of-time rule compilation) tractable.

## Decision

Target OWL 2 RL/RDF semantics only, over the RDF data model, with a SPARQL 1.1 query surface.

Explicit non-goals:

- OWL 2 DL completeness (full existentials, complex cardinality beyond RL).
- Beating RDFox on pure single-node main-memory materialization throughput.
- A rule-interpretation engine.
- Embedding-based neural reasoning as the source of truth.
- Being a property-graph database.
- SPARQL 1.1 Federation and GeoSPARQL (deferred indefinitely).

## Consequences

+ Bounded scope maps cleanly onto Datalog-style forward rules plus transitive closure, which in turn enables the GraphBLAS and ahead-of-time-compilation bets.
+ Gradeable against the W3C OWL 2 RL conformance suite.
− No DL expressivity (full existentials, complex cardinality beyond RL).
− Property-graph and DL feature requests must be declined.

100% on the W3C OWL 2 RL conformance suite is a non-negotiable project success criterion.

## Related

- Governing spec: `docs/specs/SPEC-00-vision.md` (differentiating bets, non-goals, success criteria).
- `docs/architecture.md` §1.
- Siblings: ADR-0004, ADR-0005, ADR-0006.
