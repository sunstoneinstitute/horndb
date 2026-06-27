# HornDB docs index

This is the human-facing entry point for the docs directory, and the first stop for coding agents using progressive discovery.

## Start here

- [`../README.md`](../README.md) — project overview, status, architecture, and build/test commands.
- [`../CLAUDE.md`](../CLAUDE.md) — working rules and repo conventions for agent sessions.
- [`specs/README.md`](specs/README.md) — index of the authoritative SPEC documents.
- [`adr/README.md`](adr/README.md) — Architecture Decision Records: the *why* behind the cross-cutting choices (the six SPEC-00 bets plus major tech decisions).
- [`../TASKS.md`](../TASKS.md) — live follow-up list and current gaps.
- [`../BENCHMARKS.md`](../BENCHMARKS.md) — performance targets, baselines, and measurement commands.

## Docs in this directory

- [`architecture.md`](architecture.md) — single-page architecture map across all SPECs, with a **Status** field (implemented / specified / planned / deferred) per subsystem and feature. Read this to see what exists today; kept in sync with `../TASKS.md`.
- [`adr/`](adr/README.md) — Architecture Decision Records (17 so far), one decision per file in Nygard format. Read these for the rationale behind a choice; read `architecture.md` for its current status.
- [`rdflib.md`](rdflib.md) — compare common rdflib workflows with the current HornDB surface area; read SPEC-10 for the compatibility contract.
- [`maplib.md`](maplib.md) — related-systems comparison with DataTreehouse's maplib: shared DNA, divergence, and a source-grounded look at its SPARQL-on-Polars execution model vs. HornDB's WCOJ executor (SPEC-03) and GraphBLAS closure (SPEC-05). Read before query-execution or closure work where the other Polars-native RDF engine is a useful reference.
- [`ideas/silicondb-horndb-claim-layer.md`](ideas/silicondb-horndb-claim-layer.md) — design sketch for a probabilistic claim layer with HornDB certification.
- [`specs/2026-06-05-provenance-symbolic-reasoning-landscape.md`](specs/2026-06-05-provenance-symbolic-reasoning-landscape.md) — competitive landscape: who else combines provenance proof with symbolic reasoning (EYE, RDFox, Stardog, GraphDB, Soufflé, Scallop, ZK-SPARQL). Read before scoping verifiable-justification work in SPEC-04/SPEC-08.

## Where to go next

- Working on query/update behavior? Read [`specs/SPEC-07-sparql-frontend.md`](specs/SPEC-07-sparql-frontend.md) and then [`rdflib.md`](rdflib.md). Active plan: [`plans/2026-06-08-SPEC-07-wcoj-bgp-executor.md`](plans/2026-06-08-SPEC-07-wcoj-bgp-executor.md) — wiring BGP eval onto `horndb-wcoj` (#67). Delivered: [`plans/2026-06-10-task-66-sparql-expression-surface.md`](plans/2026-06-10-task-66-sparql-expression-surface.md) — expression surface (arithmetic/`IF`/`COALESCE`/builtins) + `GRAPH` lowering (#66); [`plans/2026-06-14-SPEC-07-pattern-update.md`](plans/2026-06-14-SPEC-07-pattern-update.md) — pattern-based Update (`INSERT`/`DELETE … WHERE`) (#51); [`plans/2026-06-18-SPEC-07-kleene-paths.md`](plans/2026-06-18-SPEC-07-kleene-paths.md) — recursive Kleene property paths `*`/`+` via runtime closure (#50); [`plans/2026-06-18-spec07-graph-management-update.md`](plans/2026-06-18-spec07-graph-management-update.md) — graph-management Update verbs `LOAD`/`CLEAR`/`DROP`/`CREATE`/`ADD`/`MOVE`/`COPY` + multi-op updates, default-graph-only (#52); [`plans/2026-06-18-spec07-explain-pragma.md`](plans/2026-06-18-spec07-explain-pragma.md) — non-standard `EXPLAIN`/`EXPLAIN JSON` pragma (F9): renders the chosen plan with execution mode + per-node cardinality estimates, no execution (#53).
- Working on Python bindings or rdflib compatibility? Read [`specs/SPEC-10-rdflib-compatible-python-api.md`](specs/SPEC-10-rdflib-compatible-python-api.md), then [`rdflib.md`](rdflib.md) and the binding crate `crates/python/` (CLAUDE.md + README). The first increment (terms, `Graph`, parse/serialize, SPARQL passthrough, namespaces) is implemented; its differential gate is `crates/python/tests/` + [`harness/curation/rdflib-compat.md`](../harness/curation/rdflib-compat.md). The crate is off the Cargo workspace so `cargo build/test/clippy --workspace` stays Python-free.
- Working on storage or triple access? Read [`specs/SPEC-02-storage.md`](specs/SPEC-02-storage.md) and [`../crates/storage/INTEGRATION-NOTES.md`](../crates/storage/INTEGRATION-NOTES.md). Delivered: [`plans/2026-06-14-SPEC-02-hdt-snapshot.md`](plans/2026-06-14-SPEC-02-hdt-snapshot.md) — HDT-derived compact snapshot export/import (SPEC-02 F9, acceptance #5).
- Working on reasoning or rule behavior? Read [`specs/SPEC-04-rule-engine.md`](specs/SPEC-04-rule-engine.md) and [`../crates/owlrl/INTEGRATION-NOTES.md`](../crates/owlrl/INTEGRATION-NOTES.md). Delivered: [`plans/2026-06-18-SPEC-04-f5-rdftype-skew-parallelism.md`](plans/2026-06-18-SPEC-04-f5-rdftype-skew-parallelism.md) — `rdf:type` partition-by-class parallelism for the list rules (SPEC-04 F5, #39).
- Working on SSSOM mappings or ontology crosswalks? Read [`specs/SPEC-11-mappings.md`](specs/SPEC-11-mappings.md) and [`../crates/owlrl/INTEGRATION-NOTES.md`](../crates/owlrl/INTEGRATION-NOTES.md); the conformance subset is [`../harness/curation/sssom-mappings.md`](../harness/curation/sssom-mappings.md). The reasoning slice (vocab, chain rules, negative chaining, confidence, provenance, harness loader) is implemented; the compact crosswalk index/spine (serving slice) is still planned — see `architecture.md` §13.
- Working on incremental maintenance (deltas, retraction, snapshots)? Read [`specs/SPEC-06-incremental-maintenance.md`](specs/SPEC-06-incremental-maintenance.md) and [`../crates/incremental/FUTURE-WORK.md`](../crates/incremental/FUTURE-WORK.md). Delivered: [`plans/2026-06-17-spec06-f7-mvcc-snapshots.md`](plans/2026-06-17-spec06-f7-mvcc-snapshots.md) — refcounted `Circuit::snapshot()` MVCC reader handles (SPEC-06 F7, #46); [`plans/2026-06-18-spec05-closure-retraction.md`](plans/2026-06-18-spec05-closure-retraction.md) — closure-path retraction (deletion half of SPEC-05 F6: withdraws `ClosureInferred` rows whose base support is retracted, #5).
- Working on SIMD / vectorized hot loops (WCOJ seek/intersect, dictionary decode, columnar partition scans)? Read [`specs/SPEC-12-simd.md`](specs/SPEC-12-simd.md). Status: **specified** — the `horndb-simd` leaf crate is not yet created; see `architecture.md` §14. Note the issue [#2](https://github.com/sunstoneinstitute/horndb/issues/2) caveat: the `cax-sco` / `rdf:type` materialization scan is fixed by *indexing*, not SIMD, so SIMD is explicitly **out of scope** for that path.
- Working on the SPARQL HTTP surface? Read [`../crates/sparql/README.md`](../crates/sparql/README.md).

## Progressive discovery guidance for agents

1. Read this index first.
2. Pick the narrowest doc that matches the task.
3. Only then open the corresponding spec or crate notes.
4. If you add a new doc, give it a one-line summary here so future agents can find it without scanning the whole tree.

Keep this file short, current, and navigable.
