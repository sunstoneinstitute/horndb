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

- [ ] **HIGH** · _Performance_ — SPARQL aggregation runtime: id-based bindings + hash group-by + streaming (12× SPB gap) ([#128](https://github.com/sunstoneinstitute/horndb/issues/128))
- [ ] **HIGH** · _Performance_ — SPEC-12 SIMD layer: `horndb-simd` primitives crate **landed** (F4+F5); WCOJ seek/intersect consumer (F1) **landed** — `VecIter` SoA-column + `PackedColumn` block-finish seek through `horndb_simd::lower_bound`, `LeapfrogJoin` k==2 `horndb_simd::intersect` fast path, real `per_tuple` microbench wired (differential fuzzer + leapfrog oracle green); storage decode + `rdf:type` scan consumer (F2) **landed** — `Dictionary::decode_inline_ints`/`lookup_batch` bulk inline-int decode + `PredicatePartition::subjects_with_object` via the new `horndb_simd::filter_indices_eq` primitive, `dict_decode`/`partition_scan` benches wired. SIMD intersect now wired into `BatchIter`'s inlined leapfrog (the production executor hot path; `active_run` deduplicates to honour the distinct-key contract). Real wide `intersect` kernels (AVX-512 `compressstore`/AVX2/NEON) **landed**; `intersect`/`lower_bound`/`gather`/`filter_indices_eq` benched on **Intel SPR + Zen4** (2026-06-30): intersect AVX-512 ~2.5× on Intel (regresses on Zen4 double-pump), lower_bound a scalar win on both, gather + sparse filter ~1.5–2.2× wins. **Kernel selection reworked (2026-07-01) after the real workload contradicted the microbenches:** a same-session LDBC SPB-256 A/B on Zen4 (hornbench) and Intel SPR (hel01) showed the calibrated SIMD kernels are **net-harmful vs scalar on both** (dominant culprit: AVX2 `lower_bound` on the seek-heavy leapfrog path; the "AVX-512 intersect ~2.5× on Intel" microbench claim was fiction for SPB — AVX-512 runs at ~half scalar throughput there). **Fixed:** kernel selection is now `forced → HORNDB_SIMD_MAX_ISA cap → known-CPU table (CPUID-keyed, SPB-derived) → representative-input calibration → static widest`; the known-CPU table pins scalar for both measured hosts (AMD fam 25 mdl 97 Ryzen 7 7700, Intel fam 6 mdl 143 Xeon Gold 5412U), representative calibration (seek-sweep / >L2 base / moderate selectivity) makes an unlisted CPU reject the killer kernels too, the intersect skew-gate stays, and the selection tier is exported as the `source` label on `horndb_simd_kernel_isa{kernel,isa,source}` + the serve startup log. **SPB-256 aggregation-qps recovered on Zen4: 28.6 (SIMD regression) → 36.16** (table, all scalar; +18% over the 30.6 pre-SIMD baseline); Intel steady at 34.4. **`per_tuple` measured on hornbench (2026-06-30): ~67 ns/tuple, unchanged by the intersect (criterion A/B “no change”) — NF1 ≤2.5 ns not met; bottleneck is the depth-1 narrow-run leapfrog + Arrow materialization, not the intersect.** **hornbench numbers recorded (2026-07-07, Ryzen 7 7700, node-0-pinned):** `dict_decode` scalar 14.74 µs vs AVX2 14.54 µs → **~1.01×, RED** (load/store-bound; NF4 ≥4× is a compute target the memory-bound loop can't reach — SIMD not the lever); `partition_scan` **34.5 GB/s = ~104% of STREAM-Triad (33.1 GB/s full-socket) → GREEN** (SPEC-02 acceptance #4 met). **Remaining:** close NF1 `per_tuple` (depth-1 / materialization path — not SIMD); delta-apply (F3) consumer (gated on [#133](https://github.com/sunstoneinstitute/horndb/issues/133)) ([#132](https://github.com/sunstoneinstitute/horndb/issues/132))
- [ ] **HIGH** · _Performance_ — SPEC-04: within-partition object index on `MemStore` so `rdf:type` probes are O(|extent|) ([#133](https://github.com/sunstoneinstitute/horndb/issues/133))
- [ ] **HIGH** · _Performance_ — SPEC-04: genuine delta-driven semi-naïve firing for the compiled rules ([#134](https://github.com/sunstoneinstitute/horndb/issues/134))
- [ ] **HIGH** · _Completeness_ — SPEC-11 SSSOM mappings + compact crosswalk index ([#130](https://github.com/sunstoneinstitute/horndb/issues/130))
- [ ] **HIGH** · _Operational_ — Observability metrics (Phase 1): prometheus-client + `/metrics` scrape; Slice 1 (SPARQL HTTP + closure + storage) landed, fan-out remaining ([#148](https://github.com/sunstoneinstitute/horndb/issues/148))
- [ ] **MEDIUM** · _Performance_ — LDBC SPB nightly: scale to true SF=0.256 (256M triples) + editorial agents ([#125](https://github.com/sunstoneinstitute/horndb/issues/125))
- [x] **MEDIUM** · _Conformance_ — Close the RL-reachable OWL 2 RL gap: datatype value-space intersection + `owl:imports` (97/115 → 100/115) ([#160](https://github.com/sunstoneinstitute/horndb/issues/160))
- [ ] **LOW** · _Operational_ — Disk pressure during multi-agent runs (rocksdb) ([#13](https://github.com/sunstoneinstitute/horndb/issues/13))
- [ ] **LOW** · _Operational_ — 1Password SSH agent reliability ([#14](https://github.com/sunstoneinstitute/horndb/issues/14))
- [ ] **LOW** · _Maintainability_ — Extract shared `compile_bgp_patterns` helper in `crates/sparql/src/exec/horn.rs` (#TODO)

Closed tasks are listed in [Done](#done-for-traceability).

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

- [ ] **SPEC-04: within-partition object index on `MemStore`.**
  ([#133](https://github.com/sunstoneinstitute/horndb/issues/133))
  Add `obj_index` (predicate → object → subjects) alongside `by_pred`, maintained in
  `assert`/`insert_inferred`/`clear_inferred`, so `probe(None, p, Some(o))` returns
  O(|extent|) instead of scanning the whole partition. **`TripleStore` trait unchanged**
  — no codegen/`FireFn`/engine change, just `MemStore` internals — so this is the
  low-risk, independently-shippable half. Turns the compiled `cax-sco` inner loop (and
  the F5 list-rule probes) from O(N) to O(|extent(c1)|). Ship **first**.
  Spec: `docs/specs/SPEC-15-owlrl-type-index-seminaive.md` (fix #1). Gate:
  `compiled_rules_ms` drop on the owlrl materialize A/B LUBM-shaped row + resident-set
  delta recorded in `docs/benchmarks.md`; all differential gates stay green.

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
