//! SPEC-03 acceptance criterion #3: 100K random BGPs of 2-6 patterns over a
//! LUBM-ish synthetic graph, comparing WCOJ output to BinaryHash output. The
//! check should find zero mismatches.
//!
//! Stage-1 substitute for LUBM: we use a small synthetic graph with a small
//! predicate vocabulary, which exercises the same code paths. LUBM-100
//! substitution lands in a follow-up plan once SPEC-01 conformance harness
//! can load the dataset. Stage-1 case count is 1024 (Stage-2 ramps to 100K
//! once nightly CI hosts the heavier run).

use arrow::array::UInt64Array;
use proptest::prelude::*;

use horndb_wcoj::cancel::CancelToken;
use horndb_wcoj::executor::binary_hash::BinaryHashExecutor;
use horndb_wcoj::executor::wcoj::WcojExecutor;
use horndb_wcoj::executor::Executor;
use horndb_wcoj::ids::{TermId, Triple};
use horndb_wcoj::pattern::{Bgp, Term, TriplePattern, Var};
use horndb_wcoj::plan::{ExecutionPlan, JoinSpec, PlanKind};
use horndb_wcoj::planner::Planner;
use horndb_wcoj::source::vec_source::VecTripleSource;
use horndb_wcoj::stats::SnapshotStats;

const N_VERTICES: u64 = 30;
const PREDICATES: &[u64] = &[100, 101, 102];

/// Wide vocabulary for the SIMD-coverage variant. The leapfrog's `k == 2`
/// intersect fast path only arms when a level's run is
/// `>= SIMD_INTERSECT_MIN_RUN` (64), so the default 30-vertex graph never
/// exercises it. `N_WIDE > 64`
/// makes a free-subject pattern's depth-0 run wide enough that
/// `VecIter::active_run` materialises an SoA column and the SIMD path engages
/// — the binary-hash oracle then cross-checks it across random BGP shapes.
const N_WIDE: u64 = 160;

fn build_source(seed: u64) -> VecTripleSource {
    build_source_n(seed, N_VERTICES)
}

fn build_source_n(seed: u64, n: u64) -> VecTripleSource {
    let mut state = seed | 1;
    let mut rand = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut triples = Vec::new();
    for s in 0..n {
        for &p in PREDICATES {
            // Each (s, p) yields 0-3 edges with random objects.
            for _ in 0..(rand() % 4) {
                let o = rand() % n;
                triples.push(Triple::new(s, p, o));
            }
        }
    }
    VecTripleSource::from_triples(triples)
}

fn collect_rows(
    batches: impl Iterator<Item = horndb_wcoj::error::Result<arrow::record_batch::RecordBatch>>,
) -> Vec<Vec<TermId>> {
    let mut out = Vec::new();
    for b in batches {
        let b = b.unwrap();
        let cols: Vec<&UInt64Array> = (0..b.num_columns())
            .map(|i| b.column(i).as_any().downcast_ref::<UInt64Array>().unwrap())
            .collect();
        for r in 0..b.num_rows() {
            out.push(cols.iter().map(|c| c.value(r)).collect::<Vec<TermId>>());
        }
    }
    // A sorted multiset: a duplicated row is a mismatch, not a no-op.
    out.sort_unstable();
    out
}

/// A hand-built hybrid plan: the first two non-ground patterns as one WCOJ
/// node, hash-joined with a left-deep chain of scans over the rest (ground
/// patterns included). Exercises every `JoinSpec` node kind of the tree
/// evaluator, and a cross product when the node and the chain share no var.
fn hybrid_spec(bgp: &Bgp) -> Option<JoinSpec> {
    let live: Vec<usize> = (0..bgp.patterns.len())
        .filter(|&i| !bgp.patterns[i].is_ground())
        .collect();
    if live.len() < 2 {
        return None;
    }
    let node = &live[..2];
    let sub = Bgp::new(node.iter().map(|&i| bgp.patterns[i]).collect());
    let wcoj = JoinSpec::Wcoj {
        patterns: node.to_vec(),
        var_order: sub.variables(),
    };
    let rest = (0..bgp.patterns.len()).filter(|i| !node.contains(i));
    Some(match JoinSpec::left_deep(rest) {
        None => wcoj,
        Some(chain) => JoinSpec::HashJoin {
            build: Box::new(wcoj),
            probe: Box::new(chain),
        },
    })
}

/// Like `collect_rows`, but columns are looked up by name (`v<n>`) and
/// emitted in `vars` order: a planned WCOJ emits its own variable order.
fn collect_rows_named(
    batches: impl Iterator<Item = horndb_wcoj::error::Result<arrow::record_batch::RecordBatch>>,
    vars: &[Var],
) -> Vec<Vec<TermId>> {
    let mut out = Vec::new();
    for b in batches {
        let b = b.unwrap();
        let cols: Vec<&UInt64Array> = vars
            .iter()
            .map(|v| {
                let (i, _) = b.schema().column_with_name(&format!("v{}", v.0)).unwrap();
                b.column(i).as_any().downcast_ref::<UInt64Array>().unwrap()
            })
            .collect();
        for r in 0..b.num_rows() {
            out.push(cols.iter().map(|c| c.value(r)).collect::<Vec<TermId>>());
        }
    }
    // A sorted multiset: a duplicated row is a mismatch, not a no-op.
    out.sort_unstable();
    out
}

/// The cost-based plan and the hand-built hybrid plan against the oracle.
fn check_planned(src: &VecTripleSource, bgp: &Bgp, oracle: &[Vec<TermId>]) {
    let vars = bgp.variables();
    let stats = SnapshotStats::from_source(src);
    let planned = collect_rows_named(
        Executor::for_bgp(src, bgp, &Planner::default(), &stats, CancelToken::new()),
        &vars,
    );
    assert_eq!(&planned, oracle, "cost-based plan disagrees with oracle");
    if let Some(spec) = hybrid_spec(bgp) {
        let hybrid = collect_rows_named(
            Executor::for_spec(src, bgp, &spec, CancelToken::new()),
            &vars,
        );
        assert_eq!(
            &hybrid, oracle,
            "hybrid spec {spec:?} disagrees with oracle"
        );
    }
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

/// Wide-vocabulary term: bound constants range over `0..N_WIDE` so a
/// free-subject pattern's depth-0 run can exceed the SIMD threshold. Biased
/// toward variables (2:1) so multiple patterns tend to share a free
/// leapfrog variable — the shape that arms the `k == 2` intersect.
fn arb_term_wide() -> impl Strategy<Value = Term> {
    prop_oneof![
        2 => (0u8..3u8).prop_map(|v| Term::Var(Var(v))),
        1 => (0u64..N_WIDE).prop_map(Term::Bound),
    ]
}

fn arb_pattern_wide() -> impl Strategy<Value = TriplePattern> {
    (arb_term_wide(), arb_predicate_term(), arb_term_wide())
        .prop_map(|(s, p, o)| TriplePattern::new(s, p, o))
        .prop_filter("no self-loop variables", |pat| {
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

fn arb_bgp_wide() -> impl Strategy<Value = Bgp> {
    prop::collection::vec(arb_pattern_wide(), 2..=4).prop_map(Bgp::new)
}

proptest! {
    // Default 256 cases; `PROPTEST_CASES=<n>` scales it (CI and soak runs).
    #![proptest_config(ProptestConfig::default())]

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
        prop_assert_eq!(&wcoj_rows, &bh_rows);
        check_planned(&src, &bgp, &bh_rows);
    }

    // SIMD-coverage variant: a wide graph (N_WIDE > SIMD_INTERSECT_MIN_RUN) so
    // the leapfrog's `k == 2` SIMD intersect fast path actually arms, with the
    // binary-hash executor as the differential oracle. Guards against the
    // active_run dedup hazard (a subject with many objects must still emit
    // each leapfrog key once) across random BGP shapes.
    #[test]
    fn wcoj_matches_binary_hash_wide(seed in any::<u64>(), bgp in arb_bgp_wide()) {
        let src = build_source_n(seed, N_WIDE);
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
        prop_assert_eq!(&wcoj_rows, &bh_rows);
        check_planned(&src, &bgp, &bh_rows);
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
