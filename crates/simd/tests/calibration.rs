//! SPEC-12 startup micro-calibration, default (auto-tune ON) path.
//!
//! `HORNDB_SIMD_AUTOTUNE` is unset here, so calibration runs. The off path lives
//! in its own test binary (`calibration_off.rs`) because the toggle is memoised
//! once per process and each integration-test file is a separate binary.

use horndb_simd::{calibration_report, configured_autotune, init, intersect, with_forced_isa, Isa};

/// Every reported ISA must be one the host can actually execute under
/// `with_forced_isa` (i.e. it was a real calibration candidate, not garbage).
fn host_runnable(isa: Isa) -> bool {
    match isa {
        Isa::Scalar => true,
        #[cfg(target_arch = "x86_64")]
        Isa::Avx2 => std::is_x86_feature_detected!("avx2"),
        #[cfg(target_arch = "x86_64")]
        Isa::Avx512 => std::is_x86_feature_detected!("avx512f"),
        #[cfg(target_arch = "aarch64")]
        Isa::Neon => std::arch::is_aarch64_feature_detected!("neon"),
        #[allow(unreachable_patterns)]
        _ => false,
    }
}

#[test]
fn init_then_report_has_seven_host_runnable_entries() {
    assert!(configured_autotune(), "autotune defaults on when unset");
    init();
    let report = calibration_report();
    assert_eq!(report.len(), 7, "one entry per dispatched primitive");
    for (name, isa) in report {
        assert!(host_runnable(isa), "{name}: chose non-runnable ISA {isa:?}");
    }
}

#[test]
fn calibrated_intersect_matches_scalar_oracle() {
    // Sanity: whatever kernel calibration picked must still be correct.
    let a: Vec<u64> = (0..1000u64).map(|x| x * 2).collect();
    let b: Vec<u64> = (0..1000u64).map(|x| x * 3).collect();
    let mut want = Vec::new();
    with_forced_isa(Isa::Scalar, || intersect(&a, &b, &mut want));
    let mut got = Vec::new();
    intersect(&a, &b, &mut got); // calibrated dispatch path
    assert_eq!(got, want);
}
