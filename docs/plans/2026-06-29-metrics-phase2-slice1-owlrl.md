# Metrics Phase 2 — Slice 1 (owlrl + cleanups) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Instrument the OWL 2 RL materialization engine (`horndb-owlrl`) with Prometheus metrics, and land two ride-along cleanups: closure `input_nnz` observation (epic #148 item 6) and the `MemTier` `tier` label on the storage tier-bytes gauge (item 7). Add a counter-overhead micro-bench skeleton (the deferred overhead open-question).

**Architecture:** Mirror the Slice-1 pattern exactly. A new `crates/metrics/src/owlrl.rs` defines an `OwlrlMetrics` struct of `prometheus-client` `Family` handles + a `register(&mut Registry) -> Self`; it is added as a field on `MetricsState` and registered in `MetricsState::new()`. The owlrl engine emits per-rule counters/histograms inline at the rule-fire site (per-rule, **not** per-tuple — within the §5.3 boundary) and aggregate counters/phase histograms once at the `materialize_with` finalization point. The closure and storage cleanups extend existing structs in place.

**Tech Stack:** Rust 1.90, `prometheus-client` 0.25 (typed `EncodeLabelSet`; lowercase label *values* via the existing `label_value_enum!` macro — the 0.25 derive does **not** rename), the existing `horndb-metrics`, `horndb-owlrl`, `horndb-closure`, `horndb-storage` crates.

**Reference spec:** `docs/specs/2026-06-29-metrics-design.md` §7.2 (fan-out), §5.3 (histogram cost boundary). Epic: GitHub #148.

**Branch:** stacked on `feat/metrics-phase1` (PR #149, unmerged). Create `feat/metrics-phase2-owlrl` off `feat/metrics-phase1`.

---

## Pre-flight (run once before Task 1)

- [ ] Confirm base branch and create the stacked branch:

```bash
cd /Users/stig/git/sunstone/horndb
git checkout feat/metrics-phase1
git pull --ff-only 2>/dev/null || true
git checkout -b feat/metrics-phase2-owlrl
```

- [ ] Sanity-check the current metrics crate compiles and tests pass (baseline):

```bash
cargo nextest run -p horndb-metrics -p horndb-owlrl -p horndb-closure -p horndb-storage
```
Expected: PASS (this is the Slice-1 baseline before any Phase-2 change).

---

## File Structure

- `crates/metrics/src/labels.rs` — add `Phase` value enum (via `label_value_enum!`) + `PhaseLabel` and `RuleLabel` label sets; add a `TierLabel` set for item 7.
- `crates/metrics/src/owlrl.rs` — **new**: `OwlrlMetrics` struct + `register`.
- `crates/metrics/src/lib.rs` — add `pub mod owlrl;`, the `pub owlrl: owlrl::OwlrlMetrics` field, and its `register` call in `MetricsState::new()`.
- `crates/metrics/src/closure.rs` — add `input_nnz` histogram + extend `observe()` signature (item 6).
- `crates/metrics/src/storage.rs` — attach `tier="unknown"` label to `storage_tier_bytes_estimated` in `StorageCollector::encode` (item 7).
- `crates/metrics/benches/overhead.rs` — **new**: criterion micro-bench for a resolved `.inc()`.
- `crates/metrics/Cargo.toml` — add the `[[bench]]` + `criterion` dev-dep.
- `crates/owlrl/Cargo.toml` — add `horndb-metrics.workspace = true`.
- `crates/owlrl/src/engine.rs` — emit metrics at the rule-fire site, prune decision, and `materialize_with` finalization.
- `crates/closure/src/metrics.rs` — pass `metrics.input_nnz` at both `emit_to_sink` call paths (item 6).
- Tests: inline `#[cfg(test)]` in `owlrl.rs`; `crates/owlrl/tests/metrics.rs` (new); extend `crates/closure/tests/metrics_sink.rs`; extend the storage collector test in `crates/metrics/src/storage.rs`.

---

## Task 1: Add owlrl label types

**Files:**
- Modify: `crates/metrics/src/labels.rs`

- [ ] **Step 1: Add the `Phase` value enum and label sets**

Append to `crates/metrics/src/labels.rs` (after the existing `MemTier` enum / label-set block). The `Phase` enum uses the existing `label_value_enum!` macro (lowercase values); `RuleLabel` uses a `String` field — `EncodeLabelValue` is implemented for `String` in prometheus-client, so the derive works and emits `rule="cax-sco"`:

```rust
label_value_enum!(Phase {
    CompiledRules => "compiled_rules",
    ListRules => "list_rules",
    ClosureBackend => "closure_backend",
    Apply => "apply",
});

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct PhaseLabel {
    pub phase: Phase,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct RuleLabel {
    pub rule: String,
}
```

- [ ] **Step 2: Build to verify the label types compile**

Run: `cargo build -p horndb-metrics`
Expected: PASS (no warnings).

- [ ] **Step 3: Commit**

```bash
git add crates/metrics/src/labels.rs
git commit -m 'feat(metrics): add owlrl Phase/Rule label types'
```

---

## Task 2: Add the `OwlrlMetrics` subsystem struct

**Files:**
- Create: `crates/metrics/src/owlrl.rs`
- Modify: `crates/metrics/src/lib.rs`
- Test: inline `#[cfg(test)]` in `crates/metrics/src/owlrl.rs`

- [ ] **Step 1: Write the new subsystem module**

Create `crates/metrics/src/owlrl.rs`. Mirror `sparql.rs` (the `latency_hist()` helper + `register` shape). Metric names omit the `horndb_` prefix (the registry adds it):

```rust
//! OWL 2 RL materialization metrics (SPEC-04). Emitted by `horndb-owlrl`:
//! per-rule fire counts and latency at the rule-fire site, and aggregate
//! counters + per-phase latency once per `materialize_with` call.

use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::histogram::{exponential_buckets, Histogram};
use prometheus_client::registry::Registry;

use crate::labels::{PhaseLabel, RuleLabel};

#[derive(Clone)]
pub struct OwlrlMetrics {
    /// Per-rule fire count (label `rule` = the W3C rule id, ~55 values).
    pub rule_fires: Family<RuleLabel, Counter>,
    /// Per-rule wall-clock per fire (per-rule, NOT per-tuple — §5.3).
    pub rule_duration_seconds: Family<RuleLabel, Histogram>,
    /// Per-phase wall-clock per materialize call (4 phases).
    pub phase_duration_seconds: Family<PhaseLabel, Histogram>,
    /// Total triples inferred across all materialize calls.
    pub triples_inferred: Counter,
    /// Total semi-naïve rounds across all materialize calls.
    pub rounds: Counter,
    /// Rule×round evaluations skipped by the dirty-predicate prune.
    pub rule_pruned: Counter,
    /// Rule×round evaluations considered (prune denominator).
    pub rule_considered: Counter,
}

fn latency_hist() -> Histogram {
    Histogram::new(exponential_buckets(1e-4, 3.0, 12))
}

impl OwlrlMetrics {
    pub fn register(reg: &mut Registry) -> Self {
        let rule_fires = Family::<RuleLabel, Counter>::default();
        let rule_duration_seconds =
            Family::<RuleLabel, Histogram>::new_with_constructor(latency_hist);
        let phase_duration_seconds =
            Family::<PhaseLabel, Histogram>::new_with_constructor(latency_hist);
        let triples_inferred = Counter::default();
        let rounds = Counter::default();
        let rule_pruned = Counter::default();
        let rule_considered = Counter::default();

        reg.register(
            "owlrl_rule_fires",
            "OWL RL rule fires by rule id",
            rule_fires.clone(),
        );
        reg.register(
            "owlrl_rule_duration_seconds",
            "OWL RL per-rule fire latency",
            rule_duration_seconds.clone(),
        );
        reg.register(
            "owlrl_phase_duration_seconds",
            "OWL RL per-phase materialize latency",
            phase_duration_seconds.clone(),
        );
        reg.register(
            "owlrl_triples_inferred",
            "Triples inferred by OWL RL materialization",
            triples_inferred.clone(),
        );
        reg.register(
            "owlrl_rounds",
            "OWL RL semi-naïve rounds",
            rounds.clone(),
        );
        reg.register(
            "owlrl_rule_pruned",
            "OWL RL rule evaluations skipped by the dirty-predicate prune",
            rule_pruned.clone(),
        );
        reg.register(
            "owlrl_rule_considered",
            "OWL RL rule evaluations considered (prune denominator)",
            rule_considered.clone(),
        );

        Self {
            rule_fires,
            rule_duration_seconds,
            phase_duration_seconds,
            triples_inferred,
            rounds,
            rule_pruned,
            rule_considered,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::labels::{Phase, PhaseLabel, RuleLabel};

    #[test]
    fn registers_and_encodes_owlrl_series() {
        let mut reg = Registry::with_prefix("horndb");
        let m = OwlrlMetrics::register(&mut reg);
        m.rule_fires
            .get_or_create(&RuleLabel { rule: "cax-sco".to_string() })
            .inc();
        m.phase_duration_seconds
            .get_or_create(&PhaseLabel { phase: Phase::Apply })
            .observe(0.001);
        m.triples_inferred.inc();

        let mut buf = String::new();
        prometheus_client::encoding::text::encode(&mut buf, &reg).unwrap();
        assert!(buf.contains("horndb_owlrl_rule_fires_total"), "got:\n{buf}");
        assert!(buf.contains("rule=\"cax-sco\""), "got:\n{buf}");
        assert!(buf.contains("phase=\"apply\""), "got:\n{buf}");
        assert!(buf.contains("horndb_owlrl_triples_inferred_total"), "got:\n{buf}");
    }
}
```

- [ ] **Step 2: Wire the module into `MetricsState`**

In `crates/metrics/src/lib.rs`: add `pub mod owlrl;` next to the other `pub mod` lines; add the field to the struct and the registration call. Edit the struct:

```rust
pub struct MetricsState {
    registry: Mutex<Registry>,
    pub sparql: sparql::SparqlMetrics,
    pub closure: closure::ClosureSink,
    pub storage: storage::StorageMetrics,
    pub owlrl: owlrl::OwlrlMetrics,
}
```

And in `MetricsState::new()`, after the `storage` registration:

```rust
let owlrl = owlrl::OwlrlMetrics::register(&mut registry);
```
then add `owlrl,` to the returned `Self { ... }`.

- [ ] **Step 3: Run the metrics tests**

Run: `cargo nextest run -p horndb-metrics`
Expected: PASS, including the new `registers_and_encodes_owlrl_series`.

- [ ] **Step 4: Commit**

```bash
git add crates/metrics/src/owlrl.rs crates/metrics/src/lib.rs
git commit -m 'feat(metrics): add OwlrlMetrics subsystem (rule/phase/round series)'
```

---

## Task 3: Emit owlrl metrics from the engine

**Files:**
- Modify: `crates/owlrl/Cargo.toml`
- Modify: `crates/owlrl/src/engine.rs`
- Test: `crates/owlrl/tests/metrics.rs` (new)

> **Implementer note:** verify current line numbers before editing — these were accurate as of the planning scan but the file may have shifted. The structural anchors are: the `for rule in RULES` loop, the `rule_relevant` prune `continue`, the `(rule.fire)(...)` call, the `eq-rep-p` special-case fire, and the `stats` value returned at the end of `materialize_with`.

- [ ] **Step 1: Add the metrics dependency**

In `crates/owlrl/Cargo.toml` under `[dependencies]`:

```toml
horndb-metrics.workspace = true
```

- [ ] **Step 2: Write the failing integration test**

Create `crates/owlrl/tests/metrics.rs`. Use the smallest existing materialize path in the crate's tests as a template (find an existing test that builds a store + backend and calls `materialize`/`reset_and_materialize`, and copy its setup). The assertion is the load-bearing part:

```rust
// Adapt the store/backend setup from an existing owlrl engine test.
// The point is: after a materialize that fires >= 1 rule, the global
// metrics registry must contain the owlrl series with a real sample.

#[test]
fn materialize_records_owlrl_metrics() {
    // <-- build `store` with a small schema that forces at least one rule
    //     to fire (e.g. an rdfs:subClassOf chain), and a closure backend,
    //     mirroring an existing test in crates/owlrl/tests or src.
    let _stats = run_a_small_materialize(); // replace with real setup

    let text = horndb_metrics::encode_metrics();
    assert!(text.contains("horndb_owlrl_rule_fires_total"), "got:\n{text}");
    assert!(text.contains("horndb_owlrl_phase_duration_seconds"), "got:\n{text}");
    assert!(text.contains("horndb_owlrl_rounds_total"), "got:\n{text}");
    // The global OnceLock is shared within the test binary, so assert a
    // sample exists rather than an exact count: parse the rounds counter.
    let rounds = parse_counter(&text, "horndb_owlrl_rounds_total");
    assert!(rounds >= 1, "expected >= 1 round recorded, got {rounds}:\n{text}");
}

/// Parse a bare `name <value>` counter line from OpenMetrics text.
fn parse_counter(text: &str, name: &str) -> u64 {
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix(name) {
            if let Some(v) = rest.trim().split_whitespace().next() {
                if let Ok(n) = v.parse::<f64>() {
                    return n as u64;
                }
            }
        }
    }
    0
}
```

> Replace `run_a_small_materialize()` with concrete setup copied from an existing owlrl test. If the crate's tests already construct a `MemStore` (or similar) + a closure backend, reuse that verbatim.

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo nextest run -p horndb-owlrl metrics`
Expected: FAIL — the series are absent because the engine does not emit yet.

- [ ] **Step 4: Emit per-rule fire count + latency at the fire site**

In `crates/owlrl/src/engine.rs`, in the `for rule in RULES` loop:

For the **eq-rep-p special case** (the `if rule.id == "eq-rep-p" && opts.eq_rep_p == EqRepPStrategy::Optimized` branch), wrap the canonical fire:

```rust
if rule_relevant(rule, dirty.as_ref(), store.vocab()) {
    stats.rule_fires += 1;
    let label = horndb_metrics::labels::RuleLabel { rule: rule.id.to_string() };
    horndb_metrics::metrics().owlrl.rule_fires.get_or_create(&label).inc();
    let t_rule = std::time::Instant::now();
    round_delta.merge(fire_eq_rep_p_canonical(store_as_dyn(store)));
    horndb_metrics::metrics()
        .owlrl
        .rule_duration_seconds
        .get_or_create(&label)
        .observe(t_rule.elapsed().as_secs_f64());
}
continue;
```

For the **general fire** (the `stats.rule_fires += 1; let d = (rule.fire)(...)` lines):

```rust
stats.rule_fires += 1;
let label = horndb_metrics::labels::RuleLabel { rule: rule.id.to_string() };
horndb_metrics::metrics().owlrl.rule_fires.get_or_create(&label).inc();
let t_rule = std::time::Instant::now();
let d = (rule.fire)(store_as_dyn(store), &Delta::new());
horndb_metrics::metrics()
    .owlrl
    .rule_duration_seconds
    .get_or_create(&label)
    .observe(t_rule.elapsed().as_secs_f64());
round_delta.merge(d);
```

- [ ] **Step 5: Emit the prune counters at the relevance check**

At the dirty-predicate prune site (the `if !rule_relevant(...) { continue; }` for non-delegated rules), count considered vs pruned. Place this so it covers every non-delegated rule evaluated in a round (after the `if rule.delegated { continue; }` skip, before/at the relevance check):

```rust
horndb_metrics::metrics().owlrl.rule_considered.inc();
if !rule_relevant(rule, dirty.as_ref(), store.vocab()) {
    horndb_metrics::metrics().owlrl.rule_pruned.inc();
    continue;
}
```

> Keep this consistent with the eq-rep-p branch: that branch also performs a `rule_relevant` check. Increment `rule_considered` once per non-delegated rule per round at a single point that dominates both branches (e.g. immediately after the `if rule.delegated { continue; }`), and increment `rule_pruned` wherever a `rule_relevant`-false causes a skip. Ensure no double counting.

- [ ] **Step 6: Emit aggregate counters + per-phase histograms at finalization**

Immediately before `stats` is returned at the end of `materialize_with`, add:

```rust
{
    let m = horndb_metrics::metrics();
    m.owlrl.triples_inferred.inc_by(stats.triples_inferred as u64);
    m.owlrl.rounds.inc_by(stats.rounds as u64);
    use horndb_metrics::labels::{Phase, PhaseLabel};
    for (phase, dur) in [
        (Phase::CompiledRules, stats.timings.compiled_rules),
        (Phase::ListRules, stats.timings.list_rules),
        (Phase::ClosureBackend, stats.timings.closure_backend),
        (Phase::Apply, stats.timings.apply),
    ] {
        m.owlrl
            .phase_duration_seconds
            .get_or_create(&PhaseLabel { phase })
            .observe(dur.as_secs_f64());
    }
}
stats
```

> `materialize_with` has a single tail `stats` return — confirm there are no early `return stats` paths; if any exist, route them through this emit block (extract a small `fn emit_owlrl(stats: &Stats)` and call it at each return).

- [ ] **Step 7: Run the test to verify it passes**

Run: `cargo nextest run -p horndb-owlrl`
Expected: PASS, including `materialize_records_owlrl_metrics`.

- [ ] **Step 8: Commit**

```bash
git add crates/owlrl/Cargo.toml crates/owlrl/src/engine.rs crates/owlrl/tests/metrics.rs
git commit -m 'feat(metrics): instrument owlrl engine (rule/phase/round/prune)'
```

---

## Task 4: Closure `input_nnz` (epic #148 item 6)

**Files:**
- Modify: `crates/metrics/src/closure.rs`
- Modify: `crates/closure/src/metrics.rs`
- Test: `crates/closure/tests/metrics_sink.rs`

- [ ] **Step 1: Extend the failing test**

In `crates/closure/tests/metrics_sink.rs`, add to the existing `closure_call_records_metrics` test (or a sibling test) an assertion for the new series:

```rust
assert!(text.contains("horndb_closure_input_nnz"), "got:\n{text}");
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo nextest run -p horndb-closure metrics_sink`
Expected: FAIL — `input_nnz` series absent.

- [ ] **Step 3: Add the histogram + extend `observe()`**

In `crates/metrics/src/closure.rs`, add an `input_nnz` histogram field next to `output_nnz`, register it (`"closure_input_nnz"`, help `"Closure input matrix nnz"`), and extend `observe` to take and record it:

```rust
pub fn observe(&self, mxm: f64, total: f64, iterations: u64, input_nnz: u64, output_nnz: u64) {
    self.mxm_seconds.observe(mxm);
    self.total_seconds.observe(total);
    self.iterations_to_fixpoint.observe(iterations as f64);
    self.input_nnz.observe(input_nnz as f64);
    self.output_nnz.observe(output_nnz as f64);
}
```

> Follow the exact constructor pattern the existing `output_nnz` histogram uses (same bucket choice). Add the field to the struct, the `register` call, and the returned `Self { ... }`.

- [ ] **Step 4: Pass `input_nnz` at the emit site**

In `crates/closure/src/metrics.rs`, update `emit_to_sink` to pass `metrics.input_nnz`:

```rust
fn emit_to_sink(metrics: &ClosureMetrics) {
    horndb_metrics::metrics().closure.observe(
        metrics.mxm_time.as_secs_f64(),
        metrics.total_time.as_secs_f64(),
        metrics.iterations_to_fixpoint as u64,
        metrics.input_nnz,
        metrics.closure_nnz,
    );
}
```

> Both call sites (the early-exit return and the normal-completion return) call `emit_to_sink`, so this single change covers both. Verify `ClosureMetrics.input_nnz` is populated before each emit.

- [ ] **Step 5: Run to verify it passes**

Run: `cargo nextest run -p horndb-metrics -p horndb-closure`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/metrics/src/closure.rs crates/closure/src/metrics.rs crates/closure/tests/metrics_sink.rs
git commit -m 'feat(metrics): observe closure input_nnz alongside output_nnz (#148 item 6)'
```

---

## Task 5: `MemTier` `tier` label on the storage tier-bytes gauge (epic #148 item 7)

**Files:**
- Modify: `crates/metrics/src/labels.rs`
- Modify: `crates/metrics/src/storage.rs`
- Test: extend the existing `collector_emits_storage_gauges` test in `storage.rs`

- [ ] **Step 1: Add the `TierLabel` set**

In `crates/metrics/src/labels.rs`, add (the `MemTier` value enum already exists):

```rust
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct TierLabel {
    pub tier: MemTier,
}
```

- [ ] **Step 2: Extend the collector test (failing)**

In `crates/metrics/src/storage.rs`, extend `collector_emits_storage_gauges` to assert the label is attached:

```rust
assert!(
    buf.contains("horndb_storage_tier_bytes_estimated{tier=\"unknown\"}"),
    "got:\n{buf}"
);
```

- [ ] **Step 3: Run to verify it fails**

Run: `cargo nextest run -p horndb-metrics storage`
Expected: FAIL — the gauge is currently emitted unlabelled.

- [ ] **Step 4: Emit the tier-bytes gauge with a label**

In `StorageCollector::encode`, pull `storage_tier_bytes_estimated` **out** of the shared unlabelled tuple loop and encode it separately with a `tier="unknown"` family. The other four gauges stay in the loop. Mirror the existing `ConstGauge` + `encode_descriptor` flow, then attach the label via the metric encoder's family method:

```rust
// after the existing loop that emits triples/graphs/predicates/dictionary_terms
{
    use crate::labels::{MemTier, TierLabel};
    let g = ConstGauge::new(snap.tier_bytes_estimated);
    let me = enc.encode_descriptor(
        "storage_tier_bytes_estimated",
        "Estimated tier bytes",
        None,
        g.metric_type(),
    )?;
    let me = me.encode_family(&TierLabel { tier: MemTier::Unknown })?;
    g.encode(me)?;
}
```

> **Verify the exact 0.25 API:** the method to attach a label set to a `MetricEncoder` from a `DescriptorEncoder` is expected to be `encode_family(&labelset)` returning a sub-`MetricEncoder`. If the method name/signature differs in prometheus-client 0.25, adjust — the compiler + the failing test are the oracle. Remove the `storage_tier_bytes_estimated` row from the shared tuple array so it is not emitted twice.

- [ ] **Step 5: Run to verify it passes**

Run: `cargo nextest run -p horndb-metrics`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/metrics/src/labels.rs crates/metrics/src/storage.rs
git commit -m 'feat(metrics): attach tier="unknown" label to storage_tier_bytes_estimated (#148 item 7)'
```

---

## Task 6: Counter-overhead micro-bench (deferred open-question)

**Files:**
- Create: `crates/metrics/benches/overhead.rs`
- Modify: `crates/metrics/Cargo.toml`

> This bench is **not** run in CI and its numbers are **not** recorded to `BENCHMARKS.md` from the laptop — it exists so the overhead can be confirmed on `hornbench`. It just needs to compile and run.

- [ ] **Step 1: Add the criterion dev-dep + bench target**

In `crates/metrics/Cargo.toml`:

```toml
[dev-dependencies]
criterion = { workspace = true }

[[bench]]
name = "overhead"
harness = false
```

> If `criterion` is not already in `[workspace.dependencies]`, reference the version another crate uses (grep an existing `benches/` crate's Cargo.toml). Prefer `workspace = true` to stay consistent.

- [ ] **Step 2: Write the bench**

Create `crates/metrics/benches/overhead.rs`:

```rust
use criterion::{criterion_group, criterion_main, Criterion};
use horndb_metrics::labels::{QueryKind, QueryKindLabel};

fn counter_inc(c: &mut Criterion) {
    let m = horndb_metrics::metrics();
    // Resolve the handle once, then time the hot-path increment.
    let handle = m
        .sparql
        .query_total
        .get_or_create(&QueryKindLabel { kind: QueryKind::Select })
        .clone();
    c.bench_function("counter_inc_resolved", |b| {
        b.iter(|| handle.inc());
    });
}

criterion_group!(benches, counter_inc);
criterion_main!(benches);
```

> Verify `QueryKind`/`QueryKindLabel` are the actual names in `labels.rs` and that `Counter` is `Clone` (it is — it is `Arc`-backed). If `query_total`'s label type differs, use whatever Slice-1 counter is simplest to resolve.

- [ ] **Step 3: Verify the bench compiles and runs (smoke only)**

Run: `cargo bench -p horndb-metrics --bench overhead -- --warm-up-time 1 --measurement-time 2`
Expected: compiles and produces a time (do NOT record the number; this is a laptop smoke check only).

- [ ] **Step 4: Commit**

```bash
git add crates/metrics/Cargo.toml crates/metrics/benches/overhead.rs
git commit -m 'bench(metrics): counter .inc() overhead micro-bench (run on hornbench)'
```

---

## Task 7: Docs sync + workspace verification

**Files:**
- Modify: `docs/architecture.md` (Observability/metrics §15)
- Modify: `TASKS.md`
- Modify: `docs/index.md` (only if a new doc pointer is needed — likely no change)

- [ ] **Step 1: Update `docs/architecture.md`**

In the Observability/metrics section (§15), update the fan-out status: owlrl instrumentation, closure `input_nnz`, and the `MemTier` `tier` label move from **planned** to **implemented**. Keep the wording consistent with the existing rows.

- [ ] **Step 2: Update `TASKS.md`**

Check off / annotate the metrics fan-out task entries that this slice completes (owlrl; closure input_nnz; MemTier tier label). Follow the TASKS.md header's mirroring procedure note (the GitHub issue tick happens in Task 8 review/finish, after the branch is green — do not create issues).

- [ ] **Step 3: Run the full verification suite**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run -p horndb-metrics -p horndb-owlrl -p horndb-closure -p horndb-storage
```
Expected: clippy clean; all tests PASS.

- [ ] **Step 4: Commit**

```bash
git add docs/architecture.md TASKS.md docs/index.md
git commit -m 'docs(metrics): record Phase-2 owlrl slice + cleanups (#148)'
```

---

## Self-Review checklist (run after all tasks, before PR)

- **Spec coverage (§7.2 owlrl):** `rule_fires_total{rule}` ✓ (Task 3), `triples_inferred_total` ✓, `rounds` ✓, per-phase histograms ✓, per-rule latency ✓, dirty-predicate prune skip rate ✓ (`rule_pruned`/`rule_considered`). Item 6 (closure input_nnz) ✓ Task 4. Item 7 (MemTier tier label) ✓ Task 5.
- **§5.3 boundary:** all timing is per-rule / per-phase / per-materialize — no per-tuple timing. ✓
- **Label cardinality:** `rule` label is bounded (~55 stable `&'static str` ids); `phase` is 4; `tier` is 4. ✓
- **No double-counting:** prune counters incremented once per non-delegated rule per round; finalization emit guarded against multiple return paths.
- **Type consistency:** `RuleLabel { rule: String }`, `PhaseLabel { phase: Phase }`, `TierLabel { tier: MemTier }`, `OwlrlMetrics` field names match between `owlrl.rs`, `lib.rs`, and `engine.rs` call sites.

## Execution handoff

Execute via **superpowers:subagent-driven-development** (Stig's standing rule): fresh implementer per task + spec-compliance review + code-quality review, then a final holistic branch review. Open a stacked PR against `feat/metrics-phase1` (base it on the right ref). **Do not merge** — Stig merges manually after saying "merge it". Tick the relevant boxes on epic #148 once the branch is green.
