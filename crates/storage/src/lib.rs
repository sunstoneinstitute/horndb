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
//!   * A persistent dictionary: an immutable memory-mapped base under the
//!     in-memory overlay (`dict_base`, SPEC-25 S2).
//!   * Cold, read-only, memory-mapped predicate partitions behind the same
//!     read surface as the warm ones (`cold`, `partition::Partition`,
//!     `Store::demote` / `Store::promote`, SPEC-25 S5), plus the access-driven
//!     placement policy that moves them (`tiering`, `Store::rebalance`).
//!   * A write-ahead log with checkpoints and crash recovery (`wal`,
//!     `Store::open` / `Store::checkpoint`, SPEC-25 S3). The same log carries
//!     the circuit's input records (`Store::log_input`, SPEC-24 S5,
//!     ADR-0018).
//!
//! Out of scope here: CXL/NVMe tiering, named-graph snapshots, rdfhdt
//! wire-format compatibility, HDT bulk import.

pub mod cold;
pub(crate) mod dict_base;
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
pub mod tiering;
pub mod visibility;
pub(crate) mod wal;

// Re-exports below are added incrementally as each module is implemented.
// See plans/PLAN-02-01-storage.md tasks 2–9.

pub use cold::ColdPartition;
pub use dict_base::BaseStats;
pub use dictionary::{Dictionary, DictionaryBytes};
pub use error::StorageError;
pub use memory_tier::{MemoryTier, PinnedSnapshot, TierSnapshot};
pub use ordering::{Ordering, PartitionAxis};
pub use partition::{
    hot_threshold, set_hot_threshold, OrderedColumns, Partition, PredicatePartition,
    DEFAULT_HOT_THRESHOLD, NEVER_EAGER,
};
pub use snapshot::{export_snapshot, import_snapshot, SnapshotStats};
pub use store::{FootprintReport, Store, StoreSnapshot};
pub use term::{GraphId, InternedQuad, TermId, TermKind, DEFAULT_GRAPH};
pub use tier::{ApplyReport, Tier, TierStats, TierWrite};
pub use tiering::{PlacementHints, RebalanceReport, TieringConfig};
pub use visibility::{visible, CommitVersion, LATEST, UNSET_END};
pub use wal::{InputRecord, RecoveredInputs, SyncPolicy};
