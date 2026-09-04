//! Randomized differential test for `Tier::apply_quad_batch` (HDB-102).
//!
//! `apply_quad_batch` now picks its write strategy per predicate: a predicate
//! the batch only *adds* to gets an appended run and never carries its rows
//! forward, while a predicate the batch *deletes* from is still rebuilt row by
//! row. The two paths have to be indistinguishable from outside, so this test
//! drives long randomized sequences of mixed batches against a `BTreeSet`
//! reference model and checks, after every batch:
//!
//! * `ApplyReport::retracted` / `inserted` — SPARQL Update idempotency counting
//!   and `Store::insert_quads`'s return value both ride on these being exact.
//! * the full live quad set, read back through the tier's own read path.
//! * the tier's commit version, which must move only when something changed.
//!
//! The generator deliberately produces the cases the two paths differ on:
//! add-only batches, delete-only batches, mixed batches whose deletes and adds
//! land on the same predicate, mixed batches where they land on *different*
//! predicates (so one predicate takes each path in the same call), repeated
//! inserts of live quads, deletes of absent quads, and delete+re-add of the
//! same quad inside one batch.

use horndb_storage::{
    memory_tier::MemoryTier, GraphId, Ordering, TermId, TermKind, Tier, TierWrite, DEFAULT_GRAPH,
};
use std::collections::BTreeSet;

/// A deterministic xorshift64* generator. A fixed seed keeps a failure
/// reproducible; no dev-dependency needed for this shape of test.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn id(payload: u64) -> TermId {
    TermId::new(TermKind::Uri, payload)
}

/// A quad in the reference model's own terms: raw `(graph, s, p, o)` payloads.
type Quad = (u64, u64, u64, u64);

fn graph_id(g: u64) -> GraphId {
    if g == 0 {
        DEFAULT_GRAPH
    } else {
        GraphId(id(g).0)
    }
}

fn to_tier(q: Quad) -> (GraphId, TermId, TermId, TermId) {
    (graph_id(q.0), id(q.1), id(q.2), id(q.3))
}

/// Every live quad the tier currently holds, read through its normal read
/// path (visibility-filtered at the current version).
fn live_quads(tier: &MemoryTier) -> BTreeSet<Quad> {
    let snap = tier.snapshot();
    let mut out = BTreeSet::new();
    for g in snap.graphs() {
        for p in snap.predicates(g) {
            let cols = snap
                .ordered_predicate_at(g, p, Ordering::Spo)
                .expect("a listed predicate has a partition");
            for (s, o) in cols.subject_object() {
                let graph_payload = if g == DEFAULT_GRAPH {
                    0
                } else {
                    TermId(g.0).payload()
                };
                assert!(
                    out.insert((graph_payload, s.payload(), p.payload(), o.payload())),
                    "the tier returned the same live quad twice"
                );
            }
        }
    }
    out
}

/// One randomized run. `graphs`/`preds`/`terms` bound the key space — small
/// values make collisions (and so the interesting duplicate/resurrect cases)
/// common.
///
/// `read_every` is what makes this test able to see the append-run path at
/// all. Reading the live set (or `triple_count`) goes through
/// `PredicatePartition::cols`, which **merges the partition's runs down to
/// one**. Checking after every batch therefore hands every later batch a
/// single-run partition, and `mark_live` is never asked a question that needs
/// more than one run — a probe that ignored every run but the last would still
/// pass. Reading only every `read_every`-th round lets that many unmerged runs
/// pile up first. The count and version assertions stay on every round: they
/// read no columns, so they force no merge.
fn run_differential(
    seed: u64,
    rounds: usize,
    graphs: u64,
    preds: u64,
    terms: u64,
    read_every: usize,
) {
    let mut rng = Rng(seed);
    let tier = MemoryTier::new();
    let mut model: BTreeSet<Quad> = BTreeSet::new();
    let mut version = 0u64;

    for round in 0..rounds {
        // Batch shape: 0 = add-only, 1 = delete-only, 2..=4 = mixed. Add-only
        // is weighted low here on purpose; the dedicated add-only coverage is
        // in the seeded runs below, and mixed batches are what stress the
        // per-predicate path split.
        let shape = rng.below(5);
        let n_adds = if shape == 1 { 0 } else { 1 + rng.below(12) };
        let n_dels = if shape == 0 { 0 } else { 1 + rng.below(12) };

        let random_quad = |rng: &mut Rng| -> Quad {
            (
                rng.below(graphs),
                1 + rng.below(terms),
                1 + rng.below(preds),
                1 + rng.below(terms),
            )
        };

        // Deletes: half drawn from what is actually live (so they bite), half
        // random (so absent-delete no-ops are covered).
        let live_now: Vec<Quad> = model.iter().copied().collect();
        let mut dels: Vec<Quad> = Vec::new();
        for _ in 0..n_dels {
            if !live_now.is_empty() && rng.below(2) == 0 {
                dels.push(live_now[rng.below(live_now.len() as u64) as usize]);
            } else {
                dels.push(random_quad(&mut rng));
            }
        }
        // Adds: a third re-add something the batch just deleted (the
        // delete-then-re-add-in-one-batch case), a third re-add something
        // already live (the idempotent-insert case), the rest are random.
        let mut adds: Vec<Quad> = Vec::new();
        for _ in 0..n_adds {
            match rng.below(3) {
                0 if !dels.is_empty() => adds.push(dels[rng.below(dels.len() as u64) as usize]),
                1 if !live_now.is_empty() => {
                    adds.push(live_now[rng.below(live_now.len() as u64) as usize])
                }
                _ => adds.push(random_quad(&mut rng)),
            }
        }
        // Duplicate a few entries within the batch: the tier deduplicates each
        // side, so this must not change any count.
        if !adds.is_empty() && rng.below(2) == 0 {
            adds.push(adds[rng.below(adds.len() as u64) as usize]);
        }
        if !dels.is_empty() && rng.below(2) == 0 {
            dels.push(dels[rng.below(dels.len() as u64) as usize]);
        }

        // Reference semantics (SPEC-28 S6): deletes take effect before adds;
        // both sides are sets; an absent delete and an already-live insert are
        // counted no-ops.
        let del_set: BTreeSet<Quad> = dels.iter().copied().collect();
        let add_set: BTreeSet<Quad> = adds.iter().copied().collect();
        let want_retracted = del_set.iter().filter(|q| model.contains(q)).count();
        let after_dels: BTreeSet<Quad> = model.difference(&del_set).copied().collect();
        let want_inserted = add_set.iter().filter(|q| !after_dels.contains(q)).count();
        let want_model: BTreeSet<Quad> = after_dels.union(&add_set).copied().collect();

        let del_rows: Vec<_> = dels.iter().copied().map(to_tier).collect();
        let add_rows: Vec<_> = adds.iter().copied().map(to_tier).collect();
        let report = tier.apply_quad_batch(&del_rows, &add_rows).unwrap();

        assert_eq!(
            (report.retracted, report.inserted),
            (want_retracted, want_inserted),
            "seed {seed} round {round}: counts diverged (shape {shape})"
        );

        if want_retracted > 0 || want_inserted > 0 {
            version += 1;
        }
        assert_eq!(
            tier.snapshot().version(),
            version,
            "seed {seed} round {round}: a no-op batch must not bump the version"
        );

        model = want_model;

        // Both checks below read columns, so both collapse the runs. Skip them
        // on most rounds so the runs accumulate; always check the last round.
        if round % read_every != 0 && round + 1 != rounds {
            continue;
        }
        assert_eq!(
            live_quads(&tier),
            model,
            "seed {seed} round {round}: live set diverged (shape {shape})"
        );
        assert_eq!(
            tier.triple_count() as usize,
            model.len(),
            "seed {seed} round {round}: triple_count diverged"
        );
    }
}

#[test]
fn mixed_batches_match_a_reference_model() {
    // Dense key space: heavy collision, so resurrect / idempotent-insert /
    // absent-delete all fire constantly. `read_every = 1` keeps every
    // partition merged, which is the single-run baseline.
    for seed in [1u64, 2, 3, 5, 8, 13] {
        run_differential(seed, 120, 3, 3, 12, 1);
    }
}

#[test]
fn sparse_batches_match_a_reference_model() {
    // Sparse key space: most adds are genuinely new, most deletes miss, and
    // partitions accumulate runs the way an append workload does.
    for seed in [21u64, 34, 55] {
        run_differential(seed, 120, 2, 6, 4000, 1);
    }
}

/// The same model check with the reads pulled out, so partitions carry many
/// unmerged runs while the batches are applied. This is what covers
/// `PredicatePartition::mark_live` across runs at the tier level: with
/// `read_every = 1` above, every probe sees a freshly merged single-run
/// partition and a probe that consulted only one run would still pass.
#[test]
fn batches_against_multi_run_partitions_match_a_reference_model() {
    for seed in [1u64, 2, 3, 5, 8, 13] {
        run_differential(seed, 120, 3, 3, 12, 25);
    }
    for seed in [21u64, 34, 55] {
        run_differential(seed, 120, 2, 6, 4000, 25);
    }
    // No read at all until the final round: the deepest run stack this test
    // builds. Every add-only batch in it appends another unmerged run.
    run_differential(89, 60, 1, 2, 300, usize::MAX);
}

/// The path this task exists for: repeated add-only batches into one growing
/// predicate. The reference model check is the same; what this adds is the
/// shape — no batch here ever takes the rebuild path.
#[test]
fn repeated_add_only_batches_match_a_reference_model() {
    let tier = MemoryTier::new();
    let mut model: BTreeSet<Quad> = BTreeSet::new();
    let mut rng = Rng(89);
    let mut inserted_total = 0usize;

    for round in 0..40u64 {
        let mut adds: Vec<Quad> = Vec::new();
        for _ in 0..64 {
            // A quarter of every batch re-inserts something already live.
            let s = if rng.below(4) == 0 && round > 0 {
                1 + rng.below(round * 64)
            } else {
                1 + rng.below(4096)
            };
            adds.push((0, s, 1, 1 + rng.below(8)));
        }
        let add_set: BTreeSet<Quad> = adds.iter().copied().collect();
        let want_inserted = add_set.iter().filter(|q| !model.contains(q)).count();

        let rows: Vec<_> = adds.iter().copied().map(to_tier).collect();
        let report = tier.apply_quad_batch(&[], &rows).unwrap();
        assert_eq!(report.retracted, 0);
        assert_eq!(
            report.inserted, want_inserted,
            "round {round}: add-only insert count diverged"
        );
        inserted_total += report.inserted;

        model.extend(add_set);
        // Read every eighth round only, so the partition carries several
        // unmerged runs when the next batch probes it (see `run_differential`).
        if round % 8 == 0 || round == 39 {
            assert_eq!(live_quads(&tier), model, "round {round}: live set diverged");
        }
    }
    assert_eq!(model.len(), inserted_total, "every insert was counted once");
}

/// An add-only batch whose quads are all already live changes nothing: no
/// count, and no new commit version (which would invalidate every reader
/// snapshot for nothing).
#[test]
fn a_fully_redundant_add_only_batch_is_a_true_no_op() {
    let tier = MemoryTier::new();
    let quads: Vec<_> = (1..=50u64)
        .map(|i| (DEFAULT_GRAPH, id(i), id(100), id(i * 2)))
        .collect();
    let first = tier.apply_quad_batch(&[], &quads).unwrap();
    assert_eq!(first.inserted, 50);
    let version = tier.snapshot().version();

    let second = tier.apply_quad_batch(&[], &quads).unwrap();
    assert_eq!(
        (second.retracted, second.inserted),
        (0, 0),
        "re-inserting live quads must be a counted no-op"
    );
    assert_eq!(
        tier.snapshot().version(),
        version,
        "a no-op batch must not bump the commit version"
    );
    assert_eq!(tier.triple_count(), 50);
}

/// A quad deleted and re-added in the same batch counts as both a retract and
/// a fresh insert, and ends up present — including when the delete lands on a
/// predicate the batch also adds to (the rebuild path) and when it does not
/// (the append path).
#[test]
fn delete_then_re_add_in_one_batch_counts_both_ways() {
    let tier = MemoryTier::new();
    let q = (DEFAULT_GRAPH, id(1), id(100), id(2));
    let other = (DEFAULT_GRAPH, id(3), id(101), id(4));
    tier.apply_quad_batch(&[], &[q, other]).unwrap();

    // Same predicate on both sides -> rebuild path for pred 100, append path
    // for pred 101.
    let extra = (DEFAULT_GRAPH, id(5), id(101), id(6));
    let report = tier.apply_quad_batch(&[q], &[q, extra]).unwrap();
    assert_eq!((report.retracted, report.inserted), (1, 2));
    assert_eq!(tier.triple_count(), 3);

    let live = live_quads(&tier);
    assert!(live.contains(&(0, 1, 100, 2)), "the re-added quad is live");
    assert!(live.contains(&(0, 5, 101, 6)), "the appended quad is live");
}

/// A retracted quad re-added by a *later* add-only batch comes back. The
/// append path must not mistake the dead row left behind by the retraction for
/// a live one.
#[test]
fn re_adding_a_retracted_quad_through_the_append_path_resurrects_it() {
    let tier = MemoryTier::new();
    let q = (DEFAULT_GRAPH, id(1), id(100), id(2));
    tier.apply_quad_batch(&[], &[q]).unwrap();
    assert_eq!(tier.apply_quad_batch(&[q], &[]).unwrap().retracted, 1);
    assert_eq!(tier.triple_count(), 0);

    // Add-only: takes the append path against a partition holding one dead row.
    let report = tier.apply_quad_batch(&[], &[q]).unwrap();
    assert_eq!((report.retracted, report.inserted), (0, 1));
    assert_eq!(tier.triple_count(), 1);
    assert_eq!(live_quads(&tier), BTreeSet::from([(0, 1, 100, 2)]));

    // And it is idempotent from there.
    assert_eq!(tier.apply_quad_batch(&[], &[q]).unwrap().inserted, 0);
}
