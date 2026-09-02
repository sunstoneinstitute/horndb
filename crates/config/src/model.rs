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
        assert_eq!(cfg.server.limits.default_graph, DefaultGraph::Union);
        assert_eq!(cfg.server.shutdown_drain.0, Duration::from_secs(30));
        assert_eq!(
            cfg.server.limits.max_concurrent_queries,
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(DEFAULT_MAX_CONCURRENT_QUERIES)
        );
        assert_eq!(cfg.server.limits.queue_timeout.0, Duration::from_secs(5));
        assert_eq!(
            cfg.server.limits.max_request_body,
            ByteSize(4 * 1024 * 1024)
        );
        assert_eq!(cfg.simd.max_isa, None);
        assert!(cfg.simd.autotune);
        assert_eq!(cfg.logging.level, "info");
        assert_eq!(cfg.reload.debounce.0, Duration::from_millis(250));
        assert_eq!(cfg.reasoning.on_inconsistency, OnInconsistency::Warn);
    }

    #[test]
    fn values_override_defaults() {
        let cfg: ServerConfig = toml_from(
            r#"
            [server]
            bind = "0.0.0.0:80"
            shutdown_drain = "5s"
            [server.limits]
            query_timeout = "5s"
            max_result_rows = 42
            rdf12 = true
            max_query_memory = "2GiB"
            default_graph = "strict"
            max_concurrent_queries = 3
            queue_timeout = "250ms"
            max_request_body = "1MiB"
            [simd]
            max_isa = "scalar"
            autotune = false
            [reasoning]
            on_inconsistency = "reject-startup"
            "#,
        );
        assert_eq!(cfg.server.bind, "0.0.0.0:80");
        assert_eq!(cfg.server.shutdown_drain.0, Duration::from_secs(5));
        assert_eq!(cfg.server.limits.query_timeout.0, Duration::from_secs(5));
        assert_eq!(cfg.server.limits.max_result_rows, 42);
        assert!(cfg.server.limits.rdf12);
        assert_eq!(
            cfg.server.limits.max_query_memory,
            Some(ByteSize(2 * 1024 * 1024 * 1024))
        );
        assert_eq!(cfg.server.limits.default_graph, DefaultGraph::Strict);
        assert_eq!(cfg.server.limits.max_concurrent_queries, 3);
        assert_eq!(
            cfg.server.limits.queue_timeout.0,
            Duration::from_millis(250)
        );
        assert_eq!(cfg.server.limits.max_request_body, ByteSize(1024 * 1024));
        assert_eq!(cfg.simd.max_isa.as_deref(), Some("scalar"));
        assert!(!cfg.simd.autotune);
        assert_eq!(
            cfg.reasoning.on_inconsistency,
            OnInconsistency::RejectStartup
        );
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
        assert_eq!(qs.default_graph, DefaultGraph::Union);
    }

    /// SPEC-26 S1: a rejection names the bad value (and, once loaded through
    /// `horndb_config::load` rather than raw `toml::from_str`, the source
    /// file too — see `crates/sparql/tests/serve_config_wiring.rs`'s
    /// `invalid_default_graph_exits_nonzero_naming_the_value`). A serde-level
    /// enum gets this validation for free from figment/serde, unlike the
    /// free-string `[simd].max_isa`, which is checked by hand in `serve.rs`.
    #[test]
    fn invalid_default_graph_value_is_rejected() {
        let err = toml::from_str::<ServerConfig>("[server.limits]\ndefault_graph = \"bogus\"\n")
            .unwrap_err();
        assert!(
            err.to_string().contains("bogus"),
            "error should name the bad value: {err}"
        );
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
    pub reasoning: Reasoning,
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
    /// HDB-124: how long a graceful shutdown (SIGTERM/SIGINT) waits for
    /// in-flight requests to finish before the process force-exits.
    pub shutdown_drain: HumanDuration,
}

impl Default for Server {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:3840".to_string(),
            config_dirs: vec![PathBuf::from("/etc/horndb/config.d")],
            limits: Limits::default(),
            shutdown_drain: HumanDuration(Duration::from_secs(30)),
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
    /// SPEC-28 S3/D2: how the no-dataset default graph is composed.
    pub default_graph: DefaultGraph,
    /// HDB-118 admission control: how many `/query` requests may execute at
    /// once. Server-scope, not per-query overridable (it is not in
    /// [`QuerySettings`]). Defaults to the core count; `0` is rejected.
    pub max_concurrent_queries: usize,
    /// How long a request waits for an execution slot before the server
    /// sheds it with HTTP 503 + `Retry-After` (HDB-118).
    pub queue_timeout: HumanDuration,
    /// Cap on the `/query` and `/update` request body. `LOAD` payloads are
    /// files, not request bodies, so this does not bound bulk ingest.
    pub max_request_body: ByteSize,
}

/// Fallback when the core count is unavailable (e.g. a restricted container).
const DEFAULT_MAX_CONCURRENT_QUERIES: usize = 8;

impl Default for Limits {
    fn default() -> Self {
        Self {
            query_timeout: HumanDuration(Duration::from_secs(30)),
            max_result_rows: 1_000_000,
            rdf12: false,
            max_query_memory: None,
            default_graph: DefaultGraph::default(),
            max_concurrent_queries: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(DEFAULT_MAX_CONCURRENT_QUERIES),
            queue_timeout: HumanDuration(Duration::from_secs(5)),
            max_request_body: ByteSize(4 * 1024 * 1024),
        }
    }
}

/// How the no-dataset default graph is composed (SPEC-28 S3/D2). A typed
/// enum, not a free `String`: this crate has no dependency on
/// `horndb-sparql` (`horndb-sparql` maps this onto its own
/// `DefaultGraphMode` via `From`, the other direction), but an enum still
/// gets figment/serde's file+key rejection attribution for an unrecognized
/// value — SPEC-26 S1's requirement — for free, unlike a free-string field
/// such as `[simd].max_isa`, which is checked by hand downstream.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DefaultGraph {
    /// The union of every non-reserved graph — the SPARQL-friendly default.
    #[default]
    Union,
    /// Only the default-graph sentinel; no named-graph data is visible.
    Strict,
}

/// `[reasoning]` — what the server does when OWL 2 RL materialization derives
/// the `owl:Nothing` inconsistency marker (HDB-125).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Reasoning {
    pub on_inconsistency: OnInconsistency,
}

/// Inconsistency policy. A serde enum (like `DefaultGraph`), so an
/// unrecognized value is rejected at load with file+key attribution.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OnInconsistency {
    /// Log a warning naming the `owl:Nothing` individuals, then serve.
    #[default]
    Warn,
    /// Log the same warning and exit non-zero instead of serving. The load
    /// runs after the socket binds (HDB-124), so the process exits without
    /// ever reporting ready rather than never binding.
    RejectStartup,
    /// Warn, serve, and mark every HTTP response `x-horndb-inconsistent: true`.
    ServeWithFlag,
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
    pub default_graph: DefaultGraph,
}

impl QuerySettings {
    pub fn from_limits(limits: &Limits) -> Self {
        Self {
            query_timeout: limits.query_timeout,
            max_result_rows: limits.max_result_rows,
            rdf12: limits.rdf12,
            max_query_memory: limits.max_query_memory,
            default_graph: limits.default_graph,
        }
    }
}
