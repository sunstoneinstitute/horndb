//! Error type for config resolution and loading.

use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    /// An explicitly requested config file (via `--config` or `HORNDB_CONFIG`)
    /// does not exist. A missing file at the *default* path is not an error.
    #[error("config file {0} was requested but does not exist")]
    MissingExplicitFile(PathBuf),

    /// The merged config failed to parse/validate. `source_desc` names where it
    /// came from (the base file path, or `<env/cli overrides>`).
    #[error("invalid configuration from {source_desc}: {message}")]
    Invalid {
        source_desc: String,
        message: String,
    },

    /// A `config.d` directory was set but could not be read.
    #[error("cannot read config.d directory {dir}: {message}")]
    ConfigDir { dir: PathBuf, message: String },

    /// The live-reload watcher (SPEC-26 S3) could not be established.
    #[error("cannot watch {path} for config changes: {message}")]
    Watch { path: PathBuf, message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_name_their_subject() {
        let e = ConfigError::MissingExplicitFile(PathBuf::from("/x/y.toml"));
        assert!(e.to_string().contains("/x/y.toml"));
        let e = ConfigError::Invalid {
            source_desc: "/etc/horndb/config.toml".into(),
            message: "unknown field `bnid`".into(),
        };
        assert!(e.to_string().contains("config.toml"));
        assert!(e.to_string().contains("bnid"));
    }
}
