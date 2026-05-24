//! The default SPARQL 1.1 "simple" entailment regime — no inference.
//! Used for the W3C SPARQL 1.1 Query Test Suite.

use super::EntailmentRegime;

#[derive(Debug, Default, Clone, Copy)]
pub struct SimpleRegime;

impl EntailmentRegime for SimpleRegime {
    fn name(&self) -> &'static str {
        "simple"
    }
}
