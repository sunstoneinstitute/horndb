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

/// Instruction-set path a primitive can dispatch to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Isa {
    Scalar,
    Avx2,
    Avx512,
    Neon,
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

// --- Operational ISA cap (HORNDB_SIMD_MAX_ISA) -----------------------------
//
// A process-wide ceiling on the ISA the *production* detection path may pick,
// read once from the environment. Unlike `forced_isa` (a thread-local *force*
// used only by tests/benches), this is a global *cap* meant as an ops knob:
// e.g. `HORNDB_SIMD_MAX_ISA=avx2` disables AVX-512 fleet-wide without a
// rebuild (the AVX-512 downclocking question is a per-deployment property),
// and `HORNDB_SIMD_MAX_ISA=scalar` turns SIMD off entirely — a clean escape
// hatch for isolating a suspected kernel regression in production.
//
// The cap is a width *tier*, not an exact ISA: scalar < {avx2, neon} < avx512.
// It does NOT affect `forced_isa`, so the differential proptests still exercise
// every kernel the host can run even when the variable is set in the shell.

/// Width tier used to compare ISAs for the cap. Cross-arch values never meet on
/// one host (an x86 box has no NEON kernels and vice-versa); the tier just lets
/// a single `HORNDB_SIMD_MAX_ISA` value behave sensibly on either arch.
fn tier(isa: Isa) -> u8 {
    match isa {
        Isa::Scalar => 0,
        Isa::Avx2 | Isa::Neon => 1,
        Isa::Avx512 => 2,
    }
}

/// Parse a `HORNDB_SIMD_MAX_ISA` value (case-insensitive). Unrecognised values
/// yield `None` (treated as "no cap"). Accepts `scalar`, `avx2`, `avx512`
/// (and the `avx512f`/`avx-512` spellings), and `neon`.
fn parse_isa(s: &str) -> Option<Isa> {
    match s.trim().to_ascii_lowercase().as_str() {
        "scalar" | "none" | "off" => Some(Isa::Scalar),
        "avx2" => Some(Isa::Avx2),
        "avx512" | "avx512f" | "avx-512" => Some(Isa::Avx512),
        "neon" => Some(Isa::Neon),
        _ => None,
    }
}

/// The configured cap, read once from `HORNDB_SIMD_MAX_ISA`, or `None`.
fn isa_cap() -> Option<Isa> {
    use std::sync::OnceLock;
    static CAP: OnceLock<Option<Isa>> = OnceLock::new();
    *CAP.get_or_init(|| {
        std::env::var("HORNDB_SIMD_MAX_ISA")
            .ok()
            .and_then(|v| parse_isa(&v))
    })
}

/// Pure cap check (testable without touching the environment): is `isa`
/// permitted under `cap`? Scalar is always permitted.
fn cap_allows(isa: Isa, cap: Option<Isa>) -> bool {
    match cap {
        Some(c) => tier(isa) <= tier(c),
        None => true,
    }
}

/// Whether the production detection path may select `isa`, honouring the
/// `HORNDB_SIMD_MAX_ISA` cap. Each primitive's `resolve` guards its
/// feature-detection arms with this; the test/bench `forced_isa` override
/// deliberately bypasses it.
pub(crate) fn allows(isa: Isa) -> bool {
    cap_allows(isa, isa_cap())
}

/// The operational ISA cap configured via `HORNDB_SIMD_MAX_ISA`, or `None` if
/// the variable is unset or unrecognised. Read once from the environment.
///
/// Exposed so a host can log the effective SIMD policy at startup, e.g.
/// `tracing::info!(cap = ?horndb_simd::configured_max_isa(), "SIMD dispatch")`.
/// It is a width *tier* (scalar < avx2 ≈ neon < avx512): a cap of `Avx2` lets
/// AVX2/NEON kernels run but suppresses AVX-512; `Scalar` disables all SIMD.
pub fn configured_max_isa() -> Option<Isa> {
    isa_cap()
}

// --- Startup auto-calibration toggle (HORNDB_SIMD_AUTOTUNE) -----------------
//
// Per-host kernel benchmarks proved the fastest ISA is host-dependent (AVX-512
// `intersect` wins 2.5x on Sapphire Rapids but loses 2.5x on Zen4, etc.) with
// no cheap runtime bit to tell the cases apart. So each primitive can
// micro-calibrate at startup: time every available kernel and cache the
// fastest. The behaviour is on by default and disabled with
// `HORNDB_SIMD_AUTOTUNE=off` (also `0`/`false`/`no`), which falls back to the
// static widest-ISA preference. The `HORNDB_SIMD_MAX_ISA` cap still bounds the
// candidate set either way.

/// Pure parse of a `HORNDB_SIMD_AUTOTUNE` value (testable without touching the
/// environment). Auto-tune is *disabled* iff the trimmed, lowercased value is
/// one of `off`, `0`, `false`, `no`. Unset (`None`) or anything else ⇒ enabled.
fn autotune_from(s: Option<&str>) -> bool {
    match s {
        Some(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "off" | "0" | "false" | "no"
        ),
        None => true,
    }
}

/// Whether startup micro-calibration is enabled, read once from
/// `HORNDB_SIMD_AUTOTUNE`. Defaults to `true`.
pub(crate) fn autotune_enabled() -> bool {
    use std::sync::OnceLock;
    static AUTOTUNE: OnceLock<bool> = OnceLock::new();
    *AUTOTUNE.get_or_init(|| autotune_from(std::env::var("HORNDB_SIMD_AUTOTUNE").ok().as_deref()))
}

/// Whether startup micro-calibration is enabled (`HORNDB_SIMD_AUTOTUNE`, default
/// on). Exposed so a host can log the effective SIMD policy at startup alongside
/// [`configured_max_isa`]. When off, each primitive uses its static widest-ISA
/// preference; the `HORNDB_SIMD_MAX_ISA` cap applies in both modes.
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
    fn autotune_default_on_when_unset_or_unknown() {
        assert!(autotune_from(None), "unset ⇒ on");
        assert!(autotune_from(Some("on")));
        assert!(autotune_from(Some("1")));
        assert!(autotune_from(Some("true")));
        assert!(autotune_from(Some("garbage")));
        assert!(autotune_from(Some("")));
    }

    #[test]
    fn autotune_off_spellings_disable() {
        for v in ["off", "0", "false", "no", "OFF", " Off ", "FALSE", "No"] {
            assert!(!autotune_from(Some(v)), "{v:?} must disable autotune");
        }
    }

    #[test]
    fn parse_isa_accepts_known_spellings() {
        assert_eq!(parse_isa("scalar"), Some(Isa::Scalar));
        assert_eq!(parse_isa("AVX2"), Some(Isa::Avx2));
        assert_eq!(parse_isa(" avx512 "), Some(Isa::Avx512));
        assert_eq!(parse_isa("avx512f"), Some(Isa::Avx512));
        assert_eq!(parse_isa("neon"), Some(Isa::Neon));
        assert_eq!(parse_isa("garbage"), None);
        assert_eq!(parse_isa(""), None);
    }
}
