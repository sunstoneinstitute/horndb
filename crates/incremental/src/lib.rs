//! `horndb-incremental` — DBSP-style incremental maintenance for SPEC-06.
//!
//! # Why a hand-rolled Z-set core?
//!
//! SPEC-06 explicitly allows either adopting `differential-dataflow` or
//! reimplementing the narrow Z-set subset we need. We chose the latter for
//! Stage 1 because:
//!
//! 1. The Stage-1 surface (linear + bilinear operators, checkpoint-boundary
//!    snapshots, insertion only) is ~few hundred LOC and we want to read it
//!    end-to-end when debugging the differential test (acceptance #4).
//! 2. `differential-dataflow` pulls `timely` plus ~30 transitive crates that
//!    target distributed scheduling we defer to SPEC-09 (Stage 3).
//! 3. The `BilinearRule` trait is the only contract SPEC-04 codegen depends
//!    on; we can swap the implementation behind it in Stage 2 if needed.
//!
//! Re-evaluate this decision if F5 (closure deltas) or F6 (retraction across
//! joins) forces us to duplicate `differential-dataflow`'s arrangement
//! sharing logic. See FUTURE-WORK.md.
//!
//! # Module layout
//!
//! - [`zset`]: `Zset<K>` and algebraic operations.
//! - [`types`]: triple-id, multiplicity, logical-time, derivation-kind.
//! - [`operator`]: `LinearRule`, `BilinearRule` traits; n-ary tree planner.
//! - [`delta_log`]: pending `(triple, ±1)` log between checkpoints.
//! - [`checkpoint`]: merge a delta log into the base store.
//! - [`change_feed`]: ordered MPMC stream of committed deltas (F9).
//! - [`circuit`]: top-level `Circuit` builder + tick driver.

pub mod change_feed;
pub mod checkpoint;
pub mod circuit;
pub mod closure_plan;
pub mod delta_log;
pub mod operator;
pub mod types;
pub mod zset;

pub use change_feed::{ChangeFeed, ChangeFeedRx};
pub use checkpoint::{Checkpoint, CheckpointReport};
pub use circuit::{Circuit, TickReport};
pub use closure_plan::{ClosureRule, TransitiveClosureRule};
pub use delta_log::DeltaLog;
pub use operator::{BilinearRule, LinearRule, NaryPlan};
pub use types::{DeltaRecord, DerivationKind, LogicalTime, Multiplicity, RuleId, TripleId};
pub use zset::Zset;
