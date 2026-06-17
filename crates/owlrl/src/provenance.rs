//! Per-inferred-triple proof annotation. Stage 1 keeps this in-memory only;
//! production proof recording (compressed side-table, on-demand rederivation)
//! is Future Work — see SPEC-04 F4.

use crate::types::{RuleId, Triple};
use smallvec::SmallVec;

#[derive(Clone, Eq, PartialEq, Debug)]
pub struct Provenance {
    pub rule_id: RuleId,
    pub premises: SmallVec<[Triple; 4]>,
}

impl Provenance {
    pub fn new(rule_id: RuleId, premises: impl IntoIterator<Item = Triple>) -> Self {
        Self {
            rule_id,
            premises: premises.into_iter().collect(),
        }
    }
}

/// A proof tree for a triple in the materialised store (SPEC-04 F4,
/// acceptance #5). Leaves are asserted (base) triples; internal nodes are
/// rule applications deriving the triple from its premises.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum ProofTree {
    /// A base (asserted) triple, or a triple with no recorded derivation —
    /// a leaf.
    Asserted(Triple),
    /// A derived triple: the rule that produced it and the proofs of its
    /// premises. `premises` is empty only for derivations whose backend
    /// records premises best-effort (e.g. the GraphBLAS closure backend).
    Derived {
        triple: Triple,
        rule_id: RuleId,
        premises: Vec<ProofTree>,
    },
    /// The triple is already being expanded higher in this branch — a
    /// derivation cycle (e.g. `eq-sym` ↔ `eq-sym`). Cut to keep the tree
    /// finite; the full single-level proof is still retrievable via
    /// [`crate::store::MemStore::proof`].
    Cycle(Triple),
}
