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
