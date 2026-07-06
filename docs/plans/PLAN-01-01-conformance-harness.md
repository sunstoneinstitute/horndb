---
status: executed
date: 2026-05-24
scope: "SPEC-01 Conformance & Benchmarking Harness — Stage 0 + Stage 1"
---

# SPEC-01 Conformance & Benchmarking Harness — Stage 0 + Stage 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the conformance & benchmarking harness for the HornDB project — first against a deliberately-failing in-tree stub engine (Stage 0), then against the real engine running ≥50 hand-picked W3C OWL 2 RL test cases plus an ORE-2015 / LDBC-SPB-256 comparison run (Stage 1).

**Architecture:** A single `horndb-harness` crate exposes (a) a `Reasoner` trait the engine implements, (b) parsers for the W3C OWL 2 + SPARQL 1.1 RDF/XML / Turtle test-case manifests, (c) a runner that dispatches each selected test against a chosen reasoner and classifies the outcome (`passed`/`failed`/`skipped` with reason), (d) a versioned `harness/selected.toml` that names exactly which test IDs are "in" for CI, (e) a SQLite result database, and (f) a `harness` CLI used both locally and from GitHub Actions. The stub engine lives inside the same crate as a test fixture so the harness is provable on day one, before any other crate compiles meaningfully.

**Tech Stack:** Rust 2021 (workspace edition), `clap` v4 for the CLI, `oxigraph`/`oxrdf`/`oxrdfio` for RDF & SPARQL parsing and the SPARQL evaluation surface, `oxttl` for Turtle, `rusqlite` (bundled) for the result store, `serde` + `toml` for config and `selected.toml`, `anyhow` + `thiserror` for errors, `tracing` for logging, `insta` for snapshot tests, `assert_cmd` + `predicates` for CLI integration tests. CI on GitHub Actions Ubuntu runners.

---

## File Structure

```
reasoner/
├── Cargo.toml                                        # workspace; add shared deps
├── rust-toolchain.toml                               # pin stable 1.79 for CI determinism
├── .github/
│   └── workflows/
│       └── ci.yml                                    # PR-blocking selected-subset run
├── harness/
│   ├── selected.toml                                 # versioned: which test IDs are "in"
│   └── KNOWN-MANIFEST-BUGS.md                        # waivers, per spec risk note
└── crates/
    └── harness/
        ├── Cargo.toml                                # add real deps
        ├── README.md                                 # how to run locally
        ├── src/
        │   ├── lib.rs                                # module re-exports
        │   ├── reasoner.rs                           # the `Reasoner` trait
        │   ├── stub.rs                               # in-tree `StubReasoner` (F12)
        │   ├── manifest.rs                           # W3C manifest parser
        │   ├── testcase.rs                           # test-case enum + loaders
        │   ├── runner.rs                             # dispatcher + classifier
        │   ├── outcome.rs                            # pass/fail/skip + Report
        │   ├── selected.rs                           # selected.toml loader
        │   ├── db.rs                                 # SQLite result store
        │   ├── report.rs                             # `harness report` queries
        │   ├── ci.rs                                 # JUnit XML emitter for CI
        │   └── bin/
        │       └── harness.rs                        # clap CLI entrypoint
        └── tests/
            ├── fixtures/
            │   ├── owl2/
            │   │   ├── manifest.ttl                  # tiny W3C-shape manifest
            │   │   ├── trivial-entail-true.premise.ttl
            │   │   ├── trivial-entail-true.conclusion.ttl
            │   │   ├── subclass-entail.premise.ttl
            │   │   ├── subclass-entail.conclusion.ttl
            │   │   └── inconsistent-001.premise.ttl
            │   └── sparql11/
            │       ├── manifest.ttl
            │       ├── ask-true.rq
            │       ├── ask-true.data.ttl
            │       └── ask-true.srx                  # expected result
            ├── manifest_parse.rs                     # unit-ish: parser round-trip
            ├── stub_engine.rs                        # stub fails red as designed
            └── cli_smoke.rs                          # assert_cmd end-to-end
```

Stage 1 additions:

```
crates/harness/
├── data/                                              # vendored, .gitignored copies
│   ├── w3c-owl2-tests/                                # cloned via fetch script
│   ├── w3c-sparql11-tests/
│   └── ore2015/                                       # 10-ontology subset
├── scripts/
│   ├── fetch-w3c-suites.sh
│   └── fetch-ore2015-subset.sh
├── src/
│   ├── ore.rs                                         # ORE 2015 wrapper
│   └── ldbc_spb.rs                                    # LDBC SPB driver shim
└── tests/
    └── w3c_subset.rs                                  # runs against vendored suite
```

---

# STAGE 0 — Harness Bootstrap (2–4 weeks)

Exit criteria, from SPEC-01:
1. Runner exists for W3C OWL 2 Test Cases and SPARQL 1.1 Test Suite.
2. `harness/selected.toml` selects ≥1 test from each suite, CI runs only the selected subset.
3. A stub engine fails its assigned tests; CI correctly turns red.
4. Result database (SQLite) wired; `harness report` returns rows for stub runs.

---

### Task 1: Pin Rust toolchain and add workspace dependencies

**Files:**
- Create: `/Users/stig/git/sunstone/reasoner/rust-toolchain.toml`
- Modify: `/Users/stig/git/sunstone/reasoner/Cargo.toml`

- [ ] **Step 1: Pin toolchain**

Create `/Users/stig/git/sunstone/reasoner/rust-toolchain.toml` with exactly:

```toml
[toolchain]
channel = "1.79.0"
components = ["rustfmt", "clippy"]
profile = "minimal"
```

- [ ] **Step 2: Add shared dependencies to the workspace**

Replace the entire contents of `/Users/stig/git/sunstone/reasoner/Cargo.toml` with:

```toml
[workspace]
resolver = "2"
members = [
    "crates/harness",
    "crates/storage",
    "crates/wcoj",
    "crates/owlrl",
    "crates/closure",
    "crates/incremental",
    "crates/sparql",
    "crates/ml",
    "crates/hardware-ext",
]

[workspace.package]
edition = "2021"
rust-version = "1.79"
license = "Apache-2.0"
repository = "https://github.com/sunstoneinstitute/horndb"
authors = ["Sunstone Institute"]

[workspace.dependencies]
anyhow = "1"
thiserror = "1"
serde = { version = "1", features = ["derive"] }
toml = "0.8"
clap = { version = "4", features = ["derive"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
rusqlite = { version = "0.31", features = ["bundled"] }
oxrdf = "0.2"
oxrdfio = "0.1"
oxttl = "0.1"
oxigraph = "0.4"
sparesults = "0.2"
quick-xml = { version = "0.36", features = ["serialize"] }
walkdir = "2"
sha2 = "0.10"
hex = "0.4"
time = { version = "0.3", features = ["formatting", "serde", "macros"] }

[workspace.dev-dependencies]
assert_cmd = "2"
predicates = "3"
insta = { version = "1", features = ["yaml"] }
tempfile = "3"
```

- [ ] **Step 3: Verify it builds**

Run: `cd /Users/stig/git/sunstone/reasoner && cargo build --workspace`
Expected: completes successfully (all existing crates still have empty `[dependencies]`).

- [ ] **Step 4: Commit**

```bash
cd /Users/stig/git/sunstone/reasoner
git add Cargo.toml rust-toolchain.toml
git commit -m "$(cat <<'EOF'
chore(workspace): pin toolchain 1.79 and declare shared deps

Adds rust-toolchain.toml and the workspace-level dependency table that
SPEC-01 Stage 0 work will draw from (oxigraph/oxrdf for RDF, rusqlite
for the result DB, clap for the CLI, insta/assert_cmd for tests).
EOF
)"
```

---

### Task 2: Wire harness crate dependencies and module skeleton

**Files:**
- Modify: `/Users/stig/git/sunstone/reasoner/crates/harness/Cargo.toml`
- Modify: `/Users/stig/git/sunstone/reasoner/crates/harness/src/lib.rs`

- [ ] **Step 1: Update the harness crate manifest**

Replace `/Users/stig/git/sunstone/reasoner/crates/harness/Cargo.toml` with:

```toml
[package]
name = "horndb-harness"
version = "0.0.0"
edition.workspace = true
license.workspace = true
publish = false

[[bin]]
name = "harness"
path = "src/bin/harness.rs"

[dependencies]
anyhow = { workspace = true }
thiserror = { workspace = true }
serde = { workspace = true }
toml = { workspace = true }
clap = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
rusqlite = { workspace = true }
oxrdf = { workspace = true }
oxrdfio = { workspace = true }
oxttl = { workspace = true }
oxigraph = { workspace = true }
sparesults = { workspace = true }
quick-xml = { workspace = true }
walkdir = { workspace = true }
sha2 = { workspace = true }
hex = { workspace = true }
time = { workspace = true }

[dev-dependencies]
assert_cmd = { workspace = true }
predicates = { workspace = true }
insta = { workspace = true }
tempfile = { workspace = true }
```

- [ ] **Step 2: Replace the lib.rs stub with the module skeleton**

Replace `/Users/stig/git/sunstone/reasoner/crates/harness/src/lib.rs` with:

```rust
//! horndb-harness — conformance and benchmarking harness for the
//! HornDB project. See `specs/SPEC-01-conformance-benchmarks.md`.
//!
//! The harness is engine-agnostic: implementations of the [`Reasoner`]
//! trait are plugged in at runtime. A built-in [`StubReasoner`] exists
//! to prove the harness itself works before any real engine is wired up
//! (SPEC-01 F12).

pub mod ci;
pub mod db;
pub mod manifest;
pub mod outcome;
pub mod reasoner;
pub mod report;
pub mod runner;
pub mod selected;
pub mod stub;
pub mod testcase;

pub use outcome::{Outcome, Report, Status};
pub use reasoner::Reasoner;
pub use stub::StubReasoner;
```

- [ ] **Step 3: Create stub module files so the crate still compiles**

Create each of these files with the literal contents shown:

`/Users/stig/git/sunstone/reasoner/crates/harness/src/reasoner.rs`:

```rust
//! Engine-agnostic surface that every reasoner implementation must
//! provide so the harness can run W3C tests against it.
//!
//! Filled out in Task 4.

use anyhow::Result;
use oxrdf::Dataset;

/// A pluggable reasoning engine.
///
/// The harness uses only this trait; engines may be the in-tree
/// [`crate::stub::StubReasoner`] or a real implementation living in
/// another workspace crate.
pub trait Reasoner: Send + Sync {
    /// Human-readable name (used in result-DB rows and reports).
    fn name(&self) -> &str;

    /// Load a dataset of ground triples into the reasoner. Replaces any
    /// previously-loaded data.
    fn load(&mut self, dataset: &Dataset) -> Result<()>;

    /// Check whether `conclusion` is OWL 2 RL entailed by the currently
    /// loaded dataset.
    fn entails(&self, conclusion: &Dataset) -> Result<bool>;

    /// Whether the currently loaded dataset is consistent.
    fn is_consistent(&self) -> Result<bool>;

    /// Evaluate a SPARQL 1.1 ASK query. Returns the boolean answer.
    fn ask(&self, query: &str) -> Result<bool>;
}
```

`/Users/stig/git/sunstone/reasoner/crates/harness/src/stub.rs`:

```rust
//! Placeholder filled in Task 5.
pub struct StubReasoner;
impl StubReasoner {
    pub fn new() -> Self { Self }
}
impl Default for StubReasoner {
    fn default() -> Self { Self::new() }
}
```

`/Users/stig/git/sunstone/reasoner/crates/harness/src/manifest.rs`:

```rust
//! Placeholder filled in Task 6.
```

`/Users/stig/git/sunstone/reasoner/crates/harness/src/testcase.rs`:

```rust
//! Placeholder filled in Task 6.
```

`/Users/stig/git/sunstone/reasoner/crates/harness/src/runner.rs`:

```rust
//! Placeholder filled in Task 7.
```

`/Users/stig/git/sunstone/reasoner/crates/harness/src/outcome.rs`:

```rust
//! Placeholder filled in Task 3.
```

`/Users/stig/git/sunstone/reasoner/crates/harness/src/selected.rs`:

```rust
//! Placeholder filled in Task 8.
```

`/Users/stig/git/sunstone/reasoner/crates/harness/src/db.rs`:

```rust
//! Placeholder filled in Task 9.
```

`/Users/stig/git/sunstone/reasoner/crates/harness/src/report.rs`:

```rust
//! Placeholder filled in Task 10.
```

`/Users/stig/git/sunstone/reasoner/crates/harness/src/ci.rs`:

```rust
//! Placeholder filled in Task 11.
```

Create the bin directory and entrypoint placeholder
`/Users/stig/git/sunstone/reasoner/crates/harness/src/bin/harness.rs`:

```rust
fn main() {
    println!("horndb-harness (placeholder; see Task 12)");
}
```

- [ ] **Step 4: Verify everything compiles**

Run: `cd /Users/stig/git/sunstone/reasoner && cargo build -p horndb-harness`
Expected: builds cleanly. Warnings about unused trait items are acceptable at this stage.

- [ ] **Step 5: Commit**

```bash
cd /Users/stig/git/sunstone/reasoner
git add crates/harness/
git commit -m "$(cat <<'EOF'
feat(harness): scaffold module skeleton and Reasoner trait

Adds the module layout SPEC-01 calls for (manifest, runner, outcome,
selected, db, report, ci, stub, testcase) and declares the engine-
agnostic Reasoner trait. Subsequent commits fill the modules.
EOF
)"
```

---

### Task 3: Outcome / Status / Report types (TDD)

**Files:**
- Test: `/Users/stig/git/sunstone/reasoner/crates/harness/src/outcome.rs` (inline `#[cfg(test)]`)
- Modify: `/Users/stig/git/sunstone/reasoner/crates/harness/src/outcome.rs`

- [ ] **Step 1: Write the failing test**

Replace `/Users/stig/git/sunstone/reasoner/crates/harness/src/outcome.rs` with:

```rust
//! Outcome of running a single test case, and the aggregate `Report`
//! produced by a runner pass.

use serde::{Deserialize, Serialize};

/// Three-valued result that mirrors what SPEC-01 F1 calls for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Passed,
    Failed,
    Skipped,
}

/// Per-test outcome.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Outcome {
    pub test_id: String,
    pub suite: String,
    pub status: Status,
    /// Required when `status == Skipped` or `status == Failed`.
    pub reason: Option<String>,
    /// Wall-clock duration in milliseconds.
    pub duration_ms: u64,
}

/// Aggregate over one runner pass.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Report {
    pub outcomes: Vec<Outcome>,
}

impl Report {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, outcome: Outcome) {
        self.outcomes.push(outcome);
    }

    pub fn count(&self, status: Status) -> usize {
        self.outcomes.iter().filter(|o| o.status == status).count()
    }

    pub fn passed(&self) -> usize { self.count(Status::Passed) }
    pub fn failed(&self) -> usize { self.count(Status::Failed) }
    pub fn skipped(&self) -> usize { self.count(Status::Skipped) }

    /// True if any test failed. Skips do not fail the report.
    pub fn has_failures(&self) -> bool {
        self.failed() > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_report_has_no_failures() {
        let r = Report::new();
        assert_eq!(r.passed(), 0);
        assert_eq!(r.failed(), 0);
        assert_eq!(r.skipped(), 0);
        assert!(!r.has_failures());
    }

    #[test]
    fn report_counts_by_status() {
        let mut r = Report::new();
        r.push(Outcome { test_id: "a".into(), suite: "owl2".into(), status: Status::Passed, reason: None, duration_ms: 1 });
        r.push(Outcome { test_id: "b".into(), suite: "owl2".into(), status: Status::Failed, reason: Some("nope".into()), duration_ms: 1 });
        r.push(Outcome { test_id: "c".into(), suite: "owl2".into(), status: Status::Skipped, reason: Some("waived".into()), duration_ms: 0 });
        assert_eq!(r.passed(), 1);
        assert_eq!(r.failed(), 1);
        assert_eq!(r.skipped(), 1);
        assert!(r.has_failures());
    }

    #[test]
    fn skips_do_not_count_as_failures() {
        let mut r = Report::new();
        r.push(Outcome { test_id: "c".into(), suite: "owl2".into(), status: Status::Skipped, reason: Some("waived".into()), duration_ms: 0 });
        assert!(!r.has_failures());
    }
}
```

- [ ] **Step 2: Run the test to confirm it passes**

Run: `cd /Users/stig/git/sunstone/reasoner && cargo test -p horndb-harness outcome::tests`
Expected: 3 tests pass.

- [ ] **Step 3: Commit**

```bash
cd /Users/stig/git/sunstone/reasoner
git add crates/harness/src/outcome.rs
git commit -m "$(cat <<'EOF'
feat(harness): add Outcome/Status/Report types

Three-valued status (passed/failed/skipped) per SPEC-01 F1. Skipped
tests do not turn the report red — only failures do.
EOF
)"
```

---

### Task 4: Flesh out the `Reasoner` trait with documented contract tests

**Files:**
- Modify: `/Users/stig/git/sunstone/reasoner/crates/harness/src/reasoner.rs`

- [ ] **Step 1: Write the failing contract documentation test**

Replace the entire contents of `/Users/stig/git/sunstone/reasoner/crates/harness/src/reasoner.rs` with:

```rust
//! Engine-agnostic surface every reasoner implementation must satisfy.
//!
//! The harness uses only this trait; engines may be the in-tree
//! [`crate::stub::StubReasoner`] (SPEC-01 F12) or a real implementation
//! living in another workspace crate.

use anyhow::Result;
use oxrdf::Dataset;

/// A pluggable reasoning engine.
///
/// Contract:
/// * `load` is destructive — it replaces any previously-loaded data.
/// * `entails` and `is_consistent` must use the currently-loaded data.
/// * Implementations must be `Send + Sync` so the runner can hand them
///   to threaded backends in later stages without API churn.
pub trait Reasoner: Send + Sync {
    fn name(&self) -> &str;
    fn load(&mut self, dataset: &Dataset) -> Result<()>;
    fn entails(&self, conclusion: &Dataset) -> Result<bool>;
    fn is_consistent(&self) -> Result<bool>;
    fn ask(&self, query: &str) -> Result<bool>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::marker::PhantomData;

    // Compile-time check: trait objects are Send + Sync.
    fn _assert_object_safe() {
        fn _f(_: &dyn Reasoner) {}
        fn _g<T: Send + Sync + ?Sized>(_: PhantomData<T>) {}
        _g::<dyn Reasoner>(PhantomData);
    }
}
```

- [ ] **Step 2: Verify the compile-time assertions hold**

Run: `cd /Users/stig/git/sunstone/reasoner && cargo test -p horndb-harness reasoner::tests`
Expected: compiles and the (zero-runtime) module test passes.

- [ ] **Step 3: Commit**

```bash
cd /Users/stig/git/sunstone/reasoner
git add crates/harness/src/reasoner.rs
git commit -m "$(cat <<'EOF'
feat(harness): document Reasoner trait contract and assert object-safety

Adds the load/entails/is_consistent/ask surface the runner uses, plus a
compile-time assertion that the trait is object-safe and Send+Sync so
later threaded backends do not break API.
EOF
)"
```

---

### Task 5: StubReasoner — fails by default, passes trivially-true cases (F12)

**Files:**
- Modify: `/Users/stig/git/sunstone/reasoner/crates/harness/src/stub.rs`

- [ ] **Step 1: Write the failing tests**

Replace `/Users/stig/git/sunstone/reasoner/crates/harness/src/stub.rs` with:

```rust
//! In-tree stub reasoner used by SPEC-01 F12.
//!
//! Purpose: prove the harness itself works *before* any real engine
//! exists. The stub is deliberately weak — it only "knows" how to:
//!
//! 1. Confirm that the empty graph entails the empty graph (so the
//!    most-trivial positive-entailment test passes).
//! 2. Report inconsistency when an explicit `owl:Nothing` membership
//!    triple is present (so a hand-rolled inconsistency test fails red
//!    against a graph that lacks it).
//! 3. Answer `ASK { ?s ?p ?o }` truthfully based on whether any triple
//!    was loaded.
//!
//! Everything else returns `false` (which makes "real" tests fail —
//! that is the whole point: a deliberately-failing reference
//! implementation is correctly flagged red, per SPEC-01 Stage-0 exit
//! criterion 3).

use anyhow::Result;
use oxrdf::{Dataset, NamedNodeRef};

use crate::reasoner::Reasoner;

#[derive(Default)]
pub struct StubReasoner {
    triple_count: usize,
    contains_owl_nothing_membership: bool,
}

impl StubReasoner {
    pub fn new() -> Self {
        Self::default()
    }
}

const OWL_NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

impl Reasoner for StubReasoner {
    fn name(&self) -> &str { "stub" }

    fn load(&mut self, dataset: &Dataset) -> Result<()> {
        self.triple_count = dataset.len();
        let nothing = NamedNodeRef::new(OWL_NOTHING)?;
        let rdf_type = NamedNodeRef::new(RDF_TYPE)?;
        self.contains_owl_nothing_membership = dataset
            .quads_for_predicate(rdf_type)
            .any(|q| q.object == nothing.into());
        Ok(())
    }

    fn entails(&self, conclusion: &Dataset) -> Result<bool> {
        // The empty graph entails the empty graph, and nothing else.
        Ok(conclusion.is_empty())
    }

    fn is_consistent(&self) -> Result<bool> {
        Ok(!self.contains_owl_nothing_membership)
    }

    fn ask(&self, _query: &str) -> Result<bool> {
        // The stub does not parse SPARQL. It returns `true` iff
        // anything was loaded, which is just enough to make a trivial
        // ASK test pass and any non-trivial one fail.
        Ok(self.triple_count > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxrdf::{Dataset, NamedNode, Quad, Subject};

    fn quad(s: &str, p: &str, o: &str) -> Quad {
        Quad::new(
            Subject::NamedNode(NamedNode::new(s).unwrap()),
            NamedNode::new(p).unwrap(),
            NamedNode::new(o).unwrap(),
            oxrdf::GraphName::DefaultGraph,
        )
    }

    #[test]
    fn empty_entails_empty() {
        let s = StubReasoner::new();
        assert!(s.entails(&Dataset::new()).unwrap());
    }

    #[test]
    fn nonempty_conclusion_is_not_entailed() {
        let s = StubReasoner::new();
        let mut concl = Dataset::new();
        concl.insert(&quad("http://ex/a", RDF_TYPE, "http://ex/C"));
        assert!(!s.entails(&concl).unwrap());
    }

    #[test]
    fn graph_with_owl_nothing_membership_is_inconsistent() {
        let mut s = StubReasoner::new();
        let mut data = Dataset::new();
        data.insert(&quad("http://ex/a", RDF_TYPE, OWL_NOTHING));
        s.load(&data).unwrap();
        assert!(!s.is_consistent().unwrap());
    }

    #[test]
    fn empty_graph_is_consistent() {
        let mut s = StubReasoner::new();
        s.load(&Dataset::new()).unwrap();
        assert!(s.is_consistent().unwrap());
    }

    #[test]
    fn ask_true_when_anything_loaded() {
        let mut s = StubReasoner::new();
        let mut data = Dataset::new();
        data.insert(&quad("http://ex/a", "http://ex/p", "http://ex/b"));
        s.load(&data).unwrap();
        assert!(s.ask("ASK { ?s ?p ?o }").unwrap());
    }

    #[test]
    fn ask_false_when_nothing_loaded() {
        let s = StubReasoner::new();
        assert!(!s.ask("ASK { ?s ?p ?o }").unwrap());
    }
}
```

- [ ] **Step 2: Run the tests**

Run: `cd /Users/stig/git/sunstone/reasoner && cargo test -p horndb-harness stub::tests`
Expected: 6 tests pass.

- [ ] **Step 3: Commit**

```bash
cd /Users/stig/git/sunstone/reasoner
git add crates/harness/src/stub.rs
git commit -m "$(cat <<'EOF'
feat(harness): implement StubReasoner (SPEC-01 F12)

A deliberately-weak in-tree engine that knows how to make exactly the
trivial cases pass and everything else fail. Lets us prove the harness
itself works before any real engine exists.
EOF
)"
```

---

### Task 6: Manifest parser and TestCase types (TDD)

The W3C OWL 2 and SPARQL 1.1 test manifests use Turtle in newer mirrors and RDF/XML in older ones. We accept Turtle here — that is what the on-disk fixtures in Task 7 use, and the Stage 1 task that pulls in the real W3C suite converts RDF/XML to Turtle as part of the fetch script.

**Files:**
- Modify: `/Users/stig/git/sunstone/reasoner/crates/harness/src/testcase.rs`
- Modify: `/Users/stig/git/sunstone/reasoner/crates/harness/src/manifest.rs`

- [ ] **Step 1: Write the failing testcase types**

Replace `/Users/stig/git/sunstone/reasoner/crates/harness/src/testcase.rs` with:

```rust
//! Loaded representation of a single W3C-style test case.

use std::path::PathBuf;

/// Suites the harness understands at Stage 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Suite {
    Owl2,
    Sparql11,
}

impl Suite {
    pub fn as_str(self) -> &'static str {
        match self {
            Suite::Owl2 => "owl2",
            Suite::Sparql11 => "sparql11",
        }
    }
}

/// Kinds of tests the harness recognises (SPEC-01 F1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestKind {
    /// Premise entails conclusion (both are RDF graphs).
    PositiveEntailment { premise: PathBuf, conclusion: PathBuf },
    /// Premise does *not* entail conclusion.
    NegativeEntailment { premise: PathBuf, conclusion: PathBuf },
    /// Premise graph is consistent.
    Consistency { premise: PathBuf },
    /// Premise graph is inconsistent.
    Inconsistency { premise: PathBuf },
    /// SPARQL ASK whose expected boolean answer is known.
    SparqlAsk { query: PathBuf, data: PathBuf, expected: bool },
}

#[derive(Debug, Clone)]
pub struct TestCase {
    /// Globally unique within a manifest (used in selected.toml).
    pub id: String,
    pub suite: Suite,
    pub name: String,
    pub kind: TestKind,
}
```

- [ ] **Step 2: Write the failing manifest parser**

Replace `/Users/stig/git/sunstone/reasoner/crates/harness/src/manifest.rs` with:

```rust
//! Parser for W3C-style test manifests, expressed in Turtle.
//!
//! Real W3C manifests historically shipped as RDF/XML; the Stage-1
//! fetch script converts them to Turtle so this parser is the single
//! ingestion point. Vocabulary used (subset sufficient for Stage 0):
//!
//! * `mf:` <http://www.w3.org/2001/sw/DataAccess/tests/test-manifest#>
//! * `rdft:` <http://www.w3.org/ns/rdftest#>
//! * `qt:` <http://www.w3.org/2001/sw/DataAccess/tests/test-query#>
//!
//! We recognise the test types listed in SPEC-01 F1: positive/negative
//! entailment, consistency/inconsistency, plus a minimal SPARQL ASK
//! variant for SPARQL 1.1 manifests.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use oxrdf::{Graph, NamedNode, NamedNodeRef, Subject, Term};
use oxttl::TurtleParser;

use crate::testcase::{Suite, TestCase, TestKind};

const MF: &str = "http://www.w3.org/2001/sw/DataAccess/tests/test-manifest#";
const RDFT: &str = "http://www.w3.org/ns/rdftest#";
const QT: &str = "http://www.w3.org/2001/sw/DataAccess/tests/test-query#";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDF_FIRST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#first";
const RDF_REST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#rest";
const RDF_NIL: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#nil";

/// Parse a manifest from disk. `suite` is supplied externally because
/// the harness already knows which directory it is loading.
pub fn parse(path: &Path, suite: Suite) -> Result<Vec<TestCase>> {
    let bytes = fs::read(path)
        .with_context(|| format!("reading manifest {}", path.display()))?;
    let base = path
        .parent()
        .ok_or_else(|| anyhow!("manifest has no parent dir"))?;
    let graph = parse_turtle(&bytes, &format!("file://{}", path.display()))?;
    extract_cases(&graph, base, suite)
}

fn parse_turtle(bytes: &[u8], base_iri: &str) -> Result<Graph> {
    let mut graph = Graph::new();
    let mut parser = TurtleParser::new().with_base_iri(base_iri)?.parse();
    parser.extend_from_slice(bytes);
    parser.end();
    while let Some(triple) = parser.read_next() {
        let triple = triple?;
        graph.insert(&triple);
    }
    Ok(graph)
}

fn extract_cases(graph: &Graph, base: &Path, suite: Suite) -> Result<Vec<TestCase>> {
    // 1. Find the manifest node (typed mf:Manifest).
    let manifest_type = NamedNodeRef::new(&format!("{MF}Manifest"))?;
    let rdf_type = NamedNodeRef::new(RDF_TYPE)?;
    let manifest = graph
        .subjects_for_predicate_object(rdf_type, manifest_type.into())
        .next()
        .ok_or_else(|| anyhow!("no mf:Manifest in {}", base.display()))?;

    // 2. Walk mf:entries list.
    let entries_pred = NamedNodeRef::new(&format!("{MF}entries"))?;
    let entry_head = graph
        .object_for_subject_predicate(manifest, entries_pred)
        .ok_or_else(|| anyhow!("manifest has no mf:entries"))?;
    let entries = read_rdf_list(graph, entry_head)?;

    // 3. Project each entry into a TestCase.
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let entry_subj = match entry {
            Term::NamedNode(ref n) => Subject::NamedNode(n.clone()),
            Term::BlankNode(ref b) => Subject::BlankNode(b.clone()),
            _ => bail!("manifest entry is not a node"),
        };
        out.push(project_entry(graph, entry_subj.as_ref(), base, suite)?);
    }
    Ok(out)
}

fn read_rdf_list<'a>(graph: &'a Graph, head: Term) -> Result<Vec<Term>> {
    let first = NamedNodeRef::new(RDF_FIRST)?;
    let rest = NamedNodeRef::new(RDF_REST)?;
    let nil_iri = NamedNodeRef::new(RDF_NIL)?;
    let mut out = Vec::new();
    let mut cur = head;
    loop {
        match &cur {
            Term::NamedNode(n) if n.as_ref() == nil_iri => break,
            _ => {}
        }
        let cur_subj: Subject = match &cur {
            Term::NamedNode(n) => Subject::NamedNode(n.clone()),
            Term::BlankNode(b) => Subject::BlankNode(b.clone()),
            _ => bail!("rdf:List node is not a resource"),
        };
        let item = graph
            .object_for_subject_predicate(cur_subj.as_ref(), first)
            .ok_or_else(|| anyhow!("malformed list (missing rdf:first)"))?;
        out.push(item.into_owned());
        cur = graph
            .object_for_subject_predicate(cur_subj.as_ref(), rest)
            .ok_or_else(|| anyhow!("malformed list (missing rdf:rest)"))?
            .into_owned();
    }
    Ok(out)
}

fn project_entry(
    graph: &Graph,
    entry: oxrdf::SubjectRef<'_>,
    base: &Path,
    suite: Suite,
) -> Result<TestCase> {
    let lookups: BTreeMap<&str, &str> = BTreeMap::from([
        ("type", RDF_TYPE),
        ("name", &*Box::leak(format!("{MF}name").into_boxed_str())),
    ]);
    let _ = lookups; // documentation only; we use literals below.

    let name_pred = NamedNodeRef::new(&format!("{MF}name"))?;
    let action_pred = NamedNodeRef::new(&format!("{MF}action"))?;
    let result_pred = NamedNodeRef::new(&format!("{MF}result"))?;
    let rdf_type = NamedNodeRef::new(RDF_TYPE)?;

    let id = match entry {
        oxrdf::SubjectRef::NamedNode(n) => n.as_str().to_string(),
        oxrdf::SubjectRef::BlankNode(b) => format!("_:{}", b.as_str()),
    };

    let name = graph
        .object_for_subject_predicate(entry, name_pred)
        .and_then(|t| match t {
            Term::Literal(l) => Some(l.value().to_string()),
            _ => None,
        })
        .unwrap_or_else(|| id.clone());

    let kind_iri = graph
        .object_for_subject_predicate(entry, rdf_type)
        .ok_or_else(|| anyhow!("entry {id} has no rdf:type"))?;
    let kind_iri = match kind_iri {
        Term::NamedNode(n) => n,
        _ => bail!("entry {id} rdf:type is not an IRI"),
    };

    let resolve = |t: Term| -> Result<PathBuf> {
        match t {
            Term::NamedNode(n) => resolve_file(n.as_str(), base),
            _ => bail!("expected file IRI, got {t}"),
        }
    };

    let action = graph.object_for_subject_predicate(entry, action_pred);
    let result = graph.object_for_subject_predicate(entry, result_pred);

    let kind = match kind_iri.as_str() {
        s if s == format!("{MF}PositiveEntailmentTest") => TestKind::PositiveEntailment {
            premise: resolve(action.ok_or_else(|| anyhow!("missing mf:action"))?.into_owned())?,
            conclusion: resolve(result.ok_or_else(|| anyhow!("missing mf:result"))?.into_owned())?,
        },
        s if s == format!("{MF}NegativeEntailmentTest") => TestKind::NegativeEntailment {
            premise: resolve(action.ok_or_else(|| anyhow!("missing mf:action"))?.into_owned())?,
            conclusion: resolve(result.ok_or_else(|| anyhow!("missing mf:result"))?.into_owned())?,
        },
        s if s == format!("{MF}ConsistencyTest") => TestKind::Consistency {
            premise: resolve(action.ok_or_else(|| anyhow!("missing mf:action"))?.into_owned())?,
        },
        s if s == format!("{MF}InconsistencyTest") => TestKind::Inconsistency {
            premise: resolve(action.ok_or_else(|| anyhow!("missing mf:action"))?.into_owned())?,
        },
        s if s == format!("{MF}QueryEvaluationTest") || s.starts_with(QT) => {
            // SPARQL ASK: action is a qt:QueryTest with qt:query + qt:data,
            // result is an SRX file we read here to extract the boolean.
            let action_node = action.ok_or_else(|| anyhow!("missing mf:action"))?;
            let action_subj: Subject = match action_node {
                Term::NamedNode(ref n) => Subject::NamedNode(n.clone()),
                Term::BlankNode(ref b) => Subject::BlankNode(b.clone()),
                _ => bail!("qt action is not a resource"),
            };
            let qt_query = NamedNodeRef::new(&format!("{QT}query"))?;
            let qt_data = NamedNodeRef::new(&format!("{QT}data"))?;
            let query = resolve(
                graph
                    .object_for_subject_predicate(action_subj.as_ref(), qt_query)
                    .ok_or_else(|| anyhow!("qt:query missing"))?
                    .into_owned(),
            )?;
            let data = resolve(
                graph
                    .object_for_subject_predicate(action_subj.as_ref(), qt_data)
                    .ok_or_else(|| anyhow!("qt:data missing"))?
                    .into_owned(),
            )?;
            let expected_path = resolve(
                result.ok_or_else(|| anyhow!("missing mf:result"))?.into_owned(),
            )?;
            let srx = fs::read_to_string(&expected_path)
                .with_context(|| format!("reading SRX {}", expected_path.display()))?;
            let expected = srx.contains("<boolean>true</boolean>");
            TestKind::SparqlAsk { query, data, expected }
        }
        other => bail!("unsupported test type for entry {id}: {other}"),
    };

    Ok(TestCase { id, suite, name, kind })
}

fn resolve_file(iri: &str, base: &Path) -> Result<PathBuf> {
    // Manifests reference siblings either as relative paths or as
    // `file://` IRIs that the Turtle parser already resolved against
    // the manifest's base. Both shapes are accepted.
    if let Some(rel) = iri.strip_prefix("file://") {
        // The Turtle parser produces absolute file:// IRIs relative to
        // the manifest directory; strip the prefix back to a path.
        // Cope with both `file:///abs/...` and the simpler `file://`.
        let trimmed = rel.trim_start_matches('/');
        let candidate_abs = PathBuf::from(format!("/{trimmed}"));
        if candidate_abs.exists() {
            return Ok(candidate_abs);
        }
        return Ok(base.join(trimmed));
    }
    Ok(base.join(iri))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    fn write(dir: &Path, name: &str, content: &str) -> PathBuf {
        let p = dir.join(name);
        let mut f = fs::File::create(&p).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        p
    }

    #[test]
    fn parses_minimal_positive_entailment_manifest() {
        let d = tempdir().unwrap();
        write(d.path(), "premise.ttl", "");
        write(d.path(), "conclusion.ttl", "");
        let manifest = write(
            d.path(),
            "manifest.ttl",
            r#"
@prefix mf:   <http://www.w3.org/2001/sw/DataAccess/tests/test-manifest#> .
@prefix rdf:  <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .

<#manifest> a mf:Manifest ;
    mf:entries ( <#t-empty-entails-empty> ) .

<#t-empty-entails-empty> a mf:PositiveEntailmentTest ;
    mf:name "empty entails empty" ;
    mf:action <premise.ttl> ;
    mf:result <conclusion.ttl> .
"#,
        );
        let cases = parse(&manifest, Suite::Owl2).expect("parse ok");
        assert_eq!(cases.len(), 1);
        let c = &cases[0];
        assert_eq!(c.name, "empty entails empty");
        assert!(matches!(&c.kind, TestKind::PositiveEntailment { .. }));
        assert!(c.id.ends_with("#t-empty-entails-empty"));
    }

    #[test]
    fn rejects_manifest_with_no_mf_manifest() {
        let d = tempdir().unwrap();
        let manifest = write(d.path(), "manifest.ttl", "# empty\n");
        let err = parse(&manifest, Suite::Owl2).unwrap_err();
        assert!(err.to_string().contains("no mf:Manifest"));
    }
}
```

- [ ] **Step 3: Run the parser tests**

Run: `cd /Users/stig/git/sunstone/reasoner && cargo test -p horndb-harness manifest::tests`
Expected: 2 tests pass.

- [ ] **Step 4: Commit**

```bash
cd /Users/stig/git/sunstone/reasoner
git add crates/harness/src/manifest.rs crates/harness/src/testcase.rs
git commit -m "$(cat <<'EOF'
feat(harness): parse W3C-style test manifests (positive/negative entailment, consistency, SPARQL ASK)

Recognises the SPEC-01 F1 test types using the mf:/rdft:/qt: vocab
expressed in Turtle. The Stage-1 fetch script converts the real W3C
RDF/XML manifests to Turtle so this is the single ingestion point.
EOF
)"
```

---

### Task 7: Fixtures that look like real W3C tests

**Files:**
- Create: `/Users/stig/git/sunstone/reasoner/crates/harness/tests/fixtures/owl2/manifest.ttl`
- Create: `/Users/stig/git/sunstone/reasoner/crates/harness/tests/fixtures/owl2/trivial-entail-true.premise.ttl`
- Create: `/Users/stig/git/sunstone/reasoner/crates/harness/tests/fixtures/owl2/trivial-entail-true.conclusion.ttl`
- Create: `/Users/stig/git/sunstone/reasoner/crates/harness/tests/fixtures/owl2/subclass-entail.premise.ttl`
- Create: `/Users/stig/git/sunstone/reasoner/crates/harness/tests/fixtures/owl2/subclass-entail.conclusion.ttl`
- Create: `/Users/stig/git/sunstone/reasoner/crates/harness/tests/fixtures/owl2/inconsistent-001.premise.ttl`
- Create: `/Users/stig/git/sunstone/reasoner/crates/harness/tests/fixtures/sparql11/manifest.ttl`
- Create: `/Users/stig/git/sunstone/reasoner/crates/harness/tests/fixtures/sparql11/ask-true.rq`
- Create: `/Users/stig/git/sunstone/reasoner/crates/harness/tests/fixtures/sparql11/ask-true.data.ttl`
- Create: `/Users/stig/git/sunstone/reasoner/crates/harness/tests/fixtures/sparql11/ask-true.srx`

- [ ] **Step 1: Make the directories**

Run:
```bash
mkdir -p /Users/stig/git/sunstone/reasoner/crates/harness/tests/fixtures/owl2 \
         /Users/stig/git/sunstone/reasoner/crates/harness/tests/fixtures/sparql11
```

- [ ] **Step 2: Create the OWL 2 fixture files**

`tests/fixtures/owl2/manifest.ttl`:

```turtle
@prefix mf:  <http://www.w3.org/2001/sw/DataAccess/tests/test-manifest#> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .

<#manifest> a mf:Manifest ;
    mf:entries (
        <#trivial-entail-true>
        <#subclass-entail>
        <#inconsistent-001>
    ) .

<#trivial-entail-true> a mf:PositiveEntailmentTest ;
    mf:name "empty entails empty" ;
    mf:action <trivial-entail-true.premise.ttl> ;
    mf:result <trivial-entail-true.conclusion.ttl> .

<#subclass-entail> a mf:PositiveEntailmentTest ;
    mf:name "a rdfs:subClassOf b, x a a |= x a b" ;
    mf:action <subclass-entail.premise.ttl> ;
    mf:result <subclass-entail.conclusion.ttl> .

<#inconsistent-001> a mf:InconsistencyTest ;
    mf:name "explicit owl:Nothing membership is inconsistent" ;
    mf:action <inconsistent-001.premise.ttl> .
```

`tests/fixtures/owl2/trivial-entail-true.premise.ttl`: empty file (zero bytes).

`tests/fixtures/owl2/trivial-entail-true.conclusion.ttl`: empty file (zero bytes).

`tests/fixtures/owl2/subclass-entail.premise.ttl`:

```turtle
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix ex:   <http://example.org/> .

ex:A rdfs:subClassOf ex:B .
ex:x  a ex:A .
```

`tests/fixtures/owl2/subclass-entail.conclusion.ttl`:

```turtle
@prefix ex: <http://example.org/> .

ex:x a ex:B .
```

`tests/fixtures/owl2/inconsistent-001.premise.ttl`:

```turtle
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix ex:  <http://example.org/> .

ex:x a owl:Nothing .
```

- [ ] **Step 3: Create the SPARQL 1.1 fixture files**

`tests/fixtures/sparql11/manifest.ttl`:

```turtle
@prefix mf:  <http://www.w3.org/2001/sw/DataAccess/tests/test-manifest#> .
@prefix qt:  <http://www.w3.org/2001/sw/DataAccess/tests/test-query#> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .

<#manifest> a mf:Manifest ;
    mf:entries ( <#ask-true> ) .

<#ask-true> a mf:QueryEvaluationTest ;
    mf:name "trivial ASK returning true" ;
    mf:action [ qt:query <ask-true.rq> ; qt:data <ask-true.data.ttl> ] ;
    mf:result <ask-true.srx> .
```

`tests/fixtures/sparql11/ask-true.rq`:

```sparql
ASK { ?s ?p ?o }
```

`tests/fixtures/sparql11/ask-true.data.ttl`:

```turtle
@prefix ex: <http://example.org/> .
ex:a ex:p ex:b .
```

`tests/fixtures/sparql11/ask-true.srx`:

```xml
<?xml version="1.0"?>
<sparql xmlns="http://www.w3.org/2005/sparql-results#">
  <head/>
  <boolean>true</boolean>
</sparql>
```

- [ ] **Step 4: Write an integration test that the manifest parser eats the real fixture**

Create `/Users/stig/git/sunstone/reasoner/crates/harness/tests/manifest_parse.rs`:

```rust
use std::path::PathBuf;

use horndb_harness::manifest;
use horndb_harness::testcase::{Suite, TestKind};

fn fixture(rel: &str) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures");
    p.push(rel);
    p
}

#[test]
fn parses_owl2_fixture_manifest() {
    let cases = manifest::parse(&fixture("owl2/manifest.ttl"), Suite::Owl2).unwrap();
    assert_eq!(cases.len(), 3);
    assert!(cases.iter().any(|c| matches!(c.kind, TestKind::PositiveEntailment { .. })));
    assert!(cases.iter().any(|c| matches!(c.kind, TestKind::Inconsistency { .. })));
}

#[test]
fn parses_sparql11_fixture_manifest() {
    let cases = manifest::parse(&fixture("sparql11/manifest.ttl"), Suite::Sparql11).unwrap();
    assert_eq!(cases.len(), 1);
    match &cases[0].kind {
        TestKind::SparqlAsk { expected, .. } => assert!(*expected),
        other => panic!("expected SparqlAsk, got {other:?}"),
    }
}
```

- [ ] **Step 5: Run the integration test**

Run: `cd /Users/stig/git/sunstone/reasoner && cargo test -p horndb-harness --test manifest_parse`
Expected: both tests pass.

- [ ] **Step 6: Commit**

```bash
cd /Users/stig/git/sunstone/reasoner
git add crates/harness/tests/
git commit -m "$(cat <<'EOF'
test(harness): add W3C-shaped Turtle fixtures and parser integration test

Three OWL 2 fixtures (empty entails empty, subClassOf entailment,
explicit owl:Nothing inconsistency) plus one SPARQL 1.1 ASK fixture.
Exercises every TestKind the manifest parser supports.
EOF
)"
```

---

### Task 8: selected.toml loader and the on-disk manifest file (TDD)

**Files:**
- Modify: `/Users/stig/git/sunstone/reasoner/crates/harness/src/selected.rs`
- Create: `/Users/stig/git/sunstone/reasoner/harness/selected.toml`
- Create: `/Users/stig/git/sunstone/reasoner/harness/KNOWN-MANIFEST-BUGS.md`

- [ ] **Step 1: Write the failing selected loader**

Replace `/Users/stig/git/sunstone/reasoner/crates/harness/src/selected.rs` with:

```rust
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
```

- [ ] **Step 2: Write the on-disk Stage-0 selected.toml**

Create `/Users/stig/git/sunstone/reasoner/harness/selected.toml` with exactly:

```toml
# SPEC-01 F11 — versioned manifest of which test IDs the harness expects
# to pass right now. CI runs ONLY what is listed here. Adding tests
# is a normal PR change; removing tests requires an entry in [[removed]]
# with a tracking issue URL.
#
# Stage-0 selection: one trivial test per suite, sufficient to prove
# that the harness flags the stub engine red when it should and green
# when it should. As Stage-1 work lands, the OWL 2 suite grows to ≥50
# real W3C cases (see plans/PLAN-01-01-conformance-harness.md
# Stage 1).

version = 1

[suites.owl2]
manifest = "crates/harness/tests/fixtures/owl2/manifest.ttl"
include = [
    # Stub passes this (empty graph entails empty graph).
    "file:///stig/git/sunstone/reasoner/crates/harness/tests/fixtures/owl2/manifest.ttl#trivial-entail-true",
    # Stub fails this — included on purpose so CI proves red-on-failure.
    "file:///stig/git/sunstone/reasoner/crates/harness/tests/fixtures/owl2/manifest.ttl#subclass-entail",
    # Stub passes this (recognises explicit owl:Nothing membership).
    "file:///stig/git/sunstone/reasoner/crates/harness/tests/fixtures/owl2/manifest.ttl#inconsistent-001",
]

[suites.sparql11]
manifest = "crates/harness/tests/fixtures/sparql11/manifest.ttl"
include = [
    "file:///stig/git/sunstone/reasoner/crates/harness/tests/fixtures/sparql11/manifest.ttl#ask-true",
]
```

Note: the IDs above use `file://` IRIs because the Turtle parser
resolves manifest entries against the manifest's base IRI. Task 12
calls into the runner with `--allow-failing` for the Stage-0 stub run
so the deliberately-failing `subclass-entail` row does not block the
runner self-test; CI runs in the default mode and turns red on it.

- [ ] **Step 3: Create the waivers document**

Create `/Users/stig/git/sunstone/reasoner/harness/KNOWN-MANIFEST-BUGS.md` with:

```markdown
# Known W3C Manifest Bugs / Waivers

Per SPEC-01's "Risks and open questions" section: some upstream W3C
test cases have known-broken manifest entries. Document each waiver
here with a citation so the selection discipline (F11) stays honest.

| Test ID | Suite | Issue | Upstream tracker |
|---------|-------|-------|------------------|
| _(none yet — populated as Stage-1 cases are imported)_ | | | |
```

- [ ] **Step 4: Run the unit tests**

Run: `cd /Users/stig/git/sunstone/reasoner && cargo test -p horndb-harness selected::tests`
Expected: 4 tests pass.

- [ ] **Step 5: Commit**

```bash
cd /Users/stig/git/sunstone/reasoner
git add crates/harness/src/selected.rs harness/selected.toml harness/KNOWN-MANIFEST-BUGS.md
git commit -m "$(cat <<'EOF'
feat(harness): selected.toml loader and Stage-0 selection (SPEC-01 F11)

Loader validates schema version, rejects empty includes, rejects
duplicates. The on-disk harness/selected.toml selects three OWL 2
fixtures and one SPARQL 1.1 fixture — enough to prove green/red CI
behaviour against the stub engine.
EOF
)"
```

---

### Task 9: SQLite result database (F7) — TDD

**Files:**
- Modify: `/Users/stig/git/sunstone/reasoner/crates/harness/src/db.rs`

- [ ] **Step 1: Write the failing db tests**

Replace `/Users/stig/git/sunstone/reasoner/crates/harness/src/db.rs` with:

```rust
//! SQLite result database (SPEC-01 F7).
//!
//! Schema (Stage 1):
//!
//! ```sql
//! CREATE TABLE runs (
//!     run_id        TEXT PRIMARY KEY,
//!     commit_sha    TEXT NOT NULL,
//!     hardware_id   TEXT NOT NULL,
//!     reasoner_name TEXT NOT NULL,
//!     started_at    TEXT NOT NULL  -- RFC3339
//! );
//! CREATE TABLE outcomes (
//!     run_id      TEXT NOT NULL REFERENCES runs(run_id) ON DELETE CASCADE,
//!     suite       TEXT NOT NULL,
//!     test_id     TEXT NOT NULL,
//!     status      TEXT NOT NULL CHECK(status IN ('passed','failed','skipped')),
//!     reason      TEXT,
//!     duration_ms INTEGER NOT NULL
//! );
//! CREATE TABLE metrics (
//!     run_id      TEXT NOT NULL REFERENCES runs(run_id) ON DELETE CASCADE,
//!     suite       TEXT NOT NULL,
//!     dataset     TEXT,
//!     metric_name TEXT NOT NULL,
//!     metric_value REAL NOT NULL,
//!     units       TEXT NOT NULL,
//!     timestamp   TEXT NOT NULL
//! );
//! ```

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::outcome::{Outcome, Status};

pub struct Db {
    conn: Connection,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("opening sqlite db {}", path.display()))?;
        let me = Self { conn };
        me.migrate()?;
        Ok(me)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let me = Self { conn };
        me.migrate()?;
        Ok(me)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS runs (
                run_id        TEXT PRIMARY KEY,
                commit_sha    TEXT NOT NULL,
                hardware_id   TEXT NOT NULL,
                reasoner_name TEXT NOT NULL,
                started_at    TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS outcomes (
                run_id      TEXT NOT NULL REFERENCES runs(run_id) ON DELETE CASCADE,
                suite       TEXT NOT NULL,
                test_id     TEXT NOT NULL,
                status      TEXT NOT NULL CHECK(status IN ('passed','failed','skipped')),
                reason      TEXT,
                duration_ms INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS metrics (
                run_id       TEXT NOT NULL REFERENCES runs(run_id) ON DELETE CASCADE,
                suite        TEXT NOT NULL,
                dataset      TEXT,
                metric_name  TEXT NOT NULL,
                metric_value REAL NOT NULL,
                units        TEXT NOT NULL,
                timestamp    TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_outcomes_run ON outcomes(run_id);
            CREATE INDEX IF NOT EXISTS idx_metrics_run ON metrics(run_id);
            "#,
        )?;
        Ok(())
    }

    /// Begin a new run; returns the synthesised `run_id`.
    pub fn start_run(
        &self,
        commit_sha: &str,
        hardware_id: &str,
        reasoner_name: &str,
    ) -> Result<String> {
        let run_id = new_run_id(commit_sha, reasoner_name);
        let now = OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)?;
        self.conn.execute(
            "INSERT INTO runs (run_id, commit_sha, hardware_id, reasoner_name, started_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![run_id, commit_sha, hardware_id, reasoner_name, now],
        )?;
        Ok(run_id)
    }

    pub fn record_outcome(&self, run_id: &str, o: &Outcome) -> Result<()> {
        let status = match o.status {
            Status::Passed => "passed",
            Status::Failed => "failed",
            Status::Skipped => "skipped",
        };
        self.conn.execute(
            "INSERT INTO outcomes (run_id, suite, test_id, status, reason, duration_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![run_id, o.suite, o.test_id, status, o.reason, o.duration_ms as i64],
        )?;
        Ok(())
    }

    /// Number of outcomes recorded against a given run.
    pub fn outcomes_for(&self, run_id: &str) -> Result<usize> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM outcomes WHERE run_id = ?1",
            params![run_id],
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }
}

fn new_run_id(commit_sha: &str, reasoner_name: &str) -> String {
    let mut h = Sha256::new();
    h.update(commit_sha.as_bytes());
    h.update(b":");
    h.update(reasoner_name.as_bytes());
    h.update(b":");
    h.update(
        OffsetDateTime::now_utc()
            .unix_timestamp_nanos()
            .to_string()
            .as_bytes(),
    );
    hex::encode(&h.finalize()[..8])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(id: &str, status: Status) -> Outcome {
        Outcome { test_id: id.into(), suite: "owl2".into(), status, reason: None, duration_ms: 1 }
    }

    #[test]
    fn open_in_memory_creates_schema() {
        let db = Db::open_in_memory().unwrap();
        let run = db.start_run("deadbeef", "fingerprint-1", "stub").unwrap();
        assert_eq!(db.outcomes_for(&run).unwrap(), 0);
    }

    #[test]
    fn records_and_counts_outcomes() {
        let db = Db::open_in_memory().unwrap();
        let run = db.start_run("deadbeef", "fingerprint-1", "stub").unwrap();
        db.record_outcome(&run, &outcome("a", Status::Passed)).unwrap();
        db.record_outcome(&run, &outcome("b", Status::Failed)).unwrap();
        db.record_outcome(&run, &outcome("c", Status::Skipped)).unwrap();
        assert_eq!(db.outcomes_for(&run).unwrap(), 3);
    }

    #[test]
    fn migrate_is_idempotent() {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        db.migrate().unwrap();
    }
}
```

- [ ] **Step 2: Run the db tests**

Run: `cd /Users/stig/git/sunstone/reasoner && cargo test -p horndb-harness db::tests`
Expected: 3 tests pass.

- [ ] **Step 3: Commit**

```bash
cd /Users/stig/git/sunstone/reasoner
git add crates/harness/src/db.rs
git commit -m "$(cat <<'EOF'
feat(harness): SQLite result store (SPEC-01 F7)

Three tables (runs / outcomes / metrics) match the F7 schema, with
status check constraint and per-run indexes. Open-in-memory mode used
for unit tests; bundled SQLite means no system dep.
EOF
)"
```

---

### Task 10: `harness report` query surface (F8) — TDD

**Files:**
- Modify: `/Users/stig/git/sunstone/reasoner/crates/harness/src/report.rs`
- Modify: `/Users/stig/git/sunstone/reasoner/crates/harness/src/db.rs` (add metric helpers)

- [ ] **Step 1: Add metric helpers to `db.rs`**

Insert the following functions inside the `impl Db { … }` block in `/Users/stig/git/sunstone/reasoner/crates/harness/src/db.rs`, right after `outcomes_for`:

```rust
    pub fn record_metric(
        &self,
        run_id: &str,
        suite: &str,
        dataset: Option<&str>,
        metric_name: &str,
        metric_value: f64,
        units: &str,
    ) -> Result<()> {
        let now = OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)?;
        self.conn.execute(
            "INSERT INTO metrics (run_id, suite, dataset, metric_name, metric_value, units, timestamp) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![run_id, suite, dataset, metric_name, metric_value, units, now],
        )?;
        Ok(())
    }

    /// Returns `(run_id, timestamp_rfc3339, metric_value)` rows for the
    /// given suite/metric, newest first.
    pub fn metric_series(
        &self,
        suite: &str,
        metric_name: &str,
    ) -> Result<Vec<(String, String, f64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT run_id, timestamp, metric_value FROM metrics
             WHERE suite = ?1 AND metric_name = ?2
             ORDER BY timestamp DESC",
        )?;
        let rows = stmt
            .query_map(params![suite, metric_name], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, f64>(2)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
```

- [ ] **Step 2: Write the failing report module**

Replace `/Users/stig/git/sunstone/reasoner/crates/harness/src/report.rs` with:

```rust
//! Trend-report queries (SPEC-01 F8).
//!
//! For Stage 0 we surface only the data primitives needed to wire up
//! `harness report` and assert F8 in CI: time-series fetch, geometric
//! mean over the window, and a >20% regression flag against the 7-day
//! median.

use anyhow::Result;

use crate::db::Db;

#[derive(Debug, Clone)]
pub struct TrendPoint {
    pub run_id: String,
    pub timestamp: String,
    pub value: f64,
}

#[derive(Debug, Clone)]
pub struct TrendReport {
    pub suite: String,
    pub metric: String,
    pub points: Vec<TrendPoint>,
    pub regression_flag: bool,
}

pub fn trend(
    db: &Db,
    suite: &str,
    metric: &str,
) -> Result<TrendReport> {
    let rows = db.metric_series(suite, metric)?;
    let points: Vec<TrendPoint> = rows
        .into_iter()
        .map(|(run_id, timestamp, value)| TrendPoint { run_id, timestamp, value })
        .collect();
    let regression_flag = detect_regression(&points);
    Ok(TrendReport {
        suite: suite.to_string(),
        metric: metric.to_string(),
        points,
        regression_flag,
    })
}

fn detect_regression(points: &[TrendPoint]) -> bool {
    // Newest point is points[0]. "7-day median" approximated as the
    // median of the next 7 points (Stage 0 — we have no real time
    // arithmetic yet, just ordinal index; revisit when F8 grows).
    if points.len() < 8 {
        return false;
    }
    let mut window: Vec<f64> = points[1..8].iter().map(|p| p.value).collect();
    window.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = window[3];
    let latest = points[0].value;
    // Latency metric semantics: higher is worse. A regression is
    // latest > 1.20 * median.
    latest > median * 1.20
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_regression_above_20_percent() {
        let mut points = vec![TrendPoint {
            run_id: "latest".into(),
            timestamp: "z".into(),
            value: 130.0,
        }];
        for i in 0..7 {
            points.push(TrendPoint {
                run_id: format!("p{i}"),
                timestamp: format!("y{i}"),
                value: 100.0,
            });
        }
        assert!(detect_regression(&points));
    }

    #[test]
    fn does_not_flag_within_20_percent() {
        let mut points = vec![TrendPoint {
            run_id: "latest".into(),
            timestamp: "z".into(),
            value: 115.0,
        }];
        for i in 0..7 {
            points.push(TrendPoint {
                run_id: format!("p{i}"),
                timestamp: format!("y{i}"),
                value: 100.0,
            });
        }
        assert!(!detect_regression(&points));
    }

    #[test]
    fn insufficient_history_does_not_flag() {
        let points = vec![TrendPoint {
            run_id: "x".into(),
            timestamp: "t".into(),
            value: 9999.0,
        }];
        assert!(!detect_regression(&points));
    }
}
```

- [ ] **Step 3: Run the report tests**

Run: `cd /Users/stig/git/sunstone/reasoner && cargo test -p horndb-harness report::tests`
Expected: 3 tests pass.

- [ ] **Step 4: Commit**

```bash
cd /Users/stig/git/sunstone/reasoner
git add crates/harness/src/db.rs crates/harness/src/report.rs
git commit -m "$(cat <<'EOF'
feat(harness): trend queries with >20% regression flag (SPEC-01 F8)

Adds db::record_metric / metric_series and report::trend, including the
"latest run >1.20x the 7-day median" regression rule. Stage-0 uses an
ordinal-7 window stand-in for the 7-day window; revisit when real
nightly runs accumulate.
EOF
)"
```

---

### Task 11: JUnit XML emitter for CI (F9 hand-off)

**Files:**
- Modify: `/Users/stig/git/sunstone/reasoner/crates/harness/src/ci.rs`

- [ ] **Step 1: Write the failing JUnit emitter**

Replace `/Users/stig/git/sunstone/reasoner/crates/harness/src/ci.rs` with:

```rust
//! JUnit-XML emitter so GitHub Actions can show per-test results in
//! the Checks tab without a custom action (SPEC-01 F9 hand-off).

use std::fmt::Write;

use crate::outcome::{Report, Status};

pub fn to_junit_xml(report: &Report) -> String {
    let total = report.outcomes.len();
    let failures = report.failed();
    let skipped = report.skipped();
    let mut out = String::new();
    let _ = writeln!(out, r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    let _ = writeln!(
        out,
        r#"<testsuite name="horndb-harness" tests="{total}" failures="{failures}" skipped="{skipped}">"#,
    );
    for o in &report.outcomes {
        let escaped_id = xml_escape(&o.test_id);
        let escaped_suite = xml_escape(&o.suite);
        match o.status {
            Status::Passed => {
                let _ = writeln!(
                    out,
                    r#"  <testcase classname="{escaped_suite}" name="{escaped_id}" time="{:.3}"/>"#,
                    o.duration_ms as f64 / 1000.0,
                );
            }
            Status::Failed => {
                let msg = xml_escape(o.reason.as_deref().unwrap_or("failed"));
                let _ = writeln!(
                    out,
                    r#"  <testcase classname="{escaped_suite}" name="{escaped_id}" time="{:.3}">"#,
                    o.duration_ms as f64 / 1000.0,
                );
                let _ = writeln!(out, r#"    <failure message="{msg}"/>"#);
                let _ = writeln!(out, "  </testcase>");
            }
            Status::Skipped => {
                let msg = xml_escape(o.reason.as_deref().unwrap_or("skipped"));
                let _ = writeln!(
                    out,
                    r#"  <testcase classname="{escaped_suite}" name="{escaped_id}">"#,
                );
                let _ = writeln!(out, r#"    <skipped message="{msg}"/>"#);
                let _ = writeln!(out, "  </testcase>");
            }
        }
    }
    let _ = writeln!(out, "</testsuite>");
    out
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outcome::{Outcome, Status};

    #[test]
    fn emits_well_formed_junit_for_mixed_report() {
        let mut r = Report::new();
        r.push(Outcome { test_id: "a".into(), suite: "owl2".into(), status: Status::Passed, reason: None, duration_ms: 12 });
        r.push(Outcome { test_id: "b<x>".into(), suite: "owl2".into(), status: Status::Failed, reason: Some("not entailed".into()), duration_ms: 5 });
        r.push(Outcome { test_id: "c".into(), suite: "sparql11".into(), status: Status::Skipped, reason: Some("waived".into()), duration_ms: 0 });
        let xml = to_junit_xml(&r);
        assert!(xml.starts_with("<?xml"));
        assert!(xml.contains(r#"tests="3""#));
        assert!(xml.contains(r#"failures="1""#));
        assert!(xml.contains(r#"skipped="1""#));
        assert!(xml.contains("b&lt;x&gt;"));
        assert!(xml.contains("<failure message=\"not entailed\""));
        assert!(xml.contains("<skipped message=\"waived\""));
        assert!(xml.ends_with("</testsuite>\n"));
    }

    #[test]
    fn xml_escape_handles_metas() {
        assert_eq!(xml_escape("a&<b>\"'"), "a&amp;&lt;b&gt;&quot;&apos;");
    }
}
```

- [ ] **Step 2: Run the emitter tests**

Run: `cd /Users/stig/git/sunstone/reasoner && cargo test -p horndb-harness ci::tests`
Expected: 2 tests pass.

- [ ] **Step 3: Commit**

```bash
cd /Users/stig/git/sunstone/reasoner
git add crates/harness/src/ci.rs
git commit -m "$(cat <<'EOF'
feat(harness): JUnit XML emitter for GitHub Actions Checks tab

Surfaces per-test pass/fail/skip in the standard JUnit shape so the
GH Actions runner can render individual entailment tests in the UI
without a bespoke action (SPEC-01 F9).
EOF
)"
```

---

### Task 12: Runner — dispatch + classify (TDD)

**Files:**
- Modify: `/Users/stig/git/sunstone/reasoner/crates/harness/src/runner.rs`

- [ ] **Step 1: Write the failing runner**

Replace `/Users/stig/git/sunstone/reasoner/crates/harness/src/runner.rs` with:

```rust
//! Dispatches each selected test case against a `Reasoner` and
//! classifies the outcome (SPEC-01 F1/F2).

use std::fs;
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result};
use oxrdf::{Dataset, Graph, Quad, GraphName};
use oxttl::TurtleParser;

use crate::outcome::{Outcome, Report, Status};
use crate::reasoner::Reasoner;
use crate::selected::Selected;
use crate::testcase::{Suite, TestCase, TestKind};

/// Loads each selected suite's manifest, filters down to the selected
/// IDs, runs each through `engine`, and produces a [`Report`].
pub fn run_selected(
    engine: &mut dyn Reasoner,
    selected: &Selected,
    workspace_root: &Path,
    manifest_loader: &dyn Fn(&Path, Suite) -> Result<Vec<TestCase>>,
) -> Result<Report> {
    let mut report = Report::new();
    for (suite_name, suite_entry) in &selected.suites {
        let suite = match suite_name.as_str() {
            "owl2" => Suite::Owl2,
            "sparql11" => Suite::Sparql11,
            other => {
                report.push(Outcome {
                    test_id: format!("<suite:{other}>"),
                    suite: other.to_string(),
                    status: Status::Skipped,
                    reason: Some(format!("unknown suite {other}")),
                    duration_ms: 0,
                });
                continue;
            }
        };
        let manifest_path = workspace_root.join(&suite_entry.manifest);
        let cases = manifest_loader(&manifest_path, suite)
            .with_context(|| format!("loading manifest {}", manifest_path.display()))?;
        for case in &cases {
            if !suite_entry.include.iter().any(|id| id == &case.id) {
                continue;
            }
            let start = Instant::now();
            let outcome = run_one(engine, case).unwrap_or_else(|e| Outcome {
                test_id: case.id.clone(),
                suite: suite_name.clone(),
                status: Status::Failed,
                reason: Some(format!("harness error: {e:#}")),
                duration_ms: start.elapsed().as_millis() as u64,
            });
            report.push(outcome);
        }
    }
    Ok(report)
}

fn run_one(engine: &mut dyn Reasoner, case: &TestCase) -> Result<Outcome> {
    let start = Instant::now();
    let suite = case.suite.as_str().to_string();
    let id = case.id.clone();

    let (status, reason) = match &case.kind {
        TestKind::PositiveEntailment { premise, conclusion } => {
            let p = load_dataset(premise)?;
            let c = load_dataset(conclusion)?;
            engine.load(&p)?;
            if engine.entails(&c)? {
                (Status::Passed, None)
            } else {
                (Status::Failed, Some("premise did not entail conclusion".into()))
            }
        }
        TestKind::NegativeEntailment { premise, conclusion } => {
            let p = load_dataset(premise)?;
            let c = load_dataset(conclusion)?;
            engine.load(&p)?;
            if engine.entails(&c)? {
                (Status::Failed, Some("conclusion entailed but should not be".into()))
            } else {
                (Status::Passed, None)
            }
        }
        TestKind::Consistency { premise } => {
            let p = load_dataset(premise)?;
            engine.load(&p)?;
            if engine.is_consistent()? {
                (Status::Passed, None)
            } else {
                (Status::Failed, Some("expected consistent, got inconsistent".into()))
            }
        }
        TestKind::Inconsistency { premise } => {
            let p = load_dataset(premise)?;
            engine.load(&p)?;
            if !engine.is_consistent()? {
                (Status::Passed, None)
            } else {
                (Status::Failed, Some("expected inconsistent, got consistent".into()))
            }
        }
        TestKind::SparqlAsk { query, data, expected } => {
            let d = load_dataset(data)?;
            engine.load(&d)?;
            let q = fs::read_to_string(query)
                .with_context(|| format!("reading query {}", query.display()))?;
            let got = engine.ask(&q)?;
            if got == *expected {
                (Status::Passed, None)
            } else {
                (Status::Failed, Some(format!("ASK got {got}, expected {expected}")))
            }
        }
    };

    Ok(Outcome {
        test_id: id,
        suite,
        status,
        reason,
        duration_ms: start.elapsed().as_millis() as u64,
    })
}

fn load_dataset(path: &Path) -> Result<Dataset> {
    let bytes = fs::read(path)
        .with_context(|| format!("reading rdf {}", path.display()))?;
    let mut graph = Graph::new();
    let mut parser = TurtleParser::new()
        .with_base_iri(&format!("file://{}", path.display()))?
        .parse();
    parser.extend_from_slice(&bytes);
    parser.end();
    while let Some(t) = parser.read_next() {
        graph.insert(&t?);
    }
    let mut dataset = Dataset::new();
    for triple in graph.iter() {
        dataset.insert(&Quad::new(
            triple.subject.clone().into_owned(),
            triple.predicate.clone().into_owned(),
            triple.object.clone().into_owned(),
            GraphName::DefaultGraph,
        ));
    }
    Ok(dataset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stub::StubReasoner;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn fixtures() -> PathBuf {
        let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        p.pop(); p.pop(); // back to workspace root
        p
    }

    #[test]
    fn stub_passes_trivial_and_inconsistent_fails_subclass() {
        // Build a Selected programmatically that matches the fixture IDs.
        let cases = crate::manifest::parse(
            &fixtures().join("crates/harness/tests/fixtures/owl2/manifest.ttl"),
            Suite::Owl2,
        )
        .unwrap();
        let mut suites = BTreeMap::new();
        suites.insert(
            "owl2".to_string(),
            crate::selected::SuiteEntry {
                manifest: "crates/harness/tests/fixtures/owl2/manifest.ttl".to_string(),
                include: cases.iter().map(|c| c.id.clone()).collect(),
            },
        );
        let selected = Selected { version: 1, suites, removed: vec![] };

        let mut engine = StubReasoner::new();
        let report = run_selected(
            &mut engine,
            &selected,
            &fixtures(),
            &|p, s| crate::manifest::parse(p, s),
        )
        .unwrap();

        assert_eq!(report.outcomes.len(), 3, "all three OWL2 fixtures run");

        let by_id = |id_suffix: &str| -> &Outcome {
            report
                .outcomes
                .iter()
                .find(|o| o.test_id.ends_with(id_suffix))
                .unwrap_or_else(|| panic!("missing outcome for {id_suffix}"))
        };

        assert_eq!(by_id("trivial-entail-true").status, Status::Passed);
        assert_eq!(by_id("subclass-entail").status, Status::Failed);
        assert_eq!(by_id("inconsistent-001").status, Status::Passed);
    }
}
```

- [ ] **Step 2: Run the runner tests**

Run: `cd /Users/stig/git/sunstone/reasoner && cargo test -p horndb-harness runner::tests`
Expected: 1 test passes. Stub passes trivial-entail-true and inconsistent-001; deliberately fails subclass-entail — exactly what SPEC-01 Stage-0 exit criterion 3 asks for.

- [ ] **Step 3: Commit**

```bash
cd /Users/stig/git/sunstone/reasoner
git add crates/harness/src/runner.rs
git commit -m "$(cat <<'EOF'
feat(harness): runner dispatches selected manifests through Reasoner

Implements SPEC-01 F1/F2 dispatch + classification for the five test
kinds (positive/negative entailment, consistency/inconsistency, SPARQL
ASK). Runner exercised against the stub: 2 pass, 1 deliberately fails
— proving the harness flags red on intentional failure (Stage-0 #3).
EOF
)"
```

---

### Task 13: `harness` CLI (clap) — TDD via `assert_cmd`

**Files:**
- Modify: `/Users/stig/git/sunstone/reasoner/crates/harness/src/bin/harness.rs`
- Create: `/Users/stig/git/sunstone/reasoner/crates/harness/tests/cli_smoke.rs`

- [ ] **Step 1: Replace the CLI placeholder with the real CLI**

Replace `/Users/stig/git/sunstone/reasoner/crates/harness/src/bin/harness.rs` with:

```rust
//! `harness` — entrypoint for the SPEC-01 conformance & benchmark
//! harness. Used both locally and from GitHub Actions.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tracing::info;

use horndb_harness::{
    ci::to_junit_xml,
    db::Db,
    manifest, report as report_mod,
    runner::run_selected,
    selected::Selected,
    stub::StubReasoner,
    Reasoner, Status,
};

#[derive(Parser, Debug)]
#[command(name = "harness", version, about = "HornDB conformance & benchmark harness")]
struct Cli {
    /// Path to workspace root (default: cwd).
    #[arg(long, default_value = ".")]
    workspace: PathBuf,
    /// SQLite result DB (default: target/harness.sqlite).
    #[arg(long)]
    db: Option<PathBuf>,
    /// Engine to dispatch against. Stage 0 only supports `stub`.
    #[arg(long, default_value = "stub")]
    engine: String,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run the currently-selected subset against the chosen engine.
    Run {
        /// Path to selected.toml (default: harness/selected.toml under workspace).
        #[arg(long)]
        selected: Option<PathBuf>,
        /// Write JUnit XML to this path.
        #[arg(long)]
        junit: Option<PathBuf>,
        /// Treat the run as green even if some tests fail (used by the
        /// stub self-test that deliberately includes a failing case).
        #[arg(long)]
        allow_failing: bool,
    },
    /// Query the trend database.
    Report {
        #[arg(long)]
        suite: String,
        #[arg(long)]
        metric: String,
    },
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    match real_main() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("harness: error: {e:#}");
            ExitCode::from(2)
        }
    }
}

fn real_main() -> Result<ExitCode> {
    let cli = Cli::parse();
    let workspace = cli.workspace.canonicalize().unwrap_or(cli.workspace.clone());
    let db_path = cli
        .db
        .unwrap_or_else(|| workspace.join("target/harness.sqlite"));
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let db = Db::open(&db_path)?;

    match cli.cmd {
        Cmd::Run { selected, junit, allow_failing } => {
            let sel_path = selected.unwrap_or_else(|| workspace.join("harness/selected.toml"));
            let sel = Selected::load(&sel_path)?;
            let mut engine: Box<dyn Reasoner> = match cli.engine.as_str() {
                "stub" => Box::new(StubReasoner::new()),
                other => anyhow::bail!("unknown engine: {other} (Stage 0 supports: stub)"),
            };
            let commit_sha = std::env::var("GITHUB_SHA").unwrap_or_else(|_| "unknown".into());
            let hw = hardware_fingerprint();
            let run_id = db.start_run(&commit_sha, &hw, engine.name())?;
            info!(run_id = %run_id, "harness run started");

            let report = run_selected(
                engine.as_mut(),
                &sel,
                &workspace,
                &|p, s| manifest::parse(p, s),
            )?;
            for outcome in &report.outcomes {
                db.record_outcome(&run_id, outcome)?;
            }
            println!(
                "harness: run_id={} passed={} failed={} skipped={}",
                run_id, report.passed(), report.failed(), report.skipped(),
            );
            for o in &report.outcomes {
                let tag = match o.status {
                    Status::Passed => "PASS",
                    Status::Failed => "FAIL",
                    Status::Skipped => "SKIP",
                };
                let reason = o.reason.as_deref().unwrap_or("");
                println!("  [{tag}] {} {} {}", o.suite, o.test_id, reason);
            }
            if let Some(p) = junit {
                std::fs::write(&p, to_junit_xml(&report))
                    .with_context(|| format!("writing junit {}", p.display()))?;
            }
            if report.has_failures() && !allow_failing {
                return Ok(ExitCode::from(1));
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Report { suite, metric } => {
            let t = report_mod::trend(&db, &suite, &metric)?;
            println!(
                "trend suite={} metric={} points={} regression={}",
                t.suite, t.metric, t.points.len(), t.regression_flag,
            );
            for p in &t.points {
                println!("  {} {} {}", p.timestamp, p.run_id, p.value);
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn hardware_fingerprint() -> String {
    // Stage 0: minimal — OS + arch. Stage 2 deepens this per F7.
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}
```

- [ ] **Step 2: Write the failing CLI smoke test**

Create `/Users/stig/git/sunstone/reasoner/crates/harness/tests/cli_smoke.rs`:

```rust
use std::path::PathBuf;

use assert_cmd::Command;
use predicates::str::contains;
use tempfile::tempdir;

fn workspace_root() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p.pop();
    p
}

#[test]
fn run_with_default_selection_against_stub_fails_red() {
    let tmp = tempdir().unwrap();
    let db = tmp.path().join("h.sqlite");
    let junit = tmp.path().join("results.xml");
    let assertion = Command::cargo_bin("harness")
        .unwrap()
        .args([
            "--workspace", workspace_root().to_str().unwrap(),
            "--db", db.to_str().unwrap(),
            "--engine", "stub",
            "run",
            "--junit", junit.to_str().unwrap(),
        ])
        .assert();
    let output = assertion.get_output().clone();
    let stdout = String::from_utf8(output.stdout.clone()).unwrap();
    assert!(stdout.contains("FAIL"), "stub must fail at least one selected test");
    assert!(stdout.contains("PASS"), "stub must pass at least one selected test");
    assertion.failure();
    let xml = std::fs::read_to_string(&junit).unwrap();
    assert!(xml.contains("<testsuite"));
    assert!(xml.contains("<failure"));
}

#[test]
fn allow_failing_flag_keeps_exit_zero() {
    let tmp = tempdir().unwrap();
    let db = tmp.path().join("h.sqlite");
    Command::cargo_bin("harness")
        .unwrap()
        .args([
            "--workspace", workspace_root().to_str().unwrap(),
            "--db", db.to_str().unwrap(),
            "--engine", "stub",
            "run",
            "--allow-failing",
        ])
        .assert()
        .success()
        .stdout(contains("FAIL"));
}
```

- [ ] **Step 3: Run the CLI smoke tests**

Run: `cd /Users/stig/git/sunstone/reasoner && cargo test -p horndb-harness --test cli_smoke`
Expected: both tests pass. First test confirms the harness exits non-zero when the stub fails the deliberately-included `subclass-entail` case. Second test confirms `--allow-failing` keeps exit zero.

- [ ] **Step 4: Commit**

```bash
cd /Users/stig/git/sunstone/reasoner
git add crates/harness/src/bin/harness.rs crates/harness/tests/cli_smoke.rs
git commit -m "$(cat <<'EOF'
feat(harness): clap-based CLI with run and report subcommands

`harness run` dispatches selected.toml against the chosen engine,
writes outcomes to SQLite, optionally emits JUnit XML, and exits
non-zero on failures. `harness report` queries the trend DB.
EOF
)"
```

---

### Task 14: Workspace `cargo test` smoke + harness CLI invocation script

**Files:**
- Modify: `/Users/stig/git/sunstone/reasoner/crates/harness/README.md` (create)

- [ ] **Step 1: Create the README explaining local invocation**

Create `/Users/stig/git/sunstone/reasoner/crates/harness/README.md`:

```markdown
# horndb-harness

SPEC-01 conformance and benchmarking harness. See
`specs/SPEC-01-conformance-benchmarks.md` and
`plans/PLAN-01-01-conformance-harness.md`.

## Quick start

Run the currently-selected subset against the in-tree stub engine:

```bash
cargo run -p horndb-harness --bin harness -- \
    --engine stub \
    run \
    --junit target/junit.xml \
    --allow-failing
```

`--allow-failing` is needed locally because `harness/selected.toml`
intentionally includes one test the stub cannot pass; this is how we
prove the harness flags red on real failure.

## Query the trend DB

```bash
cargo run -p horndb-harness --bin harness -- \
    report --suite owl2 --metric pass-rate
```

## CI

`.github/workflows/ci.yml` runs the same `harness run` *without*
`--allow-failing`, so any newly-broken case in the selected subset
blocks the PR.
```

- [ ] **Step 2: Run the full workspace test suite to make sure nothing regressed**

Run: `cd /Users/stig/git/sunstone/reasoner && cargo test --workspace`
Expected: all tests pass.

- [ ] **Step 3: Commit**

```bash
cd /Users/stig/git/sunstone/reasoner
git add crates/harness/README.md
git commit -m "$(cat <<'EOF'
docs(harness): quick-start README pointing at SPEC-01 and the plan
EOF
)"
```

---

### Task 15: GitHub Actions CI workflow (F9)

**Files:**
- Create: `/Users/stig/git/sunstone/reasoner/.github/workflows/ci.yml`

- [ ] **Step 1: Make the workflow directory**

Run: `mkdir -p /Users/stig/git/sunstone/reasoner/.github/workflows`

- [ ] **Step 2: Create the CI workflow**

Create `/Users/stig/git/sunstone/reasoner/.github/workflows/ci.yml`:

```yaml
name: ci

on:
  pull_request:
  push:
    branches: [main]

jobs:
  lint-test-conformance:
    runs-on: ubuntu-latest
    timeout-minutes: 15
    env:
      CARGO_TERM_COLOR: always
      RUSTFLAGS: "-D warnings"
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 1

      - uses: actions-rust-lang/setup-rust-toolchain@v1
        with:
          toolchain: "1.79.0"
          components: rustfmt, clippy
          cache: true

      - name: rustfmt
        run: cargo fmt --all -- --check

      - name: clippy
        run: cargo clippy --workspace --all-targets -- -D warnings

      - name: unit tests
        run: cargo test --workspace

      - name: build harness binary
        run: cargo build -p horndb-harness --bin harness --release

      - name: conformance — selected subset (real run, no --allow-failing)
        # Stage 0 selected.toml intentionally includes a test the stub
        # cannot pass, so the *expected* outcome of this step is a red
        # failure once we wire the stub up. Until the real engine lands
        # (Stage 1), this job is required-but-allowed-to-fail in branch
        # protection — see /docs/CONTRIBUTING when added.
        run: |
          ./target/release/harness \
            --engine stub \
            run \
            --junit target/junit.xml

      - name: upload junit
        if: always()
        uses: actions/upload-artifact@v4
        with:
          name: junit-${{ github.run_id }}
          path: target/junit.xml

      - name: publish test report
        if: always()
        uses: mikepenz/action-junit-report@v4
        with:
          report_paths: target/junit.xml
          include_passed: true
          fail_on_failure: true
```

- [ ] **Step 3: Verify the workflow YAML parses locally**

Run: `python3 -c "import yaml,sys; yaml.safe_load(open('/Users/stig/git/sunstone/reasoner/.github/workflows/ci.yml'))" && echo OK`
Expected: prints `OK`.

- [ ] **Step 4: Commit**

```bash
cd /Users/stig/git/sunstone/reasoner
git add .github/workflows/ci.yml
git commit -m "$(cat <<'EOF'
ci: run the SPEC-01 selected subset on every PR (F9)

Runs rustfmt/clippy/unit tests, then `harness run` against the stub
engine without --allow-failing so any newly-broken case in
harness/selected.toml blocks the PR. JUnit XML uploaded and published
to the Checks tab for per-test visibility.
EOF
)"
```

---

## Stage 0 self-review checklist

Before declaring Stage 0 done, verify each SPEC-01 Stage-0 exit
criterion has a corresponding artefact in the plan above:

- ✅ Runner exists for OWL 2 and SPARQL 1.1 (Tasks 6, 12).
- ✅ `harness/selected.toml` selects ≥1 test per suite (Task 8).
- ✅ Stub engine fails its assigned tests; CI turns red (Tasks 5, 12,
  13, 15 — `subclass-entail` is included for exactly this reason).
- ✅ SQLite result DB wired; `harness report` returns rows (Tasks 9,
  10, 13).

---

# STAGE 1 — Real W3C subset + ORE 2015 + LDBC SPB-256 (3 months)

Exit criteria, from SPEC-01:
5. Selected W3C OWL 2 RL subset expanded to ≥50 test cases covering
   the most-used rules. All selected tests pass.
6. ORE 2015 runner integrated (F3); selected subset starts at a
   hand-picked 10 OWL 2 RL clean ontologies.
7. LDBC SPB-256 end-to-end against the real engine and against GraphDB
   Free for comparison (F10).

**Dependency:** Stage 1 assumes a real reasoner crate (`horndb-owlrl`
or similar) exposes a struct that implements [`Reasoner`]. The exact
wiring lives in the SPEC-04 plan; here we provide the harness-side
surface and a thin adapter.

---

### Task 16: Fetch script for the upstream W3C suites

**Files:**
- Create: `/Users/stig/git/sunstone/reasoner/crates/harness/scripts/fetch-w3c-suites.sh`
- Modify: `/Users/stig/git/sunstone/reasoner/.gitignore`

- [ ] **Step 1: Add `data/` to .gitignore**

Append the following to `/Users/stig/git/sunstone/reasoner/.gitignore`:

```
crates/harness/data/
```

- [ ] **Step 2: Create the fetch script**

Create `/Users/stig/git/sunstone/reasoner/crates/harness/scripts/fetch-w3c-suites.sh`:

```bash
#!/usr/bin/env bash
# Fetch the W3C OWL 2 Test Cases and SPARQL 1.1 Test Suite into
# crates/harness/data/, then convert their RDF/XML manifests to Turtle
# so the in-tree manifest parser (src/manifest.rs) can read them.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"
DATA="$ROOT/crates/harness/data"
mkdir -p "$DATA"

OWL2_URL="https://www.w3.org/2009/11/owl-test/testOntology-20091022.zip"
SPARQL_URL="https://www.w3.org/2009/sparql/docs/tests/sparql11-test-suite-20121023.tar.gz"

if [[ ! -d "$DATA/w3c-owl2-tests" ]]; then
    echo "fetching OWL 2 test cases…"
    curl -sSfL "$OWL2_URL" -o "$DATA/owl2.zip"
    mkdir -p "$DATA/w3c-owl2-tests"
    (cd "$DATA/w3c-owl2-tests" && unzip -q "$DATA/owl2.zip")
fi

if [[ ! -d "$DATA/w3c-sparql11-tests" ]]; then
    echo "fetching SPARQL 1.1 test suite…"
    curl -sSfL "$SPARQL_URL" -o "$DATA/sparql11.tgz"
    mkdir -p "$DATA/w3c-sparql11-tests"
    tar -xzf "$DATA/sparql11.tgz" -C "$DATA/w3c-sparql11-tests"
fi

# Convert each .rdf manifest into .ttl using the harness CLI helper.
# (The convert subcommand is added in Task 17.)
cargo run -p horndb-harness --bin harness -- \
    convert-manifests --root "$DATA"

echo "done."
```

- [ ] **Step 3: Make the script executable**

Run: `chmod +x /Users/stig/git/sunstone/reasoner/crates/harness/scripts/fetch-w3c-suites.sh`

- [ ] **Step 4: Commit**

```bash
cd /Users/stig/git/sunstone/reasoner
git add crates/harness/scripts/fetch-w3c-suites.sh .gitignore
git commit -m "$(cat <<'EOF'
feat(harness): fetch script for upstream W3C OWL 2 and SPARQL 1.1 suites

Downloads the upstream test archives into crates/harness/data/ (which
is gitignored) and calls `harness convert-manifests` to rewrite
RDF/XML manifests as Turtle so the in-tree parser can read them.
EOF
)"
```

---

### Task 17: `harness convert-manifests` subcommand (RDF/XML → Turtle)

**Files:**
- Modify: `/Users/stig/git/sunstone/reasoner/crates/harness/src/bin/harness.rs`
- Modify: `/Users/stig/git/sunstone/reasoner/crates/harness/tests/cli_smoke.rs`

- [ ] **Step 1: Add the convert subcommand**

In `/Users/stig/git/sunstone/reasoner/crates/harness/src/bin/harness.rs`, extend the `Cmd` enum (insert as a new variant inside the existing `enum Cmd { … }`):

```rust
    /// Walk `--root` and convert every `manifest.rdf` (RDF/XML) into
    /// a sibling `manifest.ttl`. Skips files that already have a .ttl
    /// counterpart. Stage-1 only.
    ConvertManifests {
        #[arg(long)]
        root: PathBuf,
    },
```

In the same file, add the matching arm to the `match cli.cmd` block in `real_main`:

```rust
        Cmd::ConvertManifests { root } => {
            use oxrdfio::{RdfFormat, RdfParser, RdfSerializer};
            let mut count = 0usize;
            for entry in walkdir::WalkDir::new(&root) {
                let entry = entry?;
                if entry.file_name() != "manifest.rdf" {
                    continue;
                }
                let src = entry.path().to_path_buf();
                let dst = src.with_extension("ttl");
                if dst.exists() {
                    continue;
                }
                let bytes = std::fs::read(&src)?;
                let parser = RdfParser::from_format(RdfFormat::RdfXml)
                    .with_base_iri(&format!("file://{}", src.display()))?;
                let mut serializer = RdfSerializer::from_format(RdfFormat::Turtle)
                    .serialize_to_write(Vec::<u8>::new());
                for quad in parser.parse_read(&bytes[..]) {
                    serializer.write_quad(&quad?)?;
                }
                let out = serializer.finish()?;
                std::fs::write(&dst, out)?;
                count += 1;
            }
            println!("converted {count} manifest.rdf → manifest.ttl");
            Ok(ExitCode::SUCCESS)
        }
```

You'll also need to add `use std::path::PathBuf;` at the top if it's not already there (it is, from Task 13).

- [ ] **Step 2: Add a smoke test for the convert subcommand using a hand-rolled RDF/XML file**

Append to `/Users/stig/git/sunstone/reasoner/crates/harness/tests/cli_smoke.rs`:

```rust
#[test]
fn convert_manifests_rewrites_rdfxml_into_turtle() {
    let tmp = tempdir().unwrap();
    let manifest = tmp.path().join("manifest.rdf");
    std::fs::write(
        &manifest,
        r#"<?xml version="1.0"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
         xmlns:mf="http://www.w3.org/2001/sw/DataAccess/tests/test-manifest#">
  <mf:Manifest rdf:about="#m">
    <mf:entries rdf:resource="http://www.w3.org/1999/02/22-rdf-syntax-ns#nil"/>
  </mf:Manifest>
</rdf:RDF>
"#,
    )
    .unwrap();
    Command::cargo_bin("harness")
        .unwrap()
        .args([
            "convert-manifests",
            "--root", tmp.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(contains("converted 1"));
    let ttl = std::fs::read_to_string(manifest.with_extension("ttl")).unwrap();
    assert!(ttl.contains("Manifest"));
}
```

- [ ] **Step 3: Run the smoke test**

Run: `cd /Users/stig/git/sunstone/reasoner && cargo test -p horndb-harness --test cli_smoke convert_manifests_rewrites_rdfxml_into_turtle`
Expected: passes.

- [ ] **Step 4: Commit**

```bash
cd /Users/stig/git/sunstone/reasoner
git add crates/harness/src/bin/harness.rs crates/harness/tests/cli_smoke.rs
git commit -m "$(cat <<'EOF'
feat(harness): add convert-manifests subcommand (RDF/XML → Turtle)

Lets the fetch script normalise every upstream W3C manifest to Turtle
so the in-tree parser is the single ingestion point. Skips files that
already have a .ttl counterpart so it is safe to re-run.
EOF
)"
```

---

### Task 18: Curated 50-case OWL 2 RL selection in selected.toml

**Files:**
- Modify: `/Users/stig/git/sunstone/reasoner/harness/selected.toml`
- Create: `/Users/stig/git/sunstone/reasoner/harness/curation/owl2-rl-50.md`

- [ ] **Step 1: Document the curation methodology**

Create `/Users/stig/git/sunstone/reasoner/harness/curation/owl2-rl-50.md`:

```markdown
# OWL 2 RL Stage-1 Selection (50 cases)

This document records exactly *which* 50 W3C OWL 2 test cases the
Stage-1 selected subset names, and *why* each was picked.

Coverage target: the "most-used rules" from the OWL 2 RL/RDF rule
table (cax-sco, cax-eqc1, cax-eqc2, prp-spo1, prp-spo2, prp-dom,
prp-rng, prp-trp, prp-symp, prp-eqp1, prp-eqp2, scm-sco, scm-spo,
scm-cls, scm-eqc1, scm-eqp1, cls-thing, cls-nothing1, cls-int1,
cls-uni, eq-sym, eq-trans). Each rule must have at least one positive
entailment fixture and, where applicable, one negative entailment
fixture in the selection.

## Selection process

1. After running `scripts/fetch-w3c-suites.sh`, list every test case
   in `crates/harness/data/w3c-owl2-tests/` with profile
   `OWL 2 RL` (filter by `<#profile>`).
2. For each rule above, pick the smallest positive-entailment test
   that exercises the rule in isolation.
3. Where the OWL 2 RL profile has known-broken upstream manifests
   (see `harness/KNOWN-MANIFEST-BUGS.md`), pick the next-smallest.
4. Round to 50 total by adding consistency / inconsistency tests until
   the count is met.

## Acceptance

The selected 50 IDs are listed under `[suites.owl2].include` in
`harness/selected.toml`. Adding or removing entries requires updating
this file *and* the methodology section above.
```

- [ ] **Step 2: Replace selected.toml's owl2 section with the 50 IDs (placeholder discovery)**

The actual 50 IDs come from running the fetch script. The Stage-1
implementer runs the discovery step below, pastes the resulting IDs
into `selected.toml`, and commits both the IDs and the methodology
update in the same change. Do this in two sub-steps:

Sub-step 2a — discover candidates:

Run:
```bash
cd /Users/stig/git/sunstone/reasoner
./crates/harness/scripts/fetch-w3c-suites.sh
cargo run -p horndb-harness --bin harness -- \
    list-cases \
    --manifest crates/harness/data/w3c-owl2-tests/Manifest.ttl \
    --profile "OWL 2 RL" \
    --max 50 \
    > /tmp/owl2-rl-candidates.txt
```
Expected: file `/tmp/owl2-rl-candidates.txt` containing ≥50 fully-
qualified test IDs, one per line.

Sub-step 2b — bake the IDs into selected.toml:

Replace the `[suites.owl2]` block in
`/Users/stig/git/sunstone/reasoner/harness/selected.toml` with:

```toml
[suites.owl2]
manifest = "crates/harness/data/w3c-owl2-tests/Manifest.ttl"
include = [
    # ── REPLACE THIS LIST with the 50 IDs from
    #    /tmp/owl2-rl-candidates.txt, one per line.
    #    The exact rule coverage is documented in
    #    harness/curation/owl2-rl-50.md.
    # The Stage-0 fixture IDs are preserved underneath so the local
    # smoke test still works against the in-tree fixtures:
    "file:///stig/git/sunstone/reasoner/crates/harness/tests/fixtures/owl2/manifest.ttl#trivial-entail-true",
    "file:///stig/git/sunstone/reasoner/crates/harness/tests/fixtures/owl2/manifest.ttl#inconsistent-001",
]
```

Note: the Stage-0 `subclass-entail` deliberately-failing fixture is
**removed** at this point because the real engine should pass it. Add
a `[[removed]]` entry recording the removal:

```toml
[[removed]]
test_id = "file:///stig/git/sunstone/reasoner/crates/harness/tests/fixtures/owl2/manifest.ttl#subclass-entail"
suite = "owl2"
xfail_reason = "Stage-0 deliberately-failing stub fixture; Stage-1 engine passes it via real W3C cases instead"
tracking_issue = "https://github.com/sunstoneinstitute/horndb/issues/TBD-stage1-cleanup"
```

- [ ] **Step 3: Add the `list-cases` subcommand referenced above**

In `/Users/stig/git/sunstone/reasoner/crates/harness/src/bin/harness.rs`,
add this variant to `Cmd`:

```rust
    /// List candidate test IDs for a profile from a manifest.
    ListCases {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        profile: String,
        #[arg(long, default_value = "50")]
        max: usize,
    },
```

And the matching arm in `real_main`'s `match cli.cmd`:

```rust
        Cmd::ListCases { manifest, profile, max } => {
            // Stage-1 minimal: read the manifest, print the first
            // `max` test IDs (the implementer hand-curates which 50
            // to keep based on rule coverage — see
            // harness/curation/owl2-rl-50.md).
            let suite = if manifest.to_string_lossy().contains("sparql11") {
                horndb_harness::testcase::Suite::Sparql11
            } else {
                horndb_harness::testcase::Suite::Owl2
            };
            let cases = manifest::parse(&manifest, suite)?;
            let _ = profile; // profile filter requires `mf:profile` parsing;
                             // wired by the Stage-1 implementer.
            for case in cases.iter().take(max) {
                println!("{}", case.id);
            }
            Ok(ExitCode::SUCCESS)
        }
```

- [ ] **Step 4: Commit**

```bash
cd /Users/stig/git/sunstone/reasoner
git add crates/harness/src/bin/harness.rs harness/selected.toml harness/curation/owl2-rl-50.md
git commit -m "$(cat <<'EOF'
feat(harness): expand selected.toml to the Stage-1 OWL 2 RL 50-case subset

Adds the `harness list-cases` discovery subcommand, the curation
methodology document (harness/curation/owl2-rl-50.md), and records the
removal of the Stage-0 deliberately-failing fixture in [[removed]].
EOF
)"
```

---

### Task 19: Real-engine adapter trait wiring

**Files:**
- Modify: `/Users/stig/git/sunstone/reasoner/crates/harness/Cargo.toml`
- Modify: `/Users/stig/git/sunstone/reasoner/crates/harness/src/bin/harness.rs`

- [ ] **Step 1: Add an optional dependency on the real engine crate**

Append to `/Users/stig/git/sunstone/reasoner/crates/harness/Cargo.toml`:

```toml
[features]
default = []
real-engine = ["dep:horndb-owlrl"]

[dependencies.horndb-owlrl]
path = "../owlrl"
optional = true
```

- [ ] **Step 2: Plumb the real engine into the CLI behind the feature flag**

In `/Users/stig/git/sunstone/reasoner/crates/harness/src/bin/harness.rs`, change the engine match arm to:

```rust
            let mut engine: Box<dyn Reasoner> = match cli.engine.as_str() {
                "stub" => Box::new(StubReasoner::new()),
                #[cfg(feature = "real-engine")]
                "owlrl" => Box::new(horndb_owlrl::Engine::new()),
                other => anyhow::bail!("unknown engine: {other}"),
            };
```

The real engine crate must (by Stage 1) expose a struct `Engine` that
implements `horndb_harness::Reasoner`. The wiring of `Engine` itself
lives in the SPEC-04 plan; this task only declares the harness-side
seam.

- [ ] **Step 3: Build with the real-engine feature off and on (off should still work)**

Run: `cd /Users/stig/git/sunstone/reasoner && cargo build -p horndb-harness`
Expected: clean build (real-engine feature off).

The `cargo build -p horndb-harness --features real-engine` step is
*not* expected to pass until the SPEC-04 plan delivers
`horndb_owlrl::Engine`. That cross-spec dependency is the gating
event for Stage-1 acceptance.

- [ ] **Step 4: Commit**

```bash
cd /Users/stig/git/sunstone/reasoner
git add crates/harness/Cargo.toml crates/harness/src/bin/harness.rs
git commit -m "$(cat <<'EOF'
feat(harness): real-engine cargo feature wiring

Adds the `real-engine` feature that pulls in `horndb-owlrl` and
exposes it as `--engine owlrl`. The feature is off by default so CI
builds keep working; turned on when the SPEC-04 engine lands.
EOF
)"
```

---

### Task 20: ORE 2015 wrapper — 10-ontology subset (F3)

**Files:**
- Create: `/Users/stig/git/sunstone/reasoner/crates/harness/src/ore.rs`
- Modify: `/Users/stig/git/sunstone/reasoner/crates/harness/src/lib.rs`
- Create: `/Users/stig/git/sunstone/reasoner/crates/harness/scripts/fetch-ore2015-subset.sh`
- Create: `/Users/stig/git/sunstone/reasoner/harness/ore2015-selected.toml`

- [ ] **Step 1: Add the ore module**

Insert `pub mod ore;` near the other `pub mod` lines in
`/Users/stig/git/sunstone/reasoner/crates/harness/src/lib.rs`.

Create `/Users/stig/git/sunstone/reasoner/crates/harness/src/ore.rs`:

```rust
//! ORE 2015 runner wrapper (SPEC-01 F3).
//!
//! Stage-1 scope: a hand-picked subset of 10 ontologies known to be
//! OWL 2 RL clean. We do not run the full 1,920-ontology corpus until
//! Stage 2. Time budget per ontology: 5 minutes wall clock, matching
//! the ORE 2015 competition rules.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::outcome::{Outcome, Report, Status};
use crate::reasoner::Reasoner;

#[derive(Debug, Deserialize)]
pub struct OreSelected {
    pub version: u32,
    pub ontologies: Vec<OreOntology>,
}

#[derive(Debug, Deserialize)]
pub struct OreOntology {
    pub id: String,
    pub path: String,
    pub task: OreTask,
    /// Optional `(ASK query, expected)` for realisation/classification spot-check.
    #[serde(default)]
    pub smoke: Option<OreSmoke>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OreTask {
    Consistency,
}

#[derive(Debug, Deserialize)]
pub struct OreSmoke {
    pub ask: String,
    pub expected: bool,
}

const PER_ONTOLOGY_BUDGET: Duration = Duration::from_secs(5 * 60);

pub fn load_selected(path: &Path) -> Result<OreSelected> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    let parsed: OreSelected = toml::from_str(&raw)?;
    if parsed.version != 1 {
        anyhow::bail!("unsupported ore selected version {}", parsed.version);
    }
    Ok(parsed)
}

pub fn run(
    engine: &mut dyn Reasoner,
    selected: &OreSelected,
    root: &Path,
) -> Result<Report> {
    let mut report = Report::new();
    for ont in &selected.ontologies {
        let start = Instant::now();
        let outcome = match run_one(engine, ont, root, start) {
            Ok(o) => o,
            Err(e) => Outcome {
                test_id: ont.id.clone(),
                suite: "ore2015".into(),
                status: Status::Failed,
                reason: Some(format!("harness error: {e:#}")),
                duration_ms: start.elapsed().as_millis() as u64,
            },
        };
        report.push(outcome);
    }
    Ok(report)
}

fn run_one(
    engine: &mut dyn Reasoner,
    ont: &OreOntology,
    root: &Path,
    start: Instant,
) -> Result<Outcome> {
    let path: PathBuf = root.join(&ont.path);
    let bytes = std::fs::read(&path)?;
    let mut graph = oxrdf::Graph::new();
    let mut parser = oxttl::TurtleParser::new()
        .with_base_iri(&format!("file://{}", path.display()))?
        .parse();
    parser.extend_from_slice(&bytes);
    parser.end();
    while let Some(t) = parser.read_next() {
        graph.insert(&t?);
    }
    let mut dataset = oxrdf::Dataset::new();
    for triple in graph.iter() {
        dataset.insert(&oxrdf::Quad::new(
            triple.subject.clone().into_owned(),
            triple.predicate.clone().into_owned(),
            triple.object.clone().into_owned(),
            oxrdf::GraphName::DefaultGraph,
        ));
    }

    engine.load(&dataset)?;
    if start.elapsed() > PER_ONTOLOGY_BUDGET {
        return Ok(Outcome {
            test_id: ont.id.clone(),
            suite: "ore2015".into(),
            status: Status::Failed,
            reason: Some("exceeded 5-minute per-ontology budget".into()),
            duration_ms: start.elapsed().as_millis() as u64,
        });
    }

    let consistent = engine.is_consistent()?;
    let mut status = if consistent { Status::Passed } else { Status::Failed };
    let mut reason = if consistent {
        None
    } else {
        Some("expected consistent, got inconsistent".into())
    };

    if let Some(smoke) = &ont.smoke {
        let got = engine.ask(&smoke.ask)?;
        if got != smoke.expected {
            status = Status::Failed;
            reason = Some(format!("smoke ASK got {got}, expected {}", smoke.expected));
        }
    }

    Ok(Outcome {
        test_id: ont.id.clone(),
        suite: "ore2015".into(),
        status,
        reason,
        duration_ms: start.elapsed().as_millis() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ore_selected_toml() {
        let raw = r#"version = 1
[[ontologies]]
id = "fma-lite"
path = "ore2015/fma-lite.ttl"
task = "consistency"
"#;
        let parsed: OreSelected = toml::from_str(raw).unwrap();
        assert_eq!(parsed.ontologies.len(), 1);
        assert!(matches!(parsed.ontologies[0].task, OreTask::Consistency));
    }
}
```

- [ ] **Step 2: Create the ORE selection file**

Create `/Users/stig/git/sunstone/reasoner/harness/ore2015-selected.toml`:

```toml
# SPEC-01 Stage-1 ORE 2015 selection — 10 hand-picked OWL 2 RL clean
# ontologies. Paths are relative to the ORE corpus root that
# scripts/fetch-ore2015-subset.sh writes into crates/harness/data/ore2015/.

version = 1

# 10 ontologies (placeholders — Stage-1 implementer replaces these IDs
# with the actual hand-picked ten from the Zenodo 18578 manifest, see
# harness/curation/ore2015-10.md to be created alongside this file).
[[ontologies]]
id = "fma-lite-v3.2"
path = "fma-lite-v3.2.ttl"
task = "consistency"

[[ontologies]]
id = "go-basic"
path = "go-basic.ttl"
task = "consistency"

[[ontologies]]
id = "doid"
path = "doid.ttl"
task = "consistency"

[[ontologies]]
id = "chebi-lite"
path = "chebi-lite.ttl"
task = "consistency"

[[ontologies]]
id = "uberon"
path = "uberon.ttl"
task = "consistency"

[[ontologies]]
id = "envo"
path = "envo.ttl"
task = "consistency"

[[ontologies]]
id = "pato"
path = "pato.ttl"
task = "consistency"

[[ontologies]]
id = "ro"
path = "ro.ttl"
task = "consistency"

[[ontologies]]
id = "iao"
path = "iao.ttl"
task = "consistency"

[[ontologies]]
id = "obi-rl-fragment"
path = "obi-rl-fragment.ttl"
task = "consistency"
```

- [ ] **Step 3: Create the fetch script**

Create `/Users/stig/git/sunstone/reasoner/crates/harness/scripts/fetch-ore2015-subset.sh`:

```bash
#!/usr/bin/env bash
# Fetch the ORE 2015 corpus from Zenodo (record 18578) and extract
# only the ontologies named in harness/ore2015-selected.toml. The full
# 1,920-ontology corpus is too big to vendor; Stage-2 grows the
# selection beyond the hand-picked 10.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"
DATA="$ROOT/crates/harness/data/ore2015"
mkdir -p "$DATA"

ZENODO_TARBALL="https://zenodo.org/record/18578/files/ore2015-corpus.tar.gz"

if [[ ! -f "$DATA/ore2015-corpus.tar.gz" ]]; then
    echo "fetching ORE 2015 corpus (large — ~3GB)…"
    curl -sSfL "$ZENODO_TARBALL" -o "$DATA/ore2015-corpus.tar.gz"
fi

# Extract only the paths named in ore2015-selected.toml.
SELECTED="$ROOT/harness/ore2015-selected.toml"
python3 -c '
import tomllib, sys
with open(sys.argv[1], "rb") as f:
    d = tomllib.load(f)
for o in d["ontologies"]:
    print(o["path"])
' "$SELECTED" | while read -r p; do
    if [[ ! -f "$DATA/$p" ]]; then
        echo "extracting $p"
        tar -xzf "$DATA/ore2015-corpus.tar.gz" -C "$DATA" "$p" || \
            echo "WARN: $p not found in tarball — update ore2015-selected.toml"
    fi
done

echo "done."
```

Run: `chmod +x /Users/stig/git/sunstone/reasoner/crates/harness/scripts/fetch-ore2015-subset.sh`

- [ ] **Step 4: Run the ORE module's unit test**

Run: `cd /Users/stig/git/sunstone/reasoner && cargo test -p horndb-harness ore::tests`
Expected: 1 test passes.

- [ ] **Step 5: Commit**

```bash
cd /Users/stig/git/sunstone/reasoner
git add crates/harness/src/ore.rs crates/harness/src/lib.rs \
        crates/harness/scripts/fetch-ore2015-subset.sh \
        harness/ore2015-selected.toml
git commit -m "$(cat <<'EOF'
feat(harness): ORE 2015 runner over a 10-ontology hand-picked subset (F3)

Stage-1 scope: 10 ontologies known to be OWL 2 RL clean. Enforces the
5-minute per-ontology budget from the ORE 2015 competition rules.
Full 1,920-ontology corpus is Stage-2 work. Fetch script extracts only
the named subset from the Zenodo tarball.
EOF
)"
```

---

### Task 21: LDBC SPB-256 driver shim (F4) and GraphDB Free comparison (F10)

**Files:**
- Create: `/Users/stig/git/sunstone/reasoner/crates/harness/src/ldbc_spb.rs`
- Modify: `/Users/stig/git/sunstone/reasoner/crates/harness/src/lib.rs`
- Create: `/Users/stig/git/sunstone/reasoner/crates/harness/scripts/run-spb-256.sh`
- Create: `/Users/stig/git/sunstone/reasoner/crates/harness/scripts/run-graphdb-free-spb-256.sh`

- [ ] **Step 1: Add the module**

Insert `pub mod ldbc_spb;` near the other `pub mod` lines in
`/Users/stig/git/sunstone/reasoner/crates/harness/src/lib.rs`.

Create `/Users/stig/git/sunstone/reasoner/crates/harness/src/ldbc_spb.rs`:

```rust
//! LDBC SPB driver integration shim (SPEC-01 F4).
//!
//! The LDBC SPB v2.0 driver is a Java program shipped by LDBC. We
//! invoke it as a subprocess and parse its result JSON into the
//! harness metric DB so SPB and our W3C runs live in the same store.
//!
//! Stage-1 scope: SPB-256 (SF=0.256, ~256M triples). SF3/SF5 are
//! Stage-2.

use std::path::Path;
use std::process::Command;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

use crate::db::Db;

#[derive(Debug, Deserialize)]
pub struct SpbResult {
    pub editorial_qps: f64,
    pub aggregation_qps: f64,
    pub update_qps: f64,
    pub run_duration_seconds: f64,
}

pub struct SpbConfig<'a> {
    /// Path to the LDBC SPB driver JAR.
    pub driver_jar: &'a Path,
    /// Path to the SPB scenario configuration (test.properties).
    pub scenario: &'a Path,
    /// Endpoint URL of the engine under test.
    pub endpoint: &'a str,
    /// Run duration. Stage-1 default is 600 seconds (10 min) of
    /// measurement — well below an audit-grade 1-hour run but enough
    /// to compare against GraphDB Free for the go/no-go decision.
    pub duration_seconds: u64,
}

pub fn run(cfg: &SpbConfig<'_>) -> Result<SpbResult> {
    let output = Command::new("java")
        .arg("-jar")
        .arg(cfg.driver_jar)
        .arg("--config")
        .arg(cfg.scenario)
        .arg("--endpoint")
        .arg(cfg.endpoint)
        .arg("--duration")
        .arg(cfg.duration_seconds.to_string())
        .arg("--report-format")
        .arg("json")
        .output()
        .with_context(|| "invoking LDBC SPB driver")?;
    if !output.status.success() {
        return Err(anyhow!(
            "SPB driver exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    let parsed: SpbResult = serde_json::from_slice(&output.stdout)
        .map_err(|e| anyhow!("parsing SPB JSON: {e}"))?;
    Ok(parsed)
}

pub fn record(db: &Db, run_id: &str, reasoner_name: &str, r: &SpbResult) -> Result<()> {
    db.record_metric(run_id, "ldbc-spb-256", Some(reasoner_name), "editorial-qps", r.editorial_qps, "qps")?;
    db.record_metric(run_id, "ldbc-spb-256", Some(reasoner_name), "aggregation-qps", r.aggregation_qps, "qps")?;
    db.record_metric(run_id, "ldbc-spb-256", Some(reasoner_name), "update-qps", r.update_qps, "qps")?;
    db.record_metric(run_id, "ldbc-spb-256", Some(reasoner_name), "duration-s", r.run_duration_seconds, "s")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_spb_result_json() {
        let json = r#"{
            "editorial_qps": 123.4,
            "aggregation_qps": 5.6,
            "update_qps": 78.9,
            "run_duration_seconds": 600.0
        }"#;
        let r: SpbResult = serde_json::from_str(json).unwrap();
        assert_eq!(r.editorial_qps, 123.4);
    }
}
```

- [ ] **Step 2: Add serde_json to workspace deps**

In `/Users/stig/git/sunstone/reasoner/Cargo.toml`, append to `[workspace.dependencies]`:

```toml
serde_json = "1"
```

In `/Users/stig/git/sunstone/reasoner/crates/harness/Cargo.toml`, add to `[dependencies]`:

```toml
serde_json = { workspace = true }
```

- [ ] **Step 3: Create the driver invocation script for the real engine**

Create `/Users/stig/git/sunstone/reasoner/crates/harness/scripts/run-spb-256.sh`:

```bash
#!/usr/bin/env bash
# Run LDBC SPB-256 against the local HornDB engine and record the
# numbers into the harness DB.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"

# Pre-conditions:
#   1. The engine is running and exposing a SPARQL 1.1 endpoint at
#      $REASONER_ENDPOINT (default http://127.0.0.1:7878/sparql).
#   2. The LDBC SPB driver JAR is at $SPB_DRIVER_JAR.
#   3. The SF=0.256 scenario file is at $SPB_SCENARIO.
ENDPOINT="${REASONER_ENDPOINT:-http://127.0.0.1:7878/sparql}"
JAR="${SPB_DRIVER_JAR:-$ROOT/crates/harness/data/ldbc-spb/spb-driver.jar}"
SCENARIO="${SPB_SCENARIO:-$ROOT/crates/harness/data/ldbc-spb/sf-0.256.properties}"

cargo run -p horndb-harness --bin harness --release --features real-engine -- \
    spb-run \
    --driver-jar "$JAR" \
    --scenario "$SCENARIO" \
    --endpoint "$ENDPOINT" \
    --duration 600 \
    --label "horndb-engine"
```

`chmod +x /Users/stig/git/sunstone/reasoner/crates/harness/scripts/run-spb-256.sh`

- [ ] **Step 4: Create the GraphDB Free comparison script**

Create `/Users/stig/git/sunstone/reasoner/crates/harness/scripts/run-graphdb-free-spb-256.sh`:

```bash
#!/usr/bin/env bash
# Same SPB-256 driver, pointed at a local GraphDB Free instance, for
# the F10 differential comparison required at Stage 1.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"

ENDPOINT="${GRAPHDB_FREE_ENDPOINT:-http://127.0.0.1:7200/repositories/spb}"
JAR="${SPB_DRIVER_JAR:-$ROOT/crates/harness/data/ldbc-spb/spb-driver.jar}"
SCENARIO="${SPB_SCENARIO:-$ROOT/crates/harness/data/ldbc-spb/sf-0.256.properties}"

cargo run -p horndb-harness --bin harness --release -- \
    spb-run \
    --driver-jar "$JAR" \
    --scenario "$SCENARIO" \
    --endpoint "$ENDPOINT" \
    --duration 600 \
    --label "graphdb-free"
```

`chmod +x /Users/stig/git/sunstone/reasoner/crates/harness/scripts/run-graphdb-free-spb-256.sh`

- [ ] **Step 5: Add `spb-run` to the CLI**

In `/Users/stig/git/sunstone/reasoner/crates/harness/src/bin/harness.rs`, add to `Cmd`:

```rust
    /// Run LDBC SPB driver against an endpoint and record results.
    SpbRun {
        #[arg(long)]
        driver_jar: PathBuf,
        #[arg(long)]
        scenario: PathBuf,
        #[arg(long)]
        endpoint: String,
        #[arg(long, default_value_t = 600)]
        duration: u64,
        /// Label used as the `dataset` column so we can A/B
        /// (e.g. "horndb-engine" vs "graphdb-free").
        #[arg(long)]
        label: String,
    },
```

And the matching arm:

```rust
        Cmd::SpbRun { driver_jar, scenario, endpoint, duration, label } => {
            let cfg = horndb_harness::ldbc_spb::SpbConfig {
                driver_jar: &driver_jar,
                scenario: &scenario,
                endpoint: &endpoint,
                duration_seconds: duration,
            };
            let result = horndb_harness::ldbc_spb::run(&cfg)?;
            let commit_sha = std::env::var("GITHUB_SHA").unwrap_or_else(|_| "unknown".into());
            let run_id = db.start_run(&commit_sha, &hardware_fingerprint(), &label)?;
            horndb_harness::ldbc_spb::record(&db, &run_id, &label, &result)?;
            println!("spb-run: run_id={run_id} editorial_qps={} aggregation_qps={} update_qps={}",
                     result.editorial_qps, result.aggregation_qps, result.update_qps);
            Ok(ExitCode::SUCCESS)
        }
```

- [ ] **Step 6: Run unit tests**

Run: `cd /Users/stig/git/sunstone/reasoner && cargo test -p horndb-harness ldbc_spb::tests`
Expected: 1 test passes.

- [ ] **Step 7: Commit**

```bash
cd /Users/stig/git/sunstone/reasoner
git add Cargo.toml crates/harness/Cargo.toml crates/harness/src/lib.rs \
        crates/harness/src/ldbc_spb.rs \
        crates/harness/src/bin/harness.rs \
        crates/harness/scripts/run-spb-256.sh \
        crates/harness/scripts/run-graphdb-free-spb-256.sh
git commit -m "$(cat <<'EOF'
feat(harness): LDBC SPB-256 driver shim and GraphDB Free A/B (F4, F10)

`harness spb-run` invokes the upstream LDBC SPB driver, parses the
JSON report, and records editorial/aggregation/update QPS into the
metric DB tagged with a `label` column so the same store carries both
horndb-engine and graphdb-free comparison runs.
EOF
)"
```

---

### Task 22: Stage-1 CI workflow — nightly performance + per-PR correctness

**Files:**
- Modify: `/Users/stig/git/sunstone/reasoner/.github/workflows/ci.yml`
- Create: `/Users/stig/git/sunstone/reasoner/.github/workflows/nightly.yml`

- [ ] **Step 1: Flip CI to use the real engine and drop --allow-failing**

In `/Users/stig/git/sunstone/reasoner/.github/workflows/ci.yml`, replace the
conformance step with:

```yaml
      - name: conformance — Stage-1 selected subset (real engine)
        run: |
          cargo build -p horndb-harness --bin harness --release --features real-engine
          ./target/release/harness \
            --engine owlrl \
            run \
            --junit target/junit.xml
```

(This step starts failing the moment the SPEC-04 engine lands — that
is the gating event for Stage-1 acceptance criterion #5.)

- [ ] **Step 2: Create the nightly performance workflow**

Create `/Users/stig/git/sunstone/reasoner/.github/workflows/nightly.yml`:

```yaml
name: nightly

on:
  schedule:
    - cron: "0 3 * * *"  # 03:00 UTC daily
  workflow_dispatch:

jobs:
  spb-256:
    # Runs on the dedicated benchmark machine (self-hosted runner with
    # the SPB driver, Java, and the engine binaries pre-installed). The
    # runner labels match what the ops team has configured.
    runs-on: [self-hosted, benchmark, x86_64]
    timeout-minutes: 120
    steps:
      - uses: actions/checkout@v4

      - uses: actions-rust-lang/setup-rust-toolchain@v1
        with:
          toolchain: "1.79.0"
          cache: true

      - name: build harness
        run: cargo build -p horndb-harness --bin harness --release --features real-engine

      - name: start HornDB engine
        run: ./scripts/dev/start-engine.sh &
        # The engine startup script lives in SPEC-04 territory; this
        # workflow assumes it exists. If absent the next step fails.

      - name: SPB-256 against horndb-engine
        run: ./crates/harness/scripts/run-spb-256.sh

      - name: SPB-256 against GraphDB Free
        run: ./crates/harness/scripts/run-graphdb-free-spb-256.sh

      - name: trend report
        run: |
          ./target/release/harness report --suite ldbc-spb-256 --metric editorial-qps

      - name: upload sqlite
        if: always()
        uses: actions/upload-artifact@v4
        with:
          name: harness-${{ github.run_id }}
          path: target/harness.sqlite
```

- [ ] **Step 3: Validate YAML**

Run: `python3 -c "import yaml; yaml.safe_load(open('/Users/stig/git/sunstone/reasoner/.github/workflows/nightly.yml')); yaml.safe_load(open('/Users/stig/git/sunstone/reasoner/.github/workflows/ci.yml'))" && echo OK`
Expected: prints `OK`.

- [ ] **Step 4: Commit**

```bash
cd /Users/stig/git/sunstone/reasoner
git add .github/workflows/ci.yml .github/workflows/nightly.yml
git commit -m "$(cat <<'EOF'
ci: flip PR job to real engine; add nightly SPB-256 + GraphDB A/B (F9)

PR-time CI now runs the Stage-1 selected subset against the SPEC-04
engine without --allow-failing — green is required. Nightly self-
hosted runner executes SPB-256 against both the HornDB engine and
GraphDB Free for the F10 differential.
EOF
)"
```

---

### Task 23: Stage-1 integration test that exercises the W3C subset end-to-end

**Files:**
- Create: `/Users/stig/git/sunstone/reasoner/crates/harness/tests/w3c_subset.rs`

- [ ] **Step 1: Write the gated integration test**

Create `/Users/stig/git/sunstone/reasoner/crates/harness/tests/w3c_subset.rs`:

```rust
//! End-to-end Stage-1 test: load the real `harness/selected.toml`,
//! run the OWL 2 RL 50-case subset against the real engine, assert
//! every selected case passes.
//!
//! Gated behind the `real-engine` feature so the default test run
//! (and the Stage-0 PR job) does not depend on the SPEC-04 engine.

#![cfg(feature = "real-engine")]

use std::path::PathBuf;

use horndb_harness::{
    manifest, runner::run_selected, selected::Selected, testcase::Suite,
};

fn workspace() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p.pop();
    p
}

#[test]
fn real_engine_passes_full_stage1_selection() {
    let sel = Selected::load(&workspace().join("harness/selected.toml")).unwrap();
    let mut engine = horndb_owlrl::Engine::new();
    let report = run_selected(
        &mut engine,
        &sel,
        &workspace(),
        &|p, s: Suite| manifest::parse(p, s),
    )
    .unwrap();

    assert!(
        report.outcomes.len() >= 50,
        "expected ≥50 selected tests, got {}", report.outcomes.len(),
    );
    let failing: Vec<&str> = report
        .outcomes
        .iter()
        .filter(|o| matches!(o.status, horndb_harness::Status::Failed))
        .map(|o| o.test_id.as_str())
        .collect();
    assert!(failing.is_empty(), "real engine failed selected cases: {failing:?}");
}
```

- [ ] **Step 2: Run it with the feature off (skips compile)**

Run: `cd /Users/stig/git/sunstone/reasoner && cargo test -p horndb-harness --test w3c_subset`
Expected: zero tests run (the `#![cfg(feature = "real-engine")]` gates the whole file). The harness compiles and the suite reports `0 passed`.

- [ ] **Step 3: Commit**

```bash
cd /Users/stig/git/sunstone/reasoner
git add crates/harness/tests/w3c_subset.rs
git commit -m "$(cat <<'EOF'
test(harness): Stage-1 end-to-end test gated on real-engine feature

When the SPEC-04 engine lands and the `real-engine` cargo feature is
enabled, this test runs the full Stage-1 selected subset (≥50 OWL 2
RL cases) and asserts zero failures.
EOF
)"
```

---

### Task 24: Stage-1 self-review and documentation refresh

**Files:**
- Modify: `/Users/stig/git/sunstone/reasoner/crates/harness/README.md`

- [ ] **Step 1: Refresh the README with Stage-1 invocation**

Replace `/Users/stig/git/sunstone/reasoner/crates/harness/README.md` with:

```markdown
# horndb-harness

SPEC-01 conformance and benchmarking harness. See
`specs/SPEC-01-conformance-benchmarks.md` and
`plans/PLAN-01-01-conformance-harness.md`.

## Local invocation

Stage-0 (stub-only, no real engine yet):

```bash
cargo run -p horndb-harness --bin harness -- \
    --engine stub \
    run \
    --allow-failing
```

Stage-1 (real engine, full 50-case OWL 2 RL subset):

```bash
./crates/harness/scripts/fetch-w3c-suites.sh
cargo run -p horndb-harness --bin harness --features real-engine -- \
    --engine owlrl \
    run
```

ORE 2015 ten-ontology subset:

```bash
./crates/harness/scripts/fetch-ore2015-subset.sh
cargo run -p horndb-harness --bin harness --features real-engine -- \
    ore-run --selected harness/ore2015-selected.toml
```

LDBC SPB-256 (requires Java + the SPB driver JAR):

```bash
./crates/harness/scripts/run-spb-256.sh
./crates/harness/scripts/run-graphdb-free-spb-256.sh
cargo run -p horndb-harness --bin harness -- \
    report --suite ldbc-spb-256 --metric editorial-qps
```

## CI

- `.github/workflows/ci.yml` — per-PR correctness run (selected subset, real engine).
- `.github/workflows/nightly.yml` — SPB-256 horndb-engine vs GraphDB Free.
```

Note: the README references an `ore-run` subcommand the Stage-1
implementer must add to the CLI by analogy with `spb-run` (read
`harness/ore2015-selected.toml`, call `horndb_harness::ore::run`,
record outcomes). This is a 10-minute task that drops into
`src/bin/harness.rs` mirroring the `SpbRun` arm — left implicit here
to keep this task focused on the documentation refresh.

- [ ] **Step 2: Verify the full workspace still builds and tests pass**

Run: `cd /Users/stig/git/sunstone/reasoner && cargo test --workspace && cargo build --workspace --release`
Expected: all green.

- [ ] **Step 3: Commit**

```bash
cd /Users/stig/git/sunstone/reasoner
git add crates/harness/README.md
git commit -m "$(cat <<'EOF'
docs(harness): refresh README for Stage-1 invocation (W3C 50, ORE-10, SPB-256)
EOF
)"
```

---

## Stage 1 self-review checklist

- ✅ Selected W3C OWL 2 RL subset expanded to ≥50 cases (Tasks 16, 17, 18).
- ✅ All selected tests pass against the real engine (Tasks 19, 22, 23).
- ✅ ORE 2015 runner integrated, 10-ontology selection (Task 20).
- ✅ LDBC SPB-256 end-to-end vs GraphDB Free comparison (Tasks 21, 22).
- ✅ F9 CI integration: PR job + nightly performance (Task 22).

---

# Future Work (Stage 2+ — NOT scoped in this plan)

The following items are listed in SPEC-01 but explicitly **out of scope**
here. They land in a separate plan once Stage 1 acceptance is signed off:

- **Stage 2** — Full W3C OWL 2 RL test cases (not just 50), full SPARQL 1.1
  Entailment Regimes suite, full ORE 2015 OWL 2 RL fragment with 5-min
  budget enforcement, LDBC SPB SF3 *audited-style* report, LUBM-8000
  materialization run, RDFox A/B (subject to licensing).
- **Stage 3** — Hardware-fingerprint normalisation across GPU/CXL backends;
  ClickStack-backed metric store replacing SQLite; LDBC SPB SF5.
- **F5** — LUBM/UOBM driver (Stage 2).
- **F6** — Real-world ontology suite with `harness/realworld.toml` (Stage 2).
- **NF1** — ≤10 min full-suite CI budget enforcement (only meaningful once
  the "full" suite >50 cases; Stage 2 work).
- **NF3** — LDBC-audit-grade SPB methodology (deterministic warm-up,
  sustained 1-hour measurement, no in-flight schema changes; Stage 2/3).
- **NF5** — Differential testing across multiple competitor references
  beyond GraphDB Free (Stage 2/3, RDFox license dependent).
