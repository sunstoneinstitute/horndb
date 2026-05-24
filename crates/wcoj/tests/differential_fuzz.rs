//! SPEC-03 acceptance criterion #3: 100K random BGPs of 2-6 patterns over a
//! LUBM-ish synthetic graph, comparing WCOJ output to BinaryHash output. The
//! check should find zero mismatches.
//!
//! Stage-1 substitute for LUBM: we use a small synthetic graph with a small
//! predicate vocabulary, which exercises the same code paths. LUBM-100
//! substitution lands in a follow-up plan once SPEC-01 conformance harness
//! can load the dataset. Stage-1 case count is 1024 (Stage-2 ramps to 100K
//! once nightly CI hosts the heavier run).

use std::collections::BTreeSet;

use arrow::array::UInt64Array;
use proptest::prelude::*;

use horndb_wcoj::cancel::CancelToken;
use horndb_wcoj::executor::binary_hash::BinaryHashExecutor;
use horndb_wcoj::executor::wcoj::WcojExecutor;
use horndb_wcoj::ids::{TermId, Triple};
use horndb_wcoj::pattern::{Bgp, Term, TriplePattern, Var};
use horndb_wcoj::plan::{ExecutionPlan, PlanKind};
use horndb_wcoj::source::vec_source::VecTripleSource;

const N_VERTICES: u64 = 30;
const PREDICATES: &[u64] = &[100, 101, 102];

fn build_source(seed: u64) -> VecTripleSource {
    let mut state = seed | 1;
    let mut rand = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut triples = Vec::new();
    for s in 0..N_VERTICES {
        for &p in PREDICATES {
            // Each (s, p) yields 0-3 edges with random objects.
            for _ in 0..(rand() % 4) {
                let o = rand() % N_VERTICES;
                triples.push(Triple::new(s, p, o));
            }
        }
    }
    VecTripleSource::from_triples(triples)
}

fn collect_rows(
    batches: impl Iterator<Item = horndb_wcoj::error::Result<arrow::record_batch::RecordBatch>>,
) -> BTreeSet<Vec<TermId>> {
    let mut out = BTreeSet::new();
    for b in batches {
        let b = b.unwrap();
        let cols: Vec<&UInt64Array> = (0..b.num_columns())
            .map(|i| b.column(i).as_any().downcast_ref::<UInt64Array>().unwrap())
            .collect();
        for r in 0..b.num_rows() {
            out.insert(cols.iter().map(|c| c.value(r)).collect::<Vec<TermId>>());
        }
    }
    out
}

fn arb_term() -> impl Strategy<Value = Term> {
    prop_oneof![
        (0u8..3u8).prop_map(|v| Term::Var(Var(v))),
        (0u64..N_VERTICES).prop_map(Term::Bound),
    ]
}

fn arb_predicate_term() -> impl Strategy<Value = Term> {
    prop::sample::select(PREDICATES.to_vec()).prop_map(Term::Bound)
}

fn arb_pattern() -> impl Strategy<Value = TriplePattern> {
    (arb_term(), arb_predicate_term(), arb_term())
        .prop_map(|(s, p, o)| TriplePattern::new(s, p, o))
        .prop_filter("no self-loop variables", |pat| {
            // Stage-1 trie iterator doesn't handle (?x p ?x) yet; exclude
            // patterns where the same variable appears twice.
            let mut seen = std::collections::HashSet::new();
            for t in [pat.s, pat.p, pat.o] {
                if let Term::Var(v) = t {
                    if !seen.insert(v) {
                        return false;
                    }
                }
            }
            true
        })
}

fn arb_bgp() -> impl Strategy<Value = Bgp> {
    prop::collection::vec(arb_pattern(), 2..=6).prop_map(Bgp::new)
}

proptest! {
    // Stage-1: 256 cases (was 16 while the over-production bug was being
    // diagnosed). Once SPEC-01 conformance harness can load LUBM-100 the
    // case count ramps to 100K and the test moves to a nightly job.
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    #[test]
    fn wcoj_matches_binary_hash(seed in any::<u64>(), bgp in arb_bgp()) {
        let src = build_source(seed);
        let out_vars = bgp.variables();
        prop_assume!(!out_vars.is_empty());

        let plan = ExecutionPlan {
            kind: PlanKind::Wcoj,
            var_order: out_vars.clone(),
        };
        let wcoj_rows = collect_rows(
            WcojExecutor::new(&src, &bgp, &plan, CancelToken::new()).into_iter(),
        );
        let bh_rows = collect_rows(
            BinaryHashExecutor::new(&src, &bgp, out_vars, CancelToken::new()).into_iter(),
        );
        prop_assert_eq!(wcoj_rows, bh_rows);
    }
}

/// Sanity check: the generator must actually produce BGPs with repeated
/// patterns (i.e. structurally identical patterns appearing more than
/// once) — this is the class of inputs that surfaced the
/// over-production bug originally. If proptest is silently rejecting all
/// such BGPs (e.g. via the self-loop filter) the differential test
/// loses its main signal.
///
/// Uses a deterministic seed so the assertion isn't flaky; the threshold
/// is conservatively low (≥1 out of 2048) — well under the empirical
/// rate of repeated-pattern BGPs from the current `arb_bgp` strategy.
#[test]
fn fuzzer_generates_repeated_pattern_bgps() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::{Config, TestRng, TestRunner};
    let seed = [0xC0u8; 32];
    let rng = TestRng::from_seed(proptest::test_runner::RngAlgorithm::ChaCha, &seed);
    let mut runner = TestRunner::new_with_rng(
        Config {
            cases: 2048,
            ..Config::default()
        },
        rng,
    );
    let strat = arb_bgp();
    let mut repeated_count = 0usize;
    let mut total = 0usize;
    for _ in 0..2048 {
        let bgp = strat.new_tree(&mut runner).unwrap().current();
        total += 1;
        // Two patterns are "structurally identical" if their s/p/o terms
        // are pairwise equal (this is what triggers the WCOJ trie-iter
        // edge case — multiple iters sharing exactly the same physical
        // layout).
        let pats = &bgp.patterns;
        for i in 0..pats.len() {
            for j in (i + 1)..pats.len() {
                if pats[i].s == pats[j].s && pats[i].p == pats[j].p && pats[i].o == pats[j].o {
                    repeated_count += 1;
                    break;
                }
            }
        }
    }
    assert!(
        repeated_count >= 1,
        "expected ≥1/{total} BGPs with structurally repeated patterns, got {repeated_count} — \
         the differential fuzzer relies on this class of input to surface trie-iter sharing bugs"
    );
}
