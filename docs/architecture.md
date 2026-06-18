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
| W3C OWL 2 RL test-suite ingestion pipeline | **implemented** | `owl2_rl_extract.rs` + `harness extract-owl2-rl`; 115 W3C cases → 96 green in `[suites.owl2-w3c-rl]`, reds tracked in `harness/KNOWN-MANIFEST-BUGS.md`. |
| Versioned selection manifest (`harness/selected.toml`) | **implemented** | Single canonical file at workspace root (manifest `[suites.*]` + `[sparql_query]`). |
| Result DB (SQLite) + trend reports (`harness report`) | **implemented** | `db.rs`, `report.rs`; state in `target/harness.sqlite`, JUnit at `target/junit.xml`. |
| Stub-engine smoke target | **implemented** | `stub.rs` (F12). |
| LUBM materialization RDFox A/B (`scripts/bench/compare-rdfox.sh --lubm N`) | **implemented (N=1)** | Identical TBox+ABox through both engines; RDFox fires the `rules.toml` rules (`gen_ruleset.py`) **plus** the TBox-resolved list-axiom rules + XSD datatype base (`gen_schema_closure.py`); closure-count parity gate + HornDB wall-clock cap. The N=1 "over-derivation" ([#59](https://github.com/sunstoneinstitute/horndb/issues/59)) was diagnosed as a harness-completeness gap (HornDB's `scm-int`/`cls-int1` + datatype base were absent from the reference ruleset, not a soundness bug) and **resolved** — parity is now exact (delta 0). The 3× *timing* gate at N=1 is still open, but [#61](https://github.com/sunstoneinstitute/horndb/issues/61) **resolved its scoping**: the SPEC-05 GraphBLAS closure backend is now wired + injectable into the owlrl `Engine` and per-phase profiling (`horndb-bench materialize --backend …`; `BENCHMARKS.md`) shows the LUBM-shaped materialize cost is dominated by the compiled `cax-sco` type-expansion + delta-apply, **not** the closure (~0.3% of reason time). So the timing gap is the SPEC-04 F5 `rdf:type`-partition scan tracked in [#2](https://github.com/sunstoneinstitute/horndb/issues/2), not the closure backend. LUBM-100 not yet run (Jena `riot` unavailable in the implementation sandbox; attribution from synthetic stand-ins). RDFox numbers internal-only (DeWitt). |
| Full W3C OWL 2 + SPARQL 1.1 *evaluation* suites, ORE 2015, LDBC SPB SF3/SF5, LUBM-100/UOBM, broader RDFox A/B | **planned** | `TASKS.md` MEDIUM · *Conformance* — "SPEC-01 harness" (#10). The SPARQL 1.1 *syntax* slice is now wired (`sparql11-syntax`, [#110](https://github.com/sunstoneinstitute/horndb/issues/110)); the remaining *evaluation*/result-set suites, ORE 2015, LDBC SPB SF3/SF5, LUBM-100/UOBM, and broader RDFox A/B are still outstanding. Scaffolding exists (`ore.rs`, `ldbc_spb.rs`); full corpora not wired (heavy downloads / self-hosted runners — Stage-2). Publication of RDFox numbers gated on license review. |

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
| `Engine` satisfying the harness `Reasoner` trait | **implemented** | `integration.rs` (oxrdf dictionary over `MemStore`); closure backend is injectable via `Engine::with_backend(BackendChoice)` — default `RuleFiring`, optional GraphBLAS (`graphblas-backend` feature, [#61](https://github.com/sunstoneinstitute/horndb/issues/61)). Adapter in `harness/src/owlrl_engine.rs`. |
| Reset and rematerialize (F7) | **implemented** | Full re-materialization per `load`. |
| `owl:sameAs` routed to SPEC-05 EQREL (F6) | **implemented** | Rule engine does not re-derive `eq-sym`/`eq-trans`. |
| Subset of rules (`eq-rep-*`, common `prp-*`/`cls-*`/`cax-*`/`scm-*`, incl. `scm-eqc-rev`) | **implemented** | 96 W3C OWL 2 RL cases green. `scm-eqc-rev` derives `owl:equivalentClass` from two-way `rdfs:subClassOf`. |
| `Provenance` side-table (F4) | **implemented** | `provenance.rs` — `struct Provenance { rule_id, premises }` recorded per derived triple; the basis of the proof tree (next row). |
| Proof recording (F4: `(rule_id, premises)` per derived triple → recursive proof tree) | **implemented** | Compiled + `list_rules.rs` rules record real body premises; `MemStore::proof_tree` / `Engine::proof` return a full proof tree bottoming out at asserted triples (`provenance.rs`, `integration.rs`; `tests/proof_tree.rs` covers NF4 depth + latency). Closure-backend nodes record empty premises by design; restriction-rule schema declarations are an elided side condition (instance premises still recorded). Production *persistence* (compressed side-table, on-demand re-derivation) remains Stage 2. |
| Datatype subsumption (`dt-type1` + `dt-type2` XSD lattice) | **implemented** | Load-time injection of `byte ⊑ short ⊑ int ⊑ ... ⊑ decimal` (and unsigned/non-negative arms); flips `I5.8-006-pe`/`I5.8-011-pe` green. |
| Max-cardinality (unqualified `cls-maxc1`/`cls-maxc2`, qualified `cls-maxqc1`–`cls-maxqc4`) | **implemented** | Hand-written in `list_rules.rs`; restriction literals (`owl:maxCardinality "0"`/`"1"`, and qualified `owl:maxQualifiedCardinality` + `owl:onClass`) classified at load time in `integration.rs`. `cls-maxc1`/`cls-maxqc1`/`cls-maxqc2` → `owl:Nothing` (inconsistency), `cls-maxc2`/`cls-maxqc3`/`cls-maxqc4` → `owl:sameAs`. The qualified rules ([#36](https://github.com/sunstoneinstitute/horndb/issues/36)) are covered by unit + integration tests; no `selected.toml` entry, because the only W3C qualified-cardinality case (`ObjectQCR-002-pe`) is blocked on fresh-bnode `owl:complementOf` generation, not on these rules. |
| Disjoint properties (`prp-pdw` pairwise, `prp-adp` list `owl:AllDisjointProperties`) | **implemented** | `prp-pdw` compiled from `rules.toml`; `prp-adp` ([#37](https://github.com/sunstoneinstitute/horndb/issues/37)) hand-written in `list_rules.rs` (list-walking analogue), both head `?u rdf:type owl:Nothing` on a shared `(u, w)` pair. Covered by unit + engine tests; the W3C `DisjointObjectProperties-*-cons` / `DisjointDataProperties-*-cons` cases in the selection exercise the no-false-fire path. The `*-pe` variants stay red on a DL `differentFrom`/`AllDifferent` entailment with no OWL 2 RL rule (`harness/KNOWN-MANIFEST-BUGS.md`). |
| Literal-value datatype rules (`dt-eq`/`dt-diff`/`dt-not-type`) | **implemented** | Load-time `inject_datatype_literal_axioms` (`integration.rs`) classifies each instance literal's value via `crates/owlrl/src/datatype_literals.rs` over the Stage-1 datatype set (XSD integer tower, `xsd:string`/`boolean`, plain/lang literals): value-equal ⇒ `owl:sameAs` (`dt-eq`, cross-lexical `1`≡`+1`≡`01` and cross-datatype `1`^^byte≡`1`^^integer), value-distinct (comparable) ⇒ `owl:differentFrom` (`dt-diff`), out-of-value-space lexical form ⇒ `owl:Nothing` (`dt-not-type`). Flips `#New-Feature-Keys-006-incons` green (issue #40). Disjoint value spaces (string vs integer) are never cross-compared; non-XSD/unhandled datatypes stay opaque (Stage-1 soundness). |
| Datatype value-space intersection (`I5.8-008/009-pe`) | **deferred** | Genuine interval/value-space narrowing; tracked under issue #4. |
| `rdf:type` skew parallelism (F5) | **implemented (list-rule path)** | The `rdf:type`-driven hand-written list rules (`cls-int1`, `cls-uni`, `cax-adc`, `prp-key`) partition their per-subject filtering by class id and parallelise it across rayon above `PAR_TYPE_THRESHOLD` (`crates/owlrl/src/list_rules.rs`), selected by `MaterializeOpts::parallel` (`ParallelStrategy::Auto` default; `Serial` is the oracle). Identical closure proven by `tests/rdf_type_skew_differential.rs` (3 large-extent fixtures + proptest); `benches/rdf_type_skew.rs` + `BENCHMARKS.md` record the win ([#39](https://github.com/sunstoneinstitute/horndb/issues/39)). Parallelising the **compiled** (`cax-sco`-style) rules — which would change the generated `FireFn` signature — is a separate Stage-2 follow-up. |
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
| Wiring the GraphBLAS closure into the owlrl `Engine` (production replacement for `RuleFiringBackend`) | **implemented** | `crates/owlrl/src/graphblas_backend.rs` (`GraphBlasBackend`, `graphblas-backend` feature) computes `scm-sco`/`scm-spo`/`eq-sym`/`eq-trans`/`prp-trp` via strict `transitive_closure` over a dense `BoolMatrix`; injected via `Engine::with_backend(BackendChoice::GraphBlas)`. Differential parity with `RuleFiringBackend` in `crates/owlrl/tests/closure_backend_differential.rs`. Profiling ([#61](https://github.com/sunstoneinstitute/horndb/issues/61), `BENCHMARKS.md`) shows the swap is a decisive win only when closure dominates; the LUBM-shaped materialize cost is compiled-rule/`rdf:type`-scan bound ([#2](https://github.com/sunstoneinstitute/horndb/issues/2)), not closure-bound. |
| Vendored GraphBLAS as a git submodule (static, OpenMP, checked-in bindings) | **implemented** | `crates/closure/vendor/GraphBLAS` submodule `v10.3.0`; `vendored`+`openmp` default Cargo features (`regen-bindings` optional), statically linked (verified via `otool -L`), checked-in `src/bindings.rs`. CI checks out submodules and drops the from-source build. Supersedes the `[x]` "CI: install GraphBLAS on runners". |
| Shared, flock-guarded GraphBLAS build across worktrees | **implemented** | `build.rs` compiles the vendored GraphBLAS once per `(target, version)` into `crates/closure/vendor/.shared-build/<target>/<version>/` (anchored at the main worktree, gitignored), reused across git worktrees; concurrent builders serialise on an `fs4` advisory flock with the builder pid written in for diagnostics; CI caches the dir keyed on the submodule SHA. Details in `crates/closure/INTEGRATION-NOTES.md`. Narrows the disk-pressure concern (`TASKS.md` #13) to rocksdb. |
| Incremental closure updates (F6) — insertion + retraction | **implemented** | `closure/incremental.rs` (`IncrementalTransitiveClosure`) + `sink.rs` (`IncrementalClosureBackend`): a single-edge insert updates only the affected slice (backward-reach(s) × forward-reach(o)) and writes only the delta to the sink. **Deletion/retraction** (`delete_edge`/`delete_edges`/`delete_transitive_edges`) retains the asserted base edges alongside the closed set; retracting a base edge recomputes base-reachability over the affected source region and withdraws only the closure pairs no longer derivable over the post-delete base (invariant `closed == transitive_closure(base)`). Differential proptests vs GraphBLAS full closure (`tests/incremental.rs` insertion, `tests/incremental_retraction.rs` random insert/delete sequences). SPEC-06 owns the +/- sign; the SPEC-05 layer is sink-insertion-only and returns the withdrawn edges. Closure-path retraction delivered under [#5](https://github.com/sunstoneinstitute/horndb/issues/5) (insertion path [#42](https://github.com/sunstoneinstitute/horndb/issues/42)). |
| Valued closure / custom semirings (Sunstone annotated reasoning) | **planned** | `TASKS.md` MEDIUM · *Performance* (two entries): readiness metrics first, then Fork A (scalar, built-in semirings) → Fork B (structured carrier) → PreJIT. Spec addendum gated on the metrics. |
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
| Algebra translation (BGP, Join, LeftJoin, Filter, Project, Distinct, Slice, OrderBy, Union, Extend, Values) | **implemented** | `algebra/translate.rs`. |
| Aggregation / `GROUP BY` (`COUNT`/`SUM`/`MIN`/`MAX`/`AVG`/`SAMPLE`/`GROUP_CONCAT`, `DISTINCT` modifiers) | **implemented** | `algebra/translate.rs` + `exec/runtime.rs::eval_group`. Unblocks the LDBC SPB aggregation mix (incl. the driver's `COUNT` warm-up query). #66. |
| `FILTER`/`BIND` expression coverage | **implemented (Stage-1 surface)** | Comparisons (incl. `<=`/`>=`), `IN`/`NOT IN`, boolean connectives, arithmetic, `IF`, `COALESCE`, and 30 builtins (string/regex/numeric/type-check/datetime accessors) over the best-effort f64 lexical model — `algebra/mod.rs::Func`, `exec/runtime.rs::eval_func`. `EXISTS`, non-deterministic builtins (`RAND`/`NOW`/`UUID`/…), hashing, `STRLANG`/`STRDT`, and custom functions still return `UnsupportedAlgebra`. #66. |
| `GRAPH` named-graph patterns | **implemented (Stage-1 merged-graph)** | Lower transparently to the inner pattern; a graph-name variable stays unbound. True named-graph scoping (zero solutions for absent graphs, per-graph `?g` bindings) is deferred to the named-graph epic (#7) — see `crates/sparql/INTEGRATION-NOTES.md`. #66. |
| `MINUS` | **planned** | `translate.rs` returns `UnsupportedAlgebra`. Part of the SPEC-07 umbrella (#7). |
| Planner + runtime executor | **implemented** | `plan/`, `exec/`. BGPs route to `exec/horn.rs::HornBackend`, which executes on `horndb-storage` (kind-tagged dictionary `TermId`s — fixes the Stage-1 lexical type-erasure/IRI-coercion) via the `horndb-wcoj` Leapfrog Triejoin (binary-hash for ≤3 patterns; WCOJ via `Planner::default()` for ≥4). `MemStore` (`exec/mem.rs`) is retained as the in-process test double. `DELETE DATA` is handled by a tombstone overlay over the insertion-only storage layer. `load_with_reasoning` (`reasoner` feature, default-on) runs the `horndb-owlrl` Engine (RuleFiring backend) and loads the full materialized closure directly into the backend, replacing the earlier dump-to-flat-file round trip. The `serve` binary accepts `--materialize` to trigger this path. (#67) |
| SELECT / CONSTRUCT / ASK | **implemented** | Result formats in `results/`. |
| Entailment regimes: OWL 2 RL/RDF + simple | **implemented** | `regime/owl_rl.rs`, `regime/simple.rs` (materialized mode). |
| SPARQL Update `INSERT/DELETE DATA` | **implemented** | `update.rs`. |
| Pattern-based Update (`INSERT`/`DELETE … WHERE`, `DELETE WHERE`, `WITH/DELETE/INSERT … WHERE`) | **implemented** | `update.rs::apply_delete_insert`: evaluates the WHERE pattern via `translate_where` → planner → runtime, collects all solutions over the pre-update graph, then applies deletions-before-insertions (SPARQL 1.1 §3.1.3) through the `Store` seam. Ground-template safety drops triples with unbound slots; per-solution blank nodes are row-scoped. Default-graph only and single-op — named-graph templates and `USING`/`WITH <named>` are rejected (Stage-1 has one default graph); multi-op updates stay `UnsupportedForm`. ([#51](https://github.com/sunstoneinstitute/horndb/issues/51)) |
| Embedded HTTP server (`/query`, `/update`) | **implemented** | `server/` (axum), behind `server` feature. |
| RDF 1.2 triple-term patterns `<<( s p o )>>` | **implemented (gated)** | Accepted only when caller passes `SparqlConfig::rdf12()`; default rejects them so SPARQL 1.1 callers keep 1.1 semantics. `translate_query_with` / `execute_query_with`. |
| `DESCRIBE` query form | **implemented (partial)** | Forward one-level Concise Bounded Description: `translate.rs` lowers the describe pattern like SELECT, `exec/runtime.rs::describe_triples` emits each resource's outgoing triples. Recursive/symmetric blank-node CBD and typed-literal/Turtle serialisation deferred (Stage-1 `MemStore` erases term types on scan; tracked in [#57]). `TASKS.md` #48. |
| Non-recursive property paths (`/`, `^`, `\|`, `?`, `!`) | **implemented** | `translate.rs::translate_path` lowers them at translation time: `/`(Seq) and `^`(Inverse) expand into triple patterns; `\|`(Alternative) and `?`(ZeroOrOne) lower to `Union` (zero-length `?` binds endpoints without enumerating the graph — two distinct unbound endpoints are rejected as out of Stage-1 scope); `!`(NegatedPropertySet) lowers to a wildcard-predicate BGP under a `NOT IN` filter. A WHERE-pattern blank node (incl. the one spargebra mints when it flattens a sequence) is now treated as a non-distinguished join variable (`match_term`), which also fixes latent `/`-sequence joins across algebra boundaries. Covered by `tests/exec_property_paths.rs` and conformance fixtures `path-{alt,neg,opt}-001` (both backends). ([#49](https://github.com/sunstoneinstitute/horndb/issues/49)) |
| Kleene-star property paths (`*`, `+`) | **implemented** | `translate.rs::translate_closure_path` lowers `+`/`*` to the `Algebra::PathClosure` node (the inner one-step path is expanded over the hidden endpoint vars `?pp_src`/`?pp_dst`, so `(p\|q)+`, `^p+`, `(p/q)+` all work); `runtime.rs::eval_path_closure` materialises the edge relation, takes its transitive closure by BFS to a fixpoint (cycle-safe), and for `*` adds the reflexive pairs over the touched node set, then binds/filters against the query endpoints. Covered by `tests/exec_property_paths.rs` and conformance fixtures `path-{plus,star}-001` (both backends; `path-star-001` is the acceptance-#7 `subClassOf*` shape). **Deferred:** routing a materialised single-predicate closure through the SPEC-05 GraphBLAS backend + selectivity-based planner choice (F3 fast path — correctness ships now, acceleration later); strict full-graph node-set semantics for `*`'s zero-length match over nodes untouched by the path. ([#50](https://github.com/sunstoneinstitute/horndb/issues/50)) |
| Graph-management Update (`LOAD`/`CLEAR`/`DROP`/`CREATE`/`ADD`/`MOVE`/`COPY`) + multi-op updates | **implemented (Stage-1 default-graph)** | `update.rs`: parser now admits graph-management verbs and multi-operation sequences (`parser::ParsedUpdate::GraphManagement`); the executor walks the op list. Under the default-graph-only model: `CLEAR`/`DROP DEFAULT`/`ALL` clear the store (`Store::clear_all`, added to the seam — `MemStore` resets, `HornBackend` tombstones every live key); `LOAD <file:…> [INTO GRAPH …]` reads a `file:` source via `oxttl` (`.nt`/`.ttl`/`.nq`/`.trig`, default Turtle) and merges it into the default graph; `CREATE`, a named `CLEAR`/`DROP`/`LOAD INTO` target, and a named `ADD`/`MOVE`/`COPY` operand are errors unless `SILENT` (then no-ops). `ADD`/`MOVE`/`COPY` are spargebra-desugared to `Drop` + `DeleteInsert`; the same-graph identity case desugars to zero operations (valid no-op). Tests: `tests/update_graph_mgmt.rs` (both backends), `tests/server_http.rs` (`/update` CLEAR + LOAD). **Deferred:** true named-graph scoping (→ GSP [#54](https://github.com/sunstoneinstitute/horndb/issues/54)) and remote (`http(s):`) LOAD (no HTTP client dep). ([#52](https://github.com/sunstoneinstitute/horndb/issues/52)) |
| Backward-chained entailment mode (F4 second mode) | **deferred** | Depends on SPEC-03 magic-sets/tabling (deferred). |
| `EXPLAIN` pragma (F9) | **implemented** | `parser.rs` recognises a leading non-standard `EXPLAIN` / `EXPLAIN JSON` pragma (case-insensitive, whitespace-delimited so `?explainme` is not mistaken for it), strips it, and wraps the inner query as `ParsedQuery::Explain`. `api::execute_query` translates + plans the wrapped query **without executing it** and renders via `plan::explain` (`QueryAnswer::Explanation`): an indented operator tree (or JSON object tree) carrying a header `mode:` line (the entailment-regime execution mode — `materialized` today, labelled "backward-chaining not yet available" pending [#55](https://github.com/sunstoneinstitute/horndb/issues/55)) and per-node `~N rows` cardinality estimates. Estimates come from `Executor::cardinality_estimate` (new trait method, default `None`; `MemStore` returns the leading-pattern index size, `HornBackend` the live triple count as an upper bound) combined by textbook per-operator rules. The `/query` handler serves the rendering as `text/plain` or `application/json` by pragma. Satisfies acceptance #5 (EXPLAIN on `subClassOf+` shows mode + cardinality). Covered by `tests/explain_pragma.rs`, `tests/parser_basic.rs`, `tests/server_http.rs`, and `plan::explain` unit tests. **Deferred:** "chosen indexes" display (no cost-based index chooser yet — the plan is a 1:1 lowering) and the real materialized-vs-backward mode selection (with [#55](https://github.com/sunstoneinstitute/horndb/issues/55)). ([#53](https://github.com/sunstoneinstitute/horndb/issues/53)) |
| Graph Store Protocol | **deferred** | Stage-2. Direct REST access to *named* graphs, but the store is default-graph-only — named graphs are unrepresentable until SPEC-02 grows a quad-aware seam. Blocked on that storage work, not on the frontend. ([#54](https://github.com/sunstoneinstitute/horndb/issues/54), closed) |
| Streaming result serialization (F6 — currently buffers) | **deferred** | Stage-2 perf. The buffered path (`Vec<Bindings>` per node + buffering serializers) is correct; converting to a streaming pipeline is a perf-tuning increment, not a correctness gate. ([#56](https://github.com/sunstoneinstitute/horndb/issues/56), closed) |
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

## 13. Cross-cutting concerns

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
