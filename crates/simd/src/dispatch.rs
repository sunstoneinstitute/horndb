//! ISA selection and the F5 test-only override.
//!
//! Production code resolves the ISA from CPU feature detection. Tests and
//! benches use [`with_forced_isa`] to pin a path (scalar/AVX2/AVX-512/NEON)
//! regardless of the host, so every kernel the host *can* execute is exercised
//! by the differential proptests (SPEC-12 F5 / acceptance #1, #6).
//!
//! The override is exposed unconditionally (not `#[cfg(test)]`-gated) because
//! the differential integration test and the criterion bench compile this
//! crate as an ordinary dependency — i.e. *without* `cfg(test)` set on the
//! library — and must still be able to force a path. In production no caller
//! ever sets a force, so [`forced_isa`] returns `None` and each primitive's
//! `dispatch` falls straight through to its cached fn pointer.

use std::sync::OnceLock;

/// Instruction-set path a primitive can dispatch to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Isa {
    Scalar,
    Avx2,
    Avx512,
    Neon,
}

/// Which selection path chose a primitive's cached kernel. Reported alongside
/// the `(Isa, kernel)` by [`crate::calibration_report`] so fleet ops can see
/// *why* an ISA was picked — e.g. spot hosts that fell through to calibration
/// because they're absent from the known-CPU table.
///
/// Production never uses the `forced_isa` test/bench override for the cache, so
/// there is deliberately no `Forced` variant.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    /// The host matched an authoritative row in the known-CPU table.
    Table,
    /// Chosen by the startup micro-calibration (representative timing).
    Calibrated,
    /// The static widest-ISA preference (auto-tune disabled).
    Static,
}

impl Source {
    /// Stable, human-readable name: "table" / "calibrated" / "static".
    pub fn name(self) -> &'static str {
        match self {
            Source::Table => "table",
            Source::Calibrated => "calibrated",
            Source::Static => "static",
        }
    }
}

thread_local! {
    static FORCED: std::cell::Cell<Option<Isa>> = const { std::cell::Cell::new(None) };
}

/// The ISA a test/bench has forced for the current thread, or `None` in
/// production (the universal case — no caller forces a path).
#[inline]
pub fn forced_isa() -> Option<Isa> {
    FORCED.with(|c| c.get())
}

/// Run `f` with `isa` forced as the dispatch target on this thread. Restores
/// the previous value on return (even on panic — uses a drop guard).
///
/// Test-support API: used by the differential proptests and the intersect
/// bench to pin a specific ISA path regardless of the host CPU.
pub fn with_forced_isa<R>(isa: Isa, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<Isa>);
    impl Drop for Restore {
        fn drop(&mut self) {
            FORCED.with(|c| c.set(self.0));
        }
    }
    let prev = FORCED.with(|c| c.replace(Some(isa)));
    let _restore = Restore(prev);
    f()
}

// --- Operational SIMD policy (ISA cap + auto-tune), seeded via `configure` ---
//
// Two process-wide knobs govern the *production* detection path: an ISA-tier
// ceiling (the "cap") and the startup auto-calibration toggle. Both are seeded
// once through [`configure`] before the first dispatch — the `serve` binary
// calls it from the resolved `[simd]` config. `crates/simd` reads **no**
// environment variable itself; the old `HORNDB_SIMD_MAX_ISA` /
// `HORNDB_SIMD_AUTOTUNE` reads moved up to `horndb-config`, reachable as
// `HORNDB_SIMD__MAX_ISA` / `HORNDB_SIMD__AUTOTUNE` (double-underscore). When
// `configure` is never called (benches, unit tests, any embedder that skips
// it), each cell falls back to its auto-detect default: no cap, auto-tune on.
//
// Unlike `forced_isa` (a thread-local *force* used only by tests/benches), the
// cap is a global *tier* ceiling meant as an ops knob: a cap of `Avx2` disables
// AVX-512 fleet-wide without a rebuild, and `Scalar` turns SIMD off entirely.
// The cap is a width *tier*, not an exact ISA: scalar < {avx2, neon} < avx512.
// It does NOT affect `forced_isa`, so the differential proptests still exercise
// every kernel the host can run.

/// Process-wide ISA cap, seeded by [`configure`]. `None` (once resolved) means
/// "no cap". Defaults to `None` when `configure` is never called.
static ISA_CAP: OnceLock<Option<Isa>> = OnceLock::new();

/// Startup auto-calibration toggle, seeded by [`configure`]. Defaults to `true`
/// when `configure` is never called.
static AUTOTUNE: OnceLock<bool> = OnceLock::new();

/// Seed the process-wide SIMD policy — the ISA cap and the auto-tune toggle —
/// **before any primitive dispatches or is primed**. `serve` calls this once,
/// right after config resolution, from the resolved `[simd]` values.
///
/// `max_isa` is the width-tier ceiling (`None` = no cap); `autotune` enables the
/// startup micro-calibration (the default when this is never called).
///
/// **Contract — call exactly once, first.** Both values live in `OnceLock`s that
/// each primitive resolves lazily on first dispatch. A *second* call, or any
/// call *after* the first dispatch/priming has already resolved a cell, cannot
/// re-seed it: the late value is **silently ignored** (this is `OnceLock`
/// semantics, not an error path). A `debug_assert` fires in debug builds if
/// either cell was already initialised, to catch a mis-ordered caller; release
/// builds treat the late call as a no-op. Embedders that never call `configure`
/// (benches, unit tests) get the auto-detect defaults: no cap, auto-tune on.
pub fn configure(max_isa: Option<Isa>, autotune: bool) {
    let cap_fresh = ISA_CAP.set(max_isa).is_ok();
    let autotune_fresh = AUTOTUNE.set(autotune).is_ok();
    debug_assert!(
        cap_fresh && autotune_fresh,
        "horndb_simd::configure() ran after the SIMD policy cells were already \
         resolved (called twice, or after the first dispatch/priming) — the seed \
         was ignored; call configure() once, before any primitive dispatches"
    );
}

/// Width tier used to compare ISAs for the cap. Cross-arch values never meet on
/// one host (an x86 box has no NEON kernels and vice-versa); the tier just lets
/// a single cap value behave sensibly on either arch.
fn tier(isa: Isa) -> u8 {
    match isa {
        Isa::Scalar => 0,
        Isa::Avx2 | Isa::Neon => 1,
        Isa::Avx512 => 2,
    }
}

/// Parse a `max_isa` string into the ISA-cap tier (case-insensitive).
/// Unrecognised values yield `None`, which the caller distinguishes from
/// "unset": `serve` treats an unknown non-empty string as a startup-fatal
/// error rather than silently dropping the cap. Accepts `scalar` (also `none`
/// / `off`), `avx2`, `avx512` (also `avx512f` / `avx-512`), and `neon`.
///
/// Lives here so the string spellings stay next to the [`Isa`] enum; `serve`
/// (which owns config→enum translation, keeping this crate a leaf) calls it and
/// decides the fatal-on-unknown policy.
pub fn parse_max_isa(s: &str) -> Option<Isa> {
    match s.trim().to_ascii_lowercase().as_str() {
        "scalar" | "none" | "off" => Some(Isa::Scalar),
        "avx2" => Some(Isa::Avx2),
        "avx512" | "avx512f" | "avx-512" => Some(Isa::Avx512),
        "neon" => Some(Isa::Neon),
        _ => None,
    }
}

/// The configured cap (seeded by [`configure`], else the `None` default).
fn isa_cap() -> Option<Isa> {
    *ISA_CAP.get_or_init(|| None)
}

/// Pure cap check (testable without touching the global): is `isa` permitted
/// under `cap`? Scalar is always permitted.
fn cap_allows(isa: Isa, cap: Option<Isa>) -> bool {
    match cap {
        Some(c) => tier(isa) <= tier(c),
        None => true,
    }
}

/// Whether the production detection path may select `isa`, honouring the seeded
/// ISA cap. Each primitive's `resolve` guards its feature-detection arms with
/// this; the test/bench `forced_isa` override deliberately bypasses it.
pub(crate) fn allows(isa: Isa) -> bool {
    cap_allows(isa, isa_cap())
}

/// The operational ISA cap seeded via [`configure`], or `None` if uncapped
/// (or if `configure` was never called).
///
/// Exposed so a host can log the effective SIMD policy at startup, e.g.
/// `tracing::info!(cap = ?horndb_simd::configured_max_isa(), "SIMD dispatch")`.
/// It is a width *tier* (scalar < avx2 ≈ neon < avx512): a cap of `Avx2` lets
/// AVX2/NEON kernels run but suppresses AVX-512; `Scalar` disables all SIMD.
pub fn configured_max_isa() -> Option<Isa> {
    isa_cap()
}

// --- Startup auto-calibration toggle ---------------------------------------
//
// Per-host kernel benchmarks proved the fastest ISA is host-dependent (AVX-512
// `intersect` wins 2.5x on Sapphire Rapids but loses 2.5x on Zen4, etc.) with
// no cheap runtime bit to tell the cases apart. So each primitive can
// micro-calibrate at startup: time every available kernel and cache the
// fastest. The behaviour is on by default and disabled by seeding
// `autotune = false` through [`configure`], which falls back to the static
// widest-ISA preference. The ISA cap still bounds the candidate set either way.

/// Whether startup micro-calibration is enabled (seeded by [`configure`], else
/// the `true` default).
pub(crate) fn autotune_enabled() -> bool {
    *AUTOTUNE.get_or_init(|| true)
}

/// Whether startup micro-calibration is enabled (default on). Exposed so a host
/// can log the effective SIMD policy at startup alongside [`configured_max_isa`].
/// When off, each primitive uses its static widest-ISA preference; the ISA cap
/// applies in both modes.
pub fn configured_autotune() -> bool {
    autotune_enabled()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forced_isa_overrides_within_closure() {
        assert_eq!(forced_isa(), None);
        with_forced_isa(Isa::Scalar, || {
            assert_eq!(forced_isa(), Some(Isa::Scalar));
        });
        assert_eq!(
            forced_isa(),
            None,
            "override must not leak past the closure"
        );
    }

    #[test]
    fn no_cap_allows_everything() {
        for isa in [Isa::Scalar, Isa::Avx2, Isa::Avx512, Isa::Neon] {
            assert!(cap_allows(isa, None), "{isa:?}");
        }
    }

    #[test]
    fn avx2_cap_disables_avx512_keeps_the_rest() {
        let cap = Some(Isa::Avx2);
        assert!(cap_allows(Isa::Scalar, cap));
        assert!(cap_allows(Isa::Avx2, cap));
        assert!(cap_allows(Isa::Neon, cap)); // same tier as avx2
        assert!(!cap_allows(Isa::Avx512, cap), "avx512 must be capped out");
    }

    #[test]
    fn scalar_cap_disables_all_simd() {
        let cap = Some(Isa::Scalar);
        assert!(cap_allows(Isa::Scalar, cap));
        for isa in [Isa::Avx2, Isa::Avx512, Isa::Neon] {
            assert!(!cap_allows(isa, cap), "{isa:?} must be capped out");
        }
    }

    #[test]
    fn parse_max_isa_accepts_known_spellings() {
        assert_eq!(parse_max_isa("scalar"), Some(Isa::Scalar));
        assert_eq!(parse_max_isa("AVX2"), Some(Isa::Avx2));
        assert_eq!(parse_max_isa(" avx512 "), Some(Isa::Avx512));
        assert_eq!(parse_max_isa("avx512f"), Some(Isa::Avx512));
        assert_eq!(parse_max_isa("neon"), Some(Isa::Neon));
        assert_eq!(parse_max_isa("garbage"), None);
        assert_eq!(parse_max_isa(""), None);
    }
}
