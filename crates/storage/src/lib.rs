//! horndb-storage — Stage 0/1 scope.
//!
//! Provides:
//!   * 64-bit kind-tagged term IDs (`term`).
//!   * Concurrent term↔ID dictionary (`dictionary`).
//!   * Predicate-partitioned, columnar in-memory triple storage (`partition`),
//!     with all six trie orderings queryable per predicate (`ordering`).
//!   * A `Tier` trait with one in-memory implementation (`tier`, `memory_tier`).
//!   * A public `Store` facade (`store`) and N-Triples / Turtle / N-Quads bulk
//!     loaders (`loader::{ntriples, turtle, nquads}`, SPEC-02 F8); N-Quads
//!     routes to the graph named by its fourth term (SPEC-02 F7).
//!   * An HDT-derived compact snapshot export/import (`snapshot`, SPEC-02 F9).
//!
//! Out of Stage-1 scope: MVCC, CXL/NVMe tiering, persistent dictionary,
//! named-graph snapshots, rdfhdt wire-format compatibility, HDT bulk import.

pub mod dictionary;
pub mod error;
pub mod loader;
pub mod memory_tier;
pub mod ordering;
pub mod partition;
pub mod snapshot;
pub mod store;
pub mod term;
pub mod tier;

// Re-exports below are added incrementally as each module is implemented.
// See plans/2026-05-24-SPEC-02-storage.md tasks 2–9.

pub use dictionary::Dictionary;
pub use error::StorageError;
pub use memory_tier::{MemoryTier, TierSnapshot};
pub use ordering::{Ordering, PartitionAxis};
pub use partition::{OrderedColumns, PredicatePartition, DEFAULT_HOT_THRESHOLD};
pub use snapshot::{export_snapshot, import_snapshot, SnapshotStats};
pub use store::{FootprintReport, Store, StoreSnapshot};
pub use term::{GraphId, TermId, TermKind, DEFAULT_GRAPH};
pub use tier::{Tier, TierStats};
