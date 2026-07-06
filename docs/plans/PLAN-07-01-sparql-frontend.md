---
status: executed
date: 2026-05-24
scope: "SPEC-07 SPARQL 1.1 Frontend — Stage 0 + Stage 1"
---

# SPEC-07 SPARQL 1.1 Frontend — Stage 0 + Stage 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the public SPARQL 1.1 query/update surface for HornDB — a parser (via the `spargebra` crate), algebra translator, basic planner, and stub-backed in-memory executor that can answer SELECT/ASK/CONSTRUCT and run minimal INSERT/DELETE updates, ship behind an HTTP `/query` endpoint, and demonstrably pass a hand-picked subset of the W3C SPARQL 1.1 Query Test Suite under the default "simple" entailment regime.

**Architecture:** Sit in `crates/sparql/`. The crate is layered: (1) `parser` thin wrapper around `spargebra` producing our internal `Query`/`Update` enums; (2) `algebra` mirrors `spargebra::algebra` with our own simplified AST that hides the upstream churn surface; (3) `plan` lowers algebra into a `PhysicalPlan` tree whose leaves are `BgpScan` nodes routed to a pluggable `Executor` trait (SPEC-03 placeholder); (4) `exec` provides a built-in `MemExecutor` over a HashMap-of-triples backing store so the crate is testable in isolation, plus a `Runtime` that walks `PhysicalPlan` nodes; (5) `results` serialises bindings to SPARQL JSON Results, CSV, TSV (XML deferred); (6) `regime` provides a trait with two implementations — `SimpleRegime` (no inference, used for W3C SPARQL 1.1 Query tests) and `MaterializedOwlRlRegime` (assumes closure is already in the store, so query-time behaviour is identical to simple — the regime distinction is a marker on the answer set headers and the dataset wiring); (7) `server` exposes an `axum`-based `/query` and `/update` endpoint. Property paths `/` and `^` are evaluated by expansion to algebra `Join`; `*`, `+`, `?`, `|`, `!` return a typed `UnsupportedPathOp` error in Stage 1.

**Tech Stack:** Rust 2021, MSRV 1.87. `spargebra = "0.4"` (parser, MIT/Apache, mandatory per SPEC-07). `oxrdf = "0.3"` for shared term types (Iri/Literal/BlankNode/Variable — these are re-exported by `spargebra`). `sparesults = "0.3"` for the SPARQL JSON/XML results serializer (optional, gated). `axum = "0.8"` + `tokio = "1"` + `hyper` (transitive) for the HTTP server. `serde = "1"` + `serde_json = "1"` for JSON. `thiserror`, `anyhow` from the workspace. Dev-dependencies: `pretty_assertions`, `insta` for snapshot tests, `tempfile`.

---

## Scope boundaries (read before starting)

**In Stage 0/1 (this plan):**
- F1 Parser via `spargebra`.
- F2 Algebra translation (subset: BGP, Join, LeftJoin, Filter, Project, Distinct, Slice, Union, OrderBy, Values, Extend; **not** Group/Aggregate, **not** Minus, **not** Service).
- F3 Planner: BGPs lower to a single `BgpScan` against the `Executor` trait. No cost model — left-deep order by triple-pattern appearance.
- F4 Entailment regimes: `Simple` (default, no inference) and `MaterializedOwlRl` (marker only — assumes closure already materialised by SPEC-04/05 into the store).
- F5 Update: literal-triple `INSERT DATA` and `DELETE DATA` only. Direct writes through the `Store` trait; no template `INSERT { … } WHERE { … }` form.
- F6 Streaming results — Stage 1 buffers per-query (acceptable for selected W3C subset); streaming is a Future Work follow-up.
- F7 Protocol: `axum` server with `/query` (GET + POST) and `/update` (POST) per SPARQL 1.1 Protocol.
- Query forms: SELECT, ASK, CONSTRUCT. **DESCRIBE deferred.**
- Acceptance: a hand-picked subset of the W3C SPARQL 1.1 Query Test Suite mirrored into `harness/selected.toml`, all green.

**Deferred to Future Work (out of scope for this plan):**
- DESCRIBE.
- Full Update vocabulary (`LOAD`, `CLEAR`, `DROP`, `INSERT … WHERE`, `DELETE … WHERE`, `MODIFY`).
- Backward-chained entailment mode.
- Property paths `*`, `+`, `?`, `|`, `!` (only `/` and `^` supported).
- Graph Store Protocol.
- EXPLAIN pragma.
- Full streaming result-format pipeline (Stage 2).
- SPARQL XML Results (the SerDe lives in `sparesults` but we expose only JSON/CSV/TSV in Stage 1).
- Aggregates / GROUP BY / HAVING.
- MINUS, SERVICE.
- DBSP-routed updates (SPEC-06).

---

## File structure

The crate `crates/sparql/` is restructured from a single placeholder into:

```
crates/sparql/
├── Cargo.toml
└── src/
    ├── lib.rs                # re-exports, crate-level docs
    ├── error.rs              # SparqlError enum
    ├── parser.rs             # spargebra wrapper -> ParsedQuery/ParsedUpdate
    ├── algebra/
    │   ├── mod.rs            # Algebra enum (BGP/Join/...), Triple, Term
    │   └── translate.rs      # spargebra::Query -> Algebra
    ├── plan/
    │   ├── mod.rs            # PhysicalPlan enum
    │   └── planner.rs        # Algebra -> PhysicalPlan
    ├── exec/
    │   ├── mod.rs            # Executor trait, Bindings type
    │   ├── runtime.rs        # walks PhysicalPlan, drives Executor
    │   └── mem.rs            # MemExecutor + MemStore (in-crate)
    ├── regime/
    │   ├── mod.rs            # EntailmentRegime trait
    │   ├── simple.rs
    │   └── owl_rl.rs         # MaterializedOwlRl marker impl
    ├── update.rs             # InsertData / DeleteData on Store trait
    ├── results/
    │   ├── mod.rs            # ResultFormat enum, dispatch
    │   ├── json.rs           # SPARQL JSON Results (hand-written; sparesults later)
    │   ├── csv.rs
    │   └── tsv.rs
    └── server/
        ├── mod.rs            # build_router(state) -> axum::Router
        ├── query.rs          # /query handler
        └── update.rs         # /update handler
tests/
    ├── parse_smoke.rs
    ├── algebra_translate.rs
    ├── planner_smoke.rs
    ├── exec_select.rs
    ├── exec_construct.rs
    ├── exec_ask.rs
    ├── update_insert_delete.rs
    ├── server_http.rs
    └── w3c_suite.rs          # drives the selected subset (skipped if fixtures absent)
```

`harness/selected.toml` (in `crates/harness/`) gains a `[sparql_query]` section listing the test IDs we commit to. The harness wiring itself is owned by SPEC-01; this plan only **declares** the section and **lands the fixtures** under `crates/harness/tests/fixtures/sparql11/`.

`Store` trait (the crate-local seam between SPARQL and storage/executor) lives in `exec/mod.rs` alongside `Executor`. It's the same place future SPEC-02/SPEC-03 implementations will plug into.

---

## Task 1: Workspace prep — bump MSRV, add shared dependencies

**Files:**
- Modify: `Cargo.toml` (workspace root)

- [ ] **Step 1: Read current workspace manifest**

Run: `cat Cargo.toml`
Expected: shows `rust-version = "1.75"` and the existing `[workspace.dependencies]` block with only `anyhow` and `thiserror`.

- [ ] **Step 2: Update MSRV and add shared crates**

Replace the `[workspace.package]` and `[workspace.dependencies]` blocks so the file reads:

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
rust-version = "1.87"
license = "Apache-2.0"
repository = "https://github.com/sunstoneinstitute/horndb"
authors = ["Sunstone Institute"]

[workspace.dependencies]
anyhow = "1"
thiserror = "1"
spargebra = "0.4"
oxrdf = "0.3"
sparesults = "0.3"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
axum = "0.8"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
tower = "0.5"
hyper = "1"
pretty_assertions = "1"
insta = "1"
tempfile = "3"
```

The MSRV bump from 1.75 → 1.87 is required because `spargebra >= 0.4.0` mandates 1.87. Per SPEC-07 risk list, `spargebra` is the chosen parser; adopting it pins our MSRV.

- [ ] **Step 3: Verify workspace still resolves**

Run: `cargo metadata --no-deps --format-version=1 > /dev/null`
Expected: exit code 0, no output. (This validates manifest syntax without compiling.)

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml
git commit -m "$(cat <<'EOF'
chore(workspace): bump MSRV to 1.87, add SPARQL frontend deps

Pulls spargebra/oxrdf/sparesults/axum/serde_json/tokio into workspace
dependencies so SPEC-07 crates can inherit consistent versions.
MSRV bumped from 1.75 to 1.87 to satisfy spargebra >= 0.4.0.
EOF
)"
```

---

## Task 2: Wire up the `sparql` crate manifest

**Files:**
- Modify: `crates/sparql/Cargo.toml`

- [ ] **Step 1: Replace the placeholder manifest**

Overwrite `crates/sparql/Cargo.toml` with:

```toml
[package]
name = "horndb-sparql"
version = "0.0.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
publish = false

[features]
default = ["server"]
server = ["dep:axum", "dep:tokio", "dep:tower", "dep:hyper"]

[dependencies]
spargebra = { workspace = true }
oxrdf = { workspace = true }
sparesults = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
thiserror = { workspace = true }
anyhow = { workspace = true }
axum = { workspace = true, optional = true }
tokio = { workspace = true, optional = true }
tower = { workspace = true, optional = true }
hyper = { workspace = true, optional = true }

[dev-dependencies]
pretty_assertions = { workspace = true }
insta = { workspace = true }
tempfile = { workspace = true }
tokio = { workspace = true }
```

- [ ] **Step 2: Verify the crate compiles (empty source still in place)**

Run: `cargo check -p horndb-sparql`
Expected: dependencies download and the placeholder `lib.rs` compiles. Exit code 0. (First run will take several minutes.)

- [ ] **Step 3: Commit**

```bash
git add crates/sparql/Cargo.toml Cargo.lock
git commit -m "$(cat <<'EOF'
build(sparql): declare dependencies for SPEC-07 frontend

Adds spargebra/oxrdf/sparesults plus optional axum/tokio for the
embedded HTTP /query and /update endpoints (default-on via the
`server` feature).
EOF
)"
```

---

## Task 3: Crate skeleton (modules + top-level error type)

**Files:**
- Create: `crates/sparql/src/error.rs`
- Modify: `crates/sparql/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/sparql/tests/parse_smoke.rs`:

```rust
//! Smoke test for the public crate surface. Verifies the modules
//! and error type are exported as documented in the plan.

use horndb_sparql::SparqlError;

#[test]
fn error_type_displays() {
    let err = SparqlError::Parse("nope".into());
    let rendered = format!("{err}");
    assert!(rendered.contains("nope"), "got: {rendered}");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p horndb-sparql --test parse_smoke`
Expected: FAIL with `unresolved import 'horndb_sparql::SparqlError'`.

- [ ] **Step 3: Add the error type**

Create `crates/sparql/src/error.rs`:

```rust
//! Crate-wide error type for the SPARQL frontend.

use thiserror::Error;

/// Errors produced by the SPARQL frontend.
#[derive(Debug, Error)]
pub enum SparqlError {
    /// The input could not be parsed by `spargebra`.
    #[error("parse error: {0}")]
    Parse(String),

    /// The parsed AST contains a construct we do not translate yet.
    #[error("unsupported algebra construct: {0}")]
    UnsupportedAlgebra(String),

    /// The query references a property-path operator outside Stage 1 scope.
    #[error("unsupported property-path operator: {0}")]
    UnsupportedPathOp(String),

    /// The planner could not lower the algebra to a physical plan.
    #[error("planner error: {0}")]
    Planner(String),

    /// The executor rejected a plan or pattern.
    #[error("executor error: {0}")]
    Executor(String),

    /// I/O error.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Convenience `Result` alias.
pub type Result<T> = std::result::Result<T, SparqlError>;
```

- [ ] **Step 4: Replace `lib.rs` with the module skeleton**

Overwrite `crates/sparql/src/lib.rs`:

```rust
//! horndb-sparql — SPARQL 1.1 frontend.
//!
//! See `specs/SPEC-07-sparql-frontend.md` for scope and acceptance
//! criteria. This crate provides:
//!
//! * a parser wrapping the `spargebra` crate,
//! * an internal algebra (a stable subset of `spargebra::algebra`),
//! * a planner producing `PhysicalPlan` trees,
//! * a runtime that drives a pluggable [`exec::Executor`] (SPEC-03),
//! * SPARQL JSON / CSV / TSV result serialisers,
//! * (with the `server` feature) an embedded `axum`-based HTTP
//!   endpoint exposing `/query` and `/update`.

pub mod algebra;
pub mod error;
pub mod exec;
pub mod parser;
pub mod plan;
pub mod regime;
pub mod results;
pub mod update;

#[cfg(feature = "server")]
pub mod server;

pub use error::{Result, SparqlError};
```

- [ ] **Step 5: Create empty module stubs so the skeleton compiles**

Run:

```bash
mkdir -p crates/sparql/src/algebra crates/sparql/src/plan \
         crates/sparql/src/exec crates/sparql/src/regime \
         crates/sparql/src/results crates/sparql/src/server
```

Then create each placeholder file with a single line:

```bash
for f in \
  crates/sparql/src/parser.rs \
  crates/sparql/src/algebra/mod.rs \
  crates/sparql/src/plan/mod.rs \
  crates/sparql/src/exec/mod.rs \
  crates/sparql/src/regime/mod.rs \
  crates/sparql/src/results/mod.rs \
  crates/sparql/src/update.rs \
  crates/sparql/src/server/mod.rs ; do
  printf '//! Placeholder — populated in later plan tasks.\n' > "$f"
done
```

- [ ] **Step 6: Run the smoke test**

Run: `cargo test -p horndb-sparql --test parse_smoke`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/sparql/src crates/sparql/tests/parse_smoke.rs
git commit -m "$(cat <<'EOF'
feat(sparql): scaffold crate modules and SparqlError type

Lays out parser/algebra/plan/exec/regime/results/update/server
modules as empty placeholders so subsequent tasks have a stable
import surface. Adds SparqlError as the crate-wide error enum.
EOF
)"
```

---

## Task 4: Parser wrapper over `spargebra`

**Files:**
- Modify: `crates/sparql/src/parser.rs`
- Create: `crates/sparql/tests/parser_basic.rs`

- [ ] **Step 1: Write the failing tests**

Create `crates/sparql/tests/parser_basic.rs`:

```rust
use horndb_sparql::parser::{parse_query, parse_update, ParsedQuery, ParsedUpdate};

#[test]
fn parses_minimal_select() {
    let q = parse_query("SELECT ?s WHERE { ?s ?p ?o }").expect("must parse");
    assert!(matches!(q, ParsedQuery::Select { .. }));
}

#[test]
fn parses_ask() {
    let q = parse_query("ASK { ?s ?p ?o }").expect("must parse");
    assert!(matches!(q, ParsedQuery::Ask { .. }));
}

#[test]
fn parses_construct() {
    let q = parse_query(
        "CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o }"
    ).expect("must parse");
    assert!(matches!(q, ParsedQuery::Construct { .. }));
}

#[test]
fn rejects_garbage_query() {
    let err = parse_query("THIS IS NOT SPARQL").unwrap_err();
    assert!(format!("{err}").contains("parse error"));
}

#[test]
fn parses_insert_data() {
    let u = parse_update(
        "INSERT DATA { <http://ex/a> <http://ex/p> <http://ex/b> }"
    ).expect("must parse");
    assert!(matches!(u, ParsedUpdate::InsertData { .. }));
}

#[test]
fn parses_delete_data() {
    let u = parse_update(
        "DELETE DATA { <http://ex/a> <http://ex/p> <http://ex/b> }"
    ).expect("must parse");
    assert!(matches!(u, ParsedUpdate::DeleteData { .. }));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p horndb-sparql --test parser_basic`
Expected: FAIL with unresolved imports.

- [ ] **Step 3: Implement the parser wrapper**

Overwrite `crates/sparql/src/parser.rs`:

```rust
//! Thin wrapper around the `spargebra` crate.
//!
//! We re-shape `spargebra::Query` / `spargebra::Update` into a smaller
//! `ParsedQuery` / `ParsedUpdate` enum that the rest of the crate
//! pattern-matches against. This isolates the upstream churn surface:
//! `spargebra` does not yet guarantee API stability, and our acceptance
//! tests rely on the W3C grammar via spargebra, not on spargebra's own
//! API shape.

use crate::error::{Result, SparqlError};
use spargebra::{Query, Update};

/// A successfully parsed SPARQL query, classified by query form.
///
/// The variants carry the upstream `spargebra::Query` payload verbatim
/// (which holds the algebra) so that downstream code can pattern-match
/// without re-parsing.
#[derive(Debug, Clone)]
pub enum ParsedQuery {
    Select { inner: Query },
    Ask { inner: Query },
    Construct { inner: Query },
    /// DESCRIBE is recognised by the parser but rejected here in
    /// Stage 1; left as its own variant so the regression surface
    /// shows up at the `match` site, not as a silent fallthrough.
    Describe { inner: Query },
}

/// A successfully parsed SPARQL update. Stage 1 only supports
/// `INSERT DATA` and `DELETE DATA` literal forms.
#[derive(Debug, Clone)]
pub enum ParsedUpdate {
    InsertData { inner: Update },
    DeleteData { inner: Update },
    /// Any other update form (LOAD/CLEAR/DROP/INSERT WHERE/...) is
    /// parsed but flagged as out-of-scope at runtime.
    UnsupportedForm { inner: Update },
}

/// Parse a SPARQL 1.1 query string.
///
/// Defaults: no base IRI, no prefix mappings beyond those declared
/// in the query itself.
pub fn parse_query(input: &str) -> Result<ParsedQuery> {
    let q = Query::parse(input, None).map_err(|e| SparqlError::Parse(e.to_string()))?;
    Ok(match &q {
        Query::Select { .. } => ParsedQuery::Select { inner: q },
        Query::Ask { .. } => ParsedQuery::Ask { inner: q },
        Query::Construct { .. } => ParsedQuery::Construct { inner: q },
        Query::Describe { .. } => ParsedQuery::Describe { inner: q },
    })
}

/// Parse a SPARQL 1.1 update string.
///
/// In Stage 1 we recognise `INSERT DATA` and `DELETE DATA` only.
/// Other update forms parse successfully but are classified as
/// `UnsupportedForm`; the executor returns an explicit error when
/// asked to apply them.
pub fn parse_update(input: &str) -> Result<ParsedUpdate> {
    let u = Update::parse(input, None).map_err(|e| SparqlError::Parse(e.to_string()))?;

    // `spargebra::Update` is a sequence of `GraphUpdateOperation`s.
    // We classify by the *first* operation in Stage 1; multi-op
    // updates degrade to `UnsupportedForm` and the executor rejects
    // them. This is fine for the W3C subset we're targeting.
    use spargebra::GraphUpdateOperation;
    match u.operations.first() {
        Some(GraphUpdateOperation::InsertData { .. }) if u.operations.len() == 1 => {
            Ok(ParsedUpdate::InsertData { inner: u })
        }
        Some(GraphUpdateOperation::DeleteData { .. }) if u.operations.len() == 1 => {
            Ok(ParsedUpdate::DeleteData { inner: u })
        }
        Some(_) => Ok(ParsedUpdate::UnsupportedForm { inner: u }),
        None => Err(SparqlError::Parse("update contains no operations".into())),
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p horndb-sparql --test parser_basic`
Expected: 6 passed. If any of the `spargebra` types don't match (the upstream API may have evolved between 0.4.x patch releases — verify the actual enum field names with `cargo doc --open -p spargebra` or `rustdoc-json`), adjust the match arms but keep the public `ParsedQuery`/`ParsedUpdate` shape stable.

- [ ] **Step 5: Commit**

```bash
git add crates/sparql/src/parser.rs crates/sparql/tests/parser_basic.rs
git commit -m "$(cat <<'EOF'
feat(sparql): parser wrapper over spargebra

Provides parse_query/parse_update returning crate-owned
ParsedQuery/ParsedUpdate enums so downstream modules pattern-match
on stable types. Classifies query forms (SELECT/ASK/CONSTRUCT/
DESCRIBE) and Stage-1 update forms (INSERT DATA / DELETE DATA).
EOF
)"
```

---

## Task 5: Internal algebra — types

**Files:**
- Modify: `crates/sparql/src/algebra/mod.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/sparql/tests/algebra_types.rs`:

```rust
use horndb_sparql::algebra::{Algebra, Term, TriplePattern, Var};

#[test]
fn build_a_bgp() {
    let tp = TriplePattern {
        subject: Term::Var(Var::new("s")),
        predicate: Term::Iri("http://ex/p".into()),
        object: Term::Var(Var::new("o")),
    };
    let alg = Algebra::Bgp { patterns: vec![tp] };
    match alg {
        Algebra::Bgp { patterns } => assert_eq!(patterns.len(), 1),
        other => panic!("expected Bgp, got {other:?}"),
    }
}
```

- [ ] **Step 2: Run it to confirm failure**

Run: `cargo test -p horndb-sparql --test algebra_types`
Expected: FAIL — unresolved imports.

- [ ] **Step 3: Define the algebra types**

Overwrite `crates/sparql/src/algebra/mod.rs`:

```rust
//! Internal SPARQL algebra. A stable subset of `spargebra::algebra`.
//!
//! Why our own enum and not raw `spargebra::algebra::GraphPattern`?
//! Two reasons:
//!   * we want to constrain which operators the planner/executor are
//!     allowed to see (Stage 1 supports a smaller set than spargebra
//!     can produce);
//!   * upstream variants change between patch releases — keeping our
//!     algebra owned in-crate localises the breakage.

pub mod translate;

use std::sync::Arc;

/// A SPARQL variable. Stored as an interned name; equality is by name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Var(Arc<str>);

impl Var {
    pub fn new(name: impl Into<Arc<str>>) -> Self {
        Self(name.into())
    }
    pub fn name(&self) -> &str {
        &self.0
    }
}

/// A SPARQL term as it appears inside a triple pattern.
///
/// We hold IRIs and string-form literals as owned strings in Stage 1.
/// SPEC-02 will replace these with dictionary IDs; the algebra is
/// allowed to carry either via the `Term` enum extending later.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Term {
    Var(Var),
    Iri(String),
    BlankNode(String),
    /// A literal in N-Triples lexical form, e.g. `"hello"` or
    /// `"42"^^<http://www.w3.org/2001/XMLSchema#integer>`.
    Literal(String),
}

/// A SPARQL triple pattern.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TriplePattern {
    pub subject: Term,
    pub predicate: Term,
    pub object: Term,
}

/// A SPARQL expression — Stage 1 supports a deliberately tiny subset
/// (variable refs, term constants, equality, conjunction/disjunction,
/// arithmetic comparisons over the lexical form).
///
/// Aggregates, builtin call functions beyond the listed comparisons,
/// regex, etc. are out of scope for this plan.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Term(Term),
    Eq(Box<Expr>, Box<Expr>),
    Ne(Box<Expr>, Box<Expr>),
    Lt(Box<Expr>, Box<Expr>),
    Gt(Box<Expr>, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
    Bound(Var),
}

/// Algebra operators supported in Stage 1.
///
/// Notable omissions vs the full W3C algebra:
///   * Group/Aggregate (no GROUP BY in Stage 1)
///   * Minus
///   * Service
///   * PathOpStar/Plus/Question/Alt/Inv/NegSet — only `Seq` (`/`)
///     and `Inverse` (`^`) collapse into expanded triple patterns
///     in [`translate`].
#[derive(Debug, Clone, PartialEq)]
pub enum Algebra {
    Bgp { patterns: Vec<TriplePattern> },
    Join { left: Box<Algebra>, right: Box<Algebra> },
    LeftJoin { left: Box<Algebra>, right: Box<Algebra>, expr: Option<Expr> },
    Filter { expr: Expr, inner: Box<Algebra> },
    Union { left: Box<Algebra>, right: Box<Algebra> },
    Project { vars: Vec<Var>, inner: Box<Algebra> },
    Distinct { inner: Box<Algebra> },
    Slice { inner: Box<Algebra>, start: usize, length: Option<usize> },
    OrderBy { inner: Box<Algebra>, keys: Vec<(Expr, OrderDir)> },
    /// `BIND (?e AS ?v)` and `VALUES`-style row sets reduce to Extend.
    Extend { inner: Box<Algebra>, var: Var, expr: Expr },
    Values { vars: Vec<Var>, rows: Vec<Vec<Option<Term>>> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderDir {
    Asc,
    Desc,
}
```

- [ ] **Step 4: Run the test**

Run: `cargo test -p horndb-sparql --test algebra_types`
Expected: PASS.

- [ ] **Step 5: Add a placeholder `translate` module so the `pub mod translate;` line resolves**

Create `crates/sparql/src/algebra/translate.rs`:

```rust
//! Algebra translation from `spargebra::algebra::GraphPattern` to
//! [`crate::algebra::Algebra`]. Populated in the next task.
```

- [ ] **Step 6: Verify the crate still compiles**

Run: `cargo check -p horndb-sparql`
Expected: exit code 0.

- [ ] **Step 7: Commit**

```bash
git add crates/sparql/src/algebra/ crates/sparql/tests/algebra_types.rs
git commit -m "$(cat <<'EOF'
feat(sparql): internal Algebra/Term/TriplePattern types

Defines the in-crate SPARQL algebra enum that the planner and
executor operate on. Deliberately a subset of spargebra's grammar:
no aggregates, no Minus, no Service, no Kleene-star path operators
in Stage 1.
EOF
)"
```

---

## Task 6: Algebra translation from `spargebra`

**Files:**
- Modify: `crates/sparql/src/algebra/translate.rs`
- Create: `crates/sparql/tests/algebra_translate.rs`

- [ ] **Step 1: Write the failing tests**

Create `crates/sparql/tests/algebra_translate.rs`:

```rust
use horndb_sparql::algebra::{translate, Algebra};
use horndb_sparql::parser::{parse_query, ParsedQuery};

fn alg_of(query: &str) -> Algebra {
    let q = parse_query(query).expect("parse");
    let inner = match q {
        ParsedQuery::Select { inner }
        | ParsedQuery::Ask { inner }
        | ParsedQuery::Construct { inner } => inner,
        ParsedQuery::Describe { .. } => panic!("describe not supported here"),
    };
    translate::translate_query(&inner).expect("translate")
}

#[test]
fn select_one_pattern_is_project_over_bgp() {
    let alg = alg_of("SELECT ?s WHERE { ?s <http://ex/p> ?o }");
    match alg {
        Algebra::Project { vars, inner } => {
            assert_eq!(vars.len(), 1);
            assert_eq!(vars[0].name(), "s");
            assert!(matches!(*inner, Algebra::Bgp { .. }));
        }
        other => panic!("expected Project, got {other:?}"),
    }
}

#[test]
fn ask_is_project_zero_vars() {
    // ASK queries reduce to a "does the BGP produce any row" check,
    // which we represent as a Project with no vars wrapped around the
    // pattern. The runtime turns this into a boolean.
    let alg = alg_of("ASK { ?s ?p ?o }");
    match alg {
        Algebra::Project { vars, .. } => assert!(vars.is_empty()),
        other => panic!("expected Project, got {other:?}"),
    }
}

#[test]
fn join_of_two_bgps() {
    let alg = alg_of(
        "SELECT * WHERE { ?s <http://ex/p> ?o . ?o <http://ex/q> ?z }"
    );
    // Two patterns over distinct predicates land in a single BGP node
    // (spargebra merges them); we just verify the BGP carries both.
    let inner = match alg {
        Algebra::Project { inner, .. } => *inner,
        other => panic!("expected Project, got {other:?}"),
    };
    match inner {
        Algebra::Bgp { patterns } => assert_eq!(patterns.len(), 2),
        other => panic!("expected Bgp, got {other:?}"),
    }
}

#[test]
fn rejects_minus() {
    let q = parse_query(
        "SELECT * WHERE { ?s ?p ?o MINUS { ?s <http://ex/q> ?z } }"
    ).expect("parse");
    let inner = match q {
        ParsedQuery::Select { inner } => inner,
        _ => unreachable!(),
    };
    let err = translate::translate_query(&inner).unwrap_err();
    assert!(format!("{err}").contains("Minus"), "got: {err}");
}

#[test]
fn rejects_kleene_star_path() {
    let q = parse_query(
        "SELECT ?x WHERE { ?x <http://ex/p>* ?y }"
    ).expect("parse");
    let inner = match q {
        ParsedQuery::Select { inner } => inner,
        _ => unreachable!(),
    };
    let err = translate::translate_query(&inner).unwrap_err();
    assert!(format!("{err}").contains("property-path"), "got: {err}");
}
```

- [ ] **Step 2: Run to confirm failures**

Run: `cargo test -p horndb-sparql --test algebra_translate`
Expected: FAIL — `translate_query` unresolved.

- [ ] **Step 3: Implement the translator**

Overwrite `crates/sparql/src/algebra/translate.rs`:

```rust
//! Algebra translation from `spargebra` AST to our internal [`Algebra`].
//!
//! Stage 1 supports a deliberately small operator set; constructs we
//! do not yet handle return `SparqlError::UnsupportedAlgebra` (or
//! `UnsupportedPathOp` for the Kleene-star property paths) so the
//! planner never has to defend against them.

use crate::algebra::{Algebra, Expr, OrderDir, Term, TriplePattern, Var};
use crate::error::{Result, SparqlError};
use spargebra::algebra::{Expression, GraphPattern, OrderExpression, PropertyPathExpression};
use spargebra::term::{
    GroundTerm, NamedNodePattern, TermPattern, TriplePattern as SpgTriplePattern, Variable,
};
use spargebra::Query;

/// Top-level entry: lower a parsed `spargebra::Query` to [`Algebra`].
pub fn translate_query(q: &Query) -> Result<Algebra> {
    match q {
        Query::Select { pattern, dataset: _, base_iri: _ } => {
            let inner = translate_pattern(pattern)?;
            let vars = collect_visible_vars(pattern);
            Ok(Algebra::Project { vars, inner: Box::new(inner) })
        }
        Query::Ask { pattern, dataset: _, base_iri: _ } => {
            let inner = translate_pattern(pattern)?;
            Ok(Algebra::Project { vars: Vec::new(), inner: Box::new(inner) })
        }
        Query::Construct { template: _, pattern, dataset: _, base_iri: _ } => {
            // The CONSTRUCT template is preserved separately by the
            // runtime; here we only return the WHERE-clause algebra.
            // The planner is responsible for re-attaching the
            // template via Runtime::run_construct.
            translate_pattern(pattern)
        }
        Query::Describe { .. } => Err(SparqlError::UnsupportedAlgebra("DESCRIBE".into())),
    }
}

/// Lower a `GraphPattern` (spargebra) to our `Algebra`.
fn translate_pattern(p: &GraphPattern) -> Result<Algebra> {
    match p {
        GraphPattern::Bgp { patterns } => {
            let mut out = Vec::with_capacity(patterns.len());
            for tp in patterns {
                out.push(translate_triple(tp)?);
            }
            Ok(Algebra::Bgp { patterns: out })
        }
        GraphPattern::Path { subject, path, object } => {
            // Stage 1 supports only Seq (`/`) and Inverse (`^`); both
            // expand to additional triple patterns (fresh variables
            // for the intermediate node in `Seq`, swapped subject/
            // object for `Inverse`). Kleene-star, alternation, etc.
            // are rejected.
            let patterns = expand_path(subject, path, object)?;
            Ok(Algebra::Bgp { patterns })
        }
        GraphPattern::Join { left, right } => Ok(Algebra::Join {
            left: Box::new(translate_pattern(left)?),
            right: Box::new(translate_pattern(right)?),
        }),
        GraphPattern::LeftJoin { left, right, expression } => Ok(Algebra::LeftJoin {
            left: Box::new(translate_pattern(left)?),
            right: Box::new(translate_pattern(right)?),
            expr: expression.as_ref().map(translate_expr).transpose()?,
        }),
        GraphPattern::Filter { expr, inner } => Ok(Algebra::Filter {
            expr: translate_expr(expr)?,
            inner: Box::new(translate_pattern(inner)?),
        }),
        GraphPattern::Union { left, right } => Ok(Algebra::Union {
            left: Box::new(translate_pattern(left)?),
            right: Box::new(translate_pattern(right)?),
        }),
        GraphPattern::Project { inner, variables } => Ok(Algebra::Project {
            vars: variables.iter().map(translate_var).collect(),
            inner: Box::new(translate_pattern(inner)?),
        }),
        GraphPattern::Distinct { inner } => Ok(Algebra::Distinct {
            inner: Box::new(translate_pattern(inner)?),
        }),
        GraphPattern::Slice { inner, start, length } => Ok(Algebra::Slice {
            inner: Box::new(translate_pattern(inner)?),
            start: *start,
            length: *length,
        }),
        GraphPattern::OrderBy { inner, expression } => {
            let mut keys = Vec::with_capacity(expression.len());
            for oe in expression {
                let (e, dir) = match oe {
                    OrderExpression::Asc(e) => (translate_expr(e)?, OrderDir::Asc),
                    OrderExpression::Desc(e) => (translate_expr(e)?, OrderDir::Desc),
                };
                keys.push((e, dir));
            }
            Ok(Algebra::OrderBy {
                inner: Box::new(translate_pattern(inner)?),
                keys,
            })
        }
        GraphPattern::Extend { inner, variable, expression } => Ok(Algebra::Extend {
            inner: Box::new(translate_pattern(inner)?),
            var: translate_var(variable),
            expr: translate_expr(expression)?,
        }),
        GraphPattern::Values { variables, bindings } => {
            let vars = variables.iter().map(translate_var).collect();
            let rows = bindings
                .iter()
                .map(|row| {
                    row.iter()
                        .map(|cell| cell.as_ref().map(ground_term_to_term))
                        .collect()
                })
                .collect();
            Ok(Algebra::Values { vars, rows })
        }
        GraphPattern::Minus { .. } => Err(SparqlError::UnsupportedAlgebra("Minus".into())),
        GraphPattern::Service { .. } => Err(SparqlError::UnsupportedAlgebra("Service".into())),
        GraphPattern::Group { .. } => Err(SparqlError::UnsupportedAlgebra("Group".into())),
        GraphPattern::Reduced { .. } => {
            Err(SparqlError::UnsupportedAlgebra("Reduced".into()))
        }
        GraphPattern::Graph { .. } => Err(SparqlError::UnsupportedAlgebra("Graph".into())),
    }
}

fn translate_triple(tp: &SpgTriplePattern) -> Result<TriplePattern> {
    Ok(TriplePattern {
        subject: term_pattern_to_term(&tp.subject)?,
        predicate: named_node_pattern_to_term(&tp.predicate)?,
        object: term_pattern_to_term(&tp.object)?,
    })
}

fn term_pattern_to_term(tp: &TermPattern) -> Result<Term> {
    Ok(match tp {
        TermPattern::NamedNode(n) => Term::Iri(n.as_str().to_owned()),
        TermPattern::BlankNode(b) => Term::BlankNode(b.as_str().to_owned()),
        TermPattern::Literal(l) => Term::Literal(l.to_string()),
        TermPattern::Variable(v) => Term::Var(translate_var(v)),
        TermPattern::Triple(_) => {
            return Err(SparqlError::UnsupportedAlgebra("RDF-star triple term".into()))
        }
    })
}

fn named_node_pattern_to_term(np: &NamedNodePattern) -> Result<Term> {
    Ok(match np {
        NamedNodePattern::NamedNode(n) => Term::Iri(n.as_str().to_owned()),
        NamedNodePattern::Variable(v) => Term::Var(translate_var(v)),
    })
}

fn translate_var(v: &Variable) -> Var {
    Var::new(v.as_str().to_owned())
}

fn ground_term_to_term(gt: &GroundTerm) -> Term {
    match gt {
        GroundTerm::NamedNode(n) => Term::Iri(n.as_str().to_owned()),
        GroundTerm::Literal(l) => Term::Literal(l.to_string()),
        GroundTerm::Triple(_) => Term::Literal("<<rdf-star unsupported>>".into()),
    }
}

fn translate_expr(e: &Expression) -> Result<Expr> {
    use Expression as E;
    Ok(match e {
        E::NamedNode(n) => Expr::Term(Term::Iri(n.as_str().to_owned())),
        E::Literal(l) => Expr::Term(Term::Literal(l.to_string())),
        E::Variable(v) => Expr::Term(Term::Var(translate_var(v))),
        E::Equal(a, b) => Expr::Eq(Box::new(translate_expr(a)?), Box::new(translate_expr(b)?)),
        E::SameTerm(a, b) => {
            Expr::Eq(Box::new(translate_expr(a)?), Box::new(translate_expr(b)?))
        }
        E::Less(a, b) => Expr::Lt(Box::new(translate_expr(a)?), Box::new(translate_expr(b)?)),
        E::Greater(a, b) => {
            Expr::Gt(Box::new(translate_expr(a)?), Box::new(translate_expr(b)?))
        }
        E::And(a, b) => Expr::And(Box::new(translate_expr(a)?), Box::new(translate_expr(b)?)),
        E::Or(a, b) => Expr::Or(Box::new(translate_expr(a)?), Box::new(translate_expr(b)?)),
        E::Not(a) => Expr::Not(Box::new(translate_expr(a)?)),
        E::Bound(v) => Expr::Bound(translate_var(v)),
        other => {
            return Err(SparqlError::UnsupportedAlgebra(format!(
                "expression: {other:?}"
            )));
        }
    })
}

fn collect_visible_vars(p: &GraphPattern) -> Vec<Var> {
    // SELECT * means "all in-scope vars"; for Stage 1 we walk the
    // pattern once and dedup by name. Order matches first appearance.
    let mut seen: Vec<Var> = Vec::new();
    let mut push = |v: &Variable, acc: &mut Vec<Var>| {
        let new = translate_var(v);
        if !acc.iter().any(|x| x.name() == new.name()) {
            acc.push(new);
        }
    };

    fn walk(p: &GraphPattern, acc: &mut Vec<Var>, push: &mut impl FnMut(&Variable, &mut Vec<Var>)) {
        match p {
            GraphPattern::Bgp { patterns } => {
                for tp in patterns {
                    if let TermPattern::Variable(v) = &tp.subject { push(v, acc); }
                    if let NamedNodePattern::Variable(v) = &tp.predicate { push(v, acc); }
                    if let TermPattern::Variable(v) = &tp.object { push(v, acc); }
                }
            }
            GraphPattern::Path { subject, object, .. } => {
                if let TermPattern::Variable(v) = subject { push(v, acc); }
                if let TermPattern::Variable(v) = object { push(v, acc); }
            }
            GraphPattern::Join { left, right }
            | GraphPattern::Union { left, right }
            | GraphPattern::LeftJoin { left, right, .. } => {
                walk(left, acc, push);
                walk(right, acc, push);
            }
            GraphPattern::Filter { inner, .. }
            | GraphPattern::Distinct { inner }
            | GraphPattern::Slice { inner, .. }
            | GraphPattern::OrderBy { inner, .. }
            | GraphPattern::Reduced { inner }
            | GraphPattern::Graph { inner, .. }
            | GraphPattern::Group { inner, .. } => walk(inner, acc, push),
            GraphPattern::Project { variables, .. } => {
                for v in variables {
                    push(v, acc);
                }
            }
            GraphPattern::Extend { inner, variable, .. } => {
                walk(inner, acc, push);
                push(variable, acc);
            }
            GraphPattern::Values { variables, .. } => {
                for v in variables {
                    push(v, acc);
                }
            }
            GraphPattern::Minus { .. } | GraphPattern::Service { .. } => {}
        }
    }

    walk(p, &mut seen, &mut push);
    seen
}

/// Expand a (Stage-1 supported) property-path expression into a flat
/// list of triple patterns. Only `Seq` (`/`) and `Inverse` (`^`) and
/// a bare `NamedNode` predicate are supported.
fn expand_path(
    subject: &TermPattern,
    path: &PropertyPathExpression,
    object: &TermPattern,
) -> Result<Vec<TriplePattern>> {
    let mut out = Vec::new();
    expand_path_into(subject, path, object, &mut out, &mut 0)?;
    Ok(out)
}

fn expand_path_into(
    subject: &TermPattern,
    path: &PropertyPathExpression,
    object: &TermPattern,
    out: &mut Vec<TriplePattern>,
    fresh: &mut usize,
) -> Result<()> {
    use PropertyPathExpression as P;
    match path {
        P::NamedNode(n) => {
            out.push(TriplePattern {
                subject: term_pattern_to_term(subject)?,
                predicate: Term::Iri(n.as_str().to_owned()),
                object: term_pattern_to_term(object)?,
            });
            Ok(())
        }
        P::Reverse(inner) => {
            // ^p between s and o == p between o and s
            expand_path_into(object, inner, subject, out, fresh)
        }
        P::Sequence(a, b) => {
            // (a / b) between s and o introduces a fresh var v with
            // s -a-> v -b-> o
            let v = Var::new(format!("__path_seq_{}", *fresh));
            *fresh += 1;
            let mid_subject = TermPattern::Variable(
                spargebra::term::Variable::new(format!("__path_seq_{}", *fresh - 1))
                    .expect("valid fresh var"),
            );
            expand_path_into(subject, a, &mid_subject, out, fresh)?;
            expand_path_into(&mid_subject, b, object, out, fresh)?;
            let _ = v; // var name already injected through mid_subject
            Ok(())
        }
        other => Err(SparqlError::UnsupportedPathOp(format!("{other:?}"))),
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p horndb-sparql --test algebra_translate`
Expected: 5 passed.

If `spargebra::algebra::GraphPattern` variant names differ in the installed 0.4.x patch version (e.g. `Path` vs `PropertyPath`), adjust the match arms accordingly; the strategy stays the same.

- [ ] **Step 5: Commit**

```bash
git add crates/sparql/src/algebra/translate.rs crates/sparql/tests/algebra_translate.rs
git commit -m "$(cat <<'EOF'
feat(sparql): translate spargebra AST to internal Algebra

SELECT/ASK/CONSTRUCT all funnel through translate_query; unsupported
constructs (DESCRIBE, MINUS, GROUP, SERVICE, Kleene-star paths)
return typed SparqlError variants so the planner and executor never
see them. Sequence (/) and Inverse (^) property paths are expanded
to additional triple patterns with fresh intermediate variables.
EOF
)"
```

---

## Task 7: Executor trait and in-crate MemExecutor

**Files:**
- Modify: `crates/sparql/src/exec/mod.rs`
- Create: `crates/sparql/src/exec/mem.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/sparql/tests/exec_mem.rs`:

```rust
use horndb_sparql::algebra::{Term, TriplePattern, Var};
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::exec::{Bindings, Executor};

fn t(s: &str, p: &str, o: &str) -> (String, String, String) {
    (s.into(), p.into(), o.into())
}

fn pat_var(s: &str) -> Term {
    Term::Var(Var::new(s))
}
fn pat_iri(s: &str) -> Term {
    Term::Iri(s.into())
}

#[test]
fn mem_executor_matches_single_pattern() {
    let mut s = MemStore::default();
    s.insert(t("http://ex/a", "http://ex/p", "http://ex/b"));
    s.insert(t("http://ex/a", "http://ex/p", "http://ex/c"));
    s.insert(t("http://ex/x", "http://ex/q", "http://ex/y"));

    let pat = TriplePattern {
        subject: pat_iri("http://ex/a"),
        predicate: pat_iri("http://ex/p"),
        object: pat_var("o"),
    };
    let result: Vec<Bindings> = s.scan_bgp(std::slice::from_ref(&pat))
        .expect("scan")
        .collect();
    assert_eq!(result.len(), 2);
    let mut got: Vec<String> = result
        .iter()
        .map(|b| match b.get("o").unwrap() {
            Term::Iri(s) => s.clone(),
            other => panic!("unexpected term: {other:?}"),
        })
        .collect();
    got.sort();
    assert_eq!(got, vec!["http://ex/b".to_owned(), "http://ex/c".to_owned()]);
}

#[test]
fn mem_executor_joins_two_patterns_on_shared_var() {
    let mut s = MemStore::default();
    s.insert(t("http://ex/a", "http://ex/p", "http://ex/b"));
    s.insert(t("http://ex/b", "http://ex/q", "http://ex/c"));
    s.insert(t("http://ex/z", "http://ex/q", "http://ex/c"));

    let p1 = TriplePattern {
        subject: pat_iri("http://ex/a"),
        predicate: pat_iri("http://ex/p"),
        object: pat_var("o"),
    };
    let p2 = TriplePattern {
        subject: pat_var("o"),
        predicate: pat_iri("http://ex/q"),
        object: pat_var("z"),
    };

    let result: Vec<Bindings> = s.scan_bgp(&[p1, p2]).expect("scan").collect();
    assert_eq!(result.len(), 1);
    let b = &result[0];
    assert_eq!(b.get("o").unwrap(), &Term::Iri("http://ex/b".into()));
    assert_eq!(b.get("z").unwrap(), &Term::Iri("http://ex/c".into()));
}
```

- [ ] **Step 2: Run it to confirm failure**

Run: `cargo test -p horndb-sparql --test exec_mem`
Expected: FAIL — unresolved imports.

- [ ] **Step 3: Define the executor trait**

Overwrite `crates/sparql/src/exec/mod.rs`:

```rust
//! Executor seam: SPARQL planner -> storage/join backend.
//!
//! Stage 1 ships a single in-crate implementation [`mem::MemExecutor`]
//! over a `HashMap<(s,p,o)>`. SPEC-03 (WCOJ engine) will provide a
//! production implementation through the same trait.

pub mod mem;
pub mod runtime;

use crate::algebra::{Term, TriplePattern, Var};
use crate::error::Result;
use std::collections::BTreeMap;

/// A single SPARQL solution mapping.
///
/// We use `BTreeMap` so the order of variables in serialised results
/// is deterministic for snapshot tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Bindings {
    inner: BTreeMap<String, Term>,
}

impl Bindings {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn get(&self, var: &str) -> Option<&Term> {
        self.inner.get(var)
    }
    pub fn set(&mut self, var: impl Into<String>, term: Term) {
        self.inner.insert(var.into(), term);
    }
    pub fn vars(&self) -> impl Iterator<Item = (&str, &Term)> {
        self.inner.iter().map(|(k, v)| (k.as_str(), v))
    }
    pub fn extend_compat(&self, other: &Bindings) -> Option<Bindings> {
        // Compatible: every shared var has the same term. Merge wins.
        let mut out = self.clone();
        for (k, v) in &other.inner {
            match out.inner.get(k) {
                Some(existing) if existing != v => return None,
                _ => {
                    out.inner.insert(k.clone(), v.clone());
                }
            }
        }
        Some(out)
    }
    /// Return the set of variables bound in this row.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.inner.keys().map(|s| s.as_str())
    }
    /// Number of bound variables. Useful in tests and slicing.
    pub fn len(&self) -> usize {
        self.inner.len()
    }
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

/// The single seam Stage 1 needs from the storage/join backend.
/// SPEC-03 will eventually back this with Leapfrog Triejoin; in the
/// meantime [`mem::MemStore`] satisfies it for tests.
pub trait Executor {
    /// Iterate solutions to a BGP. Implementations are free to
    /// optimise — `MemStore` uses a naive nested loop.
    fn scan_bgp(
        &self,
        patterns: &[TriplePattern],
    ) -> Result<Box<dyn Iterator<Item = Bindings> + '_>>;
}

/// A storage-side write seam used by [`crate::update`].
///
/// `Store` is intentionally separate from `Executor` so that read-only
/// backends (e.g. mmap'd HDT) can implement only the read side.
pub trait Store {
    fn insert_triple(&mut self, subject: Term, predicate: Term, object: Term);
    fn delete_triple(&mut self, subject: &Term, predicate: &Term, object: &Term);
}

/// Convenience: a backend that is both an `Executor` and a `Store`.
pub trait FullBackend: Executor + Store {}
impl<T: Executor + Store> FullBackend for T {}

/// Helper used by the executor: bind a single pattern against a
/// concrete triple, returning the new bindings or `None` if the
/// constants don't match.
pub(crate) fn unify_one(
    pat: &TriplePattern,
    triple: &(String, String, String),
    prior: &Bindings,
) -> Option<Bindings> {
    let mut out = prior.clone();
    for (term, val) in [
        (&pat.subject, &triple.0),
        (&pat.predicate, &triple.1),
        (&pat.object, &triple.2),
    ] {
        match term {
            Term::Var(v) => {
                let new = Term::Iri(val.clone());
                match out.get(v.name()) {
                    Some(existing) if existing != &new => return None,
                    _ => out.set(v.name().to_owned(), new),
                }
            }
            Term::Iri(s) => {
                if s != val {
                    return None;
                }
            }
            Term::Literal(s) => {
                if s != val {
                    return None;
                }
            }
            Term::BlankNode(s) => {
                if s != val {
                    return None;
                }
            }
        }
    }
    Some(out)
}
```

- [ ] **Step 4: Implement `MemStore`**

Create `crates/sparql/src/exec/mem.rs`:

```rust
//! Hash-set backed in-memory triple store. Stage 1 only.
//!
//! Triples are stored as `(String, String, String)` — i.e. all terms
//! are kept as their N-Triples lexical form. This is intentionally
//! simple; SPEC-02 introduces the real dictionary-encoded store.

use crate::algebra::{Term, TriplePattern};
use crate::error::Result;
use crate::exec::{unify_one, Bindings, Executor, Store};
use std::collections::HashSet;

/// In-memory triple store. Clone-on-write semantics — each
/// `MemStore` is independent.
#[derive(Debug, Default, Clone)]
pub struct MemStore {
    triples: HashSet<(String, String, String)>,
}

impl MemStore {
    /// Insert a single triple from raw lexical-form strings.
    pub fn insert(&mut self, triple: (String, String, String)) {
        self.triples.insert(triple);
    }
    /// Number of triples currently stored. Stable; useful in tests.
    pub fn len(&self) -> usize {
        self.triples.len()
    }
    pub fn is_empty(&self) -> bool {
        self.triples.is_empty()
    }
}

fn term_to_lex(t: &Term) -> String {
    match t {
        Term::Iri(s) | Term::Literal(s) | Term::BlankNode(s) => s.clone(),
        Term::Var(v) => panic!("term_to_lex called on Var({})", v.name()),
    }
}

impl Executor for MemStore {
    fn scan_bgp(
        &self,
        patterns: &[TriplePattern],
    ) -> Result<Box<dyn Iterator<Item = Bindings> + '_>> {
        // Naive left-deep nested loop. Adequate for our test sizes
        // (W3C suite fixtures are tiny). SPEC-03 will replace this.
        let mut current: Vec<Bindings> = vec![Bindings::new()];
        for pat in patterns {
            let mut next: Vec<Bindings> = Vec::new();
            for row in &current {
                for triple in &self.triples {
                    if let Some(b) = unify_one(pat, triple, row) {
                        next.push(b);
                    }
                }
            }
            current = next;
            if current.is_empty() {
                break;
            }
        }
        Ok(Box::new(current.into_iter()))
    }
}

impl Store for MemStore {
    fn insert_triple(&mut self, subject: Term, predicate: Term, object: Term) {
        self.triples.insert((
            term_to_lex(&subject),
            term_to_lex(&predicate),
            term_to_lex(&object),
        ));
    }
    fn delete_triple(&mut self, subject: &Term, predicate: &Term, object: &Term) {
        self.triples.remove(&(
            term_to_lex(subject),
            term_to_lex(predicate),
            term_to_lex(object),
        ));
    }
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p horndb-sparql --test exec_mem`
Expected: 2 passed.

- [ ] **Step 6: Commit**

```bash
git add crates/sparql/src/exec/mod.rs crates/sparql/src/exec/mem.rs \
        crates/sparql/tests/exec_mem.rs
git commit -m "$(cat <<'EOF'
feat(sparql): Executor/Store traits and MemStore backend

Defines the seam SPEC-03 will eventually implement. The in-crate
MemStore (HashSet<(s,p,o)>) is enough to drive the Stage-1 W3C
SPARQL subset and unblocks all downstream planner/runtime tests
without waiting on SPEC-02/03.
EOF
)"
```

---

## Task 8: Planner — algebra to PhysicalPlan

**Files:**
- Modify: `crates/sparql/src/plan/mod.rs`
- Create: `crates/sparql/src/plan/planner.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/sparql/tests/planner_smoke.rs`:

```rust
use horndb_sparql::algebra::{Algebra, Term, TriplePattern, Var};
use horndb_sparql::plan::{planner, PhysicalPlan};

fn bgp(pat: TriplePattern) -> Algebra {
    Algebra::Bgp { patterns: vec![pat] }
}

#[test]
fn plans_bgp_as_scan() {
    let alg = bgp(TriplePattern {
        subject: Term::Var(Var::new("s")),
        predicate: Term::Iri("http://ex/p".into()),
        object: Term::Var(Var::new("o")),
    });
    let plan = planner::plan(&alg).expect("plan");
    assert!(matches!(plan, PhysicalPlan::BgpScan { .. }));
}

#[test]
fn plans_project_over_bgp() {
    let inner = bgp(TriplePattern {
        subject: Term::Var(Var::new("s")),
        predicate: Term::Iri("http://ex/p".into()),
        object: Term::Var(Var::new("o")),
    });
    let alg = Algebra::Project {
        vars: vec![Var::new("s")],
        inner: Box::new(inner),
    };
    let plan = planner::plan(&alg).expect("plan");
    match plan {
        PhysicalPlan::Project { vars, inner } => {
            assert_eq!(vars.len(), 1);
            assert!(matches!(*inner, PhysicalPlan::BgpScan { .. }));
        }
        other => panic!("expected Project, got {other:?}"),
    }
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p horndb-sparql --test planner_smoke`
Expected: FAIL — `planner::plan` unresolved.

- [ ] **Step 3: Define `PhysicalPlan`**

Overwrite `crates/sparql/src/plan/mod.rs`:

```rust
//! Physical plan tree. Each node is one work unit the runtime
//! executes against an [`crate::exec::Executor`].

pub mod planner;

use crate::algebra::{Expr, OrderDir, Term, TriplePattern, Var};

#[derive(Debug, Clone, PartialEq)]
pub enum PhysicalPlan {
    /// Leaf: scan a BGP via the executor.
    BgpScan { patterns: Vec<TriplePattern> },
    /// Cartesian/equi-join of two child plans on shared variables.
    Join { left: Box<PhysicalPlan>, right: Box<PhysicalPlan> },
    /// Left-outer-join, optional ON expression.
    LeftJoin {
        left: Box<PhysicalPlan>,
        right: Box<PhysicalPlan>,
        expr: Option<Expr>,
    },
    /// Filter rows by a boolean expression.
    Filter { expr: Expr, inner: Box<PhysicalPlan> },
    /// UNION of two compatible plans.
    Union { left: Box<PhysicalPlan>, right: Box<PhysicalPlan> },
    /// Restrict output columns.
    Project { vars: Vec<Var>, inner: Box<PhysicalPlan> },
    /// Deduplicate rows.
    Distinct { inner: Box<PhysicalPlan> },
    /// OFFSET/LIMIT.
    Slice { inner: Box<PhysicalPlan>, start: usize, length: Option<usize> },
    /// ORDER BY.
    OrderBy { inner: Box<PhysicalPlan>, keys: Vec<(Expr, OrderDir)> },
    /// BIND.
    Extend { inner: Box<PhysicalPlan>, var: Var, expr: Expr },
    /// VALUES row source.
    Values { vars: Vec<Var>, rows: Vec<Vec<Option<Term>>> },
}
```

- [ ] **Step 4: Implement the planner**

Create `crates/sparql/src/plan/planner.rs`:

```rust
//! Algebra → PhysicalPlan.
//!
//! Stage 1 is a thin 1:1 lowering. There is **no** cost model: BGP
//! patterns are sent to the executor in the textual order they appear,
//! and the executor (or, later, SPEC-03) is responsible for join
//! ordering. This avoids us building a planner we'd just throw away.

use crate::algebra::Algebra;
use crate::error::{Result, SparqlError};
use crate::plan::PhysicalPlan;

pub fn plan(alg: &Algebra) -> Result<PhysicalPlan> {
    Ok(match alg {
        Algebra::Bgp { patterns } => PhysicalPlan::BgpScan { patterns: patterns.clone() },
        Algebra::Join { left, right } => PhysicalPlan::Join {
            left: Box::new(plan(left)?),
            right: Box::new(plan(right)?),
        },
        Algebra::LeftJoin { left, right, expr } => PhysicalPlan::LeftJoin {
            left: Box::new(plan(left)?),
            right: Box::new(plan(right)?),
            expr: expr.clone(),
        },
        Algebra::Filter { expr, inner } => PhysicalPlan::Filter {
            expr: expr.clone(),
            inner: Box::new(plan(inner)?),
        },
        Algebra::Union { left, right } => PhysicalPlan::Union {
            left: Box::new(plan(left)?),
            right: Box::new(plan(right)?),
        },
        Algebra::Project { vars, inner } => PhysicalPlan::Project {
            vars: vars.clone(),
            inner: Box::new(plan(inner)?),
        },
        Algebra::Distinct { inner } => PhysicalPlan::Distinct {
            inner: Box::new(plan(inner)?),
        },
        Algebra::Slice { inner, start, length } => PhysicalPlan::Slice {
            inner: Box::new(plan(inner)?),
            start: *start,
            length: *length,
        },
        Algebra::OrderBy { inner, keys } => PhysicalPlan::OrderBy {
            inner: Box::new(plan(inner)?),
            keys: keys.clone(),
        },
        Algebra::Extend { inner, var, expr } => PhysicalPlan::Extend {
            inner: Box::new(plan(inner)?),
            var: var.clone(),
            expr: expr.clone(),
        },
        Algebra::Values { vars, rows } => PhysicalPlan::Values {
            vars: vars.clone(),
            rows: rows.clone(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algebra::{Term, TriplePattern, Var};

    #[test]
    fn empty_bgp_plans_to_empty_scan() {
        let plan = plan(&Algebra::Bgp { patterns: vec![] }).unwrap();
        assert_eq!(plan, PhysicalPlan::BgpScan { patterns: vec![] });
    }

    #[test]
    fn join_lowers_both_sides() {
        let bgp = Algebra::Bgp {
            patterns: vec![TriplePattern {
                subject: Term::Var(Var::new("s")),
                predicate: Term::Iri("p".into()),
                object: Term::Var(Var::new("o")),
            }],
        };
        let alg = Algebra::Join {
            left: Box::new(bgp.clone()),
            right: Box::new(bgp),
        };
        match plan(&alg).unwrap() {
            PhysicalPlan::Join { .. } => {}
            other => panic!("expected Join, got {other:?}"),
        }
    }

    // Compile-time witness that SparqlError is the error path so we
    // don't accidentally start panicking on lower failures.
    #[allow(dead_code)]
    fn err_path() -> Result<PhysicalPlan> {
        Err(SparqlError::Planner("never".into()))
    }
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p horndb-sparql --test planner_smoke && cargo test -p horndb-sparql --lib plan`
Expected: all green.

- [ ] **Step 6: Commit**

```bash
git add crates/sparql/src/plan/ crates/sparql/tests/planner_smoke.rs
git commit -m "$(cat <<'EOF'
feat(sparql): Algebra-to-PhysicalPlan lowering

1:1 lowering — no cost model in Stage 1. BGP scans route to
Executor::scan_bgp; join order is whatever the executor decides.
This unblocks the runtime without committing to a planner we'd
discard once SPEC-03 lands.
EOF
)"
```

---

## Task 9: Runtime — walk PhysicalPlan against Executor

**Files:**
- Create: `crates/sparql/src/exec/runtime.rs`
- Create: `crates/sparql/tests/exec_select.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/sparql/tests/exec_select.rs`:

```rust
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::exec::runtime::Runtime;
use horndb_sparql::exec::Store;
use horndb_sparql::algebra::Term;
use horndb_sparql::parser::{parse_query, ParsedQuery};
use horndb_sparql::algebra::translate::translate_query;
use horndb_sparql::plan::planner;

fn iri(s: &str) -> Term {
    Term::Iri(s.into())
}

fn make_store() -> MemStore {
    let mut s = MemStore::default();
    s.insert_triple(iri("http://ex/alice"), iri("http://ex/knows"), iri("http://ex/bob"));
    s.insert_triple(iri("http://ex/alice"), iri("http://ex/knows"), iri("http://ex/carol"));
    s.insert_triple(iri("http://ex/bob"), iri("http://ex/knows"), iri("http://ex/dave"));
    s
}

fn run(q: &str, store: &MemStore) -> Vec<horndb_sparql::exec::Bindings> {
    let inner = match parse_query(q).unwrap() {
        ParsedQuery::Select { inner }
        | ParsedQuery::Ask { inner }
        | ParsedQuery::Construct { inner } => inner,
        ParsedQuery::Describe { .. } => panic!("describe"),
    };
    let alg = translate_query(&inner).unwrap();
    let plan = planner::plan(&alg).unwrap();
    Runtime::new(store).run(&plan).unwrap().collect()
}

#[test]
fn select_star_returns_all_subjects() {
    let s = make_store();
    let rows = run(
        "SELECT ?s WHERE { ?s <http://ex/knows> ?o }",
        &s,
    );
    let mut subjs: Vec<String> = rows.iter()
        .map(|b| match b.get("s").unwrap() {
            Term::Iri(s) => s.clone(),
            _ => panic!(),
        })
        .collect();
    subjs.sort();
    subjs.dedup();
    assert_eq!(subjs, vec!["http://ex/alice".to_string(), "http://ex/bob".to_string()]);
}

#[test]
fn select_distinct_dedups() {
    let s = make_store();
    let rows = run(
        "SELECT DISTINCT ?s WHERE { ?s <http://ex/knows> ?o }",
        &s,
    );
    assert_eq!(rows.len(), 2);
}

#[test]
fn select_filter_eq() {
    let s = make_store();
    let rows = run(
        r#"SELECT ?o WHERE { ?s <http://ex/knows> ?o . FILTER(?s = <http://ex/alice>) }"#,
        &s,
    );
    assert_eq!(rows.len(), 2);
}

#[test]
fn select_limit_offset() {
    let s = make_store();
    let rows = run(
        "SELECT ?o WHERE { ?s <http://ex/knows> ?o } LIMIT 2",
        &s,
    );
    assert_eq!(rows.len(), 2);
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p horndb-sparql --test exec_select`
Expected: FAIL — `Runtime` unresolved.

- [ ] **Step 3: Implement the runtime**

Create `crates/sparql/src/exec/runtime.rs`:

```rust
//! Iterator-style runtime over [`PhysicalPlan`]. Each plan node
//! yields a `Vec<Bindings>` because Stage 1 materialises per-node;
//! true streaming is a Future Work item.

use crate::algebra::{Expr, OrderDir, Term, Var};
use crate::error::{Result, SparqlError};
use crate::exec::{Bindings, Executor};
use crate::plan::PhysicalPlan;

pub struct Runtime<'a, E: Executor + ?Sized> {
    exec: &'a E,
}

impl<'a, E: Executor + ?Sized> Runtime<'a, E> {
    pub fn new(exec: &'a E) -> Self {
        Self { exec }
    }

    /// Execute the plan and return all solution mappings.
    pub fn run(&self, plan: &PhysicalPlan) -> Result<std::vec::IntoIter<Bindings>> {
        let v = self.eval(plan)?;
        Ok(v.into_iter())
    }

    fn eval(&self, plan: &PhysicalPlan) -> Result<Vec<Bindings>> {
        match plan {
            PhysicalPlan::BgpScan { patterns } => {
                Ok(self.exec.scan_bgp(patterns)?.collect())
            }
            PhysicalPlan::Join { left, right } => {
                let l = self.eval(left)?;
                let r = self.eval(right)?;
                let mut out = Vec::new();
                for a in &l {
                    for b in &r {
                        if let Some(m) = a.extend_compat(b) {
                            out.push(m);
                        }
                    }
                }
                Ok(out)
            }
            PhysicalPlan::LeftJoin { left, right, expr } => {
                let l = self.eval(left)?;
                let r = self.eval(right)?;
                let mut out = Vec::new();
                for a in &l {
                    let mut matched = false;
                    for b in &r {
                        if let Some(m) = a.extend_compat(b) {
                            let keep = match expr {
                                Some(e) => eval_expr(e, &m)?,
                                None => true,
                            };
                            if keep {
                                matched = true;
                                out.push(m);
                            }
                        }
                    }
                    if !matched {
                        out.push(a.clone());
                    }
                }
                Ok(out)
            }
            PhysicalPlan::Filter { expr, inner } => {
                let v = self.eval(inner)?;
                v.into_iter()
                    .map(|b| eval_expr(expr, &b).map(|keep| (b, keep)))
                    .collect::<Result<Vec<_>>>()
                    .map(|pairs| pairs.into_iter().filter(|(_, k)| *k).map(|(b, _)| b).collect())
            }
            PhysicalPlan::Union { left, right } => {
                let mut a = self.eval(left)?;
                let b = self.eval(right)?;
                a.extend(b);
                Ok(a)
            }
            PhysicalPlan::Project { vars, inner } => {
                let v = self.eval(inner)?;
                Ok(v.into_iter().map(|b| project(&b, vars)).collect())
            }
            PhysicalPlan::Distinct { inner } => {
                let v = self.eval(inner)?;
                let mut seen: Vec<Bindings> = Vec::new();
                for b in v {
                    if !seen.contains(&b) {
                        seen.push(b);
                    }
                }
                Ok(seen)
            }
            PhysicalPlan::Slice { inner, start, length } => {
                let v = self.eval(inner)?;
                let s = *start;
                let take = length.unwrap_or(v.len().saturating_sub(s));
                Ok(v.into_iter().skip(s).take(take).collect())
            }
            PhysicalPlan::OrderBy { inner, keys } => {
                let mut v = self.eval(inner)?;
                v.sort_by(|a, b| compare_by_keys(a, b, keys));
                Ok(v)
            }
            PhysicalPlan::Extend { inner, var, expr } => {
                let v = self.eval(inner)?;
                let mut out = Vec::with_capacity(v.len());
                for mut b in v {
                    if let Some(t) = eval_expr_to_term(expr, &b)? {
                        b.set(var.name().to_owned(), t);
                    }
                    out.push(b);
                }
                Ok(out)
            }
            PhysicalPlan::Values { vars, rows } => {
                let mut out = Vec::with_capacity(rows.len());
                for row in rows {
                    let mut b = Bindings::new();
                    for (var, cell) in vars.iter().zip(row.iter()) {
                        if let Some(term) = cell {
                            b.set(var.name().to_owned(), term.clone());
                        }
                    }
                    out.push(b);
                }
                Ok(out)
            }
        }
    }
}

fn project(b: &Bindings, vars: &[Var]) -> Bindings {
    if vars.is_empty() {
        // SELECT * with no projected vars (e.g. ASK): preserve.
        return b.clone();
    }
    let mut out = Bindings::new();
    for v in vars {
        if let Some(t) = b.get(v.name()) {
            out.set(v.name().to_owned(), t.clone());
        }
    }
    out
}

fn compare_by_keys(
    a: &Bindings,
    b: &Bindings,
    keys: &[(Expr, OrderDir)],
) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    for (e, dir) in keys {
        let ta = eval_expr_to_term(e, a).ok().flatten();
        let tb = eval_expr_to_term(e, b).ok().flatten();
        let ord = match (ta, tb) {
            (Some(x), Some(y)) => lex(&x).cmp(&lex(&y)),
            (None, Some(_)) => Ordering::Less,
            (Some(_), None) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        };
        if ord != Ordering::Equal {
            return match dir {
                OrderDir::Asc => ord,
                OrderDir::Desc => ord.reverse(),
            };
        }
    }
    std::cmp::Ordering::Equal
}

fn lex(t: &Term) -> String {
    match t {
        Term::Iri(s) | Term::Literal(s) | Term::BlankNode(s) => s.clone(),
        Term::Var(v) => v.name().to_owned(),
    }
}

fn eval_expr(e: &Expr, b: &Bindings) -> Result<bool> {
    Ok(match e {
        Expr::Eq(a, c) => eval_expr_to_term(a, b)? == eval_expr_to_term(c, b)?,
        Expr::Ne(a, c) => eval_expr_to_term(a, b)? != eval_expr_to_term(c, b)?,
        Expr::Lt(a, c) => match (eval_expr_to_term(a, b)?, eval_expr_to_term(c, b)?) {
            (Some(x), Some(y)) => lex(&x) < lex(&y),
            _ => false,
        },
        Expr::Gt(a, c) => match (eval_expr_to_term(a, b)?, eval_expr_to_term(c, b)?) {
            (Some(x), Some(y)) => lex(&x) > lex(&y),
            _ => false,
        },
        Expr::And(a, c) => eval_expr(a, b)? && eval_expr(c, b)?,
        Expr::Or(a, c) => eval_expr(a, b)? || eval_expr(c, b)?,
        Expr::Not(a) => !eval_expr(a, b)?,
        Expr::Bound(v) => b.get(v.name()).is_some(),
        Expr::Term(t) => match t {
            // Bare term in boolean context: treat IRI/Literal as
            // truthy; var resolves to its binding.
            Term::Var(v) => b.get(v.name()).is_some(),
            _ => true,
        },
    })
}

fn eval_expr_to_term(e: &Expr, b: &Bindings) -> Result<Option<Term>> {
    Ok(match e {
        Expr::Term(t) => match t {
            Term::Var(v) => b.get(v.name()).cloned(),
            other => Some(other.clone()),
        },
        // Boolean-typed expressions return a typed literal (lexical
        // form "true"/"false"); good enough for Stage 1 BIND tests.
        Expr::Eq(_, _)
        | Expr::Ne(_, _)
        | Expr::Lt(_, _)
        | Expr::Gt(_, _)
        | Expr::And(_, _)
        | Expr::Or(_, _)
        | Expr::Not(_)
        | Expr::Bound(_) => Some(Term::Literal(if eval_expr(e, b)? { "true" } else { "false" }.into())),
    })
}

// Type-witness so we don't drop SparqlError from this module.
#[allow(dead_code)]
fn _witness() -> Result<()> {
    Err(SparqlError::Executor("unreachable".into()))
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p horndb-sparql --test exec_select`
Expected: 4 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/sparql/src/exec/runtime.rs crates/sparql/tests/exec_select.rs
git commit -m "$(cat <<'EOF'
feat(sparql): runtime walks PhysicalPlan against Executor

Iterator-style evaluator covering BgpScan/Join/LeftJoin/Filter/
Union/Project/Distinct/Slice/OrderBy/Extend/Values. Per-node
materialisation is acceptable for Stage 1's W3C subset; streaming
is deferred to Future Work.
EOF
)"
```

---

## Task 10: ASK and CONSTRUCT query forms

**Files:**
- Modify: `crates/sparql/src/exec/runtime.rs`
- Create: `crates/sparql/tests/exec_ask.rs`
- Create: `crates/sparql/tests/exec_construct.rs`

- [ ] **Step 1: Write failing tests**

Create `crates/sparql/tests/exec_ask.rs`:

```rust
use horndb_sparql::algebra::translate::translate_query;
use horndb_sparql::algebra::Term;
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::exec::runtime::Runtime;
use horndb_sparql::exec::Store;
use horndb_sparql::parser::{parse_query, ParsedQuery};
use horndb_sparql::plan::planner;

fn iri(s: &str) -> Term { Term::Iri(s.into()) }

fn make_store() -> MemStore {
    let mut s = MemStore::default();
    s.insert_triple(iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"));
    s
}

#[test]
fn ask_true_when_pattern_matches() {
    let s = make_store();
    let inner = match parse_query("ASK { ?s ?p ?o }").unwrap() {
        ParsedQuery::Ask { inner } => inner,
        _ => unreachable!(),
    };
    let alg = translate_query(&inner).unwrap();
    let plan = planner::plan(&alg).unwrap();
    let any = Runtime::new(&s).run(&plan).unwrap().next().is_some();
    assert!(any);
}

#[test]
fn ask_false_when_pattern_misses() {
    let s = make_store();
    let inner = match parse_query("ASK { ?s <http://ex/missing> ?o }").unwrap() {
        ParsedQuery::Ask { inner } => inner,
        _ => unreachable!(),
    };
    let alg = translate_query(&inner).unwrap();
    let plan = planner::plan(&alg).unwrap();
    let any = Runtime::new(&s).run(&plan).unwrap().next().is_some();
    assert!(!any);
}
```

Create `crates/sparql/tests/exec_construct.rs`:

```rust
use horndb_sparql::algebra::translate::translate_query;
use horndb_sparql::algebra::Term;
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::exec::runtime::Runtime;
use horndb_sparql::exec::Store;
use horndb_sparql::parser::{parse_query, ParsedQuery};
use horndb_sparql::plan::planner;
use horndb_sparql::exec::runtime::construct_triples;

fn iri(s: &str) -> Term { Term::Iri(s.into()) }

#[test]
fn construct_rewrites_pairs() {
    let mut s = MemStore::default();
    s.insert_triple(iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"));
    s.insert_triple(iri("http://ex/c"), iri("http://ex/p"), iri("http://ex/d"));

    let q = parse_query(
        "CONSTRUCT { ?s <http://ex/related> ?o } WHERE { ?s <http://ex/p> ?o }"
    ).unwrap();
    let inner = match q {
        ParsedQuery::Construct { inner } => inner,
        _ => unreachable!(),
    };
    let alg = translate_query(&inner).unwrap();
    let plan = planner::plan(&alg).unwrap();
    let rows: Vec<_> = Runtime::new(&s).run(&plan).unwrap().collect();
    let triples = construct_triples(&inner, &rows).unwrap();
    assert_eq!(triples.len(), 2);
    assert!(triples.iter().any(|(s, p, o)|
        s == "http://ex/a" && p == "http://ex/related" && o == "http://ex/b"));
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p horndb-sparql --test exec_ask --test exec_construct`
Expected: FAIL — `construct_triples` unresolved.

- [ ] **Step 3: Add the CONSTRUCT helper**

Append to `crates/sparql/src/exec/runtime.rs`:

```rust
/// Render a CONSTRUCT template against a stream of solution mappings.
///
/// Returns concrete `(s, p, o)` lexical-form triples. Triples whose
/// template references an unbound variable in the row are skipped
/// (W3C: "ground triple results only").
pub fn construct_triples(
    query: &spargebra::Query,
    rows: &[Bindings],
) -> Result<Vec<(String, String, String)>> {
    use spargebra::term::{NamedNodePattern, TermPattern};
    let template = match query {
        spargebra::Query::Construct { template, .. } => template,
        _ => {
            return Err(SparqlError::Executor(
                "construct_triples called on non-CONSTRUCT query".into(),
            ))
        }
    };

    fn resolve_term(t: &TermPattern, row: &Bindings) -> Option<String> {
        match t {
            TermPattern::NamedNode(n) => Some(n.as_str().to_owned()),
            TermPattern::BlankNode(b) => Some(b.as_str().to_owned()),
            TermPattern::Literal(l) => Some(l.to_string()),
            TermPattern::Variable(v) => match row.get(v.as_str()) {
                Some(Term::Iri(s)) | Some(Term::Literal(s)) | Some(Term::BlankNode(s)) => {
                    Some(s.clone())
                }
                _ => None,
            },
            TermPattern::Triple(_) => None,
        }
    }
    fn resolve_pred(p: &NamedNodePattern, row: &Bindings) -> Option<String> {
        match p {
            NamedNodePattern::NamedNode(n) => Some(n.as_str().to_owned()),
            NamedNodePattern::Variable(v) => match row.get(v.as_str()) {
                Some(Term::Iri(s)) => Some(s.clone()),
                _ => None,
            },
        }
    }

    let mut out = Vec::new();
    for row in rows {
        for tp in template {
            if let (Some(s), Some(p), Some(o)) = (
                resolve_term(&tp.subject, row),
                resolve_pred(&tp.predicate, row),
                resolve_term(&tp.object, row),
            ) {
                out.push((s, p, o));
            }
        }
    }
    Ok(out)
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p horndb-sparql --test exec_ask --test exec_construct`
Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/sparql/src/exec/runtime.rs \
        crates/sparql/tests/exec_ask.rs crates/sparql/tests/exec_construct.rs
git commit -m "$(cat <<'EOF'
feat(sparql): ASK and CONSTRUCT query-form support

ASK reduces to "did any solution mapping survive evaluation". CONSTRUCT
renders the template against each row via construct_triples; ground
template terms with unbound vars in the row are dropped per W3C.
EOF
)"
```

---

## Task 11: UPDATE — INSERT DATA and DELETE DATA

**Files:**
- Modify: `crates/sparql/src/update.rs`
- Create: `crates/sparql/tests/update_insert_delete.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/sparql/tests/update_insert_delete.rs`:

```rust
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::parser::parse_update;
use horndb_sparql::update::apply_update;

#[test]
fn insert_data_adds_triple() {
    let mut s = MemStore::default();
    let u = parse_update(
        "INSERT DATA { <http://ex/a> <http://ex/p> <http://ex/b> }"
    ).unwrap();
    apply_update(&u, &mut s).unwrap();
    assert_eq!(s.len(), 1);
}

#[test]
fn delete_data_removes_triple() {
    let mut s = MemStore::default();
    apply_update(
        &parse_update("INSERT DATA { <http://ex/a> <http://ex/p> <http://ex/b> }").unwrap(),
        &mut s,
    ).unwrap();
    assert_eq!(s.len(), 1);
    apply_update(
        &parse_update("DELETE DATA { <http://ex/a> <http://ex/p> <http://ex/b> }").unwrap(),
        &mut s,
    ).unwrap();
    assert_eq!(s.len(), 0);
}

#[test]
fn unsupported_update_form_errors() {
    let mut s = MemStore::default();
    let u = parse_update("CLEAR DEFAULT").unwrap();
    let err = apply_update(&u, &mut s).unwrap_err();
    assert!(format!("{err}").to_lowercase().contains("unsupported"));
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p horndb-sparql --test update_insert_delete`
Expected: FAIL — `apply_update` unresolved.

- [ ] **Step 3: Implement update application**

Overwrite `crates/sparql/src/update.rs`:

```rust
//! SPARQL Update — Stage 1 supports only `INSERT DATA` and
//! `DELETE DATA` literal forms.
//!
//! `LOAD`, `CLEAR`, `DROP`, and template `INSERT { … } WHERE { … }` /
//! `DELETE { … } WHERE { … }` are explicitly deferred (see SPEC-07
//! Future Work). The parser still accepts them; this module
//! rejects them at apply time.

use crate::algebra::Term;
use crate::error::{Result, SparqlError};
use crate::exec::Store;
use crate::parser::ParsedUpdate;
use spargebra::term::{GroundTerm, GroundTriplePattern, GroundTermPattern};

/// Apply an update to a [`Store`]. Returns `Ok(())` on success.
pub fn apply_update<S: Store>(u: &ParsedUpdate, store: &mut S) -> Result<()> {
    use spargebra::GraphUpdateOperation;
    let ops = match u {
        ParsedUpdate::InsertData { inner } | ParsedUpdate::DeleteData { inner } => {
            &inner.operations
        }
        ParsedUpdate::UnsupportedForm { .. } => {
            return Err(SparqlError::UnsupportedAlgebra(
                "update form not supported in Stage 1".into(),
            ));
        }
    };
    for op in ops {
        match op {
            GraphUpdateOperation::InsertData { data } => {
                for q in data {
                    let (s, p, o) = ground_triple(&q.subject, &q.predicate, &q.object)?;
                    store.insert_triple(s, p, o);
                }
            }
            GraphUpdateOperation::DeleteData { data } => {
                for q in data {
                    let (s, p, o) = ground_triple(&q.subject, &q.predicate, &q.object)?;
                    store.delete_triple(&s, &p, &o);
                }
            }
            other => {
                return Err(SparqlError::UnsupportedAlgebra(format!(
                    "update operation: {other:?}"
                )));
            }
        }
    }
    Ok(())
}

fn ground_triple(
    s: &GroundTermPattern,
    p: &spargebra::term::NamedNodePattern,
    o: &GroundTermPattern,
) -> Result<(Term, Term, Term)> {
    Ok((
        ground_term(s)?,
        match p {
            spargebra::term::NamedNodePattern::NamedNode(n) => Term::Iri(n.as_str().to_owned()),
            spargebra::term::NamedNodePattern::Variable(_) => {
                return Err(SparqlError::UnsupportedAlgebra(
                    "variable predicate in INSERT/DELETE DATA".into(),
                ))
            }
        },
        ground_term(o)?,
    ))
}

fn ground_term(t: &GroundTermPattern) -> Result<Term> {
    Ok(match t {
        GroundTermPattern::NamedNode(n) => Term::Iri(n.as_str().to_owned()),
        GroundTermPattern::Literal(l) => Term::Literal(l.to_string()),
        GroundTermPattern::Triple(_) => {
            return Err(SparqlError::UnsupportedAlgebra("RDF-star triple term".into()))
        }
        // Per the SPARQL grammar, `*Data` blocks may not contain
        // variables; if spargebra exposes a Variable arm we treat it
        // as a translator/upstream bug rather than a runtime case.
        GroundTermPattern::Variable(_) => {
            return Err(SparqlError::UnsupportedAlgebra(
                "variable in INSERT/DELETE DATA (parser bug?)".into(),
            ));
        }
    })
}

// Witness that we route through `GroundTerm` somewhere — the
// upstream re-exports vary; importing GroundTerm keeps the type
// path explicit for future readers.
#[allow(dead_code)]
fn _gt_witness(g: &GroundTerm) -> Result<()> {
    match g {
        GroundTerm::NamedNode(_) | GroundTerm::Literal(_) => Ok(()),
        GroundTerm::Triple(_) => Err(SparqlError::UnsupportedAlgebra("rdf-star".into())),
    }
}

// `GroundTriplePattern` re-export witness for the same reason.
#[allow(dead_code)]
fn _gtp_witness() -> Option<GroundTriplePattern> {
    None
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p horndb-sparql --test update_insert_delete`
Expected: 3 passed. If `spargebra::GraphUpdateOperation::InsertData::data` is actually a `Vec<Quad>` (not `Triple`) or uses `subject`/`predicate`/`object` field names differently in your patch version, follow the compiler errors and adjust — the *shape* of the test stays the same.

- [ ] **Step 5: Commit**

```bash
git add crates/sparql/src/update.rs crates/sparql/tests/update_insert_delete.rs
git commit -m "$(cat <<'EOF'
feat(sparql): INSERT DATA / DELETE DATA support

Applies literal-triple updates via the Store trait. CLEAR/DROP/LOAD
and template INSERT/DELETE WHERE return UnsupportedAlgebra so the
unsupported surface fails loud rather than silently no-oping.
EOF
)"
```

---

## Task 12: Entailment regime markers

**Files:**
- Modify: `crates/sparql/src/regime/mod.rs`
- Create: `crates/sparql/src/regime/simple.rs`
- Create: `crates/sparql/src/regime/owl_rl.rs`
- Create: `crates/sparql/tests/regime.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/sparql/tests/regime.rs`:

```rust
use horndb_sparql::regime::{simple::SimpleRegime, owl_rl::MaterializedOwlRlRegime, EntailmentRegime};

#[test]
fn regimes_are_distinguishable_by_name() {
    assert_eq!(SimpleRegime.name(), "simple");
    assert_eq!(
        MaterializedOwlRlRegime.name(),
        "http://www.w3.org/ns/entailment/OWL-RL"
    );
}

#[test]
fn simple_is_default() {
    let d: Box<dyn EntailmentRegime> = Default::default();
    assert_eq!(d.name(), "simple");
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p horndb-sparql --test regime`
Expected: FAIL — unresolved imports.

- [ ] **Step 3: Implement the regime trait and two impls**

Overwrite `crates/sparql/src/regime/mod.rs`:

```rust
//! SPARQL 1.1 entailment regimes.
//!
//! In Stage 1 the regime is essentially a *marker* — the runtime does
//! not rewrite queries based on it. Both implementations execute the
//! same algebra against the same store; the distinction shows up in:
//!
//!  * the answer-set metadata (so clients know what regime ran),
//!  * the contract about what the underlying store must already
//!    contain (`MaterializedOwlRl` assumes SPEC-04/05 has already
//!    written the OWL 2 RL closure into the store; `Simple` makes no
//!    such assumption).
//!
//! Stage 2 will hang query-rewriting logic (e.g. backward-chained
//! mode) off the same trait.

pub mod owl_rl;
pub mod simple;

/// Top-level regime selector. Implementations are tiny — they only
/// hold the static W3C identifier — but the trait surface is the
/// integration point SPEC-04 will plug rule logic into in Stage 2.
pub trait EntailmentRegime: Send + Sync {
    /// Stable identifier: either `"simple"` or the W3C entailment
    /// regime IRI for OWL 2 RL.
    fn name(&self) -> &'static str;
}

impl Default for Box<dyn EntailmentRegime> {
    fn default() -> Self {
        Box::new(simple::SimpleRegime)
    }
}
```

Create `crates/sparql/src/regime/simple.rs`:

```rust
//! The default SPARQL 1.1 "simple" entailment regime — no inference.
//! Used for the W3C SPARQL 1.1 Query Test Suite.

use super::EntailmentRegime;

#[derive(Debug, Default, Clone, Copy)]
pub struct SimpleRegime;

impl EntailmentRegime for SimpleRegime {
    fn name(&self) -> &'static str {
        "simple"
    }
}
```

Create `crates/sparql/src/regime/owl_rl.rs`:

```rust
//! The materialized OWL 2 RL/RDF entailment regime.
//!
//! Stage 1 contract: the caller has already loaded the OWL 2 RL
//! closure into the underlying store (via SPEC-04/05). This regime
//! is therefore a marker — queries execute as plain BGPs against
//! the materialised store.
//!
//! In Stage 2, when SPEC-04 ships, this regime will also be the
//! mount point for the optional backward-chained mode (per
//! SPEC-07 F4, second bullet).

use super::EntailmentRegime;

#[derive(Debug, Default, Clone, Copy)]
pub struct MaterializedOwlRlRegime;

impl EntailmentRegime for MaterializedOwlRlRegime {
    fn name(&self) -> &'static str {
        // W3C SPARQL 1.1 Entailment Regimes registry IRI:
        "http://www.w3.org/ns/entailment/OWL-RL"
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p horndb-sparql --test regime`
Expected: 2 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/sparql/src/regime/ crates/sparql/tests/regime.rs
git commit -m "$(cat <<'EOF'
feat(sparql): EntailmentRegime trait with Simple and Materialized OWL 2 RL

Simple is the Stage-1 default (no inference). MaterializedOwlRl is a
marker that assumes the store already carries the OWL 2 RL closure
materialised by SPEC-04/05; backward-chained mode is hung off the
same trait in Stage 2.
EOF
)"
```

---

## Task 13: SPARQL JSON Results serializer

**Files:**
- Modify: `crates/sparql/src/results/mod.rs`
- Create: `crates/sparql/src/results/json.rs`
- Create: `crates/sparql/src/results/csv.rs`
- Create: `crates/sparql/src/results/tsv.rs`
- Create: `crates/sparql/tests/results_json.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/sparql/tests/results_json.rs`:

```rust
use horndb_sparql::algebra::Term;
use horndb_sparql::exec::Bindings;
use horndb_sparql::results::json::write_select_json;

#[test]
fn select_json_shape() {
    let mut b = Bindings::new();
    b.set("x", Term::Iri("http://ex/a".into()));
    b.set("y", Term::Literal("\"42\"^^<http://www.w3.org/2001/XMLSchema#integer>".into()));
    let rows = vec![b];
    let vars = vec!["x".to_string(), "y".to_string()];
    let json = write_select_json(&vars, &rows);
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["head"]["vars"], serde_json::json!(["x", "y"]));
    let binding = &parsed["results"]["bindings"][0];
    assert_eq!(binding["x"]["type"], "uri");
    assert_eq!(binding["x"]["value"], "http://ex/a");
    assert!(binding["y"]["type"] == "literal" || binding["y"]["type"] == "typed-literal");
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p horndb-sparql --test results_json`
Expected: FAIL — `write_select_json` unresolved.

- [ ] **Step 3: Implement the serializers**

Overwrite `crates/sparql/src/results/mod.rs`:

```rust
//! Result serialisation. Stage 1 supports SPARQL JSON, CSV, TSV;
//! XML is deferred.

pub mod csv;
pub mod json;
pub mod tsv;

/// Wire-format selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultFormat {
    Json,
    Csv,
    Tsv,
}

impl ResultFormat {
    /// Map a `Accept` / format query-parameter value to a format.
    /// Defaults to JSON.
    pub fn from_accept(accept: &str) -> Self {
        let a = accept.to_ascii_lowercase();
        if a.contains("text/csv") {
            Self::Csv
        } else if a.contains("text/tab-separated-values") || a.contains("tsv") {
            Self::Tsv
        } else {
            Self::Json
        }
    }
    pub fn content_type(self) -> &'static str {
        match self {
            Self::Json => "application/sparql-results+json",
            Self::Csv => "text/csv",
            Self::Tsv => "text/tab-separated-values",
        }
    }
}
```

Create `crates/sparql/src/results/json.rs`:

```rust
//! SPARQL 1.1 Query Results JSON Format.
//! https://www.w3.org/TR/sparql11-results-json/

use crate::algebra::Term;
use crate::exec::Bindings;
use serde_json::{json, Map, Value};

pub fn write_select_json(vars: &[String], rows: &[Bindings]) -> String {
    let bindings: Vec<Value> = rows
        .iter()
        .map(|row| {
            let mut obj = Map::new();
            for v in vars {
                if let Some(t) = row.get(v) {
                    obj.insert(v.clone(), term_to_json(t));
                }
            }
            Value::Object(obj)
        })
        .collect();

    json!({
        "head": { "vars": vars },
        "results": { "bindings": bindings }
    })
    .to_string()
}

pub fn write_ask_json(answer: bool) -> String {
    json!({ "head": {}, "boolean": answer }).to_string()
}

fn term_to_json(t: &Term) -> Value {
    match t {
        Term::Iri(s) => json!({ "type": "uri", "value": s }),
        Term::BlankNode(s) => json!({ "type": "bnode", "value": s.trim_start_matches("_:") }),
        Term::Literal(raw) => parse_literal_to_json(raw),
        Term::Var(_) => json!({ "type": "literal", "value": "<unbound>" }),
    }
}

/// Parse an N-Triples-form literal into a SPARQL-JSON binding.
/// Recognises `"foo"`, `"foo"@lang`, `"foo"^^<datatype>`.
fn parse_literal_to_json(raw: &str) -> Value {
    // Best-effort lexical parsing; sufficient for the W3C subset.
    let raw = raw.trim();
    if !raw.starts_with('"') {
        return json!({ "type": "literal", "value": raw });
    }
    let mut end_quote = None;
    let bytes = raw.as_bytes();
    let mut i = 1;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            i += 2;
            continue;
        }
        if bytes[i] == b'"' {
            end_quote = Some(i);
            break;
        }
        i += 1;
    }
    let Some(eq) = end_quote else {
        return json!({ "type": "literal", "value": raw });
    };
    let value = &raw[1..eq];
    let tail = &raw[eq + 1..];

    if let Some(rest) = tail.strip_prefix("@") {
        return json!({ "type": "literal", "value": value, "xml:lang": rest });
    }
    if let Some(rest) = tail.strip_prefix("^^") {
        let dt = rest.trim_start_matches('<').trim_end_matches('>');
        return json!({
            "type": "literal",
            "value": value,
            "datatype": dt
        });
    }
    json!({ "type": "literal", "value": value })
}
```

Create `crates/sparql/src/results/csv.rs`:

```rust
//! SPARQL CSV results. https://www.w3.org/TR/sparql11-results-csv-tsv/

use crate::algebra::Term;
use crate::exec::Bindings;

pub fn write_select_csv(vars: &[String], rows: &[Bindings]) -> String {
    let mut out = String::new();
    out.push_str(&vars.join(","));
    out.push_str("\r\n");
    for row in rows {
        let cells: Vec<String> = vars
            .iter()
            .map(|v| match row.get(v) {
                None => String::new(),
                Some(t) => csv_escape(&term_to_lex(t)),
            })
            .collect();
        out.push_str(&cells.join(","));
        out.push_str("\r\n");
    }
    out
}

fn term_to_lex(t: &Term) -> String {
    match t {
        Term::Iri(s) | Term::Literal(s) | Term::BlankNode(s) => s.clone(),
        Term::Var(v) => v.name().to_owned(),
    }
}

fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        let escaped = s.replace('"', "\"\"");
        format!("\"{escaped}\"")
    } else {
        s.to_owned()
    }
}
```

Create `crates/sparql/src/results/tsv.rs`:

```rust
//! SPARQL TSV results. https://www.w3.org/TR/sparql11-results-csv-tsv/

use crate::algebra::Term;
use crate::exec::Bindings;

pub fn write_select_tsv(vars: &[String], rows: &[Bindings]) -> String {
    let mut out = String::new();
    let header: Vec<String> = vars.iter().map(|v| format!("?{v}")).collect();
    out.push_str(&header.join("\t"));
    out.push('\n');
    for row in rows {
        let cells: Vec<String> = vars
            .iter()
            .map(|v| match row.get(v) {
                None => String::new(),
                Some(Term::Iri(s)) => format!("<{s}>"),
                Some(Term::BlankNode(s)) => {
                    if s.starts_with("_:") { s.clone() } else { format!("_:{s}") }
                }
                Some(Term::Literal(s)) => s.clone(),
                Some(Term::Var(v)) => format!("?{}", v.name()),
            })
            .collect();
        out.push_str(&cells.join("\t"));
        out.push('\n');
    }
    out
}
```

- [ ] **Step 4: Run the test**

Run: `cargo test -p horndb-sparql --test results_json`
Expected: 1 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/sparql/src/results/ crates/sparql/tests/results_json.rs
git commit -m "$(cat <<'EOF'
feat(sparql): SPARQL JSON / CSV / TSV result serialisers

Hand-written for Stage 1 (sparesults adoption deferred). Covers
SELECT and ASK; CONSTRUCT/DESCRIBE serialisation will land with the
Turtle writer in Stage 2.
EOF
)"
```

---

## Task 14: Top-level `execute_query` convenience API

**Files:**
- Create: `crates/sparql/src/api.rs`
- Modify: `crates/sparql/src/lib.rs`
- Create: `crates/sparql/tests/api_end_to_end.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/sparql/tests/api_end_to_end.rs`:

```rust
use horndb_sparql::algebra::Term;
use horndb_sparql::api::{execute_query, QueryAnswer};
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::exec::Store;

fn iri(s: &str) -> Term { Term::Iri(s.into()) }

#[test]
fn end_to_end_select() {
    let mut s = MemStore::default();
    s.insert_triple(iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"));
    let ans = execute_query("SELECT ?o WHERE { ?s ?p ?o }", &s).unwrap();
    match ans {
        QueryAnswer::Solutions { vars, rows } => {
            assert_eq!(vars, vec!["o".to_string()]);
            assert_eq!(rows.len(), 1);
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn end_to_end_ask_true() {
    let mut s = MemStore::default();
    s.insert_triple(iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"));
    let ans = execute_query("ASK { ?s ?p ?o }", &s).unwrap();
    assert!(matches!(ans, QueryAnswer::Boolean(true)));
}

#[test]
fn end_to_end_construct() {
    let mut s = MemStore::default();
    s.insert_triple(iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"));
    let ans = execute_query(
        "CONSTRUCT { ?s <http://ex/r> ?o } WHERE { ?s ?p ?o }",
        &s,
    )
    .unwrap();
    match ans {
        QueryAnswer::Triples(t) => {
            assert_eq!(t.len(), 1);
            assert_eq!(t[0].1, "http://ex/r");
        }
        other => panic!("unexpected: {other:?}"),
    }
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p horndb-sparql --test api_end_to_end`
Expected: FAIL.

- [ ] **Step 3: Implement the API layer**

Create `crates/sparql/src/api.rs`:

```rust
//! High-level convenience: parse → translate → plan → run → return.
//!
//! This is what the HTTP `/query` handler and integration tests use.
//! Callers that need finer control should use the individual modules.

use crate::algebra::translate::translate_query;
use crate::error::{Result, SparqlError};
use crate::exec::runtime::{construct_triples, Runtime};
use crate::exec::{Bindings, Executor, Store};
use crate::parser::{parse_query, parse_update, ParsedQuery};
use crate::plan::planner;
use crate::update::apply_update;

/// What `execute_query` returns. Variant chosen by query form.
#[derive(Debug, Clone)]
pub enum QueryAnswer {
    /// SELECT result: list of variable names + solution rows.
    Solutions { vars: Vec<String>, rows: Vec<Bindings> },
    /// ASK result.
    Boolean(bool),
    /// CONSTRUCT result: ground triples in (s, p, o) lexical form.
    Triples(Vec<(String, String, String)>),
}

pub fn execute_query<E: Executor>(query: &str, exec: &E) -> Result<QueryAnswer> {
    let parsed = parse_query(query)?;
    match parsed {
        ParsedQuery::Select { inner } => {
            let alg = translate_query(&inner)?;
            let vars = projected_vars(&alg);
            let plan = planner::plan(&alg)?;
            let rows: Vec<Bindings> = Runtime::new(exec).run(&plan)?.collect();
            Ok(QueryAnswer::Solutions { vars, rows })
        }
        ParsedQuery::Ask { inner } => {
            let alg = translate_query(&inner)?;
            let plan = planner::plan(&alg)?;
            let any = Runtime::new(exec).run(&plan)?.next().is_some();
            Ok(QueryAnswer::Boolean(any))
        }
        ParsedQuery::Construct { inner } => {
            let alg = translate_query(&inner)?;
            let plan = planner::plan(&alg)?;
            let rows: Vec<Bindings> = Runtime::new(exec).run(&plan)?.collect();
            let triples = construct_triples(&inner, &rows)?;
            Ok(QueryAnswer::Triples(triples))
        }
        ParsedQuery::Describe { .. } => Err(SparqlError::UnsupportedAlgebra("DESCRIBE".into())),
    }
}

pub fn execute_update<S: Store>(update: &str, store: &mut S) -> Result<()> {
    let parsed = parse_update(update)?;
    apply_update(&parsed, store)
}

fn projected_vars(alg: &crate::algebra::Algebra) -> Vec<String> {
    use crate::algebra::Algebra;
    match alg {
        Algebra::Project { vars, .. } => vars.iter().map(|v| v.name().to_owned()).collect(),
        _ => Vec::new(),
    }
}
```

- [ ] **Step 4: Register the module**

Add to `crates/sparql/src/lib.rs` (immediately after `pub mod algebra;`):

```rust
pub mod api;
```

- [ ] **Step 5: Run the test**

Run: `cargo test -p horndb-sparql --test api_end_to_end`
Expected: 3 passed.

- [ ] **Step 6: Commit**

```bash
git add crates/sparql/src/api.rs crates/sparql/src/lib.rs \
        crates/sparql/tests/api_end_to_end.rs
git commit -m "$(cat <<'EOF'
feat(sparql): execute_query / execute_update facade

One-call pipeline (parse → translate → plan → run) returning a
QueryAnswer tagged by query form. Used by the HTTP handlers and
makes the integration test surface trivial.
EOF
)"
```

---

## Task 15: HTTP server — `/query` and `/update`

**Files:**
- Modify: `crates/sparql/src/server/mod.rs`
- Create: `crates/sparql/src/server/query.rs`
- Create: `crates/sparql/src/server/update.rs`
- Create: `crates/sparql/tests/server_http.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/sparql/tests/server_http.rs`:

```rust
#![cfg(feature = "server")]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use horndb_sparql::algebra::Term;
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::exec::Store;
use horndb_sparql::server::build_router;
use horndb_sparql::server::AppState;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

fn iri(s: &str) -> Term { Term::Iri(s.into()) }

fn router_with_data() -> axum::Router {
    let mut s = MemStore::default();
    s.insert_triple(iri("http://ex/a"), iri("http://ex/p"), iri("http://ex/b"));
    let state = AppState { store: Arc::new(Mutex::new(s)) };
    build_router(state)
}

#[tokio::test]
async fn get_query_returns_json() {
    let app = router_with_data();
    let req = Request::builder()
        .uri("/query?query=SELECT%20%3Fo%20WHERE%20%7B%20%3Fs%20%3Fp%20%3Fo%20%7D")
        .header("accept", "application/sparql-results+json")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["results"]["bindings"][0]["o"]["value"], "http://ex/b");
}

#[tokio::test]
async fn post_update_then_query() {
    let app = router_with_data();
    let req = Request::builder()
        .method("POST")
        .uri("/update")
        .header("content-type", "application/sparql-update")
        .body(Body::from(
            "INSERT DATA { <http://ex/x> <http://ex/p> <http://ex/y> }".to_string(),
        ))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn parse_error_returns_400() {
    let app = router_with_data();
    let req = Request::builder()
        .uri("/query?query=NOT_VALID")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p horndb-sparql --test server_http --features server`
Expected: FAIL — unresolved.

- [ ] **Step 3: Implement the server module**

Overwrite `crates/sparql/src/server/mod.rs`:

```rust
//! Embedded HTTP server exposing SPARQL 1.1 Protocol endpoints.
//!
//! Only the `/query` and `/update` endpoints. The Graph Store
//! Protocol is explicitly out of Stage 1 scope (see SPEC-07 Future
//! Work).

pub mod query;
pub mod update;

use crate::exec::mem::MemStore;
use axum::routing::{get, post};
use axum::Router;
use std::sync::{Arc, Mutex};

/// Shared state: the store is wrapped in a `Mutex` because SPARQL
/// Update is mutating and `MemStore` is not internally synchronised.
/// SPEC-02 will replace this with MVCC.
#[derive(Clone)]
pub struct AppState {
    pub store: Arc<Mutex<MemStore>>,
}

/// Build the axum router. Callers attach it to a `tokio::net::TcpListener`.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/query", get(query::handle_query_get).post(query::handle_query_post))
        .route("/update", post(update::handle_update))
        .with_state(state)
}
```

Create `crates/sparql/src/server/query.rs`:

```rust
//! `/query` HTTP handler. Per SPARQL 1.1 Protocol:
//!   * GET with `query` in the URL query string,
//!   * POST `application/sparql-query` raw,
//!   * POST `application/x-www-form-urlencoded` with `query=`.

use super::AppState;
use crate::api::{execute_query, QueryAnswer};
use crate::results::{
    csv::write_select_csv, json::write_ask_json, json::write_select_json,
    tsv::write_select_tsv, ResultFormat,
};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use serde::Deserialize;

#[derive(Deserialize)]
pub struct QueryParams {
    pub query: Option<String>,
}

pub async fn handle_query_get(
    State(state): State<AppState>,
    Query(p): Query<QueryParams>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let Some(q) = p.query else {
        return (StatusCode::BAD_REQUEST, "missing `query` parameter".to_string()).into_response();
    };
    run(state, &q, &headers).await
}

pub async fn handle_query_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> impl IntoResponse {
    // Per the protocol, `application/x-www-form-urlencoded` carries
    // a `query=` field; `application/sparql-query` is raw. We sniff.
    let ctype = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let query = if ctype.contains("application/x-www-form-urlencoded") {
        match url_form_query(&body) {
            Some(q) => q,
            None => {
                return (StatusCode::BAD_REQUEST, "form missing `query`".to_string())
                    .into_response();
            }
        }
    } else {
        body
    };
    run(state, &query, &headers).await
}

fn url_form_query(body: &str) -> Option<String> {
    for pair in body.split('&') {
        let mut it = pair.splitn(2, '=');
        if let (Some(k), Some(v)) = (it.next(), it.next()) {
            if k == "query" {
                return Some(percent_decode(v));
            }
        }
    }
    None
}

fn percent_decode(s: &str) -> String {
    // Minimal decoder — sufficient for tests. `urlencoding` crate
    // would be the prod choice; avoid the dep in Stage 1.
    let bytes = s.replace('+', " ");
    let bytes = bytes.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(
                std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""),
                16,
            ) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

async fn run(state: AppState, q: &str, headers: &HeaderMap) -> axum::response::Response {
    let store = state.store.lock().unwrap();
    let ans = match execute_query(q, &*store) {
        Ok(a) => a,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, e.to_string()).into_response();
        }
    };

    let accept = headers
        .get("accept")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let fmt = ResultFormat::from_accept(accept);

    match ans {
        QueryAnswer::Solutions { vars, rows } => {
            let body = match fmt {
                ResultFormat::Json => write_select_json(&vars, &rows),
                ResultFormat::Csv => write_select_csv(&vars, &rows),
                ResultFormat::Tsv => write_select_tsv(&vars, &rows),
            };
            (
                StatusCode::OK,
                [("content-type", fmt.content_type())],
                body,
            )
                .into_response()
        }
        QueryAnswer::Boolean(b) => (
            StatusCode::OK,
            [("content-type", ResultFormat::Json.content_type())],
            write_ask_json(b),
        )
            .into_response(),
        QueryAnswer::Triples(triples) => {
            // Stage 1: serialise CONSTRUCT as N-Triples.
            let mut s = String::new();
            for (sub, p, o) in triples {
                s.push_str(&format!("<{sub}> <{p}> <{o}> .\n"));
            }
            (
                StatusCode::OK,
                [("content-type", "application/n-triples")],
                s,
            )
                .into_response()
        }
    }
}
```

Create `crates/sparql/src/server/update.rs`:

```rust
//! `/update` HTTP handler.

use super::AppState;
use crate::api::execute_update;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;

pub async fn handle_update(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> impl IntoResponse {
    let ctype = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let update = if ctype.contains("application/x-www-form-urlencoded") {
        match super::query::url_form_update(&body) {
            Some(u) => u,
            None => return (StatusCode::BAD_REQUEST, "form missing `update`".to_string()).into_response(),
        }
    } else {
        body
    };

    let mut store = state.store.lock().unwrap();
    match execute_update(&update, &mut *store) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
}
```

- [ ] **Step 4: Add the `url_form_update` helper that the update handler references**

Append to `crates/sparql/src/server/query.rs`:

```rust
/// Pulled out for re-use by `/update`'s form-encoded body path.
pub(crate) fn url_form_update(body: &str) -> Option<String> {
    for pair in body.split('&') {
        let mut it = pair.splitn(2, '=');
        if let (Some(k), Some(v)) = (it.next(), it.next()) {
            if k == "update" {
                return Some(percent_decode(v));
            }
        }
    }
    None
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p horndb-sparql --test server_http --features server`
Expected: 3 passed.

- [ ] **Step 6: Commit**

```bash
git add crates/sparql/src/server/ crates/sparql/tests/server_http.rs
git commit -m "$(cat <<'EOF'
feat(sparql): axum-based /query and /update endpoints

Implements the SPARQL 1.1 Protocol query and update HTTP surface.
GET /query?query=..., POST /query (raw or form-encoded), POST /update.
Content negotiation: SPARQL-JSON (default), CSV, TSV; CONSTRUCT
serialised as N-Triples. Graph Store Protocol is deferred.
EOF
)"
```

---

## Task 16: Bring in the W3C SPARQL 1.1 Query Test Suite (selected subset)

**Files:**
- Create: `crates/harness/tests/fixtures/sparql11/README.md`
- Create: `crates/harness/tests/fixtures/sparql11/selected_subset/` (multiple files)
- Create: `crates/sparql/tests/w3c_suite.rs`
- Modify: `crates/harness/selected.toml` (creating if absent)

> The full W3C test suite (https://w3c.github.io/rdf-tests/sparql/sparql11/) lives in a Git submodule we will *not* vendor here. Stage 1 ships **5 hand-picked tests** committed verbatim, enough to demonstrate the loop end-to-end. SPEC-01's harness will subsume this in a later phase.

- [ ] **Step 1: Vendor 5 test fixtures**

Create `crates/harness/tests/fixtures/sparql11/README.md`:

```markdown
# SPARQL 1.1 Query Test Suite — selected Stage-1 subset

Five hand-picked tests from the W3C SPARQL 1.1 Query Test Suite
(https://w3c.github.io/rdf-tests/sparql/sparql11/), one per supported
algebra construct. The full suite belongs to SPEC-01 (the harness);
this directory is intentionally small.

| Test ID                         | Construct exercised        |
|---------------------------------|----------------------------|
| basic-001                       | single-pattern SELECT      |
| basic-002                       | DISTINCT                   |
| basic-003                       | FILTER (?x = <iri>)        |
| basic-004                       | OPTIONAL / LeftJoin        |
| basic-005                       | ASK true                   |

Each test directory contains:
* `query.rq`        — the SPARQL query
* `data.nt`         — the input dataset (N-Triples)
* `expected.srj`    — the expected SPARQL JSON Results
* `form`            — single line: `select` or `ask`

The harness (Task 17) iterates this directory, runs each query, and
diffs the JSON answer against `expected.srj` (parsed, set-compared —
binding order is not significant).
```

Create the five fixture directories. For each, fill the files as follows.

`crates/harness/tests/fixtures/sparql11/selected_subset/basic-001/query.rq`:
```sparql
SELECT ?s WHERE { ?s <http://example.org/p> ?o }
```
`crates/harness/tests/fixtures/sparql11/selected_subset/basic-001/data.nt`:
```text
<http://example.org/a> <http://example.org/p> <http://example.org/b> .
<http://example.org/c> <http://example.org/p> <http://example.org/d> .
<http://example.org/e> <http://example.org/q> <http://example.org/f> .
```
`crates/harness/tests/fixtures/sparql11/selected_subset/basic-001/expected.srj`:
```json
{
  "head": { "vars": ["s"] },
  "results": {
    "bindings": [
      { "s": { "type": "uri", "value": "http://example.org/a" } },
      { "s": { "type": "uri", "value": "http://example.org/c" } }
    ]
  }
}
```
`crates/harness/tests/fixtures/sparql11/selected_subset/basic-001/form`:
```text
select
```

`basic-002/query.rq`:
```sparql
SELECT DISTINCT ?p WHERE { ?s ?p ?o }
```
`basic-002/data.nt`:
```text
<http://example.org/a> <http://example.org/p> <http://example.org/b> .
<http://example.org/c> <http://example.org/p> <http://example.org/d> .
<http://example.org/e> <http://example.org/q> <http://example.org/f> .
```
`basic-002/expected.srj`:
```json
{
  "head": { "vars": ["p"] },
  "results": {
    "bindings": [
      { "p": { "type": "uri", "value": "http://example.org/p" } },
      { "p": { "type": "uri", "value": "http://example.org/q" } }
    ]
  }
}
```
`basic-002/form`: `select`

`basic-003/query.rq`:
```sparql
SELECT ?o WHERE {
  ?s <http://example.org/p> ?o .
  FILTER(?s = <http://example.org/a>)
}
```
`basic-003/data.nt`:
```text
<http://example.org/a> <http://example.org/p> <http://example.org/b> .
<http://example.org/c> <http://example.org/p> <http://example.org/d> .
```
`basic-003/expected.srj`:
```json
{
  "head": { "vars": ["o"] },
  "results": {
    "bindings": [
      { "o": { "type": "uri", "value": "http://example.org/b" } }
    ]
  }
}
```
`basic-003/form`: `select`

`basic-004/query.rq`:
```sparql
SELECT ?s ?o WHERE {
  ?s <http://example.org/p> "name" .
  OPTIONAL { ?s <http://example.org/q> ?o }
}
```
`basic-004/data.nt`:
```text
<http://example.org/a> <http://example.org/p> "name" .
<http://example.org/b> <http://example.org/p> "name" .
<http://example.org/a> <http://example.org/q> <http://example.org/x> .
```
`basic-004/expected.srj`:
```json
{
  "head": { "vars": ["s", "o"] },
  "results": {
    "bindings": [
      { "s": { "type": "uri", "value": "http://example.org/a" },
        "o": { "type": "uri", "value": "http://example.org/x" } },
      { "s": { "type": "uri", "value": "http://example.org/b" } }
    ]
  }
}
```
`basic-004/form`: `select`

`basic-005/query.rq`:
```sparql
ASK { ?s <http://example.org/p> ?o }
```
`basic-005/data.nt`:
```text
<http://example.org/a> <http://example.org/p> <http://example.org/b> .
```
`basic-005/expected.srj`:
```json
{ "head": {}, "boolean": true }
```
`basic-005/form`: `ask`

- [ ] **Step 2: Declare them in `harness/selected.toml`**

Create `crates/harness/selected.toml` (or append if it already exists):

```toml
# Selected conformance subset that gates CI.
# Each entry is the path of a fixture under
# `crates/harness/tests/fixtures/<suite>/`.

[sparql_query]
# SPEC-07 Stage-1 baseline. SPEC-01 will grow this as the engine
# capability widens.
tests = [
  "selected_subset/basic-001",
  "selected_subset/basic-002",
  "selected_subset/basic-003",
  "selected_subset/basic-004",
  "selected_subset/basic-005",
]
```

- [ ] **Step 3: Verify the fixtures are committed**

Run: `git status crates/harness/tests/fixtures crates/harness/selected.toml`
Expected: five fixture directories + the README + the `selected.toml` show as untracked.

- [ ] **Step 4: Commit**

```bash
git add crates/harness/tests/fixtures crates/harness/selected.toml
git commit -m "$(cat <<'EOF'
test(sparql): vendor 5-test Stage-1 W3C SPARQL conformance subset

Hand-picked tests covering single-pattern SELECT, DISTINCT, FILTER,
OPTIONAL, and ASK. Declared in selected.toml so future
SPEC-01 harness work treats them as the existing baseline. Full
W3C suite import is SPEC-01's job.
EOF
)"
```

---

## Task 17: W3C subset test runner inside the sparql crate

**Files:**
- Create: `crates/sparql/tests/w3c_suite.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/sparql/tests/w3c_suite.rs`:

```rust
//! Drives the Stage-1 W3C SPARQL Query subset committed in
//! `crates/harness/tests/fixtures/sparql11/`. Diffs each query's
//! answer against the vendored expected SPARQL-JSON file.

use horndb_sparql::algebra::Term;
use horndb_sparql::api::{execute_query, QueryAnswer};
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::exec::Store;
use horndb_sparql::results::json::{write_ask_json, write_select_json};
use std::path::PathBuf;

fn fixtures_root() -> PathBuf {
    // tests live in crates/sparql/tests/, fixtures in crates/harness/
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop(); // crates/
    p.push("harness/tests/fixtures/sparql11/selected_subset");
    p
}

fn load_ntriples(path: &PathBuf) -> MemStore {
    let mut s = MemStore::default();
    let body = std::fs::read_to_string(path).expect("read data.nt");
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Minimal N-Triples line parser: <s> <p> <o> . OR
        // <s> <p> "lit" .
        let line = line.trim_end_matches('.').trim();
        let (subj, rest) = split_term(line);
        let (pred, rest) = split_term(rest.trim());
        let obj = rest.trim().trim_end_matches('.').trim().to_owned();
        s.insert_triple(parse_term(&subj), parse_term(&pred), parse_term(&obj));
    }
    s
}

fn split_term(input: &str) -> (String, &str) {
    let input = input.trim_start();
    if input.starts_with('<') {
        let end = input.find('>').unwrap();
        (input[..=end].to_owned(), &input[end + 1..])
    } else if input.starts_with('"') {
        // find the closing quote (no escape handling — fixtures are simple).
        let rest = &input[1..];
        let end = rest.find('"').unwrap();
        (input[..=end + 1].to_owned(), &input[end + 2..])
    } else {
        // bnode `_:foo`
        let end = input.find(char::is_whitespace).unwrap();
        (input[..end].to_owned(), &input[end..])
    }
}

fn parse_term(s: &str) -> Term {
    if let Some(inner) = s.strip_prefix('<').and_then(|s| s.strip_suffix('>')) {
        Term::Iri(inner.to_owned())
    } else if s.starts_with('"') {
        Term::Literal(s.to_owned())
    } else if let Some(rest) = s.strip_prefix("_:") {
        Term::BlankNode(rest.to_owned())
    } else {
        Term::Literal(s.to_owned())
    }
}

fn read_form(dir: &PathBuf) -> String {
    std::fs::read_to_string(dir.join("form"))
        .expect("read form")
        .trim()
        .to_owned()
}

fn assert_select_equal(got: &str, expected: &str) {
    let g: serde_json::Value = serde_json::from_str(got).unwrap();
    let e: serde_json::Value = serde_json::from_str(expected).unwrap();
    // vars: compare as set
    let gv: std::collections::BTreeSet<_> =
        g["head"]["vars"].as_array().unwrap().iter().collect();
    let ev: std::collections::BTreeSet<_> =
        e["head"]["vars"].as_array().unwrap().iter().collect();
    assert_eq!(gv, ev, "vars differ");
    // bindings: compare as multiset (sort by serialised form)
    let mut gb: Vec<String> = g["results"]["bindings"]
        .as_array().unwrap().iter()
        .map(|b| serde_json::to_string(b).unwrap()).collect();
    let mut eb: Vec<String> = e["results"]["bindings"]
        .as_array().unwrap().iter()
        .map(|b| serde_json::to_string(b).unwrap()).collect();
    gb.sort();
    eb.sort();
    assert_eq!(gb, eb, "bindings differ");
}

fn run_one(name: &str) {
    let dir = fixtures_root().join(name);
    let store = load_ntriples(&dir.join("data.nt"));
    let q = std::fs::read_to_string(dir.join("query.rq")).expect("read query.rq");
    let expected = std::fs::read_to_string(dir.join("expected.srj")).expect("read expected.srj");
    let form = read_form(&dir);

    let ans = execute_query(&q, &store).unwrap_or_else(|e| panic!("{name}: {e}"));
    match (form.as_str(), ans) {
        ("select", QueryAnswer::Solutions { vars, rows }) => {
            let got = write_select_json(&vars, &rows);
            assert_select_equal(&got, &expected);
        }
        ("ask", QueryAnswer::Boolean(b)) => {
            let got = write_ask_json(b);
            let g: serde_json::Value = serde_json::from_str(&got).unwrap();
            let e: serde_json::Value = serde_json::from_str(&expected).unwrap();
            assert_eq!(g["boolean"], e["boolean"], "{name}: boolean differs");
        }
        (form, ans) => panic!("{name}: unexpected form/answer pair {form:?} / {ans:?}"),
    }
}

macro_rules! w3c_case {
    ($name:ident, $dir:expr) => {
        #[test]
        fn $name() {
            run_one($dir);
        }
    };
}

w3c_case!(basic_001, "basic-001");
w3c_case!(basic_002, "basic-002");
w3c_case!(basic_003, "basic-003");
w3c_case!(basic_004, "basic-004");
w3c_case!(basic_005, "basic-005");
```

- [ ] **Step 2: Run**

Run: `cargo test -p horndb-sparql --test w3c_suite`
Expected: 5 passed.

If a test fails, the most likely cause is a literal-form mismatch
between the fixture and the in-crate JSON serialiser; inspect the
failing test's `panic!` output, fix the serialiser **or** the fixture
(prefer the serialiser), and re-run. Do not loosen the comparator.

- [ ] **Step 3: Commit**

```bash
git add crates/sparql/tests/w3c_suite.rs
git commit -m "$(cat <<'EOF'
test(sparql): drive the 5-test Stage-1 W3C subset

Loads the vendored fixtures, runs each query through the full
parse/translate/plan/exec pipeline, and diffs the SPARQL-JSON
answer against the expected file. Multiset comparison on the
bindings — binding order is not significant per spec.
EOF
)"
```

---

## Task 18: A small synthetic-dataset latency smoke test

**Files:**
- Create: `crates/sparql/benches/latency_smoke.rs` (a regular test, not a Criterion bench, so it runs in CI)

- [ ] **Step 1: Write the smoke test**

Create `crates/sparql/tests/latency_smoke.rs`:

```rust
//! Sanity check on Stage-1 query latency for a small synthetic dataset.
//!
//! We do NOT promise the SPEC-07 NF1 (≤2× GraphDB on SPB SF3) at this
//! stage — that's a Stage-2 commitment. This test only catches gross
//! regressions: 10k triples + a single-pattern SELECT in <1 s on any
//! reasonable laptop.

use horndb_sparql::algebra::Term;
use horndb_sparql::api::{execute_query, QueryAnswer};
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::exec::Store;
use std::time::Instant;

#[test]
fn ten_thousand_triple_scan_in_under_one_second() {
    let mut s = MemStore::default();
    for i in 0..10_000_u32 {
        s.insert_triple(
            Term::Iri(format!("http://ex/s{i}")),
            Term::Iri("http://ex/p".into()),
            Term::Iri(format!("http://ex/o{i}")),
        );
    }
    let q = "SELECT ?o WHERE { <http://ex/s5000> <http://ex/p> ?o }";
    let t = Instant::now();
    let ans = execute_query(q, &s).unwrap();
    let elapsed = t.elapsed();
    match ans {
        QueryAnswer::Solutions { rows, .. } => assert_eq!(rows.len(), 1),
        other => panic!("unexpected: {other:?}"),
    }
    assert!(
        elapsed.as_secs_f64() < 1.0,
        "latency {elapsed:?} exceeds 1s budget — investigate"
    );
}
```

- [ ] **Step 2: Run**

Run: `cargo test -p horndb-sparql --test latency_smoke --release`
Expected: PASS. (The naive MemStore is O(triples) per pattern scan;
10k triples in <1 s in release mode is comfortable. If this fails on
the CI machine, the most likely cause is the test runner being on a
heavily loaded host; bump the budget to 2 s before pessimising the
implementation.)

- [ ] **Step 3: Commit**

```bash
git add crates/sparql/tests/latency_smoke.rs
git commit -m "$(cat <<'EOF'
test(sparql): 10k-triple latency smoke under 1s

Stage-1 sanity check. SPEC-07 NF1 (LDBC SPB latency) is a Stage-2
commitment; this guards against gross regressions only.
EOF
)"
```

---

## Task 19: CI workflow — selected subset is the gate

**Files:**
- Create: `.github/workflows/ci.yml`

- [ ] **Step 1: Check whether CI already exists**

Run: `ls .github/workflows/ 2>/dev/null || echo "no workflows yet"`
Expected: `no workflows yet` (the repo has none committed at plan time).
If a workflow already exists, *modify* it rather than overwriting; otherwise create the file below.

- [ ] **Step 2: Add the workflow**

Create `.github/workflows/ci.yml`:

```yaml
name: ci
on:
  pull_request:
  push:
    branches: [main]

jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 20
    steps:
      - uses: actions/checkout@v4
      - name: Install Rust
        uses: dtolnay/rust-toolchain@stable
        with:
          toolchain: "1.87"
      - name: Cache cargo
        uses: Swatinem/rust-cache@v2
      - name: Format check
        run: cargo fmt --all -- --check
      - name: Clippy
        run: cargo clippy --workspace --all-targets --all-features -- -D warnings
      - name: Test (default features)
        run: cargo test --workspace --all-targets
      - name: Test (sparql + server feature)
        run: cargo test -p horndb-sparql --features server
```

- [ ] **Step 3: Run the same commands locally to confirm they pass**

Run:
```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
cargo test -p horndb-sparql --features server
```
Expected: all green. Fix any `rustfmt` or `clippy` warnings the workflow would flag — typically `cargo fmt --all` (no `--check`) then re-run. Suppress `clippy::too_many_arguments` only if a function genuinely needs them; otherwise refactor.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "$(cat <<'EOF'
ci: gate PRs on workspace tests + sparql server feature

Runs fmt + clippy (-D warnings) + cargo test on every PR. Pinned to
Rust 1.87 to match the workspace MSRV bumped for spargebra.
EOF
)"
```

---

## Task 20: Top-level README pointer and crate docs

**Files:**
- Modify: `crates/sparql/src/lib.rs` (extend the module doc-comment)
- Create: `crates/sparql/README.md`

> This is the *only* documentation file added by this plan. Per workspace policy, we do not gold-plate.

- [ ] **Step 1: Add a brief README for the crate**

Create `crates/sparql/README.md`:

```markdown
# horndb-sparql

SPARQL 1.1 frontend for the HornDB project. See
`specs/SPEC-07-sparql-frontend.md` for the full contract.

## Stage 1 status

Implemented:
- Parser via `spargebra`.
- Algebra translation: BGP, Join, LeftJoin, Filter, Union, Project,
  Distinct, Slice, OrderBy, Extend, Values.
- Planner: 1:1 lowering to `PhysicalPlan` (no cost model).
- Runtime: walks the plan against an `Executor` impl.
- Built-in `MemStore` executor (HashSet) for tests and local use.
- Query forms: SELECT, ASK, CONSTRUCT.
- Update: `INSERT DATA`, `DELETE DATA`.
- Property paths: `/` (sequence), `^` (inverse) only.
- Entailment regimes: simple, materialized OWL 2 RL (marker).
- Result formats: SPARQL JSON, CSV, TSV (XML deferred).
- HTTP `/query` and `/update` (axum, feature-gated `server`,
  default on).

Deferred (Future Work):
- DESCRIBE.
- Full update vocabulary (LOAD/CLEAR/DROP/INSERT WHERE/DELETE WHERE).
- Property paths `*`, `+`, `?`, `|`, `!`.
- GROUP BY / aggregates / HAVING / MINUS / SERVICE.
- Graph Store Protocol.
- EXPLAIN pragma.
- Backward-chained entailment mode.
- True streaming results.
- DBSP-routed updates via SPEC-06.

The full SPARQL 1.1 conformance gate is SPEC-01's responsibility;
this crate vendors a 5-test sanity subset in
`crates/harness/tests/fixtures/sparql11/` and runs it in
`tests/w3c_suite.rs`.

## Running

```bash
cargo test -p horndb-sparql --features server
```

To start the HTTP server in your own binary:

```rust
use horndb_sparql::server::{build_router, AppState};
use horndb_sparql::exec::mem::MemStore;
use std::sync::{Arc, Mutex};

#[tokio::main]
async fn main() {
    let state = AppState { store: Arc::new(Mutex::new(MemStore::default())) };
    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:8080").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
```
```

- [ ] **Step 2: Commit**

```bash
git add crates/sparql/README.md
git commit -m "$(cat <<'EOF'
docs(sparql): crate README documenting Stage-1 surface

Explicit in-scope / deferred lists so contributors can tell at a
glance which SPEC-07 functional requirements this crate currently
delivers. Mirrors the plan's scope boundary.
EOF
)"
```

---

## Self-review checklist (run after Task 20 lands)

This is a checklist for the implementer to run after the final task. Do not skip.

- [ ] Run `cargo test --workspace --all-targets --all-features` — every test in this plan passes.
- [ ] Run `cargo clippy --workspace --all-targets --all-features -- -D warnings` — no warnings.
- [ ] Run `cargo fmt --all -- --check` — clean.
- [ ] Inspect the commit log: 20 commits, each ≈ one task, no `Co-Authored-By` trailers, no AI attribution.
- [ ] Open `crates/sparql/README.md` and confirm every "Implemented" bullet has a corresponding test under `crates/sparql/tests/`.
- [ ] Open `crates/harness/selected.toml` and confirm the `[sparql_query]` section is present and lists exactly the 5 vendored fixtures.
- [ ] Confirm `Cargo.toml` MSRV is `1.87` and the `[workspace.dependencies]` block contains `spargebra`, `oxrdf`, `sparesults`, `axum`, `tokio`, `serde_json`.
- [ ] `cargo doc -p horndb-sparql --no-deps` builds without warnings.

If anything fails, fix it before declaring the plan complete. Do not amend prior commits — add new ones.

---

## Out-of-plan follow-ups (do not implement here)

These are tracked for SPEC-07 Stage 2 and are intentionally **not** in this plan:

1. Replace hand-rolled SPARQL-JSON writer with the `sparesults` crate; gate the migration on a snapshot diff against the current W3C subset.
2. Streaming result writers (HTTP/2 backpressure per SPEC-07 F6).
3. SPEC-03 Executor implementation — swap `MemStore` for the WCOJ-backed `wcoj` crate once SPEC-03 ships its trait surface.
4. SPEC-06 delta routing — wire `apply_update` through DBSP instead of direct `Store::insert_triple` / `Store::delete_triple`.
5. Full SPARQL 1.1 Query Test Suite + Entailment Regimes Test Suite — owned by SPEC-01; grow `selected.toml` monotonically.
6. EXPLAIN pragma.
7. Graph Store Protocol.
8. Property paths `*`, `+`, `?`, `|`, `!`.
9. DESCRIBE.
10. SPARQL XML Results.
