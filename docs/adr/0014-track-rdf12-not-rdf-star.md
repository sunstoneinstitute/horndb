# ADR-0014: Track W3C RDF 1.2 (not RDF-star), gated behind config

**Status:** Accepted

**Date:** 2026-06-02

**Source:** Extracted retrospectively from `docs/specs/SPEC-00-vision.md` (RDF 1.2 note), `docs/architecture.md`, and the root `CLAUDE.md` (`horndb-sparql` notes).

## Context

The graph model needs quoted / triple terms. The community-driven RDF-star extension was superseded by the W3C RDF 1.2 standard, which has the essentially-same graph model but cleaner semantics and a cleaner SPARQL surface. SPARQL 1.1 callers must keep 1.1 semantics unchanged regardless of which direction the project takes.

## Decision

HornDB tracks W3C RDF 1.2, not RDF-star.

- `TermKind::TripleTerm` is added in storage.
- The N-Triples loader supports `<<( s p o )>>` objects.
- The `rdf12-n-triples` harness syntax suite gates the loader surface.
- SPARQL triple-term patterns are gated at runtime by `SparqlConfig::rdf12` (default `false`).
- RDF 1.2 is a Stage-2 priority, not a Stage-1 deliverable; Turtle / TriG / N-Quads and the semantics suites are deferred.

## Consequences

- `+` Standards-tracking rather than betting on a superseded extension.
- `+` The default-off gate keeps SPARQL 1.1 callers on 1.1 semantics.
- `−` Only the N-Triples and (gated) SPARQL surface ship at Stage 1; the OWL 2 RL engine and W3C-manifest paths explicitly bail on triple-term inputs.

## Related

- `docs/specs/SPEC-00-vision.md` (RDF 1.2 note)
- `docs/architecture.md` §13
- Root `CLAUDE.md` (`horndb-sparql` notes)
- Sibling: ADR-0011
