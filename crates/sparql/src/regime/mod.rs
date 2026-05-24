//! SPARQL 1.1 entailment regimes.
//!
//! In Stage 1 the regime is essentially a *marker* — the runtime does
//! not rewrite queries based on it. Both implementations execute the
//! same algebra against the same store; the distinction shows up in:
//!
//!  * the answer-set metadata (so clients know what regime ran),
//!  * the contract about what the underlying store must already
//!    contain (`MaterializedOwlRl` assumes SPEC-04/05 has already
//!    written the OWL 2 RL closure into the store; `Simple` makes no
//!    such assumption).
//!
//! Stage 2 will hang query-rewriting logic (e.g. backward-chained
//! mode) off the same trait.

pub mod owl_rl;
pub mod simple;

/// Top-level regime selector. Implementations are tiny — they only
/// hold the static W3C identifier — but the trait surface is the
/// integration point SPEC-04 will plug rule logic into in Stage 2.
pub trait EntailmentRegime: Send + Sync {
    /// Stable identifier: either `"simple"` or the W3C entailment
    /// regime IRI for OWL 2 RL.
    fn name(&self) -> &'static str;
}

impl Default for Box<dyn EntailmentRegime> {
    fn default() -> Self {
        Box::new(simple::SimpleRegime)
    }
}
