//! Core newtypes shared by the runtime and the generated rule code.

use std::fmt;

/// Dictionary-encoded RDF term identifier. Matches SPEC-02 `TermId` ABI
/// (64-bit, opaque to this crate).
#[derive(Copy, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct TermId(pub u64);

impl fmt::Debug for TermId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "T#{}", self.0)
    }
}

/// An RDF triple in subject–predicate–object order.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, PartialOrd, Ord)]
pub struct Triple {
    pub s: TermId,
    pub p: TermId,
    pub o: TermId,
}

impl Triple {
    pub const fn new(s: TermId, p: TermId, o: TermId) -> Self {
        Self { s, p, o }
    }
}

/// A resolved unqualified max-cardinality restriction (`cls-maxc1`/`cls-maxc2`).
///
/// `class` is the restriction class `?x` (`T(?x, owl:maxCardinality, n)` and
/// `T(?x, owl:onProperty, property)`); `max` is the cardinality value, which
/// the rules only act on for `0` and `1`. Resolved at load time in
/// `integration.rs` (where the dictionary can parse the literal value) and
/// fired by `list_rules.rs` in the semi-naïve loop.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct MaxCardRestriction {
    pub class: TermId,
    pub property: TermId,
    pub max: u8,
}

/// A slot inside a triple pattern: either a variable (referenced by index 0..=2)
/// or a constant term. Used by the codegen, not by the runtime hot path.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum Slot {
    Var(u8),
    Const(TermId),
}

/// A triple pattern used inside a rule body or head.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct Pattern {
    pub s: Slot,
    pub p: Slot,
    pub o: Slot,
}

/// Static rule identifier — the `id` field from `rules.toml`.
pub type RuleId = &'static str;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triple_equality_is_by_value() {
        let a = Triple::new(TermId(1), TermId(2), TermId(3));
        let b = Triple::new(TermId(1), TermId(2), TermId(3));
        assert_eq!(a, b);
    }

    #[test]
    fn slot_variants_distinct() {
        assert_ne!(Slot::Var(0), Slot::Const(TermId(0)));
    }

    #[test]
    fn max_card_restriction_fields() {
        let r = MaxCardRestriction {
            class: TermId(1),
            property: TermId(2),
            max: 1,
        };
        assert_eq!(r.max, 1);
        assert_ne!(r.class, r.property);
    }
}
