//! Drives the production `BatchIter` (inlined leapfrog) hard enough to engage
//! its `k == 2` SIMD intersect fast path — the variable runs at the shared
//! depth are >= `SIMD_SEEK_MIN_RUN` (64), so `VecIter::active_run` materialises
//! an SoA column and `BatchIter::try_arm_simd` fires. Each case checks the
//! executor output against a brute-force join oracle, so an over- or
//! under-production in the SIMD drain shows up as a row-set mismatch.

use std::collections::BTreeSet;

use arrow::array::UInt64Array;
use horndb_wcoj::cancel::CancelToken;
use horndb_wcoj::executor::wcoj::WcojExecutor;
use horndb_wcoj::ids::Triple;
use horndb_wcoj::pattern::{Bgp, Term, TriplePattern, Var};
use horndb_wcoj::plan::ExecutionPlan;
use horndb_wcoj::source::vec_source::VecTripleSource;

fn collect_rows(batches: Vec<arrow::record_batch::RecordBatch>) -> Vec<Vec<u64>> {
    let mut out: Vec<Vec<u64>> = Vec::new();
    for b in batches {
        let cols: Vec<&UInt64Array> = (0..b.num_columns())
            .map(|i| b.column(i).as_any().downcast_ref::<UInt64Array>().unwrap())
            .collect();
        for r in 0..b.num_rows() {
            out.push(cols.iter().map(|c| c.value(r)).collect());
        }
    }
    out
}

fn run(triples: Vec<Triple>, bgp: Bgp, var_order: Vec<Var>) -> Vec<Vec<u64>> {
    let src = VecTripleSource::from_triples(triples);
    let plan = ExecutionPlan {
        kind: horndb_wcoj::plan::PlanKind::Wcoj,
        var_order,
    };
    let exec = WcojExecutor::new(&src, &bgp, &plan, CancelToken::new());
    let batches: Vec<_> = exec.into_iter().collect::<Result<_, _>>().unwrap();
    let mut rows = collect_rows(batches);
    rows.sort();
    rows
}

/// Two patterns sharing only ?x at depth 0, with the *leapfrog variable* (?x)
/// as the leaf of each pattern (object bound), so the subject column is
/// naturally distinct. Wide runs (>= 64) force the SIMD intersect. This is the
/// "easy" case that mirrors `leapfrog_k2_simd_intersect_matches_btreeset_oracle`
/// but through the production executor.
#[test]
fn batchiter_simd_intersect_distinct_subjects() {
    let a: Vec<u64> = (0..120u64).collect();
    let b: Vec<u64> = (60..200u64).step_by(2).collect();
    let mut triples = Vec::new();
    for &s in &a {
        triples.push(Triple::new(s, 10, 1));
    }
    for &s in &b {
        triples.push(Triple::new(s, 20, 1));
    }
    // (?x, 10, 1) . (?x, 20, 1) — single shared variable ?x.
    let bgp = Bgp::new(vec![
        TriplePattern::new(Term::Var(Var(0)), Term::Bound(10), Term::Bound(1)),
        TriplePattern::new(Term::Var(Var(0)), Term::Bound(20), Term::Bound(1)),
    ]);
    let rows = run(triples, bgp, vec![Var(0)]);

    let set_a: BTreeSet<u64> = a.into_iter().collect();
    let set_b: BTreeSet<u64> = b.into_iter().collect();
    let expected: Vec<Vec<u64>> = set_a.intersection(&set_b).map(|&x| vec![x]).collect();
    assert_eq!(rows, expected);
    assert!(
        rows.len() >= 20,
        "expected a wide overlap, got {}",
        rows.len()
    );
}

/// The stress case: the shared leapfrog variable ?x sits *above* an unbound
/// variable ?y, so each subject carries several rows. `active_run` over the
/// subject column therefore spans repeated subject ids. The SIMD intersect
/// must still emit each ?x once (and the descent must enumerate ?y), i.e. the
/// fast path may not over-produce relative to the brute-force join.
#[test]
fn batchiter_simd_intersect_subjects_with_many_objects() {
    // A: subjects 0..100, each with objects {0,1,2} under predicate 10.
    // B: subjects 40..140, each with objects {1,2,3} under predicate 20.
    // Join (?x 10 ?y)(?x 20 ?y): ?x in 40..99, ?y in {1,2}.
    let mut triples = Vec::new();
    let a_subjects: Vec<u64> = (0..100u64).collect();
    let b_subjects: Vec<u64> = (40..140u64).collect();
    for &x in &a_subjects {
        for o in [0u64, 1, 2] {
            triples.push(Triple::new(x, 10, o));
        }
    }
    for &x in &b_subjects {
        for o in [1u64, 2, 3] {
            triples.push(Triple::new(x, 20, o));
        }
    }
    let bgp = Bgp::new(vec![
        TriplePattern::new(Term::Var(Var(0)), Term::Bound(10), Term::Var(Var(1))),
        TriplePattern::new(Term::Var(Var(0)), Term::Bound(20), Term::Var(Var(1))),
    ]);
    let rows = run(triples, bgp, vec![Var(0), Var(1)]);

    // Brute-force oracle.
    let mut expected: Vec<Vec<u64>> = Vec::new();
    for x in 40..100u64 {
        for y in [1u64, 2] {
            expected.push(vec![x, y]);
        }
    }
    expected.sort();
    assert_eq!(rows, expected);
}
