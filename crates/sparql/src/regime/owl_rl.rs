//! The materialized OWL 2 RL/RDF entailment regime.
//!
//! Stage 1 contract: the caller has already loaded the OWL 2 RL
//! closure into the underlying store (via SPEC-04/05). This regime
//! is therefore a marker — queries execute as plain BGPs against
//! the materialised store.
//!
//! In Stage 2, when SPEC-04 ships, this regime will also be the
//! mount point for the optional backward-chained mode (per
//! SPEC-07 F4, second bullet).

use super::EntailmentRegime;

#[derive(Debug, Default, Clone, Copy)]
pub struct MaterializedOwlRlRegime;

impl EntailmentRegime for MaterializedOwlRlRegime {
    fn name(&self) -> &'static str {
        // OWL 2 RL is a profile, not a regime. The W3C registry entry is
        // OWL-RDF-Based; the profile goes in `profile()`.
        "http://www.w3.org/ns/entailment/OWL-RDF-Based"
    }

    fn profile(&self) -> Option<&'static str> {
        Some("http://www.w3.org/ns/owl-profile/RL")
    }
}
