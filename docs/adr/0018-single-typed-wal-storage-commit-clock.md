# ADR-0018: One typed WAL; the storage commit version is the engine's logical clock

**Status:** Accepted

**Date:** 2026-07-19

**Source:** Joint E2/E3 design decision settling the open questions recorded on
both sides in `docs/specs/SPEC-24-incremental-stage2.md` (§S5, §S6, risks) and
`docs/specs/SPEC-25-storage-stage2.md` (§S1, §S3, risks).

## Context

Stage 2 gives HornDB two things that must agree on what "at time t" means and
on what survives a crash:

- **Storage** (SPEC-25) grows per-tuple MVCC visibility stamps (S1) and a
  write-ahead log (S3). Its clock today is `TierSnapshot.version: u64`, bumped
  once per committed batch under the single-writer lock.
- **The incremental circuit** (SPEC-24) grows a durable `DeltaLog` (S5) and
  storage-backed snapshots (S6). Its clock today is `LogicalTime`, bumped once
  per `tick()`. Its `DeltaLog` is an *input* log: user assert/retract records
  a future tick will drain — replaying it means re-running the computation.
  A storage WAL is an *outcome* log: committed batches — replaying it means
  re-applying values and stamps, no rule engine involved.

Both specs flagged the same two open questions and asked for a joint decision
before either side builds: one shared clock or a persisted mapping between the
two counters; one shared log or two coordinated ones. Two separate durable
logs would need a coordination protocol — each tick's storage commit recording
the input-log position it covers, exactly-once across a torn crash between two
fsyncs, two truncation schedules. That is a distributed-commit problem inside
one process.

## Decision

**One clock.** The storage commit version is the engine's authoritative
logical time. The load-bearing invariant: **one tick commits as exactly one
storage batch** — base delta, derived delta, and attribution together, at one
commit version. `snapshot.logical_time()`, the S1 begin/end visibility stamps,
and WAL record ordering all use this clock. The circuit may keep a tick
counter for diagnostics, but no persisted tick↔version mapping exists.
Non-circuit writers (bulk load, materialization sink, direct `Store` inserts)
bump the clock without a tick, so tick times are a subsequence of commit
versions — DBSP needs monotonicity, not contiguity. Mid-tick reads are
read-your-own-writes MVCC: the in-progress tick reads at `v+1` (its own
uncommitted stamps); everyone else reads pinned at `≤ v`. The in-memory-only
circuit (tests, no storage attached) keeps a trivial local clock behind the
same interface.

**One physical log, typed records, two replay roles.** SPEC-25 S3's WAL is
the single durable log; the SPEC-24 S5 `DeltaLog` contract is a thin layer
over it. Record types:

1. **`Input`** — a user assert/retract submitted to the circuit, durable on
   append (this satisfies S5's "append becomes a sequenced, durably-appended
   record" promise).
2. **`BaseBatch`** — a committed non-circuit write; replayed by value.
3. **`TickCommit`** — a tick boundary: the `Input` range it drained plus the
   committed base delta, stamped with its commit version.

Recovery: restore the checkpoint, value-replay committed batches, then
re-submit every `Input` record after the last `TickCommit` into a fresh tick.
Exactly-once holds by construction — an input past the last tick marker is,
by definition, un-ticked. One fsync point, one truncation schedule, one total
order that is also the clock.

**Deliberately left open** (settled by the S3 / SPEC-24 S5 implementation
plans with bench and crash-test evidence, not here): whether `TickCommit`
value-logs the *derived* delta. Logging it makes recovery rule-engine-free
but lets OWL RL amplification bloat the log; omitting it keeps the log small
but recovery must re-tick deterministically (sound — Z-set ordering is
deterministic by design — and bounded by the SPEC-06 F8 checkpoint cadence).
The default leans toward *not* logging derived deltas. The physical file
layout (single file vs. per-predicate-partition segments) likewise stays an
S3 plan decision.

## Consequences

+ "Snapshot at t" means the same thing in both layers with no mapping table
  to persist or crash-proof.
+ Tick durability is atomic and costs one fsync; no cross-log exactly-once
  protocol, no double-derive or lost-update window.
+ Storage recovery works without the rule engine for stores that never tick;
  circuit recovery recomputes only the small un-ticked input window.
− Every tick serializes through storage's single-writer commit path — nothing
  new in practice, since the concurrency contract is already
  concurrent-read / single-writer (SPEC-02, unchanged by SPEC-25).
− `horndb-incremental` gains a `horndb-storage` dependency for committed
  mode (allowed by the workspace dependency order).
− The tick-batch atomicity invariant constrains SPEC-25 S1's write-path
  design: whatever substrate S1 picks (stamp columns vs. delete bitmaps,
  CoW map vs. in-place append) must commit a tick's mixed batch atomically.

## Related

- Governing specs: `../specs/SPEC-25-storage-stage2.md` (S1, S3),
  `../specs/SPEC-24-incremental-stage2.md` (S5, S6).
- Tracking: SPEC-25 S1 [#225](https://github.com/sunstoneinstitute/horndb/issues/225),
  S3 [#227](https://github.com/sunstoneinstitute/horndb/issues/227);
  SPEC-24 S5 [#214](https://github.com/sunstoneinstitute/horndb/issues/214),
  S6 [#215](https://github.com/sunstoneinstitute/horndb/issues/215).
- Siblings: ADR-0008 (Z-set incremental maintenance), ADR-0009 (tiered
  columnar storage).
