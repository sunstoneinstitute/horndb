//! Result-invariant column-pruning plan rewrite.
//!
//! [`rewrite`] threads a top-down *demanded* set (the variable names a node's
//! parent needs from it) through the [`PhysicalPlan`] tree and drops columns
//! that no ancestor reads. The only observable effect is performance: narrower
//! rows mean less cloning/hashing through joins, DISTINCT and GROUP, and fewer
//! `TermId → Term` decodes at the result boundary.
//!
//! ## The correctness contract
//!
//! For every plan, `Runtime::run` must yield byte-identical `Bindings` with and
//! without this rewrite. Two facts make that tractable:
//!
//! 1. **`run` decodes the root batch to `Bindings` (a `BTreeMap` keyed by
//!    variable name).** Column *order* in any batch is therefore irrelevant to
//!    the result, and an `Unbound` slot contributes no key. What matters is,
//!    per row, the set of bound `(name, term)` pairs — plus row order and
//!    multiplicity.
//! 2. **The root's demanded set is its full natural output** (see [`rewrite`]),
//!    so the root is never narrowed below what it already emits.
//!
//! Given that, narrowing an interior node is safe as long as (a) every variable
//! any operator *evaluates over* (join keys, FILTER/ORDER BY/BIND expression
//! vars, GROUP keys and aggregate inputs) stays produced, and (b) we never
//! change row multiplicity. Multiplicity is only sensitive at the two
//! deduplicating points — `Distinct` and `Group`:
//!
//! * **`Distinct` is a hard barrier.** Pruning columns *before* a DISTINCT
//!   changes the dedup key set and therefore the row count. So `Distinct`
//!   demands its child's *full natural output* — the same columns the un-pruned
//!   plan would dedup on. (Narrowing may still happen *above* a DISTINCT, and
//!   *below* it only down to its own natural schema.)
//! * **`Group` is a natural restriction point.** Its output is `keys ++
//!   aggregate-outputs` and it ignores every other input column, so it demands
//!   exactly `keys ∪ (aggregate input vars)` from its child — which is what the
//!   un-pruned operator already keys/folds on. Multiplicity is unchanged.
//!
//! ## The "evaluate-wide, project-narrow" rule
//!
//! A node that introduces vars beyond `demanded` for its own evaluation
//! (FILTER expr vars, join keys, ORDER BY key vars, an Extend var the parent
//! does not want) is rebuilt with its children pruned, then — if its resulting
//! output is still wider than `demanded` — wrapped in a restricting `Project`
//! to strip the surplus back to exactly `demanded`. Leaves (`BgpScan`,
//! `Values`) that produce more than `demanded` are wrapped the same way; this
//! is where the bulk of the pruning originates.
//!
//! ## Conservative no-ops
//!
//! Where narrowing an edge is not obviously safe we pass the child its *full
//! natural output* (i.e. do not prune that edge) — correctness over
//! aggressiveness:
//!
//! * **`Union`** — recurse both branches with the incoming `demanded` (which is
//!   the full natural output whenever a DISTINCT sits above), never wrap. Both
//!   branches must share the merged schema; the existing `UnionOp` handles
//!   absent-in-one-branch vars as `Unbound`.
//! * **`PathClosure`** — the `edge` keeps its full natural output (the synthetic
//!   `?pp_src`/`?pp_dst` endpoints the BFS needs); we never prune inside it.

use crate::algebra::{AggFunc, Expr, Term, TriplePattern, Var};
use crate::error::Result;
use crate::exec::runtime::{agg_inner_exprs, referenced_vars};
use crate::plan::PhysicalPlan;
use std::collections::HashSet;

/// Rewrite `plan` into a result-equivalent plan. Two passes run in order:
///
/// 1. [`push_aggregates`] — turn a `COUNT(*)`/`COUNT(?v)` over a bare `BgpScan`
///    into a [`PhysicalPlan::CountScan`], answering the count without
///    materializing rows (#144).
/// 2. column pruning ([`prune`]) — drop interior columns no ancestor needs.
///
/// Both are result-invariant; `Runtime::run` must yield byte-identical
/// `Bindings` with and without this rewrite.
pub fn rewrite(plan: &PhysicalPlan) -> Result<PhysicalPlan> {
    let plan = push_aggregates(plan.clone());
    // The root must emit exactly what it emits today, so it demands its own
    // full natural output. No narrowing happens at the root level; only
    // interior edges (and leaves below them) can be pruned.
    let demanded: HashSet<String> = output_vars(&plan).into_iter().collect();
    Ok(prune(&plan, &demanded))
}

/// Recognize the narrow `COUNT`-over-BGP shape and rewrite it to a
/// [`PhysicalPlan::CountScan`]; recurse into every other node unchanged.
///
/// The matched shape is EXACTLY:
/// `Group { inner: BgpScan { patterns }, keys, aggregates }` where
/// * `keys.is_empty()` (implicit grouping — no `GROUP BY`), AND
/// * `aggregates.len() == 1`, AND
/// * the single aggregate has `distinct == false` and is either
///   * `COUNT(*)` (`AggFunc::CountStar`), or
///   * `COUNT(?v)` (`AggFunc::Count(Expr::Term(Term::Var(v)))`) where `?v` is
///     one of the BGP's output variables.
///
/// The `?v ∈ BGP vars` guard is what makes `COUNT(?v)` equivalent to the
/// solution count: a BGP binds every one of its variables in every solution,
/// so `COUNT(?v)` over a bound BGP var equals `COUNT(*)`. A `COUNT(?z)` over a
/// var the BGP does not produce would count 0, so it is deliberately NOT
/// rewritten and stays a `Group`.
fn push_aggregates(plan: PhysicalPlan) -> PhysicalPlan {
    use PhysicalPlan::*;
    match plan {
        Group {
            inner,
            keys,
            aggregates,
        } if keys.is_empty() && aggregates.len() == 1 => {
            // Only a bare BgpScan inner qualifies.
            if let BgpScan { patterns } = *inner {
                let agg = &aggregates[0];
                let count_over_bgp = !agg.distinct
                    && match &agg.func {
                        AggFunc::CountStar => true,
                        AggFunc::Count(e) => match &**e {
                            Expr::Term(Term::Var(v)) => {
                                let bgp_vars: HashSet<String> = output_vars(&BgpScan {
                                    patterns: patterns.clone(),
                                })
                                .into_iter()
                                .collect();
                                bgp_vars.contains(v.name())
                            }
                            _ => false,
                        },
                        _ => false,
                    };
                if count_over_bgp {
                    return CountScan {
                        patterns,
                        out_var: agg.out.clone(),
                    };
                }
                // Not the count shape: rebuild the Group unchanged.
                return Group {
                    inner: Box::new(BgpScan { patterns }),
                    keys,
                    aggregates,
                };
            }
            // Inner is not a bare BgpScan: recurse into it, keep the Group.
            Group {
                inner: Box::new(push_aggregates(*inner)),
                keys,
                aggregates,
            }
        }
        // Everything else: recurse into children unchanged.
        other => map_children(other, push_aggregates),
    }
}

/// Apply `f` to each direct child of `node`, rebuilding `node` with the
/// rewritten children. Leaves are returned unchanged.
fn map_children(node: PhysicalPlan, f: fn(PhysicalPlan) -> PhysicalPlan) -> PhysicalPlan {
    use PhysicalPlan::*;
    match node {
        leaf @ (BgpScan { .. } | CountScan { .. } | GroupCountScan { .. } | Values { .. }) => leaf,
        Join { left, right } => Join {
            left: Box::new(f(*left)),
            right: Box::new(f(*right)),
        },
        LeftJoin { left, right, expr } => LeftJoin {
            left: Box::new(f(*left)),
            right: Box::new(f(*right)),
            expr,
        },
        Union { left, right } => Union {
            left: Box::new(f(*left)),
            right: Box::new(f(*right)),
        },
        Filter { expr, inner } => Filter {
            expr,
            inner: Box::new(f(*inner)),
        },
        Project { vars, inner } => Project {
            vars,
            inner: Box::new(f(*inner)),
        },
        Distinct { inner } => Distinct {
            inner: Box::new(f(*inner)),
        },
        Slice {
            inner,
            start,
            length,
        } => Slice {
            inner: Box::new(f(*inner)),
            start,
            length,
        },
        OrderBy { inner, keys } => OrderBy {
            inner: Box::new(f(*inner)),
            keys,
        },
        Extend { inner, var, expr } => Extend {
            inner: Box::new(f(*inner)),
            var,
            expr,
        },
        Group {
            inner,
            keys,
            aggregates,
        } => Group {
            inner: Box::new(f(*inner)),
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
            edge: Box::new(f(*edge)),
            reflexive,
        },
    }
}

/// A node's natural output variables, computed structurally, in a deterministic
/// order. Column order never affects the result (the boundary decodes to a
/// name-keyed map), so the order here is only for building deterministic
/// `Project` schemas and for set membership.
///
/// `BgpScan` over-counting is harmless and never under-counts (we recurse into
/// RDF 1.2 triple-term patterns), so a demanded-and-produced column is never
/// mistakenly dropped.
pub(crate) fn output_vars(node: &PhysicalPlan) -> Vec<String> {
    use PhysicalPlan::*;
    match node {
        BgpScan { patterns } => {
            let mut out = Vec::new();
            for p in patterns {
                collect_pattern_vars(p, &mut out);
            }
            out
        }
        CountScan { out_var, .. } => vec![out_var.name().to_owned()],
        GroupCountScan { keys, out_vars, .. } => {
            let mut out: Vec<String> = Vec::new();
            for k in keys {
                push_unique(&mut out, k.name());
            }
            for v in out_vars {
                push_unique(&mut out, v.name());
            }
            out
        }
        Join { left, right } | LeftJoin { left, right, .. } | Union { left, right } => {
            let mut out = output_vars(left);
            for v in output_vars(right) {
                push_unique(&mut out, &v);
            }
            out
        }
        Filter { inner, .. } | Distinct { inner } | Slice { inner, .. } | OrderBy { inner, .. } => {
            output_vars(inner)
        }
        Project { vars, .. } => vars.iter().map(|v| v.name().to_owned()).collect(),
        Extend { inner, var, .. } => {
            let mut out = output_vars(inner);
            push_unique(&mut out, var.name());
            out
        }
        Values { vars, .. } => vars.iter().map(|v| v.name().to_owned()).collect(),
        Group {
            keys, aggregates, ..
        } => {
            let mut out: Vec<String> = keys.iter().map(|k| k.name().to_owned()).collect();
            for a in aggregates {
                push_unique(&mut out, a.out.name());
            }
            out
        }
        PathClosure {
            subject, object, ..
        } => {
            let mut out = Vec::new();
            for t in [subject, object] {
                if let Term::Var(v) = t {
                    push_unique(&mut out, v.name());
                }
            }
            out
        }
    }
}

fn push_unique(out: &mut Vec<String>, name: &str) {
    if !out.iter().any(|x| x == name) {
        out.push(name.to_owned());
    }
}

fn collect_pattern_vars(p: &TriplePattern, out: &mut Vec<String>) {
    for t in [&p.subject, &p.predicate, &p.object] {
        collect_term_vars(t, out);
    }
}

fn collect_term_vars(t: &Term, out: &mut Vec<String>) {
    match t {
        Term::Var(v) => push_unique(out, v.name()),
        Term::Triple(tp) => collect_pattern_vars(tp, out),
        _ => {}
    }
}

/// Wrap `node` in a restricting `Project` if its output is wider than
/// `demanded`. The kept columns are `node_out ∩ demanded`, in `node_out`'s
/// order (deterministic).
///
/// Guard: if the intersection is empty we must *not* wrap — an empty-`vars`
/// `Project` is interpreted as `SELECT *` by the runtime (it keeps everything).
/// In that (rare) case we return `node` unchanged; the surplus columns ride
/// upward harmlessly and are stripped by some ancestor (ultimately bounded by
/// the result-preserving root demand). Multiplicity is unaffected because the
/// nodes this helper wraps (Filter/Join/LeftJoin/OrderBy/Extend, and the
/// leaves) never deduplicate.
fn wrap_if_wider(
    node: PhysicalPlan,
    node_out: &[String],
    demanded: &HashSet<String>,
) -> PhysicalPlan {
    let kept: Vec<Var> = node_out
        .iter()
        .filter(|v| demanded.contains(*v))
        .map(|v| Var::new(v.as_str()))
        .collect();
    if kept.len() < node_out.len() && !kept.is_empty() {
        PhysicalPlan::Project {
            vars: kept,
            inner: Box::new(node),
        }
    } else {
        node
    }
}

fn intersect(superset: &HashSet<String>, scope: &[String]) -> HashSet<String> {
    scope
        .iter()
        .filter(|v| superset.contains(*v))
        .cloned()
        .collect()
}

/// Rewrite `node` so its output contains at least `demanded ∩ (node's
/// producible vars)`, pruning every edge that can be safely narrowed.
fn prune(node: &PhysicalPlan, demanded: &HashSet<String>) -> PhysicalPlan {
    use PhysicalPlan::*;
    match node {
        BgpScan { patterns } => {
            let nat = output_vars(node);
            wrap_if_wider(
                BgpScan {
                    patterns: patterns.clone(),
                },
                &nat,
                demanded,
            )
        }
        Values { vars, rows } => {
            let nat: Vec<String> = vars.iter().map(|v| v.name().to_owned()).collect();
            wrap_if_wider(
                Values {
                    vars: vars.clone(),
                    rows: rows.clone(),
                },
                &nat,
                demanded,
            )
        }
        // Count leaves: nothing below them to prune, and their output columns
        // are exactly the replaced Group's output (narrowed, if at all, by an
        // ancestor Project). Unchanged.
        CountScan { .. } | GroupCountScan { .. } => node.clone(),
        // The Project itself is the restriction point: it forwards exactly its
        // own `vars` to the child (ignoring the incoming demand, which can only
        // be a subset and is re-applied by this Project's own output).
        Project { vars, inner } => {
            let want: HashSet<String> = vars.iter().map(|v| v.name().to_owned()).collect();
            Project {
                vars: vars.clone(),
                inner: Box::new(prune(inner, &want)),
            }
        }
        // FILTER must see its expression's vars even if the parent does not.
        Filter { expr, inner } => {
            let mut d = demanded.clone();
            referenced_vars(expr, &mut d);
            let pi = prune(inner, &d);
            let node2 = Filter {
                expr: expr.clone(),
                inner: Box::new(pi),
            };
            let nat = output_vars(&node2);
            wrap_if_wider(node2, &nat, demanded)
        }
        // Both sides need the shared join keys plus whatever they contribute to
        // `demanded`. Evaluate wide, then project the join keys (and any other
        // surplus) back down to `demanded`.
        Join { left, right } => {
            let lo = output_vars(left);
            let ro = output_vars(right);
            let mut base = demanded.clone();
            for v in &lo {
                if ro.contains(v) {
                    base.insert(v.clone());
                }
            }
            let pl = prune(left, &intersect(&base, &lo));
            let pr = prune(right, &intersect(&base, &ro));
            let node2 = Join {
                left: Box::new(pl),
                right: Box::new(pr),
            };
            let nat = output_vars(&node2);
            wrap_if_wider(node2, &nat, demanded)
        }
        // Like Join, but the optional ON expression's vars are also required on
        // both sides.
        LeftJoin { left, right, expr } => {
            let lo = output_vars(left);
            let ro = output_vars(right);
            let mut base = demanded.clone();
            for v in &lo {
                if ro.contains(v) {
                    base.insert(v.clone());
                }
            }
            if let Some(e) = expr {
                referenced_vars(e, &mut base);
            }
            let pl = prune(left, &intersect(&base, &lo));
            let pr = prune(right, &intersect(&base, &ro));
            let node2 = LeftJoin {
                left: Box::new(pl),
                right: Box::new(pr),
                expr: expr.clone(),
            };
            let nat = output_vars(&node2);
            wrap_if_wider(node2, &nat, demanded)
        }
        // Conservative: both branches must share the merged schema. Recurse
        // with `demanded` (the full natural output whenever a DISTINCT sits
        // above), never wrap.
        Union { left, right } => Union {
            left: Box::new(prune(left, demanded)),
            right: Box::new(prune(right, demanded)),
        },
        // BARRIER: DISTINCT dedups on its child's columns. Pruning before it
        // would change the dedup key set and therefore the row count, so it
        // demands the child's full natural output.
        Distinct { inner } => {
            let nat: HashSet<String> = output_vars(inner).into_iter().collect();
            Distinct {
                inner: Box::new(prune(inner, &nat)),
            }
        }
        // OFFSET/LIMIT does not change columns and preserves row order; narrower
        // rows below it are fine.
        Slice {
            inner,
            start,
            length,
        } => Slice {
            inner: Box::new(prune(inner, demanded)),
            start: *start,
            length: *length,
        },
        // Sort needs its key vars; strip key-only columns after sorting.
        OrderBy { inner, keys } => {
            let mut d = demanded.clone();
            for (e, _) in keys {
                referenced_vars(e, &mut d);
            }
            let pi = prune(inner, &d);
            let node2 = OrderBy {
                inner: Box::new(pi),
                keys: keys.clone(),
            };
            let nat = output_vars(&node2);
            wrap_if_wider(node2, &nat, demanded)
        }
        // BIND: the child needs the expr's vars; `var` is produced here, so it
        // is removed from the child's demand.
        Extend { inner, var, expr } => {
            let mut d = demanded.clone();
            d.remove(var.name());
            referenced_vars(expr, &mut d);
            let pi = prune(inner, &d);
            let node2 = Extend {
                inner: Box::new(pi),
                var: var.clone(),
                expr: expr.clone(),
            };
            let nat = output_vars(&node2);
            wrap_if_wider(node2, &nat, demanded)
        }
        // GROUP BY is normally a natural restriction point: its output is
        // `keys ++ aggregate-outputs`, and it reads only the grouping keys and
        // the aggregates' input expressions from the child.
        //
        // EXCEPTION — `COUNT(DISTINCT *)` dedups whole solution rows, so it
        // reads EVERY input column (`agg_inner_exprs` is empty for it, which is
        // exactly why it must be special-cased). When any aggregate is a
        // distinct `CountStar`, Group becomes a full barrier and demands the
        // child's entire natural output, like `Distinct`. (Plain `COUNT(*)`
        // only needs the member count, so it imposes no column demand.)
        Group {
            inner,
            keys,
            aggregates,
        } => {
            let distinct_star = aggregates
                .iter()
                .any(|a| matches!(a.func, AggFunc::CountStar) && a.distinct);
            let d: HashSet<String> = if distinct_star {
                output_vars(inner).into_iter().collect()
            } else {
                let mut d: HashSet<String> = keys.iter().map(|k| k.name().to_owned()).collect();
                for a in aggregates {
                    for e in agg_inner_exprs(a) {
                        referenced_vars(e, &mut d);
                    }
                }
                d
            };
            Group {
                inner: Box::new(prune(inner, &d)),
                keys: keys.clone(),
                aggregates: aggregates.clone(),
            }
        }
        // Conservative: the BFS needs the synthetic endpoint vars; keep the
        // edge's full natural output and do not prune inside it.
        PathClosure {
            subject,
            object,
            edge,
            reflexive,
        } => {
            let nat: HashSet<String> = output_vars(edge).into_iter().collect();
            PathClosure {
                subject: subject.clone(),
                object: object.clone(),
                edge: Box::new(prune(edge, &nat)),
                reflexive: *reflexive,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algebra::translate::translate_query_with;
    use crate::exec::horn::HornBackend;
    use crate::exec::runtime::Runtime;
    use crate::exec::{Bindings, Store};
    use crate::parser::parse_query;
    use crate::plan::planner;
    use crate::SparqlConfig;

    fn plan_select(q: &str) -> PhysicalPlan {
        let parsed = parse_query(q).expect("query parse failed");
        let inner = match parsed {
            crate::parser::ParsedQuery::Select { inner } => inner,
            other => panic!("expected SELECT, got {other:?}"),
        };
        let alg =
            translate_query_with(&inner, &SparqlConfig::default()).expect("translation failed");
        planner::plan(&alg).expect("planning failed")
    }

    /// A small, deterministic store covering the shapes the battery exercises.
    fn fixture() -> HornBackend {
        let mut horn = HornBackend::new();
        let iri = |s: &str| Term::Iri(format!("http://ex/{s}"));
        let lit = |s: &str| Term::Literal(format!("\"{s}\""));
        // people with names, ages, and friends
        horn.insert_triple(iri("a"), iri("name"), lit("Alice"));
        horn.insert_triple(iri("a"), iri("age"), Term::Literal("\"30\"".into()));
        horn.insert_triple(iri("a"), iri("knows"), iri("b"));
        horn.insert_triple(iri("b"), iri("name"), lit("Bob"));
        horn.insert_triple(iri("b"), iri("age"), Term::Literal("\"25\"".into()));
        horn.insert_triple(iri("b"), iri("knows"), iri("c"));
        horn.insert_triple(iri("c"), iri("name"), lit("Carol"));
        // c has no age (drives OPTIONAL coverage)
        horn.insert_triple(iri("c"), iri("knows"), iri("a"));
        horn.insert_triple(iri("d"), iri("name"), lit("Alice")); // duplicate name
        horn
    }

    /// Canonical, order-independent rendering of a result set for byte-identical
    /// comparison. Each row becomes a sorted `"var=lex"` join (Bindings is a
    /// BTreeMap, so the per-row string is already deterministic); the outer Vec
    /// is sorted to make the comparison a multiset equality.
    fn canon(mut rows: Vec<Bindings>) -> Vec<String> {
        let mut out: Vec<String> = rows
            .drain(..)
            .map(|b| {
                b.vars()
                    .map(|(k, v)| format!("{k}={v:?}"))
                    .collect::<Vec<_>>()
                    .join("\u{1}")
            })
            .collect();
        out.sort();
        out
    }

    /// Run a plan WITHOUT the rewrite (build the original plan directly). `run`
    /// always rewrites, so this is the guaranteed no-rewrite baseline.
    fn run_raw(horn: &HornBackend, plan: &PhysicalPlan) -> Vec<Bindings> {
        Runtime::new(horn).run_unpruned_for_test(plan)
    }

    /// The correctness gate: a battery of plan shapes whose results must be
    /// byte-identical with and without the rewrite.
    #[test]
    fn rewrite_is_result_invariant() {
        let horn = fixture();
        let queries = [
            // BGP only
            "SELECT * WHERE { ?s <http://ex/name> ?n }",
            // Project dropping a var (?p, ?o unused)
            "SELECT ?s WHERE { ?s ?p ?o }",
            // Join with unused vars on each side (sub-SELECT keeps a real Join)
            "SELECT ?n WHERE { ?s <http://ex/knows> ?o . { SELECT ?s ?n WHERE { ?s <http://ex/name> ?n } } }",
            // Filter with an expr var not in the output
            "SELECT ?s WHERE { ?s <http://ex/age> ?age FILTER(?age > \"20\") }",
            // Group with COUNT and a key
            "SELECT ?n (COUNT(?s) AS ?c) WHERE { ?s <http://ex/name> ?n } GROUP BY ?n",
            // Implicit grouping COUNT(*)
            "SELECT (COUNT(*) AS ?c) WHERE { ?s <http://ex/name> ?n }",
            // COUNT(DISTINCT *) — dedups whole rows; Group must keep all cols
            "SELECT (COUNT(DISTINCT *) AS ?c) WHERE { ?s ?p ?o }",
            "SELECT ?n (COUNT(DISTINCT *) AS ?c) WHERE { ?s <http://ex/name> ?n } GROUP BY ?n",
            // ORDER BY on a key var not selected
            "SELECT ?s WHERE { ?s <http://ex/age> ?age } ORDER BY ?age",
            // DISTINCT over a Project that drops cols
            "SELECT DISTINCT ?n WHERE { ?s <http://ex/name> ?n }",
            // DISTINCT over a full BGP, then narrowed by outer Project
            "SELECT ?s WHERE { ?s <http://ex/name> ?n } ORDER BY ?s",
            // LeftJoin (OPTIONAL) where the optional var is unused
            "SELECT ?s WHERE { ?s <http://ex/name> ?n OPTIONAL { ?s <http://ex/age> ?age } }",
            // LeftJoin where the optional var IS used
            "SELECT ?s ?age WHERE { ?s <http://ex/name> ?n OPTIONAL { ?s <http://ex/age> ?age } }",
            // Union
            "SELECT ?x WHERE { { ?x <http://ex/name> ?n } UNION { ?x <http://ex/age> ?age } }",
            // BIND with an unused source var
            "SELECT ?s WHERE { ?s <http://ex/age> ?age BIND(?age AS ?b) }",
            // Slice
            "SELECT ?s WHERE { ?s <http://ex/name> ?n } ORDER BY ?s LIMIT 2 OFFSET 1",
            // Nested: Distinct over Join with surplus columns
            "SELECT DISTINCT ?n WHERE { ?s <http://ex/knows> ?o . ?s <http://ex/name> ?n }",
        ];
        for q in queries {
            let plan = plan_select(q);
            let with: Vec<Bindings> = Runtime::new(&horn).run(&plan).unwrap().collect();
            let without = run_raw(&horn, &plan);
            assert_eq!(
                canon(with),
                canon(without),
                "rewrite changed results for query:\n{q}\nrewritten plan: {:#?}",
                rewrite(&plan).unwrap()
            );
        }
    }

    /// Pruning actually narrows at least one plan: `SELECT ?s WHERE { ?s ?p ?o }`
    /// must end up with the scan wrapped in a Project that drops ?p and ?o.
    #[test]
    fn pruning_narrows_bgp_scan() {
        let plan = plan_select("SELECT ?s WHERE { ?s ?p ?o }");
        let rewritten = rewrite(&plan).unwrap();
        // Find a BgpScan and assert it is wrapped by a Project that keeps only ?s.
        assert!(
            scan_is_narrowed_to(&rewritten, &["s"]),
            "expected the BGP scan to be wrapped in Project([?s]); got {rewritten:#?}"
        );
        // And the un-rewritten scan was wider (?s ?p ?o).
        let mut bgp_vars = Vec::new();
        find_bgp_vars(&plan, &mut bgp_vars);
        assert_eq!(bgp_vars, vec!["s", "p", "o"]);
    }

    /// DISTINCT barrier (the task's most dangerous case): a `Distinct` directly
    /// over a wide `BgpScan`, with the parent (`Project([?s])`) demanding only
    /// `?s`. Pruning the scan to `?s` *before* the DISTINCT would dedup on `?s`
    /// alone and drop rows; the barrier must keep the scan at full width so
    /// DISTINCT keys on all of `?s ?p ?o`. `SELECT DISTINCT` is no good here —
    /// it lowers to `Distinct(Project([?s], BGP))`, which dedups on the
    /// projection by design — so we hand-build the `Distinct(BGP)` shape.
    #[test]
    fn distinct_barrier_keeps_full_dedup_key() {
        use crate::algebra::TriplePattern;
        let mut horn = HornBackend::new();
        let iri = |s: &str| Term::Iri(format!("http://ex/{s}"));
        // Two triples sharing a subject but differing in (p,o): DISTINCT on the
        // full row keeps both; DISTINCT on ?s alone would collapse to one.
        horn.insert_triple(iri("s1"), iri("p1"), iri("o1"));
        horn.insert_triple(iri("s1"), iri("p2"), iri("o2"));

        let var = |n: &str| Term::Var(Var::new(n));
        let bgp = PhysicalPlan::BgpScan {
            patterns: vec![TriplePattern {
                subject: var("s"),
                predicate: var("p"),
                object: var("o"),
            }],
        };
        let plan = PhysicalPlan::Project {
            vars: vec![Var::new("s")],
            inner: Box::new(PhysicalPlan::Distinct {
                inner: Box::new(bgp),
            }),
        };

        let rewritten = rewrite(&plan).unwrap();
        // The scan feeding the Distinct must still produce all of ?s ?p ?o.
        let inner = distinct_inner(&rewritten).expect("rewritten plan must keep a Distinct");
        let mut inner_out = output_vars(inner);
        inner_out.sort();
        assert_eq!(
            inner_out,
            vec!["o".to_string(), "p".to_string(), "s".to_string()],
            "Distinct's child must keep the full dedup key; got {inner:#?}"
        );

        // Result parity, and: two distinct (s,p,o) rows both project to s1, so
        // the (non-deduping) outer Project yields ?s = s1 twice.
        let with: Vec<Bindings> = Runtime::new(&horn).run(&plan).unwrap().collect();
        let without = run_raw(&horn, &plan);
        assert_eq!(canon(with.clone()), canon(without));
        assert_eq!(
            with.len(),
            2,
            "DISTINCT must key on all 3 columns: {with:?}"
        );
    }

    // ---- COUNT-over-BGP pushdown (#144) ----

    /// True iff the plan tree contains a `CountScan` node anywhere.
    fn has_count_scan(p: &PhysicalPlan) -> bool {
        match p {
            PhysicalPlan::CountScan { .. } => true,
            PhysicalPlan::Project { inner, .. }
            | PhysicalPlan::Filter { inner, .. }
            | PhysicalPlan::Distinct { inner }
            | PhysicalPlan::Slice { inner, .. }
            | PhysicalPlan::OrderBy { inner, .. }
            | PhysicalPlan::Extend { inner, .. }
            | PhysicalPlan::Group { inner, .. } => has_count_scan(inner),
            PhysicalPlan::Join { left, right }
            | PhysicalPlan::LeftJoin { left, right, .. }
            | PhysicalPlan::Union { left, right } => has_count_scan(left) || has_count_scan(right),
            PhysicalPlan::PathClosure { edge, .. } => has_count_scan(edge),
            PhysicalPlan::BgpScan { .. }
            | PhysicalPlan::GroupCountScan { .. }
            | PhysicalPlan::Values { .. } => false,
        }
    }

    /// A store with exactly `n` triples on predicate `p` (distinct subjects).
    fn n_triples(n: usize) -> HornBackend {
        let mut horn = HornBackend::new();
        let iri = |s: &str| Term::Iri(format!("http://ex/{s}"));
        for i in 0..n {
            horn.insert_triple(iri(&format!("s{i}")), iri("p"), iri(&format!("o{i}")));
        }
        horn
    }

    /// The single `?n` count value of a one-row result, as its lexical form.
    fn single_count(rows: &[Bindings]) -> String {
        assert_eq!(rows.len(), 1, "expected exactly one result row: {rows:?}");
        format!("{:?}", rows[0].get("n").expect("?n must be bound"))
    }

    #[test]
    fn count_star_over_bgp_pushes_down_and_counts() {
        let horn = n_triples(7);
        let plan = plan_select("SELECT (COUNT(*) AS ?n) WHERE { ?s <http://ex/p> ?o }");
        let rewritten = rewrite(&plan).unwrap();
        assert!(
            has_count_scan(&rewritten),
            "COUNT(*) over a bare BGP must rewrite to CountScan; got {rewritten:#?}"
        );
        let out: Vec<Bindings> = Runtime::new(&horn).run(&plan).unwrap().collect();
        assert_eq!(
            single_count(&out),
            format!("{:?}", crate::exec::runtime::integer_literal(7))
        );
    }

    #[test]
    fn count_var_over_bound_bgp_var_pushes_down_and_counts() {
        let horn = n_triples(5);
        // ?s is bound by every BGP solution, so COUNT(?s) == COUNT(*) == 5.
        let plan = plan_select("SELECT (COUNT(?s) AS ?n) WHERE { ?s <http://ex/p> ?o }");
        let rewritten = rewrite(&plan).unwrap();
        assert!(
            has_count_scan(&rewritten),
            "COUNT(?s) over a bound BGP var must rewrite to CountScan; got {rewritten:#?}"
        );
        let out: Vec<Bindings> = Runtime::new(&horn).run(&plan).unwrap().collect();
        assert_eq!(
            single_count(&out),
            format!("{:?}", crate::exec::runtime::integer_literal(5))
        );
    }

    #[test]
    fn count_pushdown_parity_with_streaming_group() {
        let horn = fixture();
        for q in [
            "SELECT (COUNT(*) AS ?n) WHERE { ?s <http://ex/name> ?o }",
            "SELECT (COUNT(?s) AS ?n) WHERE { ?s <http://ex/knows> ?o }",
            "SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }",
            // No solutions: COUNT(*) of nothing is one row with 0.
            "SELECT (COUNT(*) AS ?n) WHERE { ?s <http://ex/nope> ?o }",
        ] {
            let plan = plan_select(q);
            // The pushdown path (run) vs the streaming Group baseline.
            let with: Vec<Bindings> = Runtime::new(&horn).run(&plan).unwrap().collect();
            let without = run_raw(&horn, &plan);
            assert_eq!(canon(with), canon(without), "count parity broke for:\n{q}");
        }
    }

    #[test]
    fn scope_guard_keeps_group_and_stays_correct() {
        let horn = fixture();
        // Each of these must NOT become a CountScan, and must still be correct.
        let cases = [
            // DISTINCT count: not the plain-count shape.
            "SELECT (COUNT(DISTINCT ?s) AS ?n) WHERE { ?s <http://ex/name> ?o }",
            // GROUP BY: non-empty keys.
            "SELECT ?c (COUNT(*) AS ?n) WHERE { ?s <http://ex/name> ?c } GROUP BY ?c",
            // Inner is Filter(BGP), not a bare BgpScan.
            "SELECT (COUNT(*) AS ?n) WHERE { ?s <http://ex/age> ?o FILTER(?o > \"20\") }",
            // Two aggregates.
            "SELECT (COUNT(*) AS ?n) (COUNT(?o) AS ?m) WHERE { ?s <http://ex/name> ?o }",
            // COUNT over a var the BGP does not bind: must count 0, not solutions.
            "SELECT (COUNT(?z) AS ?n) WHERE { ?s <http://ex/name> ?o }",
        ];
        for q in cases {
            let plan = plan_select(q);
            let rewritten = rewrite(&plan).unwrap();
            assert!(
                !has_count_scan(&rewritten),
                "scope guard failed — this must stay a Group:\n{q}\ngot {rewritten:#?}"
            );
            let with: Vec<Bindings> = Runtime::new(&horn).run(&plan).unwrap().collect();
            let without = run_raw(&horn, &plan);
            assert_eq!(canon(with), canon(without), "result parity broke for:\n{q}");
        }
    }

    #[test]
    fn count_scan_falls_back_when_count_bgp_is_none() {
        use crate::algebra::TriplePattern;
        use crate::exec::mem::MemStore;
        // MemStore uses the default `count_bgp` (None), exercising the
        // `scan_bgp_ids().rows.len()` correctness fallback in CountScanOp.
        let mut mem = MemStore::default();
        for i in 0..4 {
            mem.insert((format!("s{i}"), "p".into(), format!("o{i}")));
        }
        let var = |n: &str| Term::Var(Var::new(n));
        let plan = PhysicalPlan::CountScan {
            patterns: vec![TriplePattern {
                subject: var("s"),
                predicate: Term::Iri("p".into()),
                object: var("o"),
            }],
            out_var: Var::new("n"),
        };
        let out: Vec<Bindings> = Runtime::new(&mem).run(&plan).unwrap().collect();
        assert_eq!(
            single_count(&out),
            format!("{:?}", crate::exec::runtime::integer_literal(4)),
            "default count_bgp (None) must fall back to scan+len"
        );
    }

    #[test]
    fn group_count_scan_falls_back_when_seam_is_none() {
        use crate::algebra::TriplePattern;
        use crate::exec::mem::MemStore;
        // MemStore uses the default `count_bgp_grouped` (None), exercising the
        // scan + hash-count fallback in GroupCountScanOp.
        let mut mem = MemStore::default();
        // s0 has two objects, s1 has one.
        mem.insert(("s0".into(), "p".into(), "o0".into()));
        mem.insert(("s0".into(), "p".into(), "o1".into()));
        mem.insert(("s1".into(), "p".into(), "o2".into()));
        let var = |n: &str| Term::Var(Var::new(n));
        let plan = PhysicalPlan::GroupCountScan {
            patterns: vec![TriplePattern {
                subject: var("s"),
                predicate: Term::Iri("p".into()),
                object: var("o"),
            }],
            keys: vec![Var::new("s")],
            out_vars: vec![Var::new("c")],
        };
        let out: Vec<Bindings> = Runtime::new(&mem).run(&plan).unwrap().collect();
        // Same deterministic order as eval_group_native: decoded-lexical key
        // sort, so s0 before s1.
        assert_eq!(out.len(), 2, "one row per group: {out:?}");
        assert_eq!(out[0].get("s"), Some(&Term::Iri("s0".into())));
        assert_eq!(
            format!("{:?}", out[0].get("c").expect("?c bound")),
            format!("{:?}", crate::exec::runtime::integer_literal(2))
        );
        assert_eq!(out[1].get("s"), Some(&Term::Iri("s1".into())));
        assert_eq!(
            format!("{:?}", out[1].get("c").expect("?c bound")),
            format!("{:?}", crate::exec::runtime::integer_literal(1))
        );
    }

    #[test]
    fn group_count_scan_no_keys_is_implicit_group() {
        use crate::algebra::TriplePattern;
        use crate::exec::mem::MemStore;
        let var = |n: &str| Term::Var(Var::new(n));
        let mk_plan = || PhysicalPlan::GroupCountScan {
            patterns: vec![TriplePattern {
                subject: var("s"),
                predicate: Term::Iri("p".into()),
                object: var("o"),
            }],
            keys: vec![],
            out_vars: vec![Var::new("n"), Var::new("m")],
        };
        // Zero solutions + implicit group: exactly one row of zeros
        // (SPARQL §11.2 — COUNT of nothing is 0).
        let empty = MemStore::default();
        let out: Vec<Bindings> = Runtime::new(&empty).run(&mk_plan()).unwrap().collect();
        assert_eq!(out.len(), 1, "implicit group yields one row: {out:?}");
        for v in ["n", "m"] {
            assert_eq!(
                format!("{:?}", out[0].get(v).expect("count var bound")),
                format!("{:?}", crate::exec::runtime::integer_literal(0))
            );
        }
        // Three solutions: both counts carry 3.
        let mut mem = MemStore::default();
        for i in 0..3 {
            mem.insert((format!("s{i}"), "p".into(), format!("o{i}")));
        }
        let out: Vec<Bindings> = Runtime::new(&mem).run(&mk_plan()).unwrap().collect();
        assert_eq!(out.len(), 1);
        for v in ["n", "m"] {
            assert_eq!(
                format!("{:?}", out[0].get(v).expect("count var bound")),
                format!("{:?}", crate::exec::runtime::integer_literal(3))
            );
        }
    }

    #[test]
    fn group_count_scan_zero_solutions_with_keys_yields_no_rows() {
        use crate::algebra::TriplePattern;
        use crate::exec::mem::MemStore;
        let empty = MemStore::default();
        let var = |n: &str| Term::Var(Var::new(n));
        let plan = PhysicalPlan::GroupCountScan {
            patterns: vec![TriplePattern {
                subject: var("s"),
                predicate: Term::Iri("p".into()),
                object: var("o"),
            }],
            keys: vec![Var::new("s")],
            out_vars: vec![Var::new("c")],
        };
        let out: Vec<Bindings> = Runtime::new(&empty).run(&plan).unwrap().collect();
        assert!(
            out.is_empty(),
            "keyed grouping of nothing has no groups: {out:?}"
        );
    }

    // ---- structural test helpers ----

    fn scan_is_narrowed_to(p: &PhysicalPlan, want: &[&str]) -> bool {
        match p {
            PhysicalPlan::Project { vars, inner }
                if matches!(**inner, PhysicalPlan::BgpScan { .. }) =>
            {
                let got: Vec<&str> = vars.iter().map(|v| v.name()).collect();
                got == want
            }
            PhysicalPlan::Project { inner, .. }
            | PhysicalPlan::Filter { inner, .. }
            | PhysicalPlan::Distinct { inner }
            | PhysicalPlan::Slice { inner, .. }
            | PhysicalPlan::OrderBy { inner, .. }
            | PhysicalPlan::Extend { inner, .. }
            | PhysicalPlan::Group { inner, .. } => scan_is_narrowed_to(inner, want),
            PhysicalPlan::Join { left, right }
            | PhysicalPlan::LeftJoin { left, right, .. }
            | PhysicalPlan::Union { left, right } => {
                scan_is_narrowed_to(left, want) || scan_is_narrowed_to(right, want)
            }
            PhysicalPlan::PathClosure { edge, .. } => scan_is_narrowed_to(edge, want),
            PhysicalPlan::BgpScan { .. }
            | PhysicalPlan::CountScan { .. }
            | PhysicalPlan::GroupCountScan { .. }
            | PhysicalPlan::Values { .. } => false,
        }
    }

    fn find_bgp_vars(p: &PhysicalPlan, out: &mut Vec<String>) {
        match p {
            PhysicalPlan::BgpScan { .. } => {
                if out.is_empty() {
                    *out = output_vars(p);
                }
            }
            PhysicalPlan::Project { inner, .. }
            | PhysicalPlan::Filter { inner, .. }
            | PhysicalPlan::Distinct { inner }
            | PhysicalPlan::Slice { inner, .. }
            | PhysicalPlan::OrderBy { inner, .. }
            | PhysicalPlan::Extend { inner, .. }
            | PhysicalPlan::Group { inner, .. } => find_bgp_vars(inner, out),
            PhysicalPlan::Join { left, right }
            | PhysicalPlan::LeftJoin { left, right, .. }
            | PhysicalPlan::Union { left, right } => {
                find_bgp_vars(left, out);
                find_bgp_vars(right, out);
            }
            PhysicalPlan::PathClosure { edge, .. } => find_bgp_vars(edge, out),
            PhysicalPlan::CountScan { .. }
            | PhysicalPlan::GroupCountScan { .. }
            | PhysicalPlan::Values { .. } => {}
        }
    }

    fn distinct_inner(p: &PhysicalPlan) -> Option<&PhysicalPlan> {
        match p {
            PhysicalPlan::Distinct { inner } => Some(inner),
            PhysicalPlan::Project { inner, .. }
            | PhysicalPlan::Filter { inner, .. }
            | PhysicalPlan::Slice { inner, .. }
            | PhysicalPlan::OrderBy { inner, .. }
            | PhysicalPlan::Extend { inner, .. }
            | PhysicalPlan::Group { inner, .. } => distinct_inner(inner),
            PhysicalPlan::Join { left, right }
            | PhysicalPlan::LeftJoin { left, right, .. }
            | PhysicalPlan::Union { left, right } => {
                distinct_inner(left).or_else(|| distinct_inner(right))
            }
            PhysicalPlan::PathClosure { edge, .. } => distinct_inner(edge),
            PhysicalPlan::BgpScan { .. }
            | PhysicalPlan::CountScan { .. }
            | PhysicalPlan::GroupCountScan { .. }
            | PhysicalPlan::Values { .. } => None,
        }
    }
}
