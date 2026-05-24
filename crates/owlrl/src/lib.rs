//! reasoner-owlrl — OWL 2 RL/RDF rule engine (Stage 1).
//!
//! See `specs/SPEC-04-rule-engine.md` for the design contract and
//! `plans/2026-05-24-SPEC-04-owl-rl-rule-engine.md` for the implementation plan.

pub mod backend;
pub mod delta;
pub mod engine;
pub mod provenance;
pub mod store;
pub mod types;
pub mod vocab;

pub mod generated {
    include!(concat!(env!("OUT_DIR"), "/generated_rules.rs"));
}

pub use engine::{materialize, reset_and_materialize, Stats};
