# `horndb-incremental` (SPEC-06) — agent notes

DBSP-style Z-set deltas, change feed, checkpointing.

- **Rule retraction is delta-incremental** (SPEC-24 S1, #210, `PLAN-24-01`):
  `Circuit::tick()` runs one unified incremental fixpoint; retraction ticks
  run a two-phase overdelete / re-derive pass driven by per-row per-rule
  weight traces (`rule_weights`). The Stage-1 full recompute survives only as
  a config-gated fallback (`Circuit::new_with_recompute_fallback()`) and as
  the differential-test oracle. Earlier increments: F6 recompute-and-diff
  (#45), F7 in-flight reader visibility (#46), closure-path retraction (#5).
  Treat the code as the source of truth for what currently works.

- **Reader snapshots ride storage MVCC** (SPEC-24 S6, #215, ADR-0018).
  `Circuit::attach_store(store, graphs)` binds the reader view to the store
  the S4 wiring writes (default graph + derived-mirror graph);
  `Circuit::snapshot()` then pins that store's current commit version in O(1)
  and returns `Some(Snapshot)`. The circuit materializes no presence set of
  its own. `Snapshot::logical_time()` **is** the storage commit version — one
  clock, no mapping. A circuit with no store attached returns `None`, which is
  the normal shape for unit tests and benches; assert against
  `asserted_base()` / `derived_base()` there.

- **The input log is durable behind the storage WAL** (SPEC-24 S5, #214,
  ADR-0018). `Circuit::attach_input_log(store)` turns every
  `assert_triple`/`retract_triple` into an `Input` record in the store's
  SPEC-25 S3 log and every tick into a `TickCommit` marker;
  `Circuit::recover()` replays what `Store::open` found — closed batches as
  their own ticks, the un-ticked tail as pending input. Attaching it arms the
  F8 checkpoint cadence (`CheckpointPolicy`, 1 min / 100K deltas), which
  persists the store and truncates the log. **A checkpoint truncates the
  input log**, so a driver that attaches one must have committed its tick
  outcomes to the store by then. A circuit with no input log behaves exactly
  as before — that is the shape unit tests and benches use.

- **The change feed nets per tick and bounds its subscribers** (SPEC-24 S3,
  #212): derived emissions accumulate in `Circuit::pending_derived` (keyed by
  `(triple, kind)`, fed by the single `emit_derived` funnel) and publish as
  non-zero nets at tick end; asserted records still publish per record. Route
  every new derived publish through `emit_derived` or it escapes the netting.
  `subscribe_bounded(capacity, LagPolicy)` is the consumer-facing subscribe —
  see `INTEGRATION-NOTES.md` for the API and the policy trade-off.

See `INTEGRATION-NOTES.md` for the consumer-facing contract, and
`FUTURE-WORK.md` and SPEC-06 for the retraction/MVCC roadmap. The
Stage-2 contract is `docs/specs/SPEC-24-incremental-stage2.md` (epic #186;
S1–S6 delivered, remaining phase sub-issues #216–#217): the join
runtime and the intra-tick joint fixpoint. The engine wiring (S4) lives on the consumer side,
`crates/sparql/src/exec/circuit.rs`.
