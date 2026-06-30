//! `EXPLAIN` pragma rendering (SPEC-07 F9, acceptance #5).
//!
//! Walks a [`PhysicalPlan`] and produces a human- or machine-readable
//! description of the chosen plan: the per-node operator, its estimated
//! output cardinality, and a header carrying the execution mode
//! (entailment regime). It **does not execute** the query — that is the
//! whole point of `EXPLAIN`.
//!
//! ## Execution mode
//!
//! The only modes that exist today are the entailment-regime markers
//! (`simple` / materialized OWL-RL). Backward-chained mode (issue #55)
//! is not yet wired, so `EXPLAIN` reports the materialized mode for every
//! query and labels backward-chaining as not-yet-available. When #55
//! lands, the mode line gains the real per-query selection.
//!
//! ## Cardinality
//!
//! Per-node estimates come from [`Executor::cardinality_estimate`] for
//! `BgpScan` leaves and textbook combination rules for composite nodes.
//! The planner (`plan::planner`) is a 1:1 lowering with no cost model, so
//! these are *estimates*, surfaced with a `~` prefix — not guarantees.

use crate::exec::Executor;
use crate::plan::PhysicalPlan;
use std::fmt::Write as _;

/// Output format for an `EXPLAIN` rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExplainFormat {
    /// Indented text tree (the default).
    Text,
    /// A JSON object tree.
    Json,
}

/// The execution mode `EXPLAIN` reports. Stage-1 only distinguishes the
/// entailment regime; backward-chaining (#55) is not yet selectable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionMode {
    /// Query runs against an already-materialized store (simple regime, or
    /// OWL-RL closure pre-written by SPEC-04/05). This is the only mode
    /// today.
    Materialized,
}

impl ExecutionMode {
    fn label(self) -> &'static str {
        match self {
            ExecutionMode::Materialized => "materialized",
        }
    }
}

/// Render an `EXPLAIN` of `plan` using `exec` for cardinality estimates.
///
/// `mode` is the execution mode to report (Stage-1: always
/// [`ExecutionMode::Materialized`]).
pub fn explain<E: Executor + ?Sized>(
    plan: &PhysicalPlan,
    exec: &E,
    mode: ExecutionMode,
    format: ExplainFormat,
) -> String {
    match format {
        ExplainFormat::Text => render_text(plan, exec, mode),
        ExplainFormat::Json => render_json(plan, exec, mode),
    }
}

/// Estimated output cardinality of a plan node, recursively. Returns
/// `None` when the backend cannot estimate the underlying scans.
fn estimate<E: Executor + ?Sized>(plan: &PhysicalPlan, exec: &E) -> Option<usize> {
    match plan {
        PhysicalPlan::BgpScan { patterns } => exec.cardinality_estimate(patterns),
        // A pushed-down COUNT always yields exactly one row.
        PhysicalPlan::CountScan { .. } => Some(1),
        PhysicalPlan::Values { rows, .. } => Some(rows.len()),
        // Join: upper-bounded by the product of inputs. We keep the
        // textbook product (an upper bound when join vars are absent) but
        // never below the larger input, since a join with shared vars can
        // only shrink, not grow past, the cartesian product.
        PhysicalPlan::Join { left, right } => {
            combine2(left, right, exec, |l, r| l.saturating_mul(r))
        }
        // Left-join keeps at least every left row.
        PhysicalPlan::LeftJoin { left, right, .. } => {
            combine2(left, right, exec, |l, r| l.max(l.saturating_mul(r)))
        }
        PhysicalPlan::Union { left, right } => {
            combine2(left, right, exec, |l, r| l.saturating_add(r))
        }
        // Filter / Distinct shrink (or keep) the row count; we report the
        // input estimate as an upper bound.
        PhysicalPlan::Filter { inner, .. }
        | PhysicalPlan::Distinct { inner }
        | PhysicalPlan::Project { inner, .. }
        | PhysicalPlan::Extend { inner, .. }
        | PhysicalPlan::OrderBy { inner, .. }
        | PhysicalPlan::Group { inner, .. } => estimate(inner, exec),
        // Slice caps the row count at `length` (when present).
        PhysicalPlan::Slice { inner, length, .. } => {
            let inner = estimate(inner, exec)?;
            Some(match length {
                Some(len) => inner.min(*len),
                None => inner,
            })
        }
        // The transitive/reflexive closure of an edge relation can grow
        // super-linearly; we have no closed-form estimate, so report the
        // edge cardinality as a (loose) lower bound rather than guess.
        PhysicalPlan::PathClosure { edge, .. } => estimate(edge, exec),
    }
}

fn combine2<E: Executor + ?Sized>(
    left: &PhysicalPlan,
    right: &PhysicalPlan,
    exec: &E,
    f: impl Fn(usize, usize) -> usize,
) -> Option<usize> {
    let l = estimate(left, exec)?;
    let r = estimate(right, exec)?;
    Some(f(l, r))
}

/// The operator label shown for a node (no children).
fn node_label(plan: &PhysicalPlan) -> String {
    match plan {
        PhysicalPlan::BgpScan { patterns } => {
            format!(
                "BgpScan({} pattern{})",
                patterns.len(),
                plural(patterns.len())
            )
        }
        PhysicalPlan::CountScan { patterns, out_var } => {
            format!(
                "CountScan({} pattern{} -> ?{})",
                patterns.len(),
                plural(patterns.len()),
                out_var.name()
            )
        }
        PhysicalPlan::Join { .. } => "Join".to_owned(),
        PhysicalPlan::LeftJoin { expr, .. } => {
            if expr.is_some() {
                "LeftJoin(with filter)".to_owned()
            } else {
                "LeftJoin".to_owned()
            }
        }
        PhysicalPlan::Filter { .. } => "Filter".to_owned(),
        PhysicalPlan::Union { .. } => "Union".to_owned(),
        PhysicalPlan::Project { vars, .. } => {
            let names: Vec<&str> = vars.iter().map(|v| v.name()).collect();
            format!("Project(?{})", names.join(", ?"))
        }
        PhysicalPlan::Distinct { .. } => "Distinct".to_owned(),
        PhysicalPlan::Slice { start, length, .. } => match length {
            Some(len) => format!("Slice(offset={start}, limit={len})"),
            None => format!("Slice(offset={start})"),
        },
        PhysicalPlan::OrderBy { keys, .. } => {
            format!("OrderBy({} key{})", keys.len(), plural(keys.len()))
        }
        PhysicalPlan::Extend { var, .. } => format!("Extend(?{})", var.name()),
        PhysicalPlan::Values { vars, rows } => {
            format!(
                "Values({} var{}, {} row{})",
                vars.len(),
                plural(vars.len()),
                rows.len(),
                plural(rows.len())
            )
        }
        PhysicalPlan::Group {
            keys, aggregates, ..
        } => {
            format!(
                "Group({} key{}, {} aggregate{})",
                keys.len(),
                plural(keys.len()),
                aggregates.len(),
                plural(aggregates.len())
            )
        }
        PhysicalPlan::PathClosure { reflexive, .. } => {
            if *reflexive {
                "PathClosure(reflexive+transitive, p*)".to_owned()
            } else {
                "PathClosure(transitive, p+)".to_owned()
            }
        }
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// The direct children of a node, in render order.
fn children(plan: &PhysicalPlan) -> Vec<&PhysicalPlan> {
    match plan {
        PhysicalPlan::BgpScan { .. }
        | PhysicalPlan::CountScan { .. }
        | PhysicalPlan::Values { .. } => vec![],
        PhysicalPlan::Join { left, right }
        | PhysicalPlan::LeftJoin { left, right, .. }
        | PhysicalPlan::Union { left, right } => vec![left, right],
        PhysicalPlan::Filter { inner, .. }
        | PhysicalPlan::Distinct { inner }
        | PhysicalPlan::Project { inner, .. }
        | PhysicalPlan::Slice { inner, .. }
        | PhysicalPlan::OrderBy { inner, .. }
        | PhysicalPlan::Extend { inner, .. }
        | PhysicalPlan::Group { inner, .. } => vec![inner],
        PhysicalPlan::PathClosure { edge, .. } => vec![edge],
    }
}

fn render_text<E: Executor + ?Sized>(plan: &PhysicalPlan, exec: &E, mode: ExecutionMode) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "EXPLAIN");
    let _ = writeln!(
        out,
        "mode: {} (backward-chaining not yet available)",
        mode.label()
    );
    out.push_str("plan:\n");
    render_text_node(plan, exec, 0, &mut out);
    out
}

fn render_text_node<E: Executor + ?Sized>(
    plan: &PhysicalPlan,
    exec: &E,
    depth: usize,
    out: &mut String,
) {
    let indent = "  ".repeat(depth);
    let card = match estimate(plan, exec) {
        Some(n) => format!("~{n} rows"),
        None => "~? rows".to_owned(),
    };
    let _ = writeln!(out, "{indent}{} [{card}]", node_label(plan));
    for child in children(plan) {
        render_text_node(child, exec, depth + 1, out);
    }
}

fn render_json<E: Executor + ?Sized>(plan: &PhysicalPlan, exec: &E, mode: ExecutionMode) -> String {
    let mut out = String::new();
    out.push('{');
    let _ = write!(out, "\"mode\":\"{}\",", mode.label());
    out.push_str("\"backwardChainingAvailable\":false,");
    out.push_str("\"plan\":");
    render_json_node(plan, exec, &mut out);
    out.push('}');
    out
}

fn render_json_node<E: Executor + ?Sized>(plan: &PhysicalPlan, exec: &E, out: &mut String) {
    out.push('{');
    let _ = write!(out, "\"op\":{},", json_string(&node_label(plan)));
    match estimate(plan, exec) {
        Some(n) => {
            let _ = write!(out, "\"estRows\":{n},");
        }
        None => out.push_str("\"estRows\":null,"),
    }
    out.push_str("\"children\":[");
    let kids = children(plan);
    for (i, child) in kids.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        render_json_node(child, exec, out);
    }
    out.push_str("]}");
}

/// Minimal JSON string escaper — enough for operator labels (which only
/// contain ASCII identifiers, punctuation, and `?var` names; SPARQL
/// VARNAME excludes `"` and control chars).
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algebra::{Term, TriplePattern, Var};
    use crate::exec::mem::MemStore;

    fn store() -> MemStore {
        let mut st = MemStore::default();
        st.insert(("a".into(), "sco".into(), "b".into()));
        st.insert(("b".into(), "sco".into(), "c".into()));
        st.insert(("x".into(), "type".into(), "T".into()));
        st
    }

    fn scan(p: &str) -> PhysicalPlan {
        PhysicalPlan::BgpScan {
            patterns: vec![TriplePattern {
                subject: Term::Var(Var::new("s")),
                predicate: Term::Iri(p.to_owned()),
                object: Term::Var(Var::new("o")),
            }],
        }
    }

    #[test]
    fn text_shows_mode_and_cardinality() {
        let st = store();
        let plan = scan("sco");
        let text = explain(&plan, &st, ExecutionMode::Materialized, ExplainFormat::Text);
        assert!(text.contains("mode: materialized"), "{text}");
        assert!(text.contains("BgpScan(1 pattern)"), "{text}");
        // two `sco` triples
        assert!(text.contains("~2 rows"), "{text}");
    }

    #[test]
    fn path_closure_node_rendered_with_mode() {
        // The acceptance-#5 shape: a recursive Kleene path over `sco`.
        let st = store();
        let edge = scan("sco");
        let plan = PhysicalPlan::PathClosure {
            subject: Term::Var(Var::new("x")),
            object: Term::Var(Var::new("y")),
            edge: Box::new(edge),
            reflexive: false,
        };
        let text = explain(&plan, &st, ExecutionMode::Materialized, ExplainFormat::Text);
        assert!(text.contains("PathClosure(transitive, p+)"), "{text}");
        assert!(text.contains("BgpScan"), "{text}");
        assert!(text.contains("mode: materialized"), "{text}");
    }

    #[test]
    fn json_is_well_formed_tree() {
        let st = store();
        let plan = PhysicalPlan::Project {
            vars: vec![Var::new("o")],
            inner: Box::new(scan("sco")),
        };
        let json = explain(&plan, &st, ExecutionMode::Materialized, ExplainFormat::Json);
        assert!(json.starts_with('{'), "{json}");
        assert!(json.ends_with('}'), "{json}");
        assert!(json.contains("\"mode\":\"materialized\""), "{json}");
        assert!(json.contains("\"op\":\"Project(?o)\""), "{json}");
        assert!(json.contains("\"estRows\":2"), "{json}");
        assert!(json.contains("\"children\":["), "{json}");
        // Parseable as JSON.
        let v: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(v["mode"], "materialized");
        assert_eq!(v["plan"]["children"][0]["op"], "BgpScan(1 pattern)");
    }
}
