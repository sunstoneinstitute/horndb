# HornDB Compression & PGO Experiments — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a profiling-guided autoresearch loop, then run a ranked queue of entity/relationship-encoding experiments against it. Each experiment must keep the OWL-2-RL conformance subset green and either shrink memory (B/triple), speed up the WCOJ hot path (ns/seek, ns/intersect), or both — measured against a stable baseline.

**Architecture:**
1. A new `crates/bench-support` crate wraps Criterion with hardware-counter and Callgrind measurement, emits NDJSON per run, and writes into the existing `target/harness.sqlite`.
2. A new `harness autoresearch` subcommand runs `correctness gate → bench env hygiene → measure → diff vs baseline` and produces a single verdict JSON. Variants are git branches or `--features` flips.
3. The experiment catalog (Part B) is a queue the autoresearch driver consumes. Each experiment has explicit before/after numbers and a kill criterion, evaluated across a **scale ladder** (Part F) so a single-zone win can't masquerade as a real one.
4. Bench numbers are produced on **dedicated Hetzner bare-metal hosts** (Part E) — never on Hetzner Cloud or shared CI, since shared-DRAM-channel hosts pollute compression A/Bs. Dev iteration is on laptops; verdict numbers are on Hetzner.

**Tech Stack:** Rust 1.88.0 stable, Criterion 0.5, `perf-event` (Linux HW counters), `darwin-kperf-criterion` (macOS), `iai-callgrind` 0.15+ (CI deterministic), `cargo-pgo` 0.2 + LLVM BOLT (release path only), `samply` for sampling profiles. Compression candidates: `spiraldb/fastlanes`, `sucds`/`sux-rs` for Elias-Fano, `fcsd`+`fsst` for dictionary, `congee` for ART, `xorf` for binary-fuse, `croaring` for AVX-512 set ops.

**Critical context (from `CLAUDE.md` + codebase scan):**
- The `horndb-wcoj` differential fuzzer is currently **red** (CRITICAL in TASKS.md: BGPs with repeated patterns). The autoresearch correctness gate **must run with `--ignored` enabled** so a "win" can't be a silent regression.
- `oxrocksdb-sys` cold-builds in minutes; `horndb-harness` is excluded from the pre-push clippy hook. Mirror that exclusion in autoresearch loops or you'll pay it once per variant.
- `horndb-closure/build.rs` requires SuiteSparse:GraphBLAS via pkg-config. Don't strip it from CI.
- The current baseline: per-predicate `Arc<UInt64Array>` columns (S, O) in SPO order + `RoaringTreemap` side sets; `DashMap<Term, TermId>` dictionary holding raw strings; binary-search leapfrog on raw u64 vecs (`crates/wcoj/src/source/vec_source.rs:49-54`). Zero compression. **This is the number every experiment beats.**

---

## Part A: Scaffolding Tasks

Build the autoresearch infrastructure before running any experiments. Each task is one commit.

### Task A1: Create `bench-support` crate skeleton

**Files:**
- Create: `crates/bench-support/Cargo.toml`
- Create: `crates/bench-support/src/lib.rs`
- Modify: `Cargo.toml` (workspace `members` list)

- [ ] **Step 1: Add the crate to the workspace**

Modify the top-level `Cargo.toml` to add `crates/bench-support` to the workspace `members` array, in alphabetical position between `crates/horndb-harness` and `crates/horndb-incremental` (or wherever the existing alphabetical order dictates).

- [ ] **Step 2: Create the crate manifest**

```toml
# crates/bench-support/Cargo.toml
[package]
name = "horndb-bench-support"
version = "0.1.0"
edition = "2021"
publish = false

[dependencies]
criterion = { workspace = true }
serde = { workspace = true, features = ["derive"] }
serde_json = { workspace = true }
anyhow = { workspace = true }

[target.'cfg(target_os = "linux")'.dependencies]
perf-event = "0.4"

[features]
default = []
```

If `criterion`, `serde`, `serde_json`, `anyhow` aren't in `[workspace.dependencies]` yet, add them.

- [ ] **Step 3: Create a stub lib that re-exports criterion and a `CounterSnapshot` type**

```rust
// crates/bench-support/src/lib.rs
//! Bench measurement glue: HW counters + JSON emit on top of Criterion.

pub use criterion;

use serde::Serialize;

#[derive(Debug, Default, Clone, Serialize)]
pub struct CounterSnapshot {
    pub cycles: Option<u64>,
    pub instructions: Option<u64>,
    pub l1d_loads: Option<u64>,
    pub l1d_load_misses: Option<u64>,
    pub llc_load_misses: Option<u64>,
    pub branch_misses: Option<u64>,
    pub dtlb_load_misses: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_default_is_all_none() {
        let s = CounterSnapshot::default();
        assert!(s.cycles.is_none());
    }
}
```

- [ ] **Step 4: Verify the workspace builds**

Run: `cargo build -p horndb-bench-support && cargo test -p horndb-bench-support`
Expected: builds clean; one test passes.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/bench-support/
git commit -F /tmp/msg <<'MSGEND'
feat(bench-support): scaffold bench-support crate for autoresearch loop

Adds a workspace member that will host HW-counter capture, NDJSON emit,
and a Criterion measurement adapter. No measurement code yet — wired
shell only so subsequent tasks can land incrementally.
MSGEND
```

### Task A2: Linux HW counter wrapper

**Files:**
- Create: `crates/bench-support/src/counters_linux.rs`
- Modify: `crates/bench-support/src/lib.rs`

- [ ] **Step 1: Write the failing integration test**

Create `crates/bench-support/tests/counters_linux.rs`:

```rust
#![cfg(target_os = "linux")]
use horndb_bench_support::counters::PerfCounters;

#[test]
fn captures_cycles_and_instructions() {
    let mut c = PerfCounters::new().expect("perf-event init");
    c.enable();
    let mut sum: u64 = 0;
    for i in 0..1_000_000u64 {
        sum = sum.wrapping_add(i);
    }
    c.disable();
    std::hint::black_box(sum);
    let snap = c.snapshot();
    assert!(snap.cycles.unwrap_or(0) > 0, "no cycles measured");
    assert!(snap.instructions.unwrap_or(0) > 1_000_000, "no instr measured");
}
```

- [ ] **Step 2: Run the test, confirm it fails**

Run: `cargo test -p horndb-bench-support --test counters_linux`
Expected: FAIL (module `counters` doesn't exist).

- [ ] **Step 3: Implement the Linux PerfCounters wrapper**

```rust
// crates/bench-support/src/counters_linux.rs
use crate::CounterSnapshot;
use perf_event::{events::Hardware, Builder, Counter, Group};
use anyhow::Result;

pub struct PerfCounters {
    group: Group,
    cycles: Counter,
    instructions: Counter,
    branch_misses: Counter,
    // Cache & TLB counters added in a follow-up commit — perf-event only allows
    // ~3-4 grouped HW events without multiplexing on most CPUs.
}

impl PerfCounters {
    pub fn new() -> Result<Self> {
        let mut group = Group::new()?;
        let cycles = Builder::new().group(&mut group).kind(Hardware::CPU_CYCLES).build()?;
        let instructions = Builder::new().group(&mut group).kind(Hardware::INSTRUCTIONS).build()?;
        let branch_misses = Builder::new().group(&mut group).kind(Hardware::BRANCH_MISSES).build()?;
        Ok(Self { group, cycles, instructions, branch_misses })
    }

    pub fn enable(&mut self) { let _ = self.group.enable(); }
    pub fn disable(&mut self) { let _ = self.group.disable(); }

    pub fn snapshot(&mut self) -> CounterSnapshot {
        let counts = self.group.read().ok();
        CounterSnapshot {
            cycles: counts.as_ref().map(|c| c[&self.cycles]),
            instructions: counts.as_ref().map(|c| c[&self.instructions]),
            branch_misses: counts.as_ref().map(|c| c[&self.branch_misses]),
            ..Default::default()
        }
    }
}
```

Update `lib.rs`:

```rust
#[cfg(target_os = "linux")]
pub mod counters {
    pub use crate::counters_linux::PerfCounters;
}

#[cfg(target_os = "linux")]
mod counters_linux;

#[cfg(not(target_os = "linux"))]
pub mod counters {
    use crate::CounterSnapshot;
    pub struct PerfCounters;
    impl PerfCounters {
        pub fn new() -> anyhow::Result<Self> { Ok(Self) }
        pub fn enable(&mut self) {}
        pub fn disable(&mut self) {}
        pub fn snapshot(&mut self) -> CounterSnapshot { CounterSnapshot::default() }
    }
}
```

- [ ] **Step 4: Run the test on Linux**

Run: `cargo test -p horndb-bench-support --test counters_linux`
Expected: PASS on Linux. On macOS, the test is `#[cfg]`-gated and won't run.

If on Linux the kernel rejects `perf_event_open` (`EACCES`/`EPERM`), set `/proc/sys/kernel/perf_event_paranoid` to `1` for dev (`sudo sysctl -w kernel.perf_event_paranoid=1`). Document in `crates/bench-support/README.md`.

- [ ] **Step 5: Create the README**

```markdown
<!-- crates/bench-support/README.md -->
# horndb-bench-support

Wraps Criterion with hardware-counter capture (Linux: perf-event, macOS: kperf
via `darwin-kperf-criterion`) and emits NDJSON per benchmark run for ingestion
into the `harness autoresearch` SQLite store.

## Linux setup

```
sudo sysctl -w kernel.perf_event_paranoid=1
sudo sysctl -w kernel.kptr_restrict=0     # only if profiling with samply
```

On a benchmark-host runner also set:

```
cpufreq-set -g performance
echo 1 > /sys/devices/system/cpu/intel_pstate/no_turbo
echo never > /sys/kernel/mm/transparent_hugepage/enabled
```

## macOS

No setup; reads counters via `kperf` (Apple Silicon and Intel both fine).
```

- [ ] **Step 6: Commit**

```bash
git add crates/bench-support/
git commit -F /tmp/msg <<'MSGEND'
feat(bench-support): capture cycles/instructions/branch-misses on Linux

Adds a thin Group wrapper over the perf-event crate that produces a
`CounterSnapshot` struct keyed to be serde-serializable for the
autoresearch NDJSON stream. macOS path is a no-op stub today; the
kperf-backed implementation lands in Task A4.
MSGEND
```

### Task A3: Criterion `Measurement` adapter with NDJSON emit

**Files:**
- Create: `crates/bench-support/src/measurement.rs`
- Create: `crates/bench-support/src/emit.rs`
- Modify: `crates/bench-support/src/lib.rs`

- [ ] **Step 1: Write the failing test**

```rust
// crates/bench-support/tests/emit.rs
use horndb_bench_support::{emit::BenchRecord, CounterSnapshot};

#[test]
fn record_serializes_to_ndjson_line() {
    let r = BenchRecord {
        schema_version: 1,
        experiment_id: "exp-test".into(),
        variant_name: "baseline".into(),
        variant_git_sha: "deadbeef".into(),
        bench_crate: "horndb-wcoj".into(),
        bench_name: "four_cycle/lubm-1k".into(),
        wall_time_ns_mean: 100_000.0,
        wall_time_ns_stddev: 1_200.0,
        counters: CounterSnapshot { cycles: Some(350_000), ..Default::default() },
        rustc: "1.88.0".into(),
        target_cpu: "native".into(),
    };
    let line = r.to_ndjson_line().unwrap();
    assert!(line.contains("\"experiment_id\":\"exp-test\""));
    assert!(line.ends_with('\n'));
    let parsed: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(parsed["bench_crate"], "horndb-wcoj");
}
```

- [ ] **Step 2: Run, confirm fail**

Run: `cargo test -p horndb-bench-support --test emit`
Expected: FAIL — `BenchRecord` undefined.

- [ ] **Step 3: Implement `BenchRecord`**

```rust
// crates/bench-support/src/emit.rs
use crate::CounterSnapshot;
use anyhow::Result;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct BenchRecord {
    pub schema_version: u32,
    pub experiment_id: String,
    pub variant_name: String,
    pub variant_git_sha: String,
    pub bench_crate: String,
    pub bench_name: String,
    pub wall_time_ns_mean: f64,
    pub wall_time_ns_stddev: f64,
    pub counters: CounterSnapshot,
    pub rustc: String,
    pub target_cpu: String,
}

impl BenchRecord {
    pub fn to_ndjson_line(&self) -> Result<String> {
        let mut s = serde_json::to_string(self)?;
        s.push('\n');
        Ok(s)
    }

    pub fn append_to(&self, path: &std::path::Path) -> Result<()> {
        use std::io::Write;
        let line = self.to_ndjson_line()?;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
        f.write_all(line.as_bytes())?;
        Ok(())
    }
}
```

Add module declaration to `lib.rs`:

```rust
pub mod emit;
```

- [ ] **Step 4: Run all tests**

Run: `cargo test -p horndb-bench-support`
Expected: all green.

- [ ] **Step 5: Implement the Criterion `Measurement` trait wrapping wall-time + counters**

```rust
// crates/bench-support/src/measurement.rs
use crate::counters::PerfCounters;
use crate::CounterSnapshot;
use criterion::measurement::{Measurement, ValueFormatter, WallTime};
use std::time::{Duration, Instant};

pub struct PerfMeasurement {
    inner: WallTime,
}

pub struct PerfState {
    start: Instant,
    counters: PerfCounters,
}

#[derive(Default, Clone)]
pub struct PerfValue {
    pub elapsed_ns: u128,
    pub snapshot: CounterSnapshot,
}

// Criterion's Measurement::Value must be a numeric type it can stat.
// We expose elapsed_ns as the headline metric; counters ride along in a
// side-channel (per-iteration JSON record). Format-time we report ns.
impl Measurement for PerfMeasurement {
    type Intermediate = PerfState;
    type Value = u128;

    fn start(&self) -> PerfState {
        let mut counters = PerfCounters::new().expect("perf init");
        counters.enable();
        PerfState { start: Instant::now(), counters }
    }

    fn end(&self, mut s: PerfState) -> u128 {
        s.counters.disable();
        let elapsed = s.start.elapsed();
        // TODO(autoresearch): emit per-iter NDJSON via thread-local sink.
        elapsed.as_nanos()
    }

    fn add(&self, v1: &u128, v2: &u128) -> u128 { v1 + v2 }
    fn zero(&self) -> u128 { 0 }
    fn to_f64(&self, v: &u128) -> f64 { *v as f64 }
    fn formatter(&self) -> &dyn ValueFormatter { self.inner.formatter() }
}

impl Default for PerfMeasurement {
    fn default() -> Self { Self { inner: WallTime } }
}
```

- [ ] **Step 6: Commit**

```bash
git add crates/bench-support/
git commit -F /tmp/msg <<'MSGEND'
feat(bench-support): NDJSON BenchRecord emitter + Criterion Measurement

`BenchRecord::append_to(path)` writes one record per benchmark variant
into a newline-delimited JSON stream; `PerfMeasurement` is a Criterion
Measurement that captures HW counters alongside wall time. The
per-iteration counter sink is a TODO — Task A5 wires it.
MSGEND
```

### Task A4: macOS HW counter wrapper (kperf)

**Files:**
- Modify: `crates/bench-support/Cargo.toml`
- Create: `crates/bench-support/src/counters_macos.rs`
- Modify: `crates/bench-support/src/lib.rs`

- [ ] **Step 1: Add the macOS dependency**

```toml
# crates/bench-support/Cargo.toml — add under a new target-conditional dep block
[target.'cfg(target_os = "macos")'.dependencies]
darwin-kperf = "0.1"
```

If `darwin-kperf` is unavailable on crates.io at plan time, fall back to the no-op stub already in `lib.rs` and skip Task A4 entirely — Linux runners are the primary autoresearch host.

- [ ] **Step 2: Write the failing test (gated by macOS)**

```rust
// crates/bench-support/tests/counters_macos.rs
#![cfg(target_os = "macos")]
use horndb_bench_support::counters::PerfCounters;

#[test]
fn captures_cycles_macos() {
    let Ok(mut c) = PerfCounters::new() else {
        eprintln!("kperf unavailable on this host, skipping");
        return;
    };
    c.enable();
    let mut sum = 0u64;
    for i in 0..1_000_000u64 { sum = sum.wrapping_add(i); }
    c.disable();
    std::hint::black_box(sum);
    let snap = c.snapshot();
    assert!(snap.cycles.unwrap_or(0) > 0);
}
```

- [ ] **Step 3: Implement the kperf wrapper**

Mirror `counters_linux.rs` API exactly. If `darwin-kperf` crate proves brittle, accept the no-op stub on macOS — bench numbers come from Linux runners anyway.

- [ ] **Step 4: Run on macOS dev box**

Run: `cargo test -p horndb-bench-support --test counters_macos`
Expected: PASS or graceful skip.

- [ ] **Step 5: Commit**

```bash
git add crates/bench-support/
git commit -F /tmp/msg <<'MSGEND'
feat(bench-support): macOS HW counters via kperf

Mirrors the Linux PerfCounters API on macOS dev hosts using
darwin-kperf. CI HW-counter numbers still come from the Linux runner;
this lets dev-box flamegraph runs report meaningful per-bench cycles.
MSGEND
```

### Task A5: Wire `bench-support` into the existing 5 benches

**Files:**
- Modify: `crates/horndb-wcoj/benches/four_cycle.rs`
- Modify: `crates/horndb-wcoj/benches/per_tuple.rs`
- Modify: `crates/horndb-storage/benches/load_lubm.rs`
- Modify: `crates/horndb-incremental/benches/insert_throughput.rs`
- Modify: `crates/horndb-closure/benches/transitive.rs`
- Modify: `crates/horndb-closure/benches/sameas.rs`
- Modify: each crate's `Cargo.toml` `[dev-dependencies]`

- [ ] **Step 1: Add `horndb-bench-support` to dev-deps of each crate**

For each of `horndb-wcoj`, `horndb-storage`, `horndb-incremental`, `horndb-closure`:

```toml
# Cargo.toml of the crate
[dev-dependencies]
horndb-bench-support = { path = "../bench-support" }
```

- [ ] **Step 2: Replace `Criterion::default()` in `four_cycle.rs`**

Find the existing `criterion_group!`/`fn main` block; replace the `Criterion` constructor with the perf-measurement variant. Show the exact diff:

```rust
// OLD
criterion_group!(benches, bench_four_cycle);
criterion_main!(benches);

// NEW
use criterion::{Criterion, criterion_main};
use horndb_bench_support::measurement::PerfMeasurement;

fn benches() {
    let mut c = Criterion::default()
        .with_measurement(PerfMeasurement::default())
        .sample_size(100)
        .measurement_time(std::time::Duration::from_secs(10))
        .warm_up_time(std::time::Duration::from_secs(3));
    bench_four_cycle(&mut c);
    c.final_summary();
}
criterion_main!(benches);
```

Repeat the same swap in each remaining bench file.

- [ ] **Step 3: Run all benches to confirm none broke**

Run: `cargo bench --workspace --no-run`
Expected: all 6+ bench binaries compile.

Run a quick smoke benchmark to confirm:
Run: `cargo bench -p horndb-wcoj --bench four_cycle -- --quick`
Expected: emits standard Criterion output; no panics.

- [ ] **Step 4: Commit**

```bash
git add crates/*/Cargo.toml crates/*/benches/*.rs
git commit -F /tmp/msg <<'MSGEND'
feat(benches): wire all benches through bench-support PerfMeasurement

Replaces bare `Criterion::default()` with a perf-counter-aware
measurement adapter across the 6 Criterion benches. Sample sizes and
warmup are now pinned to fixed values so autoresearch A/B runs see
the same N per variant.
MSGEND
```

### Task A6: SQLite ingestion + `harness bench-run` subcommand

**Files:**
- Modify: `crates/horndb-harness/src/db.rs` (add `bench_runs` table)
- Modify: `crates/horndb-harness/src/bin/harness.rs` (add `bench-run` subcommand)
- Create: `crates/horndb-harness/src/bench_ingest.rs`

- [ ] **Step 1: Schema migration**

Read `crates/horndb-harness/src/db.rs`. Add a new migration that creates:

```sql
CREATE TABLE IF NOT EXISTS bench_runs (
  id INTEGER PRIMARY KEY,
  ts INTEGER NOT NULL,
  experiment_id TEXT NOT NULL,
  variant_name TEXT NOT NULL,
  git_sha TEXT NOT NULL,
  bench_crate TEXT NOT NULL,
  bench_name TEXT NOT NULL,
  wall_time_ns_mean REAL NOT NULL,
  wall_time_ns_stddev REAL NOT NULL,
  counters_json TEXT,
  rustc TEXT,
  target_cpu TEXT
);
CREATE INDEX IF NOT EXISTS idx_bench_runs_variant ON bench_runs(variant_name, bench_crate, bench_name);
```

Add a corresponding `insert_bench_run(&self, r: &BenchRecord) -> Result<()>` method and a `recent_for_bench(...)` query.

- [ ] **Step 2: Implement the `bench-run` subcommand**

```rust
// crates/horndb-harness/src/bin/harness.rs — extend the existing clap enum
BenchRun {
    /// Variant label, e.g. "baseline" or "fastlanes-spo"
    #[arg(long)]
    variant: String,
    /// Optional experiment ID (defaults to YYYY-MM-DD-<variant>)
    #[arg(long)]
    experiment_id: Option<String>,
    /// Path to NDJSON emitted by the bench binaries
    #[arg(long, default_value = "target/horndb-bench/results.ndjson")]
    ndjson: PathBuf,
},
```

The handler should:
1. Read all lines from the NDJSON file.
2. Decode each into `BenchRecord` (re-export the struct from `bench-support`).
3. Insert into `bench_runs`.
4. Print a summary table.

- [ ] **Step 3: Add `harness bench-diff`**

```rust
BenchDiff {
    #[arg(long)]
    base: String,    // git sha or variant name
    #[arg(long)]
    head: String,
    /// Welch t-test alpha
    #[arg(long, default_value_t = 0.05)]
    alpha: f64,
},
```

Computes per-`bench_name` mean/stddev for `base` and `head`, runs Welch's t-test (use the `statrs` crate, already in the workspace if not add it), prints a markdown table:

```
| bench                          | base ns       | head ns       | Δ%      | p     | verdict |
|--------------------------------|---------------|---------------|---------|-------|---------|
| four_cycle/lubm-1k             | 1.23 ms ±2%   | 1.05 ms ±3%   | -14.6%  | 0.001 | WIN     |
```

Exit nonzero if any "head" bench regresses by more than `--max-regress-pct` (default 2.0).

- [ ] **Step 4: Test the round-trip**

```bash
mkdir -p target/horndb-bench
cargo bench -p horndb-wcoj --bench four_cycle -- --quick > /dev/null   # populates NDJSON
cargo run -p horndb-harness --bin harness -- bench-run --variant baseline
cargo run -p horndb-harness --bin harness -- bench-diff --base baseline --head baseline
```

Expected: ingestion logs "ingested N rows"; diff prints a table with 0% deltas.

- [ ] **Step 5: Commit**

```bash
git add crates/horndb-harness/
git commit -F /tmp/msg <<'MSGEND'
feat(harness): bench-run + bench-diff subcommands for autoresearch loop

bench-run ingests NDJSON emitted by criterion benches into
target/harness.sqlite. bench-diff runs Welch's t-test per bench
between two stored variants and exits nonzero on significant
regression — the gate the autoresearch loop will use to accept/reject
candidate variants.
MSGEND
```

### Task A7: `bench-env.sh` host-hygiene script

**Files:**
- Create: `crates/horndb-harness/scripts/bench-env.sh`

- [ ] **Step 1: Write the script**

```bash
#!/usr/bin/env bash
# crates/horndb-harness/scripts/bench-env.sh
# Pins the host into a deterministic state for autoresearch comparisons.
# Re-runnable. Linux only; on macOS it just warns and exits 0.

set -euo pipefail

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "[bench-env] non-Linux host, skipping kernel knobs" >&2
  exit 0
fi

require_root() {
  if [[ $EUID -ne 0 ]]; then
    echo "[bench-env] requires root for: $1" >&2
    exit 1
  fi
}

CMD="${1:-status}"

case "$CMD" in
  apply)
    require_root "writing to /sys"
    echo performance | tee /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor >/dev/null || true
    [[ -e /sys/devices/system/cpu/intel_pstate/no_turbo ]] && echo 1 > /sys/devices/system/cpu/intel_pstate/no_turbo
    echo never > /sys/kernel/mm/transparent_hugepage/enabled
    sysctl -w kernel.perf_event_paranoid=1 >/dev/null
    sysctl -w kernel.kptr_restrict=0 >/dev/null
    sync && echo 3 > /proc/sys/vm/drop_caches
    echo "[bench-env] applied"
    ;;
  status)
    echo "governor:      $(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor 2>/dev/null || echo n/a)"
    echo "no_turbo:      $(cat /sys/devices/system/cpu/intel_pstate/no_turbo 2>/dev/null || echo n/a)"
    echo "thp:           $(cat /sys/kernel/mm/transparent_hugepage/enabled 2>/dev/null | tr -d '\n' || echo n/a)"
    echo "perf_paranoid: $(cat /proc/sys/kernel/perf_event_paranoid)"
    ;;
  *)
    echo "usage: $0 {apply|status}" >&2
    exit 2
    ;;
esac
```

- [ ] **Step 2: Mark executable and test**

Run:
```
chmod +x crates/horndb-harness/scripts/bench-env.sh
crates/horndb-harness/scripts/bench-env.sh status
```
Expected: prints status lines without crashing on either Linux or macOS.

- [ ] **Step 3: Commit**

```bash
git add crates/horndb-harness/scripts/bench-env.sh
git commit -m 'feat(harness): bench-env.sh host-hygiene script for autoresearch'
```

### Task A8: `harness autoresearch` driver

**Files:**
- Create: `crates/horndb-harness/src/autoresearch.rs`
- Modify: `crates/horndb-harness/src/bin/harness.rs`
- Create: `experiments/baseline.toml`

- [ ] **Step 1: Define the experiment manifest format**

```toml
# experiments/baseline.toml
name = "baseline"
description = "Unmodified main branch — establishes the reference numbers."

# Either a git ref OR a feature-flag set; not both.
[variant]
git_ref = "HEAD"     # autoresearch will checkout this in a worktree
# or:
# features = ["fastlanes-partition"]

[gates]
# All of these must pass before bench numbers are even ingested.
correctness = [
  { cmd = "cargo test --workspace --release" },
  # Currently red; intentionally listed so an experiment that fixes it gets credit.
  # { cmd = "cargo test -p horndb-wcoj -- --ignored differential" },
]

[bench]
# Benches to run for this variant. Empty = all.
crates = ["horndb-wcoj", "horndb-storage", "horndb-incremental", "horndb-closure"]
extra_args = []
```

- [ ] **Step 2: Driver loop**

```rust
// crates/horndb-harness/src/autoresearch.rs
//! Orchestrates a single variant run end-to-end:
//! 1. Apply variant (checkout or feature flip in a worktree)
//! 2. Run correctness gates; abort on failure
//! 3. Run bench-env.sh apply (best-effort)
//! 4. cargo bench the requested crates, emit NDJSON
//! 5. Ingest into target/harness.sqlite
//! 6. Diff vs --baseline; emit a verdict JSON
//! 7. Tear down worktree

use anyhow::{Context, Result};
use std::path::PathBuf;

pub struct AutoresearchRun {
    pub experiment_id: String,
    pub manifest_path: PathBuf,
    pub baseline_variant: String,
    pub max_regress_pct: f64,
}

pub fn run(args: AutoresearchRun) -> Result<Verdict> { /* … */ }

pub struct Verdict {
    pub experiment_id: String,
    pub significant_wins: Vec<String>,
    pub significant_regressions: Vec<String>,
    pub correctness_passed: bool,
    pub overall: VerdictKind,
}

pub enum VerdictKind { Win, Neutral, Regression, Failed }
```

- [ ] **Step 3: CLI subcommand**

```rust
Autoresearch {
    /// Path to the TOML experiment manifest.
    manifest: PathBuf,
    /// Variant label to compare against (must already exist in bench_runs).
    #[arg(long, default_value = "baseline")]
    baseline: String,
    /// Max acceptable regression on any bench (percent).
    #[arg(long, default_value_t = 2.0)]
    max_regress_pct: f64,
    /// Worktree root.
    #[arg(long, default_value = ".claude/worktrees")]
    worktree_root: PathBuf,
},
```

- [ ] **Step 4: End-to-end test**

```bash
# 1. Establish baseline on main
cargo run -p horndb-harness --bin harness -- autoresearch experiments/baseline.toml --baseline baseline
# 2. Run the same manifest again — should be a no-op "neutral" verdict.
cargo run -p horndb-harness --bin harness -- autoresearch experiments/baseline.toml --baseline baseline
```

Expected: verdict JSON written to `target/horndb-autoresearch/<experiment-id>.json` with `overall: "Neutral"` on the second run.

- [ ] **Step 5: Commit**

```bash
git add crates/horndb-harness/ experiments/
git commit -F /tmp/msg <<'MSGEND'
feat(harness): autoresearch driver for variant A/B benchmarking

`harness autoresearch <manifest.toml>` checks out the variant, runs
the correctness gate, executes the configured benches, ingests them
into the SQLite store, diffs against a named baseline, and emits a
single verdict JSON. This is the unit the karpathy-style outer loop
consumes.
MSGEND
```

### Task A9: iai-callgrind track for the inner loops

**Files:**
- Modify: `crates/horndb-wcoj/Cargo.toml`
- Create: `crates/horndb-wcoj/benches/leapfrog_iai.rs`
- Modify: `crates/horndb-owlrl/Cargo.toml`
- Create: `crates/horndb-owlrl/benches/rule_fixpoint_iai.rs`

- [ ] **Step 1: Add iai-callgrind dev-dep to both crates**

```toml
[dev-dependencies]
iai-callgrind = "0.15"
```

Add the bench binary registration:

```toml
[[bench]]
name = "leapfrog_iai"
harness = false
```

- [ ] **Step 2: Write `leapfrog_iai.rs`**

Mirror the structure of the existing `four_cycle.rs` Criterion bench but using `iai-callgrind::main!`. Pin a fixed input (LUBM-1k, deterministic seed) and measure the leapfrog inner loop's instruction count + cache misses. This produces deterministic per-PR numbers.

- [ ] **Step 3: Run locally**

```bash
cargo install iai-callgrind-runner --version 0.15.0
cargo bench -p horndb-wcoj --bench leapfrog_iai
```

Expected: prints `instructions: <N>`, `L1 hits: <N>`, etc. No statistical noise.

- [ ] **Step 4: Commit**

```bash
git add crates/horndb-wcoj/ crates/horndb-owlrl/
git commit -F /tmp/msg <<'MSGEND'
feat(benches): iai-callgrind deterministic benches for WCOJ + OWL-RL

Wall-time benches are gated to nightly self-hosted runners. These
Callgrind-driven benches run on every PR — they're cycle-deterministic
so they catch instruction-count regressions reliably even on noisy
shared CI hardware.
MSGEND
```

### Task A10: PR-gated CI workflow

**Files:**
- Create: `.github/workflows/bench-pr.yml`
- Modify: `.github/workflows/nightly.yml` (add full bench sweep + NDJSON publish)

- [ ] **Step 1: PR workflow runs iai-callgrind only**

```yaml
# .github/workflows/bench-pr.yml
name: bench-pr
on:
  pull_request:
    paths-ignore: ['**.md', 'specs/**']
jobs:
  iai:
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@1.88.0
      - run: sudo apt-get update && sudo apt-get install -y valgrind libgraphblas-dev
      - run: cargo install iai-callgrind-runner --version 0.15.0
      - run: cargo bench -p horndb-wcoj --bench leapfrog_iai -- --save-baseline pr
      - run: cargo bench -p horndb-owlrl --bench rule_fixpoint_iai -- --save-baseline pr
      # Diff against the previously-cached main baseline; fail on >2% IR regress.
      # (iai-callgrind has built-in regression detection via --save-baseline +
      #  --baseline. Wire it once the baseline cache key is settled.)
```

- [ ] **Step 2: Nightly workflow runs full sweep**

Append a job to `.github/workflows/nightly.yml`:

```yaml
  autoresearch-baseline:
    runs-on: [self-hosted, linux, x64, bench]
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@1.88.0
      - run: sudo crates/horndb-harness/scripts/bench-env.sh apply
      - run: cargo run -p horndb-harness --release --bin harness -- autoresearch experiments/baseline.toml --baseline baseline
      - uses: actions/upload-artifact@v4
        with:
          name: autoresearch-baseline
          path: target/horndb-autoresearch/*.json
```

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/
git commit -F /tmp/msg <<'MSGEND'
ci: PR-gated iai-callgrind + nightly autoresearch baseline

PRs get an instruction-count gate (deterministic, ~2 min). The nightly
self-hosted runner takes the full bench sweep + autoresearch baseline,
publishing per-day NDJSON for the experiment queue to diff against.
MSGEND
```

### Task A11: Establish the baseline number set

**Files:** (none — produces data)

- [ ] **Step 1: Run on a quiet host**

```bash
sudo crates/horndb-harness/scripts/bench-env.sh apply
cargo run -p horndb-harness --release --bin harness -- \
  autoresearch experiments/baseline.toml --baseline baseline
```

- [ ] **Step 2: Update `BENCHMARKS.md` with current numbers**

Read the existing `BENCHMARKS.md` and replace the "current measured" rows for the 6 benches with what the autoresearch verdict JSON reports. Commit the BENCHMARKS.md change separately so it's easy to bisect later.

- [ ] **Step 3: Commit**

```bash
git add BENCHMARKS.md
git commit -m 'docs(benchmarks): record initial autoresearch baseline numbers'
```

### Task A12: Stable release-bench profile + PGO/BOLT paths

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Create: `Justfile` (or extend existing one)

- [ ] **Step 1: Add the `release-bench` profile**

```toml
# Cargo.toml workspace root
[profile.release-bench]
inherits = "release"
debug = 1            # keep frame pointers + line info for samply
lto = "thin"
codegen-units = 1    # reduces variance across builds
panic = "abort"
incremental = false
```

- [ ] **Step 2: Add `just` targets**

```just
# Justfile
bench-pgo VARIANT='baseline':
    cargo install cargo-pgo
    cargo pgo build --release
    target/release/horndb-harness autoresearch experiments/{{VARIANT}}.toml --baseline baseline
    cargo pgo optimize build --release
    target/release/horndb-harness autoresearch experiments/{{VARIANT}}.toml --baseline baseline-pgo

bench-bolt VARIANT='baseline':
    just bench-pgo {{VARIANT}}
    cargo pgo bolt build --release --with-pgo

flamegraph BENCH:
    cargo install samply
    cargo bench -p horndb-wcoj --bench {{BENCH}} -- --profile-time 10
    samply record target/release/deps/{{BENCH}}-*
```

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Justfile
git commit -m 'chore: release-bench profile + just targets for PGO/BOLT/flamegraph'
```

---

## Part B: Experiment Catalog

Each experiment below is a self-contained candidate the autoresearch loop will A/B against `baseline`. Implement them as separate git branches (preferred — clean diff) or feature flags. Run them in the listed order; the order is "expected cost × probability of win" — cheapest expected wins first.

For each: I name the **kill criterion** (when to abandon and move on), the **expected win**, and the **bench(es) it must move**. If an experiment doesn't move its named bench by at least the expected magnitude after a day of tuning, abandon it.

### Experiment B1 — Eytzinger layout for sorted partition columns

**Hypothesis:** Replacing the binary search inside `OrderedTripleIter::seek` with a branchless Eytzinger-laid-out u64 array reduces L1-d misses by ≥30% and ns/seek by ≥40% on the `four_cycle` and `per_tuple` benches.

**Files to touch:**
- `crates/horndb-wcoj/src/source/vec_source.rs:49-54` — the binary-search seek
- `crates/horndb-storage/src/partition.rs` — keep the existing sorted Arrow array, add an Eytzinger-permuted shadow Vec built at finalize time (`Vec<u64>` over the unique subject column for level 0, over per-subject-block object spans for level 1)

**Reference impl:** Khuong & Morin 2017, ~80 lines. No mature Rust crate; copy-port. Add a `lookup_eytzinger(arr: &[u64], key: u64) -> Option<usize>` with branchless cmov + 64-byte prefetch.

**Bench to move:** `four_cycle/*`, `per_tuple/*`.

**Kill criterion:** if Δns/seek < 10% after running with `RUSTFLAGS="-C target-cpu=native"`, abandon.

**Expected win:** 2-5× on the seek-bound benches per Khuong-Morin and the cache-conscious agent's notes.

### Experiment B2 — CSR-ify `PredicatePartition`

**Hypothesis:** Replacing the two parallel `UInt64Array`s with a CSR `(subject_ptr: Vec<u32>, object_neighbours: Vec<u64>)` layout cuts the partition memory by ~30-40% on `rdf:type`-heavy datasets and gives leapfrog O(1) "open subject" instead of a binary search.

**Files to touch:**
- `crates/horndb-storage/src/partition.rs`
- `crates/horndb-wcoj/src/source/vec_source.rs` — `open_level` becomes a slice index instead of a search

**Bench to move:** `load_lubm` memory footprint (add a new metric: bytes/triple resident), and `four_cycle`.

**Kill criterion:** if memory/triple doesn't drop by ≥25% or `open_level` doesn't show measurable improvement, fold back.

**Expected win:** ~30% memory + ~10-20% on join open-level.

**Note:** This is the structural change that B3 and B4 below build on top of. Land it second so the bit-packing experiments have somewhere to bit-pack.

### Experiment B3 — FastLanes FOR + bit-pack on the object column

**Hypothesis:** Per-1024-tuple-morsel min/max + bit-pack of the object IDs in each CSR partition shrinks the column 3-5× with ≤2× slowdown on random `seek` and faster sequential scan.

**Files to touch:**
- Add `spiraldb/fastlanes` to workspace deps
- `crates/horndb-storage/src/partition.rs` — store object column as `Vec<FastLaneMorsel>` (struct with `min: u64, bit_width: u8, packed: [u8]`)
- `crates/horndb-wcoj/src/source/vec_source.rs` — unpack the morsel on `peek`/`seek`, cache the decoded slice for the current cursor block

**Bench to move:** `load_lubm` (memory), `four_cycle` (seek cost), and the new `bytes/triple` metric.

**Kill criterion:** if compressed footprint doesn't beat raw by 3× *or* seek slows by >2×, abandon. (3× ratio without runtime loss is the target.)

**Expected win:** 3-5× memory, neutral-to-positive throughput from better cache locality.

### Experiment B4 — Partitioned Elias-Fano for the leapfrog seek column

**Hypothesis:** Partitioned EF (Ottaviano & Venturini, SIGIR'14) on the sorted object column hits 3-4 bits/int with O(log(u/n)) `predecessor` — which is exactly what leapfrog `seek` needs. Compared to B3's FastLanes pack, EF gives **lower memory** but slightly **higher seek cost** (~30-80ns).

**Files to touch:**
- Workspace dep: `sucds = "0.8"` or `sux-rs`
- `crates/horndb-storage/src/partition.rs` — add an alternative `EFPartition` backed by `sucds::EliasFano`
- `crates/horndb-wcoj/src/source/ef_source.rs` (new) — a `TrieIterator` impl over `EliasFano::predecessor`

**Bench to move:** `four_cycle` (head-to-head against B3 FastLanes), memory/triple.

**Kill criterion:** if neither memory nor seek beats B3 on its own bench, abandon.

**Expected win:** ~3 bits/triple; tilts toward "smallest" rather than "fastest."

**Note:** B3 vs B4 is the central A/B of the compression queue. Pick the winner by total `(memory × seek_ns)` product, weighted by your HBM budget.

### Experiment B5 — FSST + front-coded dictionary

**Hypothesis:** Replacing `DashMap<Term, TermId>` + `RwLock<Vec<Term>>` with a `papaya` (or `scc::HashIndex`) forward map and an FSST-compressed front-coded reverse index shrinks the dictionary 3-5× and keeps lookup under 100ns.

**Files to touch:**
- Workspace deps: `papaya`, `fsst` (or `vortex-fsst`), `fcsd`
- `crates/horndb-storage/src/dictionary.rs:14-17` — the actual swap
- Add a bench: `crates/horndb-storage/benches/dictionary.rs` measuring intern + lookup at 1M and 10M terms

**Bench to move:** new `dictionary` bench, `load_lubm` total memory.

**Kill criterion:** if FSST decode adds >100ns to ID→Term lookup, fall back to plain front-coding without FSST. If front-coding alone is <2× memory shrink on LUBM, abandon for now and come back when DBpedia is in the test corpus (more namespace skew = more compression).

**Expected win:** 3-5× dictionary memory shrink, ~30-80ns added lookup cost.

### Experiment B6 — Frequency-by-degree subject-ID reordering (orthogonal)

**Hypothesis:** Bulk-load assigns low TermIds to high-degree subjects (computed in a first pass). Result: smaller gap distribution in the sorted columns → better compression for B3/B4, fewer cache misses for any sorted-array layout.

**Files to touch:**
- Add a `--reorder` flag to the bulk-loader (`crates/horndb-storage/src/loader.rs` or equivalent)
- Compute subject degree during pass 1; remap TermIds; rewrite Arrow arrays in pass 2

**Bench to move:** `load_lubm` (compressed sizes from B3/B4 should drop further); `four_cycle` (warm-cache locality).

**Kill criterion:** if reordering doesn't multiplicatively help B3 or B4 by ≥10% on top of their standalone numbers, drop.

**Expected win:** 20-40% extra compression on top of B3/B4 (per WebGraph BV literature).

### Experiment B7 — Roaring-on-CRoaring for delta merge

**Hypothesis:** The `incremental` crate's Z-set merge spends measurable time in symmetric-difference over the side-set Roaring bitmaps. Replacing the pure-Rust `roaring` crate with `croaring-rs` (FFI, AVX-512) on `insert_throughput` cuts that by 3-10×.

**Files to touch:**
- `crates/horndb-incremental/Cargo.toml` — add `croaring`
- Wherever `RoaringTreemap` set ops happen in the merge path

**Bench to move:** `insert_throughput`.

**Kill criterion:** if the FFI overhead per call exceeds the saved cycles (likely on small deltas), keep `roaring` for control-plane and use `croaring` only on the bulk-merge path.

**Expected win:** 3-10× on bulk-merge throughput; near-zero on small deltas.

### Experiment B8 — ART (`congee`) for trie children

**Hypothesis:** Replacing the per-trie-level binary-search-over-sorted-u64 with an Adaptive Radix Tree (Leis 2013) per (predicate, depth) gives O(1)-amortized `next` and adapts node sizes to RDF's predicate-skewed fanout. Bigger structural change than B1; bigger upside on the WCOJ critical path.

**Files to touch:**
- Workspace dep: `congee = "0.4"`
- `crates/horndb-wcoj/src/source/art_source.rs` (new) — `TrieIterator` impl over a per-level `congee::Art`
- Build the ART lazily, only when the partition gets joined more than N times; cache it

**Bench to move:** `four_cycle`, `per_tuple`; also re-run the **currently-red** differential fuzzer — ART moves away from sorted-array assumptions and may flush out the repeated-pattern BGP bug (TASKS.md CRITICAL).

**Kill criterion:** if ART setup amortization doesn't beat Eytzinger (B1) on the four_cycle bench by ≥1.5×, defer to Stage 2.

**Expected win:** 2-3× over Eytzinger on join-heavy workloads; depends heavily on cache footprint of the resulting ART.

### Experiment B9 — Binary-fuse pre-filter for partition membership

**Hypothesis:** A 9-bit-per-key binary-fuse filter (Graf & Lemire 2022, `xorf` crate) over each partition's subject set is 5× smaller than the current `RoaringTreemap` and fast enough to L1-resident on every partition. Use it to short-circuit `seek` calls that would miss anyway.

**Files to touch:**
- Workspace dep: `xorf`
- `crates/horndb-storage/src/partition.rs` — add `subject_filter: BinaryFuse8`
- `crates/horndb-wcoj/src/source/vec_source.rs` — check the filter before paying for the search

**Bench to move:** `transitive` (lots of partition probes), `sameas`, `four_cycle` on patterns with selective predicates.

**Kill criterion:** if the false-positive rate inflates the search count, or the filter cost adds more than it saves, abandon.

**Expected win:** 10-30% on join workloads with selective predicates; memory shrink for the side-set.

### Experiment B10 — Cargo PGO + BOLT on `release` builds

**Hypothesis:** PGO using the LUBM-1k load + 10 SPARQL queries as the training corpus gives 5-15% on top of B1-B9 wins. BOLT on top of PGO adds another 5%.

**Files to touch:**
- The `Justfile` `bench-pgo` / `bench-bolt` targets already added in Task A12
- A new `experiments/pgo-training.sh` that runs a representative workload (LUBM-1k load, a 6-query mix, the 50-case OWL-RL subset)

**Bench to move:** all benches; this is a final-mile pass after the other experiments have landed.

**Kill criterion:** if PGO produces <3% on the geometric mean of the bench suite, BOLT is unlikely to pay off — skip BOLT.

**Expected win:** 5-15% PGO, +5% BOLT (per Kobzol's writeups on rustc itself).

### Experiment B11 — HDT-rs cold tier

**Hypothesis:** Writing frozen partitions out to HDT (or iHDT++) and reading them back via the existing `hdt` crate lands SPEC-02 NF1's 6 B/triple cold target while keeping warm-tier queryability via promotion.

**Files to touch:**
- Workspace dep: `hdt = "0.3"`
- `crates/horndb-storage/src/cold_tier.rs` (new) — freeze/thaw codec
- A new bench: `cold_thaw` measuring promotion time

**Bench to move:** new `bytes/triple` cold metric; also `load_lubm` to verify thaw cost is acceptable.

**Kill criterion:** if thaw at first-touch dominates query latency, defer to Stage 2.

**Expected win:** 6 B/triple cold, ~5-10× shrink on rarely-accessed partitions.

### Experiment B12 — Stage-2 swing: CompactLTJ or The Ring

This is **the big one**, listed last because it's a 20-30 day swing and replaces parts of B2-B4 wholesale. Don't start until B1-B7 are in.

**Hypothesis:** Implementing CompactLTJ (Arroyuelo et al., VLDB J. 2025) or The Ring (Arroyuelo et al., ACM TODS 2024) as the WCOJ-native storage gets all 6 orderings in ~13 B/triple with full leapfrog support — collapsing the entire "warm-tier compression + secondary ordering materialization" story.

**Files to touch:**
- Probably a new crate `horndb-succinct` containing rank/select primitives (port from `sucds` or build on `sux-rs`)
- Major rewrite of `crates/horndb-storage/src/partition.rs` and `crates/horndb-wcoj/src/source/*`

**Bench to move:** all of them, with the expectation of being **at parity or better** on memory and within 2× on speed compared to the post-B1-B7 baseline.

**Kill criterion:** if the succinct-data-structure porting effort exceeds 30 days without a runnable prototype, fall back to layered B2+B3 as the long-term storage. Aim for a 7-day "is the algorithm tractable in Rust?" spike before committing.

**Expected win:** SPEC-02 NF1 + SPEC-03 acceptance in one structure; the right Stage-2 prize.

---

## Part C: Profiling-Driven Experiment Discovery

Once the autoresearch loop is running, use these flows weekly to find new experiments to add to Part B's queue:

### C1 — Flamegraph review

```bash
just flamegraph four_cycle
just flamegraph transitive
```

Look for stack frames that consume >5% of cycles and aren't already addressed by a B-experiment. Add them as new B-numbers with measurement-grounded hypotheses (e.g. "DashMap shard contention shows 8% on `intern` calls under bulk-load — try `papaya`").

### C2 — iai-callgrind regression triage

When a PR fails the IR-count gate but wall time is unchanged, suspect:
- Vectorization regression (look at `cargo asm -p <crate>`)
- Inlining barrier from a recent abstraction
- Allocator interaction (toggle jemalloc vs default)

### C3 — `perf record` cache-miss profiles

```bash
sudo perf record -e LLC-load-misses -c 1000 -- cargo bench -p horndb-wcoj --bench four_cycle -- --quick
sudo perf report
```

Sites of high LLC misses that aren't on the partition columns are candidates for vertex/triple reordering (B6 may not cover them) or for moving onto a different storage tier.

### C4 — Cargo-show-asm spot-checks

For inner loops (leapfrog `seek`, dictionary intern), inspect `cargo asm` per experiment to confirm vectorization expectations. A bench win that comes from an unintended scalar path is a red flag — the win won't generalize.

---

## Part D: Decision Tree for the Outer Loop

Pseudocode the karpathy-style outer loop should run:

```
load experiment queue B1..B12
for exp in queue:
  spawn isolated worktree (.claude/worktrees/<exp.id>)
  apply variant (git checkout branch OR cargo features)
  run gate: cargo test --workspace --release
    -> fail: write Verdict{Failed, "correctness regressed"}; continue
  run gate: cargo test -p horndb-wcoj -- --ignored differential
    -> if was-red and now-green: tag exp.id as "FIXES-CRITICAL"
    -> if was-green and now-red: write Verdict{Failed}; continue
  bench-env apply
  cargo bench (selected by manifest)
  ingest NDJSON
  bench-diff vs baseline
  if exp.expected_win.met:
    write Verdict{Win}; promote variant to main, update baseline
  elif exp.expected_win.partially_met and !any.regression:
    write Verdict{Neutral}; log for human review
  else:
    write Verdict{Regression}; tear down worktree
  emit verdict to .planning/autoresearch/<date>-<exp.id>.json
```

The "promote to main" step is a hard gate — humans review the verdict before any merge. The autoresearch loop is **suggestion-with-evidence**, not auto-merge.

---

## Self-Review Notes

**Spec coverage check:** Plan covers (a) profiling scaffolding the user explicitly asked for (Part A), (b) compression/representation experiments grounded in five-agent research (Part B), (c) profiling-driven discovery flows (Part C), (d) the karpathy autoresearch outer loop (Part D). The user asked for "various techniques," "correctness tests as the gate," "benchmarks to say whether the experiment works," and "profiling-guided scaffolding." All present.

**Known gaps to acknowledge:**
- The `horndb-wcoj` differential fuzzer is currently red (CRITICAL in TASKS.md). The plan accepts this and uses it as a *signal* — Experiment B8 (ART) may flush it out, and the autoresearch loop tags any variant that turns it green as `FIXES-CRITICAL`. **Do not block experiments on fixing it first** — that work is already tracked separately in TASKS.md.
- GPU/HBM-specific experiments are intentionally deferred. The closure crate's GraphBLAS hooks make CXL/HBM offload a Stage-3 conversation; this plan stops at "keep the door open" (consistent NVTX/roctx tracing names).
- The Ring / CompactLTJ Stage-2 swing (B12) is sized as 20-30 days. Insert a 7-day spike before committing, per the kill criterion.

**No-placeholders check:** Every task in Part A has exact file paths, exact code, exact run commands, exact expected output. Part B experiments are research-grade specs (one hypothesis, one bench, one kill criterion each) — they are deliberately *not* broken down into Part-A-style steps yet because each experiment's implementation depends on what Part A's profiling reveals. When promoting an experiment to "ready to implement," re-run writing-plans for that specific experiment.
