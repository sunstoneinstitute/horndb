//! reasoner-storage — Stage 0/1 scope.
//!
//! Provides:
//!   * 64-bit kind-tagged term IDs (`term`).
//!   * Concurrent term↔ID dictionary (`dictionary`).
//!   * Predicate-partitioned, columnar in-memory triple storage (`partition`).
//!   * A `Tier` trait with one in-memory implementation (`tier`, `memory_tier`).
//!   * A public `Store` facade (`store`) and an N-Triples bulk loader (`loader::ntriples`).
//!
//! Out of Stage-1 scope: HDT cold tier, all-six index orderings, MVCC,
//! CXL/NVMe tiering, persistent dictionary.

pub mod dictionary;
pub mod error;
pub mod loader;
pub mod memory_tier;
pub mod partition;
pub mod store;
pub mod term;
pub mod tier;

// Re-exports below are added incrementally as each module is implemented.
// See plans/2026-05-24-SPEC-02-storage.md tasks 2–9.

pub use dictionary::Dictionary;
pub use error::StorageError;
pub use memory_tier::MemoryTier;
pub use partition::PredicatePartition;
pub use store::{FootprintReport, Store};
pub use term::{GraphId, TermId, TermKind, DEFAULT_GRAPH};
pub use tier::{Tier, TierStats};
