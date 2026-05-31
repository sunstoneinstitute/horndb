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
//! - Production proof recording (compressed side-table, on-demand rederivation
//!   via SPEC-03) — Stage 2; today's `Provenance` is in-memory only.
//! - `rdf:type` skew optimization (partition-by-class-id parallelism) — Stage 2.
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
pub mod delta;
pub mod engine;
pub mod eq_rep_p_opt;
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

pub use engine::{materialize, reset_and_materialize, Stats};
pub use integration::Engine;
