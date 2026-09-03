---
status: executed
date: 2026-07-29
scope: "SPEC-29 P1 — the reasoning materializer slice: declared views over a once-closed spine, per-view inferred graphs diffed idempotently through the store boundary, dirty-marking from the update path with background re-derivation, the [reasoning] config section, and the D5/D6 visibility invariants"
---

# SPEC-29 P1 — Reasoning materializer slice

> **Executed.** T1–T6 landed with HDB-72
> ([#269](https://github.com/sunstoneinstitute/horndb/issues/269)):
> `crates/sparql/src/reasoning/` (view model, catalog, per-view derivation, D7
> routing), `Engine::load_base`/`fork`/`extend` in
> `crates/owlrl/src/integration.rs`, the `[reasoning]` config section, the
> `reasoning_*` metrics, and the `serve` wiring. T7 closed with HDB-144: the
> `view_derivation` bench was written (it had not been) and run on `hornbench`,
> and `docs/benchmarks.md`'s three SPEC-29 rows now carry numbers — single-view
> re-derivation **5.29 ms**, 19× inside the inherited 100 ms budget; fan-out
> linear at **0.55 ms/view** over 250 and 1,000 views; **25.5 KiB/view** resident
> with every view clean.
>
> Two deviations from the plan as written: routing lives in the catalog
> (`catalog.rs::route`) rather than a separate `router.rs` — the file was not
> worth its own module; and T7's open harness question resolved to *both*
> — criterion for the two short measurements, plain one-shot timing for the
> whole-corpus re-derive and the resident-memory gauge, which criterion cannot
> express.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A store of named graphs reasons per declared view (shared
vocabulary spine + one data graph), derived triples land in per-view
inferred graphs under the reserved namespace, a source-graph read never
returns a derived quad, and a single-graph update re-derives exactly one
view. Tracking issue:
[#269](https://github.com/sunstoneinstitute/horndb/issues/269). Spec:
`docs/specs/SPEC-29-named-graph-reasoning-scope.md` (P1 slice).
**Depends on** SPEC-28 phase 4 (`apply_quads`, PLAN-28-04) for every write
this plan makes, phase 3 (PLAN-28-03) for reading inferred graphs by name
and for the D6 flag's dataset composition, and SPEC-30 P1 (PLAN-30-01) as
the recovery story it leans on.

**Architecture — batch engines, not circuits.** Verified: the incremental
`Circuit` is wired to nothing (`load_with_reasoning` is the only reasoning
entry; SPEC-24 S4 #213 is unbuilt, gated on #212) and is neither `Send` nor
`Sync` (`ClosureRule` lacks bounds; `RefCell` in `version_cache`) — putting
5,000 of them in an axum server is P2 work at the earliest. SPEC-29's P1
text requires re-derivation, not incremental maintenance ("spine changes
mark every dependent view stale and re-derive it"), so P1 builds the view
machinery on the **existing batch `Engine`**: the spine closes once into a
reusable template engine; a view derivation forks the template, extends it
with the data graph, and diffs the result into the view's inferred graph
through the idempotent store boundary. D7's *routing* (a quad delta touches
exactly its graph's view) is implemented as dirty-marking on the update
path; D7's per-view circuits arrive in P2 by swapping the re-derive step,
with the view model, catalog, routing, and invariants all unchanged. This
also sidesteps the per-view resident-state hazard entirely: P1's resident
per-view state is the inferred graph itself plus a catalog entry — the
spine lives in **one** template engine.

**Tech Stack:** Rust 1.90; `crates/owlrl` (small API additions),
`crates/sparql` (new `reasoning` module behind the `reasoner` feature),
`crates/config`, `crates/metrics`.

---

## Design (read this before any task)

### owlrl additions: `close`, `fork`, `extend`

`Engine` today is one-shot: `load(&Dataset)` rebuilds `LoadState` from
scratch and skips every non-default-graph quad
(`integration.rs:189-203`). P1 needs:

```rust
impl Engine {
    /// Load + materialize, keeping state reusable (today's load, renamed intent).
    pub fn load_base(&mut self, triples: impl IntoIterator<Item = (String, String, String)>) -> Result<()>;
    /// Clone the materialized state (LoadState: dict + MemStore + counters — all cloneable maps).
    pub fn fork(&self) -> Engine;
    /// Assert more triples and re-run rules to fixpoint (semi-naive from the delta).
    pub fn extend(&mut self, triples: impl IntoIterator<Item = (String, String, String)>) -> Result<()>;
}
```

Feeding lexical triples (not `oxrdf::Dataset`) removes the graph-skip
question from the engine entirely: **callers choose the scope**, the engine
never sees a graph — which is the D1 "scope is a declared view, not an
accident of loading" boundary drawn in code. The existing
`load(&Dataset)` stays for the harness/`--materialize` path.

The load-bearing correctness test is the D3 identity run through this API:
`fork(load_base(S)).extend(D)` must equal `load_base(S ∪ D)` as
materialized sets, over the `harness/curation/owl2-rl-50.md` rule shapes
**including** `owl:sameAs` fixtures (D3 condition 1: full sameAs
materialization — if this differential ever fails on a sameAs shape, stop
and re-read SPEC-29 D3 before "fixing" anything) and an `owl:Nothing`
fixture (condition 2: inconsistency propagates as derivation;
`is_consistent()` on the forked engine reports it per view). This is
spec acceptance 3, and it gates everything below.

### The view model (in `crates/sparql/src/reasoning/`)

```rust
pub struct ViewCatalog {                    // rebuilt at startup, maintained on writes
    spine_graphs: Vec<GraphId>,             // from [reasoning].spine patterns
    spine_version: u64,                     // bumped on any spine-graph write
    views: HashMap<GraphId, ViewState>,     // source graph → state
}
pub struct ViewState {
    inferred_graph: GraphId,                // minted, D4
    derived_at_spine_version: u64,
    dirty: bool,
    consistent: bool,                       // D3 condition 2's per-view flag
}
```

- **Membership** (D1/D2 default template): every visible graph that is not
  a spine graph, not reserved, and not the default graph gets a view; the
  default graph gets one iff it is non-empty (the degenerate case below).
  `views.select` patterns narrow it.
- **Minting** (D4):
  `https://horndb.io/graph/inferred/<percent-encode(source-IRI)>` —
  percent-encode everything outside RFC 3986 unreserved, one exact
  implementation with round-trip tests; collisions are impossible because
  decode is exact.
- **The catalog is also quads** in `https://horndb.io/graph/views` (source,
  inferred-graph IRI, spine version derived against, derived-at store
  version, consistency flag; plus one node carrying the current spine
  version) — written through `apply_quads` after each derivation, so an
  operator reads staleness with a query. The in-memory struct is the
  worker's working state; the quads are its exhaust. On startup the
  in-memory catalog is rebuilt from config + `graphs()` and **every view
  starts dirty** — re-derivation is idempotent diffing, so a restart
  converges without any recovered state (this is the SPEC-30
  rebuild-from-feed posture applied to derived state).

### Deriving one view

1. Template: `spine_engine` = `load_base(spine asserted ∪ nothing else)`,
   computed once per spine version; its derived-beyond-asserted set is
   diffed into `https://horndb.io/graph/spine-closure` (shared, D3).
2. `view_engine = spine_engine.fork(); view_engine.extend(scan_graph(source))`.
3. `view_inferred = view_engine.materialized() − spine_engine.materialized() − source asserted` —
   exactly the triples the view derives beyond the spine closure (D3's
   storage rule).
4. Diff into the inferred graph: `dels = current − view_inferred`,
   `adds = view_inferred − current`, one `apply_quads` batch. Idempotent by
   the store boundary; an empty diff writes nothing.
5. Catalog update (+ `consistent = view_engine.is_consistent()`).

Derivation-count metric increments per view derived — spec acceptance 6
reads it to prove one-graph-updates-one-view.

### Routing and the worker

- The update path (PLAN-28-04's `update.rs`) reports the set of graphs each
  request touched. A `ViewRouter` hook (a callback on `AppState`, no-op
  when reasoning is off) marks: touched data graph → its view dirty;
  touched spine graph → spine version bump + **all** views dirty (P1's
  honest cost; P2 makes it incremental); touched reserved graph → nothing
  (writes there are ours).
- A background worker (one `std::thread` owned by serve, receiving
  dirty-notifications over a channel) drains dirty views one at a time —
  derivations serialize in P1 (`fanout.*` keys deliberately do not land,
  per D9's table). Tests drive the same code path synchronously via a
  `ViewManager::run_until_clean(&mut backend)` entry so nothing depends on
  thread timing.
- `reasoning.enabled = false` (default): no router, no worker, no
  template — behaviour is byte-identical to today (acceptance 10's first
  half; pin with a test asserting zero reserved graphs appear).

### Visibility invariants

- **D5 holds by construction** — derivations only ever write minted
  reserved graphs; nothing writes a source graph. Test it anyway
  (acceptance 4): write graph G through the update path, let views
  converge, read G back (`scan_graph` and, via PLAN-28-03, `GRAPH <G>`) —
  exactly the quads written, then a second write/read round trip produces
  an empty diff.
- **D6:** `reasoning.default_dataset_includes_inferred` (hot-reloadable
  key; P1 reads it per query from `AppState`) — when set, the default
  dataset's union and `GRAPH ?g` enumeration add **exactly** the per-view
  inferred graphs and the spine-closure graph (by IRI list from the
  catalog — not the whole reserved prefix: the views catalog and the feed
  graph stay out; acceptance 5's "and nothing else"). Implemented as a
  small extension to PLAN-28-03's reserved-exclusion seam in the snapshot
  scope composition.

### The degenerate case and `--materialize`

On a store with no named graphs and reasoning enabled: one view over the
default graph, inferred graph
`https://horndb.io/graph/inferred/default` (sentinel has no IRI; the
constant segment `default` is reserved for it). Acceptance 10: its
`spine-closure ∪ view-inferred ∪ asserted` equals today's
`load_with_reasoning` materialized set, as sets. The `--materialize` CLI
path itself is untouched this slice (it remains the legacy
flatten-into-default-graph loader; SPEC-28 S5's `?default` GSP restriction
keys off it) — migrating it onto views is a follow-up noted in the spec's
P1, not silently done here.

### Config (`[reasoning]`, D9)

`crates/config/src/model.rs`: `Reasoning` section struct per the spec's
table minus the `fanout.*` keys (P2): `enabled` (false), `spine`
(Vec<String>, IRI-prefix patterns), `views.select`
(`"all-except-spine"` | pattern list), `views.include_spine` (true),
`views.output` (`"graph"`; `"none"` is a startup error naming P4),
`default_dataset_includes_inferred` (false). Domain validation follows the
`serve.rs:114-122` pattern (config crate stays serde-only): spine ∩
views.select overlap → fatal naming both keys; any pattern matching
`https://horndb.io/graph/` → fatal; enabled with empty spine → the
option-(b) warning log. None of these keys joins any per-query override
surface (D9 — assert in the same place PLAN-28-03's `default_graph` param
is parsed: unknown keys there stay 400).

### File map

- Modify: `crates/owlrl/src/integration.rs` (+ store.rs `Clone` derives as
  needed)
- Create: `crates/sparql/src/reasoning/{mod.rs,catalog.rs,derive.rs,router.rs}`
  (feature `reasoner`)
- Modify: `crates/sparql/src/{update.rs,bin/serve.rs,server/mod.rs,lib.rs}`,
  the PLAN-28-03 scope-composition seam in `exec/horn.rs`
- Modify: `crates/config/src/model.rs`
- Create: `crates/metrics/src/reasoning.rs` (derivations counter, dirty
  gauge, spine version gauge, per-derivation duration histogram) +
  `docs/metrics.md` rows
- Create: `crates/sparql/tests/reasoning_views.rs`,
  `crates/owlrl/tests/spine_factoring.rs`
- Modify: `docs/architecture.md`, `crates/owlrl/AGENTS.md` (§7 caveat
  becomes a description of the view boundary), `docs/benchmarks.md`, this
  plan

---

### Task 1: `Engine::load_base` / `fork` / `extend` + the D3 differential

**Files:**
- Modify: `crates/owlrl/src/integration.rs` (+ `Clone` on `LoadState`/`MemStore`)
- Create: `crates/owlrl/tests/spine_factoring.rs`

- [ ] **Step 1: Failing tests** — `fork_extend_equals_joint_load`
  (property-style over the curated rule-shape fixtures: random split of a
  fixture into S/D; `fork(load_base(S)).extend(D)` vs `load_base(S∪D)` —
  materialized-set equality), `sameas_across_the_split` (an `owl:sameAs`
  fixture split so the pair spans S and D), `nothing_propagates_per_fork`
  (S consistent, D inconsistent → fork reports inconsistent, template
  stays consistent), `extend_is_idempotent` (extend with already-present
  triples derives nothing new).
- [ ] **Step 2: Verify failure** — `cargo nextest run -p horndb-owlrl
  spine_factoring` (compile error: methods undefined).
- [ ] **Step 3: Implement** — per the design; `extend` re-enters the
  generated-rule fixpoint seeded from the newly asserted delta (the
  semi-naive machinery exists — see `semi_naive.rs`; if the entry point
  only supports full runs, a full re-run over the forked store is
  *correct* and acceptable for P1 — note which was done, the bench in
  Task 7 decides if it matters).
- [ ] **Step 4: Run** — `cargo nextest run -p horndb-owlrl`.
- [ ] **Step 5: Commit** — `feat(owlrl): Engine fork/extend with
  spine-factoring differential (SPEC-29 D3, #269)`.

### Task 2: `[reasoning]` config

**Files:**
- Modify: `crates/config/src/model.rs`, `crates/sparql/src/bin/serve.rs`
- Test: `crates/config` unit tests, `crates/sparql/tests/serve_config_wiring.rs`

- [ ] **Step 1: Failing tests** — defaults (`enabled == false`, etc.),
  layering (file < env `HORNDB_REASONING__ENABLED` < argv if a flag is
  added — no new CLI flag this slice), and the three validation cases
  (overlap fatal naming both keys; reserved-pattern fatal; empty-spine
  warning — assert via the validation function's return, not log
  scraping).
- [ ] **Step 2: Verify failure; implement** (section struct + a
  `validate_reasoning(&Reasoning) -> Result<Vec<Warning>, Error>` called
  from serve startup).
- [ ] **Step 3: Run** — `cargo nextest run -p horndb-config -p
  horndb-sparql`.
- [ ] **Step 4: Commit** — `feat(config): [reasoning] section + startup
  validation (SPEC-29 D9, #269)`.

### Task 3: View catalog, minting, spine closure

**Files:**
- Create: `crates/sparql/src/reasoning/{mod.rs,catalog.rs,derive.rs}`
- Test: `crates/sparql/tests/reasoning_views.rs`

- [ ] **Step 1: Failing tests** — `minting_roundtrips_and_is_injective`
  (nasty source IRIs: unicode, `%`, `/`, `#`),
  `catalog_covers_non_spine_non_reserved_graphs`,
  `spine_closure_graph_holds_derived_beyond_asserted` (fixture spine with
  a known closure; the spine-closure graph gets exactly
  closure − asserted), `catalog_quads_readable` (the views graph carries
  the expected nodes after a derivation pass).
- [ ] **Step 2: Verify failure; implement** — `ViewCatalog` build from
  config + `graphs()`; spine template engine; spine-closure diffing;
  catalog quad emission through `apply_quads`.
- [ ] **Step 3: Run; Commit** — `feat(sparql): reasoning view catalog +
  spine closure graph (SPEC-29 D1/D2/D3/D4, #269)`.

### Task 4: Per-view derivation, routing, worker

**Files:**
- Create: `crates/sparql/src/reasoning/router.rs`
- Modify: `crates/sparql/src/{update.rs,bin/serve.rs,server/mod.rs}`
- Test: `crates/sparql/tests/reasoning_views.rs`

- [ ] **Step 1: Failing tests** (all through
  `ViewManager::run_until_clean` — no thread timing):
  `view_derives_spine_x_data` (acceptance 1's shape: for fixture S, G —
  `S ∪ G ∪ spine-closure ∪ view-inferred == load_base(S ∪ G)`
  materialized set, per graph over a 3-graph fixture),
  `isolation_two_graphs_no_cross_entailment` (acceptance 2: an
  `owl:sameAs` in G1 naming a G2 subject derives nothing in either view
  that depends on the other's data),
  `single_graph_update_derives_one_view` (acceptance 6: write G1; the
  derivation counter shows exactly one derivation; re-applying the
  identical batch derives zero and diffs empty),
  `source_graph_read_returns_exactly_what_was_written` (acceptance 4 /
  D5: write-read-write round trip, empty second diff),
  `spine_edit_marks_all_dirty_and_converges` (acceptance 8 P1 half:
  retract one `rdfs:subClassOf` from the spine; dependent derived
  triples disappear from every view; converges; then simulate restart —
  rebuild catalog, all dirty, run to clean — same state),
  `inconsistent_view_flagged_not_fatal`,
  `disabled_means_no_reserved_graphs` (acceptance 10 first half).
- [ ] **Step 2: Verify failure; implement** — derivation per the 5-step
  design; the touched-graphs report from `update.rs`; the router +
  worker + `run_until_clean`; metrics emission
  (`crates/metrics/src/reasoning.rs` + `docs/metrics.md` rows in the
  same commit).
- [ ] **Step 3: Run** — `cargo nextest run -p horndb-sparql --features
  "server reasoner"`.
- [ ] **Step 4: Commit** — `feat(sparql): per-view derivation with
  dirty-routing and background worker (SPEC-29 D5/D7-routing, #269)`.

### Task 5: D6 — `default_dataset_includes_inferred`

**Files:**
- Modify: `crates/sparql/src/exec/horn.rs` (the PLAN-28-03 scope seam),
  `crates/sparql/src/lib.rs`, server threading
- Test: `crates/sparql/tests/reasoning_views.rs`

- [ ] **Step 1: Failing tests** — acceptance 5 verbatim: flag off →
  unqualified `SELECT` equals reasoning-disabled results and `GRAPH ?g`
  binds no reserved graph while ground `GRAPH <inferred-g>` answers; flag
  on → the union and enumeration gain exactly the inferred graphs + the
  spine-closure graph and nothing else (views catalog and feed graph
  stay invisible to `?g`).
- [ ] **Step 2: Verify failure; implement** (catalog-supplied IRI list
  into the snapshot scope composition; flag read per query).
- [ ] **Step 3: Run; Commit** — `feat(sparql):
  default_dataset_includes_inferred (SPEC-29 D6, #269)`.

### Task 6: Degenerate case

**Files:**
- Test: `crates/sparql/tests/reasoning_views.rs`

- [ ] **Step 1:** `degenerate_default_graph_view_matches_legacy`:
  no-named-graph corpus; enabled reasoning; assert
  asserted ∪ spine-closure ∪ inferred == `load_with_reasoning`'s
  materialized set (acceptance 10 second half). Also
  `harness selected subset stays green`: `cargo nextest run --workspace`
  with no selection change — reasoning defaults off, so this is the
  no-regression pin.
- [ ] **Step 2: Commit** — `test(sparql): degenerate single-view parity
  with the legacy materialize path (SPEC-29 acceptance 10, #269)`.

### Task 7: Bench + docs

**Files:**
- Create: `crates/sparql/benches/view_derivation.rs` — created under HDB-144; `required-features = ["reasoner"]`
- Modify: `docs/benchmarks.md`, `docs/architecture.md`,
  `crates/owlrl/AGENTS.md`, this plan

- [x] **Step 1:** Bench on the PLAN-28-02 synthetic corpus grown with a
  vocabulary spine (the spec says the two phases share one corpus):
  measure (a) spine template build, (b) single-view derivation
  end-to-end (fork + extend + diff) on a small graph — the SPEC-06 NF1
  100 ms check, (c) full cold re-derive of all views, (d) resident
  memory with all views clean. Run on hornbench; record in
  `docs/benchmarks.md` with host + commit. If (b) busts 100 ms, the
  finding goes to #269 (candidates: cheaper fork via persistent
  data structures, or pulling P2's incremental path forward) —
  measured first, not guessed.
- [x] **Step 2:** Docs — `docs/architecture.md`: SPEC-29 row → P1
  implemented (P2 fan-out, P3 provenance, P4 virtual outstanding);
  `crates/owlrl/AGENTS.md` §7: the "silently drops named graphs" caveat
  is rewritten — the engine is scope-blind *by contract* now, callers
  own scope via `load_base`/`fork`/`extend`. Flip this plan's status.
- [x] **Step 3:** Full verification — fmt, clippy `-D warnings`,
  `cargo nextest run --workspace`, plus the `reasoner`+`server` feature
  matrix.
- [x] **Step 4: Commit** — `bench,docs(sparql): view derivation numbers +
  SPEC-29 P1 sync (#269)`.

---

## Self-review notes

- P1 scope check against the spec's slice list: view model + catalog
  (T3), spine factoring (T1/T3), per-view inferred graphs (T4), D5 (T4),
  quad input deltas + idempotent apply + per-view routing (PLAN-28-04's
  boundary + T4's router), `[reasoning]` config (T2), disabled no-op
  (T4), spine-change re-derive, resumable (T4). Explicitly deferred, per
  spec: incremental fan-out + `fanout.*` keys (P2), provenance (P3),
  virtual views / `output = "none"` (P4), `--materialize` migration
  (noted, not smuggled in).
- The circuits-vs-batch decision is the plan's one deliberate
  architecture call beyond the spec's letter; it is grounded in verified
  code facts (nothing wires circuits today; `Circuit` is
  `!Send + !Sync`), satisfies the P1 text, and leaves D7's circuit shape
  to P2 where the spec puts the incremental requirement. If a reviewer
  reads D7 as mandating circuits *in P1*, resolve against the Phasing
  section, which says P1 has "no incremental fan-out yet".
- Acceptance criteria this plan does NOT close: 7 (the 5,000-view ceiling
  measurement — T7 measures the realistic corpus; the ceiling run is P2's
  budget-setting input), 8's P2 half, 3's closure-backend variants beyond
  the curated shapes.
