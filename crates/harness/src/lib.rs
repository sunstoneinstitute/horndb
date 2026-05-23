//! reasoner-harness — conformance and benchmarking harness for the
//! `reasoner` project. See `specs/SPEC-01-conformance-benchmarks.md`.
//!
//! The harness is engine-agnostic: implementations of the [`Reasoner`]
//! trait are plugged in at runtime. A built-in [`StubReasoner`] exists
//! to prove the harness itself works before any real engine is wired up
//! (SPEC-01 F12).

pub mod ci;
pub mod db;
pub mod manifest;
pub mod outcome;
pub mod reasoner;
pub mod report;
pub mod runner;
pub mod selected;
pub mod stub;
pub mod testcase;

pub use outcome::{Outcome, Report, Status};
pub use reasoner::Reasoner;
pub use stub::StubReasoner;
