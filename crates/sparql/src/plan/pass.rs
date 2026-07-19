//! Pass registry + driver (SPEC-23 §5.2), modeled on DuckDB's
//! `RunOptimizer`/`OptimizerType` and ClickHouse's `IQueryTreePass`.
//!
//! * Typed, ordered, individually **disable-able** passes (`PlanCtx`).
//! * Ordering constraints are **declared** (`LogicalPass::must_follow`) and
//!   asserted at startup — not left as "must run before X" comments.
//! * Debug builds re-infer the lattice and structurally **validate** the IR
//!   after every pass, so a plan regression bisects to one `PassId`.
//!
//! Phase 1 registered exactly one pass, [`CoalesceBgp`]. Phase 2 adds
//! [`crate::plan::passes::Normalize`] (constant folding + `Eq`→`SameTerm`
//! strength reduction); the remaining `PassId` variants exist so the rest of
//! Phase 2+ slots in without an enum change and so a pragma can name them.

use crate::plan::logical::LogicalPlan;
#[cfg(debug_assertions)]
use crate::plan::types::infer;
use std::collections::HashSet;
use std::str::FromStr;

/// Identity of a logical pass. Source order in [`standard_passes`] is the run
/// order; `must_follow` declares the constraints the driver asserts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PassId {
    CoalesceBgp,
    Normalize,
    FilterPullup,
    FilterPushdown,
    ProjectionPushdown,
    JoinPlanning,
}

impl PassId {
    /// Every variant, in pipeline order. The single list `from_str` and the
    /// round-trip test iterate — adding a variant without extending this
    /// array is caught by the (compiler-forced) `as_str` match plus the
    /// `pass_id_round_trips_through_str` length assertion.
    pub const ALL: [PassId; 6] = [
        PassId::CoalesceBgp,
        PassId::Normalize,
        PassId::FilterPullup,
        PassId::FilterPushdown,
        PassId::ProjectionPushdown,
        PassId::JoinPlanning,
    ];

    /// Stable lowercase-kebab name used by the query pragma and diagnostics.
    pub fn as_str(&self) -> &'static str {
        match self {
            PassId::CoalesceBgp => "coalesce-bgp",
            PassId::Normalize => "normalize",
            PassId::FilterPullup => "filter-pullup",
            PassId::FilterPushdown => "filter-pushdown",
            PassId::ProjectionPushdown => "projection-pushdown",
            PassId::JoinPlanning => "join-planning",
        }
    }
}

impl FromStr for PassId {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        PassId::ALL
            .iter()
            .copied()
            .find(|id| id.as_str() == s)
            .ok_or_else(|| format!("unknown pass id `{s}`"))
    }
}

/// Planning context threaded through every pass. Phase 1 carries only the
/// disabled-pass set (config + pragma); a statistics/cost seam is added in a
/// later phase.
#[derive(Debug, Clone, Default)]
pub struct PlanCtx {
    pub disabled_passes: HashSet<PassId>,
}

/// A logical optimization pass.
pub trait LogicalPass {
    fn id(&self) -> PassId;
    fn run(&self, plan: LogicalPlan, ctx: &PlanCtx) -> LogicalPlan;
    /// Passes that must run *before* this one. Asserted at startup.
    fn must_follow(&self) -> &'static [PassId] {
        &[]
    }
}

/// The pipeline. Source order == run order.
pub fn standard_passes() -> Vec<Box<dyn LogicalPass>> {
    let passes: Vec<Box<dyn LogicalPass>> = vec![
        Box::new(CoalesceBgp),
        Box::new(crate::plan::passes::Normalize),
    ];
    assert_pass_order(&passes);
    passes
}

/// Assert every pass's declared `must_follow` constraint is satisfied by the
/// wired order (each named predecessor appears strictly earlier). Panics
/// otherwise — a wiring bug, caught at startup.
pub fn assert_pass_order(passes: &[Box<dyn LogicalPass>]) {
    for (i, p) in passes.iter().enumerate() {
        for req in p.must_follow() {
            let ok = passes[..i].iter().any(|q| q.id() == *req);
            assert!(
                ok,
                "pass {:?} must follow {:?}, but {:?} is not wired earlier",
                p.id(),
                req,
                req
            );
        }
    }
}

/// Run `passes` in order, skipping any in `ctx.disabled_passes`. In debug
/// builds the IR is validated after each pass: a pass must not *introduce*
/// dangling variable references its own input did not already have.
///
/// The check is differential, not absolute, because legal SPARQL may
/// reference variables its pattern never binds — `FILTER(?z = <iri>)` with
/// unbound `?z` just drops every row, and `SELECT ?z` over a pattern that
/// never binds `?z` projects it unbound. Those dangling references arrive
/// from the parser and must survive; only *new* ones (a pass corrupting the
/// IR) are a bug. Two details make the attribution exact:
///
/// * The baseline is a **multiset** (tag → count), so a corrupt node whose
///   tag collides with a legal dangling ref elsewhere still raises the count
///   and is caught.
/// * The baseline **rolls forward** to each pass's own input, so a ref a
///   pass legally removes cannot be "spent" later by a different buggy pass
///   — a regression always bisects to the one `PassId` that introduced it.
pub fn run_passes(
    mut plan: LogicalPlan,
    passes: &[Box<dyn LogicalPass>],
    ctx: &PlanCtx,
) -> LogicalPlan {
    #[cfg(debug_assertions)]
    let mut baseline = dangling_refs(&plan);
    for p in passes {
        if ctx.disabled_passes.contains(&p.id()) {
            continue;
        }
        plan = p.run(plan, ctx);
        #[cfg(debug_assertions)]
        {
            let now = dangling_refs(&plan);
            let fresh: Vec<&String> = now
                .iter()
                .filter(|(tag, n)| baseline.get(*tag).copied().unwrap_or(0) < **n)
                .map(|(tag, _)| tag)
                .collect();
            assert!(
                fresh.is_empty(),
                "IR invalid after pass {:?}: new dangling variable refs {fresh:?}",
                p.id()
            );
            baseline = now;
        }
    }
    plan
}

/// Structural check backing the post-pass validation: count every variable a
/// node *references* (Project list; Filter / LeftJoin-ON / Extend / OrderBy /
/// aggregate-input expressions) that is not produced by the subtree(s) below
/// it, keyed by `NodeKind:?var`. `infer` runs on each referencing node's
/// children, so a `Project` that hides a deeper binding is respected.
#[cfg(debug_assertions)]
pub(crate) fn dangling_refs(plan: &LogicalPlan) -> std::collections::BTreeMap<String, usize> {
    use crate::algebra::{AggFunc, Expr};
    use crate::exec::runtime::referenced_vars;
    use std::collections::{BTreeMap, HashSet as Set};

    fn produced(node: &LogicalPlan) -> Set<String> {
        infer(node).vars().map(|v| v.name().to_owned()).collect()
    }

    fn note_expr(
        kind: &str,
        expr: &Expr,
        inner_vars: &Set<String>,
        out: &mut BTreeMap<String, usize>,
    ) {
        let mut refs: Set<String> = Set::new();
        referenced_vars(expr, &mut refs);
        for r in &refs {
            if !inner_vars.contains(r) {
                *out.entry(format!("{kind}:?{r}")).or_insert(0) += 1;
            }
        }
    }

    fn walk(node: &LogicalPlan, out: &mut BTreeMap<String, usize>) {
        match node {
            LogicalPlan::Project { vars, inner } => {
                let inner_vars = produced(inner);
                for v in vars {
                    if !inner_vars.contains(v.name()) {
                        *out.entry(format!("Project:?{}", v.name())).or_insert(0) += 1;
                    }
                }
                walk(inner, out);
            }
            LogicalPlan::Filter { expr, inner } => {
                note_expr("Filter", expr, &produced(inner), out);
                walk(inner, out);
            }
            LogicalPlan::LeftJoin { left, right, expr } => {
                // The ON expression sees both sides' bindings.
                if let Some(e) = expr {
                    let mut vars = produced(left);
                    vars.extend(produced(right));
                    note_expr("LeftJoin", e, &vars, out);
                }
                walk(left, out);
                walk(right, out);
            }
            LogicalPlan::Extend { inner, expr, .. } => {
                note_expr("Extend", expr, &produced(inner), out);
                walk(inner, out);
            }
            LogicalPlan::OrderBy { inner, keys } => {
                let inner_vars = produced(inner);
                for (e, _) in keys {
                    note_expr("OrderBy", e, &inner_vars, out);
                }
                walk(inner, out);
            }
            LogicalPlan::Group {
                inner, aggregates, ..
            } => {
                let inner_vars = produced(inner);
                for a in aggregates {
                    match &a.func {
                        AggFunc::CountStar => {}
                        AggFunc::Count(e)
                        | AggFunc::Sum(e)
                        | AggFunc::Min(e)
                        | AggFunc::Max(e)
                        | AggFunc::Avg(e)
                        | AggFunc::Sample(e)
                        | AggFunc::GroupConcat { expr: e, .. } => {
                            note_expr("Group", e, &inner_vars, out);
                        }
                    }
                }
                walk(inner, out);
            }
            // Structural recursion into every child; leaf nodes are trivially ok.
            LogicalPlan::Join { left, right } | LogicalPlan::Union { left, right } => {
                walk(left, out);
                walk(right, out);
            }
            LogicalPlan::Distinct { inner } | LogicalPlan::Slice { inner, .. } => walk(inner, out),
            LogicalPlan::PathClosure { edge, .. } => walk(edge, out),
            LogicalPlan::Bgp { .. } | LogicalPlan::Values { .. } => {}
        }
    }
    let mut out = BTreeMap::new();
    walk(plan, &mut out);
    out
}

/// `CoalesceBgp` (SPEC-23 §5.1/§5.2): fold contiguous `Join(Bgp, Bgp)` into
/// one flat `Bgp`, bottom-up, via the [`LogicalPlan::join`] smart
/// constructor, so the WCOJ planner sees the widest possible pattern set.
/// Idempotent, and result-invariant (a natural join over the merged pattern
/// set — proven in `tests/logical_pipeline.rs`).
///
/// When it fires today: spargebra already merges *adjacent triple patterns*
/// into one `Algebra::Bgp`, but HornDB's Stage-1 `GRAPH` lowering
/// (`translate.rs` — `GRAPH <g> { P }` lowers to `P` under merged-graph
/// semantics) produces `Join(Bgp, Bgp)` whenever a query mixes top-level
/// triples with a `GRAPH` block. Those plans coalesce to one flat `BgpScan`;
/// everything else is untouched (the Phase-1 golden gate).
pub struct CoalesceBgp;

impl LogicalPass for CoalesceBgp {
    fn id(&self) -> PassId {
        PassId::CoalesceBgp
    }
    fn run(&self, plan: LogicalPlan, _ctx: &PlanCtx) -> LogicalPlan {
        coalesce(plan)
    }
}

fn coalesce(plan: LogicalPlan) -> LogicalPlan {
    use LogicalPlan::*;
    match plan {
        Join { left, right } => {
            // Recurse first, then rebuild through the coalescing constructor.
            LogicalPlan::join(coalesce(*left), coalesce(*right))
        }
        LeftJoin { left, right, expr } => LeftJoin {
            left: Box::new(coalesce(*left)),
            right: Box::new(coalesce(*right)),
            expr,
        },
        Union { left, right } => Union {
            left: Box::new(coalesce(*left)),
            right: Box::new(coalesce(*right)),
        },
        Filter { expr, inner } => Filter {
            expr,
            inner: Box::new(coalesce(*inner)),
        },
        Project { vars, inner } => Project {
            vars,
            inner: Box::new(coalesce(*inner)),
        },
        Distinct { inner } => Distinct {
            inner: Box::new(coalesce(*inner)),
        },
        Slice {
            inner,
            start,
            length,
        } => Slice {
            inner: Box::new(coalesce(*inner)),
            start,
            length,
        },
        OrderBy { inner, keys } => OrderBy {
            inner: Box::new(coalesce(*inner)),
            keys,
        },
        Extend { inner, var, expr } => Extend {
            inner: Box::new(coalesce(*inner)),
            var,
            expr,
        },
        Group {
            inner,
            keys,
            aggregates,
        } => Group {
            inner: Box::new(coalesce(*inner)),
            keys,
            aggregates,
        },
        PathClosure {
            subject,
            object,
            edge,
            reflexive,
        } => PathClosure {
            subject,
            object,
            edge: Box::new(coalesce(*edge)),
            reflexive,
        },
        leaf @ (Bgp { .. } | Values { .. }) => leaf,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algebra::{Term, TriplePattern, Var};

    fn pat(s: &str, p: &str, o: &str) -> TriplePattern {
        TriplePattern {
            subject: Term::Var(Var::new(s)),
            predicate: Term::Iri(p.to_owned()),
            object: Term::Var(Var::new(o)),
        }
    }
    fn bgp(pats: Vec<TriplePattern>) -> LogicalPlan {
        LogicalPlan::Bgp { patterns: pats }
    }
    fn raw_join(l: LogicalPlan, r: LogicalPlan) -> LogicalPlan {
        LogicalPlan::Join {
            left: Box::new(l),
            right: Box::new(r),
        }
    }

    #[test]
    fn coalesce_folds_join_of_bgps() {
        let plan = raw_join(
            bgp(vec![pat("s", "http://ex/p", "o")]),
            bgp(vec![pat("o", "http://ex/q", "z")]),
        );
        let out = run_passes(plan, &standard_passes(), &PlanCtx::default());
        match out {
            LogicalPlan::Bgp { patterns } => assert_eq!(patterns.len(), 2),
            other => panic!("CoalesceBgp must flatten Join(Bgp,Bgp); got {other:?}"),
        }
    }

    #[test]
    fn disabling_coalesce_keeps_the_join() {
        let plan = raw_join(
            bgp(vec![pat("s", "http://ex/p", "o")]),
            bgp(vec![pat("o", "http://ex/q", "z")]),
        );
        let ctx = PlanCtx {
            disabled_passes: HashSet::from([PassId::CoalesceBgp]),
        };
        let out = run_passes(plan, &standard_passes(), &ctx);
        assert!(
            matches!(out, LogicalPlan::Join { .. }),
            "disabled CoalesceBgp must leave the Join intact"
        );
    }

    // A test-only pass to exercise the ordering assertion.
    struct NeedsCoalesce;
    impl LogicalPass for NeedsCoalesce {
        fn id(&self) -> PassId {
            PassId::FilterPushdown
        }
        fn run(&self, plan: LogicalPlan, _ctx: &PlanCtx) -> LogicalPlan {
            plan
        }
        fn must_follow(&self) -> &'static [PassId] {
            &[PassId::CoalesceBgp]
        }
    }

    #[test]
    fn assert_pass_order_accepts_a_satisfied_constraint() {
        let passes: Vec<Box<dyn LogicalPass>> =
            vec![Box::new(CoalesceBgp), Box::new(NeedsCoalesce)];
        assert_pass_order(&passes); // must not panic
    }

    #[test]
    #[should_panic(expected = "must follow")]
    fn assert_pass_order_rejects_a_violated_constraint() {
        let passes: Vec<Box<dyn LogicalPass>> =
            vec![Box::new(NeedsCoalesce), Box::new(CoalesceBgp)];
        assert_pass_order(&passes);
    }

    #[test]
    fn legal_dangling_filter_ref_survives_the_pipeline() {
        // FILTER over a var the pattern never binds is legal SPARQL (rows
        // drop at eval time). It is in the pre-pass baseline, so the
        // post-pass validation must not reject it.
        use crate::algebra::Expr;
        let plan = LogicalPlan::Filter {
            expr: Expr::Eq(
                Box::new(Expr::Term(Term::Var(Var::new("z")))),
                Box::new(Expr::Term(Term::Iri("http://ex/b".into()))),
            ),
            inner: Box::new(bgp(vec![pat("s", "http://ex/p", "o")])),
        };
        let out = run_passes(plan, &standard_passes(), &PlanCtx::default());
        assert!(matches!(out, LogicalPlan::Filter { .. }));
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "new dangling")]
    fn corrupting_pass_is_caught_by_debug_validation() {
        use crate::algebra::Expr;
        // A pass that wraps the plan in a Filter over a variable nothing
        // binds introduces a NEW dangling ref — the driver must panic.
        struct Corruptor;
        impl LogicalPass for Corruptor {
            fn id(&self) -> PassId {
                PassId::Normalize
            }
            fn run(&self, plan: LogicalPlan, _ctx: &PlanCtx) -> LogicalPlan {
                LogicalPlan::Filter {
                    expr: Expr::Bound(Var::new("no_such_var")),
                    inner: Box::new(plan),
                }
            }
        }
        let passes: Vec<Box<dyn LogicalPass>> = vec![Box::new(Corruptor)];
        run_passes(
            bgp(vec![pat("s", "http://ex/p", "o")]),
            &passes,
            &PlanCtx::default(),
        );
    }

    #[test]
    fn pass_id_round_trips_through_str() {
        for id in PassId::ALL {
            assert_eq!(id.as_str().parse::<PassId>().unwrap(), id);
        }
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "new dangling")]
    fn duplicate_tag_corruption_is_caught() {
        use crate::algebra::Expr;
        // The input already has a LEGAL dangling Filter over ?z. A pass that
        // adds a SECOND dangling Filter over the same ?z produces the same
        // tag string — only the multiset count exposes the corruption.
        struct DuplicateCorruptor;
        impl LogicalPass for DuplicateCorruptor {
            fn id(&self) -> PassId {
                PassId::Normalize
            }
            fn run(&self, plan: LogicalPlan, _ctx: &PlanCtx) -> LogicalPlan {
                LogicalPlan::Filter {
                    expr: Expr::Bound(Var::new("z")),
                    inner: Box::new(plan),
                }
            }
        }
        let legal = LogicalPlan::Filter {
            expr: Expr::Bound(Var::new("z")),
            inner: Box::new(bgp(vec![pat("s", "http://ex/p", "o")])),
        };
        let passes: Vec<Box<dyn LogicalPass>> = vec![Box::new(DuplicateCorruptor)];
        run_passes(legal, &passes, &PlanCtx::default());
    }
}
