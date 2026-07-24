//! SPEC-12 / SPEC-26: the operational SIMD policy (ISA cap + auto-tune) is
//! seeded through `horndb_simd::configure` before the first dispatch — no
//! environment variable is read by this crate. The seed lands in a `OnceLock`
//! resolved once per process, so this test is its own binary: `cargo nextest`
//! runs each test in a fresh process, and under `cargo test` it is the only
//! test here, so the first-write guarantee holds either way. The pure cap/tier
//! logic is unit-tested in `dispatch.rs`; this covers the seeding seam end to
//! end. The default (unseeded) path is asserted in `calibration.rs`, which
//! never calls `configure`.

use horndb_simd::{configure, configured_autotune, configured_max_isa, Isa};

#[test]
fn configure_seeds_cap_and_autotune() {
    // First lines of the only test in this binary: seed before any dispatch so
    // the one-shot reads observe it.
    configure(Some(Isa::Avx2), false);
    assert_eq!(
        configured_max_isa(),
        Some(Isa::Avx2),
        "cap must reflect seed"
    );
    assert!(!configured_autotune(), "autotune must reflect seed");
}
