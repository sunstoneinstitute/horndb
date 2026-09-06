//! Logical IR (SPEC-23 §5.1): a resolved query plan distinct from
//! [`crate::plan::PhysicalPlan`]. The critical departure from Oxigraph
//! `sparopt`: the BGP is a **flat, n-ary** set of triple patterns
//! ([`LogicalPlan::Bgp`]) — the WCOJ unit — not a tree of binary joins.
//!
//! Smart constructors ([`LogicalPlan::join`] / [`filter`](LogicalPlan::filter)
//! / [`union`](LogicalPlan::union)) fold empty/identity/constant cases at
//! build time so passes can skip trivial shapes. Phase-1 lowering
//! (`crate::plan::lower`) deliberately does **not** call them — it builds raw
//! variants so the pipeline is the single, bisectable place transformations
//! happen — but the `CoalesceBgp` pass and later heuristic passes do.

use crate::algebra::{Aggregate, Expr, OrderDir, Term, TriplePattern, Var};
use crate::plan::GraphScope;

/// A logical query plan node.
#[derive(Debug, Clone, PartialEq)]
pub enum LogicalPlan {
    /// Flat, n-ary basic graph pattern — the WCOJ unit. `scope` is the
    /// graph the patterns read (SPEC-28 S3/D5), pushed down here by
    /// lowering.
    Bgp {
        patterns: Vec<TriplePattern>,
        scope: GraphScope,
    },
    /// Join of two non-BGP subtrees (adjacent `Bgp`s coalesce via
    /// [`LogicalPlan::join`] / the `CoalesceBgp` pass).
    Join {
        left: Box<LogicalPlan>,
        right: Box<LogicalPlan>,
    },
    /// Left-outer join with optional ON expression.
    LeftJoin {
        left: Box<LogicalPlan>,
        right: Box<LogicalPlan>,
        expr: Option<Expr>,
    },
    /// `MINUS` (SPARQL 1.1 §18.5): anti-join. Output is `left`'s schema —
    /// `right`'s columns never surface. See `crate::algebra::Algebra::Minus`.
    Minus {
        left: Box<LogicalPlan>,
        right: Box<LogicalPlan>,
    },
    /// Boolean filter.
    Filter { expr: Expr, inner: Box<LogicalPlan> },
    /// Union of two compatible subtrees.
    Union {
        left: Box<LogicalPlan>,
        right: Box<LogicalPlan>,
    },
    /// Restrict output columns.
    Project {
        vars: Vec<Var>,
        inner: Box<LogicalPlan>,
    },
    /// Deduplicate rows.
    Distinct { inner: Box<LogicalPlan> },
    /// OFFSET/LIMIT.
    Slice {
        inner: Box<LogicalPlan>,
        start: usize,
        length: Option<usize>,
    },
    /// ORDER BY.
    OrderBy {
        inner: Box<LogicalPlan>,
        keys: Vec<(Expr, OrderDir)>,
    },
    /// BIND.
    Extend {
        inner: Box<LogicalPlan>,
        var: Var,
        expr: Expr,
    },
    /// VALUES row source.
    Values {
        vars: Vec<Var>,
        rows: Vec<Vec<Option<Term>>>,
    },
    /// GROUP BY + aggregates.
    Group {
        inner: Box<LogicalPlan>,
        keys: Vec<Var>,
        aggregates: Vec<Aggregate>,
    },
    /// `GRAPH ?g { P }`: evaluate `inner` once per named graph with `var`
    /// free, then join `{var -> that graph}` onto that graph's rows
    /// (SPARQL 1.1 §18.2.2.2, SPEC-28 D6). Output columns are `inner`'s
    /// plus `var`.
    PerGraph { var: Var, inner: Box<LogicalPlan> },
    /// Recursive Kleene property path `p+`/`p*`.
    PathClosure {
        subject: Term,
        object: Term,
        edge: Box<LogicalPlan>,
        reflexive: bool,
    },
}

impl LogicalPlan {
    /// A `Bgp` reading the query's default graph — the shape lowering
    /// produces outside any `GRAPH` wrapper.
    pub fn bgp(patterns: Vec<TriplePattern>) -> LogicalPlan {
        LogicalPlan::Bgp {
            patterns,
            scope: GraphScope::DefaultGraph,
        }
    }

    /// Join two subtrees, coalescing **adjacent flat `Bgp`s into one flat
    /// `Bgp`** (SPEC-23 §5.1 — the inverse of `sparopt`'s flatten-and-rebuild,
    /// done once). Any non-`Bgp` operand keeps a real `Join`.
    ///
    /// Two `Bgp`s merge only when their graph scopes are **equal**: one flat
    /// pattern set is scanned in one graph, so merging
    /// `GRAPH <g1> { P1 } GRAPH <g2> { P2 }` would silently read both
    /// patterns from one graph (SPEC-28 S3).
    pub fn join(left: LogicalPlan, right: LogicalPlan) -> LogicalPlan {
        match (left, right) {
            (
                LogicalPlan::Bgp {
                    patterns: mut l,
                    scope: ls,
                },
                LogicalPlan::Bgp {
                    patterns: r,
                    scope: rs,
                },
            ) if ls == rs => {
                l.extend(r);
                LogicalPlan::Bgp {
                    patterns: l,
                    scope: ls,
                }
            }
            (left, right) => LogicalPlan::Join {
                left: Box::new(left),
                right: Box::new(right),
            },
        }
    }

    /// Filter, dropping a **constant-true** predicate (the filter is then a
    /// no-op and the child is returned directly).
    pub fn filter(expr: Expr, inner: LogicalPlan) -> LogicalPlan {
        if is_constant_true(&expr) {
            inner
        } else {
            LogicalPlan::Filter {
                expr,
                inner: Box::new(inner),
            }
        }
    }

    /// Union of two subtrees. (No fold in Phase 1 — the empty/identity cases
    /// need the lattice to be sound, so they land with the Phase-2 `Normalize`
    /// pass; the constructor exists now for a stable call site.)
    pub fn union(left: LogicalPlan, right: LogicalPlan) -> LogicalPlan {
        LogicalPlan::Union {
            left: Box::new(left),
            right: Box::new(right),
        }
    }
}

/// True iff `expr` is the constant boolean `true` in either of the crate's
/// two literal encodings: the quoted typed form is what the parser path
/// produces (oxrdf `Literal::to_string`, via `translate.rs`); the bare
/// `true` form is the internal runtime convention (`runtime.rs::bool_lit`,
/// used in binding values) that a future constant-folding pass may emit.
/// Misses are safe — an unfolded constant-true filter is just a no-op
/// operator — but keep this in sync with `runtime.rs`'s boolean parsing if
/// either encoding changes.
fn is_constant_true(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::Term(Term::Literal(s))
            if s == "true"
                || s == "\"true\"^^<http://www.w3.org/2001/XMLSchema#boolean>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pat(s: &str, p: &str, o: &str) -> TriplePattern {
        TriplePattern {
            subject: Term::Var(Var::new(s)),
            predicate: Term::Iri(p.to_owned()),
            object: Term::Var(Var::new(o)),
        }
    }

    fn bgp(pats: Vec<TriplePattern>) -> LogicalPlan {
        LogicalPlan::bgp(pats)
    }

    #[test]
    fn join_coalesces_adjacent_bgps_into_one_flat_bgp() {
        let left = bgp(vec![pat("s", "p", "o")]);
        let right = bgp(vec![pat("o", "q", "z")]);
        match LogicalPlan::join(left, right) {
            LogicalPlan::Bgp { patterns, .. } => assert_eq!(patterns.len(), 2),
            other => panic!("expected coalesced Bgp, got {other:?}"),
        }
    }

    /// Two BGPs under *different* `GRAPH` wrappers must stay separate: one
    /// flat pattern set is scanned in one graph (SPEC-28 S3).
    #[test]
    fn join_keeps_bgps_with_different_graph_scopes_apart() {
        use crate::algebra::GraphSpec;
        let left = bgp(vec![pat("s", "p", "o")]);
        let right = LogicalPlan::Bgp {
            patterns: vec![pat("o", "q", "z")],
            scope: GraphScope::Named(GraphSpec::Iri("http://ex/g".into())),
        };
        assert!(
            matches!(LogicalPlan::join(left, right), LogicalPlan::Join { .. }),
            "different scopes must not coalesce"
        );
    }

    #[test]
    fn join_keeps_a_real_join_when_a_side_is_not_a_bgp() {
        let left = bgp(vec![pat("s", "p", "o")]);
        let right = LogicalPlan::Project {
            vars: vec![Var::new("o")],
            inner: Box::new(bgp(vec![pat("o", "q", "z")])),
        };
        assert!(matches!(
            LogicalPlan::join(left, right),
            LogicalPlan::Join { .. }
        ));
    }

    #[test]
    fn filter_drops_constant_true() {
        let inner = bgp(vec![pat("s", "p", "o")]);
        let out = LogicalPlan::filter(Expr::Term(Term::Literal("true".into())), inner.clone());
        assert_eq!(out, inner, "constant-true filter must fold away");
    }

    #[test]
    fn filter_keeps_a_real_predicate() {
        let inner = bgp(vec![pat("s", "p", "o")]);
        let out = LogicalPlan::filter(Expr::Bound(Var::new("o")), inner);
        assert!(matches!(out, LogicalPlan::Filter { .. }));
    }
}
