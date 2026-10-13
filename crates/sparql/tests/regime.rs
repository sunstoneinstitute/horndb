use horndb_sparql::regime::{
    owl_rl::MaterializedOwlRlRegime, simple::SimpleRegime, EntailmentRegime,
};

#[test]
fn regimes_are_distinguishable_by_name() {
    assert_eq!(SimpleRegime.name(), "simple");
    assert_eq!(
        MaterializedOwlRlRegime.name(),
        "http://www.w3.org/ns/entailment/OWL-RDF-Based"
    );
    assert_eq!(
        MaterializedOwlRlRegime.profile(),
        Some("http://www.w3.org/ns/owl-profile/RL")
    );
    assert_eq!(SimpleRegime.profile(), None);
}

#[test]
fn simple_is_default() {
    let d: Box<dyn EntailmentRegime> = Default::default();
    assert_eq!(d.name(), "simple");
}
