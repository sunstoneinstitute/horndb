//! SPEC-12 acceptance #1: every primitive is bit-identical to its scalar
//! oracle on the scalar path AND every ISA path the CI host can execute.
//! Mirrors the WCOJ binary-join fuzzer and the owlrl closure differential.

use horndb_simd::{
    dedup, filter_range, gather, intersect, lower_bound, merge, with_forced_isa, Isa,
};
use proptest::prelude::*;

/// Every ISA path the current host can actually execute (always scalar).
fn host_paths() -> Vec<Isa> {
    let mut v = vec![Isa::Scalar];
    #[cfg(target_arch = "x86_64")]
    {
        if std::is_x86_feature_detected!("avx2") {
            v.push(Isa::Avx2);
        }
        if std::is_x86_feature_detected!("avx512f") {
            v.push(Isa::Avx512);
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        if std::arch::is_aarch64_feature_detected!("neon") {
            v.push(Isa::Neon);
        }
    }
    v
}

fn sorted_deduped(v: &mut Vec<u64>) {
    v.sort_unstable();
    v.dedup();
}

proptest! {
    #[test]
    fn intersect_matches_oracle(mut a: Vec<u64>, mut b: Vec<u64>) {
        sorted_deduped(&mut a);
        sorted_deduped(&mut b);
        let mut want = Vec::new();
        // scalar oracle via forced scalar path
        with_forced_isa(Isa::Scalar, || intersect(&a, &b, &mut want));
        for isa in host_paths() {
            let mut got = Vec::new();
            with_forced_isa(isa, || intersect(&a, &b, &mut got));
            prop_assert_eq!(&got, &want, "intersect {:?}", isa);
        }
    }

    #[test]
    fn lower_bound_matches_oracle(mut h: Vec<u64>, value: u64) {
        h.sort_unstable();
        let want = h.partition_point(|&x| x < value);
        for isa in host_paths() {
            let got = with_forced_isa(isa, || lower_bound(&h, value));
            prop_assert_eq!(got, want, "lower_bound {:?}", isa);
        }
    }

    #[test]
    fn merge_matches_oracle(mut a: Vec<u64>, mut b: Vec<u64>) {
        a.sort_unstable();
        b.sort_unstable();
        let mut want = Vec::new();
        with_forced_isa(Isa::Scalar, || merge(&a, &b, &mut want));
        for isa in host_paths() {
            let mut got = Vec::new();
            with_forced_isa(isa, || merge(&a, &b, &mut got));
            prop_assert_eq!(&got, &want, "merge {:?}", isa);
        }
    }

    #[test]
    fn dedup_matches_oracle(mut v: Vec<u64>) {
        v.sort_unstable();
        let mut want = Vec::new();
        with_forced_isa(Isa::Scalar, || dedup(&v, &mut want));
        for isa in host_paths() {
            let mut got = Vec::new();
            with_forced_isa(isa, || dedup(&v, &mut got));
            prop_assert_eq!(&got, &want, "dedup {:?}", isa);
        }
    }

    #[test]
    fn filter_range_matches_oracle(v: Vec<u64>, lo: u64, hi: u64) {
        let mut want = Vec::new();
        with_forced_isa(Isa::Scalar, || filter_range(&v, lo, hi, &mut want));
        for isa in host_paths() {
            let mut got = Vec::new();
            with_forced_isa(isa, || filter_range(&v, lo, hi, &mut got));
            prop_assert_eq!(&got, &want, "filter_range {:?}", isa);
        }
    }

    #[test]
    fn gather_matches_oracle(base: Vec<u64>, raw: Vec<u32>) {
        prop_assume!(!base.is_empty());
        let indices: Vec<u32> = raw.iter().map(|&i| i % base.len() as u32).collect();
        let mut want = Vec::new();
        with_forced_isa(Isa::Scalar, || gather(&base, &indices, &mut want));
        for isa in host_paths() {
            let mut got = Vec::new();
            with_forced_isa(isa, || gather(&base, &indices, &mut got));
            prop_assert_eq!(&got, &want, "gather {:?}", isa);
        }
    }
}
