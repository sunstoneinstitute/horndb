//! SPEC-12 startup micro-calibration, auto-tune OFF path.
//!
//! Seeds `autotune = false` via `configure` before the first dispatch so the
//! memoised toggle observes it. This is its own test binary so the memoisation
//! can't be polluted by the default-on test in `calibration.rs`. With
//! calibration off, dispatch must still return a *correct* kernel (the static
//! widest-ISA preference).

use horndb_simd::{configure, configured_autotune, intersect, with_forced_isa, Isa};

#[test]
fn autotune_off_still_dispatches_correctly() {
    // First line of the only test in this binary: seed before any dispatch so the
    // one-shot read observes it. `None` cap = uncapped; `false` = calibration off.
    configure(None, false);
    assert!(!configured_autotune(), "off must disable autotune");

    let a: Vec<u64> = (0..1000u64).map(|x| x * 2).collect();
    let b: Vec<u64> = (0..1000u64).map(|x| x * 3).collect();
    let mut want = Vec::new();
    with_forced_isa(Isa::Scalar, || intersect(&a, &b, &mut want));
    let mut got = Vec::new();
    intersect(&a, &b, &mut got); // static-preference dispatch path
    assert_eq!(got, want);
}
