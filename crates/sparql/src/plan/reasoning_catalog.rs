//! Read-only reasoning/materialization catalog seam (SPEC-23 §5.8).
//!
//! Parallel to `horndb-wcoj`'s [`Stats`](horndb_wcoj::stats::Stats) seam, and
//! read the same way: the optimizer asks, the catalog answers, nothing here
//! derives triples. It answers two questions about one triple pattern:
//!
//! 1. Are the inferred triples the pattern needs **already in the store**
//!    ([`ClosureState`])?
//! 2. What does it **cost** to resolve the pattern a given way
//!    ([`Strategy`]) — materialize the missing inferences, rewrite the pattern
//!    against the rules, or delegate to a resolver?
//!
//! A later pass compares those three costs and picks one. This module defines
//! the trait, its data types and [`UninformedCatalog`] — the deliberately
//! conservative fallback for when no real catalog is wired up.
//!
//! Not to be confused with [`crate::reasoning::catalog`], the SPEC-29
//! named-graph *view* catalog. That one tracks which views are stale and
//! re-derives them; this one is a planner input.
//!
//! **The costs here are stubs.** SPEC-23 §8 #4 (how a recursive fixpoint node
//! is costed on a scale built for non-recursive joins, and how much stays
//! opaque inside the closure operator) is unsettled, so no real cost model can
//! be written yet. Every [`Cost`] carries [`Cost::measured`] to say whether it
//! is a real number or a placeholder.

use crate::algebra::TriplePattern;

/// How much of what a pattern needs is already materialized in the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClosureState {
    /// Every inferred triple matching the pattern is already in the store, so
    /// a plain scan answers it in full.
    Closed,
    /// Some matching inferences are in the store, some are not. A plan must
    /// still derive the rest.
    Partial,
    /// No matching inferences are in the store — **or the catalog does not
    /// know**. The two fold into one variant on purpose: both mean "derive
    /// what you need", which is always sound, just possibly redundant work.
    NotClosed,
}

/// Where inferred triples come from when they are not already materialized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolver {
    /// A compiled OWL 2 RL rule firing (SPEC-04).
    CompiledRule,
    /// The GraphBLAS transitive-closure operator (SPEC-05).
    GraphblasClosure,
    /// Crosswalk / SKOS hierarchy expansion (SPEC-11).
    Crosswalk,
}

/// The three ways the optimizer can answer a pattern that needs reasoning.
/// They compete on cost; [`ReasoningCatalog::cost`] prices each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    /// Derive the missing inferences into the store first, then scan.
    Materialize,
    /// Rewrite the pattern itself (for example `?x a :C` into a UNION over
    /// `:C`'s subclasses) and answer from asserted triples.
    Rewrite,
    /// Hand the pattern to a specialized operator and use its output directly.
    Delegate(Resolver),
}

/// A cost on the SPEC-23 §5.5 additive scale — "rows touched", the same units
/// `horndb-wcoj`'s `CostModel` prices joins in, so reasoning and join costs
/// can be added.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cost {
    /// Estimated rows touched.
    pub rows: f64,
    /// `false` when `rows` is a placeholder rather than a number derived from
    /// statistics or measurement. A caller that costs on unmeasured numbers is
    /// costing on fiction — check this before comparing.
    pub measured: bool,
}

/// Read-only reasoning/materialization catalog. All methods are cheap lookups.
pub trait ReasoningCatalog: Send + Sync {
    /// Whether this catalog's answers carry real signal. When `false`, the
    /// costs are placeholders and the planner must keep its current behaviour
    /// instead of choosing between strategies — the same rule
    /// `Stats::is_informed` sets for join planning.
    fn is_informed(&self) -> bool {
        true
    }

    /// How much of what `pattern` needs is already materialized.
    fn closure_state(&self, pattern: &TriplePattern) -> ClosureState;

    /// Estimated cost of resolving `pattern` by `strategy`.
    fn cost(&self, pattern: &TriplePattern, strategy: Strategy) -> Cost;
}

/// Placeholder cost every [`UninformedCatalog`] answer carries. The value is
/// arbitrary and identical across strategies, so it cannot tip a comparison
/// on its own. A real number needs SPEC-23 §8 #4 settled first.
pub const STUB_COST_ROWS: f64 = 1.0;

/// The fallback catalog: nothing is known. Claims nothing is closed and prices
/// every strategy the same, both marked unmeasured, so a planner can never be
/// made worse by knowledge this catalog does not have.
pub struct UninformedCatalog;

impl ReasoningCatalog for UninformedCatalog {
    fn is_informed(&self) -> bool {
        false
    }

    /// Never claims a pattern is closed: claiming `Closed` wrongly drops
    /// answers, claiming `NotClosed` wrongly only costs redundant work.
    fn closure_state(&self, _pattern: &TriplePattern) -> ClosureState {
        ClosureState::NotClosed
    }

    fn cost(&self, _pattern: &TriplePattern, _strategy: Strategy) -> Cost {
        Cost {
            rows: STUB_COST_ROWS,
            measured: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algebra::{Term, Var};

    fn any_pattern() -> TriplePattern {
        TriplePattern {
            subject: Term::Var(Var::new("x")),
            predicate: Term::Iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#type".into()),
            object: Term::Iri("http://example.org/C".into()),
        }
    }

    #[test]
    fn uninformed_catalog_is_conservative() {
        let cat = UninformedCatalog;
        assert!(!cat.is_informed());
        // Never claims anything is already closed.
        assert_eq!(cat.closure_state(&any_pattern()), ClosureState::NotClosed);
    }

    #[test]
    fn uninformed_costs_are_stubs_and_cannot_tip_a_choice() {
        let cat = UninformedCatalog;
        let pat = any_pattern();
        let strategies = [
            Strategy::Materialize,
            Strategy::Rewrite,
            Strategy::Delegate(Resolver::CompiledRule),
            Strategy::Delegate(Resolver::GraphblasClosure),
            Strategy::Delegate(Resolver::Crosswalk),
        ];
        for s in strategies {
            let c = cat.cost(&pat, s);
            assert!(!c.measured, "{s:?} must be marked unmeasured");
            assert_eq!(
                c.rows, STUB_COST_ROWS,
                "{s:?} must not be cheaper than any other"
            );
        }
    }
}
