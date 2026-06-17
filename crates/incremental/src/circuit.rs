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
//! Stage 1 simplifications:
//! - One round of rule firing per tick. SPEC-04 will wrap this in a
//!   semi-naïve fixed-point loop driven by its dirty-flag machinery.
//! - Derived deltas are not fed back as inputs to other plans within
//!   the same tick. Multi-plan recursion is a Stage 2 concern that
//!   intersects SPEC-04's evaluation order.
//! - Closure deltas (F5) run after the rule fixed-point via
//!   `add_closure_plan` / `ClosureRule` (insertion-only). Closure↔rule
//!   cross-feedback within one tick remains a Stage-2 concern.

use std::collections::BTreeMap;

use crate::change_feed::{ChangeFeed, ChangeFeedRx};
use crate::closure_plan::ClosureRule;
use crate::delta_log::DeltaLog;
use crate::operator::NaryPlan;
use crate::types::{DerivationKind, LogicalTime, RuleId, TripleId};
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
        // the positive part of the asserted delta — closure-path
        // retraction stays deferred under parent #6. We materialise the
        // positive-only delta only when there are closure plans to feed
        // (a no-closure circuit must not pay an O(|Δ|) clone per tick; the
        // empty placeholder is never read because the closure loop has no
        // iterations when `closure_plans` is empty).
        let asserted_delta_for_closure = if self.closure_plans.is_empty() {
            Zset::new()
        } else {
            Zset::from_iter(
                asserted_delta
                    .iter()
                    .filter(|(_, m)| *m > 0)
                    .map(|(t, m)| (*t, m)),
            )
        };

        let mut combined_base: Zset<TripleId> = self.asserted_base.clone();
        combined_base.add_assign(&self.derived_base);
        let mut derived_merged = 0;

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

            for _ in 0..MAX_ROUNDS {
                let mut next_delta: Zset<TripleId> = Zset::new();
                for (plan, rid) in &self.plans {
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
                        let t = self.derived_clock;
                        self.derived_clock = self
                            .derived_clock
                            .checked_add(1)
                            .expect("derived-clock overflow");
                        self.feed
                            .publish(*triple, mult, t, DerivationKind::RuleInferred(*rid));
                        derived_merged += 1;
                        next_delta.add(*triple, mult);
                    }
                }
                if next_delta.is_empty() {
                    break;
                }
                round_delta = next_delta;
            }
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
            // Closure-inferred rows (F5) are NOT in `rule_attr`, so the rule
            // diff leaves them untouched; closure-path retraction is
            // explicitly deferred under parent #6.
            let new_rule = self.recompute_rule_closure();
            let old_rule: BTreeMap<TripleId, RuleId> = std::mem::take(&mut self.rule_attr);

            // Newly derivable rows → add + publish positive RuleInferred.
            for (triple, rid) in &new_rule {
                if !old_rule.contains_key(triple) {
                    self.derived_base.add(*triple, 1);
                    let t = self.derived_clock;
                    self.derived_clock = self
                        .derived_clock
                        .checked_add(1)
                        .expect("derived-clock overflow");
                    self.feed
                        .publish(*triple, 1, t, DerivationKind::RuleInferred(*rid));
                    derived_merged += 1;
                }
            }
            // No-longer-derivable rows → withdraw to zero + publish a
            // negative RuleInferred attributed to the rule that had derived
            // it.
            for (triple, old_rid) in &old_rule {
                if !new_rule.contains_key(triple) {
                    let cur = self.derived_base.get(triple);
                    if cur != 0 {
                        self.derived_base.add(*triple, -cur);
                    }
                    let t = self.derived_clock;
                    self.derived_clock = self
                        .derived_clock
                        .checked_add(1)
                        .expect("derived-clock overflow");
                    self.feed
                        .publish(*triple, -1, t, DerivationKind::RuleInferred(*old_rid));
                    derived_merged += 1;
                }
            }
            self.rule_attr = new_rule;

            // Keep `combined_base` consistent for the closure pass below
            // (closure plans dedup against it). Rebuild from the now-current
            // asserted + derived bases.
            combined_base = self.asserted_base.clone();
            combined_base.add_assign(&self.derived_base);
        }

        // Closure pass (SPEC-06 F5): run each closure operator over the
        // *positive-only* asserted insertion delta (`asserted_delta_for_closure`).
        // Newly inferred triples not already present in the combined base are
        // merged into derived_base and published as ClosureInferred.
        // Insertion-only; closure↔rule cross-feedback within a tick is a Stage-2
        // concern (see FUTURE-WORK.md). Closure-PATH retraction (withdrawing a
        // closure-inferred row when its support is retracted) stays deferred
        // under parent #6: `ClosureRule` is stateful and insertion-only, so we
        // never hand it the negative part of the delta. Rule-path retraction
        // above never disturbs closure-inferred rows — they are absent from
        // `rule_attr`, so the rule diff leaves them alone.
        //
        // We take the closure_plans out of self to satisfy the borrow checker:
        // iterating over &mut closure_plans conflicts with borrowing
        // self.derived_base / self.feed / self.derived_clock mutably through
        // self at the same time (they are disjoint fields, but the compiler
        // can't see through `self` without NLL field disjointness for &mut).
        let mut closure_plans = std::mem::take(&mut self.closure_plans);
        for rule in &mut closure_plans {
            let inferred = rule.apply_insert_delta(&asserted_delta_for_closure);
            for triple in inferred {
                if combined_base.get(&triple) != 0 {
                    continue;
                }
                self.derived_base.add(triple, 1);
                combined_base.add(triple, 1);
                let t = self.derived_clock;
                self.derived_clock = self
                    .derived_clock
                    .checked_add(1)
                    .expect("derived-clock overflow");
                self.feed
                    .publish(triple, 1, t, DerivationKind::ClosureInferred);
                derived_merged += 1;
            }
        }
        self.closure_plans = closure_plans;

        TickReport {
            asserted_merged,
            derived_merged,
            logical_time,
        }
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
    fn recompute_rule_closure(&self) -> BTreeMap<TripleId, RuleId> {
        // Bound the naïve fixpoint. The retraction path operates on small
        // working sets; a runaway means a non-terminating ruleset, which we
        // want to surface loudly rather than hang.
        const MAX_ROUNDS: usize = 4096;

        let mut closure: Zset<TripleId> = Zset::from_iter(
            self.asserted_base
                .iter()
                .filter(|(_, m)| *m > 0)
                .map(|(t, _)| (*t, 1)),
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
