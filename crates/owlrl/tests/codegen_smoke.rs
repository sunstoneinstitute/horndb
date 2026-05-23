use reasoner_owlrl::generated::{RULE_COUNT, RULE_IDS};

#[test]
fn rules_were_generated() {
    assert!(
        RULE_COUNT >= 25,
        "expected ≥25 rules in Stage-1 subset, got {RULE_COUNT}"
    );
    assert_eq!(RULE_IDS.len(), RULE_COUNT);
    assert!(RULE_IDS.contains(&"cax-sco"));
    assert!(RULE_IDS.contains(&"scm-eqc1"));
    assert!(RULE_IDS.contains(&"prp-dom"));
}
