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
| 3 | DBSP-style incremental maintenance (Z-set deltas) | **partially implemented** | Insertion-only Z-set machinery ships (SPEC-06); retraction is **deferred**. |
| 4 | GraphBLAS for the closure subset | **implemented** | SuiteSparse:GraphBLAS backend ships (SPEC-05). |
| 5 | Soufflé-style ahead-of-time rule compilation (no interpreter) | **implemented** | `build.rs` codegen from `rules.toml` (SPEC-04). |
| 6 | Provenance / correctability as a hard requirement | **partially implemented** | Stage-1 ships a stub `Provenance`; production proof recording (SPEC-04 F4) is **planned**. |

**Non-goals (explicit, unchanged):** beating RDFox on pure single-node
materialization throughput; OWL 2 DL completeness; a rule-interpretation
engine; neural reasoning as source of truth; being a property-graph database.

---

## 2. Subsystem layering

Nine Rust crates under `crates/`, all `publish = false`, `edition = 2021`,
pinned to Rust `1.88.0`. Dependency / build order:

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
        python / rdflib API (10): planned, no crate yet.
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
| W3C OWL 2 RL test-suite ingestion pipeline | **implemented** | `owl2_rl_extract.rs` + `harness extract-owl2-rl`; 91 W3C cases → 78 green in `[suites.owl2-w3c-rl]`, reds tracked in `harness/KNOWN-MANIFEST-BUGS.md`. |
| Versioned selection manifest (`harness/selected.toml`) | **implemented** | Single canonical file at workspace root (manifest `[suites.*]` + `[sparql_query]`). |
| Result DB (SQLite) + trend reports (`harness report`) | **implemented** | `db.rs`, `report.rs`; state in `target/harness.sqlite`, JUnit at `target/junit.xml`. |
| Stub-engine smoke target | **implemented** | `stub.rs` (F12). |
| Full W3C OWL 2 + SPARQL 1.1 suites, ORE 2015, LDBC SPB SF3/SF5, LUBM/UOBM, RDFox A/B | **planned** | `TASKS.md` MEDIUM · *Conformance* — "SPEC-01 harness". Scaffolding exists (`ore.rs`, `ldbc_spb.rs`); full corpora not wired. RDFox comparison gated on license review. |

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
| Six index orderings on demand (for hot predicates) | **planned** | `TASKS.md` MEDIUM · *Completeness* — "SPEC-02 storage". |
| HDT cold tier, CXL/NVMe tiering, snapshot HDT export | **planned / deferred** | Cold-tier/tiering is Stage 2+; CXL/NVMe placement is SPEC-09 (Stage 3). |
| MVCC with per-tuple visibility (Stage 1 uses copy-on-write snapshots) | **deferred** | Stage 2; intersects SPEC-06. |
| Persistent on-disk dictionary (Marisa-trie / FST) | **deferred** | Stage 2. |
| Turtle / N-Quads / HDT bulk-import paths | **planned** | Tracked under SPEC-02 completeness; add when a consumer needs them. |

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
| 4-cycle ≥10× WCOJ-over-binary-join gate (acceptance #2) | **implemented** | Met in [#1](https://github.com/sunstoneinstitute/horndb/issues/1) by re-pointing `benches/four_cycle.rs` at the *canonical* WCOJ win case — a skewed ~10⁶-edge graph (`SyntheticGraph::skewed_four_cycle`: high-out-degree hubs + a thin, dedicated closure) instead of the old uniform low-degree graph. The binary-hash join materialises the full `#2-paths · hub_out ≈ 3.2·10⁷` 3-path relation over every source; WCOJ binds `[a,b,c,d]` depth-first and never materialises an intermediate — the cycle-closing intersection `out(c) ∩ in(a)` is empty for almost every `(a,b,c)` prefix, so it backtracks in O(1) without expanding the hubs (a ≈`hub_out` advantage). Measured (macOS dev workstation): WCOJ **0.55 s** vs binary-hash **18.8 s** → **~34× faster**. Correctness pinned by `tests/skewed_four_cycle.rs` against an independent brute-force count. Earlier compression work ([#15](https://github.com/sunstoneinstitute/horndb/issues/15)) was a partial lever (1.11×) but the gap was workload shape, not bandwidth. |
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
| `Engine` satisfying the harness `Reasoner` trait | **implemented** | `integration.rs` (oxrdf dictionary over `MemStore` + `RuleFiringBackend`), adapter in `harness/src/owlrl_engine.rs`. |
| Reset and rematerialize (F7) | **implemented** | Full re-materialization per `load`. |
| `owl:sameAs` routed to SPEC-05 EQREL (F6) | **implemented** | Rule engine does not re-derive `eq-sym`/`eq-trans`. |
| Subset of rules (`eq-rep-*`, common `prp-*`/`cls-*`/`cax-*`/`scm-*`) | **implemented** | 78 W3C OWL 2 RL cases green. |
| Stub `Provenance` (F4 placeholder) | **implemented** | `provenance.rs` — `struct Provenance`, not yet a production proof tree. |
| Production proof recording (F4: `(rule_id, premise_ids[])`, on-demand re-derivation) | **planned** | `TASKS.md` MEDIUM · *Completeness* — "SPEC-04 rules". |
| Full `dt-*` datatype rules, `cls-int*` / `cls-uni*` list-walking rules | **planned** | As above. |
| `rdf:type` skew parallelism (F5) | **planned** | As above. |
| `eq-rep-p` predicate-position skew fix + always-relevant rule marking | **implemented** | Always-relevant marking via `wildcard_predicate`; semantics-preserving class-canonical path in `crates/owlrl/src/eq_rep_p_opt.rs` (union-find over `owl:sameAs`), default `EqRepPStrategy::Optimized`. Differential proptest `tests/eq_rep_p_differential.rs` proves identical closure to the naïve oracle. `TASKS.md` #2. Downstream F5 partition-by-class-id (row above) still planned. |
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
| Vendored GraphBLAS as a git submodule (static, OpenMP, checked-in bindings) | **implemented** | `crates/closure/vendor/GraphBLAS` submodule `v10.3.0`; `vendored`+`openmp` default Cargo features (`regen-bindings` optional), statically linked (verified via `otool -L`), checked-in `src/bindings.rs`. CI checks out submodules and drops the from-source build. Supersedes the `[x]` "CI: install GraphBLAS on runners". |
| Incremental closure updates (F6) | **planned** | `TASKS.md` MEDIUM · *Completeness* — "SPEC-05 closure"; needs SPEC-06 closure deltas. |
| Valued closure / custom semirings (Sunstone annotated reasoning) | **planned** | `TASKS.md` MEDIUM · *Performance* (two entries): readiness metrics first, then Fork A (scalar, built-in semirings) → Fork B (structured carrier) → PreJIT. Spec addendum gated on the metrics. |
| LAGraph adoption; GPU GraphBLAS backend | **deferred** | Stage 2 (LAGraph) / SPEC-09 Stage 3 (GPU). |

---

## 8. SPEC-06 — DBSP incremental maintenance

**Crate:** `horndb-incremental` · **Spec:** `SPEC-06` · **Overall status: implemented (insertion-only)**

Maintains the materialized closure under updates using DBSP / Z-set
semantics. **Insertion-only at Stage 1** — the highest-risk spec.

| Component | Status | Notes |
|---|---|---|
| Z-set storage (`(triple, ±1)` multiplicity) | **implemented** | `zset.rs`. |
| Linear rule operator (single-pattern bodies) | **implemented** | `operator.rs`. |
| Bilinear rule operator (two-pattern bodies) | **implemented** | `operator.rs`, `circuit.rs`. |
| Change feed (`(triple, mult, time, derivation_kind)`) | **implemented** | `change_feed.rs`. |
| Checkpoint merge (collapse ±1 pairs) | **implemented** | `checkpoint.rs`, `delta_log.rs`. |
| Retraction semantics (F6) | **deferred** | `TASKS.md` MEDIUM · *Completeness* — "SPEC-06 incremental". Insertion only at Stage 1 (`FUTURE-WORK.md`). |
| Closure-operator deltas (F5) | **planned** | Pairs with SPEC-05 incremental closure. |
| MVCC for in-flight reads | **deferred** | Stage 2. |
| Distributed timely-dataflow | **deferred** | SPEC-09, Stage 3. |

---

## 9. SPEC-07 — SPARQL 1.1 frontend

**Crate:** `horndb-sparql` · **Spec:** `SPEC-07` · **Overall status: implemented (Stage-1 slice)**

The public query surface. Parser → algebra → planner → runtime, with an axum
HTTP server (`server` feature, on by default).

| Component | Status | Notes |
|---|---|---|
| Parser (spargebra) → AST | **implemented** | `parser.rs`. |
| Algebra translation (BGP, Join, LeftJoin, Filter, Project, Distinct, Slice, Group, OrderBy, Union, Minus) | **implemented** | `algebra/`. |
| Planner + runtime executor | **implemented** | `plan/`, `exec/`. BGPs route to the WCOJ executor. |
| SELECT / CONSTRUCT / ASK | **implemented** | Result formats in `results/`. |
| Entailment regimes: OWL 2 RL/RDF + simple | **implemented** | `regime/owl_rl.rs`, `regime/simple.rs` (materialized mode). |
| SPARQL Update `INSERT/DELETE DATA` | **implemented** | `update.rs`. |
| Embedded HTTP server (`/query`, `/update`) | **implemented** | `server/` (axum), behind `server` feature. |
| RDF 1.2 triple-term patterns `<<( s p o )>>` | **implemented (gated)** | Accepted only when caller passes `SparqlConfig::rdf12()`; default rejects them so SPARQL 1.1 callers keep 1.1 semantics. `translate_query_with` / `execute_query_with`. |
| `DESCRIBE` query form | **planned** | `translate.rs` returns `UnsupportedAlgebra("DESCRIBE")`. `TASKS.md` MEDIUM · *Completeness* — "SPEC-07 SPARQL". |
| Kleene-star property paths (`*`, `+`) | **planned** | `UnsupportedPathOp`; needs closure-on-demand or backward chaining. Same task. |
| Full Update vocabulary (`LOAD` / `CLEAR` / `DROP`) | **planned** | Same task. |
| Backward-chained entailment mode (F4 second mode) | **deferred** | Depends on SPEC-03 magic-sets/tabling (deferred). |
| `EXPLAIN` pragma; Graph Store Protocol | **planned** | Same task. |
| Streaming result serialization (F6 — currently buffers) | **planned** | Same task. |
| SPARQL 1.1 Federation (`SERVICE`) | **deferred** | Indefinitely. |

---

## 10. SPEC-08 — ML / LLM integration boundary

**Crate:** `horndb-ml` · **Spec:** `SPEC-08` · **Overall status: implemented (interfaces only, opt-in)**

The boundary where ML sits. Symbolic reasoning is the source of truth; ML
proposes and advises. Disabling all ML must be bit-identical for correctness
(NF1). The whole crate is opt-in via configuration.

| Component | Status | Notes |
|---|---|---|
| `CandidateGenerator` trait (propose `sameAs` etc.) | **implemented** | `candidate.rs` — interface + reference scaffolding. |
| `PlanAdvisor` trait (cost/join-order hints) | **implemented** | `planner.rs`. |
| `HotSetAdvisor` trait (tier-placement hints) | **implemented** | `hotset.rs`. |
| Provenance for ML-derived facts (F5) | **implemented** | `provenance.rs`. |
| Model registry + config (`ml.enabled`) | **implemented** | `registry.rs`, `config.rs`. |
| LLM → SPARQL HTTP endpoint (`POST /nl-query`, F3) | **planned** | `TASKS.md` MEDIUM · *Completeness* — "SPEC-08 ML". |
| Real FAISS-backed `CandidateGenerator` | **planned** | Same task. |
| HTTP audit endpoint (`GET /ml-audit`, F6) + cost reporting | **planned** | Same task; `audit.rs` holds the in-process side. |
| Training-data leakage controls | **planned** | Same task. |

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

**Crate:** none yet · **Spec:** `SPEC-10` · **Overall status: planned**

A Python compatibility layer (PyO3/maturin) exposing rdflib-shaped term
classes, `Graph`/`Dataset`, core operations, and SPARQL passthrough to the
Rust engine. No code exists today; `docs/rdflib.md` compares common rdflib
workflows with the current HornDB surface. Tracked as a single task in
`TASKS.md` MEDIUM · *Completeness* — "SPEC-10 rdflib-compatible Python API".

| Component | Status | Notes |
|---|---|---|
| rdflib-shaped terms (`URIRef`, `BNode`, `Literal`, `Variable`, `Namespace`) | **planned** | SPEC-10 F1. |
| `Graph` / `Dataset` / `ConjunctiveGraph` facades | **planned** | F2, F3. |
| `parse` / `serialize` (Turtle, N-Triples) | **planned** | F4. |
| `query` / `update` passthrough to SPEC-07 | **planned** | F5. |
| `rdflib-compat` harness subset | **planned** | Acceptance #1. |

> SPEC-10 is the only spec without a Stage-1 plan file. The single tracking
> task covers the whole binding layer; split it into per-feature tasks when
> implementation starts.

---

## 13. Cross-cutting concerns

### Provenance / correctability
**Status: partially implemented.** Stage-1 ships a stub `Provenance`
(`owlrl/src/provenance.rs`) and an ML-derived-fact provenance hook
(`ml/src/provenance.rs`). Production proof trees (SPEC-04 F4) and proof
retrieval (NF4) are **planned** (`TASKS.md` SPEC-04 rules).

### RDF 1.2 (triple terms)
**Status: implemented end-to-end (Stage-1 surface).** We track W3C **RDF 1.2**,
not the community RDF-star extension. `TermKind::TripleTerm` in storage, the
N-Triples loader, gated SPARQL triple-term patterns, and the
`rdf12-n-triples` harness suite all ship. Turtle/TriG/N-Quads/semantics suites
remain **deferred** (`TASKS.md`, RDF 1.2 entries — both `[x]`). The OWL 2 RL
Stage-1 engine and W3C-manifest paths explicitly bail on triple-term inputs.

### Performance gates (BENCHMARKS.md)
**Status: partially implemented.** Per-subsystem targets and measured numbers
live in `BENCHMARKS.md`. SPEC-03's 4-cycle ≥10× gate is now **met** (~34× on
the canonical skewed win case, [#1](https://github.com/sunstoneinstitute/horndb/issues/1)).
Keep `BENCHMARKS.md` rows in sync with the `TASKS.md` performance entries.

### Build & CI split
**Status: implemented.** Pre-commit runs `cargo fmt --check` only; pre-push
runs workspace `clippy -D warnings` + `cargo build`. CI mirrors this plus a
real-engine conformance run. The closure crate needs SuiteSparse:GraphBLAS
locally (being moved to a vendored submodule — §7).

---

## 14. Roadmap stages

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
