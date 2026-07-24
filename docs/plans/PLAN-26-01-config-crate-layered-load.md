---
status: draft
date: 2026-07-22
scope: "SPEC-26 Phase 1a — the horndb-config crate: typed ServerConfig/QuerySettings model, ByteSize/HumanDuration unit newtypes, layered load (defaults < base config.toml < config.d/*.toml < env < argv) with config.d merge and file-location resolution, and validation. Library only; serve wiring + [simd] injection are PLAN-26-02."
---

# horndb-config crate — layered load Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the new `horndb-config` foundation crate that loads one typed `ServerConfig` by layering built-in defaults, a base `config.toml`, `config.d/*.toml` drop-ins, environment variables, and caller-supplied command-line overrides — with the precedence, `config.d` merge, and validation SPEC-26 S1/S2 require. Tracking issue: `#TODO` (file before the first task lands).

**Architecture:** A dependency-light library crate. The typed model (`ServerConfig` and its sections, plus `QuerySettings`) is plain serde structs with `Default` impls and `deny_unknown_fields`. Two small newtypes (`ByteSize`, `HumanDuration`) parse human strings like `"2GiB"` / `"30s"`. Layering uses the [`figment`](https://docs.rs/figment) crate internally (providers merged low→high: `Serialized` defaults → `Toml` base file → each `Toml` `config.d` fragment in lexical order → `Env` with the `HORNDB_` prefix → a `Serialized` dict built from caller command-line overrides). `figment` is an internal implementation detail — the public API is `load(&LoadInputs) -> Result<ServerConfig, ConfigError>`, so no `figment` type appears in the crate's surface.

**Tech Stack:** Rust 2021 (workspace-pinned 1.90), `serde`, `figment` (features `toml`, `env`), `thiserror`; `tempfile` for tests. This crate does NOT touch the SPARQL server, `serve`, or `horndb-simd` — that wiring is PLAN-26-02.

---

## Design (read this before any task)

### File / module layout

All under a new crate `crates/config/` (crate name `horndb-config`):

- `crates/config/Cargo.toml` — manifest; workspace member + default-member.
- `crates/config/AGENTS.md` + `crates/config/CLAUDE.md` (symlink) — crate agent notes.
- `crates/config/src/lib.rs` — crate docs + `pub use` re-exports; wires the modules.
- `crates/config/src/units.rs` — `ByteSize` and `HumanDuration` newtypes + parsers + serde.
- `crates/config/src/model.rs` — `ServerConfig`, `Server`, `Limits`, `Simd`, `Logging`, `Reload`, `QuerySettings`.
- `crates/config/src/error.rs` — `ConfigError` (thiserror).
- `crates/config/src/load.rs` — `LoadInputs`, `CliOverrides`, path resolution, the figment layering, `load`.
- `crates/config/tests/layering.rs` — integration tests over `config.d` dirs with `tempfile`.

### The two precedence rules (do not conflate them)

1. **Config-file *location*** — *which file* is the base file. `LoadInputs.cli_config_path`
   (from `--config`) > `LoadInputs.env_config_path` (from `HORNDB_CONFIG`) >
   `/etc/horndb/config.toml`. A missing file at the **default** path is fine (skip it);
   a missing file at an **explicit** path (cli or env) is a fatal `ConfigError`.
2. **Config *values*** — for a given setting, lowest→highest: built-in defaults <
   base `config.toml` < `config.d/*.toml` (lexical, `99-*` beats `00-*`) < env vars <
   command-line overrides (`CliOverrides`). CLI wins.

### `config_dirs` resolution is single-pass-then-merge

`config_dirs` (the ordered list of directories where `config.d` fragments live, default
`["/etc/horndb/config.d"]`) may itself be set in the base file / env / CLI. Resolve it from
**defaults + base file + env + CLI only** (a first extract), then merge the fragments from
every listed directory for the final extract. A `config_dirs` value set *inside a config.d
fragment* does NOT relocate the directories (no recursion) — document this in the crate docs.

**Cross-directory apply order (systemd-style).** Pool every `*.toml` from every listed
directory, then apply them sorted by **file name** (base name, not full path): a `90-*` in
one directory overrides a `50-*` in another. Directory position in `config_dirs` only breaks
exact-filename ties — the fragment from the directory later in the list is applied later
(wins). This lets a manually-maintained directory and a machine-maintained one (a future k8s
operator) each drop numbered fragments without clobbering the other wholesale.

### Env var mapping

`figment`'s `Env::prefixed("HORNDB_").split("__")` maps `HORNDB_SERVER__BIND` → `server.bind`
and `HORNDB_SERVER__LIMITS__QUERY_TIMEOUT` → `server.limits.query_timeout`. The two legacy
SIMD var *aliases* (`HORNDB_SIMD_MAX_ISA`, `HORNDB_SIMD_AUTOTUNE`) are handled in PLAN-26-02
at the `serve` layer (they are single-underscore and would otherwise map to `simd.max` /
`simd.autotune`); this crate only implements the general `__`-split mapping. Note that in tests.

### Validation

Every model struct derives `#[serde(deny_unknown_fields, default)]`, so an unknown key or a
mistyped value fails `figment`'s `extract` with a message that names the key path. `load`
converts that into `ConfigError::Invalid { source_desc, message }` where `source_desc` names
the base file path (or `"<env/cli overrides>"`).

### Public API (final shape after this plan)

```rust
// horndb_config
pub use error::ConfigError;
pub use model::{ServerConfig, Server, Limits, Simd, Logging, Reload, QuerySettings};
pub use units::{ByteSize, HumanDuration};
pub use load::{CliOverrides, LoadInputs, load};
```

---

## Task 1: Scaffold the `horndb-config` crate

**Files:**
- Create: `crates/config/Cargo.toml`
- Create: `crates/config/src/lib.rs`
- Create: `crates/config/AGENTS.md`
- Create: `crates/config/CLAUDE.md` (symlink → `AGENTS.md`)
- Modify: `Cargo.toml` (workspace root: `members`, `default-members`, `[workspace.dependencies]`)

- [ ] **Step 1: Add `figment` to the workspace dependency table**

In root `Cargo.toml`, under `[workspace.dependencies]`, add (next to the other shared deps):

```toml
figment = { version = "0.10", features = ["toml", "env"] }
horndb-config = { path = "crates/config" }
```

- [ ] **Step 2: Register the crate as a workspace member and default-member**

In root `Cargo.toml`, add `"crates/config"` to both the `members` array and the
`default-members` array (it is a lightweight leaf, safe to build by default).

- [ ] **Step 3: Write the crate manifest**

Create `crates/config/Cargo.toml`:

```toml
[package]
name = "horndb-config"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true
authors.workspace = true
publish = false

[dependencies]
serde = { workspace = true }
figment = { workspace = true }
thiserror = { workspace = true }

[dev-dependencies]
tempfile = { workspace = true }
```

- [ ] **Step 4: Write a minimal `lib.rs` so the crate compiles**

Create `crates/config/src/lib.rs`:

```rust
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
pub use model::{Limits, Logging, QuerySettings, Reload, Server, ServerConfig, Simd};
pub use units::{ByteSize, HumanDuration};
```

This will not compile until Tasks 2–5 create the modules; that is expected — do those next
before running a build.

- [ ] **Step 5: Write crate agent notes and the symlink**

Create `crates/config/AGENTS.md`:

```markdown
# `horndb-config` (SPEC-26) — agent notes

Foundation crate for the operator configuration system. Loads one typed
`ServerConfig` by layering (lowest→highest): built-in defaults < base
`config.toml` < `config.d/*.toml` drop-ins < env vars (`HORNDB_` prefix,
`__` nesting) < caller command-line overrides. `figment` is an internal
detail; the public API is `load(&LoadInputs) -> Result<ServerConfig, _>`.

- Model: `src/model.rs` — plain serde structs, `#[serde(deny_unknown_fields, default)]`.
- Units: `src/units.rs` — `ByteSize` (`"2GiB"`), `HumanDuration` (`"30s"`).
- `[server].config_dirs` is a *list* of drop-in dirs (default one entry). Fragments
  from all dirs are pooled and applied in filename order; directory position only
  breaks exact-filename ties (later dir wins). Lets a manual dir and a k8s-operator
  dir coexist. A `config_dirs` set inside a fragment does not relocate the dirs.
- `[simd]` is restart-only; the reload watcher (SPEC-26 S3, later phase) never touches it.
- `serve` wiring and `[simd]` injection live in `crates/sparql` (PLAN-26-02), not here.
```

Then create the symlink (never a copy):

```bash
ln -s AGENTS.md crates/config/CLAUDE.md
```

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates/config/Cargo.toml crates/config/src/lib.rs crates/config/AGENTS.md crates/config/CLAUDE.md
git commit -m "feat(config): scaffold horndb-config crate"
```

---

## Task 2: `ByteSize` newtype + parser

**Files:**
- Create: `crates/config/src/units.rs`
- Test: `crates/config/src/units.rs` (inline `#[cfg(test)]`)

- [ ] **Step 1: Write the failing tests for `ByteSize`**

Create `crates/config/src/units.rs` with the tests first (the types below are added in Step 3):

```rust
//! Human-string unit newtypes shared by config files and (later) URL params.
//!
//! `ByteSize` accepts a raw integer byte count or an IEC binary-unit suffix
//! (`KiB`/`MiB`/`GiB`/`TiB`, case-insensitive). `HumanDuration` accepts an
//! integer with an `ms`/`s`/`m`/`h` suffix. One grammar, no decimal-vs-binary
//! ambiguity (SPEC-26 S2 / Risks: pin one grammar).

use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[cfg(test)]
mod byte_size_tests {
    use super::*;

    #[test]
    fn parses_raw_bytes() {
        assert_eq!("1024".parse::<ByteSize>().unwrap(), ByteSize(1024));
        assert_eq!("0".parse::<ByteSize>().unwrap(), ByteSize(0));
    }

    #[test]
    fn parses_iec_suffixes() {
        assert_eq!("2GiB".parse::<ByteSize>().unwrap(), ByteSize(2 * 1024 * 1024 * 1024));
        assert_eq!("512MiB".parse::<ByteSize>().unwrap(), ByteSize(512 * 1024 * 1024));
        assert_eq!("1kib".parse::<ByteSize>().unwrap(), ByteSize(1024)); // case-insensitive
        assert_eq!(" 4 TiB ".parse::<ByteSize>().unwrap(), ByteSize(4 * 1024u64.pow(4))); // trimmed + inner space
    }

    #[test]
    fn rejects_garbage() {
        assert!("2GB".parse::<ByteSize>().is_err()); // decimal units not accepted
        assert!("abc".parse::<ByteSize>().is_err());
        assert!("".parse::<ByteSize>().is_err());
        assert!("-5".parse::<ByteSize>().is_err());
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p horndb-config byte_size_tests`
Expected: FAIL to compile — `ByteSize` not defined.

- [ ] **Step 3: Implement `ByteSize`**

Add to `crates/config/src/units.rs`:

```rust
/// A byte count parsed from a raw integer or an IEC binary suffix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ByteSize(pub u64);

impl FromStr for ByteSize {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let t = s.trim();
        if t.is_empty() {
            return Err("empty byte size".to_string());
        }
        // Split leading digits from an optional unit suffix; allow an inner space.
        let digits_end = t.find(|c: char| !c.is_ascii_digit()).unwrap_or(t.len());
        if digits_end == 0 {
            return Err(format!("no leading number in byte size {s:?}"));
        }
        let num: u64 = t[..digits_end]
            .parse()
            .map_err(|_| format!("invalid number in byte size {s:?}"))?;
        let unit = t[digits_end..].trim().to_ascii_lowercase();
        let mult: u64 = match unit.as_str() {
            "" | "b" => 1,
            "kib" => 1024,
            "mib" => 1024 * 1024,
            "gib" => 1024 * 1024 * 1024,
            "tib" => 1024u64.pow(4),
            other => return Err(format!("unknown byte-size unit {other:?} (use B/KiB/MiB/GiB/TiB)")),
        };
        num.checked_mul(mult)
            .map(ByteSize)
            .ok_or_else(|| format!("byte size {s:?} overflows u64"))
    }
}

impl Serialize for ByteSize {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(self.0)
    }
}

impl<'de> Deserialize<'de> for ByteSize {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // Accept either an integer (raw bytes) or a string ("2GiB").
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Int(u64),
            Str(String),
        }
        match Repr::deserialize(d)? {
            Repr::Int(n) => Ok(ByteSize(n)),
            Repr::Str(s) => s.parse().map_err(serde::de::Error::custom),
        }
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p horndb-config byte_size_tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/config/src/units.rs
git commit -m "feat(config): ByteSize newtype with IEC-suffix parsing"
```

---

## Task 3: `HumanDuration` newtype + parser

**Files:**
- Modify: `crates/config/src/units.rs`

- [ ] **Step 1: Write the failing tests for `HumanDuration`**

Append to `crates/config/src/units.rs`:

```rust
#[cfg(test)]
mod duration_tests {
    use super::*;

    #[test]
    fn parses_suffixes() {
        assert_eq!("30s".parse::<HumanDuration>().unwrap().0, Duration::from_secs(30));
        assert_eq!("250ms".parse::<HumanDuration>().unwrap().0, Duration::from_millis(250));
        assert_eq!("5m".parse::<HumanDuration>().unwrap().0, Duration::from_secs(300));
        assert_eq!("1h".parse::<HumanDuration>().unwrap().0, Duration::from_secs(3600));
        assert_eq!(" 0s ".parse::<HumanDuration>().unwrap().0, Duration::from_secs(0));
    }

    #[test]
    fn rejects_garbage() {
        assert!("30".parse::<HumanDuration>().is_err()); // bare number: unit required
        assert!("s".parse::<HumanDuration>().is_err());
        assert!("30x".parse::<HumanDuration>().is_err());
        assert!("".parse::<HumanDuration>().is_err());
    }

    #[test]
    fn round_trips_through_serde_string() {
        // Deserializing from a TOML-ish string value works.
        let d: HumanDuration = serde_json::from_str("\"45s\"").unwrap();
        assert_eq!(d.0, Duration::from_secs(45));
    }
}
```

Add `serde_json` as a dev-dependency for this round-trip test — in `crates/config/Cargo.toml`
under `[dev-dependencies]` add `serde_json = { workspace = true }`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p horndb-config duration_tests`
Expected: FAIL to compile — `HumanDuration` not defined.

- [ ] **Step 3: Implement `HumanDuration`**

Append to `crates/config/src/units.rs`:

```rust
/// A duration parsed from an integer with an `ms`/`s`/`m`/`h` suffix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HumanDuration(pub Duration);

impl FromStr for HumanDuration {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let t = s.trim();
        let digits_end = t.find(|c: char| !c.is_ascii_digit()).unwrap_or(t.len());
        if digits_end == 0 {
            return Err(format!("no leading number in duration {s:?}"));
        }
        let num: u64 = t[..digits_end]
            .parse()
            .map_err(|_| format!("invalid number in duration {s:?}"))?;
        let unit = t[digits_end..].trim();
        let dur = match unit {
            "ms" => Duration::from_millis(num),
            "s" => Duration::from_secs(num),
            "m" => Duration::from_secs(num * 60),
            "h" => Duration::from_secs(num * 3600),
            "" => return Err(format!("duration {s:?} needs a unit (ms/s/m/h)")),
            other => return Err(format!("unknown duration unit {other:?} (use ms/s/m/h)")),
        };
        Ok(HumanDuration(dur))
    }
}

impl Serialize for HumanDuration {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        // Serialize back to a human string so `Serialized::defaults` round-trips.
        let ms = self.0.as_millis();
        s.serialize_str(&format!("{ms}ms"))
    }
}

impl<'de> Deserialize<'de> for HumanDuration {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p horndb-config duration_tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/config/src/units.rs crates/config/Cargo.toml
git commit -m "feat(config): HumanDuration newtype with ms/s/m/h parsing"
```

---

## Task 4: Typed model with defaults + `deny_unknown_fields`

**Files:**
- Create: `crates/config/src/model.rs`

- [ ] **Step 1: Write the failing tests for the model**

Create `crates/config/src/model.rs` with the tests first:

```rust
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
        assert_eq!(cfg.server.config_dirs, vec![PathBuf::from("/etc/horndb/config.d")]);
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
        assert_eq!(cfg.server.limits.max_query_memory, Some(ByteSize(2 * 1024 * 1024 * 1024)));
        assert_eq!(cfg.simd.max_isa.as_deref(), Some("scalar"));
        assert!(!cfg.simd.autotune);
    }

    #[test]
    fn unknown_key_is_rejected() {
        let err = toml::from_str::<ServerConfig>("[server]\nbnid = \"x\"\n").unwrap_err();
        assert!(err.to_string().contains("bnid"), "error should name the bad key: {err}");
    }

    #[test]
    fn query_settings_from_limits() {
        let limits = Limits { max_result_rows: 7, ..Default::default() };
        let qs = QuerySettings::from_limits(&limits);
        assert_eq!(qs.max_result_rows, 7);
        assert_eq!(qs.query_timeout.0, Duration::from_secs(30));
    }

    fn toml_from(s: &str) -> ServerConfig {
        toml::from_str(s).expect("valid config")
    }
}
```

Add `toml` as a dev-dependency: in `crates/config/Cargo.toml` under `[dev-dependencies]` add
`toml = { workspace = true }`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p horndb-config --lib model`
Expected: FAIL to compile — the model types are not defined.

- [ ] **Step 3: Implement the model**

Add to `crates/config/src/model.rs` (above the `#[cfg(test)]` module):

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ServerConfig {
    pub server: Server,
    pub simd: Simd,
    pub logging: Logging,
    pub reload: Reload,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            server: Server::default(),
            simd: Simd::default(),
            logging: Logging::default(),
            reload: Reload::default(),
        }
    }
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
        Self { max_isa: None, autotune: true }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Logging {
    pub level: String,
}

impl Default for Logging {
    fn default() -> Self {
        Self { level: "info".to_string() }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Reload {
    pub debounce: HumanDuration,
}

impl Default for Reload {
    fn default() -> Self {
        Self { debounce: HumanDuration(Duration::from_millis(250)) }
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
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p horndb-config --lib model`
Expected: PASS (all four model tests).

- [ ] **Step 5: Commit**

```bash
git add crates/config/src/model.rs crates/config/Cargo.toml
git commit -m "feat(config): typed ServerConfig/QuerySettings model with defaults"
```

---

## Task 5: `ConfigError` type

**Files:**
- Create: `crates/config/src/error.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/config/src/error.rs`:

```rust
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
    Invalid { source_desc: String, message: String },

    /// A `config.d` directory was set but could not be read.
    #[error("cannot read config.d directory {dir}: {message}")]
    ConfigDir { dir: PathBuf, message: String },
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
```

- [ ] **Step 2: Run the test to verify it fails, then passes**

Run: `cargo test -p horndb-config --lib error`
Expected: FAIL to compile until the file is saved, then PASS (the type and test are in the
same file, so once saved it compiles and passes).

- [ ] **Step 3: Commit**

```bash
git add crates/config/src/error.rs
git commit -m "feat(config): ConfigError type"
```

---

## Task 6: File-location resolution

**Files:**
- Create: `crates/config/src/load.rs`

- [ ] **Step 1: Write the failing tests for path resolution**

Create `crates/config/src/load.rs` with the input types and resolution tests first:

```rust
//! Layered loading: path resolution, figment providers, and `load`.

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
        assert_eq!(resolve_base_path(&inputs), (PathBuf::from("/cli.toml"), true));

        let inputs = LoadInputs { env_config_path: Some("/env.toml".into()), ..Default::default() };
        assert_eq!(resolve_base_path(&inputs), (PathBuf::from("/env.toml"), true));

        let inputs = LoadInputs::default();
        assert_eq!(resolve_base_path(&inputs), (PathBuf::from(DEFAULT_CONFIG_PATH), false));
    }
}
```

- [ ] **Step 2: Run the tests to verify they pass**

Run: `cargo test -p horndb-config --lib resolve_tests`
Expected: PASS. (The `figment`/`ServerConfig` imports are unused until Task 7 — allow the
`unused_imports` warning for now, or add `#[allow(unused_imports)]` on the `use` lines; Task 7
removes the need.)

- [ ] **Step 3: Commit**

```bash
git add crates/config/src/load.rs
git commit -m "feat(config): base-file path resolution (cli > env > default)"
```

---

## Task 7: Layered load with `config.d` merge

**Files:**
- Modify: `crates/config/src/load.rs`
- Test: `crates/config/tests/layering.rs`

- [ ] **Step 1: Write the failing integration test**

Create `crates/config/tests/layering.rs`:

```rust
//! End-to-end layering tests over real temp directories.

use std::fs;

use horndb_config::{load, ByteSize, CliOverrides, LoadInputs};
use tempfile::tempdir;

/// Build a `LoadInputs` pointing at a base file, with an isolated config.d.
fn inputs_for(base: &std::path::Path) -> LoadInputs {
    LoadInputs { cli_config_path: Some(base.to_path_buf()), ..Default::default() }
}

#[test]
fn config_d_fragment_overrides_base_and_orders_lexically() {
    let dir = tempdir().unwrap();
    let cfg_d = dir.path().join("config.d");
    fs::create_dir(&cfg_d).unwrap();

    let base = dir.path().join("config.toml");
    fs::write(
        &base,
        format!(
            "[server]\nbind = \"1.1.1.1:1\"\nconfig_dirs = [\"{}\"]\n[server.limits]\nmax_result_rows = 1\n",
            cfg_d.display()
        ),
    )
    .unwrap();
    fs::write(&cfg_d.join("00-a.toml"), "[server.limits]\nmax_result_rows = 2\n").unwrap();
    fs::write(&cfg_d.join("99-z.toml"), "[server.limits]\nmax_result_rows = 3\n").unwrap();

    let cfg = load(&inputs_for(&base)).unwrap();
    // 99-z wins over 00-a wins over base.
    assert_eq!(cfg.server.limits.max_result_rows, 3);
    // A key only in the base survives the merge.
    assert_eq!(cfg.server.bind, "1.1.1.1:1");
}

#[test]
fn multiple_config_dirs_pool_and_sort_by_filename() {
    let dir = tempdir().unwrap();
    // Two independent drop-in directories: a "manual" one and an "operator" one.
    let manual = dir.path().join("manual.d");
    let operator = dir.path().join("operator.d");
    fs::create_dir(&manual).unwrap();
    fs::create_dir(&operator).unwrap();

    let base = dir.path().join("config.toml");
    fs::write(
        &base,
        format!(
            "[server]\nconfig_dirs = [\"{}\", \"{}\"]\n[server.limits]\nmax_result_rows = 1\n",
            manual.display(),
            operator.display()
        ),
    )
    .unwrap();
    // Operator drops 50-*, manual overrides with 90-*: cross-directory filename
    // order means 90-* is applied last and wins, regardless of directory.
    fs::write(&operator.join("50-op.toml"), "[server.limits]\nmax_result_rows = 2\n").unwrap();
    fs::write(&manual.join("90-override.toml"), "[server.limits]\nmax_result_rows = 3\n").unwrap();
    let cfg = load(&inputs_for(&base)).unwrap();
    assert_eq!(cfg.server.limits.max_result_rows, 3);

    // Same filename in both dirs: the later directory (operator) wins the tie.
    fs::write(&manual.join("50-op.toml"), "[server.limits]\nmax_result_rows = 7\n").unwrap();
    fs::remove_file(manual.join("90-override.toml")).unwrap();
    let cfg = load(&inputs_for(&base)).unwrap();
    assert_eq!(cfg.server.limits.max_result_rows, 2); // operator.d/50-op wins the tie
}

#[test]
fn cli_override_beats_file() {
    let dir = tempdir().unwrap();
    let base = dir.path().join("config.toml");
    fs::write(&base, "[server]\nbind = \"1.1.1.1:1\"\n").unwrap();

    let mut inputs = inputs_for(&base);
    inputs.cli_overrides = CliOverrides { bind: Some("2.2.2.2:2".into()), ..Default::default() };
    let cfg = load(&inputs).unwrap();
    assert_eq!(cfg.server.bind, "2.2.2.2:2"); // CLI wins over the file
}

#[test]
fn missing_default_file_is_ok_missing_explicit_is_error() {
    // Explicit path that does not exist -> error.
    let inputs = LoadInputs {
        cli_config_path: Some("/nonexistent/nope.toml".into()),
        ..Default::default()
    };
    assert!(load(&inputs).is_err());

    // No file anywhere (default path unlikely to exist in CI) -> defaults.
    let cfg = load(&LoadInputs::default()).unwrap();
    assert_eq!(cfg.server.limits.max_query_memory, None::<ByteSize>);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p horndb-config --test layering`
Expected: FAIL to compile — `load` is not defined yet.

- [ ] **Step 3: Implement `load`**

Append to `crates/config/src/load.rs`:

```rust
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
        .map_err(|e| ConfigError::Invalid { source_desc, message: e.to_string() })?;

    // CLI overrides are the top layer, applied as a typed overlay because figment
    // cannot express "only override when `Some`".
    Ok(apply_cli_overrides(cfg, &inputs.cli_overrides))
}

/// The environment layer: `HORNDB_`-prefixed vars, restricted to the nested form
/// (`HORNDB_SERVER__BIND` → `server.bind`). Flat vars are dropped, so
/// `HORNDB_CONFIG` (the file-location var) and the legacy single-underscore SIMD
/// aliases (`HORNDB_SIMD_MAX_ISA` / `HORNDB_SIMD_AUTOTUNE`, consumed explicitly by
/// serve in PLAN-26-02) never leak in and never trip `deny_unknown_fields`.
/// (Confirm the exact `filter`/`split` combinator order against the figment docs
/// when implementing; the intent is "keep only keys containing `__`, then split".)
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
```

Add `use std::fs;` to the top of `load.rs`. Note the design decision baked in here: the
`Serialized`/figment layer cannot express "only override when `Some`", so command-line
overrides are applied by `apply_cli_overrides` (a typed overlay) as the final, top layer —
this is simpler and less error-prone than constructing a partial figment `Dict`, and keeps
CLI-wins precedence exact. The env layer is still figment-native.

- [ ] **Step 4: Run the integration test to verify it passes**

Run: `cargo test -p horndb-config --test layering`
Expected: PASS (all three tests).

- [ ] **Step 5: Add an env-precedence test**

Because env vars are process-global and can bleed across tests, run this one in its own
`#[test]` using `figment`'s jailed environment. Append to `crates/config/src/load.rs` a
`#[cfg(test)]` module:

```rust
#[cfg(test)]
mod env_tests {
    use super::*;

    #[test]
    fn env_overrides_file_but_cli_overrides_env() {
        figment::Jail::expect_with(|jail| {
            jail.create_file("config.toml", "[server]\nbind = \"1.1.1.1:1\"\n")?;
            jail.set_env("HORNDB_SERVER__BIND", "3.3.3.3:3");
            // Flat HORNDB_ vars must be IGNORED, not rejected by deny_unknown_fields:
            // the file-location var and the legacy single-underscore SIMD alias.
            jail.set_env("HORNDB_CONFIG", "/should/be/ignored.toml");
            jail.set_env("HORNDB_SIMD_MAX_ISA", "avx2");

            let base = jail.directory().join("config.toml");
            // env beats file; flat vars are ignored (max_isa stays default None here —
            // the legacy alias is wired in serve, PLAN-26-02).
            let inputs = LoadInputs { cli_config_path: Some(base.clone()), ..Default::default() };
            let cfg = load(&inputs).unwrap();
            assert_eq!(cfg.server.bind, "3.3.3.3:3");
            assert_eq!(cfg.simd.max_isa, None);

            // cli beats env
            let inputs = LoadInputs {
                cli_config_path: Some(base),
                cli_overrides: CliOverrides { bind: Some("4.4.4.4:4".into()), ..Default::default() },
                ..Default::default()
            };
            assert_eq!(load(&inputs).unwrap().server.bind, "4.4.4.4:4");
            Ok(())
        });
    }
}
```

- [ ] **Step 6: Run the env test**

Run: `cargo test -p horndb-config --lib env_tests`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/config/src/load.rs crates/config/tests/layering.rs
git commit -m "feat(config): layered load with config.d merge and CLI/env precedence"
```

---

## Task 8: Validation surfaces bad keys/values

**Files:**
- Modify: `crates/config/tests/layering.rs`

- [ ] **Step 1: Write the failing validation tests**

Append to `crates/config/tests/layering.rs`:

```rust
#[test]
fn unknown_key_in_base_file_errors_with_key_name() {
    let dir = tempdir().unwrap();
    let base = dir.path().join("config.toml");
    fs::write(&base, "[server]\nbnid = \"oops\"\n").unwrap();

    let err = load(&inputs_for(&base)).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("bnid"), "error should name the bad key: {msg}");
    assert!(msg.contains("config.toml"), "error should name the source file: {msg}");
}

#[test]
fn bad_duration_value_errors() {
    let dir = tempdir().unwrap();
    let base = dir.path().join("config.toml");
    fs::write(&base, "[server.limits]\nquery_timeout = \"30x\"\n").unwrap();

    let err = load(&inputs_for(&base)).unwrap_err();
    assert!(err.to_string().to_lowercase().contains("duration"), "{err}");
}
```

- [ ] **Step 2: Run to verify behavior**

Run: `cargo test -p horndb-config --test layering`
Expected: PASS. (If the source-file name is not in the message because the error surfaces in
pass 1 before `source_desc` is attached, confirm `load` wraps *both* extracts with the same
`ConfigError::Invalid { source_desc, .. }`; it does per Task 7. `deny_unknown_fields` makes the
unknown-key case fail extraction, and `HumanDuration`'s parser supplies the `"duration"` text.)

- [ ] **Step 3: Commit**

```bash
git add crates/config/tests/layering.rs
git commit -m "test(config): validation surfaces unknown keys and bad values"
```

---

## Task 9: Lint, docs sync, and plan close-out

**Files:**
- Modify: `docs/architecture.md`
- Modify: `docs/plans/PLAN-26-01-config-crate-layered-load.md` (this file: flip `status:` to `executed`)

- [ ] **Step 1: Full lint + build + test for the new crate**

Run:
```bash
cargo fmt --all
cargo clippy -p horndb-config --all-targets -- -D warnings
cargo nextest run -p horndb-config
```
Expected: clean format, no clippy warnings, all tests pass. Fix any `unused_imports` left from
Task 6 (the `ServerConfig`/`figment` imports are used once Task 7 lands).

- [ ] **Step 2: Add the config subsystem to the architecture status map**

In `docs/architecture.md`, add a new subsystem section for SPEC-26 (follow the existing
section shape: a `**Crate:** ... · **Spec:** ... · **Overall status:** ...` line, a
one-paragraph description, and a component table). The table's first row records this plan:

```markdown
| Component | Status | Notes |
|---|---|---|
| Layered load (`horndb-config`: defaults < base < config.d < env < argv), typed model, validation | **implemented** | `crates/config/`, SPEC-26 S1/S2 (PLAN-26-01). Library only. |
| `serve` wiring (`--config`, value flags, `[simd]` injection, startup-fatal validation) | **planned** | SPEC-26 S6 (PLAN-26-02, `#TODO`). |
| Live watch/reload, per-query URL overrides + enforcement | **planned** | SPEC-26 S3/S4/S5 (later phases, `#TODO`). |
```

(Do not edit `TASKS.md` on this feature branch — the matching `TASKS.md` transition lands as a
locked commit on `main` after merge, per the root `CLAUDE.md` feature-branch exception.)

- [ ] **Step 3: Flip the plan status**

In this file's frontmatter, change `status: draft` to `status: executed`.

- [ ] **Step 4: Commit**

```bash
git add docs/architecture.md docs/plans/PLAN-26-01-config-crate-layered-load.md
git commit -m "docs: record horndb-config crate in architecture; close PLAN-26-01"
```

---

## Self-review notes

Run before the first task lands:

- **Spec coverage (SPEC-26 Phase 1 library portion):** S1 layered resolution + `config.d`
  merge + precedence (Tasks 6, 7); S1 validation (Tasks 4, 8); S2 typed model + two tiers +
  unit newtypes (Tasks 2, 3, 4). Deferred to PLAN-26-02 (called out, not dropped): `serve`
  `--config`/value flags, `[simd]` injection into `horndb-simd`, startup-fatal exit wiring,
  the legacy single-underscore SIMD env aliases, and per-query URL overrides (S4) + enforcement
  (S5) + live reload (S3), which are later phases.
- **Precedence direction:** every test asserts CLI > env > config.d > base > defaults (SPEC-26
  S1, CLI-wins). `apply_cli_overrides` is the top layer; `Env::prefixed(...).split("__")` sits
  below it and above the files.
- **Type consistency:** `HumanDuration(Duration)` and `ByteSize(u64)` are used identically in
  `units.rs`, `model.rs`, and the tests; `Limits` field names (`query_timeout`,
  `max_result_rows`, `rdf12`, `max_query_memory`) match `QuerySettings::from_limits` and every
  test. `config_dirs` default `["/etc/horndb/config.d"]` (single entry, no trailing slash) is
  asserted in the model test; `config_d_files` pools `*.toml` across all listed directories and
  applies them in filename order (directory index breaks exact-name ties).
- **`[simd]` is restart-only** — nothing in this crate reloads or re-reads it; injection into
  `horndb-simd` is PLAN-26-02 and happens once at startup.
- **Placeholder scan:** no `TODO`/`TBD` in code; the only `#TODO`s are the sanctioned
  unfiled-issue markers (file the tracking issue before Task 1).
