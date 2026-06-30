//! Skew-gated galloping path coverage for `intersect` (repairs the regression
//! bisected to `ccecd5f`). The production gate routes skewed size ratios to the
//! scalar galloping path and balanced ratios to the block-SIMD kernels; this
//! test exercises the *unforced* production path (so the gate engages) and
//! checks it against an independent two-pointer reference. The forced-ISA block
//! kernels keep their own differential coverage in `tests/differential.rs`.

use horndb_simd::intersect;

/// Independent scalar two-pointer reference — the oracle. Deliberately not the
/// kernel under test, so this can't false-green by comparing a function to
/// itself.
fn reference(a: &[u64], b: &[u64]) -> Vec<u64> {
    let mut out = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    out
}

/// A sorted, deduped run of `n` u64 with stride `stride` starting at `start`.
fn run(start: u64, stride: u64, n: usize) -> Vec<u64> {
    (0..n as u64).map(|x| start + x * stride).collect()
}

/// Build a (small, large) pair with partial overlap. Both sides are sorted and
/// deduped; the small side's stride (4) is double the large side's (2), so every
/// small element that falls within `large`'s range lands on a value present
/// there. For the skewed shapes the small side fits entirely inside `large` (so
/// every small element matches); the balanced shape's small side outruns
/// `large`'s max, giving a true partial overlap. The dedicated overrun case in
/// the test below covers the skewed-overrun regime separately.
fn overlapping(small_n: usize, large_n: usize) -> (Vec<u64>, Vec<u64>) {
    // Large side: 0, 2, 4, ... (dense evens).
    let large = run(0, 2, large_n);
    // Small side: 0, 4, 8, ... — every element is an even, hence present in the
    // large side whenever it falls within `large`'s range.
    let small = run(0, 4, small_n);
    (small, large)
}

#[test]
fn gallop_matches_oracle_across_skew_and_balanced() {
    // (a_len, b_len): heavy skew in both orientations, plus balanced.
    let shapes = [
        (8usize, 100_000usize),
        (64, 1_000_000),
        (256, 1_000_000),
        (4096, 4096),
    ];
    for (na, nb) in shapes {
        let (small, large) = overlapping(na.min(nb), na.max(nb));
        // Test both operand orderings: gallop must be correct regardless of
        // which side is larger, and the gate keys off max/min not arg position.
        for (a, b) in [(&small, &large), (&large, &small)] {
            let mut got = Vec::new();
            intersect(a, b, &mut got); // unforced: production gate engages
            let want = reference(a, b);
            assert_eq!(got, want, "intersect({}, {}) mismatch", a.len(), b.len());
        }
    }

    // Skewed shape whose small side deliberately overruns `large`'s max value, so
    // gallop's mid-loop `break` (lower_bound lands past the end of `large`) fires
    // under skew. The balanced/within-range shapes above never reach it: balanced
    // routes to the block kernel, and the in-range skewed shapes keep `small`
    // inside `large`. Small is 8 wide-strided evens, the last one past `large`'s
    // max (199_998); the 100_000/8 ratio keeps it firmly in the gallop regime.
    let large = run(0, 2, 100_000); // evens 0..=199_998
    let small = run(0, 30_000, 8); // 0, 30_000, ..., 210_000 — last element overruns
    for (a, b) in [(&small, &large), (&large, &small)] {
        let mut got = Vec::new();
        intersect(a, b, &mut got); // unforced: gate routes to gallop
        let want = reference(a, b);
        assert_eq!(
            got,
            want,
            "overrun intersect({}, {}) mismatch",
            a.len(),
            b.len()
        );
    }
}

#[test]
fn gallop_edge_cases() {
    // Empty small side against a large side: intersection is empty, and the
    // skew gate (hi >= 16 * max(1, lo)) must not divide-by-zero on lo == 0.
    let large = run(0, 1, 100_000);
    let mut got = Vec::new();
    intersect(&[], &large, &mut got);
    assert!(got.is_empty());
    got.clear();
    intersect(&large, &[], &mut got);
    assert!(got.is_empty());

    // Disjoint skewed inputs: small side entirely outside the large range.
    let small = run(10_000_000, 1, 8);
    got.clear();
    intersect(&small, &large, &mut got);
    assert!(got.is_empty());

    // Full overlap of the small side (subset of large).
    let small = run(0, 1, 8);
    got.clear();
    intersect(&small, &large, &mut got);
    assert_eq!(got, small);
}
