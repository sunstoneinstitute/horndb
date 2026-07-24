//! `horndb-config` (SPEC-26) — the operator configuration system.
//!
//! Loads one typed [`ServerConfig`] by layering, lowest precedence to highest:
//! built-in defaults, the base `config.toml`, `config.d/*.toml` drop-ins
//! (lexical order), environment variables, and caller command-line overrides.
//! See `docs/specs/SPEC-26-config-system.md`.

mod error;
mod load;
mod model;
mod units;

// NOTE: pub-use lines below are added incrementally, task by task, as each
// symbol lands (Tasks 2-7 of PLAN-26-01) — a lib.rs that re-exports a symbol
// before it exists would fail to compile the whole crate for every
// intermediate TDD checkpoint. The final set (after Task 7) is exactly:
//   pub use error::ConfigError;
//   pub use load::{load, CliOverrides, LoadInputs};
//   pub use model::{Limits, Logging, QuerySettings, Reload, Server, ServerConfig, Simd};
//   pub use units::{ByteSize, HumanDuration};
pub use model::{Limits, Logging, QuerySettings, Reload, Server, ServerConfig, Simd};
pub use units::{ByteSize, HumanDuration};
