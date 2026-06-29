//! `Circuit` — the SPEC-06 stage-1 driver.
//!
//! Owns:
//! - `asserted_base`: `Zset<TripleId>` of asserted triples.
//! - `derived_base`: `Zset<TripleId>` of rule/closure consequences.
//! - `log`: pending asserted records since the last tick.
//! - `plans`: registered operators (each tagged with its `RuleId` for
//!   change-feed `DerivationKind` annotation).
//! - `feed`: change-feed publisher.
//!
//! One `tick()` call:
//! 1. Snapshots the pending log as `Δ_asserted`.
//! 2. Runs every registered plan over (`asserted_base`, `Δ_asserted`)
//!    to compute `Δ_derived` (sum across plans).
//! 3. Drains `log` into `asserted_base` via `Checkpoint::merge`,
//!    publishing every record to the feed (kind = Asserted).
//! 4. Merges `Δ_derived` into `derived_base`, publishing each record
//!    (kind = RuleInferred(rule_id) for the originating plan).
//!
//! `tick()` has two regimes: insertion-only ticks take the forward
//! semi-naïve path described above; any tick containing a retraction
//! (`mult < 0`) instead recomputes the set-semantics rule closure of the
//! post-delta base and diffs it against the prior rule-derived rows (F6,
//! see `tick()`). On retraction ticks the closure plans also run their
//! retraction pass (withdrawing `ClosureInferred` rows whose base support is
//! gone) BEFORE the rule recompute, so the recompute joins only against
//! still-supported closure edges.
//!
//! Stage 1 simplifications:
//! - One round of rule firing per tick. SPEC-04 will wrap this in a
//!   semi-naïve fixed-point loop driven by its dirty-flag machinery.
//! - Derived deltas are not fed back as inputs to other plans within
//!   the same tick. Multi-plan recursion is a Stage 2 concern that
//!   intersects SPEC-04's evaluation order.
//! - Closure deltas (F5): on insertion-only ticks the closure INSERTION pass
//!   runs after the rule fixed-point via `add_closure_plan` / `ClosureRule`.
//!   On retraction (mixed) ticks BOTH the closure retraction pass AND the
//!   closure insertion pass run BEFORE the rule recompute, so the recompute
//!   sees the post-tick closure (a rule consequence off a replacement closure
//!   edge survives — Finding 2). The end-of-tick insertion pass is skipped on
//!   retraction ticks so it never runs twice. Closure→rule cross-feedback
//!   WITHIN a pure insertion tick (a closure edge feeding a rule body in the
//!   same tick it is first derived) remains a Stage-2 concern.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::change_feed::{ChangeFeed, ChangeFeedRx};
use crate::closure_plan::ClosureRule;
use crate::delta_log::DeltaLog;
use crate::operator::NaryPlan;
use crate::snapshot::Snapshot;
use crate::types::{DerivationKind, LogicalTime, Multiplicity, RuleId, TripleId};
use crate::zset::Zset;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TickReport {
    pub asserted_merged: usize,
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
    derived_clock: LogicalTime,
    /// Rule that derived each rule-inferred row. Closure-inferred rows are
    /// deliberately absent (they are owned by the insertion-only closure
    /// plans, see the retraction regime in `tick`). Used by the retraction
    /// recompute to (a) recover the prior rule-derived set to diff and
    /// (b) attribute withdrawal records to the right `RuleId`.
    rule_attr: BTreeMap<TripleId, RuleId>,
    /// Triples the closure pass owns as **materialized derived rows**.
    ///
    /// Invariant: `closure_support ⊆ derived_base`. A triple is recorded here
    /// only when the closure pass actually materializes it in `derived_base`
    /// (it was absent from both bases), OR when a rule already put it in
    /// `derived_base` and the closure plan re-emits it (the Finding-2 overlap
    /// case). A triple that the closure plan emits but is present only because
    /// it is *asserted* is deliberately NOT recorded: after the asserted copy
    /// is retracted it would be absent from both bases yet still seeded into
    /// `recompute_rule_closure` — a ghost input that keeps stale rule
    /// consequences alive.
    ///
    /// The insertion closure pass only ever *adds* here; the closure
    /// **retraction** pass (F6) removes a row when its base support is gone,
    /// zeroing the matching `derived_base` row unless a rule still owns it. The
    /// rule withdrawal diff never zeroes a `closure_support` row in
    /// `derived_base` (it skips them), so the invariant is preserved across
    /// ticks. The retraction path reads this set for two reasons: (1) it seeds
    /// `recompute_rule_closure` so rules can join against closure-derived
    /// inputs exactly as the forward path does (Finding 1), and (2) it lets
    /// the withdrawal diff retain a triple whose rule ownership lapsed but
    /// whose closure support is intact (Finding 2). Only *written* by the
    /// closure pass; only *read* on retraction ticks.
    closure_support: BTreeSet<TripleId>,
    /// SPEC-06 F7 — lazily-built, cached presence view shared with live
    /// [`Snapshot`]s. `None` means "stale": the next `snapshot()` rebuilds it
    /// from the current bases. A state-changing `tick()` invalidates it in O(1)
    /// (no allocation) so steady-state ticks stay delta-sized; the O(n) build is
    /// paid only when a reader actually acquires a snapshot, and is reused by
    /// further acquires until the next tick.
    version_cache: RefCell<Option<Arc<Zset<TripleId>>>>,
    /// Logical time the current `version` represents (SPEC-06 F7, INCLUSIVE):
    /// the last committed asserted-record timestamp. A snapshot reflects every
    /// record with timestamp ≤ this value. Advances only on ticks that merge
    /// asserted records (derived-only ticks leave it unchanged; an empty circuit
    /// stays at 0).
    version_time: LogicalTime,
}

impl Default for Circuit {
    fn default() -> Self {
        Self::new()
    }
}

impl Circuit {
    pub fn new() -> Self {
        Self {
            asserted_base: Zset::new(),
            derived_base: Zset::new(),
            log: DeltaLog::new(),
            plans: Vec::new(),
            closure_plans: Vec::new(),
            feed: ChangeFeed::new(),
            derived_clock: 0,
            rule_attr: BTreeMap::new(),
            closure_support: BTreeSet::new(),
            version_cache: RefCell::new(None),
            version_time: 0,
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

    pub fn asserted_base(&self) -> &Zset<TripleId> {
        &self.asserted_base
    }
    pub fn derived_base(&self) -> &Zset<TripleId> {
        &self.derived_base
    }

    /// Acquire an MVCC [`Snapshot`] (SPEC-06 F7): a refcounted, consistent
    /// `(asserted ∪ derived)` view pinned at the current logical time. The
    /// snapshot survives subsequent `tick()`s until dropped; readers and
    /// writers never block.
    ///
    /// Amortized O(1) when the cache is warm (clones an `Arc` of the already
    /// materialized presence set), O(|asserted| + |derived|) on the first
    /// acquire after a state-changing `tick()` invalidated the cache. The build
    /// is paid lazily here rather than on every tick, so steady-state writes
    /// stay delta-sized.
    pub fn snapshot(&self) -> Snapshot {
        let mut cache = self.version_cache.borrow_mut();
        let view = cache
            .get_or_insert_with(|| Arc::new(self.materialize_presence()))
            .clone();
        Snapshot::new(self.version_time, view)
    }

    /// Build the presence-set view `asserted ∪ derived` (each present triple at
    /// multiplicity 1). Positive multiplicities only: a net-zero/negative triple
    /// is absent. O(|asserted| + |derived|); called lazily from `snapshot()`.
    ///
    /// We take only **positive** multiplicities (`m > 0`) — the same presence
    /// rule `recompute_rule_closure` uses for asserted rows. A net-negative
    /// count (a retraction of a triple that was not asserted, or duplicate
    /// retractions) is *not* present, and a net-zero (asserted then retracted)
    /// triple is absent. Summing the two Z-sets (raw `add_assign`) would instead
    /// expose multiplicity 2+ for a triple that is both asserted and derived, or
    /// asserted twice — a multiset, which an RDF reader surface must not be.
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

    /// Build the per-tick dedup oracle `asserted_base ∪ derived_base` as a fresh
    /// Z-set. Rebuilt rather than cached because `tick()` mutates `derived_base`
    /// across its passes; callers re-acquire it after each mutation phase.
    fn combined_base(&self) -> Zset<TripleId> {
        let mut combined = self.asserted_base.clone();
        combined.add_assign(&self.derived_base);
        combined
    }

    /// Publish one derived delta and advance the derived logical clock.
    ///
    /// Every derived row — rule- or closure-inferred, added or withdrawn —
    /// flows through here so the clock-advance/overflow contract and the feed
    /// publish stay in a single place. Returns 1 so callers can fold it into
    /// their merged-row count.
    fn emit_derived(
        &mut self,
        triple: TripleId,
        mult: Multiplicity,
        kind: DerivationKind,
    ) -> usize {
        let t = self.derived_clock;
        self.derived_clock = self
            .derived_clock
            .checked_add(1)
            .expect("derived-clock overflow");
        self.feed.publish(triple, mult, t, kind);
        1
    }

    /// Append an insertion to the pending log. Kind = Asserted.
    pub fn assert_triple(&mut self, triple: TripleId) {
        self.log.append(triple, 1, DerivationKind::Asserted);
    }

    /// Append a retraction. Stage 1: retraction of a triple with no
    /// derived consequences will produce the right answer; retraction
    /// of a triple whose consequences must also retract is F6 (Stage 2).
    pub fn retract_triple(&mut self, triple: TripleId) {
        self.log.append(triple, -1, DerivationKind::Asserted);
    }

    pub fn tick(&mut self) -> TickReport {
        let t_tick = std::time::Instant::now();
        // First, drain pending asserted records into asserted_base and
        // publish them. We need them in the base before running the
        // fixed-point so that subsequent rounds can join against them.
        let asserted_records: Vec<_> = self.log.drain().collect();
        let asserted_merged = asserted_records.len();
        let mut asserted_delta: Zset<TripleId> = Zset::new();
        // F6: a tick is "retraction-containing" if any drained record
        // withdraws (mult < 0). Insertion-only ticks keep the existing
        // forward semi-naïve path byte-for-byte; retraction-containing
        // ticks recompute the rule closure and diff (see below).
        let mut has_retraction = false;
        for rec in &asserted_records {
            if rec.mult < 0 {
                has_retraction = true;
            }
            asserted_delta.add(rec.triple, rec.mult);
            self.asserted_base.add(rec.triple, rec.mult);
            self.feed.publish_record(*rec);
        }
        let logical_time = asserted_records.last().map(|r| r.time).unwrap_or(0);

        // The closure pass (F5) is insertion-only and must only ever see
        // base edges that are PRESENT post-tick — closure-path retraction
        // stays deferred under parent #6. We materialise this positive
        // insertion delta only when there are closure plans to feed (a
        // no-closure circuit must not pay an O(|Δ|) clone per tick; the empty
        // placeholder is never read because the closure loop has no iterations
        // when `closure_plans` is empty).
        //
        // Finding 1: filter by POST-TICK presence (`asserted_base.get(t) > 0`),
        // not raw `m > 0` from this tick's delta. The asserted records were
        // already merged into `asserted_base` in the drain loop above, so `get`
        // is post-tick here. An edge over-retracted to a NEGATIVE multiplicity
        // and then partially re-asserted so its NET post-tick multiplicity is
        // `<= 0` is ABSENT; a raw `m > 0` filter would still feed it to the
        // closure backend, deriving closure edges off an absent base edge. This
        // is the same present/absent boundary `materialize_presence`,
        // `recompute_rule_closure`, and the retraction gate all use. Normal
        // inserts (an edge whose multiplicity becomes +1) still flow through:
        // their post-tick presence holds.
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
        // positive-only guard above). The closure retraction pass runs BEFORE
        // the rule recompute below so the recompute sees the shrunk
        // `closure_support`.
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
                    // present/absent boundary `materialize_presence` and
                    // `recompute_rule_closure` use (`m > 0` ⇒ present).
                    .filter(|(t, m)| *m < 0 && self.asserted_base.get(t) <= 0)
                    .map(|(t, _)| (*t, -1)),
            )
        };

        let mut combined_base = self.combined_base();
        let mut derived_merged = 0;
        let mut rounds_run = 0usize;
        let mut withdraw_n = 0u64;
        let mut promote_n = 0u64;

        if !has_retraction {
            // ---- Insertion-only regime (unchanged forward path) ----
            //
            // Fixed-point: keep firing plans until no new derived rows
            // appear. Inputs to a plan are (asserted_base ∪ derived_base)
            // and the running delta (asserted_delta initially, then
            // last-round's derived delta).
            //
            // Bound the loop at MAX_ROUNDS to surface non-termination
            // bugs early in development.
            const MAX_ROUNDS: usize = 64;
            let mut round_delta = asserted_delta;

            // Take the plans out of `self` so the per-row `emit_derived` call
            // (which needs `&mut self`) does not conflict with iterating
            // `&self.plans` — the same borrow-checker dance the closure passes
            // use. Restored after the fixed-point loop.
            let plans = std::mem::take(&mut self.plans);
            for round in 0..MAX_ROUNDS {
                rounds_run = round + 1;
                let mut next_delta: Zset<TripleId> = Zset::new();
                for (plan, rid) in &plans {
                    let dd = plan.apply_delta(&combined_base, &round_delta);
                    // Set-semantics filter: emit only the rows that cross
                    // the "present / absent" boundary in combined_base.
                    // Stage 1 is insertion-only, so a row is "newly
                    // derived" iff combined_base.get(triple) == 0 before
                    // this addition. Multi-rule re-derivations in the
                    // same round produce the same key multiple times;
                    // the first emits, the rest are filtered.
                    let mut new_only: Zset<TripleId> = Zset::new();
                    for (triple, _mult) in dd.iter() {
                        if combined_base.get(triple) == 0 && new_only.get(triple) == 0 {
                            new_only.add(*triple, 1);
                        }
                    }
                    for (triple, mult) in new_only.iter() {
                        self.derived_base.add(*triple, mult);
                        combined_base.add(*triple, mult);
                        self.rule_attr.insert(*triple, *rid);
                        derived_merged +=
                            self.emit_derived(*triple, mult, DerivationKind::RuleInferred(*rid));
                        next_delta.add(*triple, mult);
                    }
                }
                if next_delta.is_empty() {
                    break;
                }
                round_delta = next_delta;
            }
            self.plans = plans;
        } else {
            // ---- Retraction-containing regime (DBSP distinct-in-loop) ----
            //
            // Set-semantics rule recursion: a consequence holds iff ≥1
            // derivation holds. Pure derivation-count Z-set accumulation
            // diverges on cyclic recursive rules, so the correct primitive
            // is "recompute the set-semantics rule closure of the post-delta
            // asserted_base, then diff against the prior rule-derived rows".
            // This is order-independent and correct for arbitrary (t, ±k).
            //
            // ---- Closure-path retraction (F6), BEFORE the rule recompute ----
            //
            // Withdraw `ClosureInferred` rows whose base support was retracted
            // this tick, and shrink `closure_support` accordingly, so that
            // `recompute_rule_closure` (which seeds from
            // `asserted_base ∪ closure_support`) sees the already-shrunk support
            // and does not re-derive a rule consequence off a withdrawn closure
            // edge. Take the plans out via `mem::take` to satisfy the borrow
            // checker (same pattern as the insertion closure pass below).
            //
            // A withdrawn closure edge is zeroed in `derived_base` and published
            // as a negative `ClosureInferred` UNLESS the row is also currently
            // rule-owned (`rule_attr`): that materialization belongs to the rule
            // (Finding-2 dual), so closure only loses its ownership. We also only
            // touch `derived_base` for rows actually materialized there
            // (`get != 0`) — an edge that is also asserted lives in
            // `asserted_base`, not `derived_base`, so there is nothing to zero.
            let mut closure_plans = std::mem::take(&mut self.closure_plans);
            for rule in &mut closure_plans {
                let crate::closure_plan::ClosureRetractDelta { withdraw, promote } =
                    rule.apply_retract_delta(&asserted_delta_for_closure_retract);
                withdraw_n += withdraw.len() as u64;
                promote_n += promote.len() as u64;
                for triple in withdraw {
                    // Closure loses ownership regardless.
                    let was_support = self.closure_support.remove(&triple);
                    if !was_support {
                        // Closure did not own this as a materialized derived row
                        // (e.g. it was only ever present as an asserted edge).
                        // Nothing to zero or publish here.
                        continue;
                    }
                    // If a rule still owns the row in derived_base, leave the
                    // materialization to the rule (it will be re-confirmed or
                    // withdrawn by the recompute-and-diff below).
                    if self.rule_attr.contains_key(&triple) {
                        continue;
                    }
                    let cur = self.derived_base.get(&triple);
                    if cur != 0 {
                        self.derived_base.add(triple, -cur);
                    }
                    derived_merged +=
                        self.emit_derived(triple, -1, DerivationKind::ClosureInferred);
                }

                // P1 — promote deleted-but-still-entailed asserted edges. The
                // edge lost its asserted copy this tick but remains derivable in
                // the closure; it had no materialized derived row (it lived only
                // in `asserted_base`), so we must PROMOTE it to a `ClosureInferred`
                // derived row. We materialize only when it is genuinely absent
                // from BOTH bases now (the asserted copy is gone and no rule/
                // closure row already owns it) and not already `closure_support`.
                // Promotions ADD `closure_support`, so they run BEFORE the rule
                // recompute below — rules can then join against the promoted
                // closure edge (consistent with Finding-1).
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
                    derived_merged += self.emit_derived(triple, 1, DerivationKind::ClosureInferred);
                }
            }
            self.closure_plans = closure_plans;

            // Closure INSERTION pass, run on mixed ticks BEFORE the rule
            // recompute (Finding 2). A mixed tick that retracts one support edge
            // and inserts a replacement path must let the rule recompute see the
            // POST-TICK closure: fold the positive closure delta into the backend
            // and into `closure_support`/`derived_base` now, so a re-derived
            // closure edge (e.g. an edge still entailed via the replacement path)
            // is already in the recompute's `asserted_base ∪ closure_support`
            // seed. Without this the recompute would withdraw a rule consequence
            // whose closure support the post-tick base actually entails, and the
            // (formerly end-of-tick) insertion pass would re-add the closure edge
            // only after rules had run. The end-of-tick insertion pass is skipped
            // on retraction ticks so this does not run twice. `combined_base`
            // currently reflects asserted ∪ derived (rebuilt below in the rule
            // diff); rebuild it here first so the closure dedup is correct.
            combined_base = self.combined_base();
            // Finding 2 (change-feed precision, NOT a final-state bug): in a
            // single mixed tick that retracts one support path AND inserts a
            // replacement path for the SAME closure edge, the deletion pass
            // above already published a `ClosureInferred -1` and zeroed the
            // edge; this insertion pass then re-adds it and publishes
            // `ClosureInferred +1`. The FINAL `derived_base` state is correct
            // (the edge is present, supported by the replacement path), but the
            // change feed shows a transient -1 then +1 and `derived_merged`
            // counts both. Reconciling the intra-tick withdraw/re-add to a
            // net-zero feed delta means computing the closure delta against the
            // FINAL post-tick base — a larger change that risks regressing this
            // now-correct mixed-tick state handling. It is a documented Stage-2
            // follow-up (see `FUTURE-WORK.md`, F5 "Still Stage 2").
            derived_merged +=
                self.run_closure_insertion_pass(&asserted_delta_for_closure, &mut combined_base);

            // Closure-inferred rows (F5) are NOT in `rule_attr`, so the rule
            // diff leaves them untouched. The closure-retraction pass above has
            // already shrunk `closure_support`, and the closure insertion pass
            // above has re-grown it with any post-tick replacement edges, so the
            // recompute below joins against the correct post-tick closure.
            let new_rule = self.recompute_rule_closure();
            let old_rule: BTreeMap<TripleId, RuleId> = std::mem::take(&mut self.rule_attr);

            // Newly derivable rows → add + publish positive RuleInferred.
            for (triple, rid) in &new_rule {
                if !old_rule.contains_key(triple) {
                    self.derived_base.add(*triple, 1);
                    derived_merged +=
                        self.emit_derived(*triple, 1, DerivationKind::RuleInferred(*rid));
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
                    let cur = self.derived_base.get(triple);
                    if cur != 0 {
                        self.derived_base.add(*triple, -cur);
                    }
                    derived_merged +=
                        self.emit_derived(*triple, -1, DerivationKind::RuleInferred(*old_rid));
                }
            }
            self.rule_attr = new_rule;

            // Keep `combined_base` consistent for the closure pass below
            // (closure plans dedup against it). Rebuild from the now-current
            // asserted + derived bases.
            combined_base = self.combined_base();
        }

        // Closure INSERTION pass (SPEC-06 F5). On insertion-only ticks this runs
        // here, AFTER the rule forward pass (closure→rule feedback within a pure
        // insertion tick stays Stage-2). On retraction-containing (mixed) ticks
        // it has ALREADY run above, before the rule recompute (see the regime
        // block) — so the recompute joins against the post-tick closure and a
        // rule consequence that depends on a replacement closure edge survives
        // (Finding 2). We must not run it twice, so it is skipped here on
        // retraction ticks.
        if !has_retraction {
            derived_merged +=
                self.run_closure_insertion_pass(&asserted_delta_for_closure, &mut combined_base);
        }

        // SPEC-06 F7: a state-changing tick invalidates the cached snapshot view
        // (O(1), no allocation). The presence set is rebuilt lazily on the next
        // snapshot() acquire — so steady-state ticks stay delta-sized and the O(n)
        // build is only paid when a reader needs it.
        //
        // `version_time` is the **last committed asserted timestamp** (INCLUSIVE):
        // a snapshot reflects every record with timestamp ≤ `version_time`
        // (SPEC-06 F7). `logical_time` is the timestamp of the last asserted
        // record merged this tick — exactly that inclusive `t`. It advances only
        // when asserted records merged: a derived-only tick adds no new asserted
        // records and leaves it unchanged, and an empty circuit stays at 0.
        if asserted_merged > 0 || derived_merged > 0 {
            *self.version_cache.borrow_mut() = None;
            if asserted_merged > 0 {
                self.version_time = logical_time;
            }
        }

        {
            let m = horndb_metrics::metrics();
            m.incremental
                .tick_duration_seconds
                .observe(t_tick.elapsed().as_secs_f64());
            m.incremental.asserted_merged.inc_by(asserted_merged as u64);
            m.incremental.derived_merged.inc_by(derived_merged as u64);
            m.incremental.closure_withdraw.inc_by(withdraw_n);
            m.incremental.closure_promote.inc_by(promote_n);
            m.incremental.fixpoint_rounds.observe(rounds_run as f64);
        }
        TickReport {
            asserted_merged,
            derived_merged,
            logical_time,
        }
    }

    /// Run the closure INSERTION pass (SPEC-06 F5) over the positive-only
    /// asserted insertion delta: fold each closure plan's newly-inferred triples
    /// into `derived_base` / `closure_support`, publish them as
    /// `ClosureInferred`, and keep `combined_base` in sync. Returns the number of
    /// derived rows merged (for `derived_merged`).
    ///
    /// Shared by both tick regimes: on insertion-only ticks it runs at end of
    /// tick (after the rule forward pass); on retraction (mixed) ticks it runs
    /// before the rule recompute so the recompute sees the post-tick closure
    /// (Finding 2). It is therefore idempotent w.r.t. already-present rows: a
    /// triple already in `combined_base` only (re)records closure ownership when
    /// it is materialized in `derived_base`, never double-counting multiplicity.
    fn run_closure_insertion_pass(
        &mut self,
        asserted_delta_for_closure: &Zset<TripleId>,
        combined_base: &mut Zset<TripleId>,
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
                if combined_base.get(&triple) != 0 {
                    // Already present. Record closure ownership ONLY if it is
                    // materialized in derived_base (the rule-derived overlap
                    // case — keeps the "retain closure-supported overlap on
                    // rule retraction" fix working, Finding 2). Do NOT record
                    // a triple that is present only because it is asserted:
                    // after the asserted copy is retracted it would become a
                    // ghost input to recompute_rule_closure (absent from both
                    // bases yet seeded), keeping stale rule consequences alive.
                    // This preserves the invariant `closure_support ⊆ derived_base`.
                    if self.derived_base.get(&triple) != 0 {
                        self.closure_support.insert(triple);
                    }
                    continue;
                }
                // Not present anywhere → the closure pass materializes it now,
                // so recording closure ownership keeps `closure_support ⊆ derived_base`.
                self.derived_base.add(triple, 1);
                combined_base.add(triple, 1);
                self.closure_support.insert(triple);
                merged += self.emit_derived(triple, 1, DerivationKind::ClosureInferred);
            }
        }
        self.closure_plans = closure_plans;
        merged
    }

    /// Recompute the set-semantics rule closure of the current
    /// `asserted_base` from scratch, returning the rule that first derived
    /// each *rule-derived* triple.
    ///
    /// Used only on retraction-containing ticks. Triples that are present
    /// in `asserted_base` (or are both asserted and derivable) are seeded
    /// at multiplicity 1 and never get an attribution entry — this mirrors
    /// the forward path, which excludes asserted triples from
    /// `derived_base`. The returned map therefore contains exactly the
    /// rule-derived rows, suitable for diffing against `rule_attr`.
    ///
    /// The seed is `asserted_base ∪ closure_support` (Finding 1): the
    /// forward path runs rules over `asserted ∪ derived`, and
    /// closure-derived rows live in `derived` and persist (closure-path
    /// retraction is deferred). Seeding the recompute with `closure_support`
    /// therefore reproduces the forward path's input extent — rules can join
    /// against closure-derived inputs, and a rule consequence that depends
    /// on a closure row is not spuriously withdrawn when an unrelated (or
    /// the closure's own asserted-edge) retraction lands. Closure-supported
    /// triples are seeded at multiplicity 1 and, like asserted triples, get
    /// no attribution entry, so they are treated as stable base inputs.
    ///
    /// The seed never contains a ghost because `closure_support ⊆ derived_base`
    /// (see the field doc): every seeded `closure_support` row is a live
    /// materialized derived row, never a triple that is merely asserted (and
    /// might have been retracted this tick).
    fn recompute_rule_closure(&self) -> BTreeMap<TripleId, RuleId> {
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
            let mut changed = false;
            for (plan, rid) in &self.plans {
                let dd = plan.apply_full(&closure);
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
            rounds += 1;
            assert!(
                rounds < MAX_ROUNDS,
                "rule closure failed to converge within {MAX_ROUNDS} rounds"
            );
        }

        attr
    }
}
