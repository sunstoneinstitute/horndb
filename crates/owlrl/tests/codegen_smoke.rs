use horndb_owlrl::generated::{CompiledRule, RULES, RULE_COUNT};

#[test]
fn rules_were_generated() {
    assert_eq!(RULES.len(), RULE_COUNT);
    // Use the runtime `RULES.len()` rather than the `RULE_COUNT` const so
    // clippy doesn't fold this as a constant assertion — the value is set by
    // codegen at build time and we want a real lower-bound check on it.
    assert!(
        RULES.len() >= 25,
        "expected ≥25 Stage-1 rules, got {RULE_COUNT}"
    );
    let ids: Vec<&str> = RULES.iter().map(|r: &CompiledRule| r.id).collect();
    for required in ["cax-sco", "scm-eqc1", "prp-dom", "scm-sco", "eq-trans"] {
        assert!(ids.contains(&required), "missing rule {required}");
    }
}

#[test]
fn closure_delegated_rules_marked() {
    let delegated: Vec<&str> = RULES.iter().filter(|r| r.delegated).map(|r| r.id).collect();
    for required in [
        "eq-ref", "eq-sym", "eq-trans", "prp-trp", "scm-sco", "scm-spo",
    ] {
        assert!(
            delegated.contains(&required),
            "{required} should be closure-delegated"
        );
    }
}
