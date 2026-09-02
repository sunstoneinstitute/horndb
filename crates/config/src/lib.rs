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

pub use error::ConfigError;
pub use load::{load, CliOverrides, LoadInputs};
pub use model::{
    DefaultGraph, Limits, Logging, OnInconsistency, QuerySettings, Reasoning, Reload, Server,
    ServerConfig, Simd,
};
pub use units::{ByteSize, HumanDuration};
