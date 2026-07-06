---
status: draft
date: 2026-05-25
scope: "SPEC-10 — rdflib-Compatible Python API"
---

# SPEC-10 — rdflib-Compatible Python API

## Purpose

Define a Python-facing API that lets common `rdflib`-oriented code run against HornDB with minimal changes, while delegating storage, SPARQL, and reasoning to the Rust engine. The goal is not to clone every corner of `rdflib`; it is to provide a practical compatibility layer for graph-centric workflows, notebooks, data pipelines, and test suites that already speak the `rdflib` idiom.

## Scope

In scope:

- A Python package exposing `rdflib`-shaped term classes and helpers:
  - `URIRef`
  - `BNode`
  - `Literal`
  - `Variable`
  - `Namespace`
  - `NamespaceManager`
- Graph-facing containers and facades:
  - `Graph`
  - `Dataset`
  - `ConjunctiveGraph` or a compatibility alias with the same practical behavior
- Core graph operations:
  - `add`, `remove`, `set`
  - `triples`, `subjects`, `predicates`, `objects`, `value`
  - `__len__`, `__contains__`, iteration over terms/triples
- SPARQL query and update passthrough to HornDB’s SPARQL frontend.
- Parsing and serialization for a small, stable subset of RDF formats, at minimum Turtle and N-Triples.
- Named-graph support sufficient for the query/update and Dataset APIs above.
- A compatibility layer for common rdflib import patterns used in application code.

Out of scope for Stage 1:

- The full rdflib plugin registry and entry-point ecosystem.
- All rdflib store backends and remote store adapters.
- A Python-native reasoner; entailment stays in the Rust engine.
- Full parity with every rdflib serializer/parser format.
- Property-graph APIs, SHACL authoring, or other non-RDF surfaces.

## Compatibility target

The compatibility target is **common Graph-centric rdflib code**, not the entire rdflib package surface. The library should aim to support the kinds of code people usually write first:

1. create a graph,
2. load RDF,
3. add/remove triples,
4. run SPARQL queries,
5. serialize results or the graph,
6. work with namespaces and named graphs.

If a feature only exists because rdflib has a deep plugin architecture or a large legacy API surface, it is not automatically in scope.

## Functional requirements

**F1. rdflib-shaped term semantics.** `URIRef`, `BNode`, `Literal`, and `Variable` must preserve rdflib-compatible equality, hashing, and stringification for the common RDF term cases covered by the compatibility suite.

**F2. Graph lifecycle.** `Graph` must provide the usual rdflib creation and mutation workflow: instantiate, bind namespaces, add/remove triples, test membership, iterate triples, and compute size.

**F3. Dataset and named-graph support.** `Dataset` and `ConjunctiveGraph` must expose a practical named-graph model that supports the subset of rdflib code exercised by the compatibility tests and the SPARQL frontend.

**F4. Parse and serialize.** `Graph.parse()` and `Graph.serialize()` must support at least Turtle and N-Triples. Additional formats may be added when the backing parser/serializer is stable, but the Python API must not require rdflib’s plugin system to function.

**F5. SPARQL passthrough.** `Graph.query()` and `Graph.update()` must delegate to HornDB’s SPARQL frontend and return Python objects with sensible, documented iteration and result-access behavior.

**F6. Namespace handling.** `bind()`, `namespace_manager`, and prefix/QName helpers must behave closely enough to rdflib that ordinary serialization and query-string construction workflows work without special casing.

**F7. Error compatibility.** The compatibility layer should raise rdflib-like exceptions where practical, or documented HornDB-specific subclasses that are easy to catch from existing rdflib-oriented code.

**F8. Minimal import friction.** The public module layout should mirror rdflib’s common import paths closely enough that existing code can migrate by changing only the package name, or by using a thin shim package if a fully drop-in import path is not acceptable for packaging reasons.

**F9. Streaming behavior.** Queries, parses, and serializations must be lazy/streamed where feasible so Python does not have to materialize entire datasets just to forward data to or from the Rust engine.

## Non-functional requirements

**NF1. Supported Python versions.** Initial support target: CPython 3.10 through 3.13 on macOS and Linux. Windows support is desirable but may be deferred if the Rust/Python build story is not yet stable there.

**NF2. Low overhead on the hot path.** Term conversion and triple iteration must avoid unnecessary Python object creation. Heavy scans should release the GIL around Rust-side work when safe.

**NF3. Fast import, delayed initialization.** Importing the package should not initialize a store, open files, or start a server. Expensive setup must be explicit or lazy.

**NF4. Behavioral parity over surface mimicry.** When rdflib and HornDB differ semantically, the compatibility layer should prefer correctness and document the divergence rather than silently inventing a misleading approximation.

**NF5. Regression coverage.** The compatibility layer must be covered by an automated suite that compares HornDB behavior with rdflib on a curated set of representative examples.

## Dependencies

- SPEC-01 — the conformance harness must host the Python compatibility subset.
- SPEC-02 — storage and triple access.
- SPEC-07 — SPARQL query/update parsing, planning, execution, and serialization.
- SPEC-04 — reasoning semantics that may affect query answers under the default entailment regime.
- A Python binding layer such as PyO3/maturin or an equivalent maintained build path.
- The existing `rdflib` package for differential tests and compatibility references.

## Acceptance criteria

1. SPEC-01 includes a dedicated `rdflib-compat` subset covering term classes, `Graph` mutation, namespace binding, parse/serialize, and SPARQL query/update.
2. A curated smoke suite based on common rdflib examples passes against HornDB with the compatibility layer installed.
3. `Graph.add()`, `remove()`, `triples()`, `query()`, `update()`, `parse()`, and `serialize()` all work on a representative fixture without requiring manual HornDB internals.
4. Namespace round-tripping, `len(graph)`, and membership checks behave consistently with the documented compatibility rules.
5. Named-graph access through `Dataset` or `ConjunctiveGraph` works for the subset of operations exercised by the compatibility suite.
6. Differential tests against rdflib pass for the agreed compatibility subset, and any intentional divergence is documented in the spec or a linked compatibility note.
7. The supported Python versions build and run the compatibility suite in CI on at least one macOS and one Linux target.

## Risks and open questions

- **rdflib semantics are broad and historically messy.** Literal normalization, datatype handling, blank-node behavior, and namespace edge cases can differ subtly across rdflib versions.
- **Plugin ecosystem mismatch.** Many rdflib users depend on parser/serializer/store plugins. We should be explicit about whether the first release is a compatibility layer or a full ecosystem clone.
- **Import-path strategy.** Shipping a module literally named `rdflib` risks collisions with the upstream package; shipping a different distribution name risks one extra migration step. This needs a packaging decision.
- **Named graph semantics.** `Graph`, `Dataset`, and `ConjunctiveGraph` are not all the same thing in practice, and existing rdflib code often relies on subtle behavior around default graphs and contexts.
- **Reasoning visibility.** rdflib users may expect asserted-triple semantics from graph inspection APIs, while HornDB may want entailment-aware query answers. The compatibility layer must be explicit about which operations are base-store only and which are inference-aware.
- **Format coverage.** Turtle and N-Triples are enough for a first useful slice, but many existing rdflib workflows also expect TriG, N-Quads, RDF/XML, and JSON-LD.
