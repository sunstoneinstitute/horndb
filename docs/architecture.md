# HornDB Architecture

This document is the single-page map of HornDB's architecture: what each
subsystem is, how the pieces fit together, and — for every part — what
state it is actually in. It is synthesised from the authoritative SPECs
(`docs/specs/SPEC-00..10-*.md`) and their Stage-1 implementation plans
(`docs/plans/2026-05-24-*.md`).

For the canonical "why" read `docs/specs/SPEC-00-vision.md` first; for the
ground-truth gap list read `TASKS.md`. This document sits between them: the
SPECs say what *should* exist, `TASKS.md` tracks the work to close the gaps,
and the **Status** fields here say what exists *today*.

## How to read this document

Every architectural part carries a **Status** field with one of four values:

| Status | Meaning |
|---|---|
| **implemented** | Code exists and is exercised by tests and/or the conformance harness at Stage-1 level. |
| **specified** | A SPEC (and usually a plan) describes it, but there is no code yet. |
| **planned** | A concrete follow-up exists in `TASKS.md` to build or finish it. |
| **deferred** | Intentionally out of scope for now — a later roadmap stage, or indefinitely. |

A part can move only forward: specified → planned → implemented. "deferred"
is orthogonal — it marks a scope decision, not a progress point.

> **Maintenance:** the Status fields here and the checkboxes in `TASKS.md`
> are two views of the same reality and must be kept in sync. See
> [Keeping this document honest](#keeping-this-document-honest) and the rule
> in the root `CLAUDE.md`.

---

## 1. Vision and the differentiating bets

**Source:** `docs/specs/SPEC-00-vision.md` · **Status: implemented (Stage 1)**

HornDB is a hybrid forward/backward-chaining RDF reasoner targeting **OWL 2 RL**
semantics with a **SPARQL 1.1** frontend, built in Rust for unified-memory
hardware (HBM / CXL). The symbolic reasoner is the source of truth; ML is a
force multiplier, never the reasoner.

Six bets define the project. Their current state:

| # | Bet | Status | Notes |
|---|---|---|---|
| 1 | Hybrid execution (materialize the closure subset, backward-chain the rest with magic sets) | **partially implemented** | Forward materialization (SPEC-04) and GraphBLAS closure (SPEC-05) ship. Magic-sets / backward-chaining (SPEC-03 F4/F5, SPEC-07 backward mode) is **deferred**. |
| 2 | Unified-memory hardware as a first-class target (HBM/DDR5/CXL/NVMe) | **specified / deferred** | Tier API scaffolding exists in SPEC-02; GPU/CXL/NVMe specialization is SPEC-09, Stage 3. |
| 3 | DBSP-style incremental maintenance (Z-set deltas) | **partially implemented** | Insertion Z-set machinery ships (SPEC-06); **rule-path retraction across joins** works via recompute-and-diff on retraction-containing ticks (SPEC-06 F6, [#45](https://github.com/sunstoneinstitute/horndb/issues/45)), and **closure-path retraction** withdraws `ClosureInferred` rows whose base support is retracted ([#5](https://github.com/sunstoneinstitute/horndb/issues/5)). A fully delta-incremental retraction path (no affected-region recompute) remains **deferred**. |
| 4 | GraphBLAS for the closure subset | **implemented** | SuiteSparse:GraphBLAS backend ships (SPEC-05). |
| 5 | Soufflé-style ahead-of-time rule compilation (no interpreter) | **implemented** | `build.rs` codegen from `rules.toml` (SPEC-04). |
| 6 | Provenance / correctability as a hard requirement | **partially implemented** | Stage-1 ships per-triple `Provenance` and proof trees (SPEC-04 F4: `MemStore::proof_tree` / `Engine::proof`); production proof *persistence* (compressed side-table) is **planned**. |

**Non-goals (explicit, unchanged):** beating RDFox on pure single-node
materialization throughput; OWL 2 DL completeness; a rule-interpretation
engine; neural reasoning as source of truth; being a property-graph database.

---

## 2. Subsystem layering

Nine Rust crates under `crates/`, all `publish = false`, `edition = 2021`,
pinned to Rust `1.90.0`. Dependency / build order:

```
                          ┌──────────────┐   ┌──────────┐
                          │ harness (01) │   │  ml (08) │
                          └──────┬───────┘   └────┬─────┘
                                 │ grades         │ opt-in, advises
                                 ▼                ▼
        ┌──────────────────────────────────────────────────┐
        │                  sparql (07)                       │  public surface
        └───────────────────────┬────────────────────────────┘
                                 ▼
                        ┌─────────────────┐
                        │ incremental (06)│  Z-set deltas (insert-only)
                        └────────┬────────┘
                  ┌──────────────┴──────────────┐
                  ▼                              ▼
          ┌──────────────┐              ┌────────────────┐
          │  owlrl (04)  │  routes ───▶ │  closure (05)  │
          └──────┬───────┘  closure     └───────┬────────┘
                 ▼                               │
          ┌──────────────┐                       │
          │  wcoj (03)   │  join substrate       │
          └──────┬───────┘                       │
                 ▼                               ▼
        ┌──────────────────────────────────────────────────┐
        │                  storage (02)                      │  foundation
        └────────────────────────────────────────────────────┘

        hardware-ext (09): empty placeholder, Stage 3.
        python / rdflib API (10): partial — crates/python core surface; off-workspace.
```

Layering rule (SPEC-00): **the harness (SPEC-01) comes first** — the test
bench exists before the engine it grades. A SPEC is not satisfied until its
referenced subset in the harness is green; work may *grow* a subset but never
bypass it.

---

## 3. SPEC-01 — Conformance & benchmarking harness

**Crate:** `horndb-harness` · **Spec:** `SPEC-01` · **Overall status: implemented (Stage 1)**

The bench every other spec is graded against. Ships the `harness` binary with
two engines: `--engine stub` (plumbing) and `--engine owlrl` (real, needs
`--features real-engine`).

| Component | Status | Notes |
|---|---|---|
| W3C OWL 2 RL test-case runner (manifest parse, classify pass/fail/skip) | **implemented** | `runner.rs`, `manifest.rs`, `testcase.rs`. Suite keys: `owl2`, `owl2-w3c-rl`. |
| SPARQL 1.1 test runner | **implemented** | Suite key `sparql11`; path-based `[sparql_query]` consumed by `crates/sparql/tests/w3c_suite.rs`. |
| W3C RDF 1.2 N-Triples *syntax* suite | **implemented** | Suite key `rdf12-n-triples`; 4 positive + 6 negative cases via `oxttl::NTriplesParser`, no reasoner. |
| W3C SPARQL 1.1 *syntax* suite (query + update) | **implemented** | Suite key `sparql11-syntax` ([#110](https://github.com/sunstoneinstitute/horndb/issues/110), epic #10); `mf:*SyntaxTest11` types graded by `spargebra` (same parser as SPEC-07) via `TestKind::SparqlSyntax{Positive,Negative}`. 10 positive + 5 negative (5 update-form) curated checked-in cases under `tests/fixtures/sparql11-syntax/`; relative IRIs resolve against the action-file IRI; sub-ms, no network, no reasoner. |
| W3C OWL 2 RL test-suite ingestion pipeline | **implemented** | `owl2_rl_extract.rs` + `harness extract-owl2-rl`; 115 W3C cases → 99 green in `[suites.owl2-w3c-rl]`, 16 reds tracked in `harness/KNOWN-MANIFEST-BUGS.md` ([#160](https://github.com/sunstoneinstitute/horndb/issues/160) tracks the RL-reachable remainder — datatype value-space intersection landed, `owl:imports` external resolution remains). |
| Versioned selection manifest (`harness/selected.toml`) | **implemented** | Single canonical file at workspace root (manifest `[suites.*]` + `[sparql_query]`). |
| Result DB (SQLite) + trend reports (`harness report`) | **implemented** | `db.rs`, `report.rs`; state in `target/harness.sqlite`, JUnit at `target/junit.xml`. |
| Stub-engine smoke target | **implemented** | `stub.rs` (F12). |
| LUBM materialization RDFox A/B (`scripts/bench/compare-rdfox.sh --lubm N`) | **implemented (N=1)** | Identical TBox+ABox and rule set through both engines; closure-count parity gate + HornDB wall-clock cap. Parity is exact (delta 0, [#59](https://github.com/sunstoneinstitute/horndb/issues/59)). The 3× *timing* gate is still open and is **not** closure-bound — the gap is the SPEC-04 F5 `rdf:type`-partition scan ([#133](https://github.com/sunstoneinstitute/horndb/issues/133)). RDFox numbers internal-only (DeWitt). Status and numbers: `docs/benchmarks.md`. |
| LDBC SPB nightly throughput A/B (`.github/workflows/nightly.yml`) | **implemented (feasible scale)** | Per-run HornDB bring-up via `crates/harness/scripts/start-engine.sh` (serving the prepared flat closure, no reasoning); `harness spb-run` drives the SPB aggregation mix and records the full driver report to the trend DB. A/B references are **GraphDB Free 10.8.14** and **Oxigraph 0.5.9** (the latter a Rust/RocksDB SPARQL store with no reasoner — the closest architectural peer, serving the same flat closure), each brought up per run so no engine competes for RAM during another's measurement; each leg skips gracefully if that engine fails to start. The Oxigraph leg needs a one-time persisted-store load on the runner (`bootstrap-oxigraph-spb.sh`) before it records. Runs at *feasible scale* (512 k-triple SPB closure, aggregation-only); true SF=0.256 + editorial agents is a TASKS.md follow-up. Numbers: `docs/benchmarks.md`. |
| Full W3C OWL 2 + SPARQL 1.1 *evaluation* suites, ORE 2015, LDBC SPB SF3/SF5, LUBM-100/UOBM, broader RDFox A/B | **deferred** | SPEC-01 harness epic ([#10](https://github.com/sunstoneinstitute/horndb/issues/10)) **closed** after the Stage-1 core surface landed (OWL 2 RL ingestion + `owl2-w3c-rl`, RDF 1.2 N-Triples syntax, LUBM RDFox A/B at N=1, and the SPARQL 1.1 *syntax* suite `sparql11-syntax`, [#110](https://github.com/sunstoneinstitute/horndb/issues/110)). **Stage-2 deferred** (heavy external corpora needing large downloads / self-hosted runners): the SPARQL 1.1 *evaluation*/result-set suites, the full ORE 2015 corpus, LDBC SPB SF3/SF5 audited runs, LUBM-100/1000/8000 + UOBM at scale, and broader/published RDFox A/B (DeWitt license review). Scaffolding exists (`ore.rs`, `ldbc_spb.rs`). |

---

## 4. SPEC-02 — Storage & dictionary encoding

**Crate:** `horndb-storage` · **Spec:** `SPEC-02` · **Overall status: implemented (Stage-1 slice)**

Predicate-partitioned, columnar, dictionary-encoded triple store. The
foundation every other crate reads/writes through.

| Component | Status | Notes |
|---|---|---|
| Dictionary (URIs/blank nodes/literals → stable 64-bit ID, reverse lookup) | **implemented** | `dictionary.rs`, lock-free reads via `DashMap`. |
| Term taxonomy in high bits (`TermKind`, inline small literals) | **implemented** | `term.rs`. Includes `TripleTerm = 6` (RDF 1.2). |
| Predicate-partitioned columnar `(s_id, o_id)` storage | **implemented** | `partition.rs`, `store.rs`. |
| In-memory tiering scaffolding | **implemented** | `tier.rs`, `memory_tier.rs` — single warm tier in Stage 1. |
| N-Triples bulk loader (incl. RDF 1.2 `<<( s p o )>>` objects) | **implemented** | `loader/`; fixture `tests/fixtures/triple_term.nt`. |
| Six index orderings on demand (for hot predicates) | **implemented** | `ordering.rs`, `partition.rs` — object-major layout eager for hot predicates, lazy (`OnceLock`) for cold; `Store::scan_predicate_ordered` / `top_predicates`. [#16](https://github.com/sunstoneinstitute/horndb/issues/16) (SPEC-02 F4 + acceptance #6). |
| HDT-derived snapshot export/import (SPEC-02 F9) | **implemented** | `snapshot/` — default-graph export to a compact, front-coded + gap-coded format and re-import; round-trip is label-preserving (acceptance #5). Measured 5.440 B/triple on synthetic LUBM-shaped data (NF1 ≤6). Named-graph snapshots deferred (export errors on named-graph data); not rdfhdt wire-compatible. |
| HDT cold tier, CXL/NVMe tiering (placement) | **planned / deferred** | Cold-tier/tiering is Stage 2+; CXL/NVMe placement is SPEC-09 (Stage 3). |
| Copy-on-write snapshot isolation (concurrent-read / single-writer) | **implemented** | SPEC-02 #19: `Store::snapshot()` / `StoreSnapshot` pin a stable, internally-consistent read transaction over an immutable versioned `TierSnapshot`; `MemoryTier::insert_quad_batch` is copy-on-write (clone the top-level graph map, rebuild only affected graphs, bump version, atomically swap) so concurrent writers never disturb a pinned snapshot. The append-only dictionary keeps pinned term ids meaningful. HDT export reads one pinned snapshot, so a checkpoint under concurrent writes is internally consistent (NF5). `memory_tier.rs`, `store.rs`. |
| MVCC with per-tuple visibility | **deferred** | Stage 2; intersects SPEC-06. True per-tuple-visibility MVCC sits above the copy-on-write snapshot isolation row. |
| Persistent on-disk dictionary (Marisa-trie / FST) | **deferred** | Stage 2. |
| Turtle / N-Quads bulk-import paths (SPEC-02 F8) | **implemented** | `loader/turtle.rs`, `loader/nquads.rs` (streaming, via `oxttl`); N-Quads routes each quad to the graph named by its fourth term (F7), default-graph triples to the reserved sentinel. Shared `LoadStats`/`BATCH_SIZE`/`subject_to_term` hoisted to `loader/mod.rs`; N-Triples path unchanged. Fixtures `tests/fixtures/{tiny.ttl, with_literals.ttl, named_graphs.nq}`. [#18](https://github.com/sunstoneinstitute/horndb/issues/18). |
| HDT bulk-import path | **planned** | Tracked under SPEC-02 completeness ([#3](https://github.com/sunstoneinstitute/horndb/issues/3)); add when a consumer needs HDT ingest (export side ships, row above). |

> **Note:** SPEC-03's 4-cycle ≥10× performance gate was first hypothesised to
> be blocked here — that closing it needed a compressed columnar warm tier
> (SPEC-02 F1), not more executor tuning. [#15](https://github.com/sunstoneinstitute/horndb/issues/15)
> tested that with a compressed columnar `TripleSource` inside `horndb-wcoj`
> (7.5× smaller, WCOJ 0.73× → 1.11×) — directionally right but **not** ≥10×.
> The gate was finally closed in [#1](https://github.com/sunstoneinstitute/horndb/issues/1)
> by fixing the *graph shape*: the old uniform low-degree synthetic graph never
> forces the intermediate-result blow-up WCOJ needs. The canonical win case is
> a *skewed* graph (high-out-degree hubs + a thin closure), where a binary join
> must materialise a huge 3-path relation while WCOJ never does. See §5.

---

## 5. SPEC-03 — WCOJ query engine

**Crate:** `horndb-wcoj` · **Spec:** `SPEC-03` · **Overall status: implemented (Stage-1 slice)**

The join substrate all triple-pattern matching flows through. Leapfrog
Triejoin with a binary-hash fallback.

| Component | Status | Notes |
|---|---|---|
| Triple-pattern executor (variable bindings out) | **implemented** | `executor/wcoj.rs`. |
| Leapfrog Triejoin on n-way patterns | **implemented** | `trie/leapfrog.rs`, `trie/source_iter.rs`. |
| Binary hash-join fallback | **implemented** | `executor/binary_hash.rs`. |
| Generic-over-source executor (GAT, no `Box<dyn>` in hot path) | **implemented** | Removed vtable dispatch and per-prime allocations during the WCOJ perf pass. |
| Cardinality estimation + cost-based plan choice | **implemented** | `cardinality.rs`, `planner.rs`, `plan.rs`. |
| Cancellation (≤100 ms) | **implemented** | `cancel.rs`. |
| Correctness vs binary-join (differential fuzzer) | **implemented** | Repeated-pattern over-production bug fixed; fuzzer cases 16 → 256, `#[ignore]` removed. |
| 4-cycle ≥10× WCOJ-over-binary-join gate (acceptance #2) | **implemented** | Met in [#1](https://github.com/sunstoneinstitute/horndb/issues/1) by re-pointing `benches/four_cycle.rs` at the *canonical* WCOJ win case — a skewed ~10⁶-edge graph (`SyntheticGraph::skewed_four_cycle`: high-out-degree hubs + a thin, dedicated closure) instead of the old uniform low-degree graph, which never forces the intermediate-result blow-up WCOJ exists to avoid. Correctness pinned by `tests/skewed_four_cycle.rs` against an independent brute-force count. Measured ~34×; numbers in `docs/benchmarks.md`. |
| Magic-sets / demand transformation (F4) | **deferred** | `wcoj/src/lib.rs`: "Magic sets and SLG tabling are deferred." |
| SLG-resolution tabling (F5) | **deferred** | As above. Blocks SPEC-07 backward-chained mode. |
| GPU WCOJ kernels | **deferred** | SPEC-09, Stage 3. |

---

## 6. SPEC-04 — OWL 2 RL rule engine

**Crate:** `horndb-owlrl` · **Spec:** `SPEC-04` · **Overall status: implemented (Stage-1 slice)**

Forward-chaining engine. The OWL 2 RL/RDF rule set is **compiled** to native
Rust at build time from `rules.toml` (Soufflé-style) — no interpreter.

| Component | Status | Notes |
|---|---|---|
| Codegen pipeline (`build.rs` from `rules.toml`, `codegen/`) | **implemented** | Emits `fire_<id>` functions; see `INTEGRATION-NOTES.md`. |
| Semi-naïve evaluation with delta tables | **implemented** | `delta.rs`, `engine.rs`, `backend.rs`. |
| `Engine` satisfying the harness `Reasoner` trait | **implemented** | `integration.rs` (oxrdf dictionary over `MemStore`); closure backend is injectable via `Engine::with_backend(BackendChoice)` — default `RuleFiring`, optional GraphBLAS (`graphblas-backend` feature, [#61](https://github.com/sunstoneinstitute/horndb/issues/61)). Adapter in `harness/src/owlrl_engine.rs`. |
| Reset and rematerialize (F7) | **implemented** | Full re-materialization per `load`. |
| `owl:sameAs` routed to SPEC-05 EQREL (F6) | **implemented** | Rule engine does not re-derive `eq-sym`/`eq-trans`. |
| Subset of rules (`eq-rep-*`, common `prp-*`/`cls-*`/`cax-*`/`scm-*`, incl. `scm-eqc-rev`) | **implemented** | 98 W3C OWL 2 RL cases green. `scm-eqc-rev` derives `owl:equivalentClass` from two-way `rdfs:subClassOf`. Datatype value-space intersection narrowing of `rdfs:range` (`datatype_ranges.rs`) flips `I5.8-008/009-pe`. |
| `Provenance` side-table (F4) | **implemented** | `provenance.rs` — `struct Provenance { rule_id, premises }` recorded per derived triple; the basis of the proof tree (next row). |
| Proof recording (F4: `(rule_id, premises)` per derived triple → recursive proof tree) | **implemented** | Compiled + `list_rules.rs` rules record real body premises; `MemStore::proof_tree` / `Engine::proof` return a full proof tree bottoming out at asserted triples (`provenance.rs`, `integration.rs`; `tests/proof_tree.rs` covers NF4 depth + latency). Closure-backend nodes record empty premises by design; restriction-rule schema declarations are an elided side condition (instance premises still recorded). Production *persistence* (compressed side-table, on-demand re-derivation) remains Stage 2. |
| Datatype subsumption (`dt-type1` + `dt-type2` XSD lattice) | **implemented** | Load-time injection of `byte ⊑ short ⊑ int ⊑ ... ⊑ decimal` (and unsigned/non-negative arms); flips `I5.8-006-pe`/`I5.8-011-pe` green. |
| Max-cardinality (unqualified `cls-maxc1`/`cls-maxc2`, qualified `cls-maxqc1`–`cls-maxqc4`) | **implemented** | Hand-written in `list_rules.rs`; restriction literals (`owl:maxCardinality "0"`/`"1"`, and qualified `owl:maxQualifiedCardinality` + `owl:onClass`) classified at load time in `integration.rs`. `cls-maxc1`/`cls-maxqc1`/`cls-maxqc2` → `owl:Nothing` (inconsistency), `cls-maxc2`/`cls-maxqc3`/`cls-maxqc4` → `owl:sameAs`. The qualified rules ([#36](https://github.com/sunstoneinstitute/horndb/issues/36)) are covered by unit + integration tests; no `selected.toml` entry, because the only W3C qualified-cardinality case (`ObjectQCR-002-pe`) is blocked on fresh-bnode `owl:complementOf` generation, not on these rules. |
| Disjoint properties (`prp-pdw` pairwise, `prp-adp` list `owl:AllDisjointProperties`) | **implemented** | `prp-pdw` compiled from `rules.toml`; `prp-adp` ([#37](https://github.com/sunstoneinstitute/horndb/issues/37)) hand-written in `list_rules.rs` (list-walking analogue), both head `?u rdf:type owl:Nothing` on a shared `(u, w)` pair. Covered by unit + engine tests; the W3C `DisjointObjectProperties-*-cons` / `DisjointDataProperties-*-cons` cases in the selection exercise the no-false-fire path. The `*-pe` variants stay red on a DL `differentFrom`/`AllDifferent` entailment with no OWL 2 RL rule (`harness/KNOWN-MANIFEST-BUGS.md`). |
| Literal-value datatype rules (`dt-eq`/`dt-diff`/`dt-not-type`) | **implemented** | Load-time `inject_datatype_literal_axioms` (`integration.rs`) classifies each instance literal's value via `crates/owlrl/src/datatype_literals.rs` over the Stage-1 datatype set (XSD integer tower, `xsd:string`/`boolean`, plain/lang literals): value-equal ⇒ `owl:sameAs` (`dt-eq`, cross-lexical `1`≡`+1`≡`01` and cross-datatype `1`^^byte≡`1`^^integer), value-distinct (comparable) ⇒ `owl:differentFrom` (`dt-diff`), out-of-value-space lexical form ⇒ `owl:Nothing` (`dt-not-type`). Flips `#New-Feature-Keys-006-incons` green (issue #40). Disjoint value spaces (string vs integer) are never cross-compared; non-XSD/unhandled datatypes stay opaque (Stage-1 soundness). |
| Datatype value-space intersection (`I5.8-008/009-pe`) | **implemented** | Post-materialization pass `crates/owlrl/src/datatype_ranges.rs` (`derive_range_intersections`, wired in `integration.rs`): models each XSD numeric-tower datatype's value space as an integer interval, intersects the value spaces of a property's ≥2 *independent* (subset-incomparable) `rdfs:range` datatypes, and asserts `rdfs:range T` for every `T` whose value space is a superset of that intersection (supersets only ⇒ sound). Runs after the fixpoint so it composes with `scm-rng1`/`scm-rng2`-inferred ranges. Flips `I5.8-008/009-pe` green (issue #160). |
| `rdf:type` skew parallelism (F5) | **partially implemented (list-rule path; compiled-rule hotspot planned)** | The `rdf:type`-driven hand-written list rules (`cls-int1`, `cls-uni`, `cax-adc`, `prp-key`) partition their per-subject filtering by class id and parallelise it across rayon above `PAR_TYPE_THRESHOLD` (`crates/owlrl/src/list_rules.rs`), selected by `MaterializeOpts::parallel` (`ParallelStrategy::Auto` default; `Serial` is the oracle). Identical closure proven by `tests/rdf_type_skew_differential.rs` (3 large-extent fixtures + proptest); `benches/rdf_type_skew.rs` + `docs/benchmarks.md` record the win ([#39](https://github.com/sunstoneinstitute/horndb/issues/39)). The **compiled** (`cax-sco`-style) rules are the open hotspot, but profiling ([#133](https://github.com/sunstoneinstitute/horndb/issues/133)) shows the cost is an un-indexed full `rdf:type`-partition scan + naïve (non-delta) re-firing, **not** a parallelism gap. The two ranked fixes — a within-partition object index on `MemStore` (no `FireFn`/trait change) and genuine delta-driven semi-naïve firing (which does change the `FireFn` signature) — are specified in `docs/specs/SPEC-15-owlrl-type-index-seminaive.md`. |
| `eq-rep-p` predicate-position skew fix + always-relevant rule marking | **implemented** | Always-relevant marking via `wildcard_predicate`; semantics-preserving class-canonical path in `crates/owlrl/src/eq_rep_p_opt.rs` (union-find over `owl:sameAs`), default `EqRepPStrategy::Optimized`. Differential proptest `tests/eq_rep_p_differential.rs` proves identical closure to the naïve oracle. `TASKS.md` #2. Downstream F5 partition-by-class-id (row above) now implemented for the list-rule path. |
| User-defined rules (runtime Datalog frontend) | **deferred** | Stage 2 extension. |

---

## 7. SPEC-05 — GraphBLAS closure backend

**Crate:** `horndb-closure` · **Spec:** `SPEC-05` · **Overall status: implemented (Stage-1 slice)**

Handles the *closure subset* — transitive properties, `rdfs:subClassOf`,
`rdfs:subPropertyOf`, `owl:sameAs` — as semiring matrix algebra on
SuiteSparse:GraphBLAS. SPEC-04 routes those axioms here.

| Component | Status | Notes |
|---|---|---|
| SuiteSparse:GraphBLAS C-ABI integration (`build.rs` + bindgen, `links = "graphblas"`) | **implemented** | `ffi.rs`, `grb.rs`, `bindings.rs`. |
| Transitive closure via iterated `GrB_mxm` (`LOR_LAND_BOOL`) | **implemented** | `closure/transitive.rs`. |
| `rdfs:subClassOf` / `rdfs:subPropertyOf` schema closure | **implemented** | `closure/schema.rs`. |
| `owl:sameAs` equivalence classes (union-find / EQREL) | **implemented** | `sameas.rs`. |
| Dense renumbering cache (`dictionary_id ↔ dense_index`) | **implemented** | `dense_id.rs`. |
| Materialization writeback to storage (no rule re-fire) | **implemented** | `sink.rs`. |
| Wiring the GraphBLAS closure into the owlrl `Engine` (production replacement for `RuleFiringBackend`) | **implemented** | `crates/owlrl/src/graphblas_backend.rs` (`GraphBlasBackend`, `graphblas-backend` feature) computes `scm-sco`/`scm-spo`/`eq-sym`/`eq-trans`/`prp-trp` via strict `transitive_closure` over a dense `BoolMatrix`; injected via `Engine::with_backend(BackendChoice::GraphBlas)`. Differential parity with `RuleFiringBackend` in `crates/owlrl/tests/closure_backend_differential.rs`. Profiling ([#61](https://github.com/sunstoneinstitute/horndb/issues/61), `docs/benchmarks.md`) shows the swap is a decisive win only when closure dominates; the LUBM-shaped materialize cost is compiled-rule/`rdf:type`-scan bound ([#133](https://github.com/sunstoneinstitute/horndb/issues/133)), not closure-bound. |
| Vendored GraphBLAS as a git submodule (static, OpenMP, checked-in bindings) | **implemented** | `crates/closure/vendor/GraphBLAS` submodule `v10.3.0`; `vendored`+`openmp` default Cargo features (`regen-bindings` optional), statically linked (verified via `otool -L`), checked-in `src/bindings.rs`. CI checks out submodules and drops the from-source build. Supersedes the `[x]` "CI: install GraphBLAS on runners". |
| Shared, flock-guarded GraphBLAS build across worktrees | **implemented** | `build.rs` compiles the vendored GraphBLAS once per `(target, version)` into `crates/closure/vendor/.shared-build/<target>/<version>/` (anchored at the main worktree, gitignored), reused across git worktrees; concurrent builders serialise on an `fs4` advisory flock with the builder pid written in for diagnostics; CI caches the dir keyed on the submodule SHA. Details in `crates/closure/INTEGRATION-NOTES.md`. Narrows the disk-pressure concern (`TASKS.md` #13) to rocksdb. |
| Incremental closure updates (F6) — insertion + retraction | **implemented** | `closure/incremental.rs` (`IncrementalTransitiveClosure`) + `sink.rs` (`IncrementalClosureBackend`): a single-edge insert updates only the affected slice (backward-reach(s) × forward-reach(o)) and writes only the delta to the sink. **Deletion/retraction** (`delete_edge`/`delete_edges`/`delete_transitive_edges`) retains the asserted base edges alongside the closed set; retracting a base edge recomputes base-reachability over the affected source region and withdraws only the closure pairs no longer derivable over the post-delete base (invariant `closed == transitive_closure(base)`). Differential proptests vs GraphBLAS full closure (`tests/incremental.rs` insertion, `tests/incremental_retraction.rs` random insert/delete sequences). SPEC-06 owns the +/- sign; the SPEC-05 layer is sink-insertion-only and returns the withdrawn edges. Closure-path retraction delivered under [#5](https://github.com/sunstoneinstitute/horndb/issues/5) (insertion path [#42](https://github.com/sunstoneinstitute/horndb/issues/42)). |
| Valued closure / custom semirings (Sunstone annotated reasoning) — Fork A | **implemented** | Readiness metrics ([#11](https://github.com/sunstoneinstitute/horndb/issues/11)): `grb::ValuedMatrix` (FP64 `(max,×)` carrier, built-in + user-defined-op multiply) and `metrics::valued_transitive_closure` (N/nnz/density/iterations-to-fixpoint/per-iter frontier work/MxM share). **Fork A delivered** ([#12](https://github.com/sunstoneinstitute/horndb/issues/12)): `crosswalk::CrosswalkGraph` — build a weighted concept/entity adjacency from RDF 1.2 triple-term–annotated confidences (dictionary IDs → dense F7 renumbering) and resolve best-confidence crosswalk/propagation mappings in one built-in `(max,×)` closure instead of a SPARQL property-path crawl (`tests/crosswalk.rs`, `benches/crosswalk.rs` on a GTIO/SKOS-shaped graph). **Measured on `hornbench` (`docs/benchmarks.md`):** valued penalty a modest constant vs boolean; generic-kernel penalty for a scalar FP64 op ~1.0× → built-in semirings suffice for a scalar carrier and **PreJIT buys ≈0**. **Fork B (structured carrier / custom semiring) and PreJIT deferred** (SPEC-05 valued-reasoning addendum) until a use case needs a structured `(confidence, match-type, provenance)` carrier. |
| LAGraph adoption; GPU GraphBLAS backend | **deferred** | Stage 2 (LAGraph) / SPEC-09 Stage 3 (GPU). |

---

## 8. SPEC-06 — DBSP incremental maintenance

**Crate:** `horndb-incremental` · **Spec:** `SPEC-06` · **Overall status: implemented; rule-path and closure-path retraction (F6) landed**

Maintains the materialized closure under updates using DBSP / Z-set
semantics. Insertion is fully incremental; **rule-path retraction across
joins** ([#45](https://github.com/sunstoneinstitute/horndb/issues/45))
lands via recompute-and-diff on retraction-containing ticks, and
**closure-path retraction** ([#5](https://github.com/sunstoneinstitute/horndb/issues/5))
withdraws `ClosureInferred` rows whose base support is retracted, via the
deletion half of SPEC-05's incremental closure. A *fully delta-incremental*
retraction path (threading negative deltas without any affected-region
recompute) remains deferred.

| Component | Status | Notes |
|---|---|---|
| Z-set storage (`(triple, ±1)` multiplicity) | **implemented** | `zset.rs`. |
| Linear rule operator (single-pattern bodies) | **implemented** | `operator.rs`. |
| Bilinear rule operator (two-pattern bodies) | **implemented** | `operator.rs`, `circuit.rs`. |
| Change feed (`(triple, mult, time, derivation_kind)`) | **implemented** | `change_feed.rs`. |
| Checkpoint merge (collapse ±1 pairs) | **implemented** | `checkpoint.rs`, `delta_log.rs`. |
| Retraction semantics (F6) | **implemented (rule path)** | Recompute-and-diff on retraction-containing ticks: `circuit.rs` (`recompute_rule_closure`, `rule_attr`) recomputes the set-semantics rule closure of the post-delta base and diffs against prior rule-derived rows, publishing positive/negative `RuleInferred`. Order-independent and correct for arbitrary `(triple, ±k)`. Tests: `tests/retraction.rs` (acceptance #3 — insert 10K / retract 10K bit-identical) + tightened acceptance #4 (`tests/acceptance_differential.rs`, multiplicity equality over interleaved insert+retract). Increment [#45](https://github.com/sunstoneinstitute/horndb/issues/45) under epic [#6](https://github.com/sunstoneinstitute/horndb/issues/6). **Closure-path retraction and a fully delta-incremental retraction path stay deferred** (`FUTURE-WORK.md`). |
| Closure-operator deltas (F5) | **implemented (insertion + retraction)** | `closure_plan.rs` (`ClosureRule` / `TransitiveClosureRule`) + `circuit.rs` (`add_closure_plan`, closure pass): wraps SPEC-05's `IncrementalClosureBackend` ([#42](https://github.com/sunstoneinstitute/horndb/issues/42)), folds the asserted insertion delta into the retained per-predicate closure, emits only newly inferred triples tagged `ClosureInferred`. Differential proptest vs full recompute (`tests/closure_deltas_differential.rs`) ([#44](https://github.com/sunstoneinstitute/horndb/issues/44)). **Closure-path retraction** ([#5](https://github.com/sunstoneinstitute/horndb/issues/5)): `ClosureRule::apply_retract_delta` consumes the negative-only delta and `Circuit::tick` runs it before the rule recompute on retraction ticks, withdrawing a `ClosureInferred` row whose base support is gone (publishing a negative `ClosureInferred`) while preserving rows still rule-owned or otherwise supported (`tests/closure_retraction.rs`, updated `tests/retraction_closure.rs`). |
| MVCC for in-flight reads (F7) | **implemented (in-process)** | `Circuit::snapshot()` returns a refcounted `Snapshot` (`snapshot.rs`) pinning a consistent `(asserted ∪ derived)` view at a logical time: amortized-O(1) Arc-versioned acquire (the presence view is built lazily and cached — a tick invalidates it in O(1) so writes stay delta-sized, and the first acquire after a write pays one O(|asserted|+|derived|) build), stable across later ticks until dropped, readers and writers never block (NF4). Tests: `tests/snapshot.rs` (cross-tick pinning, overlapping independence, derived-row pinning, concurrent reader/writer). Increment [#46](https://github.com/sunstoneinstitute/horndb/issues/46) under epic [#6](https://github.com/sunstoneinstitute/horndb/issues/6). **Backing the snapshot interface onto SPEC-02 per-tuple storage MVCC stays deferred** (parent #6). |
| Distributed timely-dataflow | **deferred** | SPEC-09, Stage 3. |

---

## 9. SPEC-07 — SPARQL 1.1 frontend

**Crate:** `horndb-sparql` · **Spec:** `SPEC-07` · **Overall status: implemented (epic #7 closed)** — the SPARQL 1.1 query/update surface is delivered (SELECT/ASK/CONSTRUCT/DESCRIBE, full expression + aggregation surface, all property-path operators incl. recursive, pattern + graph-management Update on real storage, EXPLAIN). Remaining sub-features are Stage-2 and tracked as **deferred** rows below (GSP, backward-chaining, streaming, Turtle CONSTRUCT/DESCRIBE output). Full W3C conformance (acceptance #1/#2) gates on the harness epic ([#10](https://github.com/sunstoneinstitute/horndb/issues/10)), not on more frontend features.

The public query surface. Parser → algebra → planner → runtime, with an axum
HTTP server (`server` feature, on by default).

| Component | Status | Notes |
|---|---|---|
| Parser (spargebra) → AST | **implemented** | `parser.rs`. |
| Algebra translation (BGP, Join, LeftJoin, Filter, Project, Distinct, Slice, OrderBy, Union, Extend, Values) | **implemented** | `algebra/translate.rs`. All 14 runtime operator impls run native on id-carrying slot rows (`Slot`/`Row`/`Batch`) after Slice 2 of [#128](https://github.com/sunstoneinstitute/horndb/issues/128). `Join`/`LeftJoin` (`OPTIONAL`) are hash joins that now **stream their probe (left) side** ([#128](https://github.com/sunstoneinstitute/horndb/issues/128), `docs/specs/SPEC-20-join-probe-streaming.md`): the build (right) side is drained once into a `JoinState` index, the probe side is pulled chunk-by-chunk (`exec/runtime.rs` `probe_join_chunk`/`probe_left_join_chunk`), replacing the earlier drain-both `compute_join`/`compute_left_join` — ~linear in the common case (was a quadratic nested loop pre-#116/#141). Join keys are selected from the build side's actually-*bound* columns (`bound_join_vars`, replacing the schema-intersection `batch_join_vars`) so an all-unbound shared variable no longer degrades the probe toward O(\|l\|·\|r\|). `merge_rows_with` applies the slot compatibility rule (per-join column-index lookups hoisted to a once-per-join `build_merge_plan`); a required `Op::may_emit_term` static provenance claim + per-column `force_term_columns` preserve the stream-wide no-Id∧Term-mix invariant that `normalize_columns` (still used by `Union`, which drains both children) relied on for whole-batch joins. |
| Aggregation / `GROUP BY` (`COUNT`/`SUM`/`MIN`/`MAX`/`AVG`/`SAMPLE`/`GROUP_CONCAT`, `DISTINCT` modifiers) | **implemented** | `algebra/translate.rs` + `exec/runtime.rs::eval_group_native`. Unblocks the LDBC SPB aggregation mix (incl. the driver's `COUNT` warm-up query). #66. **Perf ([#128](https://github.com/sunstoneinstitute/horndb/issues/128) — Slice 1 + Slice 2 landed):** `eval_group_native` keys groups on raw-id `KeyPart`s (no per-row `TermId → String` decode); `COUNT(*)` is `members.len()` (zero column access); value aggregates decode the union of all aggregates' referenced columns once per group via `decode_subset`; the per-group key-slot row is moved, not cloned (#167). `DISTINCT` dedup hashes on `Vec<KeyPart>`. Probe-side join streaming + the bound-key probe fix **landed** (2026-07-06, `docs/plans/PLAN-20-01-join-probe-streaming.md`); remaining increments **planned** (see `TASKS.md` #128 entry): filter-aware/grouped/multi-count pushdown; HTTP result streaming. SPB-256 `aggregation-qps` progression and current gap vs GraphDB Free: `docs/benchmarks.md`. |
| `FILTER`/`BIND` expression coverage | **implemented (Stage-1 surface)** | Comparisons (incl. `<=`/`>=`), `IN`/`NOT IN`, boolean connectives, arithmetic, `IF`, `COALESCE`, and 30 builtins (string/regex/numeric/type-check/datetime accessors) over the best-effort f64 lexical model — `algebra/mod.rs::Func`, `exec/runtime.rs::eval_func`. `EXISTS`, non-deterministic builtins (`RAND`/`NOW`/`UUID`/…), hashing, `STRLANG`/`STRDT`, and custom functions still return `UnsupportedAlgebra`. #66. |
| `GRAPH` named-graph patterns | **implemented (Stage-1 merged-graph)** | Lower transparently to the inner pattern; a graph-name variable stays unbound. True named-graph scoping (zero solutions for absent graphs, per-graph `?g` bindings) is deferred to the named-graph epic (#7) — see `crates/sparql/INTEGRATION-NOTES.md`. #66. |
| `MINUS` | **planned** | `translate.rs` returns `UnsupportedAlgebra`. Part of the SPEC-07 umbrella (#7). |
| Planner + runtime executor | **implemented** | `plan/`, `exec/`. BGPs route to `exec/horn.rs::HornBackend`, which executes on `horndb-storage` (kind-tagged dictionary `TermId`s — fixes the Stage-1 lexical type-erasure/IRI-coercion) via the `horndb-wcoj` Leapfrog Triejoin (binary-hash for ≤3 patterns; WCOJ via `Planner::default()` for ≥4). `MemStore` (`exec/mem.rs`) is retained as the in-process test double. `DELETE DATA` is handled by a tombstone overlay over the insertion-only storage layer. `load_with_reasoning` (`reasoner` feature, default-on) runs the `horndb-owlrl` Engine (RuleFiring backend) and loads the full materialized closure directly into the backend, replacing the earlier dump-to-flat-file round trip. The `serve` binary accepts `--materialize` to trigger this path. (#67) **Perf ([#128](https://github.com/sunstoneinstitute/horndb/issues/128) — Slice 1 + Slice 2 landed):** `scan_bgp_ids` (`exec/horn.rs`) feeds the runtime id-carrying slot rows (`Slot`/`Row`/`Batch`) straight from the WCOJ `UInt64Array` columns — the dictionary is no longer defeated at this seam. `Runtime::run` decodes once at the boundary via `decode_term`. **All 13 operators are now native on slot rows** (Slice 2 ported the last six — LeftJoin, Union, OrderBy, Extend, Values, PathClosure); the `from_bindings`/`to_bindings` decode-adapter (`eval_rows`) and the `cfg(test)` `eval_legacy` differential oracle are removed — one slot runtime. Value-needing operators (`FILTER`/`ORDER BY`/`BIND`/aggregates) decode only their referenced columns on demand via `referenced_vars` + `decode_subset`; `ORDER BY`/`MIN`/`MAX`/relational comparisons always decode (ids are insertion-ordered, not value-ordered). The string `scan_bgp` is retained only as the default for non-`HornBackend` executors (DESCRIBE still adapts through it). **#143 Streaming pull-based runtime IMPLEMENTED** (2026-06-30): the runtime is now a pull-based, batch-at-a-time operator tree (`crates/sparql/src/exec/op/`); every Op is native; legacy materializing `eval` deleted; chunk-boundary invariance tested. **#144 Column pruning IMPLEMENTED** (`plan/pushdown.rs`). **#144 COUNT-over-BGP aggregate pushdown IMPLEMENTED** (`Executor::count_bgp` + `CountScan` + `CountScanOp`). **Join probe-side streaming + bound-key join-variable selection LANDED** (2026-07-06, `docs/plans/PLAN-20-01-join-probe-streaming.md`): `JoinOp`/`LeftJoinOp` drain only their build side and stream the probe side chunk-by-chunk; join keys come from `bound_join_vars`. **Planned:** count-pushdown extensions — equality-filter inlining, grouped COUNT via `Executor::count_bgp_grouped`, multi-count (`docs/plans/PLAN-21-01-count-pushdown-extensions.md`); result streaming to the HTTP layer (`docs/plans/PLAN-22-01-http-streaming-results.md`). SPB-256 numbers: `docs/benchmarks.md`. |
| SELECT / CONSTRUCT / ASK | **implemented** | Result formats in `results/`. |
| Entailment regimes: OWL 2 RL/RDF + simple | **implemented** | `regime/owl_rl.rs`, `regime/simple.rs` (materialized mode). |
| SPARQL Update `INSERT/DELETE DATA` | **implemented** | `update.rs`. |
| Pattern-based Update (`INSERT`/`DELETE … WHERE`, `DELETE WHERE`, `WITH/DELETE/INSERT … WHERE`) | **implemented** | `update.rs::apply_delete_insert`: evaluates the WHERE pattern via `translate_where` → planner → runtime, collects all solutions over the pre-update graph, then applies deletions-before-insertions (SPARQL 1.1 §3.1.3) through the `Store` seam. Ground-template safety drops triples with unbound slots; per-solution blank nodes are row-scoped. Default-graph only and single-op — named-graph templates and `USING`/`WITH <named>` are rejected (Stage-1 has one default graph); multi-op updates stay `UnsupportedForm`. ([#51](https://github.com/sunstoneinstitute/horndb/issues/51)) |
| Embedded HTTP server (`/query`, `/update`) | **implemented** | `server/` (axum), behind `server` feature. |
| trainmarks (DataTreehouse) end-to-end benchmark | **implemented** | All six trainmarks queries complete on `HornBackend` at all three scales (100K/1M/10M), no timeouts — `hornbench` baseline 2026-07-06 in `docs/benchmarks.md`. Native driver `crates/bench-trainmarks` + `scripts/bench/trainmarks.sh`. The original q4 `OPTIONAL` cliff (~231s@1M / TIMEOUT@10M) was removed by the hash `LeftJoin` ([#116](https://github.com/sunstoneinstitute/horndb/issues/116), [#128](https://github.com/sunstoneinstitute/horndb/issues/128) Slice 2): q4 now 0.334s@1M / 6.80s@10M. |
| RDF 1.2 triple-term patterns `<<( s p o )>>` | **implemented (gated)** | Accepted only when caller passes `SparqlConfig::rdf12()`; default rejects them so SPARQL 1.1 callers keep 1.1 semantics. `translate_query_with` / `execute_query_with`. |
| `DESCRIBE` query form | **implemented (partial)** | Forward one-level Concise Bounded Description: `translate.rs` lowers the describe pattern like SELECT, `exec/runtime.rs::describe_triples` emits each resource's outgoing triples. Recursive/symmetric blank-node CBD and typed-literal/Turtle serialisation deferred (Stage-1 `MemStore` erases term types on scan; tracked in [#57]). `TASKS.md` #48. |
| Non-recursive property paths (`/`, `^`, `\|`, `?`, `!`) | **implemented** | `translate.rs::translate_path` lowers them at translation time: `/`(Seq) and `^`(Inverse) expand into triple patterns; `\|`(Alternative) and `?`(ZeroOrOne) lower to `Union` (zero-length `?` binds endpoints without enumerating the graph — two distinct unbound endpoints are rejected as out of Stage-1 scope); `!`(NegatedPropertySet) lowers to a wildcard-predicate BGP under a `NOT IN` filter. A WHERE-pattern blank node (incl. the one spargebra mints when it flattens a sequence) is now treated as a non-distinguished join variable (`match_term`), which also fixes latent `/`-sequence joins across algebra boundaries. Covered by `tests/exec_property_paths.rs` and conformance fixtures `path-{alt,neg,opt}-001` (both backends). ([#49](https://github.com/sunstoneinstitute/horndb/issues/49)) |
| Kleene-star property paths (`*`, `+`) | **implemented** | `translate.rs::translate_closure_path` lowers `+`/`*` to the `Algebra::PathClosure` node (the inner one-step path is expanded over the hidden endpoint vars `?pp_src`/`?pp_dst`, so `(p\|q)+`, `^p+`, `(p/q)+` all work); `runtime.rs::eval_path_closure` materialises the edge relation, takes its transitive closure by BFS to a fixpoint (cycle-safe), and for `*` adds the reflexive pairs over the touched node set, then binds/filters against the query endpoints. Covered by `tests/exec_property_paths.rs` and conformance fixtures `path-{plus,star}-001` (both backends; `path-star-001` is the acceptance-#7 `subClassOf*` shape). **Deferred:** routing a materialised single-predicate closure through the SPEC-05 GraphBLAS backend + selectivity-based planner choice (F3 fast path — correctness ships now, acceleration later); strict full-graph node-set semantics for `*`'s zero-length match over nodes untouched by the path. ([#50](https://github.com/sunstoneinstitute/horndb/issues/50)) |
| Graph-management Update (`LOAD`/`CLEAR`/`DROP`/`CREATE`/`ADD`/`MOVE`/`COPY`) + multi-op updates | **implemented (Stage-1 default-graph)** | `update.rs`: parser now admits graph-management verbs and multi-operation sequences (`parser::ParsedUpdate::GraphManagement`); the executor walks the op list. Under the default-graph-only model: `CLEAR`/`DROP DEFAULT`/`ALL` clear the store (`Store::clear_all`, added to the seam — `MemStore` resets, `HornBackend` tombstones every live key); `LOAD <file:…> [INTO GRAPH …]` reads a `file:` source via `oxttl` (`.nt`/`.ttl`/`.nq`/`.trig`, default Turtle) and merges it into the default graph; `CREATE`, a named `CLEAR`/`DROP`/`LOAD INTO` target, and a named `ADD`/`MOVE`/`COPY` operand are errors unless `SILENT` (then no-ops). `ADD`/`MOVE`/`COPY` are spargebra-desugared to `Drop` + `DeleteInsert`; the same-graph identity case desugars to zero operations (valid no-op). Tests: `tests/update_graph_mgmt.rs` (both backends), `tests/server_http.rs` (`/update` CLEAR + LOAD). **Deferred:** true named-graph scoping (→ GSP [#54](https://github.com/sunstoneinstitute/horndb/issues/54)) and remote (`http(s):`) LOAD (no HTTP client dep). ([#52](https://github.com/sunstoneinstitute/horndb/issues/52)) |
| Backward-chained entailment mode (F4 second mode) | **deferred** | Depends on SPEC-03 magic-sets/tabling (deferred). |
| `EXPLAIN` pragma (F9) | **implemented** | `parser.rs` recognises a leading non-standard `EXPLAIN` / `EXPLAIN JSON` pragma (case-insensitive, whitespace-delimited so `?explainme` is not mistaken for it), strips it, and wraps the inner query as `ParsedQuery::Explain`. `api::execute_query` translates + plans the wrapped query **without executing it** and renders via `plan::explain` (`QueryAnswer::Explanation`): an indented operator tree (or JSON object tree) carrying a header `mode:` line (the entailment-regime execution mode — `materialized` today, labelled "backward-chaining not yet available" pending [#55](https://github.com/sunstoneinstitute/horndb/issues/55)) and per-node `~N rows` cardinality estimates. Estimates come from `Executor::cardinality_estimate` (new trait method, default `None`; `MemStore` returns the leading-pattern index size, `HornBackend` the live triple count as an upper bound) combined by textbook per-operator rules. The `/query` handler serves the rendering as `text/plain` or `application/json` by pragma. Satisfies acceptance #5 (EXPLAIN on `subClassOf+` shows mode + cardinality). Covered by `tests/explain_pragma.rs`, `tests/parser_basic.rs`, `tests/server_http.rs`, and `plan::explain` unit tests. **Deferred:** "chosen indexes" display (no cost-based index chooser yet — the plan is a 1:1 lowering) and the real materialized-vs-backward mode selection (with [#55](https://github.com/sunstoneinstitute/horndb/issues/55)). ([#53](https://github.com/sunstoneinstitute/horndb/issues/53)) |
| Graph Store Protocol | **deferred** | Stage-2. Direct REST access to *named* graphs, but the store is default-graph-only — named graphs are unrepresentable until SPEC-02 grows a quad-aware seam. Blocked on that storage work, not on the frontend. ([#54](https://github.com/sunstoneinstitute/horndb/issues/54), closed) |
| Streaming result serialization (F6 — currently buffers) | **planned** | The per-node buffering is gone (#143 streaming runtime), but `Runtime::run` still collects a full `Vec<Bindings>` and the serializers buffer the whole body. Planned 2026-07-06: `docs/specs/SPEC-22-http-streaming-results.md` + `docs/plans/PLAN-22-01-http-streaming-results.md` — `Runtime::run_stream` + incremental serializers + chunked HTTP bodies for all four SELECT formats. ([#56](https://github.com/sunstoneinstitute/horndb/issues/56), closed; tracked under [#128](https://github.com/sunstoneinstitute/horndb/issues/128)) |
| SPARQL 1.1 Federation (`SERVICE`) | **deferred** | Indefinitely. |

---

## 10. SPEC-08 — ML / LLM integration boundary

**Crate:** `horndb-ml` · **Spec:** `SPEC-08` · **Overall status: implemented (interfaces + HTTP boundary, opt-in)**

The boundary where ML sits. Symbolic reasoning is the source of truth; ML
proposes and advises. Disabling all ML must be bit-identical for correctness
(NF1). The whole crate is opt-in via configuration. The HTTP boundary
(`POST /nl-query`, `GET /ml-audit`) ships behind the off-by-default `server`
feature; the LLM is never bundled (reached via the `Translator` trait, mock-tested
hermetically). The one remaining piece is the real FAISS-backed candidate
generator (Stage-2, native-linkage heavy) — see `TASKS.md` / SPEC-08.

| Component | Status | Notes |
|---|---|---|
| `CandidateGenerator` trait (propose `sameAs` etc.) | **implemented** | `candidate.rs` — interface + reference scaffolding. |
| `PlanAdvisor` trait (cost/join-order hints) | **implemented** | `planner.rs`. |
| `HotSetAdvisor` trait (tier-placement hints) | **implemented** | `hotset.rs`. |
| Provenance for ML-derived facts (F5) | **implemented** | `provenance.rs`. |
| Model registry + config (`ml.enabled`) | **implemented** | `registry.rs`, `config.rs`. |
| LLM → SPARQL HTTP endpoint (`POST /nl-query`, F3) | **implemented** | `server/nlquery.rs` (`server` feature). `Translator`/`SparqlExecutor` traits in `nlquery.rs`; LLM never bundled, mock-tested (hermetic). Generated SPARQL always returned for audit. |
| HTTP audit endpoint (`GET /ml-audit`, F6) | **implemented** | `server/audit.rs` wraps the in-process `MlAuditLog`; paginated, `since`-filtered. |
| Cost reporting (token counts + est. USD) | **implemented** | `CostReport` surfaced in the `/nl-query` response. |
| Training-data leakage controls | **implemented** | `config::LlmPrivacy` — no-retention default, redaction option; single `loggable_text` chokepoint. |
| Real FAISS-backed `CandidateGenerator` | **planned** | Open increment under `TASKS.md` MEDIUM · *Completeness* — "SPEC-08 ML" (#8). Native FAISS linkage; separable from the HTTP boundary. |

---

## 11. SPEC-09 — Hardware specialization (Stage 3)

**Crate:** `horndb-hardware-ext` (empty placeholder) · **Spec:** `SPEC-09` · **Overall status: specified / deferred**

Roadmap, not an implementation contract. Stage 1 and Stage 2 must not depend
on it; Stage 3 begins only after Stage 2 acceptance passes.

| Component | Status |
|---|---|
| GPU/APU GraphBLAS closure backend | **deferred** (Stage 3) |
| GPU WCOJ kernels (cuMatch-style) | **deferred** (Stage 3) |
| CXL 2.0/3.0 warm-tier extension | **deferred** (Stage 3) |
| NVMe cold tier via GPUDirect Storage / BaM | **deferred** (Stage 3) |
| Multi-node distributed DBSP | **deferred** (Stage 3) |
| TPU / NPU / FPGA / custom silicon | **deferred** (indefinitely) |

---

## 12. SPEC-10 — rdflib-compatible Python API

**Crate:** `crates/python` (`horndb-python`) · **Spec:** `SPEC-10` ·
**Overall status: partially implemented**

A Python compatibility layer (PyO3/maturin) exposing rdflib-shaped term
classes, a `Graph` facade, core operations, parse/serialize, and SPARQL
passthrough to the Rust engine. The first increment ships the core
graph-centric surface; `docs/rdflib.md` compares common rdflib workflows with
the HornDB surface. Tracked as a MEDIUM *Completeness* epic in `TASKS.md`
(#9), split into shippable increments.

The binding crate is **excluded from the Cargo workspace** so the hermetic
`cargo build/clippy/test --workspace` never needs a Python interpreter; it is
built with maturin and exercised by a dedicated `python-rdflib-compat` CI job.

| Component | Status | Notes |
|---|---|---|
| rdflib-shaped terms (`URIRef`, `BNode`, `Literal`, `Variable`, `Namespace`) | **implemented** | SPEC-10 F1; differential-tested vs upstream rdflib. |
| `Graph` facade (add/remove/set/triples/subjects/objects/value/len/contains/iter) | **implemented** | F2. |
| `Dataset` / `ConjunctiveGraph` named-graph facades | **planned** | F3; Stage-1 store is default-graph only. |
| `parse` / `serialize` (Turtle, N-Triples) | **implemented** | F4; TriG/N-Quads/RDF-XML/JSON-LD deferred. |
| `query` / `update` passthrough to SPEC-07 | **implemented** | F5; SELECT/ASK/CONSTRUCT + INSERT/DELETE DATA. |
| Namespace binding (`bind`, `namespaces`, `Namespace`) | **implemented** | F6. |
| `rdflib-compat` differential subset | **implemented** | Acceptance #1/#2/#6; `crates/python/tests/`, `harness/curation/rdflib-compat.md`. |
| Multi-version CPython wheel matrix (macOS + Linux) | **planned** | Acceptance #7; one Linux CI job today. |

> The tracking epic (#9) is split into per-increment sub-issues as
> implementation lands; the first increment delivered the core surface above.

---

## 13. SPEC-11 — SSSOM mappings & crosswalk index

**Crate:** `crates/owlrl` (chain rules) + `crates/storage` (crosswalk index) ·
**Spec:** `SPEC-11` · **Overall status: partial / in progress**

First-class support for [SSSOM](https://mapping-commons.github.io/sssom/)
ontology crosswalks: mappings arrive as RDF from the external SoR (ADR-0016,
data-platform ADR-0002 — HornDB does **not** parse SSSOM/TSV in production),
their chain-rule closure is materialized by the compiled rule engine, and
query-time crosswalking is served from a compact, SIMD-friendly index over
sequential `TermId`s. `skos:exactMatch` is a crosswalk edge, **not** OWL
identity (ADR-0017). Tracked as a HIGH *Completeness* task in `TASKS.md`.

| Component | Status | Notes |
|---|---|---|
| Mapping-predicate vocabulary in `vocab.rs` (SKOS/OWL/semapv) | **implemented** | SPEC-11 F1. |
| Mapping representation (n-ary `sssom:Mapping` node + positive base triple; negated = n-ary only) | **partial** | F2; n-ary node builder exists, full materialization-on-inference is follow-up. RDF 1.2 deferred (ADR-0014, ADR-0002 D10). |
| SSSOM chaining rules in `rules.toml` (T1 / RCE1-2 / RI1-5 / RG1-2; transitive → closure) | **implemented** | F3; rides SPEC-04 codegen + SPEC-05 closure. RCE-N OWL rules already entailed by `cax-*`/`scm-*`. |
| Negative-mapping chaining (monotone, `Not` as distinct predicate) | **implemented** | F4; preserves SPEC-04 negation-free stratification. |
| Compact crosswalk index (rung-2 EF+FOR baseline → rung-4 PGM) | **planned** | F5; ~10 B/pair bidi target (NF2). |
| Crosswalk spine (designated sets always-resident; identity rides ADR-0007 spine) | **planned** | F6. |
| Confidence propagation along chains (product default; SeMRA) | **implemented** | F7. |
| Chain provenance (`derived_from` = proof premises) | **implemented** | F8; reuses SPEC-04 F4. |
| Harness SSSOM/TSV loader (bench/standalone only) | **implemented** | F9; not a production surface. |

> `skos:exactMatch` is deliberately kept out of OWL identity (ADR-0017) — the
> chain rules give crosswalk recall without `eq-rep-*` entailment pollution.

---

## 14. SPEC-12 — SIMD acceleration layer

**Crate:** new `horndb-simd` (zero-dep leaf) + consumers `crates/wcoj`,
`crates/storage`, `crates/owlrl` · **Spec:** `SPEC-12` · **Overall status: partially implemented** (primitives crate + WCOJ F1 seek/intersect consumer + storage F2 decode/scan consumer landed; F3 delta-apply still specified, gated on [#133](https://github.com/sunstoneinstitute/horndb/issues/133))

A single, shared, runtime-dispatched SIMD layer for the data-parallel hot loops:
`std::arch` intrinsics on stable Rust with cached-function-pointer dispatch
(AVX-512/AVX2 on the EPYC Zen4 reference host, NEON on Apple-Silicon dev Macs,
**scalar fallback always present as the correctness oracle**). Closes SPEC-03 NF1
(`per_tuple`) and the SIMD-friendly half of SPEC-02 NF2 (STREAM `rdf:type` scan).
Every kernel is differential-proven bit-identical to its scalar oracle. Tracked as a
HIGH *Performance* task in `TASKS.md`.

| Component | Status | Notes |
|---|---|---|
| `horndb-simd` primitives crate + scalar oracle + per-kernel differential proptests | **implemented** | F4+F5; new zero-dep leaf *below* `storage` (`simd → storage → wcoj → …`). Sole home for hand-written intrinsics. Ships six runtime-dispatched primitives (`lower_bound`, `intersect`, `merge`, `dedup`, `filter`/`filter_range`, `gather`) over `&[u64]`/`&[u32]`, each differential-proven bit-identical to the scalar oracle on every ISA path the host runs (`crates/simd/tests/differential.rs`), plus the `with_forced_isa` F5 override. AVX2/AVX-512 kernels for x86_64, NEON for aarch64; `merge` and `filter_range`'s AVX2 arm keep scalar-equivalent bodies until a bench earns intrinsics. Per-host kernel choice is the dispatch row's selection ladder; kernel bench numbers in `docs/benchmarks.md`. |
| WCOJ seek + leapfrog intersect SIMD | **implemented** | F1; highest payoff. Seek: `VecIter` builds a transient SoA `LevelColumn` for the **depth-0 (root, full-data) level** — built once, reused for the whole scan — and seeks it through `horndb_simd::lower_bound`; deeper levels stay on the scalar AoS `partition_point` to avoid an O(range) per-`open_level` column rebuild in the inner loop (a single bound predicate makes the depth-1 child range nearly the whole dataset). `PackedColumn::lower_bound` (compressed source) bisects to the owning block, decodes it, and SIMD-finishes (`source/soa.rs`, `source/vec_source.rs`, `source/packed_column.rs`). Intersect: both the standalone `LeapfrogJoin` (`trie/leapfrog.rs`) **and the production executor's inlined leapfrog** (`executor/wcoj.rs::BatchIter`) gain a k==2 fast path over `active_run` contiguous views via `horndb_simd::intersect` — when both contributing iters at a depth expose a run ≥ `SIMD_SEEK_MIN_RUN` (64), the whole pairwise intersection is precomputed once and drained, replacing per-candidate round-robin seeks. The SIMD *seek* path is likewise live in `BatchIter`. To honour the leapfrog's distinct-key contract, `active_run` now returns a **deduplicated** view (`LevelColumn::distinct_run`): the raw SoA column keeps duplicates for the seek index-mapping, but the intersect path consumes a cached distinct copy, so a subject with many objects still emits each key once. Output bit-identical to scalar — gated by the WCOJ differential fuzzer (narrow + a wide `N_WIDE > 64` variant that arms the intersect), the leapfrog BTreeSet oracle, and `tests/batchiter_simd.rs` (incl. the duplicate-subject hazard). `per_tuple` misses NF1 — the bottleneck is the depth-1 narrow-run leapfrog + Arrow materialization, not the intersect (`docs/benchmarks.md`). |
| Dictionary decode + `rdf:type` partition scan SIMD | **implemented (hornbench numbers recorded; scan meets SPEC-02 #4, decode misses NF4)** | F2; jointly satisfies SPEC-02 acceptance #4. `horndb-storage` consumes `horndb-simd`: bulk inline-int decode (`Dictionary::decode_inline_ints`/`lookup_inline_int_batch`/`lookup_batch`, the mask+cast unpack core) and a vectorised `rdf:type` partition scan (`PredicatePartition::subjects_with_object` via the new `horndb_simd::filter_indices_eq` scan+index-compact primitive composed with `gather`). New primitive is differential-proven equal to scalar on every host ISA path (`crates/simd/tests/differential.rs`); storage paths covered by `crates/storage` unit tests. **hornbench measured (2026-07-07, Ryzen 7 7700, node-0-pinned):** `partition_scan` **34.5 GB/s = ~104% of STREAM-Triad** → SPEC-02 acceptance #4 **met (GREEN)**; `dict_decode` AVX2 vs scalar **~1.01×** → NF4 ≥4× **not met (RED)**, the decode loop is load/store-bound so SIMD is not the lever (`docs/benchmarks.md`). |
| Delta-apply merge/dedup/sort SIMD | **specified (gated on [#133](https://github.com/sunstoneinstitute/horndb/issues/133))** | F3; needs hash-delta → sorted-run change first. The `cax-sco` partition-filter scan is **out of scope** — superseded by #133's object index + semi-naïve firing. |
| Runtime ISA dispatch (cached fn-ptr, `is_*_feature_detected!`, no nightly) | **implemented** | NF5; cached `OnceLock` fn-ptr per primitive, scalar-forced build green on stable 1.90. F5 `with_forced_isa` makes dispatch test-forceable so the differential suite exercises every host ISA path. **Kernel selection** resolves each primitive through the ladder `forced → HORNDB_SIMD_MAX_ISA cap → known-CPU table → representative-input calibration → static widest` (reworked 2026-07-01 after an LDBC SPB-256 A/B proved the previously-calibrated SIMD kernels net-harmful vs scalar on both measured hosts — a kernel microbench win does not imply a workload win). The known-CPU table (`cpu.rs`, CPUID-keyed, SPB-derived) pins scalar on both measured hosts; representative-input calibration (`HORNDB_SIMD_AUTOTUNE`) is the fallback for unlisted CPUs. Selected ISA + selection tier exported as the `horndb_simd_kernel_isa{kernel,isa,source}` gauge. Full ladder + knobs: `docs/architecture/simd.md`; measurements: `docs/benchmarks.md`. |

> SIMD accelerates loops that are already *algorithmically right*. It is **not** a
> substitute for the missing indexes/semi-naïve firing that dominate the SPEC-04
> materialize path — that is [#133](https://github.com/sunstoneinstitute/horndb/issues/133)
> (see §6), explicitly out of SPEC-12's scope.

---

## 15. Cross-cutting concerns

### Provenance / correctability
**Status: partially implemented.** Stage-1 ships per-triple `Provenance`
(`owlrl/src/provenance.rs`) and an ML-derived-fact provenance hook
(`ml/src/provenance.rs`). Proof trees (SPEC-04 F4) and proof retrieval
(NF4) are **implemented**: `MemStore::proof_tree` / `Engine::proof` build
a recursive proof bottoming out at asserted triples, within the NF4 100 ms
budget (`owlrl/tests/proof_tree.rs`). Production *persistence* of proofs
(compressed side-table, on-demand re-derivation) is **planned**
(`TASKS.md` SPEC-04 rules).

### RDF 1.2 (triple terms)
**Status: implemented end-to-end (Stage-1 surface).** We track W3C **RDF 1.2**,
not the community RDF-star extension. `TermKind::TripleTerm` in storage, the
N-Triples loader, gated SPARQL triple-term patterns, and the
`rdf12-n-triples` harness suite all ship. Turtle/TriG/N-Quads/semantics suites
remain **deferred** (`TASKS.md`, RDF 1.2 entries — both `[x]`). The OWL 2 RL
Stage-1 engine and W3C-manifest paths explicitly bail on triple-term inputs.

### Performance gates (docs/benchmarks.md)
**Status: partially implemented.** Per-subsystem targets and measured numbers
live in `docs/benchmarks.md`. SPEC-03's 4-cycle ≥10× gate is **met**
([#1](https://github.com/sunstoneinstitute/horndb/issues/1)). SPEC-03 NF1
(`per_tuple` ≤2.5 ns/tuple) and the SPEC-02 NF2 STREAM `rdf:type` scan are now
owned by **SPEC-12** (§14, the SIMD layer). Keep `docs/benchmarks.md` rows in
sync with the `TASKS.md` performance entries.

### Observability / metrics
**Status: implemented (Phase-1 Slice 1 + Phase-2 fan-out complete: owlrl + incremental + ml + wcoj + sparql-bytes slices); OTel traces/logs deferred to a later phase.** Metrics use
`prometheus-client` (typed `#[derive(EncodeLabelSet)]` labels, no strings) in a
foundational `horndb-metrics` crate that owns a process-global `OnceLock`
registry and the only `prometheus-client` dependency. Hot-path updates are
direct atomic ops on cached handles; quantities that are expensive to compute
(triple/dictionary/tier sizes) are pulled at scrape time via a `Collector`, not
maintained continuously. Slice 1 ships the SPARQL HTTP layer (request
count/latency/status + per-stage parse/translate/plan/exec timing +
query-kind counters), the closure backend (`ClosureMetrics` → histograms), and
storage sizes, exposed at `GET /metrics` (OpenMetrics text, behind the `server`
feature). OTel interop is achieved off-box by a collector scraping `/metrics`;
no in-process OTLP push. **Phase-2 Slice 1 (owlrl):** `OwlrlMetrics` subsystem
— per-rule fire counts (`horndb_owlrl_rule_fires_total{rule}`), per-rule +
per-phase latency histograms, `owlrl_triples_inferred_total`,
`owlrl_rounds_total`, dirty-predicate prune counters; closure `input_nnz`
observed alongside `output_nnz`; `storage_tier_bytes_estimated` now carries the
`tier` label (`MemTier` enum wired, `tier="unknown"` until full HBM/CXL
accounting lands). **Phase-2 Slice 2 (incremental):** `IncrementalMetrics`
subsystem — `horndb_incremental_tick_duration_seconds` histogram (per-tick
latency), `horndb_incremental_asserted_merged_total` /
`horndb_incremental_derived_merged_total` counters (merge work per tick),
`horndb_incremental_closure_withdraw_total` /
`horndb_incremental_closure_promote_total` counters (retraction/promotion),
`horndb_incremental_fixpoint_rounds` histogram (convergence depth); and
`horndb_incremental_change_feed_subscribers` gauge (maintained at subscribe +
publish-reap). **Phase-2 Slice 3 (ml):** `MlMetrics` subsystem (behind the
`server` feature of `horndb-ml`) — `horndb_ml_nl_query_total{result}` counter
(`result` ∈ `ok`/`error`); `horndb_ml_prompt_tokens_total`,
`horndb_ml_completion_tokens_total`, `horndb_ml_estimated_usd_total` counters
(from `CostJson`); `horndb_ml_translate_duration_seconds`,
`horndb_ml_execute_duration_seconds`, `horndb_ml_audit_query_duration_seconds`
histograms; `horndb-metrics` is an optional dep of `horndb-ml` gated on the
`server` feature. **Phase-2 Slice 4 (wcoj):** `WcojMetrics` subsystem — three
unlabelled histograms (`horndb_wcoj_seeks_per_query`,
`horndb_wcoj_iterations_per_query`, `horndb_wcoj_peak_iterators`) observed
exactly once per query in `impl Drop for BatchIter`; the inner loop only
increments plain `u64` struct fields (NO per-seek atomic/timing — strict §5.3
compliance). Whole-query granularity only. **Phase-2 Slice 5 (sparql-bytes):**
`horndb_sparql_request_bytes_total{endpoint}` and
`horndb_sparql_response_bytes_total{endpoint}` counters added to `SparqlMetrics`;
implemented via a `CountingBody` `http_body::Body` wrapper wired into the existing
`record_request` middleware — tallies data-frame bytes and observes once on
end-of-stream (exact, robust to streaming; not a `Content-Length` guess). Replaces
the permanently-zero series removed in Slice 1 (commit `d2cace9`). **Phase-2
fan-out is now complete** — no remaining Phase-2 fan-out items. **Deferred to a
later phase:** OTel traces and logs.
Design: `docs/specs/SPEC-17-metrics.md`.

### Build & CI split
**Status: implemented.** Pre-commit runs `cargo fmt --check` only; pre-push
runs workspace `clippy -D warnings` + `cargo build`. CI mirrors this plus a
real-engine conformance run. The closure crate needs SuiteSparse:GraphBLAS
locally (being moved to a vendored submodule — §7).

### Integration-test runner (cargo nextest)
**Status: implemented.** The workspace builds ~90 separate `crates/*/tests/*.rs`
binaries; cargo's built-in runner executes them serially per binary, which
dominated `cargo test --workspace` wall-clock. The standard runner is now
`cargo nextest run`, which schedules all tests across the binaries in one
concurrent pool — same test set, no source changes (locally ~40% faster on a
quiet machine; more under contention / in CI). Config: `.config/nextest.toml`
(`default` + `ci` profiles). nextest does not run doctests, so CI keeps a
separate `cargo test --workspace --doc` step (zero runnable doctests today).
Chosen over consolidating test files into fewer targets, which would touch test
source and risk dropping coverage for a smaller, riskier win ([#108](https://github.com/sunstoneinstitute/horndb/issues/108)).

---

## 16. Roadmap stages

| Stage | Scope | Status |
|---|---|---|
| **Stage 0** — Harness bootstrap | SPEC-01 minimal slice + CI gating | **implemented** |
| **Stage 1** — Feasibility prototype | SPEC-02/03/04 slices + SPEC-05/06/07 slices, ≥50-case W3C OWL 2 RL subset green | **implemented** (with open gaps tracked in `TASKS.md`) |
| **Stage 2** — MVP | Full SPEC-02..07, full W3C OWL 2 RL + SPARQL 1.1 entailment suites, ORE 2015, LDBC SPB SF3, RDF 1.2 priority | **planned / specified** |
| **Stage 3** — Hardware specialization | SPEC-09: GPU/CXL/NVMe/multi-node | **deferred** |

---

## Keeping this document honest

The Status fields above mirror the checkbox state in `TASKS.md`. They drift
apart the moment one is edited without the other. Two rules (also recorded in
the root `CLAUDE.md`):

1. **When you change `TASKS.md`** (check off, add, remove, or re-scope a task),
   update the matching **Status** field here in the same commit — e.g.
   checking off "SPEC-07 DESCRIBE" flips that row from **planned** to
   **implemented**.
2. **When you change a SPEC or plan** (`docs/specs/` or `docs/plans/`) such
   that the work-to-do changes, update `TASKS.md` in the same commit — add or
   re-scope the tracking task — and then reflect it here.

Source of truth for *intent* is the SPECs; for *outstanding work* it is
`TASKS.md`; for *current state* it is this document. When they disagree, the
code wins — fix whichever is stale.
