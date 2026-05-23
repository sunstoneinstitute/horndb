//! Schema closures for OWL 2 RL `scm-sco` (rdfs:subClassOf) and `scm-spo`
//! (rdfs:subPropertyOf). Both are **reflexive** transitive closures over
//! the extent of the matrix — every class is a subclass of itself, every
//! property is a sub-property of itself.

use crate::closure::transitive::{identity_like, transitive_closure};
use crate::error::GrbError;
use crate::grb::BoolMatrix;

/// `M* = I ∨ M⁺`. Reflexive transitive closure over `0..n`.
///
/// Use this for `rdfs:subClassOf` (`scm-sco`) and `rdfs:subPropertyOf`
/// (`scm-spo`). Do **not** use for general transitive properties
/// (`prp-trp`) — those use `transitive_closure` directly.
pub fn reflexive_transitive_closure(m: &BoolMatrix) -> Result<BoolMatrix, GrbError> {
    let mut closure = transitive_closure(m)?;
    let identity = identity_like(m)?;
    closure.or_assign(&identity)?;
    closure.wait()?;
    Ok(closure)
}
