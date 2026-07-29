---
status: draft
date: 2026-07-28
scope: "SPEC-30 — what HornDB guarantees a change-feed consumer about applied state: the applied-position slot, startup reconciliation against an external cursor, rebuild-from-zero, the feed-level ordering contract, and the metrics a consumer monitors"
---

# SPEC-30 — Change-feed materializer contract

**One-line thesis:** A materializer applies a batch to HornDB, advances its own
cursor, and HornDB then crashes — and because HornDB's store is checkpoint-lossy
(SPEC-02 NF5), the applied batch is gone while the cursor says it landed. Replay
from that cursor never re-delivers it, so the divergence is permanent and silent.
This spec fixes that with the cheapest primitive that works: HornDB records an
**applied position** in the same store batch as the data, so position and data are
lost or survive together, and a consumer that resumes from the position HornDB
reports always converges.

**Refines:** nothing. **Consumes** SPEC-28 S6/D9 (store-boundary idempotent
quad-grain apply) and SPEC-28 S4 (each Update operation is its own store batch).
**Coordinates with** SPEC-28's `DROP ALL` decision — SPEC-28 owns the SPARQL verb,
this spec owns the operational reset procedure. **Consumed by** SPEC-29, whose P1
slice names rebuild-from-feed as its recovery story; this spec is what makes that
story real. **Upgraded by** SPEC-25 S4
([#228](https://github.com/sunstoneinstitute/horndb/issues/228)), S2
([#226](https://github.com/sunstoneinstitute/horndb/issues/226)) and S3
([#227](https://github.com/sunstoneinstitute/horndb/issues/227)) as they land —
see Phasing. **Tracking:**
[#263](https://github.com/sunstoneinstitute/horndb/issues/263).

## Problem — the correctness argument the upstream design cannot make here

The Sunstone data platform emits a change feed from a Postgres system of record:
one outbox row per commit, payload `{branch, adds: [[g,s,p,o]…], dels: [[…]]}`,
consumed by a single-replica materializer that holds its cursor in Postgres. The
Oxigraph materializer design (`2026-07-22-oxigraph-materializer-design.md`) states
the loop's correctness argument in its §6.3: **apply to the target first, then
advance the cursor**. If the process dies in between, the batch is re-read and
re-applied, and idempotent apply makes the replay a no-op.

That argument is sound, and it silently assumes one thing: *the target's applied
state is at least as durable as the cursor*. For RocksDB-backed Oxigraph that is
true. For HornDB it is false today:

- **SPEC-02 NF5** accepts losing every update since the last checkpoint.
- **The only checkpoint mechanism is an explicit snapshot export**, and
  `Store::export_snapshot` refuses a store holding named-graph data
  (`has_named_graph_data`, `crates/storage/src/store.rs`). Every graph in this
  workload is a named graph, so a HornDB deployment for this platform has **no
  checkpoint path at all** until SPEC-25 S4 (#228) lands.
- **There is no WAL.** SPEC-25 S3 (#227) is planned, not built.
- **The dictionary is in-memory only.** SPEC-25 S2 (#226) is planned, not built.

So the failure is concrete: the materializer applies rows 100–200, advances its
Postgres cursor to 200, HornDB crashes, HornDB restarts holding whatever the last
checkpoint held, the materializer resumes at 201, and rows 100–200 are missing
forever. Nothing in HornDB reports this. Every query after it returns a plausible
wrong answer.

The consumer cannot detect it either, because HornDB exposes no statement about
what it has applied. That missing statement — not the missing WAL — is the actual
gap this spec closes.

## Decisions

| # | Decision | Rationale in one line |
|---|---|---|
| D1 | HornDB stores an **applied-position slot** — feed id, generation, opaque position token, wall-clock — committed in the **same store batch** as the data it describes. | Position and data then share one durability fate by construction, whatever that fate currently is. |
| D2 | The position token is **opaque to HornDB**: stored and returned verbatim, never parsed, compared, or ordered. | Keeps the platform's `(xid8, seq)` cursor encoding out of HornDB, and lets the encoding change without touching HornDB. |
| D3 | **Reconciliation is the consumer's job.** HornDB reports its position honestly; the consumer decides what to replay. | The consumer is the only party that can order two positions (D2) and the only party that can read the feed. |
| D4 | The shipped answer to durability is **(c) the slot**, not (a) declare the store ephemeral, nor (b) pull SPEC-25 S2/S3/S4 onto the critical path. | The slot costs days, subsumes (a) as its degenerate case, and turns (b) into a quality upgrade rather than a correctness prerequisite. See "Why the slot". |
| D5 | HornDB **must never report a position ahead of the data that survived**. Reporting a position that is too old is always allowed. | This one-sided guarantee is what makes resume-and-replay safe; a position that overstates is exactly the original bug. |
| D6 | A slot whose **feed id** does not match the applying consumer's is a **refusal**, at startup and at apply time — never a silent overwrite. | An unrelated slot means the store belongs to a different feed; applying to it corrupts both. |
| D7 | **Rebuild-from-zero is a first-class store operation**, wider than SPARQL `DROP ALL`: it clears asserted quads, derived and reserved graphs, circuit state, and the slot, and bumps the generation. | A rebuild must reset everything derived at once; a verb that clears only what SPARQL can name leaves stale reasoner state behind. |
| D8 | HornDB assumes **exactly one feed applier**. One slot, single-writer. | Matches the upstream single-replica materializer (its D1). A second applier needs a keyed slot map; that is not this spec. |
| D9 | The slot is **read** through a reserved graph (`https://horndb.io/graph/feed`) and **written** as a parameter on the update request that carries the batch. | Reading needs no new API; writing must ride the same store batch as the data (D1), so it cannot be a separate SPARQL operation. |

### Why the slot — and what the other two answers cost

**(a) Declare the H1 store ephemeral.** Every restart is a rebuild-from-zero with
a consumer-side cursor reset. This costs HornDB nothing, and it is genuinely
defensible for a derived view: the upstream design says the same about Oxigraph's
own volume ("a lost volume is a recovery, not data loss", its D5), and
rebuild-from-feed is already the platform's stated recovery story.

Two things make it the wrong *shipped answer*. First, a full rebuild's cost grows
with the corpus, without bound — at 15 M asserted triples plus reasoning it is
minutes to tens of minutes, and it is paid on every restart, including a routine
redeploy. Second, it takes a permanent dependency on **outbox retention**, which
is a platform-side assumption HornDB has no control over: the upstream design
lists outbox pruning as a follow-up (its §10) and `consumed_at` as the hook for
it. The day that job ships, "replay the whole feed" stops being a recovery story
and becomes a data-loss story, and nothing in HornDB would notice.

**(b) Pull SPEC-25 S2 + S3 + S4 onto the critical path.** Persistent dictionary,
WAL, and named-graph snapshots make HornDB's applied state durable in the ordinary
sense, and the upstream §6.3 argument then applies unchanged. This is the right
end state and it is a large amount of work — three of SPEC-25's six phases, the
correctness-critical ones — to unblock a store the platform explicitly treats as
rebuildable. It is the upgrade path, not the entry price.

**(c) The slot, chosen.** The insight is that the failure is not "HornDB loses
data" — a derived view is allowed to lose data. The failure is "HornDB loses data
and the consumer's cursor still claims it landed". A position committed atomically
with the batch fixes exactly that, and nothing else, because the position can only
survive if the batch it describes survived.

It also **subsumes (a)**. On today's store, with no checkpoint and no WAL, the slot
recovers as absent, so reconciliation says "resume from the beginning" and the
restart *is* a rebuild-from-zero — but as a consequence of the contract rather
than as a special case, and reported rather than assumed. And it **stages (b)**:
each SPEC-25 phase that lands makes the recovered position more recent without
changing one word of the contract.

**When (b) becomes necessary.** Two triggers, either one sufficient:

1. **Rebuild time exceeds the recovery-time objective.** Measured, not guessed —
   acceptance 5 records full-replay wall-clock in `docs/benchmarks.md`. When that
   number exceeds the deployment's tolerated restart window, SPEC-25 S4 (#228)
   moves onto the critical path so a checkpoint bounds the replay tail.
2. **Outbox retention becomes finite.** The moment the platform ships an outbox
   pruning job, replay-from-zero stops being possible and a checkpoint is the only
   way to bound what must be retained. Then S4 first (a checkpoint the slot rides
   in), S2 + S3 after (a WAL that advances the slot per batch).

## Non-goals

- **Branches, tags, and as-of reads.** SPEC-25 S1 gives one commit clock and one
  lineage. The platform's branch model is its H2 horizon; no spec covers it.
- **Authentication and authorization.** Terminated upstream at `graph-server`;
  HornDB is cluster-internal (SPEC-07 F7, SPEC-28 non-goals).
- **HornDB *emitting* a durable change feed with external cursors.** HornDB is a
  feed **consumer** here. An emitted feed that other systems hold cursors into is
  the system-of-record horizon (H2) and needs its own spec: it requires durable
  commits, a stable external sequence, and retention — none of which the in-process
  `crates/incremental/src/change_feed.rs` provides. Nothing here should make that
  harder; nothing here delivers any of it.
- **The SPARQL surface.** `GRAPH`, `FROM`/`FROM NAMED`, named-graph Update, GSP,
  and the store-boundary idempotence rule are SPEC-28's. This spec consumes them.
- **What is reasoned over and where inferences land.** SPEC-29.
- **The materializer process itself.** The polling loop, the `(xid8, seq)`
  watermark, and the Postgres cursor table are owned by the data platform's own
  design. This spec defines only what that process may assume about HornDB.

## Requirements

### S1. The durability contract

State it so a materializer author can reason about restart without reading any
HornDB internals:

> **HornDB never reports an applied position ahead of the data that survived.**
> On startup, HornDB reports the position it recovered. Every batch at or before
> that position is present in full. Batches after it may be present, partly
> present, or absent — a consumer must assume absent. A consumer that resumes
> from the position HornDB reports, and replays forward, always converges to the
> correct state, whatever HornDB lost.

What the contract does **not** promise, said as plainly:

- **It does not promise the position is recent.** It may be arbitrarily far
  behind, down to "nothing applied". A consumer must always be able to replay
  from the position it is handed — including from the beginning.
- **It does not promise a batch after the position is absent.** A crash may leave
  a partly-applied batch behind. Replay repairs it because apply is idempotent at
  quad grain (SPEC-28 S6); nothing else does.
- **It does not promise readers see whole requests.** A multi-operation request
  commits one store batch per operation (SPEC-28 S4), so a concurrent reader can
  observe a state between two operations of one request. Acceptable for a derived
  view; recorded here so nobody assumes otherwise.
- **It does not promise anything to a second applier.** One slot, one writer (D8).

The contract holds at every phase below. What changes as SPEC-25 lands is only
*how far behind* the recovered position can be — never the guarantee itself.

### S2. The applied-position slot

- **Contents.** A **feed id** (opaque string identifying the feed and the SoR
  instance), a **generation** (u64, incremented by every rebuild reset, S4), an
  **opaque position token** (bytes, D2), and the **wall-clock time** the slot last
  advanced.
- **Atomicity.** Advancing the slot is part of the same store batch as the quads
  it describes (D1), at the same commit version. There is no ordering, no
  two-phase step, and no window in which one committed and the other did not.
- **Writing.** The consumer supplies the position with the update request that
  carries the batch — a request-level parameter on `/update` (header or query
  parameter; the implementation plan picks the exact spelling). The slot advances
  **only after every operation in the request has committed**. A request that
  fails, or dies part-way, leaves the slot where it was.
- **Reading.** The slot is exposed as quads in the reserved graph
  `https://horndb.io/graph/feed`, so a consumer reads it with an ordinary SPARQL
  query and no new API surface. It is read-only through SPARQL, on the same terms
  as SPEC-27's provenance view and SPEC-29's reserved graphs — the write path is
  the request parameter above, never `INSERT DATA`.
- **No monotonicity check.** HornDB stores whatever token it is given (D2). After
  a rewind the position legitimately moves backwards. Ordering positions is the
  consumer's business (D3).
- **Feed-id mismatch is a refusal (D6).** An update request carrying a feed id
  that differs from a non-empty slot's is an error naming both ids, at apply time
  as well as at startup. An empty slot adopts the first feed id it is given.

### S3. Startup reconciliation

Reconciliation runs in the **consumer**, using HornDB's reported position `H` and
its own cursor `C`. Three cases, and one rule that covers the first two.

**The rule: resume from `min(H, C)`, and replay forward.** Idempotent apply
(SPEC-28 S6) makes any overlap free, so replaying too much is always safe and
replaying too little never is.

1. **HornDB behind (`H < C`).** HornDB lost applied batches. This is the failure
   this spec exists for. The consumer rewinds `C` to `H` and replays. If `H` is
   older than the feed still retains, the only correct action is a full rebuild
   (S4) — say so loudly rather than resuming from the retention edge.
2. **HornDB ahead (`H > C`).** **This is normal, not an error.** Under
   apply-then-advance, a consumer crash between the two leaves HornDB holding the
   batch and the consumer's cursor pointing before it. The consumer replays from
   `C`; the overlap applies as no-ops with an affected count of zero, and the slot
   re-advances to the same token. The consumer must **not** fast-forward `C` to
   `H` — `C` is the authoritative resume point on the platform side, and `H` is a
   token the consumer wrote, not a claim about the feed.
   The genuinely impossible case is `H` **ahead by more than the in-flight
   window** — a position the consumer cannot locate in its feed at all. That means
   some other writer touched the store. Refuse to resume and require an explicit
   rebuild; do not guess.
3. **Unrelated (`feed id` mismatch).** A different feed, a rebuilt SoR, or a
   HornDB pointed at the wrong platform. `min(H, C)` is meaningless because the
   tokens are not comparable. HornDB refuses the first apply (D6) and the consumer
   must either point at the right store or perform an explicit rebuild-from-zero,
   which is the only operation that clears a feed id.

A **generation** mismatch (same feed id, different generation) means a rebuild
happened on one side and not the other. Treat it as case 1 with `H` = beginning:
the consumer resets its cursor to zero and replays.

### S4. Rebuild-from-zero as a sanctioned operation

Rebuild is the recovery story for every case above that resume-and-replay cannot
repair. It must be **one operation** that leaves nothing derived behind.

- **What it clears**, all of it, in one step:
  - every asserted quad, in the default graph and in every named graph;
  - every derived graph under the reserved `https://horndb.io/graph/` namespace —
    per-view inferred graphs, the spine-closure graph, and the view catalog
    (SPEC-29 D4);
  - provenance state (SPEC-27);
  - incremental circuit state — Z-set traces, the incremental-`distinct` weight
    traces, `rule_attr` attribution, and the `DeltaLog` (SPEC-24);
  - the applied-position slot, whose generation is then incremented and whose
    position becomes absent.
- **What it may keep.** The dictionary. Ids are append-only and never re-bind
  (SPEC-25 S2), so retaining interned terms across a rebuild is sound and saves
  the re-interning cost. Clearing it is also legal; the plan picks.
- **Restartable by construction.** A rebuild interrupted part-way leaves the slot
  absent, so the next startup reconciles to "resume from the beginning" and the
  rebuild simply continues. There is no half-rebuilt state that can be mistaken
  for a complete one.
- **Coordination with SPEC-28 (settled).** SPEC-28 defines `DROP ALL` as a *data*
  reset: it drops every non-reserved graph and the default graph quad by quad,
  and reserved graphs empty out only as the view circuits withdraw the derived
  triples of what was dropped. So `DROP ALL` is the SPARQL-expressible subset of
  this operation, and the store-level reset here is what a rebuild uses. The
  difference is not cosmetic: circuit state and the slot are not nameable as
  graphs, so no SPARQL verb can clear them.
- **Readiness.** Between the reset and the consumer's explicit
  rebuild-complete signal, the store reports **rebuild in progress** (S6). A store
  in that state is serving a partial view and should not be treated as current by
  anything downstream.

### S5. The feed-level ordering contract

SPEC-28 S4/B4 owns the store-batch rule: **each Update operation is its own store
batch**. This section says what a consumer may assume across a whole batch of feed
rows concatenated into one request
(`DELETE DATA{r1}; INSERT DATA{r1}; DELETE DATA{r2}; …`).

- **Operations apply in written order**, each as its own store batch. The final
  state of any quad is decided by the **last operation in the request that touched
  it** — per-quad last-writer-wins. A request that deletes a quad, re-adds it, and
  deletes it again ends with the quad absent.
- **Within one operation**, deletions apply before insertions (SPEC-28 S6), so a
  retract+insert pair of the same quad ends with it present. This rule is scoped
  to a single operation and never applied across a request — applying it
  request-wide would reorder a later delete behind an earlier insert and produce
  the wrong final state.
- **Replay produces the same final state as first application.** This requires
  last-writer-wins above **and** that the apply path performs no value
  normalization: quad identity is RDF term equality on the lexical form (SPEC-28
  B5). A store that rewrote `"01"^^xsd:integer` to `"1"` would not match the
  replayed `DELETE DATA` of the original form, and replay would stop converging.
- **The slot advances once, at the end.** A crash between operations leaves the
  slot unadvanced, so the consumer replays the whole request. Combined with
  last-writer-wins, this makes a partly-applied request self-repairing.
- **No cross-request ordering is provided.** Ordering across requests is the
  consumer's serialization, which D8 assumes is single-writer.
- **Blank nodes do not appear.** The platform skolemizes to IRIs at write time, so
  feed payloads carry none. A payload that did contain them would break replay
  convergence, because request-scoped blank nodes never equal stored ones.

### S6. What a consumer monitors

All of it goes through SPEC-17's existing registry (`crates/metrics/`) with rows
added to `docs/metrics.md` in the same commit, per the root sync rule. No parallel
mechanism. Names follow SPEC-17's `horndb_<subsystem>_<name>_<unit>` convention
with subsystem `feed`; counters are registered without the `_total` suffix the
scrape adds.

| Scraped name | Type | Meaning |
|---|---|---|
| `horndb_feed_applied_batches_total` | counter | update requests that advanced the slot |
| `horndb_feed_applied_quads_total` | counter, label `op` = `add`/`del` | quads applied, by direction |
| `horndb_feed_last_apply_seconds` | gauge | unix time of the last slot advance |
| `horndb_feed_generation` | gauge | slot generation; a change means a rebuild happened |
| `horndb_feed_rebuild_in_progress` | gauge | 1 between reset and rebuild-complete, else 0 |
| `horndb_feed_recovery_gap_seconds` | gauge | at startup: process start time minus the recovered slot's wall-clock. Zero when no slot recovered |

Two things this deliberately does not do:

- **Lag is not a HornDB metric.** Lag is the distance between HornDB's position
  and the feed head, and HornDB cannot see the feed head or order two positions
  (D2). The consumer computes lag — it holds both ends — and HornDB contributes
  the "time since last applied batch" half through `feed_last_apply_seconds`.
- **The position itself is not a metric.** It is opaque bytes, not a number. It is
  read from the reserved feed graph (S2), which is where a consumer or an operator
  looks for it.

`horndb_feed_recovery_gap_seconds` is the alerting signal for silent loss: it
bounds how much wall-clock of applied work the restart may have dropped. It
overstates by the downtime, which is fine for alerting and is stated at the metric.

## Phasing

Each slice is independently shippable. P1 is decomposed
([#270](https://github.com/sunstoneinstitute/horndb/issues/270),
`PLAN-30-01`); plans and tracking issues for P2–P4 are filed when each is
picked up (`#TODO` until they exist).

1. **P1 — the slot and the contract.** *(tracking:
   [#270](https://github.com/sunstoneinstitute/horndb/issues/270))* The slot (S2), the
   startup reconciliation surface (S3), the ordering contract made explicit and
   tested (S5), and the metrics (S6). On today's store the slot recovers as
   absent, so every restart reconciles to "resume from the beginning" — the
   contract holds, and the honest cost is visible in a metric instead of hidden in
   an assumption. Depends on SPEC-28 S6 (store-boundary idempotent quad apply),
   which is landable independently of the rest of SPEC-28.
2. **P2 — rebuild-from-zero as one operation.** *(tracking: `#TODO`)* The reset of
   S4, the rebuild-in-progress flag, and readiness. Coordinates with SPEC-28's B6
   decision on `DROP ALL`. Depends on P1 for the slot it resets.
3. **P3 — the slot rides the checkpoint.** *(tracking: `#TODO`)* Once SPEC-25 S4
   ([#228](https://github.com/sunstoneinstitute/horndb/issues/228)) gives
   quad-bearing stores a checkpoint, the slot becomes part of it. Restart replays
   only the tail since the last checkpoint, and the platform's retention obligation
   shrinks from "the whole outbox" to "back to the oldest checkpoint". Contract
   wording unchanged.
4. **P4 — the slot rides the WAL.** *(tracking: `#TODO`)* Once SPEC-25 S2
   ([#226](https://github.com/sunstoneinstitute/horndb/issues/226)) and S3
   ([#227](https://github.com/sunstoneinstitute/horndb/issues/227)) land, the slot
   advances durably per batch and the recovered position is at most one fsync
   window old. Contract wording unchanged again — only the bound tightens.

P1 stands alone and delivers the near-term target. P2 depends on P1. P3 and P4 are
pulled in by the triggers in "When (b) becomes necessary", not by a schedule.

## Acceptance criteria

1. **Kill-and-reconcile converges.** Apply N batches through the feed path, kill
   the process without a clean shutdown at a random point, restart, reconcile per
   S3, replay. The resulting store is quad-set equal — asserted quads and every
   derived graph — to a single clean application of the same feed. Run over many
   random kill points, not one.
2. **The position never overstates (D5).** A property test over randomized kill
   points asserts that every batch at or before the recovered position is present
   in full. A single counterexample is a release blocker, since this is the one
   guarantee the contract rests on.
3. **Ahead-by-one-batch is a no-op.** Simulate the consumer crashing after apply
   and before its cursor advance: replaying the last batch reports an affected
   quad count of zero for every quad, the slot ends at the same token, and the
   store is unchanged.
4. **Unrelated feeds are refused (D6).** A store whose slot carries feed id `A`
   refuses an update request carrying feed id `B`, with an error naming both, at
   apply time and at startup. No quad is written by the refused request. A
   generation mismatch resumes from the beginning rather than from `H`.
5. **Rebuild-from-zero is complete and measured (S4).** After a reset, a query for
   any asserted quad, any inferred graph, the spine-closure graph, the view
   catalog, and the slot all come back empty, and the circuit reports zero derived
   rows. A full replay afterwards reproduces the pre-reset store exactly, including
   inferred graphs. Full-replay wall-clock on the SPEC-29 acceptance-7 synthetic
   corpus is recorded in `docs/benchmarks.md` with the host noted — this is the
   number that triggers P3.
6. **Multi-operation ordering is last-writer-wins (S5).** A single request
   `DELETE DATA{q}; INSERT DATA{q}; DELETE DATA{q}` ends with `q` absent; replaying
   the identical request produces the same state; a crash injected between two
   operations leaves the slot unadvanced, and the replayed request converges.
   A literal in a non-canonical-but-valid lexical form survives an add/delete round
   trip unchanged (no value normalization).
7. **Monitoring exists and agrees with the code.** Every metric in S6 is emitted,
   `docs/metrics.md` carries a matching row, and
   `horndb_feed_recovery_gap_seconds` is non-zero after a kill-and-restart with a
   recovered slot and zero when no slot was recovered.

## Open items

- **Where the slot physically lives before P3.** With no checkpoint and no WAL,
  the slot has nowhere durable to go, so P1's slot is in-memory and always
  recovers as absent. Whether P1 nevertheless writes a tiny sidecar file — giving
  a *recent* position that the store's data cannot back — is a trap, not an
  option: it would violate D5. The plan must either keep the slot purely in-memory
  until P3 or make the sidecar strictly conservative.
- **The exact spelling of the write parameter (S2).** Header versus query
  parameter on `/update`. It is not a SPEC-26 S4 query-setting override — those
  are read-path settings — so it needs its own small decision in the P1 plan.
- **Rebuild verb ownership — resolved.** SPEC-28 settles `DROP ALL` as a data
  reset that does not clear reserved graphs directly, so the store-level reset in
  S4 is the rebuild primitive and `DROP ALL` is its SPARQL-expressible subset.
  Left here only because the P2 plan must expose the store-level reset through
  some operator-facing surface (admin endpoint, CLI flag, or startup mode), and
  that choice is not made yet.
- **Multiple appliers.** D8 assumes one. A second feed would need a slot keyed by
  feed id rather than a single slot. Cheap to add later, wrong to build now.
- **Rebuild-complete signalling.** S4 has the consumer signal completion so the
  readiness flag clears. Whether that is worth an API at all, or whether readiness
  can be inferred from lag falling below a threshold, is open — the flag is the
  simpler thing to ship first.
- **Reasoning rebuild cost dominates replay cost.** A rebuild re-applies the feed
  *and* re-derives every view (SPEC-29 P1 re-derives on spine change). Acceptance 5
  measures the pair together, because that is what a restart actually costs; if the
  derivation half dominates, the P3 trigger should be re-read as an argument for
  checkpointing derived graphs too, which SPEC-25 S4 covers for free since they are
  named graphs.
