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
        // First, drain pending asserted records into asserted_base and
        // publish them. We need them in the base before running the
        // fixed-point so that subsequent rounds can join against them.
        let asserted_records: Vec<_> = self.log.drain().collect();
        let asserted_merged = asserted_records.len();
        let mut asserted_delta: Zset<TripleId> = Zset::new();
        for rec in &asserted_records {
            asserted_delta.add(rec.triple, rec.mult);
            self.asserted_base.add(rec.triple, rec.mult);
            self.feed.publish_record(*rec);
        }
        let logical_time = asserted_records.last().map(|r| r.time).unwrap_or(0);

        // Fixed-point: keep firing plans until no new derived rows
        // appear. Inputs to a plan are (asserted_base ∪ derived_base)
        // and the running delta (asserted_delta initially, then
        // last-round's derived delta).
        //
        // Bound the loop at MAX_ROUNDS to surface non-termination
        // bugs early in development.
        const MAX_ROUNDS: usize = 64;
        let mut combined_base: Zset<TripleId> = self.asserted_base.clone();
        combined_base.add_assign(&self.derived_base);
        let mut round_delta = asserted_delta;
        let mut derived_merged = 0;

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

        TickReport {
            asserted_merged,
            derived_merged,
            logical_time,
        }
    }
}
