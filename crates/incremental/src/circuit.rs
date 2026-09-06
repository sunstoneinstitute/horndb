//! `Circuit` — the SPEC-06 driver: ONE delta-incremental `tick()` path for
//! insertions and retractions alike (SPEC-24 S1, PLAN-24-01).
//!
//! The incremental state, on top of the two bases:
//!
//! - `extent`: the set-semantics extent rules join against. Invariant:
//!   `t ∈ extent ⟺ asserted_base.get(t) > 0 ∨ derived_base.get(t) > 0`.
//!   Maintained incrementally via `note_presence_change` — never rebuilt.
//! - `rule_weights`: per derived row, per rule, the count of **one-step**
//!   derivations over the current `extent`. This is the DBSP incremental
//!   `distinct` trace. Recursion stays finite because the extent is a set
//!   (weights count one-step derivations, not paths). A positive weight
//!   alone does NOT prove a row derivable — it may be cyclic self-support —
//!   so deletion runs as a two-phase DRed-with-weights fixpoint (PLAN-24-01
//!   AMENDED): R1 overdeletes every row that loses ≥ 1 one-step derivation,
//!   R2 re-derives from the well-founded survivors, and only the net
//!   `RuleInferred` transitions publish.
//!
//! `tick()` phases:
//! 1. Drain asserted records into `asserted_base` (publishing each); apply
//!    asserted-presence transitions into the pending extent delta.
//! 2. Closure retract pass (retraction ticks only). A withdrawn row that a
//!    rule still owns (`rule_attr`) keeps its materialization but is
//!    collected as an orphan.
//! 3. Closure insertion pass — always before the rule fixpoint, so closure
//!    edges feed rule bodies in the same tick on every tick kind. Then each
//!    orphan still uncovered (no closure or asserted cover) is seeded -1
//!    into the fixpoint delta: its remaining rule weight may be cyclic-only,
//!    so R1/R2 must re-check it.
//! 4. Rule fixpoint. Each round feeds the pending presence delta through
//!    every plan's stateful delta operator against the frozen pre-round
//!    extent, folds the delta into `extent`, and applies weight updates.
//!    A tick with no negative presence changes runs the positive fixpoint
//!    alone, materializing and publishing per round. A retraction tick runs
//!    R1 (overdelete cascade) then R2 (re-derive), buffering rule events and
//!    publishing only the net transitions afterward — an overdeleted row
//!    that R2 re-adds produces no feed transient.
//! 5. Net-delta feed flush (SPEC-24 S3): derived emissions accumulated in a
//!    tick-local Z-set keyed by `(triple, kind)` publish as non-zero nets
//!    only, in key order. Asserted records keep per-record publish semantics
//!    in phase 1 (they are the user's own operations, in the user's order).
//! 6. Metrics.
//!
//! The Stage-1 full recompute (`recompute_rule_closure`) is demoted to a
//! config-gated debug fallback: `new_with_recompute_fallback()` runs the old
//! recompute-and-diff regime on retraction ticks, then resyncs the
//! incremental state from scratch (`resync_incremental_state`).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Instant;

use horndb_storage::{GraphId, StorageError, Store};

use crate::change_feed::{ChangeFeed, ChangeFeedRx, LagPolicy};
use crate::checkpoint::{Checkpoint, CheckpointPolicy};
use crate::closure_plan::ClosureRule;
use crate::delta_log::DeltaLog;
use crate::extent::PredExtent;
use crate::operator::NaryPlan;
use crate::snapshot::Snapshot;
use crate::types::{DerivationKind, LogicalTime, Multiplicity, RuleId, TripleId};
use crate::zset::Zset;

/// Bound on rule-fixpoint rounds, per phase. A runaway means a
/// non-terminating ruleset; surface it loudly rather than hang (matches the
/// old recompute guard; never a silent break).
const MAX_FIXPOINT_ROUNDS: usize = 4096;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TickReport {
    pub asserted_merged: usize,
    /// NET derived records published to the change feed this tick (SPEC-24
    /// S3) — a row withdrawn and re-derived in the same tick counts zero, so
    /// this equals what a subscriber receives. It is NOT the amount of
    /// derivation work done: the
    /// `horndb_incremental_derived_merged_total` metric keeps that raw
    /// emission count.
    pub derived_merged: usize,
    pub logical_time: LogicalTime,
}

pub struct Circuit {
    asserted_base: Zset<TripleId>,
    derived_base: Zset<TripleId>,
    log: DeltaLog,
    plans: Vec<(NaryPlan, RuleId)>,
    closure_plans: Vec<Box<dyn ClosureRule>>,
    feed: ChangeFeed,
    /// SPEC-24 S3 — tick-local net of derived emissions, keyed by
    /// `(triple, kind)`. `emit_derived` accumulates here instead of
    /// publishing; `flush_derived_feed` publishes the non-zero nets in key
    /// order at tick end, so a same-tick withdraw+re-add never reaches a
    /// subscriber. Empty between ticks — `tick()` clears it on entry so a
    /// panic caught mid-tick cannot leak stale emissions into the next one.
    pending_derived: Zset<(TripleId, DerivationKind)>,
    derived_clock: LogicalTime,
    /// Rule that owns each **rule-materialized** derived row: the keys are
    /// exactly the rows present in `derived_base` because a rule owns them.
    /// `t ∈ rule_attr ⟹ total_weight(t) > 0 ∧ derived_base.get(t) > 0`.
    /// A row derivable while already asserted- or closure-covered records
    /// weights but gets no attr entry (the Stage-1 "newly present" filter).
    /// Closure-inferred rows are deliberately absent (they are owned by the
    /// closure plans via `closure_support`). Used to attribute withdrawal
    /// records to the right `RuleId`.
    rule_attr: BTreeMap<TripleId, RuleId>,
    /// Triples the closure pass owns as **materialized derived rows**.
    ///
    /// Invariant: `closure_support ⊆ derived_base`. A triple is recorded here
    /// only when the closure pass actually materializes it in `derived_base`
    /// (it was absent from both bases), OR when a rule already put it in
    /// `derived_base` and the closure plan re-emits it (the Finding-2 overlap
    /// case). A triple that the closure plan emits but is present only because
    /// it is *asserted* is deliberately NOT recorded: after the asserted copy
    /// is retracted it would be absent from both bases yet still treated as a
    /// live input — a ghost that keeps stale rule consequences alive.
    ///
    /// The insertion closure pass only ever *adds* here; the closure
    /// **retraction** pass (F6) removes a row when its base support is gone,
    /// zeroing the matching `derived_base` row unless a rule still owns it
    /// (`rule_attr`) — such orphans are reseeded into the rule fixpoint so
    /// cyclic-only rule weight cannot keep them alive (codex P1).
    /// The rule-fixpoint withdrawal never zeroes a
    /// `closure_support` row in `derived_base` (it skips them), so the
    /// invariant is preserved across ticks. The distinct transitions read
    /// this set to decide whether a row losing its rule weight keeps its
    /// closure-owned materialization (Finding 2); the recompute fallback
    /// additionally seeds `recompute_rule_closure` from it (Finding 1).
    closure_support: BTreeSet<TripleId>,
    /// SPEC-24 S1 — per-row, per-rule count of one-step derivations over the
    /// current `extent` (the incremental-`distinct` trace). Zero-weight
    /// per-rule entries and zero-total rows are pruned eagerly.
    rule_weights: BTreeMap<TripleId, BTreeMap<RuleId, i64>>,
    /// SPEC-24 S1 — the set-semantics extent rules join against (the shared
    /// z⁻¹ integrated input trace). Invariant: a present `t` sits in `extent`
    /// at multiplicity exactly one, present meaning
    /// `asserted_base.get(t) > 0 ∨ derived_base.get(t) > 0`. Indexed by
    /// predicate (SPEC-24 S7) so `NaryPlan` leaves can bind to a single
    /// predicate's rows instead of scanning the whole extent.
    extent: PredExtent,
    /// When true, retraction-containing ticks run the Stage-1 recompute
    /// regime instead of the unified delta path (debug fallback), followed by
    /// `resync_incremental_state`. Insertion-only ticks always use the
    /// unified path.
    recompute_fallback: bool,
    /// SPEC-06 F8 / SPEC-24 S5 — the checkpoint cadence, and what has
    /// accumulated since the last one. Armed only once
    /// [`Circuit::attach_input_log`] has made the log durable: a circuit with
    /// no input log has nothing to truncate.
    checkpoint_policy: CheckpointPolicy,
    deltas_since_checkpoint: usize,
    last_checkpoint: Instant,
    /// SPEC-24 S6 — the storage view [`Circuit::snapshot`] pins, and the graphs
    /// whose union is this circuit's `(asserted ∪ derived)` set. `None` until
    /// [`Circuit::attach_store`] runs: an in-memory-only circuit (unit tests,
    /// benches) has no reader view.
    store_view: Option<StoreView>,
}

/// Where [`Circuit::snapshot`] reads from (SPEC-24 S6, ADR-0018).
struct StoreView {
    store: Arc<Store>,
    graphs: Arc<[GraphId]>,
}

impl Default for Circuit {
    fn default() -> Self {
        Self::new()
    }
}

impl Circuit {
    pub fn new() -> Self {
        Self::with_fallback(false)
    }

    /// A circuit whose retraction-containing ticks run the Stage-1 recompute
    /// regime (debug fallback; O(store) per retraction tick). Insertion-only
    /// ticks use the unified delta path unchanged.
    pub fn new_with_recompute_fallback() -> Self {
        Self::with_fallback(true)
    }

    fn with_fallback(recompute_fallback: bool) -> Self {
        Self {
            asserted_base: Zset::new(),
            derived_base: Zset::new(),
            log: DeltaLog::new(),
            plans: Vec::new(),
            closure_plans: Vec::new(),
            feed: ChangeFeed::new(),
            pending_derived: Zset::new(),
            derived_clock: 0,
            rule_attr: BTreeMap::new(),
            closure_support: BTreeSet::new(),
            rule_weights: BTreeMap::new(),
            extent: PredExtent::new(),
            recompute_fallback,
            checkpoint_policy: CheckpointPolicy::default(),
            deltas_since_checkpoint: 0,
            last_checkpoint: Instant::now(),
            store_view: None,
        }
    }

    /// Make the input log durable through `store`'s write-ahead log (SPEC-24
    /// S5, ADR-0018): every `assert_triple` / `retract_triple` becomes an
    /// `Input` record and every tick writes a `TickCommit` marker, so a
    /// crash replays to the same Z-set state. Arms the SPEC-06 F8 checkpoint
    /// cadence.
    ///
    /// The driver that attaches an input log owns durability of the tick
    /// *outcomes* as well: a checkpoint truncates the log, so whatever the
    /// ticks produced must already be committed to `store` by then. The
    /// SPEC-24 S4 engine wiring is that driver.
    pub fn attach_input_log(&mut self, store: Arc<Store>) -> Result<(), StorageError> {
        self.log.attach_wal(store)?;
        self.last_checkpoint = Instant::now();
        self.deltas_since_checkpoint = 0;
        Ok(())
    }

    /// Override the SPEC-06 F8 cadence (default: 1 minute or 100K deltas,
    /// whichever comes first).
    pub fn set_checkpoint_policy(&mut self, policy: CheckpointPolicy) {
        self.checkpoint_policy = policy;
    }

    /// Re-submit the inputs the attached store recovered at open (ADR-0018):
    /// each batch a tick marker closed replays as its own tick, and the
    /// un-ticked tail is left pending for the next [`Circuit::tick`].
    /// Returns the number of records replayed.
    ///
    /// Call it once, right after [`Circuit::attach_input_log`] and before any
    /// new input: replayed records keep their original logical times.
    pub fn recover(&mut self) -> usize {
        let Some(store) = self.log.store() else {
            return 0;
        };
        let recovered = store.take_recovered_inputs();
        let mut replayed = 0;
        for batch in &recovered.ticks {
            for rec in batch {
                self.log.append_recovered(rec);
            }
            replayed += batch.len();
            self.tick();
        }
        for rec in &recovered.pending {
            self.log.append_recovered(rec);
        }
        replayed + recovered.pending.len()
    }

    /// Checkpoint now (SPEC-06 F8): fold every pending input into the bases,
    /// persist the attached store, and truncate the input log — the new log
    /// generation starts from the checkpoint's rows. A no-op without an
    /// input log.
    pub fn checkpoint(&mut self) -> Result<(), StorageError> {
        if self.log.store().is_none() {
            return Ok(());
        }
        // No un-ticked input may cross a checkpoint boundary: truncation
        // would drop it, and folding it into the base without running the
        // rules would leave the derived state behind.
        if !self.log.is_empty() {
            self.tick();
        }
        if let Some(store) = self.log.store() {
            store.checkpoint()?;
        }
        self.deltas_since_checkpoint = 0;
        self.last_checkpoint = Instant::now();
        Ok(())
    }

    /// The F8 scheduler, run at the end of every tick. Panics if the
    /// checkpoint fails: the log can no longer be truncated, so continuing
    /// would silently grow it past the point recovery is bounded by.
    fn maybe_checkpoint(&mut self, deltas: usize) {
        if self.log.store().is_none() {
            return;
        }
        self.deltas_since_checkpoint += deltas;
        if self.deltas_since_checkpoint >= self.checkpoint_policy.deltas
            || self.last_checkpoint.elapsed() >= self.checkpoint_policy.interval
        {
            self.checkpoint().expect("SPEC-06 F8 checkpoint");
        }
    }

    pub fn add_plan(&mut self, plan: NaryPlan, attribution: RuleId) {
        self.plans.push((plan, attribution));
    }

    /// Register a closure operator (SPEC-06 F5). On each tick its
    /// `apply_insert_delta` runs over the asserted insertion delta and the
    /// newly inferred triples are merged into `derived_base`, published as
    /// `DerivationKind::ClosureInferred`.
    ///
    /// A plan registered against a `Circuit` that already contains edges the
    /// plan depends on must be pre-seeded with the existing closed state (see
    /// `TransitiveClosureRule::seed_closed_edges`); plans registered on an
    /// empty circuit need no seeding.
    pub fn add_closure_plan(&mut self, rule: Box<dyn ClosureRule>) {
        self.closure_plans.push(rule);
    }

    pub fn subscribe(&self) -> ChangeFeedRx {
        self.feed.subscribe()
    }

    /// Subscribe with a bounded buffer and an explicit lag policy (SPEC-24 S3).
    /// See [`ChangeFeed::subscribe_bounded`].
    pub fn subscribe_bounded(&self, capacity: usize, policy: LagPolicy) -> ChangeFeedRx {
        self.feed.subscribe_bounded(capacity, policy)
    }

    /// Live change-feed subscribers (the value behind the
    /// `incremental_change_feed_subscribers` gauge). Drops — receiver closed,
    /// or a `DisconnectSlow` subscriber that lagged — are reflected on the
    /// next publish.
    pub fn subscriber_count(&self) -> usize {
        self.feed.subscriber_count()
    }

    pub fn asserted_base(&self) -> &Zset<TripleId> {
        &self.asserted_base
    }
    pub fn derived_base(&self) -> &Zset<TripleId> {
        &self.derived_base
    }

    /// Bind this circuit's reader view to `store` (SPEC-24 S6). `graphs` are
    /// the graphs whose union is the circuit's materialized `(asserted ∪
    /// derived)` set — for the SPEC-24 S4 engine wiring, the default graph
    /// (asserted rows) plus the derived-mirror graph.
    ///
    /// The circuit never writes through this handle: the caller that commits
    /// a tick's batch to storage owns the write path, and this is the read
    /// side of the same commit versions (ADR-0018, one clock).
    pub fn attach_store(&mut self, store: Arc<Store>, graphs: Vec<GraphId>) {
        self.store_view = Some(StoreView {
            store,
            graphs: graphs.into(),
        });
    }

    /// Acquire an MVCC [`Snapshot`] (SPEC-24 S6): a pinned, consistent
    /// `(asserted ∪ derived)` view at the store's current commit version.
    /// O(1) — an `Arc` clone plus a pin-count bump, with no presence set
    /// materialized in the circuit. The snapshot survives subsequent `tick()`s
    /// until dropped; readers and writers never block.
    ///
    /// `None` until [`Circuit::attach_store`] has bound a store: an
    /// in-memory-only circuit has nothing to pin (ADR-0018).
    pub fn snapshot(&self) -> Option<Snapshot> {
        let view = self.store_view.as_ref()?;
        Some(Snapshot::new(view.store.pin(), Arc::clone(&view.graphs)))
    }

    /// Build the presence-set view `asserted ∪ derived` (each present triple at
    /// multiplicity 1). Positive multiplicities only: a net-zero/negative triple
    /// is absent. O(|asserted| + |derived|); used by the recompute-fallback
    /// resync to rebuild `extent` from scratch. Reader snapshots do NOT go
    /// through here — they ride storage MVCC (SPEC-24 S6).
    ///
    /// We take only **positive** multiplicities (`m > 0`) — the same presence
    /// rule the whole tick path uses. A net-negative count (a retraction of a
    /// triple that was not asserted, or duplicate retractions) is *not*
    /// present, and a net-zero (asserted then retracted) triple is absent.
    /// Summing the two Z-sets would instead expose multiplicity 2+ for a
    /// triple that is both asserted and derived, or asserted twice — a
    /// multiset, which an RDF reader surface must not be.
    fn materialize_presence(&self) -> Zset<TripleId> {
        let mut materialized: Zset<TripleId> = Zset::new();
        for (triple, mult) in self.asserted_base.iter() {
            if mult > 0 {
                materialized.add(*triple, 1);
            }
        }
        for (triple, mult) in self.derived_base.iter() {
            if mult > 0 && materialized.get(triple) == 0 {
                materialized.add(*triple, 1);
            }
        }
        materialized
    }

    /// Present in either base (the extent-membership predicate).
    fn base_presence(&self, t: &TripleId) -> bool {
        self.asserted_base.get(t) > 0 || self.derived_base.get(t) > 0
    }

    /// Total one-step derivation weight of `t` (0 when untracked).
    fn total_weight(&self, t: &TripleId) -> i64 {
        self.rule_weights
            .get(t)
            .map(|per_rule| per_rule.values().sum())
            .unwrap_or(0)
    }

    /// Lowest `RuleId` with positive weight for `t` — the deterministic
    /// (BTreeMap-order) attribution choice when a row is rule-materialized.
    fn first_attr_rule(&self, t: &TripleId) -> Option<RuleId> {
        self.rule_weights
            .get(t)?
            .iter()
            .find(|(_, w)| **w > 0)
            .map(|(rid, _)| *rid)
    }

    /// Record into `delta` the extent-membership change for `t` implied by the
    /// bases, WITHOUT mutating `extent` (the fixpoint folds deltas only at
    /// round boundaries). The Zset add/cancel semantics make same-tick
    /// lose-then-regain presence (e.g. drain retract → closure promote) a
    /// net-zero entry automatically.
    fn note_presence_change(&self, t: TripleId, delta: &mut Zset<TripleId>) {
        let now = self.asserted_base.get(&t) > 0 || self.derived_base.get(&t) > 0;
        let pending = self.extent.get(&t) + delta.get(&t) > 0;
        if now && !pending {
            delta.add(t, 1);
        } else if !now && pending {
            delta.add(t, -1);
        }
    }

    /// Record one derived delta into this tick's net buffer.
    ///
    /// Every derived row — rule- or closure-inferred, added or withdrawn —
    /// flows through here, so netting the tick (SPEC-24 S3) needs no other
    /// interception point. Nothing is published until
    /// [`flush_derived_feed`](Self::flush_derived_feed) runs at tick end.
    /// Returns 1 so callers can fold it into their raw-emission count (which
    /// drives snapshot invalidation; the reported `derived_merged` counts net
    /// records instead).
    fn emit_derived(
        &mut self,
        triple: TripleId,
        mult: Multiplicity,
        kind: DerivationKind,
    ) -> usize {
        self.pending_derived.add((triple, kind), mult);
        1
    }

    /// Publish this tick's net derived records — non-zero nets only, in
    /// `(triple, kind)` key order — and advance the derived logical clock once
    /// per published record. Returns the number of net records published,
    /// which is what `TickReport::derived_merged` reports (SPEC-24 S3).
    fn flush_derived_feed(&mut self) -> usize {
        // Not `mem::take`: `DerivationKind` has no `Default`, so the derived
        // `Default` bound on `Zset` does not apply to this key type.
        let pending = std::mem::replace(&mut self.pending_derived, Zset::new());
        let mut published = 0usize;
        for (&(triple, kind), mult) in pending.iter() {
            // Per (triple, kind) a tick adds or withdraws a row at most once,
            // so a net record is always ±1. Consumers rely on this (see
            // INTEGRATION-NOTES.md). Note the per-TRIPLE total can still be
            // -2 across two different kinds — a closure withdrawal plus a rule
            // withdrawal of the same row — which is why a consumer must sum
            // per triple over the whole tick, not react per record.
            debug_assert!(
                mult.abs() <= 1,
                "net derived record for {triple:?}/{kind:?} has |mult| = {} > 1",
                mult.abs()
            );
            let time = self.derived_clock;
            self.derived_clock = self
                .derived_clock
                .checked_add(1)
                .expect("derived-clock overflow");
            self.feed.publish(triple, mult, time, kind);
            published += 1;
        }
        published
    }

    /// Materialize a rule-owned derived row: add it to `derived_base`, record
    /// its attribution, and publish the positive `RuleInferred`. Returns 1 so
    /// callers fold it into their merged-row count. Callers own any surrounding
    /// presence-delta bookkeeping (it differs per call site).
    fn materialize_rule_row(&mut self, t: TripleId, rid: RuleId) -> usize {
        self.derived_base.add(t, 1);
        self.rule_attr.insert(t, rid);
        self.emit_derived(t, 1, DerivationKind::RuleInferred(rid))
    }

    /// Withdraw a materialized derived row: zero its `derived_base` entry (if
    /// any) and publish the negative delta under `kind`. Returns 1 so callers
    /// fold it into their merged-row count. Callers own any surrounding
    /// `closure_support`/`rule_attr` and presence-delta bookkeeping.
    fn withdraw_derived_row(&mut self, t: TripleId, kind: DerivationKind) -> usize {
        let cur = self.derived_base.get(&t);
        if cur != 0 {
            self.derived_base.add(t, -cur);
        }
        self.emit_derived(t, -1, kind)
    }

    /// One rule-fixpoint round: feed `round_delta` through every plan's
    /// stateful delta operator against the frozen PRE-round extent (the
    /// "old-old" delta convention), then fold `round_delta` into the extent.
    /// Returns the per-key merged weight deltas, in key order.
    fn fixpoint_round(
        &mut self,
        round_delta: &Zset<TripleId>,
    ) -> BTreeMap<TripleId, Vec<(RuleId, i64)>> {
        // Take the plans out of `self` for the duration of the calls — the
        // same borrow dance the closure passes use.
        let mut plans = std::mem::take(&mut self.plans);
        let mut merged: BTreeMap<TripleId, Vec<(RuleId, i64)>> = BTreeMap::new();
        for (plan, rid) in &mut plans {
            let raw = plan.apply_delta_stateful(&self.extent, round_delta);
            for (t, m) in raw.iter() {
                merged.entry(*t).or_default().push((*rid, m));
            }
        }
        self.plans = plans;
        self.extent.add_assign(round_delta);
        merged
    }

    /// Fold one round's per-rule weight deltas for `t` into the trace,
    /// pruning zero per-rule entries and removing the row when its total
    /// reaches 0. Returns `(old_total, new_total)`.
    fn apply_weight_deltas(&mut self, t: TripleId, deltas: &[(RuleId, i64)]) -> (i64, i64) {
        let old_total = self.total_weight(&t);
        {
            let per_rule = self.rule_weights.entry(t).or_default();
            for (rid, dm) in deltas {
                let w = per_rule.entry(*rid).or_insert(0);
                *w += dm;
                if *w == 0 {
                    per_rule.remove(rid);
                }
            }
        }
        let new_total = self.total_weight(&t);
        debug_assert!(
            new_total >= 0,
            "derivation weight went negative for {t:?}: {new_total}"
        );
        if new_total == 0 {
            self.rule_weights.remove(&t);
        }
        (old_total, new_total)
    }

    /// Positive-only rule fixpoint — the insertion fast path. This is the
    /// amended plan's R2 alone, with per-round materialization and
    /// publishing (nothing was overdeleted, so no buffering is needed).
    /// Returns `(rounds_run, derived_rows_merged)`.
    fn run_insertion_fixpoint(&mut self, init_delta: Zset<TripleId>) -> (usize, usize) {
        let mut rounds_run = 0usize;
        let mut merged_rows = 0usize;
        let mut round_delta = init_delta;
        while !round_delta.is_empty() {
            rounds_run += 1;
            assert!(
                rounds_run <= MAX_FIXPOINT_ROUNDS,
                "rule fixpoint failed to converge within {MAX_FIXPOINT_ROUNDS} rounds"
            );
            let merged = self.fixpoint_round(&round_delta);
            let mut next_delta: Zset<TripleId> = Zset::new();
            for (t, deltas) in merged {
                let (old_total, new_total) = self.apply_weight_deltas(t, &deltas);
                debug_assert!(
                    new_total >= old_total,
                    "insertion-only round decreased a weight for {t:?}"
                );
                // Incremental distinct: materialize on the 0 → positive
                // crossing when not asserted- or closure-covered; otherwise
                // record weights only (no attr, no feed, no extent change).
                if old_total <= 0
                    && new_total > 0
                    && self.asserted_base.get(&t) <= 0
                    && self.derived_base.get(&t) == 0
                {
                    let rid = self
                        .first_attr_rule(&t)
                        .expect("positive total weight implies a positive per-rule weight");
                    merged_rows += self.materialize_rule_row(t, rid);
                    self.note_presence_change(t, &mut next_delta);
                }
            }
            round_delta = next_delta;
        }
        (rounds_run, merged_rows)
    }

    /// Two-phase DRed-with-weights retraction fixpoint (PLAN-24-01 AMENDED).
    ///
    /// A positive weight alone does not prove derivability — it may be cyclic
    /// self-support. So: **R1** cascades overdeletion (every row that loses
    /// ≥ 1 one-step derivation is provisionally removed from the extent);
    /// after R1 every extent survivor is well-founded by induction. **R2**
    /// re-derives from the survivors plus this tick's positive changes; a
    /// candidate whose weight over the surviving extent is positive is
    /// re-added, while cyclic-only support ends with weight 0 and stays dead.
    /// Rule feed events are buffered: after R2 each touched row's final state
    /// is compared to its pre-tick state and only the NET `RuleInferred`
    /// transitions publish — an overdelete-then-re-add produces no transient.
    /// Returns `(rounds_run, derived_rows_merged)`.
    fn run_two_phase_fixpoint(&mut self, init_delta: Zset<TripleId>) -> (usize, usize) {
        let mut merged_rows = 0usize;

        // Split the pending presence delta by sign. A row whose presence
        // dropped while it still has recorded weight (a retracted asserted
        // row, or a withdrawn closure row rules also derive) is an overdelete
        // candidate like any other — R2 decides whether it is re-added.
        let mut r1_seed: Zset<TripleId> = Zset::new();
        let mut r2_seed: Zset<TripleId> = Zset::new();
        let mut candidates: BTreeSet<TripleId> = BTreeSet::new();
        // Every row touched by R1/R2 (a weight moved), for the net transitions.
        // Only the key set matters — the pre-tick totals themselves are unused.
        let mut touched: BTreeSet<TripleId> = BTreeSet::new();
        for (t, m) in init_delta.iter() {
            if m < 0 {
                r1_seed.add(*t, m);
                if self.total_weight(t) > 0 {
                    candidates.insert(*t);
                    touched.insert(*t);
                }
            } else {
                r2_seed.add(*t, m);
            }
        }

        // ---- R1: overdelete cascade ----
        let mut rounds_run = 0usize;
        let mut round_delta = r1_seed;
        while !round_delta.is_empty() {
            rounds_run += 1;
            assert!(
                rounds_run <= MAX_FIXPOINT_ROUNDS,
                "rule fixpoint (overdelete) failed to converge within {MAX_FIXPOINT_ROUNDS} rounds"
            );
            let merged = self.fixpoint_round(&round_delta);
            let mut next_delta: Zset<TripleId> = Zset::new();
            for (t, deltas) in merged {
                let (old_total, new_total) = self.apply_weight_deltas(t, &deltas);
                touched.insert(t);
                debug_assert!(
                    new_total <= old_total,
                    "overdelete round increased a weight for {t:?}"
                );
                if new_total < old_total && !candidates.contains(&t) {
                    // Lost ≥ 1 one-step derivation → candidate. Only rows not
                    // asserted- and not closure-covered are provisionally
                    // removable from the extent; covered rows keep their
                    // presence and only their weights move.
                    if self.asserted_base.get(&t) <= 0 && !self.closure_support.contains(&t) {
                        candidates.insert(t);
                        if self.extent.get(&t) + next_delta.get(&t) > 0 {
                            next_delta.add(t, -1);
                        }
                    }
                }
            }
            round_delta = next_delta;
        }

        // ---- R2 seed: this tick's positive changes + re-addable candidates.
        // A candidate with positive weight over the surviving extent has
        // well-founded support (R1 left the survivors' weights exact) and is
        // re-added; cyclic-only support can never re-add a row because R2
        // grows from well-founded rows only.
        for t in &candidates {
            if self.total_weight(t) > 0 && self.extent.get(t) + r2_seed.get(t) <= 0 {
                r2_seed.add(*t, 1);
            }
        }

        // ---- R2: re-derive ----
        let mut r2_rounds = 0usize;
        let mut round_delta = r2_seed;
        while !round_delta.is_empty() {
            r2_rounds += 1;
            assert!(
                r2_rounds <= MAX_FIXPOINT_ROUNDS,
                "rule fixpoint (re-derive) failed to converge within {MAX_FIXPOINT_ROUNDS} rounds"
            );
            let merged = self.fixpoint_round(&round_delta);
            let mut next_delta: Zset<TripleId> = Zset::new();
            for (t, deltas) in merged {
                let (old_total, new_total) = self.apply_weight_deltas(t, &deltas);
                touched.insert(t);
                debug_assert!(
                    new_total >= old_total,
                    "re-derive round decreased a weight for {t:?}"
                );
                // 0 → positive crossing: the row becomes present unless the
                // pending extent already has it (asserted-/closure-covered
                // rows never left; re-added candidates were seeded).
                if old_total <= 0 && new_total > 0 && self.extent.get(&t) + next_delta.get(&t) <= 0
                {
                    next_delta.add(t, 1);
                }
            }
            round_delta = next_delta;
        }
        rounds_run += r2_rounds;

        // ---- Net distinct transitions (PLAN-24-01 "Distinct transitions
        // (net, after R2)"), publish once, in key order. `rule_attr` was not
        // touched by R1/R2, so it still reflects the pre-tick attribution.
        for t in touched {
            let new_total = self.total_weight(&t);
            let was_attr = self.rule_attr.contains_key(&t);
            let covered = self.asserted_base.get(&t) > 0 || self.closure_support.contains(&t);
            let in_extent = self.extent.get(&t) > 0;
            debug_assert!(
                new_total <= 0 || in_extent,
                "positive-weight row left out of the extent: {t:?}"
            );
            // The row should be rule-materialized iff it is derivable over
            // the final extent and either a rule already owned it (it keeps
            // its row through asserted/closure co-coverage — divergences 1
            // and 2) or nothing else covers it.
            let should = new_total > 0 && in_extent && (was_attr || !covered);
            if should {
                let rid = self
                    .first_attr_rule(&t)
                    .expect("positive total weight implies a positive per-rule weight");
                if was_attr {
                    // Keeps its row; refresh the attribution from the
                    // surviving weights (silent change is fine).
                    self.rule_attr.insert(t, rid);
                } else {
                    // Newly rule-materialized: a fresh derivation, or an
                    // overdeleted / asserted-retracted row R2 re-added.
                    debug_assert_eq!(
                        self.derived_base.get(&t),
                        0,
                        "un-attributed, non-closure row must not be in derived_base"
                    );
                    merged_rows += self.materialize_rule_row(t, rid);
                }
            } else if was_attr {
                let rid = self.rule_attr.remove(&t).expect("checked contains_key");
                if self.closure_support.contains(&t) {
                    // The closure still owns the row: rule ownership lapses
                    // silently (no feed, no derived_base change).
                } else {
                    // Withdraw the rule-owned materialization. A row still
                    // asserted (derive-then-assert case) keeps its presence
                    // via `asserted_base`; a dead candidate is already out of
                    // the extent.
                    merged_rows += self.withdraw_derived_row(t, DerivationKind::RuleInferred(rid));
                }
            }
            // else: record-only weights or a dead un-materialized row —
            // nothing to publish, presence unchanged.
        }
        (rounds_run, merged_rows)
    }

    /// Append an insertion to the pending log. Kind = Asserted.
    pub fn assert_triple(&mut self, triple: TripleId) {
        self.log.append(triple, 1, DerivationKind::Asserted);
    }

    /// Append a retraction. Consequences whose support disappears are
    /// withdrawn delta-incrementally on the next `tick()`.
    pub fn retract_triple(&mut self, triple: TripleId) {
        self.log.append(triple, -1, DerivationKind::Asserted);
    }

    pub fn tick(&mut self) -> TickReport {
        let t_tick = std::time::Instant::now();
        // Defensive: a tick that panicked after an `emit_derived` and was
        // caught upstream would otherwise republish its emissions here.
        self.pending_derived = Zset::new();

        // ---- Phase 1: drain asserted records ----
        //
        // The records leave the pending log first, so the base advance below
        // is SPEC-06 F8's collapsing merge over exactly this tick's batch.
        let mut tick_log = self.log.take_pending();
        let asserted_records: Vec<_> = tick_log.iter().copied().collect();
        let mut asserted_delta: Zset<TripleId> = Zset::new();
        let mut has_retraction = false;
        for rec in &asserted_records {
            if rec.mult < 0 {
                has_retraction = true;
            }
            asserted_delta.add(rec.triple, rec.mult);
        }
        // Pre-merge asserted presence per net-changed triple. A triple whose
        // records net to zero this tick cannot flip presence, so the keys of
        // `asserted_delta` cover every possible flip.
        let prev_present: Vec<(TripleId, bool)> = asserted_delta
            .iter()
            .map(|(t, _)| (*t, self.asserted_base.get(t) > 0))
            .collect();
        // F8 merge: collapse the batch into the base (`Zset::add` prunes the
        // rows that reach multiplicity zero), then publish in append order.
        let asserted_merged = Checkpoint::merge(&mut self.asserted_base, &mut tick_log).merged;
        for rec in &asserted_records {
            self.feed.publish_record(*rec);
        }
        let logical_time = asserted_records.last().map(|r| r.time).unwrap_or(0);

        // The closure INSERTION pass must only ever see base edges that are
        // PRESENT post-tick. We materialise this positive insertion delta only
        // when there are closure plans to feed (a no-closure circuit must not
        // pay an O(|Δ|) clone per tick; the empty placeholder is never read
        // because the closure loop has no iterations when `closure_plans` is
        // empty).
        //
        // Finding 1: filter by POST-TICK presence (`asserted_base.get(t) > 0`),
        // not raw `m > 0` from this tick's delta. The asserted records were
        // already merged into `asserted_base` in the drain loop above, so `get`
        // is post-tick here. An edge over-retracted to a NEGATIVE multiplicity
        // and then partially re-asserted so its NET post-tick multiplicity is
        // `<= 0` is ABSENT; a raw `m > 0` filter would still feed it to the
        // closure backend, deriving closure edges off an absent base edge. This
        // is the same present/absent boundary `materialize_presence` and the
        // whole tick path use. Normal inserts (an edge whose multiplicity
        // becomes +1) still flow through: their post-tick presence holds.
        let asserted_delta_for_closure = if self.closure_plans.is_empty() {
            Zset::new()
        } else {
            Zset::from_iter(
                asserted_delta
                    .iter()
                    .filter(|(t, m)| *m > 0 && self.asserted_base.get(t) > 0)
                    .map(|(t, _)| (*t, 1)),
            )
        };

        // F6 closure-path retraction: the negative-only asserted delta the
        // closure plans use to withdraw `ClosureInferred` rows. Built only when
        // there are closure plans AND this tick actually withdraws something —
        // a no-closure or insertion-only circuit pays nothing (mirrors the
        // positive-only guard above).
        //
        // P2 (multiplicity-aware base deletion): a base edge must be deleted
        // from the closure backend ONLY when its POST-TICK asserted multiplicity
        // reaches 0 (genuinely gone). `asserted_base` is a Z-set multiset —
        // `assert_triple` appends `(triple, +1)` each call — so an edge asserted
        // twice then retracted once still has multiplicity +1 and its base edge
        // must SURVIVE. The asserted records were already merged into
        // `asserted_base` in the drain loop above, so `asserted_base.get` is
        // post-tick here; we keep only edges withdrawn this tick (`m < 0`) whose
        // post-tick multiplicity is exactly 0. We negate to `-1` (the backend's
        // `delete_transitive_edges` only checks `mult < 0`, not the magnitude;
        // one logical deletion regardless of how many copies were withdrawn).
        let asserted_delta_for_closure_retract = if self.closure_plans.is_empty() || !has_retraction
        {
            Zset::new()
        } else {
            Zset::from_iter(
                asserted_delta
                    .iter()
                    // Presence boundary: the base edge is genuinely gone iff its
                    // POST-TICK asserted multiplicity is non-positive (Finding 4).
                    // `== 0` would wrongly suppress the deletion for an edge
                    // over-retracted to a NEGATIVE multiplicity (asserted once,
                    // retracted twice in one tick). This is the same
                    // present/absent boundary `materialize_presence` uses
                    // (`m > 0` ⇒ present).
                    .filter(|(t, m)| *m < 0 && self.asserted_base.get(t) <= 0)
                    .map(|(t, _)| (*t, -1)),
            )
        };

        let mut derived_merged = 0;
        let rounds_run: usize;
        let mut withdraw_n = 0u64;
        let mut promote_n = 0u64;

        if self.recompute_fallback && has_retraction {
            // Stage-1 debug fallback: old recompute-and-diff regime, then a
            // from-scratch resync of the incremental state.
            let (dm, rr, wn, pn) = self.run_recompute_fallback(
                &asserted_delta_for_closure,
                &asserted_delta_for_closure_retract,
            );
            derived_merged = dm;
            rounds_run = rr;
            withdraw_n = wn;
            promote_n = pn;
        } else {
            // Pending extent-membership delta that seeds the rule fixpoint.
            let mut init_delta: Zset<TripleId> = Zset::new();

            // Asserted-presence transitions (PLAN-24-01 "Asserted-presence
            // transitions", as amended): presence flips flow into the pending
            // extent delta. A retracted row with positive rule weight is NOT
            // eagerly materialized — the weight may be cyclic-only self-
            // support. It becomes an R1 overdelete candidate; R2 re-adds it
            // (and the net publish materializes it) only when its support is
            // well-founded.
            for (t, was_present) in prev_present {
                let now_present = self.asserted_base.get(&t) > 0;
                if was_present != now_present {
                    self.note_presence_change(t, &mut init_delta);
                }
            }

            // ---- Phase 2: closure retract pass (retraction ticks only) ----
            let mut closure_orphans: Vec<TripleId> = Vec::new();
            if has_retraction {
                let (dm, wn, pn, orphans) = self
                    .run_closure_retract_pass(&asserted_delta_for_closure_retract, &mut init_delta);
                derived_merged += dm;
                withdraw_n = wn;
                promote_n = pn;
                closure_orphans = orphans;
            }

            // ---- Phase 3: closure insertion pass ----
            // Always BEFORE the rule fixpoint: closure-derived edges feed rule
            // bodies in the same tick on every tick kind (PLAN-24-01
            // divergence 3).
            derived_merged +=
                self.run_closure_insertion_pass(&asserted_delta_for_closure, &mut init_delta);

            // Orphan reseed (codex P1): a rule-owned row that lost its closure
            // ownership in Phase 2 may be resting on cyclic-only rule weight,
            // which a positive weight alone cannot distinguish from real
            // support. Seed a -1 so the two-phase fixpoint re-checks it: R1
            // overdeletes it (it has weight > 0 by invariant iii), R2 re-adds
            // it iff well-founded support survives. Runs AFTER Phase 3 so a
            // same-tick closure re-cover (replacement base path) skips the
            // seed — the row stays covered and is never overdeleted.
            for t in closure_orphans {
                if self.closure_support.contains(&t)
                    || self.asserted_base.get(&t) > 0
                    || !self.rule_attr.contains_key(&t)
                {
                    continue;
                }
                // The row stayed present in `derived_base` through Phases 1-3
                // (nothing withdraws a rule-owned row there), so no presence
                // note was written for it: its `init_delta` entry is 0. The
                // guard keeps the extent removal at exactly -1 even if that
                // ever changes.
                if init_delta.get(&t) == 0 {
                    init_delta.add(t, -1);
                }
            }

            // ---- Phase 4: rule fixpoint (incremental distinct) ----
            //
            // No negative presence change → nothing can be overdeleted: run
            // the positive fixpoint alone with per-round publishing. Any
            // negative presence change → two-phase DRed-with-weights
            // (PLAN-24-01 AMENDED) with net publishing after R2.
            let has_negative = init_delta.iter().any(|(_, m)| m < 0);
            let (rounds, merged) = if has_negative {
                self.run_two_phase_fixpoint(init_delta)
            } else {
                self.run_insertion_fixpoint(init_delta)
            };
            rounds_run = rounds;
            derived_merged += merged;
        }

        // ---- Phase 5/6: net feed flush, metrics ----
        //
        // SPEC-24 S3: publish the tick's NET derived records.
        //
        // `TickReport::logical_time` is the timestamp of the last asserted
        // record merged this tick — a per-record counter kept for diagnostics
        // and for the change feed. It is NOT the reader-visible clock: since
        // ADR-0018 that is the storage commit version, which
        // `Snapshot::logical_time()` reports directly.
        let derived_emissions = derived_merged;
        let derived_merged = self.flush_derived_feed();

        {
            let m = horndb_metrics::metrics();
            m.incremental
                .tick_duration_seconds
                .observe(t_tick.elapsed().as_secs_f64());
            m.incremental.asserted_merged.inc_by(asserted_merged as u64);
            // Deliberately the RAW emission count, not the net: this counter is a
            // tick-COST signal (how much derivation work the tick did), which the
            // perftest skill reads. `TickReport::derived_merged` is the net —
            // what a subscriber actually sees. The two differ on a tick that
            // withdraws and re-derives the same row.
            m.incremental
                .derived_merged
                .inc_by(derived_emissions as u64);
            m.incremental.closure_withdraw.inc_by(withdraw_n);
            m.incremental.closure_promote.inc_by(promote_n);
            m.incremental.fixpoint_rounds.observe(rounds_run as f64);
            m.incremental
                .distinct_trace_keys
                .set(self.rule_weights.len() as i64);
        }

        // ADR-0018: mark the range this tick drained, so recovery replays it
        // as one batch instead of re-submitting it as pending input. The
        // marker syncs the log, so a completed tick is always durable.
        if asserted_merged > 0 {
            self.log.commit_tick(logical_time);
        }
        self.maybe_checkpoint(asserted_merged + derived_emissions);
        TickReport {
            asserted_merged,
            derived_merged,
            logical_time,
        }
    }

    /// Run the closure RETRACTION pass (SPEC-06 F6): withdraw `ClosureInferred`
    /// rows whose base support was retracted this tick, shrink
    /// `closure_support`, and promote deleted-but-still-entailed asserted edges
    /// to materialized closure rows (P1). Every `derived_base` mutation notes
    /// its extent change into `init_delta`. Returns
    /// `(derived_merged, withdraw_n, promote_n, rule_owned_orphans)` — the
    /// orphans are withdrawn rows whose materialization a rule still owns
    /// (`rule_attr`); `tick()` reseeds the fixpoint for any that stay
    /// uncovered after the insertion pass.
    fn run_closure_retract_pass(
        &mut self,
        retract_delta: &Zset<TripleId>,
        init_delta: &mut Zset<TripleId>,
    ) -> (usize, u64, u64, Vec<TripleId>) {
        let mut merged = 0usize;
        let mut withdraw_n = 0u64;
        let mut promote_n = 0u64;
        let mut rule_owned_orphans: Vec<TripleId> = Vec::new();
        // Take the plans out via `mem::take` to satisfy the borrow checker
        // (same pattern as the insertion closure pass).
        let mut closure_plans = std::mem::take(&mut self.closure_plans);
        for rule in &mut closure_plans {
            let crate::closure_plan::ClosureRetractDelta { withdraw, promote } =
                rule.apply_retract_delta(retract_delta);
            withdraw_n += withdraw.len() as u64;
            promote_n += promote.len() as u64;
            // A withdrawn closure edge is zeroed in `derived_base` and
            // published as a negative `ClosureInferred` UNLESS the row is also
            // currently rule-owned (`rule_attr`): that materialization belongs
            // to the rule (Finding-2 dual), so closure only loses its
            // ownership. We also only touch `derived_base` for rows actually
            // materialized there (`get != 0`) — an edge that is also asserted
            // lives in `asserted_base`, not `derived_base`, so there is
            // nothing to zero.
            for triple in withdraw {
                // Closure loses ownership regardless.
                let was_support = self.closure_support.remove(&triple);
                if !was_support {
                    // Closure did not own this as a materialized derived row
                    // (e.g. it was only ever present as an asserted edge).
                    // Nothing to zero or publish here.
                    continue;
                }
                // If a rule still owns the row (positive weight in
                // `rule_attr`), leave the materialization to the rule — but
                // record the ORPHAN: its remaining rule weight may be
                // cyclic-only self-support, so `tick()` must reseed the
                // fixpoint to re-check it unless the row is re-covered by the
                // insertion pass (codex P1).
                if self.rule_attr.contains_key(&triple) {
                    rule_owned_orphans.push(triple);
                    continue;
                }
                merged += self.withdraw_derived_row(triple, DerivationKind::ClosureInferred);
                self.note_presence_change(triple, init_delta);
            }

            // P1 — promote deleted-but-still-entailed asserted edges. The
            // edge lost its asserted copy this tick but remains derivable in
            // the closure; it had no materialized derived row (it lived only
            // in `asserted_base`), so we must PROMOTE it to a `ClosureInferred`
            // derived row. We materialize only when it is genuinely absent
            // from BOTH bases now (the asserted copy is gone and no rule/
            // closure row already owns it) and not already `closure_support`.
            // Promotions ADD `closure_support`, so they run BEFORE the rule
            // fixpoint — rules can then join against the promoted closure edge
            // (consistent with Finding-1).
            for triple in promote {
                if self.closure_support.contains(&triple) {
                    continue;
                }
                // Finding 4: an over-retracted edge with NEGATIVE asserted
                // multiplicity is absent; use the `> 0` presence boundary.
                if self.asserted_base.get(&triple) > 0 {
                    // Still asserted with surviving positive multiplicity:
                    // the base edge did not actually go away, no-op.
                    continue;
                }
                if self.derived_base.get(&triple) > 0 {
                    // Already materialized in derived_base because a rule (or
                    // another closure row) owns it. Finding 3: record closure
                    // ownership WITHOUT adding another multiplicity and WITHOUT
                    // publishing — mirroring the insertion pass's "already
                    // present, record ownership if materialized" logic. This
                    // keeps `closure_support ⊆ derived_base` AND ensures a
                    // later rule retraction does not zero a row the closure
                    // backend still entails (the dual of Finding 2). Skipping
                    // this (the previous no-op) lost the closure's ownership.
                    self.closure_support.insert(triple);
                    continue;
                }
                self.derived_base.add(triple, 1);
                self.closure_support.insert(triple);
                merged += self.emit_derived(triple, 1, DerivationKind::ClosureInferred);
                self.note_presence_change(triple, init_delta);
            }
        }
        self.closure_plans = closure_plans;
        (merged, withdraw_n, promote_n, rule_owned_orphans)
    }

    /// Run the closure INSERTION pass (SPEC-06 F5) over the positive-only
    /// asserted insertion delta: fold each closure plan's newly-inferred triples
    /// into `derived_base` / `closure_support`, publish them as
    /// `ClosureInferred`, and note each materialization's extent change into
    /// `init_delta`. Returns the number of derived rows merged.
    ///
    /// Runs before the rule fixpoint on every tick kind, so closure-derived
    /// edges feed rule bodies in the same tick. Idempotent w.r.t.
    /// already-present rows: a triple already present only (re)records closure
    /// ownership when it is materialized in `derived_base`, never
    /// double-counting multiplicity.
    fn run_closure_insertion_pass(
        &mut self,
        asserted_delta_for_closure: &Zset<TripleId>,
        init_delta: &mut Zset<TripleId>,
    ) -> usize {
        let mut merged = 0;
        // Take the closure_plans out of self to satisfy the borrow checker:
        // iterating over &mut closure_plans conflicts with borrowing
        // self.derived_base / self.feed / self.derived_clock mutably through
        // self at the same time (they are disjoint fields, but the compiler
        // can't see through `self` without NLL field disjointness for &mut).
        let mut closure_plans = std::mem::take(&mut self.closure_plans);
        for rule in &mut closure_plans {
            let inferred = rule.apply_insert_delta(asserted_delta_for_closure);
            for triple in inferred {
                if self.base_presence(&triple) {
                    // Already present. Record closure ownership ONLY if it is
                    // materialized in derived_base (the rule-derived overlap
                    // case — keeps the "retain closure-supported overlap on
                    // rule retraction" fix working, Finding 2). Do NOT record
                    // a triple that is present only because it is asserted:
                    // after the asserted copy is retracted it would become a
                    // ghost input (absent from both bases yet treated as
                    // supported), keeping stale rule consequences alive.
                    // This preserves the invariant `closure_support ⊆ derived_base`.
                    if self.derived_base.get(&triple) != 0 {
                        self.closure_support.insert(triple);
                    }
                    continue;
                }
                // Not present anywhere → the closure pass materializes it now,
                // so recording closure ownership keeps `closure_support ⊆ derived_base`.
                self.derived_base.add(triple, 1);
                self.closure_support.insert(triple);
                merged += self.emit_derived(triple, 1, DerivationKind::ClosureInferred);
                self.note_presence_change(triple, init_delta);
            }
        }
        self.closure_plans = closure_plans;
        merged
    }

    /// Stage-1 retraction regime, kept as the config-gated debug fallback:
    /// closure retract pass, early closure insertion pass, then
    /// `recompute_rule_closure` + diff against `rule_attr`. Afterwards the
    /// incremental state (extent, weights, plan traces) is resynced from
    /// scratch so subsequent unified-path ticks stay correct. Returns
    /// `(derived_merged, rounds_run, withdraw_n, promote_n)`.
    fn run_recompute_fallback(
        &mut self,
        asserted_delta_for_closure: &Zset<TripleId>,
        retract_delta: &Zset<TripleId>,
    ) -> (usize, usize, u64, u64) {
        // Presence-change notes are irrelevant here — the resync below
        // rebuilds the incremental state from scratch. Use a scratch delta.
        // Rule-owned orphans are dropped too: the recompute seeds from
        // `asserted ∪ closure_support` (already shrunk), so a row without
        // well-founded support simply falls out of the diff.
        let mut scratch: Zset<TripleId> = Zset::new();
        let (mut derived_merged, withdraw_n, promote_n, _orphans) =
            self.run_closure_retract_pass(retract_delta, &mut scratch);

        // Closure INSERTION pass BEFORE the rule recompute (Finding 2): the
        // recompute must see the POST-TICK closure so a rule consequence off a
        // replacement closure edge survives.
        derived_merged += self.run_closure_insertion_pass(asserted_delta_for_closure, &mut scratch);

        // Closure-inferred rows (F5) are NOT in `rule_attr`, so the rule
        // diff leaves them untouched. The closure-retraction pass above has
        // already shrunk `closure_support`, and the closure insertion pass
        // has re-grown it with any post-tick replacement edges, so the
        // recompute joins against the correct post-tick closure.
        let (new_rule, rounds_run) = self.recompute_rule_closure();
        let old_rule: BTreeMap<TripleId, RuleId> = std::mem::take(&mut self.rule_attr);

        // Newly derivable rows → add + publish positive RuleInferred.
        // Not `materialize_rule_row`: `rule_attr` was taken above and is
        // bulk-reassigned below, so a per-row insert would be discarded.
        for (triple, rid) in &new_rule {
            if !old_rule.contains_key(triple) {
                self.derived_base.add(*triple, 1);
                derived_merged += self.emit_derived(*triple, 1, DerivationKind::RuleInferred(*rid));
            }
        }
        // No-longer-derivable rows → withdraw to zero + publish a
        // negative RuleInferred attributed to the rule that had derived
        // it. EXCEPT rows still in `closure_support`: the row keeps its
        // closure derivation, so only its rule ownership lapses. `rule_attr`
        // already drops it (it is absent from `new_rule`); we must NOT
        // zero `derived_base` or publish a withdrawal, or we would
        // destroy still-valid closure support (Finding 2). Note the closure
        // retraction pass above has already removed from `closure_support`
        // any closure edge whose base support is gone, so a row that lost
        // BOTH rule and closure support is correctly withdrawn here.
        for (triple, old_rid) in &old_rule {
            if !new_rule.contains_key(triple) {
                if self.closure_support.contains(triple) {
                    continue;
                }
                derived_merged +=
                    self.withdraw_derived_row(*triple, DerivationKind::RuleInferred(*old_rid));
            }
        }
        self.rule_attr = new_rule;

        self.resync_incremental_state();
        (derived_merged, rounds_run, withdraw_n, promote_n)
    }

    /// Rebuild the incremental state after a recompute-fallback tick:
    /// `extent` from base presence, `rule_weights` from each plan's
    /// `apply_full` over that extent (exactly the one-step derivation
    /// weights), and reset every plan's integrated trace so the next
    /// stateful call cold-starts from the rebuilt extent. O(store) —
    /// acceptable for a debug fallback. `rule_attr` stays as the recompute
    /// diff produced it.
    fn resync_incremental_state(&mut self) {
        self.extent = PredExtent::from_zset(&self.materialize_presence());
        self.rule_weights.clear();
        for (plan, rid) in &self.plans {
            let full = plan.apply_full(&self.extent);
            for (t, m) in full.iter() {
                if m != 0 {
                    *self
                        .rule_weights
                        .entry(*t)
                        .or_default()
                        .entry(*rid)
                        .or_insert(0) += m;
                }
            }
        }
        // Prune zero per-rule entries and zero-total rows (defensive: `add`
        // above records raw sums exactly as `apply_full` returns them).
        self.rule_weights.retain(|_, per_rule| {
            per_rule.retain(|_, w| *w != 0);
            !per_rule.is_empty()
        });
        for (plan, _) in &mut self.plans {
            plan.reset_state();
        }
    }

    /// Recompute the set-semantics rule closure of the current
    /// `asserted_base` from scratch, returning the rule that first derived
    /// each *rule-derived* triple.
    ///
    /// Used only by the recompute fallback (`new_with_recompute_fallback`).
    /// Triples that are present in `asserted_base` (or are both asserted and
    /// derivable) are seeded at multiplicity 1 and never get an attribution
    /// entry — this mirrors the unified path, which excludes asserted triples
    /// from `derived_base`. The returned map therefore contains exactly the
    /// rule-derived rows, suitable for diffing against `rule_attr`.
    ///
    /// The seed is `asserted_base ∪ closure_support` (Finding 1): the
    /// unified path runs rules over `asserted ∪ derived`, and
    /// closure-derived rows live in `derived` and persist. Seeding the
    /// recompute with `closure_support` therefore reproduces the unified
    /// path's input extent — rules can join against closure-derived inputs,
    /// and a rule consequence that depends on a closure row is not spuriously
    /// withdrawn when an unrelated (or the closure's own asserted-edge)
    /// retraction lands. Closure-supported triples are seeded at multiplicity
    /// 1 and, like asserted triples, get no attribution entry, so they are
    /// treated as stable base inputs.
    ///
    /// The seed never contains a ghost because `closure_support ⊆ derived_base`
    /// (see the field doc): every seeded `closure_support` row is a live
    /// materialized derived row, never a triple that is merely asserted (and
    /// might have been retracted this tick).
    /// Returns `(attr, rounds_run)` where `rounds_run` is the number of
    /// fixpoint iterations executed (1 means the closure converged in a
    /// single pass).
    fn recompute_rule_closure(&self) -> (BTreeMap<TripleId, RuleId>, usize) {
        // Bound the naïve fixpoint. The retraction path operates on small
        // working sets; a runaway means a non-terminating ruleset, which we
        // want to surface loudly rather than hang.
        const MAX_ROUNDS: usize = 4096;

        let mut closure: Zset<TripleId> = Zset::from_iter(
            self.asserted_base
                .iter()
                .filter(|(_, m)| *m > 0)
                .map(|(t, _)| (*t, 1))
                .chain(self.closure_support.iter().map(|t| (*t, 1))),
        );
        let mut attr: BTreeMap<TripleId, RuleId> = BTreeMap::new();

        let mut rounds = 0;
        loop {
            rounds += 1;
            let mut changed = false;
            // Rebuilt each round: `closure` grows as rows are added below, so
            // a predicate index from a stale round would miss them. This
            // recompute path is already O(store) per retraction tick (a
            // debug fallback, not the production path), so an O(round size)
            // rebuild is not a new cost class.
            let pred_closure = PredExtent::from_zset(&closure);
            for (plan, rid) in &self.plans {
                let dd = plan.apply_full(&pred_closure);
                for (triple, m) in dd.iter() {
                    if m != 0 && closure.get(triple) == 0 {
                        closure.add(*triple, 1);
                        attr.entry(*triple).or_insert(*rid);
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
            assert!(
                rounds < MAX_ROUNDS,
                "rule closure failed to converge within {MAX_ROUNDS} rounds"
            );
        }

        (attr, rounds)
    }

    /// Differential-testing oracle: the Stage-1 from-scratch rule closure over
    /// the current base, returning the rule that first derived each
    /// rule-derived row (the attribution map; the round count is dropped).
    /// O(store). Exposed for integration tests only — the production path
    /// never calls it.
    #[doc(hidden)]
    pub fn oracle_rule_closure(&self) -> BTreeMap<TripleId, RuleId> {
        self.recompute_rule_closure().0
    }

    /// Brute-force check of the incremental-state invariants (differential-test
    /// harness). Panics with a message naming the violated invariant and the
    /// offending triple on failure. O(store) — call after a tick in tests, not
    /// on the production path.
    ///
    /// The invariants (PLAN-24-01 "Circuit state invariants"):
    /// (i) `extent` ≡ base presence, every extent entry at multiplicity 1;
    /// (ii) `rule_weights` equals the exact per-rule one-step derivation counts
    /// over the final extent (recomputed via each plan's stateless
    /// `apply_full`); (iii) every `rule_attr` key has positive total weight, a
    /// positive `derived_base` row, and the attributed rule has positive weight
    /// for it; (iv) `closure_support ⊆ derived_base` keys.
    #[doc(hidden)]
    pub fn debug_validate(&self) {
        // (i) extent ≡ presence. Zset prunes zero entries, so every extent
        // entry is nonzero; the design pins each present row at multiplicity 1.
        for (t, m) in self.extent.iter() {
            assert_eq!(
                m, 1,
                "invariant (i) extent multiplicity: {t:?} has extent multiplicity {m}, expected 1"
            );
            assert!(
                self.base_presence(t),
                "invariant (i) extent ⊆ presence: {t:?} in extent but absent from both bases"
            );
        }
        for (t, m) in self.asserted_base.iter() {
            if m > 0 {
                assert!(
                    self.extent.get(t) > 0,
                    "invariant (i) presence ⊆ extent: asserted {t:?} not in extent"
                );
            }
        }
        for (t, m) in self.derived_base.iter() {
            if m > 0 {
                assert!(
                    self.extent.get(t) > 0,
                    "invariant (i) presence ⊆ extent: derived {t:?} not in extent"
                );
            }
        }

        // (ii) weights exact. Recompute per-rule one-step derivation counts
        // over the final extent via each plan's stateless `apply_full` (`&self`
        // — it never disturbs any plan state) and compare to `rule_weights`
        // exactly. Missing == 0; no zero entries are stored on either side.
        let mut expected: BTreeMap<TripleId, BTreeMap<RuleId, i64>> = BTreeMap::new();
        for (plan, rid) in &self.plans {
            let full = plan.apply_full(&self.extent);
            for (t, m) in full.iter() {
                if m != 0 {
                    *expected.entry(*t).or_default().entry(*rid).or_insert(0) += m;
                }
            }
        }
        expected.retain(|_, per_rule| {
            per_rule.retain(|_, w| *w != 0);
            !per_rule.is_empty()
        });
        assert!(
            expected == self.rule_weights,
            "invariant (ii) weights exact: rule_weights disagrees with recomputed one-step \
             counts.\n  expected: {expected:?}\n  actual:   {:?}",
            self.rule_weights
        );

        // (iii) attribution.
        for (t, rid) in &self.rule_attr {
            assert!(
                self.total_weight(t) > 0,
                "invariant (iii) attribution: rule_attr {t:?} has non-positive total weight"
            );
            assert!(
                self.derived_base.get(t) > 0,
                "invariant (iii) attribution: rule_attr {t:?} not materialized in derived_base"
            );
            let w = self
                .rule_weights
                .get(t)
                .and_then(|per_rule| per_rule.get(rid))
                .copied()
                .unwrap_or(0);
            assert!(
                w > 0,
                "invariant (iii) attribution: rule {rid} attributed to {t:?} but its weight is {w}"
            );
        }

        // (iv) closure_support ⊆ derived_base keys.
        for t in &self.closure_support {
            assert!(
                self.derived_base.get(t) > 0,
                "invariant (iv) closure_support ⊆ derived_base: {t:?} in closure_support but \
                 absent from derived_base"
            );
        }
    }
}
