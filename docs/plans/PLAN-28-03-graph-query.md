---
status: executed
date: 2026-07-29
scope: "SPEC-28 phase 3 (S3) — query-side named graphs: Algebra::Graph, ground and variable evaluation with the graph as a scan scope, FROM/FROM NAMED dataset construction, the union|strict default-graph mode with its config and per-query override, path and pushdown scoping, and the W3C graph/ + dataset/ conformance families"
---

# SPEC-28 phase 3 — Query: `GRAPH` and dataset construction

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `GRAPH <g>`, `GRAPH ?g`, `FROM`, and `FROM NAMED` evaluate with
SPARQL 1.1 semantics; the no-dataset default graph is the union of
non-reserved graphs (switchable to `strict`); phase 1's refusals are
removed. Tracking issue:
[#266](https://github.com/sunstoneinstitute/horndb/issues/266). Spec:
`docs/specs/SPEC-28-named-graph-dataset-semantics.md` §S3, decisions
D2–D6. **Depends on phase 2** (PLAN-28-02: `scan_graph`,
`iter_graph_term_ids`, visibility-filtered `graphs()`, `graph_uri`).

**Architecture:** The graph scope is carried as a field **on the scan
nodes**, not as a runtime wrapper operator (D5): translation produces
`Algebra::Graph { name, inner }`, and lowering pushes the scope down onto
every `BgpScan`/`CountScan`/`GroupCountScan` under it, erroring on a scope
it cannot push (there is deliberately no post-filter fallback). Execution
resolves a scope to a **scoped WCOJ snapshot**: `HornBackend::wcoj_snapshot`
becomes `wcoj_snapshot(&GraphScopeKey)` with a per-scope memo. `GRAPH ?g` is
one scan node whose operator loops over the snapshot's named graphs at
runtime and emits `?g` as an extra output column — plan size never grows
with graph count (D6), and `?g` binds as `Slot::Id(TermId(g.0))` because
`GraphId` *is* the interned `TermId` (`store.rs:151-153`), so decode is the
existing `decode_term`.

**Tech Stack:** Rust 1.90; `crates/sparql`, `crates/config`; checked-in W3C
fixtures for `graph/` + `dataset/`.

---

## Execution notes — where the plan and reality diverged

Recorded when the last task landed (Task 7). Everything below this section is
the plan **as written**; these are the points where the delivered code differs.
Current behaviour lives in `docs/architecture.md` and
`crates/sparql/INTEGRATION-NOTES.md`, not here.

- **Two `GRAPH ?g` shapes are refused, not answered.** The plan assumed every
  shape would evaluate. Two do not, and each was a silent wrong answer before
  the refusal landed: (1) a barrier node between the wrapper and its scan
  leaves — `Project` (sub-`SELECT`), `Distinct`, `Group`, `Slice`,
  `PathClosure`, `Values` — drops or merges the graph column; (2) a block that
  **reads** `?g` in a position where binding on the leaf diverges from SPARQL
  1.1 §18.2.2.2's post-join (any expression, `BIND(… AS ?g)`, or `?g` in a
  `LeftJoin`'s right arm). Both raise `UnsupportedAlgebra` from `plan/lower.rs`
  (`per_graph_barrier`, `per_graph_var_divergence`). Lifting them needs
  per-graph evaluation of the whole block, which is a change to D5/D6.
- **`PassId::CountPushdown` does not exist** (the design's differential-battery
  bullet assumed it). `PassId` has six variants, none of them the count
  pushdown: that pushdown is a *physical* rewrite in `Runtime::run_stream`,
  downstream of the logical pass pipeline. The battery therefore switches it
  off with `Runtime::run_unpruned_for_test` — the same off-switch the crate's
  existing `rewrite_is_result_invariant` battery uses, and a strictly stronger
  one (it disables the whole module, not one pass).
- **The `graph/` and `dataset/` families are W3C SPARQL 1.0 (DAWG) tests**, not
  the SPARQL 1.1 tarball the design assumed. `fetch-w3c-suites.sh` gained a
  `sparql10` section with an explicit case allowlist. SPEC-28 S7 carries the
  same correction.
- **`SnapshotScope` has no `PerGraph` variant.** The design's sketch listed
  one. The per-graph seam is `ResolvedScope::PerGraph` at the *operator* level
  (`exec/scope.rs`), which matches the design's own prose that `PerGraph` does
  not build one flattened source. The backend's `SnapshotScope` only ever names
  one set of triples.
- **Graph-scoped snapshots are deliberately not memoized.** The design said the
  memo becomes a keyed map; it did, but only whole-dataset scopes are cached
  (`SnapshotScope::memoisable`). Caching per graph would let an unauthenticated
  `/query` walking `GRAPH <g1>`…`GRAPH <gN>` pin six sorted index copies of the
  store per graph named, evicted only by a write.
- **The per-query URL parameter is spelled `default_graph`**, not the
  originally-planned `default-graph` — amended in Task 2, reasoning at the
  "Config and the per-query override" heading below.

**Delivered:** 24 of the 29 upstream `graph/`+`dataset/` cases selected and
green on both backends; the other 5 are in `harness/KNOWN-MANIFEST-BUGS.md`
with the capability that gates each, including the note that no selected case
grades the shipping `union` default-graph mode.

---

## Design (read this before any task)

### New algebra and its journey through the tiers

```rust
// crates/sparql/src/algebra/mod.rs
pub enum GraphSpec {            // the scope attached by a GRAPH pattern
    Iri(String),                // GRAPH <g> { … }
    Var(Var),                   // GRAPH ?g { … }
}
Algebra::Graph { name: GraphSpec, inner: Box<Algebra> }
```

Translation (`translate.rs`): the phase-1 error arm becomes
`Algebra::Graph` construction; the four `translate_query_with` arms stop
erroring on a dataset and instead record it (below). `collect_visible_vars`
keeps its existing behaviour of scoping the graph variable
(`translate.rs:530-537`) — now correct instead of vacuous.

Lowering does **not** keep a `Graph` node: `plan/lower.rs` rewrites
`Graph { name, inner }` by setting the scope on every scan leaf inside
`inner`:

```rust
// on BgpScan / CountScan / GroupCountScan / (transitively) PathClosure.edge
pub enum GraphScope {
    DefaultGraph,          // the query's default graph (mode- and dataset-dependent)
    Named(GraphSpec),      // inside GRAPH <g> / GRAPH ?g
}
```

Every scan node gains `scope: GraphScope` (default `DefaultGraph`). Nested
`GRAPH` overrides outer scope (innermost wins, per SPARQL). `Values`,
`Filter`, joins, etc. pass through untouched. A `GraphScope::Named(Var)`
whose variable is already bound by an enclosing scope's column joins on
equality like any shared variable — the batch schema handles that for free.

### Dataset construction and the default-graph mode

```rust
// crates/sparql/src/lib.rs
pub enum DefaultGraphMode { Union, Strict }       // D2
pub struct SparqlConfig {
    pub rdf12: bool,
    pub default_graph: DefaultGraphMode,          // default Union
}

// resolved per query, threaded to the executor
pub struct DatasetSpec {
    /// None = no FROM: the mode decides (union of non-reserved graphs | sentinel only).
    /// Some(v) = FROM list: default graph is the term-level set union of these graphs.
    pub default: Option<Vec<String>>,
    /// None = no FROM NAMED: all non-reserved graphs are nameable/enumerable.
    /// Some(v) = exactly these (empty vec = empty named set).
    pub named: Option<Vec<String>>,
}
```

Rules pinned here (all from S3/D2–D4):

- `FROM` list present → default graph = union of exactly those graphs
  (term-level set union; RDF-merge bnode disjointness is not implemented —
  the platform skolemizes, and W3C cases that require merge-renaming stay
  out of the selection with a `KNOWN-MANIFEST-BUGS.md` entry). Reserved
  graphs may be named explicitly in `FROM`/`FROM NAMED` — naming is the
  opt-in.
- `FROM NAMED` without `FROM` → **empty default graph** (D4;
  `DatasetSpec { default: Some(vec![]), … }` — note `Some(empty)`, distinct
  from `None`).
- No dataset clause → `default: None, named: None`; execution composes the
  default graph per the mode and the named set as all non-reserved graphs.
  Reserved graphs (IRI prefix `https://horndb.io/graph/`) are excluded from
  the union and from `GRAPH ?g` enumeration in both modes; the
  SPEC-29 `default_dataset_includes_inferred` flag is **not** implemented
  here (it arrives with PLAN-29-01) — the exclusion rule is.
- `GRAPH ?g` never binds the default graph (D3), in both modes.
- Unknown graph IRI anywhere → zero rows, never an error.

### Config and the per-query override

Verified gaps this plan must fill (not work around): `AppState` carries no
config (`server/mod.rs:35`), handlers construct `SparqlConfig::default()`
(`server/query.rs:124,328`), and the SPEC-26 S4 URL-override whitelist is
**prose only** (`config/src/model.rs:182-184` defers to PLAN-26-02; nothing
in `crates/sparql` reads `QuerySettings`). Minimal forward-compatible
slice:

- `crates/config`: `Limits` gains `default_graph: DefaultGraph` — a
  serde-level enum (`union | strict`, default `union`), not a free string:
  an unrecognized value is then rejected by figment/serde itself, with
  file+key source attribution (SPEC-26 S1), instead of a hand-written check
  that only names the value (contrast `[simd].max_isa`). `horndb-sparql`
  bridges it onto its own `DefaultGraphMode` via `From` (the dependency runs
  one way: `horndb-config` has no dependency on `horndb-sparql`).
  `QuerySettings::from_limits` picks it up.
- `AppState` gains `cfg: SparqlConfig` (built once in `serve.rs` from the
  loaded config); both query handlers use it instead of
  `SparqlConfig::default()`.
- Per-query override: the query handlers accept a `default_graph` URL/form
  parameter (`union|strict`), parsed next to the existing `query` param
  (`url_form_field`, `query.rs:75`); invalid value → 400 naming the key.
  **Amendment (post-Task-2 review):** spelled `default_graph` — the
  `QuerySettings` field name — not the originally-planned `default-graph`.
  Reason: SPEC-26 S4 spells every future override key after its field name
  (e.g. `?query_timeout=30s`), and `default-graph` sits one suffix from the
  SPARQL 1.1 Protocol's reserved `default-graph-uri`, which SPEC-28 phase 5
  (GSP) needs on this same endpoint — two near-identical names would be a
  standing support burden. This is deliberately the S4 contract for one key;
  when SPEC-26 Phase 2 ([#251](https://github.com/sunstoneinstitute/horndb/issues/251))
  builds the real whitelist, this parameter folds into it
  (leave a `// SPEC-26 S4:` comment at the parse site). SPEC-26's spec
  whitelist already names `default_graph` (SPEC-28 S3 added it). A
  form-encoded POST reads the override from the request body first, falling
  back to the URL query string if the body doesn't carry it — the same
  precedence `query=` already implies over any URL query string.

### Execution: the scoped snapshot

`HornBackend::wcoj_snapshot` (`horn.rs:430`) currently builds one memoised
`Arc<VecTripleSource>` from `scan_all_term_ids()` (default graph). It
becomes:

```rust
enum SnapshotScope {           // resolved from GraphScope + DatasetSpec
    DefaultUnion,              // union of non-reserved graphs (deduped)
    DefaultStrict,             // DEFAULT_GRAPH partitions only
    FromUnion(Vec<GraphId>),   // FROM list (deduped union; empty = empty)
    OneGraph(GraphId),         // ground GRAPH <g>
    PerGraph(Vec<GraphId>),    // GRAPH ?g: per-graph batches, ?g column
}
fn wcoj_snapshot(&self, scope: &SnapshotScope) -> Arc<VecTripleSource>
```

with the memo becoming a small `HashMap<SnapshotScopeKey, Arc<…>>` behind
the existing `Mutex` (invalidated wholesale on write, as today). Union
scopes dedup `(s,p,o)` across graphs (set semantics — the same triple in
two graphs is one row of the union default graph). `PerGraph` does **not**
build one flattened source: the scan operator iterates the graphs, runs the
per-graph source, and stitches batches with the `?g` column appended
(`Slot::Id(TermId(g.0))`) — this is the D6 "scan column, not Union" shape
at the operator level, with cost O(Σ scanned graphs) and plan size O(1).
Reserved-graph exclusion happens where the graph list is computed:
`snapshot.graphs()` filtered through `graph_uri` prefix test, cached per
store snapshot.

`Executor` trait: `scan_bgp`, `scan_bgp_ids`, `count_bgp`,
`count_bgp_grouped`, `cardinality_estimate` all gain a
`scope: &ScanScope` parameter (`ScanScope` = the plan-level
`GraphScope` + the query's `DatasetSpec`, resolved by the runtime to a
`SnapshotScope` at the backend). `MemStore` implements the same semantics
over its in-memory maps — which requires MemStore to hold quads at all:

**MemStore grows a graph dimension in this plan** (`exec/mem.rs`): its
triple set and indexes become quad-keyed
(`(GraphName-as-Term, s, p, o)` with a default-graph sentinel), plus a
test-visible `insert_quad` helper. The `exec::Store` *write trait* is
untouched (phase 4 owns it) — `insert_triple` writes the default graph.
PLAN-28-04 consumes this same MemStore work; whichever plan executes first
carries it (both plans say this — do not implement it twice).

### Pushdowns and estimates (S3's silent-wrong-answer clause)

All count shortcuts bottom out in `wcoj_snapshot` (`count_bgp` :962,
`count_bgp_grouped` :1064, `CountScanOp`/`GroupCountScanOp`
`exec/op/source.rs:62,111`), so threading the scope through the snapshot
fixes results at one seam — there is no whole-store counter left on the
*result* path. Two guards on top:

- `count_bgp` and `count_bgp_grouped` **must return `Ok(None)`** (decline)
  for any scope they have not been explicitly taught, forcing the scan
  fallback (`source.rs:76`) — decline-by-default is what makes a future
  scope addition safe.
- `cardinality_estimate` may stay coarse (whole-store `total_triples()` is
  a valid upper bound under any scope) — but it feeds plans and `EXPLAIN`
  only; assert by construction (type-level: estimates return
  `Option<usize>` into the planner, never into a `Batch`).

The **differential pushdown test** (spec risk "pushdown regressions are
silent by nature"): for every pushdown-eligible shape in the existing
`pushdown.rs` test battery (`:960`, `:1331`), run inside `GRAPH <g>` and
`GRAPH ?g` on a two-graph fixture with the pass enabled and disabled
(`PassId::CountPushdown` off via the existing pass-config mechanism) and
assert identical results.

### Property paths

`PathClosure.edge` is a sub-plan; lowering pushes the enclosing scope onto
the edge's scan nodes like any other subtree, so the closure is computed
over the scoped edge relation — scope-before-closure falls out of the
design (S3's requirement) rather than needing a special case. Add the
regression test anyway: a path `:p+` inside `GRAPH <g1>` where the chain
hops `g1 → g2 → g1` must NOT connect (post-filtering would connect it).
Under the union default graph (no `GRAPH`), the same chain **does** connect
— both asserted in one test pair. Note `runtime.rs:228`'s zero-length-path
approximation interacts here; keep its current behaviour, scoped.

### `CoalesceBgp`

The pass merges adjacent `BgpScan`s (`plan/pass.rs`). With scopes on scan
nodes it must only merge scans whose `scope` fields are equal — add the
guard and a pin test (two BGPs under different `GRAPH` wrappers must not
coalesce).

### Conformance (S7, corrected)

Verified: the harness binary's `sparql11` suite key does **not** run a real
query engine (`Reasoner::ask` is a stub — `owlrl/src/integration.rs:429`
ignores the query), and no result-set `TestKind` exists. The repo's real
query-eval gate is `selected.toml`'s `[sparql_query]` section driving
`crates/sparql/tests/w3c_suite.rs` (real engine, both backends, multiset
result diff). Therefore:

- The `graph/` and `dataset/` families land in **`[sparql_query]`**, as
  mirrored per-case fixture dirs under
  `crates/harness/tests/fixtures/sparql11/selected_subset/` (the existing
  pattern), sourced from the already-fetched W3C tarball
  (`fetch-w3c-suites.sh:35-40`) — add a mirror step to the script following
  its rdf12 allowlist pattern (`:51-74`).
- `w3c_suite.rs` learns named-graph inputs: a case dir may carry
  `data.trig` (parsed with the TriG parser, quads routed to their graphs)
  in place of `data.nt`; `run_one` loads via the backend's quad path.
  Exact case IDs are enumerated when the mirror lands; cases needing
  RDF-merge bnode renaming or other unimplemented corners go to
  `harness/KNOWN-MANIFEST-BUGS.md` (new SPARQL section) with reasons.
- **SPEC-28 S7 is amended in this plan's docs task**: its claim that these
  families "fit the existing manifest-driven runner unchanged" is wrong —
  the runner has no result-set kind; the families gate through
  `[sparql_query]` instead, which is equally CI-gating (the `tests` job).
  The `sparql11` harness-key sentence is corrected rather than silently
  ignored.

### File map

- Modify: `crates/sparql/src/algebra/mod.rs`, `algebra/translate.rs`
- Modify: `crates/sparql/src/plan/{logical.rs,mod.rs,lower.rs,pass.rs,pushdown.rs,explain.rs}`
- Modify: `crates/sparql/src/exec/{mod.rs,horn.rs,mem.rs,op/mod.rs,op/source.rs,runtime.rs}`
- Modify: `crates/sparql/src/{lib.rs,api.rs}`, `crates/sparql/src/server/{mod.rs,query.rs}`, `crates/sparql/src/bin/serve.rs`
- Modify: `crates/config/src/model.rs` (+ `crates/config` tests)
- Modify: `crates/harness/scripts/fetch-w3c-suites.sh`,
  `crates/sparql/tests/w3c_suite.rs`, `harness/selected.toml`,
  `harness/KNOWN-MANIFEST-BUGS.md`
- Create: fixture dirs under
  `crates/harness/tests/fixtures/sparql11/selected_subset/`
- Modify: `docs/specs/SPEC-28-named-graph-dataset-semantics.md` (S7
  amendment), `docs/specs/SPEC-26-config-system.md` (whitelist key),
  `docs/architecture.md`, `crates/sparql/INTEGRATION-NOTES.md`, this plan

---

### Task 1: Algebra + translation (`Algebra::Graph`, dataset capture)

**Files:**
- Modify: `crates/sparql/src/algebra/mod.rs`, `algebra/translate.rs`
- Modify: `crates/sparql/tests/exec_expressions.rs` (the phase-1 refusal
  pins), new tests in `crates/sparql/tests/algebra_translate.rs`

- [x] **Step 1: Failing tests** — in `algebra_translate.rs`:
  `graph_iri_translates_to_graph_node` (translate
  `GRAPH <g> { ?s ?p ?o }`, assert the tree is
  `Graph { name: GraphSpec::Iri(..), inner: Bgp }`),
  `graph_var_translates_and_scopes_var` (`GRAPH ?g` + `SELECT *` projects
  `?g`), `nested_graph_innermost_wins`, `from_clause_recorded`
  (`translate_query_with` returns the `DatasetSpec` — decide the return
  plumbing: a `TranslatedQuery { algebra, dataset }` struct replacing the
  bare `Algebra` return; `api.rs` callers adapt),
  `from_named_only_yields_empty_default` (D4 pin at the `DatasetSpec`
  level).
- [x] **Step 2: Verify failure** — `cargo nextest run -p horndb-sparql
  algebra_translate`.
- [x] **Step 3: Implement** — `GraphSpec` + `Algebra::Graph`; the phase-1
  error arm becomes construction; the four dataset arms build
  `DatasetSpec` (the `refuse_nonempty_dataset` helper from PLAN-28-01 is
  deleted); update the phase-1 refusal tests in `exec_expressions.rs` to
  expect success (they become evaluation tests in Task 4 — for now assert
  translation succeeds).
- [x] **Step 4: Crate suite** — `cargo nextest run -p horndb-sparql`.
  Everything except the old refusal pins passes; downstream lowering of
  `Algebra::Graph` errors with `Planner("Graph not lowered")` until Task 3
  — gate the two new end-to-end paths behind translation-level tests only
  in this task.
- [x] **Step 5: Commit** — `feat(sparql): Algebra::Graph + dataset capture
  in translation (SPEC-28 S3, #266)`.

### Task 2: Config plumbing (`default_graph` mode, server threading, URL override)

**Files:**
- Modify: `crates/config/src/model.rs`, `crates/sparql/src/lib.rs`,
  `crates/sparql/src/server/{mod.rs,query.rs}`,
  `crates/sparql/src/bin/serve.rs`
- Test: `crates/config` unit tests, `crates/sparql/tests/serve_config_wiring.rs`,
  `crates/sparql/tests/server_http.rs`

- [x] **Step 1: Failing tests** — config: `Limits` default carries
  `default_graph == "union"`, TOML/env override works (follow the crate's
  existing layering tests). Server: `default_graph_url_param_overrides`
  (POST with `default_graph=strict` flips one query's behaviour — full
  assertion lands in Task 4; here assert the 400 on
  `default_graph=bogus` naming the key), `serve_config_wiring.rs` asserts
  `AppState.cfg` reflects the loaded config.
- [x] **Step 2: Verify failure.**
- [x] **Step 3: Implement** per the design (Limits field + validation,
  `DefaultGraphMode` on `SparqlConfig`, `AppState.cfg`, handler param
  parse). `MemStore`-backed handlers thread the same config — the mode is
  interpreted by the executor, not the backend, so this is uniform.
- [x] **Step 4: Run** — `cargo nextest run -p horndb-config -p horndb-sparql
  --features server`.
- [x] **Step 5: Commit** — `feat(sparql,config): default_graph mode —
  server setting + per-query override (SPEC-28 S3/D2, #266)`.

### Task 3: Scan-scope lowering + scoped snapshots (ground `GRAPH`, modes)

**Files:**
- Modify: `crates/sparql/src/plan/{logical.rs,mod.rs,lower.rs}`,
  `crates/sparql/src/exec/{mod.rs,horn.rs,mem.rs,op/mod.rs}`
- Test: new `crates/sparql/tests/graph_query.rs`

- [x] **Step 1: Failing tests** (`graph_query.rs`, generic over both
  backends like `update_where.rs`): fixture = default graph {t1}, g1 {t2},
  g2 {t3, t2} (t2 in two graphs). Pins:
  `ground_graph_scopes_to_one_graph` (`GRAPH <g1>` → exactly t2),
  `unknown_graph_yields_zero_rows`,
  `union_mode_unqualified_sees_all_non_reserved_deduped` (t1,t2,t3 — t2
  once), `strict_mode_unqualified_sees_default_only` (t1),
  `from_builds_union` (`FROM <g1> FROM <g2>` → t2,t3 deduped),
  `from_named_only_empty_default_graph` (zero rows for an unqualified BGP),
  `reserved_graph_excluded_from_union` (insert a quad into
  `https://horndb.io/graph/x` via the storage/mem seam; unqualified query
  misses it; `GRAPH <…/x>` finds it).
- [x] **Step 2: Verify failure.**
- [x] **Step 3: Implement** — `GraphScope` on the three scan node types
  through `LogicalPlan`/`PhysicalPlan`; lowering rewrite of
  `Graph { … }` (innermost-wins); `Executor` scope parameter;
  `HornBackend::wcoj_snapshot(scope)` with the keyed memo, union dedup,
  reserved-set cache; MemStore quad storage + the same scope resolution;
  runtime `DatasetSpec` threading from `TranslatedQuery` through
  `api.rs`/`plan_select` to the operators.
- [x] **Step 4: Run** — the new suite + full crate.
- [x] **Step 5: Commit** — `feat(sparql): graph-scoped scans — ground GRAPH,
  dataset construction, union|strict default graph (SPEC-28 S3, #266)`.

### Task 4: `GRAPH ?g` — the graph column

**Files:**
- Modify: `crates/sparql/src/exec/{op/mod.rs,op/source.rs,horn.rs,mem.rs,runtime.rs}`
- Test: `crates/sparql/tests/graph_query.rs`

- [x] **Step 1: Failing tests** — `graph_var_enumerates_named_graphs_only`
  (`GRAPH ?g { ?s ?p ?o }` on the Task-3 fixture binds `?g` ∈ {g1, g2},
  never the default graph — D3, both modes), `graph_var_binds_per_row`
  (t2 appears twice: once with ?g=g1, once ?g=g2),
  `graph_var_restricted_by_from_named` (`FROM NAMED <g1>` → only g1),
  `graph_var_join_with_ground_var` (`GRAPH ?g { … } . FILTER(?g = <g1>)`
  and a shared-variable join both work),
  `select_star_projects_graph_var` (revives the old `:433` pin, now with a
  bound value), `reserved_graphs_do_not_enumerate`.
- [x] **Step 2: Verify failure.**
- [x] **Step 3: Implement** — the per-graph scan loop in the scan operator
  (`op/mod.rs:95` build path): resolve the graph list (named set ∩
  visibility ∩ non-reserved), run the per-graph source, append the `?g`
  column as `Slot::Id(TermId(g.0))`; `MemStore` mirrors with its term map.
- [x] **Step 4: Run** — full crate suite.
- [x] **Step 5: Commit** — `feat(sparql): GRAPH ?g as a scan output column
  (SPEC-28 S3/D6, #266)`.

### Task 5: Pushdown + estimator scoping, `CoalesceBgp` guard, paths

**Files:**
- Modify: `crates/sparql/src/plan/{pushdown.rs,pass.rs,explain.rs}`,
  `crates/sparql/src/exec/{horn.rs,op/source.rs}`, `runtime.rs`
- Test: `crates/sparql/src/plan/pushdown.rs` in-file battery,
  `crates/sparql/tests/graph_query.rs`, `crates/sparql/tests/logical_pipeline.rs`

- [x] **Step 1: Failing tests** — the **differential pushdown battery**
  from the design (every eligible shape × {GRAPH <g>, GRAPH ?g} ×
  {pushdown on, off} → identical results); `count_declines_unknown_scope`
  (a `count_bgp` impl receiving an untaught scope returns `Ok(None)` — pin
  via MemStore); `bgps_under_different_graphs_do_not_coalesce`;
  `path_scope_applied_before_closure` + `path_over_union_traverses_graphs`
  (the g1→g2→g1 pair from the design).
- [x] **Step 2: Verify failure.**
- [x] **Step 3: Implement** — scope on `CountScan`/`GroupCountScan` reaches
  `count_bgp`/`count_bgp_grouped` via the scoped snapshot;
  decline-by-default; `CoalesceBgp` equal-scope guard; `explain.rs`
  estimates labelled as estimates (no change to result paths — verify by
  reading, note in the pass doc).
- [x] **Step 4: Run** — full crate suite.
- [x] **Step 5: Commit** — `feat(sparql): scope-aware pushdowns
  (decline-by-default), CoalesceBgp scope guard, path scoping (SPEC-28 S3,
  #266)`.

### Task 6: W3C `graph/` + `dataset/` families

**Files:**
- Modify: `crates/harness/scripts/fetch-w3c-suites.sh`,
  `crates/sparql/tests/w3c_suite.rs`, `harness/selected.toml`,
  `harness/KNOWN-MANIFEST-BUGS.md`
- Create: fixture dirs under
  `crates/harness/tests/fixtures/sparql11/selected_subset/`

- [x] **Step 1:** Run `fetch-w3c-suites.sh`; enumerate the `graph/` and
  `dataset/` manifests' evaluation cases; mirror each runnable case into a
  fixture dir (`data.trig`/`data.nt` + `query.rq` + expected results,
  matching the existing dir shape); extend the script with the mirror
  allowlist (rdf12 pattern, `fetch-w3c-suites.sh:51-74`).
- [x] **Step 2:** Extend `w3c_suite.rs::run_one` to load `data.trig` via a
  quad path on both backends; add the mirrored case names to
  `[sparql_query]` in `harness/selected.toml`. Cases that cannot pass get a
  `KNOWN-MANIFEST-BUGS.md` SPARQL section entry with the gating reason
  (expected: RDF-merge bnode renaming; enumerate exactly).
- [x] **Step 3:** `cargo nextest run -p horndb-sparql w3c_suite` — green.
- [x] **Step 4: Commit** — `test(sparql): W3C graph/ + dataset/ families in
  the selected subset (SPEC-28 S7, #266)`.

### Task 7: Docs + spec amendments

**Files:**
- Modify: `docs/specs/SPEC-28-named-graph-dataset-semantics.md`,
  `docs/specs/SPEC-26-config-system.md`, `docs/architecture.md`,
  `crates/sparql/INTEGRATION-NOTES.md`, this plan

- [x] **Step 1:** SPEC-28 S7 amendment per the design ("fit the existing
  manifest-driven runner unchanged" → the `[sparql_query]` route, with the
  reason). SPEC-26: confirm `default_graph` is on the S4 whitelist list
  (add if the earlier spec edit missed it). `docs/architecture.md`: the
  `GRAPH` row flips **refused** → **implemented (SPEC-28 phase 3)**; the
  dataset-clause line likewise. `INTEGRATION-NOTES.md`: the refusal
  section becomes a description of the semantics + the mode.
- [x] **Step 2:** Full verification — `cargo fmt --all`, `cargo clippy
  --workspace --all-targets -- -D warnings`, `cargo nextest run
  --workspace`, plus `cargo nextest run -p horndb-sparql --features
  server`.
- [x] **Step 3: Commit** — `docs(sparql): SPEC-28 S3 sync — S7 amendment,
  architecture, notes (#266)`.

---

## Self-review notes

- S3 bullet coverage: `Algebra::Graph` → T1; ground form → T3; variable
  form/D6 → T4; dataset construction incl. D4's empty default → T1+T3;
  no-dataset default + mode + SPEC-26 override → T2+T3; no-dataset named
  set + reserved exclusion → T3+T4; paths → T5; pushdowns → T5; the
  reasoning seam — this plan keeps every scope base-only (no derived
  quads exist yet; SPEC-29 P1 defines the derived side and needs no seam
  change here beyond what `SnapshotScope` already expresses).
- Estimator risk (spec: "expect estimator work in phase 3"): handled as
  decline-by-default + coarse-upper-bound estimates; no estimate can reach
  a result path by construction (types).
- Cross-plan handshake: MemStore quad storage is shared with PLAN-28-04
  (stated in both; implement once).
