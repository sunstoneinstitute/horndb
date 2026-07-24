//! Layered loading: path resolution, figment providers, and `load`.

use std::path::PathBuf;

#[allow(unused_imports)]
use figment::{
    providers::{Env, Format, Serialized, Toml},
    Figment,
};

#[allow(unused_imports)]
use crate::error::ConfigError;
#[allow(unused_imports)]
use crate::model::ServerConfig;

#[allow(dead_code)]
const DEFAULT_CONFIG_PATH: &str = "/etc/horndb/config.toml";

/// Inputs that determine *which* base file to read and the caller-supplied
/// command-line value overrides. Kept separate from process globals so the whole
/// loader is unit-testable without touching argv or a real `/etc`.
#[derive(Debug, Default, Clone)]
pub struct LoadInputs {
    /// `--config <path>` (highest precedence for the file location).
    pub cli_config_path: Option<PathBuf>,
    /// `HORNDB_CONFIG` value (middle precedence for the file location).
    pub env_config_path: Option<PathBuf>,
    /// Command-line value overrides (highest precedence for config *values*).
    pub cli_overrides: CliOverrides,
}

/// Command-line value overrides — the top config-value layer (CLI wins).
/// Only `Some` fields override; `None` leaves the lower layers intact. Applied
/// as a typed overlay (not a figment provider) so "override only when `Some`" is
/// exact — a `Serialized` layer would re-introduce defaults and clobber the file.
#[derive(Debug, Default, Clone)]
pub struct CliOverrides {
    pub bind: Option<String>,
    pub simd_max_isa: Option<String>,
    pub simd_autotune: Option<bool>,
}

/// Resolve the base config file path and whether it was explicitly requested.
/// Precedence: `cli_config_path` > `env_config_path` > the default path.
#[allow(dead_code)]
fn resolve_base_path(inputs: &LoadInputs) -> (PathBuf, bool) {
    if let Some(p) = &inputs.cli_config_path {
        return (p.clone(), true);
    }
    if let Some(p) = &inputs.env_config_path {
        return (p.clone(), true);
    }
    (PathBuf::from(DEFAULT_CONFIG_PATH), false)
}

#[cfg(test)]
mod resolve_tests {
    use super::*;

    #[test]
    fn cli_beats_env_beats_default() {
        let inputs = LoadInputs {
            cli_config_path: Some("/cli.toml".into()),
            env_config_path: Some("/env.toml".into()),
            ..Default::default()
        };
        assert_eq!(
            resolve_base_path(&inputs),
            (PathBuf::from("/cli.toml"), true)
        );

        let inputs = LoadInputs {
            env_config_path: Some("/env.toml".into()),
            ..Default::default()
        };
        assert_eq!(
            resolve_base_path(&inputs),
            (PathBuf::from("/env.toml"), true)
        );

        let inputs = LoadInputs::default();
        assert_eq!(
            resolve_base_path(&inputs),
            (PathBuf::from(DEFAULT_CONFIG_PATH), false)
        );
    }
}
