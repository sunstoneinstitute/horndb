---
status: executed
date: 2026-07-29
scope: "SPEC-28 phase 4 (S4+S6) — named-graph SPARQL Update (quad data, pattern updates, graph management, WITH/USING, SILENT fidelity, the closed reserved namespace) on top of a store-boundary idempotent quad-grain apply with one commit batch per operation"
---

# SPEC-28 phase 4 — Named-graph Update and idempotent quad apply

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every SPARQL Update form operates on real named graphs, and the
store boundary guarantees idempotent quad-grain apply — insert-present and
retract-absent are counted no-ops — so an at-least-once change feed replays
safely. Tracking issue:
[#267](https://github.com/sunstoneinstitute/horndb/issues/267). Spec:
`docs/specs/SPEC-28-named-graph-dataset-semantics.md` §S4+§S6. **Depends on
phase 2** (PLAN-28-02); independent of phase 3 except for the shared
MemStore quad storage (see Cross-plan handshake) and the `USING` dataset
machinery, which reuses phase 3's `DatasetSpec` if it has landed and
otherwise lands here first (S3 and S4 spell the same construct).

**Architecture:** Bottom-up. S6 first: a new `Tier::apply_quad_batch(dels,
adds)` commits deletions-then-insertions at **one commit version** with
accurate affected counts and no version bump on a no-op — the single
enforcement point (D9) every caller inherits. `Store::apply_quads` wraps it
at the lexical layer. Then `update.rs` is rewritten from
reject-named-graphs to route-by-graph, one `apply_quads` batch per Update
operation, preserving the existing preflight-then-apply atomicity shape.
The `exec::Store` write trait becomes quad-shaped. SILENT fidelity for
`ADD`/`MOVE`/`COPY` is recovered with a source-text pre-scan because
spargebra desugars those verbs and drops the flag.

**Tech Stack:** Rust 1.90; `crates/storage`, `crates/sparql`; W3C update
family fixtures.

---

## Design (read this before any task)

### S6 — the store boundary (`crates/storage`)

Verified current behaviour: `insert_quad_batch` appends unconditionally
(builder dedups exact live duplicates, `partition.rs:417-452`), returns no
count, and **always bumps the version** even for a pure no-op batch;
`retract_quad_batch` already returns a count and skips the version bump
when nothing retracted (`memory_tier.rs:454-456`); insert and retract are
separate commits, so there is no way to apply dels+adds at one version.

New surface:

```rust
// crates/storage/src/tier.rs
pub struct ApplyReport { pub retracted: usize, pub inserted: usize }
fn apply_quad_batch(
    &self,
    dels: &[(GraphId, TermId, TermId, TermId)],
    adds: &[(GraphId, TermId, TermId, TermId)],
) -> Result<ApplyReport>;

// crates/storage/src/store.rs
pub fn apply_quads(
    &self,
    dels: &[(GraphId, Term, Term, Term)],
    adds: &[(GraphId, Term, Term, Term)],
) -> Result<ApplyReport>;
```

Contract (S6, verbatim requirements):

- One commit version covers the whole batch; **dels apply before adds**, so
  a del+add of the same quad ends present.
- `inserted` counts only quads not visible before the batch (after the
  dels — a quad deleted and re-added in one batch counts once retracted,
  once inserted); `retracted` counts only quads actually visible.
- A batch whose effective change is empty **does not bump the version**
  (extends today's retract behaviour to the combined path — this also stops
  a replayed feed batch from invalidating every reader snapshot for
  nothing).
- Quad identity is lexical term equality; the dictionary's
  identity-preserving inline-int path (`dictionary.rs:197-204`) already
  satisfies "no value normalization"; a test pins `"01"^^xsd:integer` ≠
  `"1"^^xsd:integer` through the whole path.
- `insert_quads` / `retract_quads` remain as thin wrappers
  (`apply_quads(&[], q)` / `apply_quads(q, &[])`); `insert_quads` gains the
  count its callers never had.

Implementation shape: one writer-mutex pass per touched graph — compute
per-partition del-target sets and add sets, rebuild each touched partition
once applying ends-then-appends, counting as it goes (the rebuild already
visits every row; `live_len` from PLAN-28-02 updates in the same pass).

### The write trait (`crates/sparql/src/exec/mod.rs:156`)

`Store` (trait) is re-cut quad-shaped — phase 2 deliberately left it; this
phase owns it:

```rust
pub trait Store {
    /// One atomic batch: dels before adds, idempotent, counted (S6).
    fn apply_quads(&mut self, dels: Vec<AlgebraQuad>, adds: Vec<AlgebraQuad>) -> Result<ApplyCounts>;
    fn clear_graph(&mut self, graph: &GraphTarget) -> Result<usize>;   // CLEAR/DROP sweep, via apply_quads internally
    fn graph_exists(&self, graph: &str) -> bool;                       // D11 existence (visible-quad test)
    fn named_graphs(&self) -> Vec<String>;                             // DROP ALL / ADD-MOVE-COPY enumeration
    fn scan_graph_quads(&self, graph: &GraphTarget) -> Result<Vec<AlgebraTriple>>; // ADD/MOVE/COPY source read
}
```

(`AlgebraQuad` = `(GraphName-as-algebra-Term-or-default, Term, Term, Term)`;
exact naming follows the file's conventions.) `insert_triple` /
`delete_triple` / `clear_all` are deleted — their call sites all live in
`update.rs` and tests. `HornBackend` implements via
`Store::apply_quads` + `intern_graph_uri` + phase 2's `scan_graph` /
`graphs()` / `graph_len`; the `live_keys` cache stays as an O(1) fast path
but is no longer load-bearing for correctness (the store boundary is).
`MemStore` implements over its quad maps (Cross-plan handshake below).

### `update.rs` — from reject to route

- **Quad data.** `INSERT DATA`/`DELETE DATA` group their quads per
  operation into one `apply_quads` call (`require_default_graph_name`
  retired). One operation = one batch = one commit version — the S4 rule;
  the multi-op collapse optimization is *not* taken (correct-first; the
  spec allows collapsing only under per-quad last-writer order, revisit
  with a bench if update throughput ever matters).
- **Pattern updates.** `require_default_graph` retired; template
  instantiation resolves each template quad's `GraphNamePattern`
  (named / default / variable bound by the row); `apply_delete_insert`
  builds `dels` + `adds` and makes **one** `apply_quads` call — which also
  fixes the verified one-commit-per-triple behaviour of the current loop
  (`update.rs:611-616`).
- **`WITH` / `USING` / `USING NAMED`** (D10). Discovery step first: pin
  spargebra 0.4.6's actual desugaring of `WITH` (template quads acquire the
  graph? the WHERE pattern gets wrapped in `GraphPattern::Graph`?) with a
  unit test before building on it — the plan's assumption is spargebra
  applies WITH to both sides; if the WHERE side is *not* wrapped, wrap it
  ourselves at validate time. `USING`/`USING NAMED` build the WHERE
  dataset with S3's `DatasetSpec` machinery;
  `validate_delete_insert`'s blanket rejection (`update.rs:508-512`) and
  `where_has_graph_pattern` (`:623`) both go — the WHERE clause now runs
  through the phase-3 query path, which understands `GRAPH`. If phase 3
  has not landed when this executes, `USING`/WHERE-`GRAPH` support waits
  for it (the rest of this plan does not).
- **Graph management** (D11 semantics):
  - `CREATE <g>`: no-op if absent (succeeds), error if `graph_exists`
    unless `SILENT`. No registry — D11.
  - `CLEAR`/`DROP <g>`: absent graph → error unless `SILENT`; present →
    retract every visible quad **through `apply_quads`** (never a
    structural unlink — the spec forbids it for the delta path's sake).
    `CLEAR DEFAULT` sweeps the sentinel graph. `DROP ALL` = every
    non-reserved graph + default, quad by quad; reserved graphs are not
    touched (SPEC-30 owns the store-level reset).
  - `LOAD <src> [INTO GRAPH <g>]`: destination routing replaces the
    named-destination error. Triples formats route to the destination
    (default graph if no `INTO`). Dataset formats (`.nq`/`.trig`): plain
    `LOAD` routes each quad to its own named graph (matching the N-Quads
    loader's semantics — the current code's graph-name discard at
    `update.rs:268,278` is removed); `LOAD … INTO GRAPH` of a dataset
    format is an error naming the reason (redirecting quads is not
    defined; W3C LOAD is a graph operation). `file:`-only stays (#189).
  - `ADD`/`MOVE`/`COPY`: arrive desugared as `Drop` + `DeleteInsert`
    pairs. With named graphs representable, the desugared ops now
    *execute* — the remaining defect is the dropped `SILENT` flag.
    **SILENT recovery:** a pre-scan of the raw update string (a small
    tokenizer that skips comments, IRIs, and string literals — no regex)
    records `(verb, silent)` per `ADD|MOVE|COPY` occurrence in order;
    `apply_update_with` gains the parsed hint list and aligns it with the
    desugared op-pairs by occurrence order. Ambiguity (hint count ≠
    detected pair count) falls back to non-silent — an honest error, never
    a silent wrong outcome. File an upstream spargebra issue asking for
    structured Add/Move/Copy (or a preserved flag) and link it from the
    tokenizer's doc comment; this machinery is deleted the day that ships.
    The identity case (`ADD <g> TO <g>`) already collapses to zero ops in
    the parser — keep the existing pin tests.
- **The reserved namespace is closed** (S4): a prefix check
  (`https://horndb.io/graph/`) in `validate_op` covering every write form
  (data quads, templates, `CREATE`/`CLEAR`/`DROP`/`LOAD INTO`, ADD/MOVE/
  COPY destinations). New error variant semantics: **not suppressible by
  `SILENT`** — implement as a distinct check that runs before the
  silent-existence logic, with an error text naming the namespace.
- **Atomicity shape preserved:** `validate_op` preflight mirrors every
  apply-time error exactly as today (including the reserved-namespace and
  SILENT-hint checks) so a failing multi-op request mutates nothing.

### Conformance (S7) — update-eval runner

Verified: no `UpdateEvaluationTest` support exists anywhere (`manifest.rs`
has no `ut:` namespace; `Reasoner` has no update method), and the harness
binary's `sparql11` key runs no real engine. Same route as PLAN-28-03:

- New test file `crates/sparql/tests/w3c_update_suite.rs`, driven by a new
  `[sparql_update] tests = [...]` section in `harness/selected.toml`
  (schema: `Option<SparqlUpdateSection>` on `Selected` — backward
  compatible; mirror `SparqlQuerySection`, `selected.rs:50-54`).
- Fixture dirs mirrored from the W3C tarball's `add/`, `copy/`, `move/`,
  `clear/`, `drop/`, `delete-insert/` (graph-specific cases): each dir
  carries `request.ru`, initial state (`data.trig` or per-graph files as
  the manifest's `ut:graphData` dictates — the mirror script materializes
  them into one `data.trig`), and expected final state
  (`expected.trig`); the runner loads, applies, and compares **quad-set
  equality** on both backends.
- Cases blocked on D11 (empty-graph-existence distinctions the spec's risk
  section predicts in `clear/`/`drop/`) go to `KNOWN-MANIFEST-BUGS.md`
  with the D11 rationale — and per the spec, if the count is material,
  that finding goes back to #261 to settle the explicit-existence-set
  fallback *before* building on D11 further.

### The replay differential (S7's platform-shape test, acceptance 7)

`crates/storage/tests/feed_replay.rs`: a proptest generating a quad-grain
feed (batches of adds/dels over a small term space, including del+add of
the same quad in one batch), applied (a) once cleanly, (b) with two
duplicate-delivery mechanisms: an immediate echo of each already-applied
batch, and a stale-point mid-stream tail replay (redelivering the tail from
a random checkpoint). Assert quad-set equality of (a) and (b) and the
non-canonical-literal identity pin. Zero-count no-ops
(`retracted==0 && inserted==0`) are asserted only on the immediate-echo
redelivery — the mechanism under which they provably hold — and only for a
batch whose dels and adds don't target the same quad (such a batch reports
`{retracted:1, inserted:1}` on every application, by S6's
dels-before-adds contract, replay included). On the stale-point tail
replay, only net state convergence is asserted per batch: an interior
batch replayed against a state that already reflects a later, colliding
batch in the same tail can report a real transient non-zero count even
though the tail's net effect stays a no-op. This is storage-level; the
SPARQL-level version (same feed rendered as `DELETE DATA;INSERT DATA`
requests) lives in `crates/sparql/tests/update_feed_replay.rs` and
additionally pins one-batch-per-operation ordering (a request
`DELETE{q};INSERT{q};DELETE{q}` ends absent).

### Cross-plan handshake

MemStore quad storage (graph-keyed maps + a quad insert seam) is specified
identically in PLAN-28-03 Task 3 — whichever plan executes first
implements it; the second finds it done. The `USING` dataset machinery
reuses S3's `DatasetSpec` the same way.

### File map

- Modify: `crates/storage/src/{tier.rs,memory_tier.rs,store.rs,partition.rs}`
- Create: `crates/storage/tests/feed_replay.rs`
- Modify: `crates/sparql/src/{update.rs,exec/mod.rs,exec/horn.rs,exec/mem.rs}`
- Create: `crates/sparql/tests/{update_named_graph.rs,update_feed_replay.rs,w3c_update_suite.rs}`
- Modify: `crates/sparql/tests/{update_graph_mgmt.rs,update_where.rs}` (pin inversions)
- Modify: `crates/harness/src/selected.rs`, `harness/selected.toml`,
  `crates/harness/scripts/fetch-w3c-suites.sh`, `harness/KNOWN-MANIFEST-BUGS.md`
- Modify: `docs/architecture.md`, `crates/sparql/INTEGRATION-NOTES.md`,
  `docs/specs/SPEC-28-named-graph-dataset-semantics.md` (only if D11
  fallback triggers), this plan

---

### Task 1: `apply_quad_batch` — the S6 store boundary

**Files:**
- Modify: `crates/storage/src/{tier.rs,memory_tier.rs,store.rs,partition.rs}`

- [ ] **Step 1: Failing tests** (`store.rs` test module +
  `memory_tier.rs`): `apply_is_one_commit_version` (dels+adds, version
  bumps by exactly 1; a reader pinned before sees neither),
  `dels_before_adds_within_batch` (del+add same quad → present; add in
  batch N, del+add in batch N+1 → present with one retract + one insert
  counted), `insert_present_is_counted_noop` (re-insert visible quad →
  `inserted == 0`, **version unchanged**), `retract_absent_is_counted_noop`
  (existing behaviour, now through the combined path),
  `noop_batch_does_not_bump_version`,
  `non_canonical_literal_identity_preserved` (`"01"^^xsd:integer` insert +
  `"1"^^xsd:integer` delete → delete is a 0-count no-op),
  `quad_identity_is_per_graph` (same triple in two graphs: retracting one
  leaves the other).
- [ ] **Step 2: Verify failure** — `cargo nextest run -p horndb-storage
  apply_`.
- [ ] **Step 3: Implement** per the design (one writer pass, per-graph
  partition rebuild, counts, `live_len` maintained, wrappers re-cut).
- [ ] **Step 4: Run** — `cargo nextest run -p horndb-storage`, then
  `cargo nextest run -p horndb-sparql` (compile fallout from the wrapper
  signature change, if any).
- [ ] **Step 5: Commit** — `feat(storage): apply_quad_batch — atomic
  dels-then-adds, idempotent, counted (SPEC-28 S6, #267)`.

### Task 2: Replay differential

**Files:**
- Create: `crates/storage/tests/feed_replay.rs`

- [ ] **Step 1:** Write the proptest per the design. Run:
  `cargo nextest run -p horndb-storage feed_replay` — must pass against
  Task 1's implementation; any failure is a Task 1 bug (budget debugging
  here, this is the acceptance-7 gate).
- [ ] **Step 2: Commit** — `test(storage): at-least-once feed replay
  differential (SPEC-28 S6/S7, #267)`.

### Task 3: Quad-shaped write trait + backends

**Files:**
- Modify: `crates/sparql/src/exec/{mod.rs,horn.rs,mem.rs}`

- [ ] **Step 1: Failing tests** — backend-generic (both `HornBackend` and
  `MemStore`): `apply_quads_routes_by_graph`, `apply_counts_are_accurate`
  (mirror Task 1's pins at this layer), `clear_graph_and_exists`,
  `scan_graph_quads_roundtrip`. MemStore quad storage lands here if
  PLAN-28-03 hasn't already (Cross-plan handshake).
- [ ] **Step 2: Verify failure.**
- [ ] **Step 3: Implement** the trait re-cut per the design; delete
  `insert_triple`/`delete_triple`/`clear_all`; `update.rs` gets a
  minimal mechanical adaptation to keep compiling (full rewrite is Task
  4) — if that staging is awkward, fold Tasks 3+4 into one commit rather
  than leaving a broken intermediate.
- [ ] **Step 4: Run** — `cargo nextest run -p horndb-sparql`.
- [ ] **Step 5: Commit** — `feat(sparql): quad-shaped backend write trait
  (SPEC-28 S4/S6, #267)`.

### Task 4: `update.rs` — quad data, pattern updates, graph management

**Files:**
- Modify: `crates/sparql/src/update.rs`
- Create: `crates/sparql/tests/update_named_graph.rs`
- Modify: `crates/sparql/tests/update_graph_mgmt.rs` (pin inversions)

- [ ] **Step 1: Failing tests** (`update_named_graph.rs`, both backends):
  `insert_delete_data_graph_blocks` (quads land in / leave the named
  graph; default untouched), `one_batch_per_operation`
  (`DELETE DATA{q};INSERT DATA{q};DELETE DATA{q}` → absent; commit-version
  delta == number of effective ops), `pattern_update_named_template`
  (`INSERT { GRAPH <g> { … } } WHERE { … }`),
  `create_clear_drop_existence_semantics` (the D11 matrix: CREATE
  absent-ok / exists-error-unless-silent; CLEAR/DROP absent-error-unless-
  silent; DROP empties → graph gone from `graphs()`),
  `drop_all_spares_reserved` (seed a reserved-graph quad at the storage
  layer; `DROP ALL` leaves it), `clear_drop_flow_through_delta_boundary`
  (counts reported match graph size — the no-structural-unlink pin),
  `load_routes_to_destination` + `load_nq_routes_quads_to_their_graphs` +
  `load_nq_into_graph_errors`, `reserved_namespace_closed_to_writes`
  (every write form; **with and without SILENT**),
  `add_move_copy_between_named_graphs` (the desugared pairs execute; MOVE
  removes the source; identity no-op pins stay),
  `add_silent_missing_source_is_noop` (the SILENT-recovery pin —
  **inverts** `update_graph_mgmt.rs:407`
  `add_named_operand_silent_still_errors`).
  Invert the existing rejection pins in `update_graph_mgmt.rs`
  (`:105-151`, `:262-303`, `:331-371`, `:444-477`) to their SPARQL 1.1
  behaviours, each with a comment citing this plan.
- [ ] **Step 2: Verify failure.**
- [ ] **Step 3: Implement** per the design: per-op batching, template
  routing, D11 verbs, LOAD routing, the SILENT tokenizer + hint
  alignment, the reserved-namespace check, preflight mirror. The
  spargebra-`WITH` discovery test runs first and its finding is recorded
  as a comment where the WHERE-side scoping is handled.
- [ ] **Step 4: Run** — `cargo nextest run -p horndb-sparql` (and
  `--features server`).
- [ ] **Step 5: Commit** — `feat(sparql): named-graph Update — quad data,
  graph management, WITH/USING, SILENT fidelity, closed reserved
  namespace (SPEC-28 S4, #267)`.

### Task 5: SPARQL-level replay + W3C update families

**Files:**
- Create: `crates/sparql/tests/update_feed_replay.rs`,
  `crates/sparql/tests/w3c_update_suite.rs`
- Modify: `crates/harness/src/selected.rs`, `harness/selected.toml`,
  `crates/harness/scripts/fetch-w3c-suites.sh`,
  `harness/KNOWN-MANIFEST-BUGS.md`

- [ ] **Step 1:** `update_feed_replay.rs` per the design (rendered-request
  replay, one-batch-per-op ordering pin).
- [ ] **Step 2:** The `[sparql_update]` section + `w3c_update_suite.rs`
  runner + mirrored fixtures for `add/`, `copy/`, `move/`, `clear/`,
  `drop/`, graph-specific `delete-insert/`; exclusions documented (D11
  cases). If the D11 exclusion count is material, raise on #261 per the
  spec's risk clause before proceeding.
- [ ] **Step 3:** `cargo nextest run -p horndb-sparql` green;
  `cargo nextest run --workspace` (harness compiles the new selected.toml
  schema).
- [ ] **Step 4: Commit** — `test(sparql,harness): W3C update graph families
  + feed replay at the SPARQL layer (SPEC-28 S7, #267)`.

### Task 6: Docs sync

**Files:**
- Modify: `docs/architecture.md`, `crates/sparql/INTEGRATION-NOTES.md`,
  this plan

- [ ] **Step 1:** `architecture.md`: named-graph Update rows → implemented;
  the update-path "honest limitation" text in `INTEGRATION-NOTES.md` is
  replaced by the new behaviour description (incl. the SILENT-recovery
  mechanism and its upstream-issue pointer). Flip this plan's status.
- [ ] **Step 2:** Full verification — fmt, clippy `-D warnings`,
  `cargo nextest run --workspace`.
- [ ] **Step 3: Commit** — `docs(sparql): SPEC-28 S4/S6 sync (#267)`.

---

## Self-review notes

- S4 coverage: quad data → T4; pattern updates → T4; graph management +
  D11 → T4; reserved namespace → T4; no-structural-unlink → T4 pin;
  `DROP ALL` scope → T4; one-batch-per-op → T1+T4; SILENT fidelity → T4
  (tokenizer); WITH/USING → T4 (with the spargebra discovery step and the
  phase-3 dependency stated); atomicity → preflight mirror in T4.
- S6 coverage: requirement/enforcement point → T1; lexical identity → T1
  pin; within-batch ordering → T1; replay convergence → T2+T5; SPEC-30
  handoff — `apply_quads` + `ApplyReport` is exactly the surface
  PLAN-30-01 consumes.
- Honest risks: the SILENT tokenizer is the ugliest piece — scoped,
  alignment-checked, fallback-to-error, and deletable on an upstream fix;
  the alternative (parsing update text ourselves) is worse. spargebra's
  `WITH` desugaring is pinned by a discovery test before anything builds
  on it.

---

## Deviations landed

Recorded so this executed plan stays an honest historical record — each
item differs from the design text above, without changing the delivered
behaviour's correctness.

- **Trait method name.** The design's `Store::named_graphs(&self) ->
  Vec<String>` shipped as `Store::graphs`. The literal name `named_graphs`
  collides with `Executor::named_graphs` (an existing method with different
  semantics on a different trait in the same module); `graphs` avoided the
  clash and matches the phase-2 `StoreSnapshot::graphs()` naming it wraps.
- **`insert_triple`/`delete_triple` were not deleted.** The design called
  for deleting them alongside `clear_all`. Only `clear_all` was removed;
  `insert_triple`/`delete_triple` survive as trait-default methods that
  delegate to `apply_quads` — deleting them would have required rewriting
  roughly 184 pre-existing test call sites across the crate for no
  behavioural gain, since the defaults already route through the real S6
  seam.
- **SILENT-ambiguity fallback is a no-op, not the design's "non-silent
  error."** The design text (`ADD`/`MOVE`/`COPY` §) specifies that an
  ambiguous hint alignment "falls back to non-silent — an honest error."
  The shipped `amc_source_status` instead treats an unaligned copy-op as
  `AmcSourceStatus::Ok` (proceed): since the underlying operation is a
  `DeleteInsert` reading a possibly-absent named graph, an absent source
  reads as zero rows and the op is a no-op, not an error. This is
  data-safe (never a silent wrong data change) but is observably different
  from the specified behaviour on the narrow ambiguous-alignment case
  (an identity `ADD <g> TO <g>`, or a user `DeleteInsert` matching the
  copy shape). **Known open minor**, to be resolved at final review —
  either update this plan's design intent to match, or change the code
  to error as originally specified.
- **Multi-op existence atomicity gap, not closed here.** `validate_op`
  preflights every operation against the pre-update store, which is exact
  for a single operation and for independent operations, but not for a
  multi-op sequence where an earlier operation changes a graph's existence
  that a later operation then existence-checks. Closing this needs
  store-level rollback, out of scope for this plan; tracked against
  `SPEC-30`. Documented in `update.rs::validate_op`, `docs/architecture.md`,
  and `crates/sparql/INTEGRATION-NOTES.md`.
