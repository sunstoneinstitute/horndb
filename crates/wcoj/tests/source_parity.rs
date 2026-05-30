//! Differential parity: `WcojExecutor` over `CompressedTripleSource` must
//! produce identical result sets to `WcojExecutor` over `VecTripleSource`
//! for arbitrary BGPs. This proves the compressed source is a
//! behaviour-preserving drop-in (GitHub #15).

use std::collections::BTreeSet;

use arrow::array::UInt64Array;
use proptest::prelude::*;

use horndb_wcoj::cancel::CancelToken;
use horndb_wcoj::executor::wcoj::WcojExecutor;
use horndb_wcoj::ids::{TermId, Triple};
use horndb_wcoj::pattern::{Bgp, Term, TriplePattern, Var};
use horndb_wcoj::plan::{ExecutionPlan, PlanKind};
use horndb_wcoj::source::compressed::CompressedTripleSource;
use horndb_wcoj::source::vec_source::VecTripleSource;

const N_VERTICES: u64 = 30;
const PREDICATES: &[u64] = &[100, 101, 102];

fn build_triples(seed: u64) -> Vec<Triple> {
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
            for _ in 0..(rand() % 4) {
                let o = rand() % N_VERTICES;
                triples.push(Triple::new(s, p, o));
            }
        }
    }
    triples
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
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    #[test]
    fn compressed_matches_dense(seed in any::<u64>(), bgp in arb_bgp()) {
        let triples = build_triples(seed);
        let dense = VecTripleSource::from_triples(triples.clone());
        let comp = CompressedTripleSource::from_triples(triples);

        let out_vars = bgp.variables();
        prop_assume!(!out_vars.is_empty());

        let plan = ExecutionPlan {
            kind: PlanKind::Wcoj,
            var_order: out_vars.clone(),
        };
        let dense_rows = collect_rows(
            WcojExecutor::new(&dense, &bgp, &plan, CancelToken::new()).into_iter(),
        );
        let comp_rows = collect_rows(
            WcojExecutor::new(&comp, &bgp, &plan, CancelToken::new()).into_iter(),
        );
        prop_assert_eq!(dense_rows, comp_rows);
    }
}
