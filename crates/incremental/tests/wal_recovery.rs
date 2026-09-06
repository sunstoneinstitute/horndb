//! SPEC-24 S5 / acceptance 5: a circuit whose input log is durable comes back
//! after a kill with the same Z-set state — the same asserted and derived
//! bases, and the same un-ticked inputs still pending.
//!
//! "Kill" here is `std::mem::forget`: no `Drop`, no flush, the same bytes a
//! SIGKILL would leave behind. It is the idiom the storage-side crash tests
//! use (`crates/storage/tests/wal_recovery.rs`).
//!
//! Tick boundaries are part of the state: `[+a, -a]` inside one tick derives
//! nothing, while the same pair split across two ticks derives and then
//! withdraws. The log's `TickCommit` markers carry that grouping, so replay
//! reproduces it (ADR-0018).

use std::sync::Arc;
use std::time::Duration;

use horndb_incremental::{
    BilinearRule, CheckpointPolicy, Circuit, NaryPlan, RuleId, TripleId, Zset,
};
use horndb_storage::Store;

const P: u64 = 7;

/// `(x P y), (y P z) ⇒ (x P z)` — enough rule to give the recovered circuit a
/// derived base that a retraction has to walk back.
struct TransitiveOnP;

impl BilinearRule for TransitiveOnP {
    fn id(&self) -> RuleId {
        1
    }
    fn apply_full(&self, a: &Zset<TripleId>, b: &Zset<TripleId>) -> Zset<TripleId> {
        let mut out = Zset::new();
        for ((xs, _, xo), ma) in a.iter() {
            for ((ys, _, yo), mb) in b.iter() {
                if xo == ys {
                    out.add((*xs, P, *yo), ma * mb);
                }
            }
        }
        out
    }
    fn apply_delta(
        &self,
        a: &Zset<TripleId>,
        b: &Zset<TripleId>,
        da: &Zset<TripleId>,
        db: &Zset<TripleId>,
    ) -> Zset<TripleId> {
        let mut out = self.apply_full(da, b);
        out.add_assign(&self.apply_full(a, db));
        out.add_assign(&self.apply_full(da, db));
        out
    }
}

fn new_circuit() -> Circuit {
    let mut plan = NaryPlan::new();
    plan.push_join(Box::new(TransitiveOnP));
    let mut circuit = Circuit::new();
    circuit.add_plan(plan, RuleId::from(1u32));
    circuit
}

fn durable_circuit(dir: &std::path::Path) -> Circuit {
    let mut circuit = new_circuit();
    circuit
        .attach_input_log(Arc::new(Store::open(dir).expect("open store")))
        .expect("attach input log");
    circuit
}

/// The input script up to the kill: six inputs across three ticks, then two
/// inputs appended and never ticked.
fn script(c: &mut Circuit) {
    c.assert_triple((0, P, 1));
    c.assert_triple((1, P, 2));
    c.tick();
    c.assert_triple((2, P, 3));
    // A second chain, untouched by the retraction below, so the recovered
    // derived base is non-empty either way.
    c.assert_triple((5, P, 6));
    c.assert_triple((6, P, 7));
    c.tick();
    c.retract_triple((1, P, 2));
    c.tick();
    // The window a crash must not lose: durable on append, never drained.
    c.assert_triple((3, P, 4));
    c.assert_triple((4, P, 5));
}

#[test]
fn kill_and_replay_reproduces_pre_crash_zset() {
    let dir = tempfile::tempdir().expect("tempdir");

    let mut before = durable_circuit(dir.path());
    script(&mut before);
    let asserted = before.asserted_base().clone();
    let derived = before.derived_base().clone();
    assert!(!derived.is_empty(), "the script must derive something");
    std::mem::forget(before); // kill

    let mut after = durable_circuit(dir.path());
    assert_eq!(after.recover(), 8, "6 ticked inputs plus the 2 un-ticked");
    assert_eq!(after.asserted_base(), &asserted);
    assert_eq!(after.derived_base(), &derived);

    // The un-ticked window came back as pending input, not as merged state.
    assert_eq!(after.tick().asserted_merged, 2);

    // ...and draining it lands where an uncrashed circuit lands.
    let mut oracle = new_circuit();
    script(&mut oracle);
    oracle.tick();
    assert_eq!(after.asserted_base(), oracle.asserted_base());
    assert_eq!(after.derived_base(), oracle.derived_base());
}

#[test]
fn checkpoint_drains_then_truncates_the_input_log() {
    let dir = tempfile::tempdir().expect("tempdir");

    let mut circuit = durable_circuit(dir.path());
    script(&mut circuit);
    circuit.checkpoint().expect("checkpoint");
    let asserted = circuit.asserted_base().clone();
    std::mem::forget(circuit);

    // No un-ticked input crossed the boundary, and the new log generation
    // starts empty.
    let reopened = Store::open(dir.path()).expect("reopen");
    assert!(!reopened.has_recovered_inputs());

    let mut oracle = new_circuit();
    script(&mut oracle);
    oracle.tick();
    assert_eq!(&asserted, oracle.asserted_base());
}

#[test]
fn delta_cadence_fires_a_checkpoint() {
    let dir = tempfile::tempdir().expect("tempdir");

    let mut circuit = durable_circuit(dir.path());
    circuit.set_checkpoint_policy(CheckpointPolicy {
        interval: Duration::from_secs(3600),
        deltas: 2,
    });
    circuit.assert_triple((0, P, 1));
    circuit.assert_triple((1, P, 2));
    circuit.tick(); // 2 asserted + 1 derived delta ≥ the limit
    std::mem::forget(circuit);

    let reopened = Store::open(dir.path()).expect("reopen");
    assert!(
        !reopened.has_recovered_inputs(),
        "the cadence should have checkpointed and truncated the log"
    );
}

#[test]
fn attach_input_log_rejects_a_store_with_no_log() {
    assert!(new_circuit()
        .attach_input_log(Arc::new(Store::in_memory()))
        .is_err());
}
