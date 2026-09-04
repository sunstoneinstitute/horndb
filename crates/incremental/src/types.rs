//! Public type aliases for SPEC-06 stage-1 interfaces.
//!
//! These names are referenced by SPEC-04 (rule codegen) and SPEC-02
//! (storage); changing them is a coordinated workspace change.

pub type TripleId = (u64, u64, u64);
pub type Multiplicity = i64;
pub type LogicalTime = u64;
pub type RuleId = u32;

/// `Ord` gives the change feed a deterministic net-publish order
/// (SPEC-24 S3): variants sort in declaration order, `RuleInferred` by rule id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DerivationKind {
    Asserted,
    RuleInferred(RuleId),
    ClosureInferred,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeltaRecord {
    pub triple: TripleId,
    pub mult: Multiplicity,
    pub time: LogicalTime,
    pub kind: DerivationKind,
}
