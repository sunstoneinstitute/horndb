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
//! - Closure deltas (F5) are not invoked here; SPEC-05 stage 2 wires
//!   in via a `add_closure_plan` extension.

use crate::change_feed::{ChangeFeed, ChangeFeedRx};
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
    feed: ChangeFeed,
    derived_clock: LogicalTime,
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
            feed: ChangeFeed::new(),
            derived_clock: 0,
        }
    }

    pub fn add_plan(&mut self, plan: NaryPlan, attribution: RuleId) {
        self.plans.push((plan, attribution));
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
        // 1. Snapshot pending Δ_asserted from the log into a Zset.
        let mut asserted_delta: Zset<TripleId> = Zset::new();
        for rec in self.log.iter() {
            asserted_delta.add(rec.triple, rec.mult);
        }

        // 2. Run every plan; collect per-plan derived deltas.
        //    We keep them separate so the change feed can attribute
        //    each derived record to its originating rule.
        let mut derived_per_plan: Vec<(Zset<TripleId>, RuleId)> =
            Vec::with_capacity(self.plans.len());
        for (plan, rid) in &self.plans {
            let dd = plan.apply_delta(&self.asserted_base, &asserted_delta);
            derived_per_plan.push((dd, *rid));
        }

        // 3. Drain the asserted log into asserted_base, publishing each
        //    record to the feed. Checkpoint::merge handles zero-pruning.
        let asserted_records: Vec<_> = self.log.drain().collect();
        let asserted_merged = asserted_records.len();
        for rec in &asserted_records {
            self.asserted_base.add(rec.triple, rec.mult);
            self.feed.publish_record(*rec);
        }
        let logical_time = if asserted_records.is_empty() {
            0
        } else {
            asserted_records.last().unwrap().time
        };

        // 4. Merge derived deltas into derived_base, publishing.
        //    Use derived_clock so derived records get monotonically
        //    increasing timestamps distinct from the asserted log's.
        let mut derived_merged = 0;
        for (dd, rid) in &derived_per_plan {
            for (triple, mult) in dd.iter() {
                self.derived_base.add(*triple, mult);
                let t = self.derived_clock;
                self.derived_clock = self
                    .derived_clock
                    .checked_add(1)
                    .expect("derived-clock overflow");
                self.feed
                    .publish(*triple, mult, t, DerivationKind::RuleInferred(*rid));
                derived_merged += 1;
            }
        }

        TickReport {
            asserted_merged,
            derived_merged,
            logical_time,
        }
    }
}
