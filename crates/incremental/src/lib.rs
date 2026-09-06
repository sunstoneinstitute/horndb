//! `horndb-incremental` — DBSP-style incremental maintenance for SPEC-06.
//!
//! # Why a hand-rolled Z-set core?
//!
//! SPEC-06 explicitly allows either adopting `differential-dataflow` or
//! reimplementing the narrow Z-set subset we need. We chose the latter for
//! Stage 1 because:
//!
//! 1. The Stage-1 surface (linear + bilinear operators, checkpoint-boundary
//!    snapshots, insertion plus rule-path retraction-by-recompute-and-diff)
//!    is ~few hundred LOC and we want to read it end-to-end when debugging
//!    the differential test (acceptance #4).
//! 2. `differential-dataflow` pulls `timely` plus ~30 transitive crates that
//!    target distributed scheduling we defer to SPEC-09 (Stage 3).
//! 3. The `BilinearRule` trait is the only contract SPEC-04 codegen depends
//!    on; we can swap the implementation behind it in Stage 2 if needed.
//!
//! F6 (rule-path retraction across joins) now works via recompute-and-diff
//! on retraction-containing ticks (see [`circuit`]); closure-path retraction
//! and a fully delta-incremental retraction path remain future. Re-evaluate
//! this decision if either — or F5 (closure deltas) — forces us to duplicate
//! `differential-dataflow`'s arrangement sharing logic. See FUTURE-WORK.md.
//!
//! # Module layout
//!
//! - [`zset`]: `Zset<K>` and algebraic operations.
//! - [`types`]: triple-id, multiplicity, logical-time, derivation-kind.
//! - [`extent`]: `PredExtent` — the base extent indexed by predicate, so
//!   `NaryPlan` leaves can bind to a single predicate's rows (SPEC-24 S7).
//! - [`operator`]: `LinearRule`, `BilinearRule` traits; n-ary tree planner.
//! - [`kernels`]: `HashJoinRule` — the generic hash-join `BilinearRule`
//!   runtime (SPEC-24 S7 leaf 2), the one join a rule author or codegen
//!   instantiates instead of hand-writing a nested loop.
//! - [`delta_log`]: pending `(triple, ±1)` log, durable behind the storage
//!   write-ahead log when a store is attached (SPEC-24 S5, ADR-0018).
//! - [`checkpoint`]: merge a delta log into the base store; the F8 cadence.
//! - [`change_feed`]: ordered MPMC stream of committed deltas (F9), with
//!   bounded subscribers and a per-subscriber lag policy (SPEC-24 S3).
//! - [`circuit`]: top-level `Circuit` builder + tick driver.
//! - [`snapshot`]: reader views pinned on storage's per-tuple MVCC, as of a
//!   storage commit version (F7, SPEC-24 S6).

pub mod change_feed;
pub mod checkpoint;
pub mod circuit;
pub mod closure_plan;
pub mod delta_log;
pub mod extent;
pub mod kernels;
pub mod operator;
pub mod snapshot;
pub mod types;
pub mod zset;

pub use change_feed::{ChangeFeed, ChangeFeedRx, LagPolicy};
pub use checkpoint::{Checkpoint, CheckpointPolicy, CheckpointReport};
pub use circuit::{Circuit, TickReport};
pub use closure_plan::{ClosureRetractDelta, ClosureRule, TransitiveClosureRule};
pub use delta_log::DeltaLog;
pub use extent::PredExtent;
pub use kernels::{HashJoinRule, KernelError};
pub use operator::{BilinearRule, LinearRule, NaryPlan};
pub use snapshot::Snapshot;
pub use types::{DeltaRecord, DerivationKind, LogicalTime, Multiplicity, RuleId, TripleId};
pub use zset::Zset;
