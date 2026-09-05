//! `horndb-config` (SPEC-26) — the operator configuration system.
//!
//! Loads one typed [`ServerConfig`] by layering, lowest precedence to highest:
//! built-in defaults, the base `config.toml`, `config.d/*.toml` drop-ins
//! (lexical order), environment variables, and caller command-line overrides.
//! See `docs/specs/SPEC-26-config-system.md`.
//!
//! [`ConfigHandle`] holds the config the server is running on, and [`watch`]
//! republishes into it when a watched file changes (SPEC-26 S3).

mod error;
mod load;
mod model;
mod units;
mod watch;

pub use error::ConfigError;
pub use load::{load, CliOverrides, LoadInputs};
pub use model::{
    DefaultGraph, Limits, Logging, OnInconsistency, QuerySettings, Reasoning, ReasoningBackend,
    Reload, Server, ServerConfig, Simd, ViewOutput, ViewSelect, ViewSelectKeyword, Views,
    OVERRIDABLE_KEYS,
};
pub use units::{ByteSize, HumanDuration};
pub use watch::{restart_only_changes, watch, ConfigHandle, ConfigWatcher};
