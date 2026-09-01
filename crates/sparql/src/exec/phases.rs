//! Per-operator execution-phase instrumentation (HDB-99).
//!
//! Splits the single `exec` pipeline stage into the phases operators
//! actually spend time in — see `docs/metrics.md`'s "SPARQL execution-time
//! phases" section for what each [`ExecPhase`] value covers. Off by
//! default; set `HORNDB_EXEC_PHASES=1` to emit `horndb_sparql_exec_phase_*`.
//!
//! **Thread-local, not a [`Runtime`](crate::exec::runtime::Runtime) field.**
//! `HornBackend::scan_bgp_ids` (the WCOJ scan this module times) is an
//! `&self` method, and callers may share one `HornBackend` across query
//! threads behind an `Arc` (e.g. `bench-trainmarks`) — a `Runtime`-owned
//! accumulator would need synchronization on every phase touch instead of
//! being free of it. Each thread accumulates its own query's phases and
//! merges them into the shared counters once, on [`flush`].
//!
//! SPEC-17 §5.3 forbids per-tuple instruments; §5.4 requires local
//! accumulation merged once per phase per query. [`timed`] and [`add`] are
//! the two ways a call site contributes to that accumulation — always at
//! batch/chunk/group granularity, never per row. [`enabled`] gates all of
//! it: a `OnceLock` read is cheap enough to check every batch, but actually
//! calling `Instant::now()` is not, so a call site that cannot route through
//! [`timed`] (because it needs the timed step's return value to compute
//! `rows` itself) must check `enabled()` *before* touching the clock, not
//! after.

use horndb_metrics::labels::ExecPhase;
use std::cell::RefCell;
use std::time::{Duration, Instant};

/// Every phase this module accumulates directly, in the thread-local
/// array's index order. `ExecPhase::Residual` is excluded: [`flush`]
/// derives it as `exec_elapsed - sum(these 12)` rather than accumulating it.
const PHASE_ORDER: [ExecPhase; 12] = [
    ExecPhase::ScanWcoj,
    ExecPhase::ScanRowBuild,
    ExecPhase::ScanProvenance,
    ExecPhase::JoinBuild,
    ExecPhase::JoinProbe,
    ExecPhase::GroupKey,
    ExecPhase::GroupDecode,
    ExecPhase::AggFold,
    ExecPhase::Sort,
    ExecPhase::StreamOp,
    ExecPhase::ResultEncode,
    ExecPhase::Clock,
];

const N: usize = PHASE_ORDER.len();

fn index_of(phase: &ExecPhase) -> usize {
    PHASE_ORDER
        .iter()
        .position(|p| p == phase)
        .expect("phase must be one of PHASE_ORDER (Residual is derived, never accumulated)")
}

thread_local! {
    static ACC: RefCell<[(u64, u64); N]> = const { RefCell::new([(0, 0); N]) };
}

/// Is `HORNDB_EXEC_PHASES=1`? Read once per process (mirrors
/// `dedupe_subphases_enabled` in `exec/horn.rs`). Call sites that cannot use
/// [`timed`] directly must check this *before* calling `Instant::now()` —
/// the gate itself (`OnceLock` load + predicted branch) is cheap enough to
/// pay at batch granularity, but the clock read it guards is not free at row
/// granularity (see the "Notes for the implementer" in the HDB-99 plan).
pub(crate) fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var("HORNDB_EXEC_PHASES")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    })
}

/// Add `ns` nanoseconds and `rows` to `phase`'s running total on this
/// thread. No-op when disabled. Call once per phase per batch/chunk/group —
/// never per row.
pub(crate) fn add(phase: ExecPhase, ns: u64, rows: u64) {
    if !enabled() {
        return;
    }
    let i = index_of(&phase);
    ACC.with(|a| {
        let mut a = a.borrow_mut();
        a[i].0 += ns;
        a[i].1 += rows;
    });
}

/// Time `f`, attribute its wall-clock and `rows` to `phase`, and return its
/// result. A plain call to `f()` when disabled, so the gate costs one
/// branch — never a clock read — outside `HORNDB_EXEC_PHASES=1`.
///
/// `f` must clock only the phase's own work, never a child operator's
/// `next()`, or the exclusivity invariant `sum(named) <= exec`
/// (`tests/exec_phases.rs`) breaks silently.
pub(crate) fn timed<T>(phase: ExecPhase, rows: u64, f: impl FnOnce() -> T) -> T {
    if !enabled() {
        return f();
    }
    let start = Instant::now();
    let out = f();
    add(phase, start.elapsed().as_nanos() as u64, rows);
    out
}

/// Merge this thread's accumulated phases into the shared counters, derive
/// `residual` as `exec_elapsed - sum(named)`, and reset the accumulator for
/// this thread's next query. Call once per query, right after the `exec`
/// stage finishes: `api::timed` when `stage == Stage::Exec`, and
/// `server::query::record_exec` for the HTTP streaming path (which only
/// covers up to the first result chunk — see `docs/metrics.md`).
pub(crate) fn flush(exec_elapsed: Duration) {
    if !enabled() {
        return;
    }
    let m = horndb_metrics::metrics();
    let mut named_ns: u64 = 0;
    ACC.with(|a| {
        let mut a = a.borrow_mut();
        for (phase, (ns, rows)) in PHASE_ORDER.iter().cloned().zip(a.iter().copied()) {
            if ns == 0 && rows == 0 {
                continue;
            }
            named_ns += ns;
            m.sparql
                .record_exec_phase(phase, Duration::from_nanos(ns), rows);
        }
        *a = [(0, 0); N];
    });
    let exec_ns = exec_elapsed.as_nanos() as u64;
    let residual_ns = exec_ns.saturating_sub(named_ns);
    m.sparql
        .record_exec_phase(ExecPhase::Residual, Duration::from_nanos(residual_ns), 0);
}
