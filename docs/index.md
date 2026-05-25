# HornDB docs index

This is the human-facing entry point for the docs directory, and the first stop for coding agents using progressive discovery.

## Start here

- [`../README.md`](../README.md) — project overview, status, architecture, and build/test commands.
- [`../CLAUDE.md`](../CLAUDE.md) — working rules and repo conventions for agent sessions.
- [`../specs/README.md`](../specs/README.md) — index of the authoritative SPEC documents.
- [`../TASKS.md`](../TASKS.md) — live follow-up list and current gaps.
- [`../BENCHMARKS.md`](../BENCHMARKS.md) — performance targets, baselines, and measurement commands.

## Docs in this directory

- [`rdflib.md`](rdflib.md) — compare common rdflib workflows with the current HornDB surface area; read SPEC-10 for the compatibility contract.
- [`silicondb-horndb-claim-layer.md`](silicondb-horndb-claim-layer.md) — design sketch for a probabilistic claim layer with HornDB certification.

## Where to go next

- Working on query/update behavior? Read [`../specs/SPEC-07-sparql-frontend.md`](../specs/SPEC-07-sparql-frontend.md) and then [`rdflib.md`](rdflib.md).
- Working on Python bindings or rdflib compatibility? Read [`../specs/SPEC-10-rdflib-compatible-python-api.md`](../specs/SPEC-10-rdflib-compatible-python-api.md) and then [`rdflib.md`](rdflib.md).
- Working on storage or triple access? Read [`../specs/SPEC-02-storage.md`](../specs/SPEC-02-storage.md) and [`../crates/storage/INTEGRATION-NOTES.md`](../crates/storage/INTEGRATION-NOTES.md).
- Working on reasoning or rule behavior? Read [`../specs/SPEC-04-rule-engine.md`](../specs/SPEC-04-rule-engine.md) and [`../crates/owlrl/INTEGRATION-NOTES.md`](../crates/owlrl/INTEGRATION-NOTES.md).
- Working on the SPARQL HTTP surface? Read [`../crates/sparql/README.md`](../crates/sparql/README.md).

## Progressive discovery guidance for agents

1. Read this index first.
2. Pick the narrowest doc that matches the task.
3. Only then open the corresponding spec or crate notes.
4. If you add a new doc, give it a one-line summary here so future agents can find it without scanning the whole tree.

Keep this file short, current, and navigable.
