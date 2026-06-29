//! Integration: tick() emits per-tick Prometheus metrics.
//!
//! Setup copied from tests/circuit_tick.rs — fires a prp-trp-shape
//! rule over two asserted edges so at least one tick sample is recorded.

use horndb_incremental::{BilinearRule, Circuit, NaryPlan, RuleId, TripleId, Zset};

const P: u64 = 7;

struct PrpTrpOnP {
    id: RuleId,
}

impl BilinearRule for PrpTrpOnP {
    fn id(&self) -> RuleId {
        self.id
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

#[test]
fn tick_records_incremental_metrics() {
    let mut plan = NaryPlan::new();
    plan.push_join(Box::new(PrpTrpOnP { id: 1 }));

    let mut circuit = Circuit::new();
    circuit.add_plan(plan, RuleId::from(1u32));

    circuit.assert_triple((0, P, 1));
    circuit.assert_triple((1, P, 2));
    circuit.tick();

    let text = horndb_metrics::encode_metrics();
    assert!(
        text.contains("horndb_incremental_tick_duration_seconds"),
        "got:\n{text}"
    );
    assert!(
        text.contains("horndb_incremental_fixpoint_rounds"),
        "got:\n{text}"
    );
    // tick_duration histogram registers buckets at init, so assert a real sample:
    let n = parse_metric(&text, "horndb_incremental_tick_duration_seconds_count");
    assert!(n >= 1, "expected >= 1 tick sample, got {n}:\n{text}");
}

/// Parse a bare `name <value>` line from OpenMetrics text (skips `#` comment lines).
fn parse_metric(text: &str, name: &str) -> u64 {
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix(name) {
            if let Some(v) = rest.split_whitespace().next() {
                if let Ok(f) = v.parse::<f64>() {
                    return f as u64;
                }
            }
        }
    }
    0
}
