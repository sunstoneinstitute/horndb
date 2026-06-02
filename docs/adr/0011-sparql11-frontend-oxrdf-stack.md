# ADR-0011: SPARQL 1.1 frontend on the oxrdf / spargebra stack

**Status:** Accepted

**Date:** 2026-06-02

**Source:** Extracted retrospectively from `docs/specs/SPEC-07-sparql-frontend.md` and `docs/architecture.md`.

## Context

HornDB needs a standards-compliant SPARQL 1.1 surface without hand-writing a parser and RDF term model. The oxigraph ecosystem (oxrdf, spargebra, oxrdfio, sparesults) is mature and RDF 1.2-capable, making it a sound basis for the frontend.

## Decision

`horndb-sparql` parses with spargebra and builds its own algebra, planner, and runtime:

- BGPs route to the WCOJ executor.
- Serves SELECT/CONSTRUCT/ASK and SPARQL Update `INSERT/DELETE DATA` over an axum HTTP server (`server` feature, on by default).
- The workspace pins unified `oxrdf 0.3` / `oxrdfio 0.2` / `sparesults 0.3` with `rdf-12` features enabled workspace-wide.

## Consequences

+ A standards-aligned term model and parser for free; RDF 1.2-ready.
− Inherits the oxigraph dependency surface, including oxrocksdb-sys transitively via the harness.
− Enabling `oxrdf/rdf-12` forces `oxigraph/rdf-12` workspace-wide (Cargo only unifies features upward).
− The backward-chained entailment mode is deferred (it depends on magic-sets/tabling).

## Related

- Governing spec: `docs/specs/SPEC-07-sparql-frontend.md`; crate notes in root `CLAUDE.md`.
- Current state: `docs/architecture.md` §9.
- Siblings: ADR-0005, ADR-0010, ADR-0014.
