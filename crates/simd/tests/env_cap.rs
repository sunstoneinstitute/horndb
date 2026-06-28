//! SPEC-12: the `HORNDB_SIMD_MAX_ISA` operational cap is read from the
//! environment. The cap value is memoised on first use, so this test sets the
//! variable *before* any dispatch happens. It works under `cargo nextest`,
//! which runs each test in its own process (a fresh `OnceLock`); under
//! `cargo test` it is the only test in this binary, so the same guarantee
//! holds. The *clamping* logic itself is unit-tested in `dispatch.rs`; this
//! integration test covers the environment-read seam end to end.

use horndb_simd::{configured_max_isa, Isa};

#[test]
fn max_isa_env_is_parsed_and_capped() {
    // Safety/ordering: set before the first `configured_max_isa()` call so the
    // one-shot read observes it. This is the first line of the only test here.
    std::env::set_var("HORNDB_SIMD_MAX_ISA", "avx2");
    assert_eq!(configured_max_isa(), Some(Isa::Avx2));
}
