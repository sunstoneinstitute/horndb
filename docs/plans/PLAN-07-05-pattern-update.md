---
status: executed
date: 2026-06-14
scope: "SPEC-07 Pattern-Based Update"
---

# SPEC-07 Pattern-Based Update Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement pattern-based SPARQL Update — `INSERT { … } WHERE { … }`, `DELETE { … } WHERE { … }`, `DELETE WHERE { … }`, and the combined `WITH/DELETE/INSERT … WHERE` form — applying the WHERE solutions as SPEC-06-style deltas against the live store (issue #51, SPEC-07 F5).

**Architecture:** The WHERE clause is a bare `spargebra::GraphPattern`. We translate it through the existing `translate_pattern` → `planner::plan` → `Runtime` pipeline, **collect** all solution rows (dropping the immutable read borrow), then instantiate the DELETE and INSERT templates per row (mirroring `construct_triples`), dropping any triple with an unbound variable. Per SPARQL 1.1 §3.1.3 we apply all deletions first, then all insertions. Evaluation needs both read and write access, so the update path is widened from `Store` to `FullBackend` (`Executor + Store`); the HTTP `/update` handler already carries a `FullBackend`.

**Tech Stack:** Rust, `spargebra` 0.4 (algebra), `crates/sparql` (translate/plan/runtime/update), axum HTTP server.

**Scope boundaries (deferred, consistent with Stage-1):**
- Named-graph targets (`GraphNamePattern::NamedNode`/`Variable` in a template quad, or `WITH <g>` / `USING <g>`): the Stage-1 store is default-graph only. A non-default graph in a template is rejected with `UnsupportedAlgebra`; `USING`/`WITH` named-graph datasets are likewise rejected. `USING`/`WITH` over the default graph is a no-op (single default graph).
- Multi-operation updates (`…; …`): the parser classifies by the single operation as today; multi-op stays `UnsupportedForm`. The trainmarks conditional update is a single `DeleteInsert` op, so this covers it.
- RDF 1.2 triple-term templates: same lexical-form limitation as `INSERT DATA` / `construct_triples` (no canonical `String` slot); such a slot drops the triple.

---

## File Structure

- `crates/sparql/src/parser.rs` — add a `DeleteInsert` variant to `ParsedUpdate`; classify a single-op `GraphUpdateOperation::DeleteInsert` into it.
- `crates/sparql/src/algebra/translate.rs` — expose `pub(crate) fn translate_where(p, cfg)` wrapping the existing private `translate_pattern`.
- `crates/sparql/src/update.rs` — widen `apply_update` to `FullBackend`; add `apply_update_with(u, store, cfg)`; implement the `DeleteInsert` branch with per-solution template instantiation and delete-then-insert application.
- `crates/sparql/src/api.rs` — widen `execute_update` to `FullBackend`; add `execute_update_with(update, store, cfg)`.
- `crates/sparql/tests/update_where.rs` — new integration tests (INSERT-WHERE, DELETE-WHERE, DELETE WHERE shorthand, combined DELETE/INSERT-WHERE, ground-safety, named-graph rejection) run against both `MemStore` and `HornBackend`.
- `crates/sparql/tests/server_http.rs` — add a `/update` test exercising a WHERE form end-to-end.
- `docs/architecture.md` — flip the SPEC-07 pattern-Update Status to implemented (Phase 7 of /next-task).

---

## Task 1: Expose a WHERE-pattern translator

**Files:**
- Modify: `crates/sparql/src/algebra/translate.rs` (near the existing `translate_pattern`, ~line 97)

- [ ] **Step 1: Add the public wrapper**

In `crates/sparql/src/algebra/translate.rs`, add directly above `fn translate_pattern`:

```rust
/// Lower a bare WHERE `GraphPattern` (as carried by `DELETE/INSERT … WHERE`
/// updates) to our `Algebra`. Unlike [`translate_query_with`], there is no
/// surrounding query form / projection — the caller plans and runs it to
/// obtain the solution rows that instantiate the update templates.
pub(crate) fn translate_where(p: &GraphPattern, cfg: &SparqlConfig) -> Result<Algebra> {
    translate_pattern(p, cfg)
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo build -p horndb-sparql`
Expected: builds (a `dead_code` warning for `translate_where` is fine until Task 3 wires it; do not add `#[allow]`).

- [ ] **Step 3: Commit**

```bash
git add crates/sparql/src/algebra/translate.rs
git commit -m "feat(sparql): expose translate_where for bare WHERE patterns (#51)"
```

---

## Task 2: Parser — classify `DELETE/INSERT … WHERE`

**Files:**
- Modify: `crates/sparql/src/parser.rs:40-97`
- Test: `crates/sparql/tests/parser_basic.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/sparql/tests/parser_basic.rs`:

```rust
#[test]
fn classifies_delete_insert_where() {
    use horndb_sparql::parser::{parse_update, ParsedUpdate};
    let u = parse_update(
        "DELETE { ?s <http://ex/p> ?o } INSERT { ?s <http://ex/q> ?o } WHERE { ?s <http://ex/p> ?o }",
    )
    .unwrap();
    assert!(matches!(u, ParsedUpdate::DeleteInsert { .. }));
}

#[test]
fn classifies_delete_where_shorthand() {
    use horndb_sparql::parser::{parse_update, ParsedUpdate};
    let u = parse_update("DELETE WHERE { ?s <http://ex/p> ?o }").unwrap();
    assert!(matches!(u, ParsedUpdate::DeleteInsert { .. }));
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p horndb-sparql --test parser_basic classifies_delete`
Expected: FAIL — `DeleteInsert` variant does not exist (compile error).

- [ ] **Step 3: Add the variant**

In `crates/sparql/src/parser.rs`, add to the `ParsedUpdate` enum (after `DeleteData`):

```rust
    /// Pattern-based update: `INSERT { … } WHERE { … }`,
    /// `DELETE { … } WHERE { … }`, `DELETE WHERE { … }`, or the
    /// combined `WITH/DELETE/INSERT … WHERE` form. spargebra lowers all
    /// of these (including the `DELETE WHERE` shorthand) into a single
    /// `GraphUpdateOperation::DeleteInsert`.
    DeleteInsert {
        inner: Update,
    },
```

- [ ] **Step 4: Classify it in `parse_update`**

In `parse_update`, add a match arm before the `Some(_)` fallback:

```rust
        Some(GraphUpdateOperation::DeleteInsert { .. }) if u.operations.len() == 1 => {
            Ok(ParsedUpdate::DeleteInsert { inner: u })
        }
```

Update the Stage-1 doc comment on `parse_update` to mention the WHERE forms are now recognised (single-op).

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p horndb-sparql --test parser_basic classifies_delete`
Expected: PASS (both).

- [ ] **Step 6: Commit**

```bash
git add crates/sparql/src/parser.rs crates/sparql/tests/parser_basic.rs
git commit -m "feat(sparql): classify DELETE/INSERT … WHERE updates (#51)"
```

---

## Task 3: Apply pattern-based updates

**Files:**
- Modify: `crates/sparql/src/update.rs` (whole file)
- Test: `crates/sparql/tests/update_where.rs` (new)

This task carries the core logic. Implement the helpers and the `DeleteInsert` branch, then prove it with integration tests.

- [ ] **Step 1: Write the failing tests**

Create `crates/sparql/tests/update_where.rs`:

```rust
//! Pattern-based SPARQL Update (`INSERT`/`DELETE … WHERE`) over both
//! Stage-1 backends. Each test applies an update, then queries the store
//! to assert the resulting triples (SPARQL Update has no result set).

use horndb_sparql::api::{execute_query, execute_update, QueryAnswer};
use horndb_sparql::exec::horn::HornBackend;
use horndb_sparql::exec::mem::MemStore;
use horndb_sparql::exec::{FullBackend, Store};
use horndb_sparql::parser::parse_update;
use horndb_sparql::update::apply_update;

fn seed<B: FullBackend + Default>(triples: &[(&str, &str, &str)]) -> B {
    use horndb_sparql::algebra::Term;
    let mut b = B::default();
    for (s, p, o) in triples {
        b.insert_triple(
            Term::Iri((*s).to_owned()),
            Term::Iri((*p).to_owned()),
            Term::Iri((*o).to_owned()),
        );
    }
    b
}

/// Return the set of `?o` IRIs for `<subj> <pred> ?o` as sorted strings.
fn objects_of<B: FullBackend>(store: &B, subj: &str, pred: &str) -> Vec<String> {
    let q = format!("SELECT ?o WHERE {{ <{subj}> <{pred}> ?o }}");
    let QueryAnswer::Solutions { rows, .. } = execute_query(&q, store).unwrap() else {
        panic!("expected solutions");
    };
    let mut out: Vec<String> = rows
        .iter()
        .filter_map(|r| match r.get("o") {
            Some(horndb_sparql::algebra::Term::Iri(s)) => Some(s.clone()),
            _ => None,
        })
        .collect();
    out.sort();
    out
}

fn insert_where<B: FullBackend + Default>() {
    let mut store: B = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    let u = parse_update(
        "INSERT { ?s <http://ex/q> ?o } WHERE { ?s <http://ex/p> ?o }",
    )
    .unwrap();
    apply_update(&u, &mut store).unwrap();
    assert_eq!(objects_of(&store, "http://ex/a", "http://ex/q"), vec!["http://ex/b"]);
    // original triple untouched
    assert_eq!(objects_of(&store, "http://ex/a", "http://ex/p"), vec!["http://ex/b"]);
}

fn delete_where<B: FullBackend + Default>() {
    let mut store: B = seed(&[
        ("http://ex/a", "http://ex/p", "http://ex/b"),
        ("http://ex/a", "http://ex/p", "http://ex/c"),
        ("http://ex/a", "http://ex/keep", "http://ex/d"),
    ]);
    let u = parse_update("DELETE WHERE { <http://ex/a> <http://ex/p> ?o }").unwrap();
    apply_update(&u, &mut store).unwrap();
    assert!(objects_of(&store, "http://ex/a", "http://ex/p").is_empty());
    assert_eq!(objects_of(&store, "http://ex/a", "http://ex/keep"), vec!["http://ex/d"]);
}

fn delete_insert_where<B: FullBackend + Default>() {
    let mut store: B = seed(&[("http://ex/a", "http://ex/old", "http://ex/b")]);
    let u = parse_update(
        "DELETE { ?s <http://ex/old> ?o } INSERT { ?s <http://ex/new> ?o } \
         WHERE { ?s <http://ex/old> ?o }",
    )
    .unwrap();
    apply_update(&u, &mut store).unwrap();
    assert!(objects_of(&store, "http://ex/a", "http://ex/old").is_empty());
    assert_eq!(objects_of(&store, "http://ex/a", "http://ex/new"), vec!["http://ex/b"]);
}

/// A template slot bound to nothing (var not in WHERE) drops that triple,
/// not the whole update.
fn ground_safety_drops_unbound<B: FullBackend + Default>() {
    let mut store: B = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    let u = parse_update(
        "INSERT { ?s <http://ex/q> ?missing . ?s <http://ex/r> ?o } \
         WHERE { ?s <http://ex/p> ?o }",
    )
    .unwrap();
    apply_update(&u, &mut store).unwrap();
    // ?missing is unbound -> first template triple dropped
    assert!(objects_of(&store, "http://ex/a", "http://ex/q").is_empty());
    // second template triple is fully ground -> inserted
    assert_eq!(objects_of(&store, "http://ex/a", "http://ex/r"), vec!["http://ex/b"]);
}

fn named_graph_template_rejected<B: FullBackend + Default>() {
    let mut store: B = seed(&[("http://ex/a", "http://ex/p", "http://ex/b")]);
    let u = parse_update(
        "INSERT { GRAPH <http://ex/g> { ?s <http://ex/q> ?o } } \
         WHERE { ?s <http://ex/p> ?o }",
    )
    .unwrap();
    let err = apply_update(&u, &mut store).unwrap_err();
    assert!(format!("{err}").to_lowercase().contains("graph"));
}

#[test]
fn mem_insert_where() { insert_where::<MemStore>() }
#[test]
fn horn_insert_where() { insert_where::<HornBackend>() }
#[test]
fn mem_delete_where() { delete_where::<MemStore>() }
#[test]
fn horn_delete_where() { delete_where::<HornBackend>() }
#[test]
fn mem_delete_insert_where() { delete_insert_where::<MemStore>() }
#[test]
fn horn_delete_insert_where() { delete_insert_where::<HornBackend>() }
#[test]
fn mem_ground_safety() { ground_safety_drops_unbound::<MemStore>() }
#[test]
fn horn_ground_safety() { ground_safety_drops_unbound::<HornBackend>() }
#[test]
fn mem_named_graph_rejected() { named_graph_template_rejected::<MemStore>() }
#[test]
fn horn_named_graph_rejected() { named_graph_template_rejected::<HornBackend>() }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p horndb-sparql --test update_where`
Expected: FAIL — `apply_update` still errors on `DeleteInsert` (and its bound is `Store`, not `FullBackend`).

- [ ] **Step 3: Rewrite `update.rs` with the `FullBackend` path and `DeleteInsert` branch**

Replace the body of `crates/sparql/src/update.rs` with the following (keeps the existing DATA-form handling and helpers, widens the bound, and adds the pattern path):

```rust
//! SPARQL Update — `INSERT DATA` / `DELETE DATA` plus pattern-based
//! `INSERT`/`DELETE … WHERE` (SPEC-07 F5).
//!
//! Graph-management verbs (`LOAD`, `CLEAR`, `DROP`, `CREATE`, …) and
//! multi-operation updates are still parsed but rejected at apply time
//! (see `parser::ParsedUpdate::UnsupportedForm` and SPEC-07 Future Work).

use crate::algebra::translate::translate_where;
use crate::algebra::Term;
use crate::error::{Result, SparqlError};
use crate::exec::{Bindings, FullBackend};
use crate::parser::ParsedUpdate;
use crate::plan::planner;
use crate::exec::runtime::Runtime;
use crate::SparqlConfig;
use spargebra::term::{
    GraphNamePattern, GroundQuadPattern, GroundTerm, GroundTermPattern, NamedNodePattern,
    NamedOrBlankNode, QuadPattern, Term as SpgTerm, TermPattern,
};

/// Lexical form for an RDF 1.2 triple term embedded in an update. The
/// Stage-1 store carries `Term::Literal(String)` slots only, so there is
/// no in-store representation for a triple term in this crate.
fn triple_term_unsupported() -> SparqlError {
    SparqlError::UnsupportedAlgebra("RDF 1.2 triple term in update (SPARQL 1.1 mode)".into())
}

fn named_graph_unsupported() -> SparqlError {
    SparqlError::UnsupportedAlgebra(
        "named-graph target in update (Stage-1 default graph only)".into(),
    )
}

/// Apply an update with the default [`SparqlConfig`] (SPARQL 1.1).
pub fn apply_update<B: FullBackend>(u: &ParsedUpdate, store: &mut B) -> Result<()> {
    apply_update_with(u, store, &SparqlConfig::default())
}

/// Apply an update, taking an explicit [`SparqlConfig`].
pub fn apply_update_with<B: FullBackend>(
    u: &ParsedUpdate,
    store: &mut B,
    cfg: &SparqlConfig,
) -> Result<()> {
    use spargebra::GraphUpdateOperation;
    let ops = match u {
        ParsedUpdate::InsertData { inner }
        | ParsedUpdate::DeleteData { inner }
        | ParsedUpdate::DeleteInsert { inner } => &inner.operations,
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
                    let s = subject_to_term(&q.subject);
                    let p = Term::Iri(q.predicate.as_str().to_owned());
                    let o = object_to_term(&q.object)?;
                    store.insert_triple(s, p, o);
                }
            }
            GraphUpdateOperation::DeleteData { data } => {
                for q in data {
                    let s = Term::Iri(q.subject.as_str().to_owned());
                    let p = Term::Iri(q.predicate.as_str().to_owned());
                    let o = ground_term_to_term(&q.object)?;
                    store.delete_triple(&s, &p, &o);
                }
            }
            GraphUpdateOperation::DeleteInsert {
                delete,
                insert,
                using: _,
                pattern,
            } => {
                apply_delete_insert(store, cfg, delete, insert, pattern)?;
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

/// Evaluate the WHERE pattern, then instantiate the DELETE/INSERT
/// templates per solution. Per SPARQL 1.1 §3.1.3 the deletions are
/// computed and applied before the insertions; both are derived from the
/// WHERE solutions over the *pre-update* graph (we collect every row
/// first, which also releases the immutable read borrow before mutating).
fn apply_delete_insert<B: FullBackend>(
    store: &mut B,
    cfg: &SparqlConfig,
    delete: &[GroundQuadPattern],
    insert: &[QuadPattern],
    pattern: &spargebra::algebra::GraphPattern,
) -> Result<()> {
    // Reject named-graph templates up front (Stage-1 default graph only),
    // so a partially-applied update can't leave the store inconsistent.
    for q in delete {
        require_default_graph(&q.graph_name)?;
    }
    for q in insert {
        require_default_graph(&q.graph_name)?;
    }

    let alg = translate_where(pattern, cfg)?;
    let plan = planner::plan(&alg)?;
    let rows: Vec<Bindings> = Runtime::new(store).run(&plan)?.collect();

    // Compute deletions from the original bindings first.
    let mut deletions: Vec<(Term, Term, Term)> = Vec::new();
    for row in &rows {
        for q in delete {
            if let (Some(s), Some(p), Some(o)) = (
                resolve_ground(&q.subject, row),
                resolve_pred(&q.predicate, row),
                resolve_ground(&q.object, row),
            ) {
                deletions.push((s, p, o));
            }
        }
    }
    // Insertions allocate fresh blank nodes per solution row.
    let mut insertions: Vec<(Term, Term, Term)> = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        for q in insert {
            if let (Some(s), Some(p), Some(o)) = (
                resolve_term(&q.subject, row, i),
                resolve_pred(&q.predicate, row),
                resolve_term(&q.object, row, i),
            ) {
                insertions.push((s, p, o));
            }
        }
    }

    for (s, p, o) in &deletions {
        store.delete_triple(s, p, o);
    }
    for (s, p, o) in insertions {
        store.insert_triple(s, p, o);
    }
    Ok(())
}

fn require_default_graph(g: &GraphNamePattern) -> Result<()> {
    match g {
        GraphNamePattern::DefaultGraph => Ok(()),
        GraphNamePattern::NamedNode(_) | GraphNamePattern::Variable(_) => {
            Err(named_graph_unsupported())
        }
    }
}

/// Resolve an INSERT-template `TermPattern` against a solution row.
/// `row_ix` scopes per-solution blank nodes so each row's template
/// blank node is distinct (SPARQL 1.1 §4.1.4). Returns `None` when a
/// variable slot is unbound (the caller drops the triple).
fn resolve_term(t: &TermPattern, row: &Bindings, row_ix: usize) -> Option<Term> {
    match t {
        TermPattern::NamedNode(n) => Some(Term::Iri(n.as_str().to_owned())),
        TermPattern::Literal(l) => Some(Term::Literal(l.to_string())),
        TermPattern::BlankNode(b) => {
            Some(Term::BlankNode(format!("{}_r{row_ix}", b.as_str())))
        }
        TermPattern::Variable(v) => row.get(v.as_str()).cloned(),
        TermPattern::Triple(_) => None,
    }
}

/// Resolve a DELETE-template `GroundTermPattern` (no blank nodes allowed
/// in DELETE templates) against a solution row.
fn resolve_ground(t: &GroundTermPattern, row: &Bindings) -> Option<Term> {
    match t {
        GroundTermPattern::NamedNode(n) => Some(Term::Iri(n.as_str().to_owned())),
        GroundTermPattern::Literal(l) => Some(Term::Literal(l.to_string())),
        GroundTermPattern::Variable(v) => row.get(v.as_str()).cloned(),
        GroundTermPattern::Triple(_) => None,
    }
}

fn resolve_pred(p: &NamedNodePattern, row: &Bindings) -> Option<Term> {
    match p {
        NamedNodePattern::NamedNode(n) => Some(Term::Iri(n.as_str().to_owned())),
        NamedNodePattern::Variable(v) => match row.get(v.as_str()) {
            Some(Term::Iri(s)) => Some(Term::Iri(s.clone())),
            _ => None,
        },
    }
}

fn subject_to_term(s: &NamedOrBlankNode) -> Term {
    match s {
        NamedOrBlankNode::NamedNode(n) => Term::Iri(n.as_str().to_owned()),
        NamedOrBlankNode::BlankNode(b) => Term::BlankNode(b.as_str().to_owned()),
    }
}

fn object_to_term(t: &SpgTerm) -> Result<Term> {
    Ok(match t {
        SpgTerm::NamedNode(n) => Term::Iri(n.as_str().to_owned()),
        SpgTerm::BlankNode(b) => Term::BlankNode(b.as_str().to_owned()),
        SpgTerm::Literal(l) => Term::Literal(l.to_string()),
        SpgTerm::Triple(_) => return Err(triple_term_unsupported()),
    })
}

fn ground_term_to_term(gt: &GroundTerm) -> Result<Term> {
    Ok(match gt {
        GroundTerm::NamedNode(n) => Term::Iri(n.as_str().to_owned()),
        GroundTerm::Literal(l) => Term::Literal(l.to_string()),
        GroundTerm::Triple(_) => return Err(triple_term_unsupported()),
    })
}
```

> **Implementer notes:**
> - Confirm the `runtime`/`planner` module paths against the crate (`crate::exec::runtime::Runtime`, `crate::plan::planner::plan`) — mirror the `use`s in `api.rs`.
> - `Runtime::new(store).run(&plan)?` borrows `store` immutably; the `.collect()` into `rows` ends that borrow before the `delete_triple`/`insert_triple` loops. Do **not** hold the iterator across the mutations.
> - For the predicate, only an IRI binding is valid; a non-IRI predicate binding drops the triple (matches `construct_triples`).

- [ ] **Step 4: Run the new tests to verify they pass**

Run: `cargo test -p horndb-sparql --test update_where`
Expected: PASS (all 11).

- [ ] **Step 5: Run the existing update test to confirm no regression**

Run: `cargo test -p horndb-sparql --test update_insert_delete`
Expected: PASS (the `S: Store` → `B: FullBackend` widening keeps `MemStore` callers compiling).

- [ ] **Step 6: Commit**

```bash
git add crates/sparql/src/update.rs crates/sparql/tests/update_where.rs
git commit -m "feat(sparql): apply INSERT/DELETE … WHERE updates as deltas (#51)"
```

---

## Task 4: Widen the `execute_update` API seam

**Files:**
- Modify: `crates/sparql/src/api.rs:86-89`

- [ ] **Step 1: Widen the bound and add the `_with` variant**

Replace the existing `execute_update` in `crates/sparql/src/api.rs` with:

```rust
pub fn execute_update<B: FullBackend>(update: &str, store: &mut B) -> Result<()> {
    execute_update_with(update, store, &SparqlConfig::default())
}

/// Like [`execute_update`] but takes an explicit [`SparqlConfig`].
pub fn execute_update_with<B: FullBackend>(
    update: &str,
    store: &mut B,
    cfg: &SparqlConfig,
) -> Result<()> {
    let parsed = parse_update(update)?;
    apply_update_with(&parsed, store, cfg)
}
```

Update the imports at the top of `api.rs`:
- add `FullBackend` to the `use crate::exec::{…}` line,
- change `use crate::update::apply_update;` to `use crate::update::{apply_update_with};` (drop `apply_update` if now unused — let the compiler tell you).

- [ ] **Step 2: Verify the workspace builds**

Run: `cargo build -p horndb-sparql --features server`
Expected: builds. `server/update.rs` already holds a `FullBackend` store, so `execute_update(&update, &mut *store)` now type-checks unchanged.

- [ ] **Step 3: Commit**

```bash
git add crates/sparql/src/api.rs
git commit -m "feat(sparql): widen execute_update to FullBackend + add _with variant (#51)"
```

---

## Task 5: End-to-end `/update` WHERE-form HTTP test

**Files:**
- Modify: `crates/sparql/tests/server_http.rs`

- [ ] **Step 1: Inspect the existing harness**

Read `crates/sparql/tests/server_http.rs` to reuse its router/`AppState` construction and request helper. Match the existing test style (do not invent a new client).

- [ ] **Step 2: Add the test**

Add a test that (a) POSTs `INSERT DATA` to seed, (b) POSTs an `INSERT { ?s <q> ?o } WHERE { ?s <p> ?o }` update and asserts `204 No Content`, (c) POSTs a SELECT to `/query` and asserts the inserted triple is present. Follow the exact request-construction pattern already used in the file (content-type `application/sparql-update` for `/update`).

- [ ] **Step 3: Run it**

Run: `cargo test -p horndb-sparql --features server --test server_http`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/sparql/tests/server_http.rs
git commit -m "test(sparql): /update INSERT … WHERE end-to-end over HTTP (#51)"
```

---

## Task 6: Full verification + docs sync

**Files:**
- Modify: `docs/architecture.md` (SPEC-07 Update Status row)

- [ ] **Step 1: Format, lint, test**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p horndb-sparql --features server
```
Expected: all green.

- [ ] **Step 2: Flip the architecture Status**

In `docs/architecture.md`, locate the SPEC-07 row covering pattern-based Update (`INSERT`/`DELETE … WHERE`) and change its Status from planned/specified to **implemented**, noting default-graph-only / single-op scope.

- [ ] **Step 3: Commit the docs**

```bash
git add docs/architecture.md
git commit -m "docs(architecture): pattern-based SPARQL Update implemented (#51)"
```

(`TASKS.md` is intentionally **not** touched on the branch — its `[v]`→`[x]` flip is a locked commit on `main` after merge, per the /next-task workflow.)

---

## Self-Review

**Spec coverage (issue #51):**
- INSERT/DELETE-WHERE + combined WITH/INSERT/DELETE/USING form → Task 3 (`apply_delete_insert`, all DeleteInsert sub-fields handled; `using` is a default-graph no-op).
- Ground-template safety (drop unbound) → Task 3 `resolve_*` returning `None`, tested by `ground_safety_drops_unbound`.
- Wire through `/update` HTTP handler → Task 4 (API widening; handler unchanged) + Task 5 (e2e test).
- Apply as SPEC-06 deltas, insertion-only consistent → uses `Store::insert_triple`/`delete_triple`; HornBackend deletes via tombstones (already insertion-only-consistent).
- Acceptance: INSERT/DELETE-WHERE rows → grown coverage in `update_where.rs` across both backends (harness-first: query-only `[sparql_query]` suite can't express updates, so integration tests are the grown subset).

**Placeholder scan:** Task 5 leaves the exact request-builder to the implementer because it must match the existing `server_http.rs` style verbatim — Step 1 mandates reading that file first. All code-bearing steps include full code.

**Type consistency:** `apply_update`/`apply_update_with` (update.rs) and `execute_update`/`execute_update_with` (api.rs) bounds match (`B: FullBackend`). `resolve_term(t, row, row_ix)` / `resolve_ground(t, row)` / `resolve_pred(p, row)` signatures are used consistently in `apply_delete_insert`. `translate_where` defined in Task 1, consumed in Task 3.
