//! Verify that `materialize` / `reset_and_materialize` emit Prometheus metrics.

use horndb_owlrl::backend::RuleFiringBackend;
use horndb_owlrl::store::MemStore;
use horndb_owlrl::types::{TermId, Triple};
use horndb_owlrl::vocab::Vocabulary;
use horndb_owlrl::{materialize, reset_and_materialize};

fn t(s: u64, p: u64, o: u64) -> Triple {
    Triple::new(TermId(s), TermId(p), TermId(o))
}

#[test]
fn materialize_records_owlrl_metrics() {
    // Setup copied from tests/reset_rematerialize.rs — fires cax-sco (A ⊑ B ⊑
    // C, x : A → x : B, x : C) so at least one rule fires at least once.
    let v = Vocabulary::synthetic(10_000);
    let mut s = MemStore::new(v);
    s.assert(t(1, v.rdfs_sub_class_of.0, 2));
    s.assert(t(2, v.rdfs_sub_class_of.0, 3));
    s.assert(t(100, v.rdf_type.0, 1));
    s.assert(t(101, v.rdf_type.0, 2));
    let mut b = RuleFiringBackend::new();

    materialize(&mut s, &mut b);
    reset_and_materialize(&mut s, &mut b);

    let text = horndb_metrics::encode_metrics();
    assert!(
        text.contains("horndb_owlrl_rule_fires_total"),
        "got:\n{text}"
    );
    assert!(
        text.contains("horndb_owlrl_phase_duration_seconds"),
        "got:\n{text}"
    );
    assert!(text.contains("horndb_owlrl_rounds_total"), "got:\n{text}");
    let rounds = parse_counter(&text, "horndb_owlrl_rounds_total");
    assert!(
        rounds >= 1,
        "expected >= 1 round recorded, got {rounds}:\n{text}"
    );
}

/// Parse a bare `name <value>` counter line from OpenMetrics text.
fn parse_counter(text: &str, name: &str) -> u64 {
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix(name) {
            if let Some(v) = rest.split_whitespace().next() {
                if let Ok(n) = v.parse::<f64>() {
                    return n as u64;
                }
            }
        }
    }
    0
}
