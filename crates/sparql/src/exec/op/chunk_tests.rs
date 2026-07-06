//! Chunk-boundary invariance tests (#143).
//!
//! Each test runs the same plan at chunk sizes 1, 2, and 4096, asserts
//! sorted results are identical, and thereby exercises all cross-chunk
//! state (DistinctOp's seen-set, SliceOp's counters, ChunkedBatch tails
//! from every blocking op, etc.).

use crate::algebra::{AggFunc, Aggregate, Expr, OrderDir, Term, TriplePattern, Var};
use crate::exec::horn::HornBackend;
use crate::exec::runtime::Runtime;
use crate::exec::Store;
use crate::plan::PhysicalPlan;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Run `plan` at the given `chunk` size, collect and sort the string-rendered
/// Bindings rows.  The thread-local batch-size override is reset to 4096 on
/// return, so successive calls in the same test don't interfere.
fn run_sorted(horn: &HornBackend, plan: &PhysicalPlan, chunk: usize) -> Vec<String> {
    super::TEST_BATCH_ROWS.with(|c| c.set(chunk));
    let rt = Runtime::new(horn);
    let mut out: Vec<String> = rt.run(plan).unwrap().map(|b| format!("{b:?}")).collect();
    super::TEST_BATCH_ROWS.with(|c| c.set(4096));
    out.sort();
    out
}

/// Like `run_sorted` but preserves emission order (for `OrderBy` tests where
/// the operator already enforces deterministic order).
fn run_ordered(horn: &HornBackend, plan: &PhysicalPlan, chunk: usize) -> Vec<String> {
    super::TEST_BATCH_ROWS.with(|c| c.set(chunk));
    let rt = Runtime::new(horn);
    let out: Vec<String> = rt.run(plan).unwrap().map(|b| format!("{b:?}")).collect();
    super::TEST_BATCH_ROWS.with(|c| c.set(4096));
    out
}

/// Assert that plan results are identical at chunk sizes 1, 2, and 4096.
macro_rules! assert_chunk_invariant {
    ($horn:expr, $plan:expr, $label:expr) => {{
        let r1 = run_sorted($horn, $plan, 1);
        let r2 = run_sorted($horn, $plan, 2);
        let rbig = run_sorted($horn, $plan, 4096);
        assert_eq!(r1, rbig, "{} result changed at chunk size 1", $label);
        assert_eq!(r2, rbig, "{} result changed at chunk size 2", $label);
    }};
}

fn iri(s: &str) -> Term {
    Term::Iri(format!("http://ex/{s}"))
}

fn some_iri(s: &str) -> Option<Term> {
    Some(iri(s))
}

// ---------------------------------------------------------------------------
// Distinct: cross-chunk seen-set
// ---------------------------------------------------------------------------

/// 8 VALUES rows with 4 unique objects — deduplication must span chunks.
#[test]
fn distinct_cross_chunk() {
    let horn = HornBackend::new();

    // Values: ?x = a,b,a,c,b,a,d,c  (4 unique)
    let values = PhysicalPlan::Values {
        vars: vec![Var::new("x")],
        rows: vec![
            vec![some_iri("a")],
            vec![some_iri("b")],
            vec![some_iri("a")],
            vec![some_iri("c")],
            vec![some_iri("b")],
            vec![some_iri("a")],
            vec![some_iri("d")],
            vec![some_iri("c")],
        ],
    };
    let plan = PhysicalPlan::Distinct {
        inner: Box::new(values),
    };

    assert_chunk_invariant!(&horn, &plan, "Distinct");

    // Sanity: exactly 4 unique rows at canonical chunk size.
    let big = run_sorted(&horn, &plan, 4096);
    assert_eq!(big.len(), 4, "Distinct should yield 4 unique rows");
}

// ---------------------------------------------------------------------------
// Slice: OFFSET lands mid-stream, LIMIT smaller than row count
// ---------------------------------------------------------------------------

/// 8 VALUES rows; OFFSET 2 LIMIT 3 must produce exactly 3 rows regardless of
/// chunk size.
#[test]
fn slice_offset_limit_cross_chunk() {
    let horn = HornBackend::new();

    let values = PhysicalPlan::Values {
        vars: vec![Var::new("n")],
        rows: (0u8..8).map(|i| vec![some_iri(&format!("r{i}"))]).collect(),
    };
    let plan = PhysicalPlan::Slice {
        inner: Box::new(values),
        start: 2,
        length: Some(3),
    };

    assert_chunk_invariant!(&horn, &plan, "Slice");

    let big = run_sorted(&horn, &plan, 4096);
    assert_eq!(big.len(), 3, "Slice(OFFSET 2 LIMIT 3) should yield 3 rows");
}

// ---------------------------------------------------------------------------
// Join: inner hash join
// ---------------------------------------------------------------------------

/// Left: ?a = [a0..a5].  Right: ?a + ?b paired.  Join on ?a -> 6 matched rows.
#[test]
fn join_cross_chunk() {
    let horn = HornBackend::new();

    let left = PhysicalPlan::Values {
        vars: vec![Var::new("a")],
        rows: (0u8..6).map(|i| vec![some_iri(&format!("a{i}"))]).collect(),
    };
    let right = PhysicalPlan::Values {
        vars: vec![Var::new("a"), Var::new("b")],
        rows: (0u8..6)
            .map(|i| vec![some_iri(&format!("a{i}")), some_iri(&format!("b{i}"))])
            .collect(),
    };
    let plan = PhysicalPlan::Join {
        left: Box::new(left),
        right: Box::new(right),
    };

    assert_chunk_invariant!(&horn, &plan, "Join");

    let big = run_sorted(&horn, &plan, 4096);
    assert_eq!(big.len(), 6, "Join should yield 6 matched rows");
}

// ---------------------------------------------------------------------------
// LeftJoin: unmatched left rows preserved with unbound right vars
// ---------------------------------------------------------------------------

/// Left: ?x = [a..f].  Right: only a,c,e have ?y bindings.
/// Left join produces 6 rows; b,d,f have ?y unbound.
#[test]
fn left_join_cross_chunk() {
    let horn = HornBackend::new();

    let left = PhysicalPlan::Values {
        vars: vec![Var::new("x")],
        rows: ["a", "b", "c", "d", "e", "f"]
            .iter()
            .map(|s| vec![some_iri(s)])
            .collect(),
    };
    let right = PhysicalPlan::Values {
        vars: vec![Var::new("x"), Var::new("y")],
        rows: vec![
            vec![some_iri("a"), some_iri("y1")],
            vec![some_iri("c"), some_iri("y2")],
            vec![some_iri("e"), some_iri("y3")],
        ],
    };
    let plan = PhysicalPlan::LeftJoin {
        left: Box::new(left),
        right: Box::new(right),
        expr: None,
    };

    assert_chunk_invariant!(&horn, &plan, "LeftJoin");

    let big = run_sorted(&horn, &plan, 4096);
    assert_eq!(big.len(), 6, "LeftJoin should yield all 6 left rows");
}

// ---------------------------------------------------------------------------
// Union
// ---------------------------------------------------------------------------

/// Left: 3 rows, right: 4 rows, disjoint ?x values -> 7 rows.
#[test]
fn union_cross_chunk() {
    let horn = HornBackend::new();

    let left = PhysicalPlan::Values {
        vars: vec![Var::new("x")],
        rows: ["a", "b", "c"].iter().map(|s| vec![some_iri(s)]).collect(),
    };
    let right = PhysicalPlan::Values {
        vars: vec![Var::new("x")],
        rows: ["d", "e", "f", "g"]
            .iter()
            .map(|s| vec![some_iri(s)])
            .collect(),
    };
    let plan = PhysicalPlan::Union {
        left: Box::new(left),
        right: Box::new(right),
    };

    assert_chunk_invariant!(&horn, &plan, "Union");

    let big = run_sorted(&horn, &plan, 4096);
    assert_eq!(big.len(), 7, "Union should yield 7 rows");
}

// ---------------------------------------------------------------------------
// Group: GROUP BY with COUNT(*)
// ---------------------------------------------------------------------------

/// Values([?g, ?v]): a->2 rows, b->2 rows, c->1 row.
/// GROUP BY ?g COUNT(*) -> 3 output rows.
#[test]
fn group_count_cross_chunk() {
    let horn = HornBackend::new();

    // 8 input rows across 3 groups (a=3, b=3, c=2)
    let values = PhysicalPlan::Values {
        vars: vec![Var::new("g"), Var::new("v")],
        rows: vec![
            vec![some_iri("a"), some_iri("v1")],
            vec![some_iri("a"), some_iri("v2")],
            vec![some_iri("b"), some_iri("v3")],
            vec![some_iri("b"), some_iri("v4")],
            vec![some_iri("c"), some_iri("v5")],
            // extra rows to force >2 chunks at chunk size 2
            vec![some_iri("a"), some_iri("v6")],
            vec![some_iri("c"), some_iri("v7")],
            vec![some_iri("b"), some_iri("v8")],
        ],
    };
    let plan = PhysicalPlan::Group {
        inner: Box::new(values),
        keys: vec![Var::new("g")],
        aggregates: vec![Aggregate {
            out: Var::new("cnt"),
            func: AggFunc::CountStar,
            distinct: false,
        }],
    };

    assert_chunk_invariant!(&horn, &plan, "Group");

    let big = run_sorted(&horn, &plan, 4096);
    assert_eq!(big.len(), 3, "Group should yield 3 groups");
}

// ---------------------------------------------------------------------------
// OrderBy: deterministic sort order must be preserved across chunk sizes
// ---------------------------------------------------------------------------

/// 6 VALUES rows in scrambled order; ORDER BY ?x ASC must produce identical
/// ordered output regardless of chunk size.
#[test]
fn order_by_cross_chunk() {
    let horn = HornBackend::new();

    let values = PhysicalPlan::Values {
        vars: vec![Var::new("x")],
        rows: ["c", "a", "f", "b", "e", "d"]
            .iter()
            .map(|s| vec![some_iri(s)])
            .collect(),
    };
    let plan = PhysicalPlan::OrderBy {
        inner: Box::new(values),
        keys: vec![(Expr::Term(Term::Var(Var::new("x"))), OrderDir::Asc)],
    };

    // For OrderBy we compare in emission order (it's deterministic by design).
    let r1 = run_ordered(&horn, &plan, 1);
    let r2 = run_ordered(&horn, &plan, 2);
    let rbig = run_ordered(&horn, &plan, 4096);

    assert_eq!(r1, rbig, "OrderBy result changed at chunk size 1");
    assert_eq!(r2, rbig, "OrderBy result changed at chunk size 2");
    assert_eq!(rbig.len(), 6, "OrderBy should yield all 6 rows");
}

// ---------------------------------------------------------------------------
// Join: shared var unbound in every build-side row (#128 bound-key selection)
// ---------------------------------------------------------------------------

/// ?v is shared but UNDEF in every right (build) row while ?w is bound
/// everywhere: the join must key on ?w alone and still honor SPARQL
/// compatibility (an unbound ?v matches anything, so each left row pairs
/// with its ?w partner). 2 rows, invariant across chunk sizes. This test is
/// a semantics pin: it passes before AND after the bound-key change — the
/// change is a complexity fix, not a result change.
#[test]
fn join_unbound_build_var_cross_chunk() {
    let horn = HornBackend::new();

    let left = PhysicalPlan::Values {
        vars: vec![Var::new("v"), Var::new("w")],
        rows: vec![
            vec![some_iri("v1"), some_iri("w1")],
            vec![some_iri("v2"), some_iri("w2")],
        ],
    };
    let right = PhysicalPlan::Values {
        vars: vec![Var::new("v"), Var::new("w"), Var::new("b")],
        rows: vec![
            vec![None, some_iri("w1"), some_iri("b1")],
            vec![None, some_iri("w2"), some_iri("b2")],
        ],
    };
    let plan = PhysicalPlan::Join {
        left: Box::new(left),
        right: Box::new(right),
    };

    assert_chunk_invariant!(&horn, &plan, "Join unbound build var");

    let big = run_sorted(&horn, &plan, 4096);
    assert_eq!(big.len(), 2, "each left row joins exactly its ?w partner");
}

// ---------------------------------------------------------------------------
// Join: probe-side streaming (#128)
// ---------------------------------------------------------------------------

/// Each probe row matches 4 build rows: at chunk size 1/2 the merged output
/// of ONE probe chunk exceeds the chunk size, exercising the pending-buffer
/// carry inside the streaming JoinOp.
#[test]
fn join_fanout_exceeds_chunk_size() {
    let horn = HornBackend::new();

    let left = PhysicalPlan::Values {
        vars: vec![Var::new("a")],
        rows: (0u8..3).map(|i| vec![some_iri(&format!("a{i}"))]).collect(),
    };
    let mut right_rows: Vec<Vec<Option<Term>>> = Vec::new();
    for i in 0u8..3 {
        for j in 0u8..4 {
            right_rows.push(vec![
                some_iri(&format!("a{i}")),
                some_iri(&format!("b{i}{j}")),
            ]);
        }
    }
    let right = PhysicalPlan::Values {
        vars: vec![Var::new("a"), Var::new("b")],
        rows: right_rows,
    };
    let plan = PhysicalPlan::Join {
        left: Box::new(left),
        right: Box::new(right),
    };

    assert_chunk_invariant!(&horn, &plan, "Join fan-out");

    let big = run_sorted(&horn, &plan, 4096);
    assert_eq!(big.len(), 12, "3 probe rows x 4 matches");
}

/// Mixed-provenance regression for the streamed Join (design doc §3): the
/// probe side (VALUES, Term provenance) has an UNDEF ?v row FIRST; the build
/// side (BGP scan) binds ?v as Slot::Id. At chunk size 1 the UNDEF probe row
/// merges the build side's Id(v1) into the output stream before any probe
/// Term(v1) appears — per-chunk normalize_columns would leave chunk 1 as Id
/// and chunk 2 as Term, and the cross-chunk DISTINCT seen-set would count
/// one logical ?v twice. The forced-term column set keeps the whole stream
/// Term-homogeneous. Goes RED if the force_term_columns call is dropped
/// from probe_join_chunk.
#[test]
fn distinct_over_streamed_join_mixed_provenance() {
    let mut horn = HornBackend::new();
    horn.insert_triple(iri("v1"), iri("p"), iri("o1"));

    let left = PhysicalPlan::Values {
        vars: vec![Var::new("v")],
        rows: vec![vec![None], vec![some_iri("v1")]],
    };
    let right = PhysicalPlan::BgpScan {
        patterns: vec![TriplePattern {
            subject: Term::Var(Var::new("v")),
            predicate: iri("p"),
            object: Term::Var(Var::new("o")),
        }],
    };
    let plan = PhysicalPlan::Distinct {
        inner: Box::new(PhysicalPlan::Project {
            vars: vec![Var::new("v")],
            inner: Box::new(PhysicalPlan::Join {
                left: Box::new(left),
                right: Box::new(right),
            }),
        }),
    };

    assert_chunk_invariant!(&horn, &plan, "Join mixed provenance");

    let big = run_sorted(&horn, &plan, 4096);
    assert_eq!(big.len(), 1, "both probe rows bind the same logical ?v=v1");
}

// ---------------------------------------------------------------------------
// LeftJoin: probe-side streaming (#128)
// ---------------------------------------------------------------------------

/// Mixed-provenance regression for the streamed LeftJoin, mirroring
/// distinct_over_streamed_join_mixed_provenance: UNDEF-first VALUES probe
/// (Term provenance) against a BGP build (Id provenance), DISTINCT ?v must
/// see ONE solution at every chunk size. Goes RED if the force_term_columns
/// call is dropped from probe_left_join_chunk.
#[test]
fn distinct_over_streamed_left_join_mixed_provenance() {
    let mut horn = HornBackend::new();
    horn.insert_triple(iri("v1"), iri("p"), iri("o1"));

    let left = PhysicalPlan::Values {
        vars: vec![Var::new("v")],
        rows: vec![vec![None], vec![some_iri("v1")]],
    };
    let right = PhysicalPlan::BgpScan {
        patterns: vec![TriplePattern {
            subject: Term::Var(Var::new("v")),
            predicate: iri("p"),
            object: Term::Var(Var::new("o")),
        }],
    };
    let plan = PhysicalPlan::Distinct {
        inner: Box::new(PhysicalPlan::Project {
            vars: vec![Var::new("v")],
            inner: Box::new(PhysicalPlan::LeftJoin {
                left: Box::new(left),
                right: Box::new(right),
                expr: None,
            }),
        }),
    };

    assert_chunk_invariant!(&horn, &plan, "LeftJoin mixed provenance");

    let big = run_sorted(&horn, &plan, 4096);
    assert_eq!(big.len(), 1, "both probe rows bind the same logical ?v=v1");
}

/// OPTIONAL over an empty build side: every probe row must stream through
/// with the build-only var unbound (the empty-build early-exit is an inner
/// Join fast path ONLY — LeftJoin must not take it).
#[test]
fn left_join_empty_optional_cross_chunk() {
    let horn = HornBackend::new();

    let left = PhysicalPlan::Values {
        vars: vec![Var::new("x")],
        rows: ["a", "b", "c"].iter().map(|s| vec![some_iri(s)]).collect(),
    };
    let right = PhysicalPlan::Values {
        vars: vec![Var::new("x"), Var::new("y")],
        rows: vec![],
    };
    let plan = PhysicalPlan::LeftJoin {
        left: Box::new(left),
        right: Box::new(right),
        expr: None,
    };

    assert_chunk_invariant!(&horn, &plan, "LeftJoin empty OPTIONAL");

    let big = run_sorted(&horn, &plan, 4096);
    assert_eq!(big.len(), 3, "all probe rows survive with ?y unbound");
}

/// Pins the build-actual-Term trigger of `forced_term` (the sibling test
/// `distinct_over_streamed_join_mixed_provenance` pins the probe-claim
/// trigger). The probe side is a LeftJoin of Values [?x] over a BGP scan
/// binding ?v, so its `may_emit_term` claim for ?v is FALSE (the Values
/// child lacks ?v; the scan side is all-Id) — only the drained build data
/// (VALUES actually holding Slot::Term) forces the decode. Probe row ?x=a
/// matches the scan and carries ?v=Id(v1); probe row ?x=b leaves ?v Unbound,
/// keys as None, probes ALL build rows and takes the build's Term(v1) in the
/// merge. Without the forced decode the output stream mixes Id(v1) and
/// Term(v1) in ?v and cross-chunk DISTINCT counts one logical value twice.
/// Goes RED if the build-side `.any(Slot::Term)` arm is dropped from
/// `build_join_state`.
#[test]
fn distinct_over_streamed_join_build_term_trigger() {
    let mut horn = HornBackend::new();
    horn.insert_triple(iri("a"), iri("q"), iri("v1"));

    // Probe: (?x=a, ?v=Id(v1)) and (?x=b, ?v=Unbound).
    let probe = PhysicalPlan::LeftJoin {
        left: Box::new(PhysicalPlan::Values {
            vars: vec![Var::new("x")],
            rows: vec![vec![some_iri("a")], vec![some_iri("b")]],
        }),
        right: Box::new(PhysicalPlan::BgpScan {
            patterns: vec![TriplePattern {
                subject: Term::Var(Var::new("x")),
                predicate: iri("q"),
                object: Term::Var(Var::new("v")),
            }],
        }),
        expr: None,
    };
    // Build: actual Slot::Term rows for ?v.
    let build = PhysicalPlan::Values {
        vars: vec![Var::new("v"), Var::new("z")],
        rows: vec![vec![some_iri("v1"), some_iri("z1")]],
    };
    let plan = PhysicalPlan::Distinct {
        inner: Box::new(PhysicalPlan::Project {
            vars: vec![Var::new("v")],
            inner: Box::new(PhysicalPlan::Join {
                left: Box::new(probe),
                right: Box::new(build),
            }),
        }),
    };

    assert_chunk_invariant!(&horn, &plan, "Join build-term trigger");

    let big = run_sorted(&horn, &plan, 4096);
    assert_eq!(
        big.len(),
        1,
        "both merged rows carry the same logical ?v=v1"
    );
}

// ---------------------------------------------------------------------------
// run_stream: chunked boundary decode (#128 HTTP streaming increment)
// ---------------------------------------------------------------------------

/// 7 VALUES rows at chunk size 3 → chunks of 3/3/1; concatenation must equal
/// `run`'s output, chunks must respect batch_rows() and never be empty.
#[test]
fn run_stream_chunks_match_run() {
    let horn = HornBackend::new();
    let plan = PhysicalPlan::Values {
        vars: vec![Var::new("x")],
        rows: (0u8..7).map(|i| vec![some_iri(&format!("v{i}"))]).collect(),
    };

    super::TEST_BATCH_ROWS.with(|c| c.set(3));
    let rt = Runtime::new(&horn);
    let mut stream = rt.run_stream(&plan).unwrap();
    let mut chunks = Vec::new();
    while let Some(chunk) = stream.next_chunk().unwrap() {
        assert!(!chunk.is_empty(), "no empty chunks mid-stream");
        assert!(chunk.len() <= 3, "chunk exceeds batch_rows()");
        chunks.push(chunk);
    }
    assert!(
        chunks.len() >= 3,
        "7 rows at chunk size 3 must span >=3 chunks"
    );
    let streamed: Vec<String> = chunks.concat().iter().map(|b| format!("{b:?}")).collect();
    let collected: Vec<String> = rt.run(&plan).unwrap().map(|b| format!("{b:?}")).collect();
    super::TEST_BATCH_ROWS.with(|c| c.set(4096));
    assert_eq!(streamed, collected);
}

/// The row-at-a-time Iterator view must yield the same rows as `run`.
#[test]
fn run_stream_iterator_matches_run() {
    let horn = HornBackend::new();
    let plan = PhysicalPlan::Values {
        vars: vec![Var::new("x")],
        rows: (0u8..7).map(|i| vec![some_iri(&format!("v{i}"))]).collect(),
    };

    super::TEST_BATCH_ROWS.with(|c| c.set(2));
    let rt = Runtime::new(&horn);
    let stream = rt.run_stream(&plan).unwrap();
    let via_iter: Vec<String> = stream.map(|r| format!("{:?}", r.unwrap())).collect();
    let via_run: Vec<String> = rt.run(&plan).unwrap().map(|b| format!("{b:?}")).collect();
    super::TEST_BATCH_ROWS.with(|c| c.set(4096));
    assert_eq!(via_iter, via_run);
}

/// Mixing the two access styles must lose or reorder nothing: pull 2 rows via
/// `Iterator::next` (leaving 1 row of the first 3-row chunk buffered), then
/// drain the rest via `next_chunk`. The first drained chunk must be exactly
/// the buffered row, and [2 iterator rows] ++ [drained rows] == `run`'s output.
#[test]
fn run_stream_mixed_iterator_then_chunks_matches_run() {
    let horn = HornBackend::new();
    let plan = PhysicalPlan::Values {
        vars: vec![Var::new("x")],
        rows: (0u8..7).map(|i| vec![some_iri(&format!("v{i}"))]).collect(),
    };

    super::TEST_BATCH_ROWS.with(|c| c.set(3));
    let rt = Runtime::new(&horn);
    let mut stream = rt.run_stream(&plan).unwrap();

    // 2 rows via the Iterator view: pulls the first 3-row chunk, buffers 1.
    let mut rows: Vec<String> = Vec::new();
    for _ in 0..2 {
        rows.push(format!("{:?}", stream.next().unwrap().unwrap()));
    }

    // Drain the rest via next_chunk; the first drained chunk is the 1
    // buffered row left over from the first 3-row chunk.
    let mut first_drained_len = None;
    while let Some(chunk) = stream.next_chunk().unwrap() {
        assert!(!chunk.is_empty(), "no empty chunks mid-stream");
        first_drained_len.get_or_insert(chunk.len());
        rows.extend(chunk.iter().map(|b| format!("{b:?}")));
    }
    assert_eq!(
        first_drained_len,
        Some(1),
        "first next_chunk after 2 Iterator rows must return the 1 buffered row"
    );

    let via_run: Vec<String> = rt.run(&plan).unwrap().map(|b| format!("{b:?}")).collect();
    super::TEST_BATCH_ROWS.with(|c| c.set(4096));
    assert_eq!(rows, via_run, "mixed access lost or reordered rows");
}
