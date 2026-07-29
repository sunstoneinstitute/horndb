---
status: draft
date: 2026-07-29
scope: "SPEC-28 phase 1 (S1) — refuse, do not lie: GRAPH patterns and non-empty FROM/FROM NAMED dataset clauses become explicit translate-time errors surfaced as HTTP 400, replacing today's silent wrong answers"
---

# SPEC-28 phase 1 — Refuse, do not lie

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A named-graph query returns an explicit error instead of
default-graph rows. Tracking issue:
[#264](https://github.com/sunstoneinstitute/horndb/issues/264). Spec:
`docs/specs/SPEC-28-named-graph-dataset-semantics.md` §S1.

**Architecture:** Two translate-time changes in
`crates/sparql/src/algebra/translate.rs`: the `GraphPattern::Graph` arm stops
dropping its wrapper and errors, and the four `dataset: _` bindings in
`translate_query_with` become real matches that error on a non-empty
`QueryDataset`. The server needs **zero** changes: every `SparqlError`
already maps to HTTP 400 (`server/query.rs:125,306,331`), which is exactly
the status S1 wants. Phase 3 (PLAN-28-03) later replaces both errors with
real evaluation.

**Tech Stack:** Rust 1.90, `crates/sparql` only.

---

## Design (read this before any task)

- **The Graph arm** (`translate.rs:269-274`) currently reads
  `GraphPattern::Graph { name: _, inner } => translate_pattern(inner, cfg)`
  under a comment whose premise ("the executor holds a single graph") died
  with SPEC-25 S1. It becomes
  `Err(SparqlError::UnsupportedAlgebra(...))` for both the ground and the
  variable form. Error text must name the construct so a caller can act on
  it (S1): `"GRAPH named-graph pattern (named-graph queries are refused
  until SPEC-28 phase 3; see #264)"`.
- **The dataset arms.** All four `translate_query_with` arms
  (`translate.rs:39,44,56,67`) bind `dataset: _` on an
  `Option<spargebra::algebra::QueryDataset>` where
  `QueryDataset { default: Vec<NamedNode>, named: Option<Vec<NamedNode>> }`.
  The error condition mirrors the update side's
  `validate_delete_insert` (`update.rs:508-512`):
  `ds.default.is_empty() && ds.named.as_ref().is_none_or(|n| n.is_empty())`
  is a no-op; anything else errors with
  `"FROM/FROM NAMED dataset clause (dataset selection is refused until
  SPEC-28 phase 3; see #264)"`. Factor the check into one helper called
  from all four arms — do not copy the condition four times.
- **`collect_visible_vars`** (`translate.rs:530-537`) pushes the graph-name
  variable into scope for `SELECT *`. It becomes unreachable for `Graph`
  patterns once translation errors first, so it needs no change — but the
  test that pinned its effect does (below).
- **Behaviour pins that must flip**, each updated with a comment citing
  this plan:
  - `crates/sparql/tests/exec_expressions.rs:376`
    `graph_iri_lowers_to_inner_pattern` → asserts
    `Err(UnsupportedAlgebra)` naming `GRAPH`.
  - `exec_expressions.rs:388` `graph_var_lowers_with_unbound_graph_var` →
    same.
  - `exec_expressions.rs:433` `select_star_keeps_graph_var_visible` → same
    (the query can no longer produce a header at all).
  - `crates/sparql/tests/logical_pipeline.rs:277`
    `graph_adjacent_bgps_coalesce_and_stay_result_equivalent` — this test's
    query used `GRAPH` lowering to *produce* the `Join(Bgp, Bgp)` shape the
    `CoalesceBgp` pass exists for. Rewrite it to produce the same shape
    without `GRAPH` (two braced group graph patterns —
    `{ ?a :p ?b } { ?b :q ?c }` — translate to `Join(Bgp, Bgp)`); verify
    with `plan_of` that the pass still fires. Update the pass rationale
    comment at `plan/pass.rs:281-285` in the same commit — its motivating
    example is now historical.
  - `crates/sparql/tests/update_where.rs:303-311` — comment (not
    behaviour) references the transparent lowering; update the wording.
    The update path's own rejections are untouched.
- **What must NOT change:** every graph-free query. The selected
  conformance subset (`harness/selected.toml`) is the regression gate; no
  selection change this phase.

### File map

- Modify: `crates/sparql/src/algebra/translate.rs`
- Modify: `crates/sparql/tests/exec_expressions.rs`,
  `crates/sparql/tests/logical_pipeline.rs`,
  `crates/sparql/src/plan/pass.rs` (comment),
  `crates/sparql/tests/update_where.rs` (comment)
- Modify: `crates/sparql/tests/server_http.rs` (new 400 tests)
- Modify: `docs/architecture.md`, `crates/sparql/INTEGRATION-NOTES.md`,
  this plan (status)

---

### Task 1: Translate-time refusal

**Files:**
- Modify: `crates/sparql/src/algebra/translate.rs`
- Modify: `crates/sparql/tests/exec_expressions.rs`

- [ ] **Step 1: Write the failing tests** — in `exec_expressions.rs`,
  replace the three lowering pins (`:376`, `:388`, `:433`) with refusal
  pins, and add dataset-clause pins:
  ```rust
  #[test]
  fn graph_pattern_is_refused_ground_and_var() {
      for q in [
          "SELECT ?s WHERE { GRAPH <http://ex/g> { ?s ?p ?o } }",
          "SELECT ?s ?g WHERE { GRAPH ?g { ?s ?p ?o } }",
          "SELECT * WHERE { GRAPH ?g { ?s ?p ?o } }",
      ] {
          let err = translate_str_err(q); // helper: parse + translate, unwrap_err
          assert!(err.to_string().contains("GRAPH"), "{err}");
      }
  }

  #[test]
  fn dataset_clause_is_refused_all_query_forms() {
      for q in [
          "SELECT ?s FROM <http://ex/g> WHERE { ?s ?p ?o }",
          "SELECT ?s FROM NAMED <http://ex/g> WHERE { ?s ?p ?o }",
          "ASK FROM <http://ex/g> { ?s ?p ?o }",
          "CONSTRUCT { ?s ?p ?o } FROM <http://ex/g> WHERE { ?s ?p ?o }",
          "DESCRIBE <http://ex/x> FROM <http://ex/g>",
      ] {
          let err = translate_str_err(q);
          assert!(err.to_string().contains("FROM"), "{err}");
      }
  }

  #[test]
  fn absent_dataset_still_translates() {
      // Graph-free query with no FROM stays a no-op — the regression guard.
      translate_str_ok("SELECT ?s WHERE { ?s ?p ?o }");
  }
  ```
  (Follow the file's existing parse/translate helper conventions; add
  `translate_str_err`/`_ok` if no equivalent exists.)
- [ ] **Step 2: Run tests, verify they fail** — `cargo nextest run -p
  horndb-sparql graph_pattern_is_refused dataset_clause_is_refused` — the
  refusal tests fail (queries currently translate fine).
- [ ] **Step 3: Implement** — the `Graph` arm error; a
  `fn refuse_nonempty_dataset(ds: &Option<QueryDataset>) -> Result<()>`
  helper; call it in all four `translate_query_with` arms (the bindings
  become `dataset`, not `dataset: _`). Delete the dead Stage-1 comment
  block at `translate.rs:268-273` and replace it with one line pointing at
  SPEC-28 phase 3.
- [ ] **Step 4: Run the crate suite** — `cargo nextest run -p horndb-sparql`.
- [ ] **Step 5: Commit** — `fix(sparql): refuse GRAPH and FROM/FROM NAMED
  instead of silently dropping them (SPEC-28 S1, #264)`.

### Task 2: Test-suite reconciliation

**Files:**
- Modify: `crates/sparql/tests/logical_pipeline.rs`,
  `crates/sparql/src/plan/pass.rs`, `crates/sparql/tests/update_where.rs`

- [ ] **Step 1:** Rewrite
  `graph_adjacent_bgps_coalesce_and_stay_result_equivalent` per the design
  note (braced groups instead of `GRAPH`); assert the `CoalesceBgp` pass
  still fires and results are unchanged. Update the `pass.rs:281-285`
  rationale comment and the `update_where.rs:303` comment.
- [ ] **Step 2:** Full crate suite: `cargo nextest run -p horndb-sparql` —
  zero failures.
- [ ] **Step 3: Commit** — `test(sparql): reconcile GRAPH-lowering pins with
  the S1 refusal (#264)`.

### Task 3: Server-level 400 pin

**Files:**
- Modify: `crates/sparql/tests/server_http.rs`

- [ ] **Step 1: Write the tests** — following the
  `parse_error_returns_400` template (`server_http.rs:120`):
  `graph_query_returns_400_naming_graph` (POST a `GRAPH <g>` query, assert
  `StatusCode::BAD_REQUEST` and body contains `"GRAPH"`) and
  `from_query_returns_400_naming_from` (same for `FROM`). Both against the
  streaming SELECT path (it plans before store access, `query.rs:124-125`)
  and one ASK to cover `run_materialized`.
- [ ] **Step 2: Run** — `cargo nextest run -p horndb-sparql --features
  server graph_query_returns_400 from_query_returns_400` → pass (no server
  code change was needed; these are pins, and acceptance criterion 1's
  evidence).
- [ ] **Step 3: Commit** — `test(sparql): pin HTTP 400 for GRAPH and
  dataset-clause queries (SPEC-28 S1, #264)`.

### Task 4: Docs sync

**Files:**
- Modify: `docs/architecture.md`, `crates/sparql/INTEGRATION-NOTES.md`,
  this plan

- [ ] **Step 1:** `docs/architecture.md`: the `GRAPH` named-graph patterns
  row flips **broken — returns wrong answers** → **refused (explicit 400;
  SPEC-28 phase 1)**, keeping the pointer to phase 3 for real support.
  `crates/sparql/INTEGRATION-NOTES.md`: the "currently WRONG" section
  (#261) is rewritten to describe the refusal and point at PLAN-28-03 for
  evaluation. Flip this plan's status (in-progress at Task 1, executed
  here).
- [ ] **Step 2:** `cargo fmt --all`, `cargo clippy --workspace --all-targets
  -- -D warnings`, `cargo nextest run -p horndb-sparql --features server`.
- [ ] **Step 3: Commit** — `docs(sparql): GRAPH/dataset refusal sync
  (SPEC-28 S1, #264)`.

---

## Self-review notes

- S1's four bullets map: bullet 1 (GRAPH errors) → Task 1; bullet 2
  (dataset errors, mirroring `validate_delete_insert`) → Task 1; bullet 3
  (HTTP 400, construct named) → Task 3 (no server change needed — verified
  against `server/query.rs:125,306,331`); bullet 4 (no graph-free change)
  → Task 1 step 4 + Task 2 step 2 + the untouched conformance selection.
- The `CoalesceBgp` interaction is the one non-obvious blast site; it is
  handled, not discovered.
