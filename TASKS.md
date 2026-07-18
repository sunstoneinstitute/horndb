# Follow-up Tasks

Outstanding work deferred from the Stage-1 pass (2026-05-24), ordered by
priority within each category. This file tracks **current state only** — closed
tasks collapse to a one-line link in [Done](#done-for-traceability).

When a task is picked up, move it to its own commit / PR and check it off here
(and in the index) in the same commit.

> **Maintenance:** the [Index](#index) is the TOC — one line per task, mirroring
> its checkbox state, **priority**, and _category_. Each open task mirrors one
> GitHub issue (`sunstoneinstitute/horndb`) via its `([#N](…))` link, on both the
> index line and the body heading, labelled with one `priority:` label
> (`critical`/`high`/`medium`/`low`) and one `category:` label to match. Keep task
> and issue in lockstep, in the same change:
>
> - **Add a task** → open an issue with the matching `priority:` + `category:`
>   labels, then put its `([#N](url))` link on both the index line and the body
>   heading. `gh issue create --title … --label "priority: …" --label "category: …" --body-file …`.
> - **Complete a task** (`[ ]` → `[x]`) → `gh issue close N`. Keep the link for traceability.
> - **Retitle / re-prioritise / re-categorise** → `gh issue edit N` to update the
>   title and swap the `priority:`/`category:` labels so they still match.
> - **Remove a task** → `gh issue close N` (comment why) and drop its `TASKS.md` lines.
>
> The `priority:`/`category:` labels are the GitHub mirror of the taxonomy below; if
> you add a new one here, `gh label create` it first. When a task changes, also
> update `docs/architecture.md` — see `CLAUDE.md` → "Keep the docs in sync".
>
> **Priority** = urgency (CRITICAL/HIGH/MEDIUM/LOW). **Category** = type of work:
> _Correctness_ · _Performance_ · _Completeness_ · _Conformance_ · _Tooling_ ·
> _Operational_ · _Maintainability_.

## Index

**Stage-2 investment epics** (opened 2026-07-07 — deferred → `to-spec`; each is a `needs-decomposition` epic, spec'd then broken into leaf issues; details in [Stage-2 investment epics](#stage-2-investment-epics) and `docs/architecture.md`):

- [x] **CRITICAL** · _Completeness_ — **EPIC E1**: SPEC-23 unified query+reasoning IR (single IR; optimizer framework ships first) ([#185](https://github.com/sunstoneinstitute/horndb/issues/185))
- [x] **HIGH** · _Completeness_ — **EPIC E2**: SPEC-06 incremental maintenance completeness (delta-incremental retraction, MVCC backing) ([#186](https://github.com/sunstoneinstitute/horndb/issues/186))
- [ ] **HIGH** · _Completeness_ — **EPIC E3**: SPEC-02 storage Stage-2 (per-tuple MVCC, persistent dict, tiering, snapshots, WAL) ([#187](https://github.com/sunstoneinstitute/horndb/issues/187))
- [ ] **MEDIUM** · _Completeness_ — **EPIC E4**: SPEC-04 rule completeness Stage-2 (proof persistence, full dt-*, list/QCR rules, user-defined rules) ([#188](https://github.com/sunstoneinstitute/horndb/issues/188))
- [ ] **MEDIUM** · _Completeness_ — **EPIC E5**: SPEC-07 SPARQL surface completeness Stage-2 (GSP, named-graph scoping, remote LOAD, XML, DESCRIBE) ([#189](https://github.com/sunstoneinstitute/horndb/issues/189))
- [ ] **MEDIUM** · _Completeness_ — **EPIC E6**: SPEC-08 ML integration Stage-2 (FAISS candidate gen, NL→SPARQL) ([#190](https://github.com/sunstoneinstitute/horndb/issues/190))
- [ ] **MEDIUM** · _Conformance_ — **EPIC E7**: RDF 1.2 Stage-2 (Turtle/TriG/N-Quads/JSON-LD serialize + semantics suites + mapping annotation) ([#191](https://github.com/sunstoneinstitute/horndb/issues/191))
- [ ] **LOW** · _Operational_ — **EPIC E8**: SPEC-17 observability Stage-2 — OpenTelemetry traces & logs ([#192](https://github.com/sunstoneinstitute/horndb/issues/192))

- [x] **CRITICAL** · _Completeness_ — SPEC-23 Phase 1: optimizer framework scaffolding — logical IR, binding/type lattice, pass registry ([#201](https://github.com/sunstoneinstitute/horndb/issues/201))
- [ ] **HIGH** · _Performance_ — SPEC-23 Phase 2: heuristic rewrite passes (Normalize, FilterPullup/Pushdown, ProjectionPushdown) — after #201 ([#202](https://github.com/sunstoneinstitute/horndb/issues/202))
- [ ] **HIGH** · _Performance_ — SPEC-23 Phase 3: layered `Stats` seam + Characteristic-Sets cardinality estimator — after #201 ([#203](https://github.com/sunstoneinstitute/horndb/issues/203))
- [ ] **HIGH** · _Performance_ — SPEC-23 Phase 4: cost-based `JoinPlanning` (retires `wcoj_cutover == 4`) — after #201–#203 ([#204](https://github.com/sunstoneinstitute/horndb/issues/204))
- [ ] **HIGH** · _Completeness_ — SPEC-23 Phase 6: reasoning in the IR (rewrite passes, delegate nodes, catalog seam) — after #201–#204 ([#206](https://github.com/sunstoneinstitute/horndb/issues/206))
- [ ] **HIGH** · _Completeness_ — SPEC-23 Phase 7: backward-chaining (magic-sets + SLG tabling + SPARQL backward mode) — after #206 ([#207](https://github.com/sunstoneinstitute/horndb/issues/207))
- [ ] **HIGH** · _Completeness_ — SPEC-24 S1: delta-incremental rule retraction — incremental distinct + operator traces ([#210](https://github.com/sunstoneinstitute/horndb/issues/210))
- [ ] **HIGH** · _Completeness_ — SPEC-24 S2: delta-incremental closure retraction + exact warm-store seeded retraction ([#211](https://github.com/sunstoneinstitute/horndb/issues/211))
- [ ] **HIGH** · _Completeness_ — SPEC-24 S4: engine wiring — SPARQL Update → Circuit → readers — after #212 ([#213](https://github.com/sunstoneinstitute/horndb/issues/213))
- [ ] **HIGH** · _Completeness_ — SPEC-24 S6: MVCC backing of `Circuit::snapshot` onto SPEC-02 per-tuple visibility — blocked on E3 #187 ([#215](https://github.com/sunstoneinstitute/horndb/issues/215))
- [ ] **HIGH** · _Performance_ — SPARQL aggregation runtime: id-based bindings + hash group-by + streaming (12× SPB gap) ([#128](https://github.com/sunstoneinstitute/horndb/issues/128))
- [ ] **HIGH** · _Performance_ — SPEC-12 SIMD layer: `horndb-simd` primitives crate **landed** (F4+F5); WCOJ seek/intersect consumer (F1) **landed** — `VecIter` SoA-column + `PackedColumn` block-finish seek through `horndb_simd::lower_bound`, `LeapfrogJoin` k==2 `horndb_simd::intersect` fast path, real `per_tuple` microbench wired (differential fuzzer + leapfrog oracle green); storage decode + `rdf:type` scan consumer (F2) **landed** — `Dictionary::decode_inline_ints`/`lookup_batch` bulk inline-int decode + `PredicatePartition::subjects_with_object` via the new `horndb_simd::filter_indices_eq` primitive, `dict_decode`/`partition_scan` benches wired. SIMD intersect now wired into `BatchIter`'s inlined leapfrog (the production executor hot path; `active_run` deduplicates to honour the distinct-key contract). Real wide `intersect` kernels (AVX-512 `compressstore`/AVX2/NEON) **landed**; `intersect`/`lower_bound`/`gather`/`filter_indices_eq` benched on **Intel SPR + Zen4** (2026-06-30): intersect AVX-512 ~2.5× on Intel (regresses on Zen4 double-pump), lower_bound a scalar win on both, gather + sparse filter ~1.5–2.2× wins. **Kernel selection reworked (2026-07-01) after the real workload contradicted the microbenches:** a same-session LDBC SPB-256 A/B on Zen4 (hornbench) and Intel SPR (hel01) showed the calibrated SIMD kernels are **net-harmful vs scalar on both** (dominant culprit: AVX2 `lower_bound` on the seek-heavy leapfrog path; the "AVX-512 intersect ~2.5× on Intel" microbench claim was fiction for SPB — AVX-512 runs at ~half scalar throughput there). **Fixed:** kernel selection is now `forced → HORNDB_SIMD_MAX_ISA cap → known-CPU table (CPUID-keyed, SPB-derived) → representative-input calibration → static widest`; the known-CPU table pins scalar for both measured hosts (AMD fam 25 mdl 97 Ryzen 7 7700, Intel fam 6 mdl 143 Xeon Gold 5412U), representative calibration (seek-sweep / >L2 base / moderate selectivity) makes an unlisted CPU reject the killer kernels too, the intersect skew-gate stays, and the selection tier is exported as the `source` label on `horndb_simd_kernel_isa{kernel,isa,source}` + the serve startup log. **SPB-256 aggregation-qps recovered on Zen4: 28.6 (SIMD regression) → 36.16** (table, all scalar; +18% over the 30.6 pre-SIMD baseline); Intel steady at 34.4. **`per_tuple` measured on hornbench (2026-06-30): ~67 ns/tuple, unchanged by the intersect (criterion A/B “no change”) — NF1 ≤2.5 ns not met; bottleneck is the depth-1 narrow-run leapfrog + Arrow materialization, not the intersect.** **hornbench numbers recorded (2026-07-07, Ryzen 7 7700, node-0-pinned):** `dict_decode` scalar 14.74 µs vs AVX2 14.54 µs → **~1.01×, RED** (load/store-bound; NF4 ≥4× is a compute target the memory-bound loop can't reach — SIMD not the lever); `partition_scan` **34.5 GB/s = ~104% of STREAM-Triad (33.1 GB/s full-socket) → GREEN** (SPEC-02 acceptance #4 met). **Remaining:** close NF1 `per_tuple` (depth-1 / materialization path — not SIMD); delta-apply (F3) consumer (gated on [#133](https://github.com/sunstoneinstitute/horndb/issues/133)) ([#132](https://github.com/sunstoneinstitute/horndb/issues/132))
- [x] **HIGH** · _Performance_ — SPEC-04: within-partition object index on `MemStore` so `rdf:type` probes are O(|extent|) ([#133](https://github.com/sunstoneinstitute/horndb/issues/133))
- [ ] **HIGH** · _Performance_ — SPEC-04: genuine delta-driven semi-naïve firing for the compiled rules ([#134](https://github.com/sunstoneinstitute/horndb/issues/134))
- [ ] **HIGH** · _Completeness_ — SPEC-11 SSSOM mappings + compact crosswalk index ([#130](https://github.com/sunstoneinstitute/horndb/issues/130))
- [ ] **HIGH** · _Operational_ — Observability metrics (Phase 1): prometheus-client + `/metrics` scrape; Slice 1 (SPARQL HTTP + closure + storage) landed, fan-out remaining ([#148](https://github.com/sunstoneinstitute/horndb/issues/148))
- [ ] **MEDIUM** · _Performance_ — LDBC SPB nightly: scale to true SF=0.256 (256M triples) + editorial agents ([#125](https://github.com/sunstoneinstitute/horndb/issues/125))
- [ ] **MEDIUM** · _Completeness_ — SPEC-24 S3: change-feed net-delta reconciliation + bounded backpressure — before #213 ([#212](https://github.com/sunstoneinstitute/horndb/issues/212))
- [ ] **MEDIUM** · _Operational_ — SPEC-24 S5: DeltaLog WAL contract + checkpoint scheduling — on-disk format with E3 #187 ([#214](https://github.com/sunstoneinstitute/horndb/issues/214))
- [ ] **MEDIUM** · _Performance_ — SPEC-24 S7: bilinear-join runtime (per-predicate leaves, cost model, hash/sort-merge kernels) — after #203 ([#216](https://github.com/sunstoneinstitute/horndb/issues/216))
- [x] **MEDIUM** · _Conformance_ — Close the RL-reachable OWL 2 RL gap: datatype value-space intersection + `owl:imports` (97/115 → 100/115) ([#160](https://github.com/sunstoneinstitute/horndb/issues/160))
- [ ] **LOW** · _Operational_ — Disk pressure during multi-agent runs (rocksdb) ([#13](https://github.com/sunstoneinstitute/horndb/issues/13))
- [ ] **LOW** · _Operational_ — 1Password SSH agent reliability ([#14](https://github.com/sunstoneinstitute/horndb/issues/14))
- [ ] **LOW** · _Performance_ — SPEC-23 Phase 5: later optimizer work (stats sketches, runtime filters/SIP, ML `PlanAdvisor` loop) — after #203/#204 ([#205](https://github.com/sunstoneinstitute/horndb/issues/205))
- [ ] **LOW** · _Completeness_ — SPEC-24 S8: intra-tick closure↔rule joint fixpoint + non-transitive closure shapes ([#217](https://github.com/sunstoneinstitute/horndb/issues/217))
- [ ] **LOW** · _Maintainability_ — Extract shared `compile_bgp_patterns` helper in `crates/sparql/src/exec/horn.rs` (#TODO)

Closed tasks are listed in [Done](#done-for-traceability).

## Stage-2 investment epics

Opened 2026-07-07. These pull previously-**deferred** work into **`to-spec`**: each
has a `needs-decomposition` GitHub epic and is queued to be specified, then broken
into leaf issues via the `to-issues` skill (leaf issues are *not* pre-created). They
mirror the [Stage-2 investment epics](docs/architecture.md#stage-2-investment-epics)
table in `docs/architecture.md`. Full item-level scope lives in each epic issue.

- [x] **EPIC E1 — SPEC-23 unified query + reasoning IR (single IR).** _CRITICAL · Completeness._
  ([#185](https://github.com/sunstoneinstitute/horndb/issues/185)) The flagship, spec'd
  first. A single logical IR expressing query **and** reasoning so the optimizer jointly
  decides join order, reasoning strategy (materialize/rewrite/delegate), and demand-driven
  partial closure. The **optimizer framework ships first** (logical IR, pass registry,
  `Stats` seam, cost-based ordering); reasoning then enters the same IR, pulling in
  magic-sets/backward-chaining (SPEC-03
  F4/F5), SPARQL backward mode (SPEC-07), reasoning-as-rewrite + a reasoning/materialization
  catalog seam, the cost-based WCOJ planner, and property-path→GraphBLAS choice. Stub:
  `docs/specs/SPEC-23-unified-ir.md`. **DoD:** write + approve SPEC-23, then decompose.
  **Done 2026-07-18** (PR [#208](https://github.com/sunstoneinstitute/horndb/pull/208)): SPEC-23 approved; decomposed into
  leaf issues [#201](https://github.com/sunstoneinstitute/horndb/issues/201)–[#207](https://github.com/sunstoneinstitute/horndb/issues/207) — tracked as the SPEC-23 phase tasks in this file (Phase 1 [#201](https://github.com/sunstoneinstitute/horndb/issues/201) first).
- [x] **EPIC E2 — SPEC-06 incremental maintenance completeness.** _HIGH · Completeness._
  ([#186](https://github.com/sunstoneinstitute/horndb/issues/186)) Fully delta-incremental
  retraction (no affected-region recompute), MVCC backing of `Circuit::snapshot`, change-feed
  reconciliation, WAL/backpressure/cost-model. Successor to Stage-1 epic #6.
  **Done 2026-07-18** (PR [#218](https://github.com/sunstoneinstitute/horndb/pull/218)): `SPEC-24` approved; decomposed into
  leaf issues [#209](https://github.com/sunstoneinstitute/horndb/issues/209)–[#217](https://github.com/sunstoneinstitute/horndb/issues/217) — tracked as the SPEC-24 phase tasks in this file (S1 [#210](https://github.com/sunstoneinstitute/horndb/issues/210)
  and S3 [#212](https://github.com/sunstoneinstitute/horndb/issues/212) are unblocked first).
- [ ] **EPIC E3 — SPEC-02 storage Stage-2.** _HIGH · Completeness._
  ([#187](https://github.com/sunstoneinstitute/horndb/issues/187)) Per-tuple MVCC, persistent
  on-disk dictionary, cold/CXL tiering seam, named-graph snapshots, WAL, deferred acceptance
  benches (hornbench). Successor to Stage-1 epic #3.
- [ ] **EPIC E4 — SPEC-04 rule completeness Stage-2.** _MEDIUM · Completeness._
  ([#188](https://github.com/sunstoneinstitute/horndb/issues/188)) Proof persistence, datatype
  value-space + full `dt-*` tower, list-walking `cls-int*/uni*`, qualified max-cardinality,
  user-defined rules, owlrl Z-set wiring. Successor to Stage-1 epic #4. (Perf hotspots #133/#134
  stay separate.)
- [ ] **EPIC E5 — SPEC-07 SPARQL surface completeness Stage-2.** _MEDIUM · Completeness._
  ([#189](https://github.com/sunstoneinstitute/horndb/issues/189)) Graph Store Protocol +
  named-graph scoping, remote LOAD, XML results, recursive DESCRIBE, streaming CONSTRUCT/DESCRIBE.
  Successor to Stage-1 epic #7. (Streaming/agg remainder → #128; python graph-scoping → #119.)
- [ ] **EPIC E6 — SPEC-08 ML integration Stage-2.** _MEDIUM · Completeness._
  ([#190](https://github.com/sunstoneinstitute/horndb/issues/190)) FAISS-backed candidate
  generator, NL→SPARQL endpoint wired into `serve`. Successor to Stage-1 epic #8. (PlanAdvisor
  validation loop lives in E1.)
- [ ] **EPIC E7 — RDF 1.2 Stage-2.** _MEDIUM · Conformance._
  ([#191](https://github.com/sunstoneinstitute/horndb/issues/191)) Turtle/TriG/N-Quads/JSON-LD
  parse+serialize, RDF 1.2 semantics suites, per-edge mapping annotation (SPEC-11). (SSSOM core
  build → #130.)
- [ ] **EPIC E8 — SPEC-17 observability Stage-2: OTel traces & logs.** _LOW · Operational._
  ([#192](https://github.com/sunstoneinstitute/horndb/issues/192)) OpenTelemetry traces and logs
  — the phase after Prometheus metrics (#148).

## CRITICAL — Completeness

- [x] **SPEC-23 Phase 1: optimizer framework scaffolding.** ([#201](https://github.com/sunstoneinstitute/horndb/issues/201))
  The foundation of epic E1 (#185, closed → decomposed) — logical IR with a flat
  n-ary `Bgp` + coalescing, binding/type lattice, smart constructors, typed
  individually-toggleable pass registry with declared ordering constraints and
  debug-build validation; `planner::plan` wired through the pipeline with **no
  behavior change** (golden-plan tests), existing `plan/pushdown.rs` heuristics
  ported onto it. Spec: `docs/specs/SPEC-23-unified-ir.md` §5.1–5.2. Plan:
  `docs/plans/PLAN-23-01-optimizer-framework-scaffolding.md`. Gates: SPEC-23
  acceptance #1–#2 (conformance subset + WCOJ differential fuzzer stay green;
  regressions bisectable to a single `PassId`). Blocks all later phases.

## HIGH — Performance

- [ ] **SPARQL aggregation runtime: id-based bindings + hash group-by + streaming.**
  ([#128](https://github.com/sunstoneinstitute/horndb/issues/128))
  **Slice 1 + Slice 2 (id-based slot rows) landed** (design spec:
  `docs/specs/SPEC-16-id-based-slot-rows.md`; plans:
  `docs/plans/PLAN-16-01-id-based-slot-rows-slice1.md`,
  `docs/plans/PLAN-16-02-id-based-slot-rows-slice2.md`). **All 13 runtime operators now
  run native on id-based slot rows** (`Slot`/`Row`/`Batch`): Slice 1 did
  BgpScan/Slice/Project/Distinct/Group/Filter/Join + native `scan_bgp_ids`; Slice 2
  ported the last six (LeftJoin, Union, OrderBy, Extend, Values, PathClosure) and
  **removed the `from_bindings`/`to_bindings` decode-adapter (`eval_rows`), the
  `cfg(test) eval_legacy` oracle, and the dead helpers it kept alive (`eval_group`,
  `project`, `hash_left_join`/`probe_into`/`join_vars`/`join_key`)** — one slot runtime.
  `Runtime::run` decodes once at the boundary. **Official nightly `aggregation-qps`
  (hornbench, 2026-06-29): ~13 → ~23 (~1.77×) vs GraphDB Free ~148 — the ~12× gap
  narrowed to ~6.5×.** Slice 2 does not touch the aggregation arms, so aggregation-qps
  is a Slice-1 number. The remaining gap at that point was owned by streaming + planner
  pushdown; both have since landed (#143, #144) — see below.

  **#143 Streaming pull-based runtime LANDED** (2026-06-30, this branch): the
  SPARQL runtime is now a pull-based, batch-at-a-time operator tree
  (`crates/sparql/src/exec/op/`); every operator (Scan/Filter/Project/Extend/
  Slice/Values/Distinct/Union/Join/LeftJoin/Group/OrderBy/PathClosure) is a
  native `Op`; legacy materializing `eval` deleted; chunk-boundary invariance
  tested. Design: `docs/specs/SPEC-19-streaming-runtime-pushdown.md`.
  **#144 Planner pushdown LANDED** (2026-06-30, this branch): column pruning
  (`plan/pushdown.rs`) + COUNT-over-BGP aggregate pushdown (`Executor::count_bgp`
  + `CountScan` + `CountScanOp`). **#145 deterministic `GROUP BY` +
  `COUNT(DISTINCT *)` test LANDED** (this branch pins it in `exec/runtime.rs`
  `slot_differential`; [#161](https://github.com/sunstoneinstitute/horndb/issues/145)
  also added `group_by_count_distinct_star` in `crates/sparql/tests/exec_aggregate.rs`).
  **`Group` micro-opts LANDED** via #167 (share decoded members across aggregates;
  drop the per-group `key_slots` clone). SPB-256 **re-measured on hornbench
  2026-06-30: HornDB ~30.8 qps (branch `4ce02e10` 30.78 ≈ same-day main
  `b142f00c` 30.71) vs GraphDB Free ~153 → ~5.0× gap** (was ~6.5× at ~23).
  The 23 → ~30.7 gain was **bisected locally to Slice 2's native-slot
  `LeftJoin`/`OPTIONAL` hash probe (`309c2db`)** (secondary: native `Extend`/`BIND`
  `bca05f2`) — the SPB aggregation queries are `OPTIONAL`-heavy (query1 has 15
  `OPTIONAL`s), so the nested-loop→hash-probe rewrite lifts their qps; `agg_profile`
  Q1–Q5 (no `OPTIONAL`) were blind to it. **#143/#144 are net-neutral on top** (branch
  with them 30.78 ≈ same-day main without 30.71). See docs/benchmarks.md.

  **Join probe-side streaming + bound-key selection LANDED** (this branch):
  `JoinOp`/`LeftJoinOp` drain only their build side (right) into a hash index on
  first `next()` and stream the probe side (left) chunk-by-chunk; join keys come
  from the build side's actually-bound columns (`bound_join_vars`, replacing the
  schema-intersection `batch_join_vars` whose all-unbound shared vars degraded
  the probe toward O(|l|·|r|)); a new required `Op::may_emit_term` provenance
  method + forced-term columns preserve the stream-wide no-Id∧Term-mix invariant
  without whole-output normalization. Design:
  `docs/specs/SPEC-20-join-probe-streaming.md`; plan:
  `docs/plans/PLAN-20-01-join-probe-streaming.md`.

  **Remaining / deferred work:**
  1. Filter-aware / grouped / multi-aggregate count pushdown (only
     COUNT-over-full-BGP is pushed down today) — **planned**:
     `docs/specs/SPEC-21-count-pushdown-extensions.md` (covers
     equality-filter inlining, grouped COUNT via an additive
     `Executor::count_bgp_grouped` seam, multi-aggregate all-plain-counts; defers
     mixed COUNT+SUM, COUNT(DISTINCT), non-equality filters, with reasons) +
     `docs/plans/PLAN-21-01-count-pushdown-extensions.md`.
  2. Streaming results out to the HTTP layer — `Runtime::run` still collects a
     `Vec<Bindings>` before serializing — **planned**:
     `docs/specs/SPEC-22-http-streaming-results.md` (new
     `Runtime::run_stream`/`BindingsStream`, all four SELECT formats stream via
     `spawn_blocking` + bounded-channel body, first chunk pre-buffered for clean
     early 400s) + `docs/plans/PLAN-22-01-http-streaming-results.md`.
  3. Hasher + join-key representation micro-opt (surfaced by the 2026-07-12
     elastic-hashing survey). Two independent pieces:
     (a) every hot hash table in the SPARQL runtime — the streaming join build
     index (`JoinState::index`, `exec/runtime.rs`), GROUP BY state, and the
     DISTINCT sets — sits on std's default SipHash hasher, while owlrl/closure
     already use `rustc-hash` (FxHash); switch these to FxHash.
     (b) the join index keys on the *decoded lexical form* of each join var
     (`HashMap<Vec<String>, _>`), one `decode_term` per build+probe row per
     jvar. That decode is a deliberate provenance choice — `row_join_key`
     "option (b)": an Id row and a Term row for the same value must land in
     the same bucket — so id-based keys need a *canonicalizing* key (encode
     `Slot::Term` back to its dictionary id when present, fall back to lex
     otherwise), not a plain `KeyPart` swap.

  See `docs/architecture.md` §9 and `docs/benchmarks.md`.

- [ ] **SPEC-12 SIMD acceleration layer.** ([#132](https://github.com/sunstoneinstitute/horndb/issues/132))
  A new stable-Rust `std::arch` SIMD layer with runtime AVX-512/AVX2/NEON dispatch +
  a scalar oracle, behind a new zero-dep leaf crate `horndb-simd`
  (`simd → storage → wcoj → …`). **Stage 1a — DONE:** the primitives crate
  (`lower_bound`/`intersect`/`merge`/`dedup`/`filter`+`filter_range`/`gather`, each
  differential-proptested bit-identical vs scalar on every host ISA path, plus the F5
  `with_forced_isa` override and the `intersect` SIMD-vs-scalar bench) landed in
  `crates/simd` (AVX2/AVX-512 on x86_64, NEON on aarch64; scalar-forced build green on
    stable 1.90). `intersect` now ships genuine wide kernels (AVX-512 `cmpeq`-mask +
  `compressstore`, AVX2 OR-reduced `cmpeq`+`movemask`, NEON `uint64x2`); `lower_bound`/
  `filter_indices_eq`/`gather` carry real intrinsics; only `merge` (all arms) and
  `filter_range`'s AVX2 arm remain scalar-equivalent behind the ISA gate. **Kernel selection
  reworked 2026-07-01** after a same-session LDBC SPB-256 A/B (Zen4 hornbench + Intel SPR
  hel01) proved the calibrated SIMD kernels **net-harmful vs scalar on both** (culprit: AVX2
  `lower_bound` on the seek-heavy leapfrog path; the "AVX-512 `intersect` ~2.5× on Intel"
  microbench claim was fiction for SPB — AVX-512 runs ~half scalar throughput there).
  Selection is now `forced → HORNDB_SIMD_MAX_ISA cap → known-CPU table (CPUID-keyed,
  SPB-derived; pins scalar for both measured hosts) → representative-input calibration
  (seek-sweep / >L2 base / moderate selectivity so an unlisted CPU also rejects the killers)
  → static widest`; the intersect skew-gate stays, and the selection tier is exported as the
  `source` label on `horndb_simd_kernel_isa{kernel,isa,source}` + the serve startup log.
  SPB-256 aggregation-qps recovered on Zen4 (28.6 regression → **36.16**, table all-scalar;
  Intel steady 34.4). See `docs/plans/PLAN-12-05-simd-cpu-table-representative-calibration.md`.
  **Stage 1b — DONE (kernels; hornbench numbers
  pending):** the WCOJ seek/intersect consumer is now live in the **production executor**.
  `executor/wcoj.rs::BatchIter`'s inlined leapfrog gains a k==2 `horndb_simd::intersect`
  fast path (mirroring the standalone `LeapfrogJoin`): when both contributing iters at a
  depth expose an `active_run` ≥ `SIMD_SEEK_MIN_RUN` (64), the whole pairwise intersection
  is precomputed once and drained, replacing per-candidate round-robin seeks. To honour the
  leapfrog's distinct-key contract, `active_run` now returns a **deduplicated** view
  (`LevelColumn::distinct_run`) — the raw SoA column keeps its duplicates for the seek
  index-mapping, but the intersect path consumes a cached distinct copy, so a subject with
  many objects still emits each leapfrog key once. Output bit-identical to scalar, gated by
  the WCOJ differential fuzzer (narrow + a new wide `N_WIDE > 64` variant that actually
  arms the intersect), the leapfrog BTreeSet oracle, and `tests/batchiter_simd.rs` (incl.
  the duplicate-subject hazard); `four_cycle` no-regress confirmed locally (WCOJ ~40× over
  binary-hash). **Remaining for NF1:** record `benches/per_tuple.rs` ≤2.5 ns/tuple on
  hornbench.
  **Stage 2 — DONE (kernels + benches; hornbench numbers pending):** `horndb-storage`
  consumes `horndb-simd` for bulk inline-int dictionary decode
  (`Dictionary::decode_inline_ints`/`lookup_inline_int_batch`/`lookup_batch`) and the
  vectorised `rdf:type` partition scan (`PredicatePartition::subjects_with_object`, built
  on the new `horndb_simd::filter_indices_eq` scan+index-compact primitive +
  `gather`; the primitive is differential-proptested bit-identical vs scalar on every
  host ISA path). The `dict_decode` (≥4×) and `partition_scan` (≥80% STREAM-Triad)
  microbenches are wired and smoke-run; the ≥4× ratio and the NUMA-pinned STREAM-Triad
  fraction are the deferred hornbench-host measurement (`docs/benchmarks.md` rows marked
  pending hornbench). The F2 "encode" stretch (vectorised `intern`) is out of scope.
  **Gated:** the delta-apply merge/dedup SIMD blocks on
  [#133](https://github.com/sunstoneinstitute/horndb/issues/133) (object index) +
  [#134](https://github.com/sunstoneinstitute/horndb/issues/134) (semi-naïve)
  and may be descoped; the `cax-sco` partition-filter scan is out of scope (superseded
  by #133). See `docs/specs/SPEC-12-simd.md`, `docs/architecture.md` §14, `docs/benchmarks.md`.

- [x] **SPEC-04: within-partition object index on `MemStore`.**
  ([#133](https://github.com/sunstoneinstitute/horndb/issues/133))
  Added `obj_index` (predicate → object → subjects) alongside `by_pred`, maintained in
  `assert`/`insert_inferred`/`clear_inferred` via `index_insert`/`index_remove` helpers,
  so `probe(None, p, Some(o))` returns O(|extent|) instead of scanning the whole
  partition. **`TripleStore` trait unchanged** — no codegen/`FireFn`/engine change, just
  `MemStore` internals — the low-risk, independently-shippable half. Turns the compiled
  `cax-sco` inner loop (and the F5 list-rule probes) from O(N) to O(|extent(c1)|).
  Spec: `docs/specs/SPEC-15-owlrl-type-index-seminaive.md` (fix #1).
  **Measured (hornbench, 2026-07-07, taxonomy d=12 / 40 k inst, graphblas):**
  `compiled_rules_ms` ~296 → ~246 ms (**−17%**), `reason_ms` ~607 → ~555 ms, RSS
  532 → 547 MiB (+2.8%), closure bit-identical (480,372 inferred); all differential
  gates green (`closure_backend_differential`, `rdf_type_skew_differential`, 177 unit
  tests). See `docs/benchmarks.md` (owlrl object index A/B row). The remaining
  cross-round re-derivation (~4×) is fix #2 (semi-naïve, [#134](https://github.com/sunstoneinstitute/horndb/issues/134)).

- [ ] **SPEC-04: genuine delta-driven semi-naïve firing for the compiled rules.**
  ([#134](https://github.com/sunstoneinstitute/horndb/issues/134))
  The compiled rules ignore their `_delta` arg (`engine.rs:127` passes `&Delta::new()`)
  and re-join the whole store every round (~12× redundant re-derivation for a depth-12
  taxonomy). Fire the n-variant delta decomposition instead: `FireFn` signature change
  (AGENTS.md §7) + `Delta` probe surface + `emit.rs` codegen + engine plumbing of the
  already-computed `applied` delta. Compounds with the object index; do **second**,
  measure between. Must stay differential-equal (closure-backend + rdf_type_skew +
  owl2-w3c-rl + acceptance #4 green). Spec:
  `docs/specs/SPEC-15-owlrl-type-index-seminaive.md` (fix #2). Gate: round/inner-loop
  work counters drop, reason-time falls.

- [ ] **SPEC-23 Phase 2: heuristic rewrite passes.** ([#202](https://github.com/sunstoneinstitute/horndb/issues/202))
  `Normalize` (`Equal→SameTerm`, constant folding), `FilterPullup` →
  `FilterPushdown` (lattice-gated legality, `LeftJoin`/`Minus` asymmetry),
  `ProjectionPushdown` — always-beneficial, no statistics, each individually
  disable-able. **Depends on #201.** Spec §5.2; plan
  `docs/plans/PLAN-23-02-heuristic-rewrite-passes.md`. Gate: slot-differential
  suite + conformance subset green with passes on and each pass off.

- [ ] **SPEC-23 Phase 3: layered `Stats` seam + Characteristic-Sets estimator.** ([#203](https://github.com/sunstoneinstitute/horndb/issues/203))
  Read-only tiered `Stats` trait over SPEC-02 (counts/NDV → Characteristic Sets →
  degree bounds → sampling hook); estimator returns `(estimate, upper_bound)`;
  memoized; wired into `EXPLAIN`; `UniformEstimator` demoted to fallback.
  **Depends on #201**; carries the SPEC-23 §8 stats-ownership open question
  (coordinate with SPEC-02/06). Spec §5.3–5.4; plan
  `docs/plans/PLAN-23-03-statistics-seam-estimator.md`. Gate: acceptance #3.

- [ ] **SPEC-23 Phase 4: cost-based `JoinPlanning`.** ([#204](https://github.com/sunstoneinstitute/horndb/issues/204))
  Structural cyclic-core hybrid (acyclic → binary hash, cyclic cores → WCOJ;
  per-subplan `ExecutionPlan` mode) + one additive i-cost/binary-cost model +
  connected-subset DP with greedy fallback and AGM guard; retires the fixed
  `wcoj_cutover == 4`. **Depends on #201–#203.** Spec §5.5; plan
  `docs/plans/PLAN-23-04-cost-based-join-planning.md`. Gates: acceptance #4–#5
  (zero result-set changes vs the WCOJ differential oracle).

## HIGH — Completeness

- [ ] **SPEC-11 SSSOM mappings + compact crosswalk index.**
  ([#130](https://github.com/sunstoneinstitute/horndb/issues/130))
  First-class support for [SSSOM](https://mapping-commons.github.io/sssom/) ontology
  crosswalks (`docs/specs/SPEC-11-mappings.md`). HornDB is the reasoning view — the
  external SoR owns SSSOM storage/ingestion (ADR-0016, data-platform ADR-0002), so
  this is the *reasoning + serving* half: (F1) SKOS/OWL/semapv mapping predicates in
  `crates/owlrl/src/vocab.rs`; (F2) n-ary `sssom:Mapping` node + positive base triple,
  negated = n-ary-only; (F3) the SSSOM chaining rules T1/RCE1-2/RI1-5/RG1-2 compiled
  into `rules.toml` with the transitive subset delegated to GraphBLAS closure (the
  RCE-N OWL rules are already entailed by `cax-*`/`scm-*`); (F4) monotone
  negative-mapping chaining (`Not` as a distinct predicate — preserves SPEC-04's
  negation-free stratification); (F5) the compact crosswalk index — rung-2 (Elias-Fano
  subjects + Frame-of-Reference bit-packed objects, ~10 B/pair bidirectional) baseline,
  rung-4 PGM target; (F6) crosswalk spine; (F7) confidence propagation (product default,
  SeMRA); (F8) chain provenance (`derived_from` = proof premises); (F9) harness-only
  SSSOM/TSV loader. **ADR-0017**: `skos:exactMatch` is a crosswalk edge, *not* OWL
  identity. Gated by a curated SSSOM conformance subset in `harness/selected.toml`;
  benched on hornbench (index ≤ ~10 B/pair; full-closure time vs the OxO2 1.16M-mapping
  / 17-min baseline). Splits into shippable increments (vocab + representation → chain
  rules → index → spine/serving). See `docs/architecture.md` §13.
  **Progress (2026-06-27, branch `spec-11-mappings-reasoning`):** the *reasoning slice*
  is implemented and green — F1 (vocab), F3 (chaining rules), F4 (negative chaining),
  F7 (confidence), F8 (provenance), F9 (harness SSSOM/TSV loader), plus the curated
  SSSOM conformance subset in `harness/selected.toml`. F2 (mapping representation) is
  **partial** — the n-ary `sssom:Mapping` node builder exists; full
  materialization-on-inference is follow-up. Still outstanding (keeps this box
  unchecked): the *serving slice* — F5 (compact crosswalk index) and F6 (crosswalk
  spine), plus GraphBLAS-backend T1 parity. Tracked as a separate serving-slice plan.

- [ ] **SPEC-23 Phase 6: reasoning in the IR.** ([#206](https://github.com/sunstoneinstitute/horndb/issues/206))
  Reasoning enters the logical IR as first-class rewrite passes (TBox expansion
  before `JoinPlanning`) + delegate nodes (`ClosureScan`/`PathClosure` → SPEC-05
  GraphBLAS), with a reasoning/materialization catalog seam parallel to `Stats`
  so materialize-vs-rewrite-vs-delegate is cost-based; property-path closure
  routed through GraphBLAS by selectivity (SPEC-07 F3 fast path). **Depends on
  #201–#204**; carries the §8 recursive-fixpoint-costing open question. Spec
  §5.8; plan `docs/plans/PLAN-23-06-reasoning-in-the-ir.md`.

- [ ] **SPEC-23 Phase 7: backward-chaining.** ([#207](https://github.com/sunstoneinstitute/horndb/issues/207))
  Magic-sets / demand transformation (SPEC-03 F4) + SLG tabling (F5) + SPARQL
  backward-chained entailment mode (SPEC-07) — the ADR-0005 hybrid
  forward/backward bet becomes real. **Depends on #206.** Plan
  `docs/plans/PLAN-23-07-backward-chaining.md`. Gate: backward answers
  result-identical to the materialized closure on the OWL 2 RL subset.

- [ ] **SPEC-24 S1: delta-incremental rule retraction.** ([#210](https://github.com/sunstoneinstitute/horndb/issues/210))
  The core DBSP bet of epic E2 (#186, closed → decomposed): thread negative
  multiplicities through the bilinear operators end-to-end, with an incremental
  `distinct` at the fixpoint boundary (per-derived-row cumulative-weight trace)
  so cyclic recursion converges; per-plan integrated input traces (`z⁻¹` state);
  `rule_attr` maintained incrementally; `recompute_rule_closure` demoted to
  differential oracle; the two `tick()` regimes collapse into one path.
  Spec: `docs/specs/SPEC-24-incremental-stage2.md` §S1. Plan: `PLAN-24-MM`
  when picked up. Gates: SPEC-24 acceptance #1 (retraction cost ∝ affected
  consequences, ≥10× over the recompute path on small-delta ticks; extended
  differential suite green; `insert_throughput` bench no regression).

- [ ] **SPEC-24 S2: delta-incremental closure retraction + exact seeded retraction.** ([#211](https://github.com/sunstoneinstitute/horndb/issues/211))
  Output-sensitive deletion on the SPEC-05 boundary (replace per-edge
  affected-region base-reachability recompute in `delete_transitive_edges` with
  a maintained support structure; keep a per-predicate recompute fallback), and
  a `seed_base_edges` variant closing the seeded-support under-withdraw
  conservatism. Spec §S2. Gates: SPEC-24 acceptance #2 (differential proptests
  green; seeded-base retraction exact).

- [ ] **SPEC-24 S4: engine wiring — SPARQL Update → Circuit → readers.** ([#213](https://github.com/sunstoneinstitute/horndb/issues/213))
  `horndb-incremental` has no consumers in the workspace; SPEC-06 acceptance
  #1–#2 are unrunnable end-to-end. Lower SPARQL Update ops to
  `assert_triple`/`retract_triple` + `tick()`; readers via `Snapshot`; rule
  registration stays a seam (owlrl wiring is E4 #188; persistence is E3 #187).
  Prefer after S3 (#212) so feed semantics settle before subscribers exist.
  Spec §S4. Gate: SPEC-24 acceptance #4 (harness-exercised `DELETE DATA`
  withdraws consequences, visible via query + change feed).

- [ ] **SPEC-24 S6: MVCC backing of `Circuit::snapshot` onto SPEC-02 per-tuple visibility.** ([#215](https://github.com/sunstoneinstitute/horndb/issues/215))
  **Blocked on E3 (#187)** per-tuple visibility + storage delete path. Bind the
  circuit `LogicalTime` to the storage tier version (design agreed with E3
  before either side builds); removes the O(n) first-acquire presence rebuild;
  makes mid-tick point reads expressible. Spec §S6. Gate: SPEC-24 acceptance #6.

## HIGH — Operational

- [ ] **Observability metrics (Phase 1): prometheus-client + `/metrics` scrape.**
  ([#148](https://github.com/sunstoneinstitute/horndb/issues/148))
  **Phase-1 Slice 1 landed** (design: `docs/specs/SPEC-17-metrics.md`;
  plan: `docs/plans/PLAN-17-01-metrics-phase1-slice1.md`). New foundational
  `horndb-metrics` crate: `prometheus-client` with typed `#[derive(EncodeLabelSet)]`
  labels (no strings), a process-global `OnceLock` registry, and free accessors —
  hot-path updates are direct atomic ops on cached handles; quantities that are
  expensive to compute (triple/dictionary/tier sizes) are pulled at scrape time via a
  `Collector`, never maintained continuously. **Slice 1 instruments:** the SPARQL HTTP
  layer (request count/latency/status via middleware + per-stage
  parse/translate/plan/exec timing + query-kind/error counters), the closure backend
  (`ClosureMetrics` → mxm/total/iterations/nnz histograms), and storage sizes; exposed
  at `GET /metrics` (OpenMetrics text, behind the `server` feature). OTel interop is
  off-box — a collector scrapes `/metrics`; no in-process OTLP push.

  **Phase-2 Slice 1 landed** (plan: `docs/plans/PLAN-17-02-metrics-phase2-slice1-owlrl.md`):
  owlrl fan-out — `OwlrlMetrics` subsystem with per-rule fire counts, per-rule + per-phase
  latency histograms, `owlrl_triples_inferred_total`, `owlrl_rounds_total`, dirty-predicate
  prune counters; closure `input_nnz` observed alongside `output_nnz`; `MemTier` enum wired
  to `storage_tier_bytes_estimated` (`tier` label, `"unknown"` until full HBM/CXL
  accounting lands). Overhead micro-bench added (`crates/metrics/benches/overhead.rs`).

  **Phase-2 Slice 2 landed** (plan: `docs/plans/PLAN-17-03-metrics-phase2-slice2-incremental.md`):
  incremental fan-out — `IncrementalMetrics` subsystem: tick-duration histogram,
  asserted/derived-merged counters, closure-withdraw/promote counters,
  fixpoint-rounds histogram; change-feed subscriber gauge maintained at subscribe +
  publish-reap.

  **Phase-2 Slice 3 landed** (plan: `docs/plans/PLAN-17-04-metrics-phase2-slice3-ml.md`):
  ml fan-out — `MlMetrics` subsystem (behind `horndb-ml`'s `server` feature):
  `horndb_ml_nl_query_total{result}` counter; `horndb_ml_prompt_tokens_total`,
  `horndb_ml_completion_tokens_total`, `horndb_ml_estimated_usd_total` counters
  (from `CostJson`); `horndb_ml_translate_duration_seconds`,
  `horndb_ml_execute_duration_seconds`, `horndb_ml_audit_query_duration_seconds`
  histograms; `horndb-metrics` is an optional dep of `horndb-ml` gated on `server`.

  **Phase-2 Slice 4 landed** (plan: `docs/plans/PLAN-17-05-metrics-phase2-slice4-wcoj.md`):
  wcoj fan-out — `WcojMetrics` subsystem: `horndb_wcoj_seeks_per_query`,
  `horndb_wcoj_iterations_per_query`, `horndb_wcoj_peak_iterators` histograms,
  all observed exactly once per query in `impl Drop for BatchIter`; inner loop
  does plain `u64` field increments only (NO per-seek timing — §5.3 compliant).

  **Phase-2 Slice 5 landed** (plan: `docs/plans/PLAN-17-06-metrics-phase2-slice5-sparql-bytes.md`):
  sparql-bytes fan-out — `horndb_sparql_request_bytes_total{endpoint}` and
  `horndb_sparql_response_bytes_total{endpoint}` counters via a `CountingBody`
  `http_body::Body` wrapper wired into the existing `record_request` middleware
  (exact data-frame byte count on end-of-stream; not a `Content-Length` guess).

  **Phase-2 fan-out is complete** — all subsystems instrumented; no remaining Phase-2 fan-out items.
  **Deferred to a later phase:** OpenTelemetry traces and logs.

## MEDIUM — Performance

- [ ] **LDBC SPB nightly: scale to true SF=0.256 + editorial agents.**
  ([#125](https://github.com/sunstoneinstitute/horndb/issues/125))
  The nightly SPB job (`.github/workflows/nightly.yml`) runs end-to-end on
  `hornbench` but only at **feasible scale** — a ~512k-triple materialized
  closure, aggregation-only, `editorialAgents=0`, so the headline metric is
  `aggregation-qps`, not the LDBC `editorial-qps`. Scale to the true SF=0.256
  (256M-triple) dataset and enable editorial (CW insert/update/delete) agents:
  materialize the 256M closure on `hornbench` and confirm both engines (HornDB
  `serve`, GraphDB Free `spb` repo) can hold it; flip `editorialAgents` on in
  `crates/harness/scenarios/spb-nightly.properties` and reconcile the nominal
  `datasetSize` (currently 18,644,617) with what is actually loaded; move the
  trend metric to `editorial-qps`. See `docs/benchmarks.md` and the SPB nightly row
  in `docs/architecture.md`.

- [ ] **SPEC-24 S7: bilinear-join runtime.** ([#216](https://github.com/sunstoneinstitute/horndb/issues/216))
  Per-predicate leaf extents for `NaryPlan` (today every leaf scans the whole
  base — also a prerequisite for S1's operator traces), cost-based join-tree
  decomposition over the SPEC-23 `Stats` seam (after #203), hash/sort-merge
  `BilinearRule` kernels replacing the O(n²) nested-loop references (codegen
  with E4 #188). Spec §S7.

## MEDIUM — Completeness

- [ ] **SPEC-24 S3: change-feed net-delta reconciliation + bounded backpressure.** ([#212](https://github.com/sunstoneinstitute/horndb/issues/212))
  Tick-local accumulation keyed `(triple, kind)`; publish only non-zero nets in
  deterministic order; `derived_merged` counts net records (the mixed-tick
  withdraw+re-add transient disappears — the pinned test flips to asserting its
  absence). Bounded `subscribe()` variant with a lag policy (`Block` /
  `DisconnectSlow`, default the latter) + drop counter metric (docs/metrics.md
  row in the same commit). Land **before** S4 (#213) creates real subscribers.
  Spec §S3. Gate: SPEC-24 acceptance #3.

## MEDIUM — Conformance

- [x] **Close the RL-reachable OWL 2 RL conformance gap.**
  ([#160](https://github.com/sunstoneinstitute/horndb/issues/160))
  The W3C `owl2-w3c-rl` subset is now **100 of 115 green** (`harness/selected.toml`);
  the 15 remaining reds are documented in `harness/KNOWN-MANIFEST-BUGS.md` and are
  all intentional OWL 2 RL non-goals (OWL 2 DL entailments / fresh-bnode TGD
  generation — explicit SPEC-04 non-goals). Both halves of the RL-reachable
  remainder landed: (1) datatype value-space *intersection* narrowing —
  `WebOnt-I5.8-008-pe`, `WebOnt-I5.8-009-pe` — via `crates/owlrl/src/datatype_ranges.rs`
  (PR #195); (2) hermetic `owl:imports` resolution — `WebOnt-imports-011-pe` — the
  harness resolves a premise's `owl:imports <IRI>` against a checked-in catalog
  (`crates/harness/tests/fixtures/owl2-w3c-rl/imports-catalog.toml`) mapping the IRI
  to a mirrored Turtle fixture, merged (transitively) before load (`crates/harness/src/rdf.rs`
  `load_premise`/`expand_imports`) — no network.

## MEDIUM — Operational

- [ ] **SPEC-24 S5: DeltaLog WAL contract + checkpoint scheduling.** ([#214](https://github.com/sunstoneinstitute/horndb/issues/214))
  Durable sequenced append behind the existing append/`drain()` shape
  (configurable fsync policy), drain paired with truncation at checkpoint,
  replay-to-identical-state recovery + crash tests; implement the SPEC-06 F8
  cadence (1 min / 100K deltas) as a real scheduler driving `Checkpoint::merge`.
  On-disk format is E3's (#187); can land against a file-backed stub. Spec §S5.
  Gate: SPEC-24 acceptance #5 (kill-and-replay bit-identical modulo timestamps).

## LOW — Completeness

- [ ] **SPEC-24 S8: intra-tick closure↔rule joint fixpoint + non-transitive closure shapes.** ([#217](https://github.com/sunstoneinstitute/horndb/issues/217))
  Iterate the closure and rule passes to a joint fixpoint so a tick's outcome is
  ordering-independent (closure→rule feedback in pure insertion ticks,
  rule→closure feedback in-tick), with ordering-independence property tests;
  extend `ClosureRule` beyond `TransitiveClosureRule` to the other SPEC-05
  shapes. Completeness tail of epic E2. Spec §S8.

## LOW — Performance

- [ ] **SPEC-23 Phase 5: later optimizer work.** ([#205](https://github.com/sunstoneinstitute/horndb/issues/205))
  Evidence-gated follow-ons behind the phase-1–4 seams: quantile/count-min
  sketches + degree-sequence bound tightening behind `Stats`, runtime filters /
  sideways information passing, the ML `PlanAdvisor` validation loop (SPEC-08
  F2, acceptance #6), Free Join / COLT and adaptive-reordering evaluations.
  **After #203/#204**; each item lands only with a measured harness win. Spec
  §5.6–5.7; plan `docs/plans/PLAN-23-05-optimizer-later-sketches-runtime-filters-ml.md`.

## LOW — Operational

- [ ] **Disk pressure during multi-agent runs.** ([#13](https://github.com/sunstoneinstitute/horndb/issues/13))
  `oxrocksdb-sys` (pulled in transitively by the harness via `oxigraph`)
  compiles a ~700 MB artifact per worktree, which exhausted free space on `/`
  during the 2026-05-24 parallel pass (surfaced as misleading "1Password failed
  to fill whole buffer" signing errors). The vendored GraphBLAS is already
  de-duplicated across worktrees; rocksdb is the remaining driver — point
  `CARGO_TARGET_DIR` at a shared path, prune the rocksdb dep, or document a
  ≥15 GB-free precondition. Stays open until rocksdb duplication is addressed.

- [ ] **1Password SSH agent reliability.** ([#14](https://github.com/sunstoneinstitute/horndb/issues/14))
  The agent intermittently returns "no identities" / "communication with agent
  failed" during long agent sessions even when the desktop app is unlocked. Fix:
  keep the app foregrounded during long sessions, or pre-cache an unencrypted
  signing key for CI. (Bypassing signing is not acceptable — global rule.)

## LOW — Maintainability

- [ ] **Extract shared `compile_bgp_patterns` helper in `crates/sparql/src/exec/horn.rs`.**
  (#TODO — file a `priority: low` / `category: maintainability` issue)
  `count_bgp`, `scan_bgp`, and `scan_bgp_ids` now share a near-identical
  pattern-compilation prologue (building `WcojPattern`s, looking up dictionary
  IDs, etc.), kept in sync by a comment. Extract a `compile_bgp_patterns` helper
  to make this a single maintenance point. Low-risk, self-contained.

## Done (for traceability)

Completed tasks; issues closed, links kept.

- [x] **CRITICAL** · _Correctness_ — SPEC-03 WCOJ over-produced on BGPs with repeated patterns (leapfrog prime-time iter sort).
- [x] **HIGH** · _Correctness_ — OWL 2 RL closure "over-derivation" vs reference on LUBM(1) ([#59](https://github.com/sunstoneinstitute/horndb/issues/59)) — was a harness-completeness gap; parity now exact (delta 0).
- [x] **HIGH** · _Maintainability_ — Workspace-wide `cargo clippy -- -D warnings` green; harness exclusion dropped from pre-push.
- [x] **HIGH** · _Performance_ — SPEC-03 WCOJ 4-cycle meets ≥10× gate ([#1](https://github.com/sunstoneinstitute/horndb/issues/1)) — ~34× on the canonical skewed win case (`SyntheticGraph::skewed_four_cycle`).
- [x] **HIGH** · _Performance_ — GraphBLAS closure backend wired + injectable into the owlrl Engine ([#61](https://github.com/sunstoneinstitute/horndb/issues/61)); profiling shows the LUBM timing gate is `rdf:type`-scan-bound (#133/#134/#39), not closure-bound.
- [x] **HIGH** · _Completeness_ — Workspace migrated to oxrdf 0.3 + end-to-end RDF 1.2 triple-term support (`<<( s p o )>>`, gated by `SparqlConfig::rdf12`).
- [x] **HIGH** · _Conformance_ — W3C RDF 1.2 N-Triples syntax subset (`rdf12-n-triples`, 4 positive + 6 negative) in `harness/selected.toml`.
- [x] **HIGH** · _Completeness_ — SPEC-07 SPARQL aggregation (`GROUP BY`/`COUNT`/`SUM`) + expanded `FILTER`/`BIND`/`IF` expressions (trainmarks-blocking) ([#66](https://github.com/sunstoneinstitute/horndb/issues/66))
- [x] **HIGH** · _Completeness_ — SPEC-07 wire SPARQL frontend onto real storage + WCOJ + materialized closure (trainmarks-blocking) ([#67](https://github.com/sunstoneinstitute/horndb/issues/67))
- [x] **HIGH** · _Completeness_ — SPEC-07 pattern-based Update (`INSERT`/`DELETE … WHERE`) (trainmarks-blocking) ([#51](https://github.com/sunstoneinstitute/horndb/issues/51))
- [x] **MEDIUM** · _Performance_ — Re-run trainmarks on hornbench post-#116 hash `LeftJoin` fix ([#177](https://github.com/sunstoneinstitute/horndb/issues/177)) — q4 cliff confirmed gone (0.334s@1M / 6.80s@10M, was ~231s / TIMEOUT); 2026-07-06 baseline recorded in `docs/benchmarks.md`.
- [x] **MEDIUM** · _Performance_ — SPEC-04 eq-rep-p skew ([#2](https://github.com/sunstoneinstitute/horndb/issues/2)) — class-canonical union-find pass (`eq_rep_p_opt.rs`), default `Optimized`; downstream `rdf:type` partition-by-class-id (F5) remains under #39. The compiled-rule `rdf:type`-scan hotspot is its own work (object index #133 + semi-naïve #134) per `docs/specs/SPEC-15-owlrl-type-index-seminaive.md` — not this (closed) eq-rep-p issue.
- [x] **MEDIUM** · _Conformance_ — W3C OWL 2 RL test-suite ingestion pipeline (`harness extract-owl2-rl`; 91 cases → 78 green in `[suites.owl2-w3c-rl]`, reds in `KNOWN-MANIFEST-BUGS.md`).
- [x] **MEDIUM** · _Completeness_ — SPEC-02 storage (HDT cold tier, CXL/NVMe tiering, MVCC, …) ([#3](https://github.com/sunstoneinstitute/horndb/issues/3))
- [x] **MEDIUM** · _Completeness_ — SPEC-04 rules (`dt-*`, `cls-maxc*`, F5 skew, …) ([#4](https://github.com/sunstoneinstitute/horndb/issues/4))
- [x] **MEDIUM** · _Completeness_ — SPEC-05 closure (retraction path, GPU backend, LAGraph) ([#5](https://github.com/sunstoneinstitute/horndb/issues/5))
- [x] **MEDIUM** · _Completeness_ — SPEC-06 incremental (retraction, MVCC) ([#6](https://github.com/sunstoneinstitute/horndb/issues/6))
- [x] **MEDIUM** · _Completeness_ — SPEC-07 SPARQL (property paths, full `Update`, GSP, `EXPLAIN`, …) ([#7](https://github.com/sunstoneinstitute/horndb/issues/7))
- [x] **MEDIUM** · _Completeness_ — SPEC-08 ML (HTTP boundary delivered; FAISS candidate generator deferred) ([#8](https://github.com/sunstoneinstitute/horndb/issues/8))
- [x] **MEDIUM** · _Completeness_ — SPEC-10 rdflib-compatible Python API (PyO3 bindings) ([#9](https://github.com/sunstoneinstitute/horndb/issues/9))
- [x] **MEDIUM** · _Conformance_ — SPEC-01 harness (full W3C/ORE/LDBC/UOBM suites; LUBM RDFox A/B wired at N=1) ([#10](https://github.com/sunstoneinstitute/horndb/issues/10))
- [x] **MEDIUM** · _Performance_ — Closure valued-reasoning readiness metrics ([#11](https://github.com/sunstoneinstitute/horndb/issues/11))
- [x] **MEDIUM** · _Performance_ — Valued-closure / custom-semiring acceleration ([#12](https://github.com/sunstoneinstitute/horndb/issues/12))
- [x] **MEDIUM** · _Tooling_ — Speed up integration test runs (parallelize and/or consolidate test targets) ([#108](https://github.com/sunstoneinstitute/horndb/issues/108))
- [x] **LOW** · _Operational_ — GraphDB Free A/B reference: per-run bring-up (supersedes systemd unit) ([#126](https://github.com/sunstoneinstitute/horndb/issues/126))
- [x] **LOW** · _Tooling_ — tasks.sh portability on macOS (flock / gawk match / GNU date) ([#78](https://github.com/sunstoneinstitute/horndb/issues/78))
- [x] **LOW** · _Tooling_ — Vendored SuiteSparse:GraphBLAS as a git submodule (`v10.3.0`, static, OpenMP, checked-in bindings); supersedes the runner-install task.
- [x] **LOW** · _Maintainability_ — Consolidated `selected.toml` into the single root file (`[sparql_query]` table).
- [x] **LOW** · _Maintainability_ — Plans/specs cross-reference cleanup (`docs/specs/README.md` Plan column).
- [x] **LOW** · _Tooling_ — CI installs GraphBLAS on runners (superseded by the vendored submodule above).
- [x] **LOW** · _Completeness_ — Wired `horndb_owlrl::Engine` to satisfy the harness `Reasoner` trait.

### Archive — project bootstrap

- [x] 9 specs written (SPEC-00..09); 9 plans (one per spec; SPEC-09 roadmap-only).
- [x] 7 implementation subagents dispatched in parallel under worktree isolation; all landed signed commits into main.
