//! Loader for `harness/selected.toml`.
//!
//! SPEC-01 F11: this file declares the exact list of test IDs the
//! harness is expected to pass *right now*. CI runs only the selected
//! subset, so adding tests is the discipline that grows the engine.
//! Removing tests requires an `xfail_reason` with a tracking issue.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Selected {
    /// Schema version of this file. Increment when the layout changes.
    pub version: u32,
    /// Per-suite entries. Suite key is the same string as
    /// [`crate::testcase::Suite::as_str`].
    pub suites: std::collections::BTreeMap<String, SuiteEntry>,
    /// History of removed tests (must be non-empty to remove anything).
    #[serde(default)]
    pub removed: Vec<Removed>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SuiteEntry {
    /// Path to the manifest file, relative to the workspace root.
    pub manifest: String,
    /// Test IDs that must pass.
    pub include: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Removed {
    pub test_id: String,
    pub suite: String,
    pub xfail_reason: String,
    pub tracking_issue: String,
}

impl Selected {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        let parsed: Selected = toml::from_str(&raw)
            .with_context(|| format!("parsing {}", path.display()))?;
        parsed.validate()?;
        Ok(parsed)
    }

    pub fn validate(&self) -> Result<()> {
        if self.version != 1 {
            bail!("unsupported selected.toml schema version: {}", self.version);
        }
        if self.suites.is_empty() {
            bail!("selected.toml must select at least one suite");
        }
        for (name, entry) in &self.suites {
            if entry.include.is_empty() {
                bail!("suite {name} has no included tests");
            }
            let mut seen = BTreeSet::new();
            for id in &entry.include {
                if !seen.insert(id) {
                    bail!("duplicate include {id} in suite {name}");
                }
            }
        }
        Ok(())
    }

    pub fn is_selected(&self, suite: &str, test_id: &str) -> bool {
        self.suites
            .get(suite)
            .map(|s| s.include.iter().any(|id| id == test_id))
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_toml(s: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(s.as_bytes()).unwrap();
        f
    }

    #[test]
    fn rejects_wrong_version() {
        let f = write_toml(r#"version = 2
[suites.owl2]
manifest = "x"
include = ["t"]
"#);
        let err = Selected::load(f.path()).unwrap_err();
        assert!(err.to_string().contains("schema version"));
    }

    #[test]
    fn rejects_empty_include() {
        let f = write_toml(r#"version = 1
[suites.owl2]
manifest = "x"
include = []
"#);
        let err = Selected::load(f.path()).unwrap_err();
        assert!(err.to_string().contains("no included tests"));
    }

    #[test]
    fn rejects_duplicates() {
        let f = write_toml(r#"version = 1
[suites.owl2]
manifest = "x"
include = ["t", "t"]
"#);
        let err = Selected::load(f.path()).unwrap_err();
        assert!(err.to_string().contains("duplicate include"));
    }

    #[test]
    fn round_trip_selected() {
        let f = write_toml(r#"version = 1
[suites.owl2]
manifest = "crates/harness/tests/fixtures/owl2/manifest.ttl"
include = ["file:///x#trivial-entail-true"]
"#);
        let sel = Selected::load(f.path()).unwrap();
        assert!(sel.is_selected("owl2", "file:///x#trivial-entail-true"));
        assert!(!sel.is_selected("owl2", "other"));
        assert!(!sel.is_selected("sparql11", "file:///x#trivial-entail-true"));
    }
}
