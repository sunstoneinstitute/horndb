# HornDB docs index

This is the human-facing entry point for the docs directory, and the first stop for coding agents using progressive discovery.

## Start here

- [`../README.md`](../README.md) — project overview, status, architecture, and build/test commands.
- [`specs/README.md`](specs/README.md) — index of the authoritative SPEC documents.
- [`specs/AGENTS.md`](specs/AGENTS.md) and [`plans/AGENTS.md`](plans/AGENTS.md) — naming and frontmatter rules: specs are `SPEC-NN-<slug>.md`, plans are `PLAN-NN-MM-<slug>.md` (`NN` = origin spec, `00` if none), both with `status:` / `date:` / `scope:` frontmatter.
- [`adr/README.md`](adr/README.md) — Architecture Decision Records: the *why* behind the cross-cutting choices (the six SPEC-00 bets plus major tech decisions).
- [`../TASKS.md`](../TASKS.md) — live follow-up list and current gaps.
- [`benchmarks.md`](benchmarks.md) — performance targets, baselines, current measured results, and reproduction commands.

## Docs in this directory

- [`architecture.md`](architecture.md) — single-page architecture map across all SPECs, with a **Status** field (implemented / specified / planned / deferred) per subsystem and feature. This is the detailed, kept-current status record — read it, not this index, for plan history, issue numbers, and bench numbers. Kept in sync with `../TASKS.md`.
- [`architecture/`](architecture/wcoj.md) — per-subsystem deep-dive guides: [`wcoj.md`](architecture/wcoj.md) (Leapfrog Triejoin internals) and [`simd.md`](architecture/simd.md) (per-host SIMD kernel selection). Read before touching either subsystem's internals.
- [`adr/`](adr/README.md) — Architecture Decision Records (18 so far), one decision per file. Read for the rationale behind a choice; `architecture.md` has current status.
- [`metrics.md`](metrics.md) — inventory of every metric and label HornDB exposes, one row per series. Read [`specs/SPEC-17-metrics.md`](specs/SPEC-17-metrics.md) for the *why*; use the `horndb-perftest-with-metrics` skill to map a symptom onto these metrics.
- [`writing-style.md`](writing-style.md) — house style for the **published** docs (`docs/ref/` + `docs/guides/` → `horndb.io/docs/`). Read before writing or editing a published page.
- Published docs tree — [`index.qmd`](index.qmd), [`ref/`](ref/index.qmd), [`guides/`](guides/index.qmd) are the only trees published to `horndb.io/docs/`, one Quarto project rooted at the repo root ([`../_quarto.yml`](../_quarto.yml)). Publish set: [`publish.toml`](publish.toml). Build with `quarto render` from the repo root.
- [`rdflib.md`](rdflib.md) — how HornDB relates to `rdflib`: the Python binding (`crates/python/`) is the actual compatibility surface; there is no rdflib-shaped Rust API.
- [`research/maplib.md`](research/maplib.md) — comparison with DataTreehouse's maplib (SPARQL-on-Polars). Read before query-execution or closure work.
- [`research/landscape.md`](research/landscape.md) — competitive landscape for provenance + symbolic reasoning. Read before scoping verifiable-justification work (SPEC-04/SPEC-08).
- [`research/optimizer-sota.md`](research/optimizer-sota.md) — cited state of the art behind SPEC-23's cardinality estimator and cost-based join planner. Read before touching the `Stats` seam, the estimator, or cost-based ordering.
- [`research/negative-filters-for-jit-materialization.md`](research/negative-filters-for-jit-materialization.md) — survey of filters that reduce JIT materialization at query time. Read before reducing materialization or adding an approximate index to the reasoning path (SPEC-04/05/07).
- [`ideas/initial-research.md`](ideas/initial-research.md) — the original feasibility study the SPECs were derived from. Historical.
- [`ideas/silicondb-horndb-claim-layer.md`](ideas/silicondb-horndb-claim-layer.md) — design sketch for a probabilistic claim layer with HornDB certification.
- [`specs/SPEC-23-unified-ir.md`](specs/SPEC-23-unified-ir.md) — the Stage-2 flagship: a single logical IR for query **and** reasoning. Read before turning the planner stubs into a real optimizer. See `architecture.md`'s [Stage-2 investment epics](architecture.md#stage-2-investment-epics) (E1) for current status.

## Where to go next

Each entry names the spec (and crate notes) to read; current implementation status, plan history, issue links, and bench numbers live in `architecture.md` (section noted) or the `TASKS.md`/epics table — not here.

- Query/update behavior → [`specs/SPEC-07-sparql-frontend.md`](specs/SPEC-07-sparql-frontend.md); status in `architecture.md` §9 (also covers named graphs/`GRAPH`/GSP under SPEC-28, reasoning scope under SPEC-29, and the change-feed materializer under SPEC-30 — all three are rows inside §9).
- Named graphs, `GRAPH`, datasets, or the Graph Store Protocol → [`specs/SPEC-28-named-graph-dataset-semantics.md`](specs/SPEC-28-named-graph-dataset-semantics.md); status in `architecture.md` §9.
- Reasoning across many named graphs (reasoning views) → [`specs/SPEC-29-named-graph-reasoning-scope.md`](specs/SPEC-29-named-graph-reasoning-scope.md); status in `architecture.md` §9.
- HornDB as a consumer of an external change feed → [`specs/SPEC-30-change-feed-materializer.md`](specs/SPEC-30-change-feed-materializer.md); status in `architecture.md` §9.
- Python bindings or rdflib compatibility → [`specs/SPEC-10-rdflib-compatible-python-api.md`](specs/SPEC-10-rdflib-compatible-python-api.md), [`rdflib.md`](rdflib.md), and `crates/python/` (CLAUDE.md + README); status in `architecture.md` §12.
- Storage or triple access → [`specs/SPEC-02-storage.md`](specs/SPEC-02-storage.md) and [`../crates/storage/INTEGRATION-NOTES.md`](../crates/storage/INTEGRATION-NOTES.md); Stage 2 is [`specs/SPEC-25-storage-stage2.md`](specs/SPEC-25-storage-stage2.md) (epic E3). Status in `architecture.md` §4 and the [Stage-2 investment epics](architecture.md#stage-2-investment-epics) table.
- Reasoning or rule behavior → [`specs/SPEC-04-rule-engine.md`](specs/SPEC-04-rule-engine.md) and [`../crates/owlrl/INTEGRATION-NOTES.md`](../crates/owlrl/INTEGRATION-NOTES.md); planned work in [`specs/SPEC-15-owlrl-type-index-seminaive.md`](specs/SPEC-15-owlrl-type-index-seminaive.md). Status in `architecture.md` §6.
- SSSOM mappings or ontology crosswalks → [`specs/SPEC-11-mappings.md`](specs/SPEC-11-mappings.md) and [`../crates/owlrl/INTEGRATION-NOTES.md`](../crates/owlrl/INTEGRATION-NOTES.md); conformance subset [`../harness/curation/sssom-mappings.md`](../harness/curation/sssom-mappings.md). Status in `architecture.md` §13.
- Exposing proofs/provenance to users → [`specs/SPEC-27-provenance-as-a-queryable-view.md`](specs/SPEC-27-provenance-as-a-queryable-view.md). Status: draft, no plan yet.
- Incremental maintenance (deltas, retraction, snapshots) → [`specs/SPEC-06-incremental-maintenance.md`](specs/SPEC-06-incremental-maintenance.md) and [`../crates/incremental/FUTURE-WORK.md`](../crates/incremental/FUTURE-WORK.md); Stage 2 is [`specs/SPEC-24-incremental-stage2.md`](specs/SPEC-24-incremental-stage2.md) (epic E2). Status in `architecture.md` §8 and the [Stage-2 investment epics](architecture.md#stage-2-investment-epics) table.
- SIMD / vectorized hot loops → [`specs/SPEC-12-simd.md`](specs/SPEC-12-simd.md); status in `architecture.md` §14.
- The WCOJ / join executor → [`architecture/wcoj.md`](architecture/wcoj.md) first, then [`specs/SPEC-03-query-engine.md`](specs/SPEC-03-query-engine.md) and the crate notes ([`../crates/wcoj/CLAUDE.md`](../crates/wcoj/CLAUDE.md) + `INTEGRATION-NOTES.md`).
- The SPARQL HTTP surface → [`../crates/sparql/README.md`](../crates/sparql/README.md).
- Operator configuration (config files, live reload, per-query settings) → [`specs/SPEC-26-config-system.md`](specs/SPEC-26-config-system.md); status in `architecture.md` §15.
- Observability / metrics → [`specs/SPEC-17-metrics.md`](specs/SPEC-17-metrics.md) and [`metrics.md`](metrics.md); status in `architecture.md` §16.

## Progressive discovery guidance for agents

1. Read this index first.
2. Pick the narrowest doc that matches the task.
3. Only then open the corresponding spec or crate notes.
4. If you add a new doc, give it a one-line summary here so future agents can find it without scanning the whole tree.

Keep this file short, current, and navigable.
