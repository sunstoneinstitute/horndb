//! The typed configuration model. Every struct is `#[serde(deny_unknown_fields,
//! default)]` so an unknown key or omitted field is a validation error / a
//! default respectively (SPEC-26 S1/S2).

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::units::{ByteSize, HumanDuration};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_table_is_all_defaults() {
        let cfg: ServerConfig = toml_from("");
        assert_eq!(cfg.server.bind, "127.0.0.1:3840");
        assert_eq!(
            cfg.server.config_dirs,
            vec![PathBuf::from("/etc/horndb/config.d")]
        );
        assert_eq!(cfg.server.limits.query_timeout.0, Duration::from_secs(30));
        assert_eq!(cfg.server.limits.max_result_rows, 1_000_000);
        assert!(!cfg.server.limits.rdf12);
        assert_eq!(cfg.server.limits.max_query_memory, None);
        assert_eq!(cfg.simd.max_isa, None);
        assert!(cfg.simd.autotune);
        assert_eq!(cfg.logging.level, "info");
        assert_eq!(cfg.reload.debounce.0, Duration::from_millis(250));
    }

    #[test]
    fn values_override_defaults() {
        let cfg: ServerConfig = toml_from(
            r#"
            [server]
            bind = "0.0.0.0:80"
            [server.limits]
            query_timeout = "5s"
            max_result_rows = 42
            rdf12 = true
            max_query_memory = "2GiB"
            [simd]
            max_isa = "scalar"
            autotune = false
            "#,
        );
        assert_eq!(cfg.server.bind, "0.0.0.0:80");
        assert_eq!(cfg.server.limits.query_timeout.0, Duration::from_secs(5));
        assert_eq!(cfg.server.limits.max_result_rows, 42);
        assert!(cfg.server.limits.rdf12);
        assert_eq!(
            cfg.server.limits.max_query_memory,
            Some(ByteSize(2 * 1024 * 1024 * 1024))
        );
        assert_eq!(cfg.simd.max_isa.as_deref(), Some("scalar"));
        assert!(!cfg.simd.autotune);
    }

    #[test]
    fn unknown_key_is_rejected() {
        let err = toml::from_str::<ServerConfig>("[server]\nbnid = \"x\"\n").unwrap_err();
        assert!(
            err.to_string().contains("bnid"),
            "error should name the bad key: {err}"
        );
    }

    #[test]
    fn query_settings_from_limits() {
        let limits = Limits {
            max_result_rows: 7,
            ..Default::default()
        };
        let qs = QuerySettings::from_limits(&limits);
        assert_eq!(qs.max_result_rows, 7);
        assert_eq!(qs.query_timeout.0, Duration::from_secs(30));
    }

    fn toml_from(s: &str) -> ServerConfig {
        toml::from_str(s).expect("valid config")
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ServerConfig {
    pub server: Server,
    pub simd: Simd,
    pub logging: Logging,
    pub reload: Reload,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Server {
    pub bind: String,
    /// Ordered list of `config.d` drop-in directories. Fragments from every
    /// directory are pooled and applied in filename order; directory position
    /// only breaks exact-filename ties (later directory wins). See crate docs.
    pub config_dirs: Vec<PathBuf>,
    pub limits: Limits,
}

impl Default for Server {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:3840".to_string(),
            config_dirs: vec![PathBuf::from("/etc/horndb/config.d")],
            limits: Limits::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Limits {
    pub query_timeout: HumanDuration,
    pub max_result_rows: u64,
    pub rdf12: bool,
    pub max_query_memory: Option<ByteSize>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            query_timeout: HumanDuration(Duration::from_secs(30)),
            max_result_rows: 1_000_000,
            rdf12: false,
            max_query_memory: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Simd {
    /// ISA cap (`"scalar"`/`"avx2"`/`"avx512"`/`"neon"`); `None` = auto-detect.
    pub max_isa: Option<String>,
    pub autotune: bool,
}

impl Default for Simd {
    fn default() -> Self {
        Self {
            max_isa: None,
            autotune: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Logging {
    pub level: String,
}

impl Default for Logging {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Reload {
    pub debounce: HumanDuration,
}

impl Default for Reload {
    fn default() -> Self {
        Self {
            debounce: HumanDuration(Duration::from_millis(250)),
        }
    }
}

/// The per-query settings tier: the bounded subset a query may override,
/// defaulting from `[server.limits]`. Override application (URL params) is
/// PLAN-26-02; here it is only constructed from the limits defaults.
#[derive(Debug, Clone, PartialEq)]
pub struct QuerySettings {
    pub query_timeout: HumanDuration,
    pub max_result_rows: u64,
    pub rdf12: bool,
    pub max_query_memory: Option<ByteSize>,
}

impl QuerySettings {
    pub fn from_limits(limits: &Limits) -> Self {
        Self {
            query_timeout: limits.query_timeout,
            max_result_rows: limits.max_result_rows,
            rdf12: limits.rdf12,
            max_query_memory: limits.max_query_memory,
        }
    }
}
