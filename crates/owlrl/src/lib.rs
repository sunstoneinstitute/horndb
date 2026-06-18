//! horndb-owlrl — OWL 2 RL/RDF rule engine, Stage-1 slice.
//!
//! # Scope
//!
//! - Ahead-of-time codegen from `rules.toml` via `build.rs` (one Rust function
//!   per rule).
//! - Stage-1 rule subset: all of Table 9 `scm-*` plus the most-used `cax-*`,
//!   `prp-*`, `cls-*` rules (target: ≥50 W3C OWL 2 RL test cases passing per
//!   SPEC-00 Stage 1).
//! - Semi-naïve evaluation driver with dirty-predicate filtering.
//! - Full re-materialization (`reset_and_materialize`).
//! - `ClosureBackend` trait for `eq-*` / `prp-trp` / `scm-sco` / `scm-spo`
//!   delegation to SPEC-05 (`horndb-closure`). A reference
//!   `RuleFiringBackend` is provided for tests.
//!
//! # Future Work (NOT in this crate yet)
//!
//! - Full ~80-rule OWL 2 RL set (remaining `cls-int*`/`cls-uni` list-walking
//!   rules and all `dt-*` datatype rules) — Stage 2.
//! - Proof recording (SPEC-04 F4) is implemented: per-triple `Provenance`
//!   composes into a recursive proof tree via `MemStore::proof_tree` /
//!   `Engine::proof` (see `tests/proof_tree.rs`). Still deferred to Stage 2 is
//!   *persistence*: a compressed side-table with on-demand rederivation via
//!   SPEC-03. Today's `Provenance` is in-memory only.
//! - `rdf:type` skew optimization (partition-by-class-id parallelism) is
//!   implemented for the hand-written list rules (`cls-int1` / `cls-uni` /
//!   `cax-adc` / `prp-key` parallelise per-subject filtering across rayon — see
//!   `list_rules.rs` and `ParallelStrategy`). The compiled `rules.toml` rules
//!   (`cax-sco`-style) are not yet parallelised — Stage 2.
//!   (`eq-rep-p`'s own class blow-up is mitigated — see `eq_rep_p_opt`.)
//! - Incremental updates via Z-sets — SPEC-06 / Stage 2.
//! - WCOJ join execution inside rule bodies — SPEC-03 / Stage 2.
//!
//! # Adding a rule
//!
//! 1. Append a `[[rule]]` block to `rules.toml`.
//! 2. `cargo build -p horndb-owlrl` regenerates `generated_rules.rs`.
//! 3. Add a unit test in `tests/single_rule.rs`.
//!
//! See `plans/2026-05-24-SPEC-04-owl-rl-rule-engine.md` for the full plan.

pub mod backend;
pub mod datatypes;
pub mod delta;
pub mod engine;
pub mod eq_rep_p_opt;
#[cfg(feature = "graphblas-backend")]
pub mod graphblas_backend;
pub mod integration;
pub mod list_rules;
pub mod provenance;
pub mod store;
pub mod types;
pub mod vocab;

pub mod generated {
    include!(concat!(env!("OUT_DIR"), "/generated_rules.rs"));
}

/// The full text of the build-time-generated `generated_rules.rs` as a
/// string. Used by the `show-rule` dev binary so contributors can inspect
/// the compiled output of any rule without spelunking under `target/`.
pub const COMPILED_RULES_SOURCE: &str =
    include_str!(concat!(env!("OUT_DIR"), "/generated_rules.rs"));

pub use engine::{
    materialize, materialize_with, reset_and_materialize, EqRepPStrategy, MaterializeOpts,
    ParallelStrategy, PhaseTimings, Stats,
};
pub use integration::{BackendChoice, Engine, StringProofTree};
pub use provenance::ProofTree;
