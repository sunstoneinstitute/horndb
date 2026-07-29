---
status: draft
date: 2026-07-29
scope: "SPEC-28 phase 2 (S2) — graph-scoped access paths: scan_graph, scan_predicate(graph, …), graph_len via cached per-partition live counts, visibility-filtered graphs(), whole-store len, and the HornBackend quad-grain de-hardwiring"
---

# SPEC-28 phase 2 — Graph-scoped access paths

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give `crates/storage`'s read surface a graph parameter — whole-graph
scan, graph-parameterized predicate scan, `graph_len`, visibility-filtered
graph enumeration — at a cost proportional to the graph, and stop
`HornBackend` hard-wiring `DEFAULT_GRAPH` on its write path. Tracking issue:
[#265](https://github.com/sunstoneinstitute/horndb/issues/265). Spec:
`docs/specs/SPEC-28-named-graph-dataset-semantics.md` §S2.

**Architecture:** The tier already has the right shape — `TierSnapshot` keys
partitions `(GraphId outer, predicate TermId inner)` (`memory_tier.rs:24`,
`:15`), so every new read path is a map probe plus existing per-partition
machinery. The one new primitive is a **cached per-partition live count**
(`live_len`), computed where partitions are built, which makes `graph_len`
unconditionally O(predicates in graph) and makes `graphs()`
visibility-filtering free. No partition layout change, no new ordering, no
WCOJ change.

**Tech Stack:** Rust 1.90, existing `crates/storage` + `crates/sparql`;
criterion for the thousand-graph bench.

**No user-visible behaviour change.** Every SPARQL path still writes and reads
the default graph only (phases 3/4 change that); this phase is the plumbing
they stand on. The selected conformance subset must stay green untouched.

---

## Design (read this before any task)

### Current state (verified 2026-07-29)

- `Store::insert_quads` (`store.rs:91`) / `retract_quads` (`store.rs:123`) /
  `intern_graph_uri` (`store.rs:151`) exist and work; `GraphId(pub u64)` with
  `DEFAULT_GRAPH = GraphId(0)` (`term.rs:85,89`; safe — dictionary indices
  start at 1).
- Hard-wired default-graph read paths on `StoreSnapshot`:
  `scan_predicate_default_graph` (`store.rs:262`), `scan_predicate_ordered`
  (`:282`), `top_predicates` (`:303`), `scan_all_term_ids` (`:315`), `len`
  (`:352`), `is_empty` (`:366`), `contains` (`:375`), `iter_all_term_ids`
  (`:387`).
- `triple_count` (`store.rs:53`, `:251`) is already whole-tier and
  visibility-filtered. `Tier::graphs()` (`tier.rs:32`) is **not**
  visibility-filtered — it returns every graph key ever written.
- `PredicatePartition::len_at(at)` (`partition.rs:174`) is O(1) only on the
  insert-only fast path (`!has_retractions && at >= max_begin`); with any
  retraction it degrades to O(rows). There is no cached live count.
- `HornBackend` (`crates/sparql/src/exec/horn.rs`): `live_keys:
  HashSet<(u64,u64,u64)>` (`:208`) is triple-keyed; `len` (`:239`) and
  `is_empty` (`:260`) lean on the documented assumption "`HornBackend` never
  writes a named graph"; `clear_all` (`:565`) sweeps
  `(DEFAULT_GRAPH, s, p, o)` via `tier().retract_quad_batch`; all writes
  funnel through `insert_oxrdf`/`insert_oxrdf_batch` →
  `Store::insert_triples` (default-graph tagging at `store.rs:84`).

### Why `live_len` is correct as a per-partition cache

Copy-on-write gives each `TierSnapshot` its own immutable partition objects:
a write rebuilds only the affected graph's partitions at `version + 1`
(`memory_tier.rs:348-393`), and a pinned older snapshot keeps the *old*
objects. Within one partition object, every row's `begin` ≤ the owning
snapshot's version (rows are stamped at rebuild time), and no row's `end` can
be in `(version, UNSET_END)` — setting an `end` happens only by building a
*new* partition at a *newer* version. So for the snapshot that owns the
partition, `visible(begin, end, version) ⟺ end == UNSET_END`, and a count of
`end == UNSET_END` rows frozen at build time **is** `len_at(version)` for that
snapshot. Older pinned versions keep using the scan path (`len_at(at)` with
`at < version` is only reachable through APIs that take an explicit older
`at`; the `StoreSnapshot` surface always passes its own version).

Two build sites must maintain the field (miss one and the cache silently
lies — the differential test in Task 1 exists for exactly that):

1. `PartitionBuilder::build_with_hot_threshold` (`partition.rs:417-452`) —
   count `end == UNSET_END` rows after the sort+dedup pass, which already
   visits every row.
2. The retraction rebuild inside `MemoryTier::retract_quad_batch`
   (`memory_tier.rs:397-460`), which stamps `end = new_version` in a linear
   pass — `live_len = old live count − retracted-in-this-partition`, or
   recount in the same pass if the code path doesn't go through the builder.

### New/changed public surface

| API | Where | Contract |
|---|---|---|
| `PredicatePartition::live_len() -> usize` | `partition.rs` | rows visible at the owning snapshot's version; O(1) |
| `StoreSnapshot::graph_len(&self, g: GraphId) -> usize` | `store.rs` | Σ `live_len` over `tier.predicates(g)`; O(predicates in graph), unconditional |
| `StoreSnapshot::len() -> usize` | `store.rs:352` | **whole-store** (= `triple_count() as usize`); the default-graph scoping is retired |
| `StoreSnapshot::scan_graph(&self, g: GraphId) -> Result<Vec<(Term, Term, Term)>>` | `store.rs` | every visible triple in one graph, decoded; O(quads in graph + predicates in graph) |
| `StoreSnapshot::iter_graph_term_ids(&self, g: GraphId) -> impl Iterator<Item = (TermId, TermId, TermId)>` | `store.rs` | id-level twin of `scan_graph`, key-ordered like `iter_all_term_ids`; this is what the future SPEC-24 S6 backing and the phase-5 GSP diff consume |
| `StoreSnapshot::scan_predicate(&self, g: GraphId, p: &Term) -> Result<Vec<(Term, Term)>>` | `store.rs:262` | replaces `scan_predicate_default_graph`; the old name is **deleted**, not aliased |
| `Store::scan_predicate(&self, g: GraphId, p: &Term)` | `store.rs:159` | delegation, same rename |
| `StoreSnapshot::graphs() -> Vec<GraphId>` | `store.rs` (new) | exactly the graphs with ≥1 visible quad (D11), **including** `DEFAULT_GRAPH` when non-empty; callers wanting named graphs only filter the sentinel |
| `Tier::graphs()` / `TierSnapshot::graphs()` | `tier.rs:32`, `memory_tier.rs:91` | visibility-filtered (`live_len > 0` on any partition); `stats()` (`memory_tier.rs:121`) shares the helper instead of re-filtering |
| `StoreSnapshot::graph_uri(&self, g: GraphId) -> Result<Term>` | `store.rs` | decode a `GraphId` back to its IRI term (`dictionary.lookup(TermId(g.0))`); error on the sentinel |

Unchanged on purpose: `scan_predicate_ordered`, `top_predicates`,
`scan_all_term_ids`, `iter_all_term_ids`, `contains` keep their default-graph
scope this phase — their callers (WCOJ snapshot build, stats, snapshot
export) are default-graph semantics until phase 3 decides the union
composition. Do **not** widen them speculatively.

### The `snapshot_len_is_default_graph_scoped` inversion — and the spec amendment

The test (`store.rs:600`) pins `len()` to the default graph "because the
SPEC-24 S6 surface backs the single-graph incremental circuit". Verified
reality: **that backing edge does not exist in code.** `crates/incremental`
has no `horndb-storage` dependency; `incremental::snapshot::Snapshot` merely
mirrors `StoreSnapshot`'s shape in anticipation of the S6 swap
([#213](https://github.com/sunstoneinstitute/horndb/issues/213), not landed).
So inverting `len()` breaks no circuit — the relocation SPEC-28 S2 demands is
a *contract* relocation: the graph-scoped surface (`graph_len`,
`iter_graph_term_ids`) must exist and be documented as what #213 wires to, so
the swap lands per-graph rather than re-growing a whole-store assumption.
Task 6 amends the spec sentence ("in the same change, the circuit's snapshot
backing moves to `graph_len`") to say this, and leaves a pointer on #213.

### HornBackend de-hardwiring (prep for phase 4, invisible now)

- `live_keys` becomes `HashSet<(u64, u64, u64, u64)>` keyed
  `(graph.0, s, p, o)`; every current use inserts/looks up with
  `DEFAULT_GRAPH.0` as the first element.
- The internal write funnel gains the graph: `insert_oxrdf` /
  `insert_oxrdf_batch` grow quad-shaped siblings
  (`insert_oxrdf_quad_batch(Vec<(GraphName-ish, Term×3)>)` is *not* needed
  yet — what phase 4 needs is `insert_quads_interned`-style internals; keep
  it minimal: thread a `GraphId` parameter through the private funnel and
  have the public triple-shaped `exec::Store` trait impls pass
  `DEFAULT_GRAPH`). The `exec::Store` trait itself (`exec/mod.rs:156`) does
  **not** change this phase — phase 4 owns the trait surface.
- `clear_all` (`horn.rs:565`) sweeps every graph: iterate
  `snapshot.graphs()`, collect `(g, s, p, o)` via `iter_graph_term_ids`,
  one `retract_quad_batch`. Observationally identical today (only the
  default graph is ever populated through this backend) and correct the day
  a named graph exists.
- `len` (`horn.rs:239`) / `is_empty` (`:260`) keep calling
  `store.triple_count()` — the *code* is already whole-store; only the doc
  comment's justification ("never writes a named graph") is dead. Rewrite
  the comments to state the new contract: `HornBackend::len` is the
  whole-store live count, which phase 3 will re-examine for the union
  default graph. Pushdowns are phase 3's problem; nothing here changes
  result shapes.

### Bench — thousand-graph scan cost and partition overhead

New `crates/storage/benches/graph_scan.rs`:

- Corpus: 1,000 named graphs × 1,000 triples each (5 predicates per graph,
  interned once), plus one 10-triple "small" graph.
- `scan_graph/small_graph_in_1k_store`: scan the 10-triple graph; the
  criterion number must not move when the corpus is doubled to 2,000 graphs
  (`scan_graph/small_graph_in_2k_store` — same work, twice the store; the
  pair *is* the O(graph)-not-O(store) evidence for acceptance criterion 4).
- `graph_len/small_graph_in_1k_store`: same shape for the count path.
- Partition-overhead measurement (SPEC-28 risk "thousands of small graphs
  versus per-partition overhead"): after loading, record
  `TierStats.bytes_estimated / total quads` and print it from the bench
  setup (a `--nocapture`-style eprintln is fine; criterion measures time,
  the bytes number is read off the run log). Record both time and B/quad in
  `docs/benchmarks.md` from **hornbench**, with the SPEC-02 NF1 ≤50 B/triple
  budget named next to the measured number. If the number busts the budget,
  that finding goes to #265 for a decision (shared-partition layout vs
  small-graph representation) — this plan measures, it does not redesign.

### File map

- Modify: `crates/storage/src/partition.rs` — `live_len` field + accessor,
  builder counting.
- Modify: `crates/storage/src/memory_tier.rs` — retraction-path `live_len`
  maintenance; visibility-filtered `graphs()`; `stats()` sharing the filter.
- Modify: `crates/storage/src/tier.rs` — `graphs()` doc contract.
- Modify: `crates/storage/src/store.rs` — new/renamed snapshot APIs, test
  inversion, new tests.
- Modify: `crates/sparql/src/exec/horn.rs` — quad-keyed `live_keys`,
  graph-threaded write funnel, `clear_all` sweep, comment rewrites.
- Modify (mechanical rename fallout): `crates/storage/tests/store_roundtrip.rs:22`,
  `crates/storage/tests/snapshot_isolation.rs:63,106`.
- Create: `crates/storage/benches/graph_scan.rs` + `[[bench]]` entry in
  `crates/storage/Cargo.toml`.
- Modify: `docs/specs/SPEC-28-named-graph-dataset-semantics.md` (S2
  circuit-edge amendment), `docs/architecture.md`, `docs/benchmarks.md`,
  `crates/storage/INTEGRATION-NOTES.md`, this plan's status.

---

### Task 1: Cached per-partition live count (`live_len`)

**Files:**
- Modify: `crates/storage/src/partition.rs`
- Modify: `crates/storage/src/memory_tier.rs`

- [ ] **Step 1: Write the failing tests** — in `partition.rs`'s test module:
  (a) `live_len_matches_len_at_own_version_insert_only`: build a partition
  of 100 rows, no retractions; assert `part.live_len() == part.len_at(v)`
  where `v` is the build version.
  (b) `live_len_matches_len_at_own_version_after_retraction`: drive a
  `MemoryTier` through `insert_quad_batch` then `retract_quad_batch`
  retracting a strict subset; for every `(g, p)` partition in the resulting
  snapshot assert `part.live_len() == part.len_at(snapshot.version())`
  (walk via `with_predicate`). This is the two-build-site differential — it
  fails if either the builder or the retraction path forgets the count.
  (c) Property test (proptest, matching the crate's existing style): random
  interleavings of insert/retract batches over a small id space; after each
  batch, for every partition, `live_len() == len_at(version)`.
- [ ] **Step 2: Run tests, verify they fail** — `cargo nextest run -p
  horndb-storage live_len` → compile error (`live_len` undefined).
- [ ] **Step 3: Implement** — `live_len: usize` on `PredicatePartition`;
  count `end == UNSET_END` after the dedup in
  `build_with_hot_threshold` (`partition.rs:417-452`); in
  `retract_quad_batch`'s rebuild pass (`memory_tier.rs:429-440`), carry
  `old.live_len - retracted_here` (or recount in the same linear pass —
  whichever the code structure makes obviously correct; the differential
  test is the referee). `pub fn live_len(&self) -> usize`.
- [ ] **Step 4: Run tests, verify pass** — `cargo nextest run -p
  horndb-storage live_len` and the full crate: `cargo nextest run -p
  horndb-storage`.
- [ ] **Step 5: Commit** — `feat(storage): cached per-partition live count
  (SPEC-28 S2, #265)`.

### Task 2: `graph_len`, whole-store `len`, visibility-filtered `graphs()`

**Files:**
- Modify: `crates/storage/src/store.rs`, `crates/storage/src/memory_tier.rs`,
  `crates/storage/src/tier.rs`

- [ ] **Step 1: Write the failing tests** — in `store.rs`'s test module:
  (a) **Invert** `snapshot_len_is_default_graph_scoped` (`store.rs:600`):
  rename to `snapshot_len_is_whole_store`; same fixture (one default-graph
  triple + one named-graph quad); assert `snap.len() == 2`,
  `snap.graph_len(DEFAULT_GRAPH) == 1`, `snap.graph_len(g1) == 1`,
  `snap.graph_len(absent) == 0`. Keep a comment: the old default-graph
  contract relocated to `graph_len`; the SPEC-24 S6 backing is a shape
  contract until #213 lands (see PLAN-28-02 design).
  (b) `graphs_is_visibility_filtered`: insert quads into `g1` and `g2`,
  retract all of `g2`'s; assert `snap.graphs()` contains `g1` (and
  `DEFAULT_GRAPH` iff it holds data) and not `g2`; assert
  `store.tier().graphs()` agrees. A fully-retracted graph ceases to exist
  (D11).
  (c) `graph_uri_roundtrip`: `intern_graph_uri(t)` then
  `snap.graph_uri(g) == t`; `graph_uri(DEFAULT_GRAPH)` errors.
- [ ] **Step 2: Run tests, verify they fail** — `cargo nextest run -p
  horndb-storage snapshot_len_is_whole_store graphs_is_visibility_filtered
  graph_uri_roundtrip`.
- [ ] **Step 3: Implement** — `StoreSnapshot::len` = `triple_count() as
  usize` (update `is_empty` accordingly); `graph_len(g)` = Σ
  `with_predicate(g, p, |part| part.live_len())` over
  `tier.predicates(g)`; `TierSnapshot::graphs()` filters
  `partitions.values().any(|p| p.live_len() > 0)` and `stats()`
  (`memory_tier.rs:121`) reuses the same predicate instead of its own
  `len_at` filter; `Tier::graphs()` doc comment states the D11 contract;
  `StoreSnapshot::graphs()` and `graph_uri()` added. Check
  `has_named_graph_data` (`store.rs:330`) — it can now be
  `self.tier.graphs().into_iter().any(|g| g != DEFAULT_GRAPH)` since
  `graphs()` is visibility-filtered; simplify it and keep its tests green.
- [ ] **Step 4: Run the full crate suite** — `cargo nextest run -p
  horndb-storage`. `snapshot_isolation.rs` exercises pinned old versions —
  watch for any test that asserted the old `len()` scoping and update it
  with a comment citing this plan.
- [ ] **Step 5: Commit** — `feat(storage): graph_len + whole-store len +
  visibility-filtered graphs() (SPEC-28 S2, #265)`.

### Task 3: `scan_graph`, `iter_graph_term_ids`, `scan_predicate(graph, …)`

**Files:**
- Modify: `crates/storage/src/store.rs`
- Modify: `crates/storage/tests/store_roundtrip.rs`,
  `crates/storage/tests/snapshot_isolation.rs`

- [ ] **Step 1: Write the failing tests** — in `store.rs`'s test module:
  (a) `scan_graph_returns_exactly_the_graphs_quads`: three graphs (default +
  two named) with overlapping triples (same `(s,p,o)` asserted in two
  graphs); `scan_graph(g1)` returns exactly `g1`'s triples, decoded, and the
  shared triple appears in both graphs' scans.
  (b) `scan_graph_respects_visibility`: retract one of `g1`'s quads;
  `scan_graph(g1)` on a fresh snapshot omits it; a snapshot pinned *before*
  the retraction still returns it (pin first, retract, then scan the old
  snapshot).
  (c) `iter_graph_term_ids_is_key_ordered`: mirrors
  `iter_all_term_ids`'s ordering contract (predicates ascending,
  subject-major within), per graph.
  (d) `scan_predicate_takes_a_graph`: `scan_predicate(g1, &p)` sees only
  `g1`'s rows; `scan_predicate(DEFAULT_GRAPH, &p)` reproduces what
  `scan_predicate_default_graph` returned on the same fixture.
- [ ] **Step 2: Run tests, verify they fail** — `cargo nextest run -p
  horndb-storage scan_graph iter_graph_term_ids scan_predicate_takes`.
- [ ] **Step 3: Implement** — `scan_graph` / `iter_graph_term_ids` follow
  the `scan_all_term_ids` pattern (`store.rs:315-327`) with `g` in place of
  `DEFAULT_GRAPH` (sorted `tier.predicates(g)`, `with_predicate(g, p,
  scan_at(version))`, decode via `self.term` for the lexical form). Rename
  `scan_predicate_default_graph` → `scan_predicate(graph, predicate)` on
  both `Store` (`store.rs:159`) and `StoreSnapshot` (`store.rs:262`) —
  delete the old name (grep proves zero external callers; the three
  storage-internal test call sites update mechanically:
  `store_roundtrip.rs:22`, `snapshot_isolation.rs:63,106`).
- [ ] **Step 4: Run the crate suite, then the workspace's storage
  dependents** — `cargo nextest run -p horndb-storage && cargo nextest run
  -p horndb-sparql` (sparql compiles against storage; the rename must not
  reach it — it never called the old name).
- [ ] **Step 5: Commit** — `feat(storage): scan_graph + graph-parameterized
  scan_predicate; retire scan_predicate_default_graph (SPEC-28 S2, #265)`.

### Task 4: HornBackend de-hardwiring

**Files:**
- Modify: `crates/sparql/src/exec/horn.rs`

- [ ] **Step 1: Write the failing test** — in the sparql test tree (follow
  the existing `HornBackend` unit-test placement),
  `clear_all_sweeps_named_graphs`: build a `HornBackend`, reach through
  `backend.store()` (or the existing test accessor) to `insert_quads` one
  named-graph quad directly at the storage layer, insert one default-graph
  triple through the backend, call `clear_all`, assert
  `store.triple_count() == 0`. Today this fails: the sweep only covers
  `DEFAULT_GRAPH`, and `live_keys`-emptiness short-circuits (`horn.rs:566`
  returns early when `live_keys` is empty — a named-graph-only store would
  skip the sweep entirely; the new code must consult the store, not the
  cache, for the early-out).
- [ ] **Step 2: Run it, verify it fails** — `cargo nextest run -p
  horndb-sparql clear_all_sweeps_named_graphs`.
- [ ] **Step 3: Implement** — key `live_keys` by `(graph.0, s, p, o)` with
  `DEFAULT_GRAPH.0` at every current site (`horn.rs:281`, `:331`, `:353`,
  `:557`); thread a `GraphId` parameter through the private write funnel
  (`insert_oxrdf`, `insert_oxrdf_batch` internals) with the public
  triple-shaped `exec::Store` impls passing `DEFAULT_GRAPH` — the trait
  (`exec/mod.rs:156`) is untouched; rewrite `clear_all` to sweep
  `snapshot.graphs()` × `iter_graph_term_ids` in one `retract_quad_batch`,
  early-out on `store.triple_count() == 0` instead of `live_keys`
  emptiness; rewrite the `len` (`horn.rs:236-241`) and `is_empty` doc
  comments — whole-store live count as the stated contract, with a pointer
  to phase 3 for the union-default-graph re-examination. Delete the "never
  writes a named graph" sentence everywhere it appears.
- [ ] **Step 4: Run the sparql suite** — `cargo nextest run -p horndb-sparql`
  and `cargo nextest run -p horndb-sparql --features server`. Everything
  must pass unchanged: this task alters no observable SPARQL behaviour.
- [ ] **Step 5: Commit** — `refactor(sparql): quad-keyed live_keys +
  graph-threaded write funnel + all-graph clear_all (SPEC-28 S2, #265)`.

### Task 5: Thousand-graph bench

**Files:**
- Create: `crates/storage/benches/graph_scan.rs`
- Modify: `crates/storage/Cargo.toml` (`[[bench]] name = "graph_scan"
  harness = false`)

- [ ] **Step 1: Write the bench** per the design section: groups
  `scan_graph/small_graph_in_1k_store`, `scan_graph/small_graph_in_2k_store`,
  `graph_len/small_graph_in_1k_store`; corpus built once per group via
  `intern_graph_uri` + `insert_quads` in 65k batches; eprintln the
  `TierStats`-derived bytes/quad from setup.
- [ ] **Step 2: Local smoke** — `cargo bench -p horndb-storage --bench
  graph_scan -- --quick`; sanity: the 1k-store and 2k-store small-graph
  numbers are within noise of each other (that is the acceptance-4 signal),
  and `graph_len` is microseconds, not milliseconds.
- [ ] **Step 3: hornbench run** — `ssh hornbench`, repo at `~/src/horndb`,
  check out the branch, `cargo bench -p horndb-storage --bench graph_scan`;
  record scan time, `graph_len` time, and B/quad in `docs/benchmarks.md`
  (new "graph-scoped scan" row set, host + commit noted, NF1 budget named).
  If B/quad > 50, note the bust in #265 — measurement, not redesign.
- [ ] **Step 4: Commit** — `bench(storage): thousand-graph scan_graph /
  graph_len / partition-overhead bench (SPEC-28 S2, #265)`.

### Task 6: Docs + spec amendment

**Files:**
- Modify: `docs/specs/SPEC-28-named-graph-dataset-semantics.md`,
  `docs/architecture.md`, `crates/storage/INTEGRATION-NOTES.md`, this plan

- [ ] **Step 1: Amend SPEC-28 S2's circuit bullet** ("Where the
  default-graph-scoped `len` contract goes"): the incremental circuit has no
  live storage edge — `crates/incremental` does not depend on
  `horndb-storage`; the S6 backing (#213) is a shape contract. Rewrite the
  bullet to require what is actually requirable now: the graph-scoped
  surface (`graph_len`, `iter_graph_term_ids`) exists, is documented as what
  #213 wires to, and #213 carries a comment pointing here. Post that note on
  #213 (one `gh issue comment`).
- [ ] **Step 2: Sync docs** — `docs/architecture.md`: SPEC-28 phase-2 row →
  implemented; `crates/storage/INTEGRATION-NOTES.md`: the snapshot-surface
  section gains the graph-scoped APIs and drops any "default-graph only"
  claims this plan made false; flip this plan to `status: in-progress` at
  Task 1 and `executed` here (same commit as the last task).
- [ ] **Step 3: Full verification** — `cargo fmt --all`, `cargo clippy
  --workspace --all-targets -- -D warnings`, `cargo nextest run
  --workspace`.
- [ ] **Step 4: Commit** — `docs(storage): SPEC-28 S2 sync — spec circuit
  amendment, architecture, integration notes (#265)`.

---

## Self-review notes

- Spec coverage: S2's seven bullets map to Task 3 (whole-graph scan +
  predicate scan), Task 2 (counts + enumeration + D11), Task 1 (the cost
  bound behind them), Task 4 (executor de-hardwiring), Task 5 (the
  acceptance-4 measurement + NF1 risk), Task 6 (SPEC-02 refinement is
  descriptive — the partition key already is `(graph, predicate)` — and the
  circuit bullet is amended to match verified reality).
- Deliberately out: widening `scan_all_term_ids` / `iter_all_term_ids` /
  `contains` / ordered scans (phase 3 owns dataset composition), the
  `exec::Store` trait (phase 4), any pushdown work (phase 3).
- The rename (`scan_predicate_default_graph` → `scan_predicate`) is safe by
  measurement: zero call sites outside `crates/storage`.
