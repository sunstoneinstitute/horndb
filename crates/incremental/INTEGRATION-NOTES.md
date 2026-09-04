# `horndb-incremental` — integration notes

What a consumer outside this crate needs to know. The first consumer is the
SPARQL engine (SPEC-24 S4, #213): `horndb-sparql`'s `exec::circuit` puts a
`Circuit` behind `HornBackend`'s write funnel and subscribes to its own feed
— see `crates/sparql/INTEGRATION-NOTES.md` ("SPEC-24 S4 — circuit wiring")
for the threading argument and the derived-row mirror. `ClosureRule` is
`Send` since then, so a `Circuit` can live inside a backend that is shared
behind a lock.

## Change-feed consumer API (SPEC-24 S3)

```rust
use horndb_incremental::{Circuit, ChangeFeedRx, DeltaRecord, LagPolicy};

// Bounded — what an engine consumer should use.
let rx: ChangeFeedRx = circuit.subscribe_bounded(1024, LagPolicy::DisconnectSlow);

// Unbounded — explicit opt-out from both bounding and backpressure.
let rx: ChangeFeedRx = circuit.subscribe();

let live: usize = circuit.subscriber_count();
```

`ChangeFeedRx` is a `crossbeam_channel::Receiver<DeltaRecord>`; a closed
channel means the circuit dropped you (see the policies below).

**Pick a `LagPolicy` deliberately.** It decides who pays when the consumer is
slower than the writer:

| Policy | On a full buffer | Cost |
|---|---|---|
| `DisconnectSlow` (default) | drops the subscriber, counts it on `incremental_change_feed_dropped_subscribers`, closes the receiver | consumer loses the stream and must resubscribe + resync from a snapshot |
| `Block` | blocks `Circuit::tick()` until the consumer drains | writers inherit the consumer's latency; a consumer that stops reading stalls the circuit |

`subscribe()` (unbounded) never drops and never blocks, and grows publisher
memory without limit if the consumer falls behind. Use it only in tests.

## What arrives on the feed

Per tick, in this order:

1. **Asserted records**, one per `assert_triple`/`retract_triple` call, in the
   caller's order, with the delta log's logical time. These are the user's own
   operations and are *not* netted — a re-assert of a live triple still
   publishes.
2. **Derived records**, netted over the whole tick and keyed by
   `(triple, kind)`: only non-zero nets publish, in key order, each with a
   fresh value from the circuit's separate derived clock. A row withdrawn and
   re-derived inside one tick (a closure replacement path) produces
   *nothing* — do not expect to see the intermediate states.

`TickReport::derived_merged` counts the net records published, so it matches
what a subscriber sees. SPEC-06 acceptance 5 ("every committed delta, in
order, no gaps or duplicates") is read over these *net* per-tick deltas.
The `horndb_incremental_derived_merged_total` metric is deliberately the
*raw* emission count instead — it is a tick-cost signal, not a feed count.

### Apply a tick by summing per triple — never record by record

**A record is a change to `(triple, kind)`, not to the triple.** The same
triple can appear twice in one tick under two kinds, and the two records are
published in `(triple, kind)` key order, which is not a causal order. The case
that bites: a row whose ownership moves from the closure to a rule publishes

```text
((s,p,o), RuleInferred(r))   +1     <-- comes first: RuleInferred < ClosureInferred
((s,p,o), ClosureInferred)   -1
```

A consumer that updates presence per record adds the triple, then **deletes a
live triple**. The only correct reading is to accumulate the whole tick and
sum the multiplicities per triple: present iff the total is > 0.

`DerivationKind`'s `Ord` — `Asserted` < `RuleInferred(id)` (by id) <
`ClosureInferred` — is **stable API**, since it fixes the publish order.
Reordering the variants is a breaking change for consumers.

### What `mult` can be

Per record, `mult` is always `+1` or `-1` (`flush_derived_feed` carries a
`debug_assert`). Per *triple* over a tick, the total is normally in
`{-1, 0, +1}`, but it can reach `-2`: `withdraw_derived_row` emits `-1`
unconditionally, so a row withdrawn by the closure retract pass and then again
by the rule fixpoint under a different kind totals `-2`. This is pre-existing
Stage-1 behaviour, kept as-is because the sum-per-triple rule above handles it
correctly (`total <= 0` ⇒ absent). Do not read the magnitude as a reference
count.

Two more consequences for a consumer:

- Deltas of a tick are only complete once `tick()` returns; there is no
  partial-tick visibility.
- The `time` field on a derived record orders derived records against each
  other, not against asserted records — the two use separate clocks.

## Other contracts

- `Circuit::snapshot()` gives a stable `(asserted ∪ derived)` read view that
  survives later ticks; readers and writers never block (SPEC-06 F7). Backing
  it onto SPEC-02 per-tuple MVCC is SPEC-24 S6 (#215).
- `DeltaLog` is in-memory. A crash between checkpoints loses pending deltas
  until the WAL lands (SPEC-24 S5, #214) — see `docs/specs/SPEC-30-change-feed-materializer.md` for
  the durability contract a feed consumer needs.
