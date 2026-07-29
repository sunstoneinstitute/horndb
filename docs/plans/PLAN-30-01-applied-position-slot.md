---
status: draft
date: 2026-07-29
scope: "SPEC-30 P1 — the applied-position slot as quads in the reserved feed graph riding the request's final store batch, feed-id refusal, the S1 durability contract surfaced at startup, the S5 slot-advance-once rule, and the six horndb_feed_* metrics"
---

# SPEC-30 P1 — Applied-position slot and the durability contract

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A change-feed consumer can hand HornDB its position with each
applied batch, read back the position HornDB actually holds, and rely on the
one-sided guarantee that the reported position is never ahead of surviving
data. Tracking issue:
[#270](https://github.com/sunstoneinstitute/horndb/issues/270). Spec:
`docs/specs/SPEC-30-change-feed-materializer.md` §S1–S3, S5, S6.
**Depends on** SPEC-28 phase 4 (`Store::apply_quads` + one-batch-per-op,
PLAN-28-04) for the write path and phase 3 (ground `GRAPH` queries,
PLAN-28-03) for the read path.

**Architecture:** The slot **is quads** in the reserved graph
`https://horndb.io/graph/feed`, written by appending
retract-old-slot + insert-new-slot to the request's **final** operation
batch through the same `apply_quads` boundary as the data. That makes D1
(same durability fate) true by construction — on today's fully in-memory
store the slot vanishes with the data on restart, recovering as absent,
which is exactly the honest "resume from the beginning" the spec's P1
promises. No sidecar file exists (the spec's open item marks one a D5
trap). The consumer supplies feed id + position as request headers on
`/update`; a mismatch against a non-empty slot refuses the whole request
before any mutation.

**Tech Stack:** Rust 1.90; `crates/sparql` (server + update path),
`crates/metrics`; no storage changes beyond what PLAN-28-04 delivers.

---

## Design (read this before any task)

### The slot's shape

Graph `https://horndb.io/graph/feed`, one subject
`https://horndb.io/graph/feed#slot`, predicates under
`https://horndb.io/ns/feed#`:

| Predicate | Object |
|---|---|
| `feed:id` | plain literal, verbatim consumer-supplied feed id |
| `feed:generation` | `xsd:integer` — **0 in P1** (the rebuild reset that increments it is P2; the quad exists so consumers parse one shape forever) |
| `feed:position` | plain literal, the opaque token **verbatim** (D2: never parsed, compared, or ordered by HornDB) |
| `feed:advancedAt` | `xsd:dateTime`, server clock at slot advance |

The write is always `dels = current slot quads, adds = new slot quads`
appended to the final operation's batch — `apply_quads`'s
dels-before-adds rule makes an identical replayed advance a clean
overwrite, and the whole thing rides one commit version with the last
data operation (D1). A request with zero operations but a position header
commits one slot-only batch. **No monotonicity check** (S2): HornDB stores
whatever token arrives; rewinds are the consumer's business (D3).

The reserved-namespace write closure from PLAN-28-04 lives at the SPARQL
verb layer (`validate_op`), so this store-layer write path is not blocked
by it — and `INSERT DATA { GRAPH <…/feed> … } }` remains an error, which
is exactly S2's "read-only through SPARQL, written by the request
parameter".

### The request surface

Headers on `POST /update` (chosen over query parameters: the update body
is often form-encoded and the values are opaque tokens; the spec left the
spelling to this plan):

```
X-HornDB-Feed-Id:       <opaque string, required with position>
X-HornDB-Feed-Position: <opaque string>
```

Rules (S2/S3, D6):

- Position without id → 400 naming both headers.
- Id present, non-empty slot with a **different** id → **409 Conflict**,
  body naming both ids; nothing is written (checked in preflight, before
  the first operation applies). Enforced at apply time; the "at startup"
  half of D6 is the consumer's reconciliation reading the slot.
- Empty slot adopts the first id it is given (S2).
- Headers absent → the update applies exactly as today, slot untouched
  (non-feed writers keep working; single-applier discipline is D8's
  assumption, not enforced beyond the id check).
- The slot advances **only after every operation committed** (S5) — the
  append-to-final-batch design gives this; an operation error or a crash
  mid-request leaves the slot where it was.

`SparqlError` gains a variant for the refusal
(`FeedIdMismatch { slot: String, request: String }`) so
`server/update.rs` can map it to 409 instead of the blanket 400
(`update.rs:48-51` today maps everything to 400 — this becomes a match).

### Reading and reconciliation

The consumer reads the slot with an ordinary query —
`SELECT ?p ?o WHERE { GRAPH <https://horndb.io/graph/feed> { ?s ?p ?o } }`
— which needs PLAN-28-03's ground `GRAPH` (reserved graphs are always
addressable by explicit name; PLAN-28-03 implements that rule).
Reconciliation (`min(H, C)`, generation handling) is **consumer-side by
design** (D3); what this plan ships is the S1 contract text in
`docs/ref/` -grade wording on the spec plus the metrics that make the
recovered state observable. No reconciliation code lands in HornDB.

### Metrics (S6)

New `crates/metrics/src/feed.rs` following the register pattern
(`incremental.rs:31-89` is the model), subsystem `feed`:
`applied_batches` (counter), `applied_quads` (counter, label
`op = add|del` — a new `label_enum!`), `last_apply_seconds` (gauge),
`generation` (gauge), `rebuild_in_progress` (gauge, registered now, set
only by P2 — 0 forever in P1, documented), `recovery_gap_seconds` (gauge,
set once at startup: 0 when no slot recovered, which on the P1 store is
always — the metric exists so the contract's observability is in place
before P3 makes it non-trivial). `docs/metrics.md` rows land in the same
commit (root sync rule). Emit sites: the slot-advance path and server
startup (`serve.rs`).

### What this plan deliberately does not do

- No rebuild-from-zero (P2, `#TODO` until filed), no checkpoint/WAL riding
  (P3/P4).
- No feed-emission surface (SPEC-30 non-goal).
- No kill −9 durability tests: on a store with no persistence a real
  kill-and-restart trivially recovers nothing; the meaningful P1 tests are
  crash-*simulation* at the apply layer (below). The full S1 property
  tests (spec acceptance 1–2) are written now against the contract surface
  and become load-bearing as P3/P4 give the slot something to survive in —
  they run over the in-memory store's "absent" answer today.

### File map

- Create: `crates/sparql/src/server/feed.rs` (slot read/build helpers) —
  or `crates/sparql/src/feed.rs` if the update path (non-server builds)
  needs it; the update path owns the append, so the module must not be
  `server`-feature-gated. Decide at Task 1: `crates/sparql/src/feed.rs`.
- Modify: `crates/sparql/src/update.rs` (position parameter through
  `apply_update_with`), `crates/sparql/src/server/update.rs` (headers,
  409), `crates/sparql/src/error.rs`, `crates/sparql/src/bin/serve.rs`
  (startup gauge)
- Create: `crates/metrics/src/feed.rs`; modify `crates/metrics/src/lib.rs`,
  `crates/metrics/src/labels.rs`, `docs/metrics.md`
- Create: `crates/sparql/tests/feed_slot.rs`
- Modify: `docs/architecture.md`, this plan

---

### Task 1: Slot module + apply-path integration

**Files:**
- Create: `crates/sparql/src/feed.rs`
- Modify: `crates/sparql/src/{update.rs,error.rs,lib.rs}`
- Create: `crates/sparql/tests/feed_slot.rs`

- [ ] **Step 1: Failing tests** (`feed_slot.rs`, both backends, driving
  `apply_update_with` directly with a
  `FeedPosition { id: String, position: String }` parameter):
  `slot_written_with_final_batch` (two-op update + position → slot quads
  present in the feed graph with the given id/token; data applied),
  `slot_advance_replaces_prior` (second update, new token → exactly one
  slot, new token), `identical_replay_is_clean` (same request + same
  position twice → data no-ops, slot unchanged, no error),
  `mismatched_feed_id_refuses_before_mutation` (slot holds id A; request
  with id B → `FeedIdMismatch` error naming both; **no data quad
  written**), `empty_slot_adopts_first_id`,
  `failing_op_leaves_slot_unadvanced` (fault-injecting backend wrapper
  erroring on the 2nd `apply_quads`; assert slot at old token — the S5
  crash-simulation), `zero_op_request_with_position_advances_slot`,
  `no_headers_means_no_slot` (plain update on a slotted store leaves the
  slot alone).
- [ ] **Step 2: Verify failure** — `cargo nextest run -p horndb-sparql
  feed_slot`.
- [ ] **Step 3: Implement** — `feed.rs`: the graph/subject/predicate
  constants, `read_slot(backend) -> Option<Slot>` (via
  `scan_graph_quads` on the feed graph), `slot_delta(old, new) ->
  (dels, adds)`; `apply_update_with` gains
  `feed: Option<&FeedPosition>` (existing callers pass `None` via the
  old-arity wrapper), preflight id check, append to final batch;
  `SparqlError::FeedIdMismatch`.
- [ ] **Step 4: Run** — `cargo nextest run -p horndb-sparql`.
- [ ] **Step 5: Commit** — `feat(sparql): applied-position slot — feed
  quads riding the final update batch (SPEC-30 S2/S5, #270)`.

### Task 2: HTTP surface

**Files:**
- Modify: `crates/sparql/src/server/update.rs`
- Modify: `crates/sparql/tests/server_http.rs`

- [ ] **Step 1: Failing tests** — `update_with_feed_headers_advances_slot`
  (POST with both headers; then a ground-`GRAPH` query reads the token
  back — this is also the S2 read-path acceptance),
  `position_without_id_is_400`, `feed_id_mismatch_is_409_naming_both`,
  `slot_survives_within_process` (two requests, second reads what the
  first wrote).
- [ ] **Step 2: Verify failure.**
- [ ] **Step 3: Implement** — header extraction in `handle_update`
  (`HeaderMap` is already a parameter, `server/update.rs:11-15`), the
  409 arm in the status match.
- [ ] **Step 4: Run** — `cargo nextest run -p horndb-sparql --features
  server`.
- [ ] **Step 5: Commit** — `feat(sparql): feed position headers on /update
  + 409 on feed-id mismatch (SPEC-30 S2/S3, #270)`.

### Task 3: Metrics

**Files:**
- Create: `crates/metrics/src/feed.rs`
- Modify: `crates/metrics/src/{lib.rs,labels.rs}`, `docs/metrics.md`,
  `crates/sparql/src/{feed.rs,bin/serve.rs}`

- [ ] **Step 1: Failing test** — encode-name assertions in `feed.rs`'s
  test module (the `incremental.rs:93-121` pattern) for all six series,
  including the `op` label variants.
- [ ] **Step 2: Verify failure; implement** the module + registration +
  emit sites (slot advance: batches/quads/last-apply; startup:
  recovery-gap = 0-when-absent, generation). Add the six rows to
  `docs/metrics.md` **in this commit**.
- [ ] **Step 3: Run** — `cargo nextest run -p horndb-metrics -p
  horndb-sparql --features server`.
- [ ] **Step 4: Commit** — `feat(metrics): horndb_feed_* series + docs
  rows (SPEC-30 S6, #270)`.

### Task 4: Contract property test + docs

**Files:**
- Modify: `crates/sparql/tests/feed_slot.rs`, `docs/architecture.md`,
  `docs/specs/SPEC-30-change-feed-materializer.md` (P1 wording only if
  reality diverged), this plan

- [ ] **Step 1:** Property test `position_never_overstates`: random op
  sequences with fault injection at random apply indices; invariant —
  whenever the slot holds token T for a request, every quad of that
  request's ops is present (the D5 one-sided guarantee at the only layer
  P1 can test it). Plus `docs/architecture.md`: SPEC-30 row → P1
  implemented (slot + contract; rebuild P2, durability riders P3/P4
  outstanding). Flip this plan's status.
- [ ] **Step 2:** Full verification — fmt, clippy `-D warnings`,
  `cargo nextest run --workspace`.
- [ ] **Step 3: Commit** — `test,docs(sparql): D5 position-never-overstates
  property + SPEC-30 P1 sync (#270)`.

---

## Self-review notes

- S2 coverage: contents → slot shape table (generation pinned 0 with the
  P2 pointer); atomicity → append-to-final-batch (T1); writing → headers
  (T2); reading → reserved-graph query (T2 test); no-monotonicity →
  verbatim store (T1); mismatch refusal → T1+T2. S3 is consumer-side by
  design — nothing to build, stated. S5's slot-advance-once → T1
  fault-injection test; the rest of S5 is PLAN-28-04's. S6 → T3.
- The spec's open item "where the slot physically lives before P3" is
  answered: as store quads, in memory with everything else — strictly
  conservative, no sidecar, D5-safe by construction.
- Honest limitation: real kill-and-restart adds nothing on a store with no
  persistence; the property tests target the contract surface and inherit
  teeth from P3/P4.
