//! Layered loading: path resolution, figment providers, and `load`.

use std::fs;
use std::path::PathBuf;

use figment::{
    providers::{Env, Format, Serialized, Toml},
    Figment,
};

use crate::error::ConfigError;
use crate::model::ServerConfig;

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

/// Load the effective [`ServerConfig`] by layering, lowest→highest precedence:
/// built-in defaults, the base `config.toml`, `config.d/*.toml` (lexical), env
/// vars (`HORNDB_` prefix, `__` nesting), and the caller's command-line overrides.
///
/// `config_dirs` is resolved from everything *except* the `config.d` fragments
/// (a value set inside a fragment does not relocate the directories).
pub fn load(inputs: &LoadInputs) -> Result<ServerConfig, ConfigError> {
    let (base_path, explicit) = resolve_base_path(inputs);
    if explicit && !base_path.exists() {
        return Err(ConfigError::MissingExplicitFile(base_path));
    }
    let source_desc = base_path.display().to_string();

    // Pass 1: resolve config_dirs from defaults + base file + env (no fragments;
    // config_dirs is not a CLI override, so CLI is irrelevant here).
    let cfg1: ServerConfig = Figment::from(Serialized::defaults(ServerConfig::default()))
        .merge(Toml::file(&base_path))
        .merge(env_provider())
        .extract()
        .map_err(|e| ConfigError::Invalid {
            source_desc: source_desc.clone(),
            message: e.to_string(),
        })?;

    // Pass 2: defaults < base file < config.d/*.toml (pooled across all
    // directories, filename order) < env.
    let mut fig =
        Figment::from(Serialized::defaults(ServerConfig::default())).merge(Toml::file(&base_path));
    for path in config_d_files(&cfg1.server.config_dirs)? {
        fig = fig.merge(Toml::file(path));
    }
    let cfg = fig
        .merge(env_provider())
        .extract::<ServerConfig>()
        .map_err(|e| ConfigError::Invalid {
            source_desc,
            message: e.to_string(),
        })?;

    // CLI overrides are the top layer, applied as a typed overlay because figment
    // cannot express "only override when `Some`".
    Ok(apply_cli_overrides(cfg, &inputs.cli_overrides))
}

/// The environment layer: `HORNDB_`-prefixed vars, restricted to the nested form
/// (`HORNDB_SERVER__BIND` → `server.bind`). Flat vars are dropped, so
/// `HORNDB_CONFIG` (the file-location var) and the legacy single-underscore SIMD
/// aliases (`HORNDB_SIMD_MAX_ISA` / `HORNDB_SIMD_AUTOTUNE`, consumed explicitly by
/// serve in PLAN-26-02) never leak in and never trip `deny_unknown_fields`.
fn env_provider() -> Env {
    Env::prefixed("HORNDB_")
        .filter(|key| key.as_str().contains("__"))
        .split("__")
}

/// Overlay the `Some` command-line overrides onto a `ServerConfig`. This maps the
/// flat `CliOverrides` fields onto the nested model, keeping CLI as the top layer.
fn apply_cli_overrides(mut cfg: ServerConfig, o: &CliOverrides) -> ServerConfig {
    if let Some(b) = &o.bind {
        cfg.server.bind = b.clone();
    }
    if let Some(m) = &o.simd_max_isa {
        cfg.simd.max_isa = Some(m.clone());
    }
    if let Some(a) = o.simd_autotune {
        cfg.simd.autotune = a;
    }
    cfg
}

/// The `*.toml` fragments across all configured drop-in directories, in apply
/// order: sorted by **file name** (base name, not full path), with a directory's
/// position in `dirs` breaking exact-filename ties (later directory applied
/// later, so it wins). A missing directory contributes nothing (not an error);
/// an unreadable one errors.
fn config_d_files(dirs: &[PathBuf]) -> Result<Vec<PathBuf>, ConfigError> {
    // (file_name, dir_index, full_path) so the sort key is name-then-dir.
    let mut out: Vec<(String, usize, PathBuf)> = Vec::new();
    for (idx, dir) in dirs.iter().enumerate() {
        if !dir.exists() {
            continue;
        }
        let rd = fs::read_dir(dir).map_err(|e| ConfigError::ConfigDir {
            dir: dir.clone(),
            message: e.to_string(),
        })?;
        for entry in rd {
            let entry = entry.map_err(|e| ConfigError::ConfigDir {
                dir: dir.clone(),
                message: e.to_string(),
            })?;
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) == Some("toml") && p.is_file() {
                let name = entry.file_name().to_string_lossy().into_owned();
                out.push((name, idx, p));
            }
        }
    }
    // Filename first (cross-directory pool), directory index as the tie-breaker.
    out.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    Ok(out.into_iter().map(|(_, _, p)| p).collect())
}

#[cfg(test)]
mod env_tests {
    use super::*;

    #[test]
    fn env_overrides_file_but_cli_overrides_env() {
        figment::Jail::expect_with(|jail| {
            jail.create_file("config.toml", "[server]\nbind = \"1.1.1.1:1\"\n")?;
            jail.set_env("HORNDB_SERVER__BIND", "3.3.3.3:3");
            // A flat (single-token) HORNDB_ var like the file-location var must be
            // IGNORED, not rejected by deny_unknown_fields.
            jail.set_env("HORNDB_CONFIG", "/should/be/ignored.toml");

            let base = jail.directory().join("config.toml");
            // env beats file; the flat HORNDB_CONFIG var is ignored by the config
            // provider (it selects the file location, handled by the caller).
            let inputs = LoadInputs {
                cli_config_path: Some(base.clone()),
                ..Default::default()
            };
            let cfg = load(&inputs).unwrap();
            assert_eq!(cfg.server.bind, "3.3.3.3:3");
            assert_eq!(cfg.simd.max_isa, None);

            // cli beats env
            let inputs = LoadInputs {
                cli_config_path: Some(base),
                cli_overrides: CliOverrides {
                    bind: Some("4.4.4.4:4".into()),
                    ..Default::default()
                },
                ..Default::default()
            };
            assert_eq!(load(&inputs).unwrap().server.bind, "4.4.4.4:4");
            Ok(())
        });
    }
}
