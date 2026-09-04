# `horndb-incremental` — integration notes

What a consumer outside this crate needs to know. The crate has no consumers
yet; engine wiring is SPEC-24 S4 (#213).

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

Two consequences for a consumer:

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
