---
status: draft
date: 2026-07-07
scope: "SPEC-23 Phase 2 — the four always-beneficial heuristic rewrite passes (Normalize, FilterPullup, FilterPushdown, ProjectionPushdown) registered into the Phase-1 logical pass pipeline, each individually disable-able and guarded by a result-invariance (slot-differential) suite"
---

# Heuristic rewrite passes (SPEC-23 Phase 2) — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Depends on: [PLAN-23-01](PLAN-23-01-logical-ir-and-pass-registry.md)** (SPEC-23 Phase 1 — the logical IR, the binding/type lattice, and the pass registry). This plan does not build any of that scaffolding; it *registers real passes into the registry Phase 1 already built*. Phase 1 must be landed on the branch before Task 1 begins. The Phase-1 surface consumed verbatim here is listed under **Architecture → Consumed Phase-1 surface**.

**Goal:** Implement SPEC-23 §6 item 2 — the four heuristic, always-beneficial, statistics-free rewrite passes and wire them into the fixed pass pipeline in source order after `CoalesceBgp`: `Normalize` → `FilterPullup` → `FilterPushdown` → `ProjectionPushdown` (spec §5.2). Each is a `LogicalPass` impl (`PassId` already reserved by Phase 1), declares its `must_follow` ordering constraint, is individually disable-able via `PlanCtx.disabled_passes`, and is validated after it runs. **No result changes** — only plan *shape* changes (fewer scanned columns, hoisted-then-pushed filters, `Equal→SameTerm` strength reduction). Every pass is gated by a result-invariance ("slot-differential") battery: run each query WITH and WITHOUT the pass and assert identical result multisets ([#185](https://github.com/sunstoneinstitute/horndb/issues/185)).

**Architecture:**

The Phase-1 SPARQL planner already routes `Algebra → LogicalPlan → run_passes → lower → PhysicalPlan`. Phase 2 appends four passes to the registry's default list. Each pass is a pure `LogicalPlan → LogicalPlan` function behind the `LogicalPass` trait; the driver (`run_passes`) runs them in the declared order, skips any in `ctx.disabled_passes`, and (in debug builds) re-runs `types::infer` after each to catch a pass that leaves a dangling variable. The design borrows are:

- **`sparopt`** (Oxigraph, already a transitive dep): the `Equal→SameTerm` strength reduction and the binding/type lattice that makes it (and filter-pushdown legality) *sound* — a filter/reduction is legal only where the lattice proves the operands bound (and, for the reduction, provably a `NamedNode`/`BlankNode`, where term-equality and value-equality coincide). Single-shot, no fixpoint.
- **DuckDB**: filter **pull-up then push-down** — hoist every conjunct to one `Filter` above the join so the pushdown step sees the complete predicate set, then push each conjunct to the deepest subtree that binds all its variables. Also DuckDB's logical/physical split: this is *logical* pushdown (new); the existing physical `plan/pushdown.rs` (count-pushdown + column pruning at the `PhysicalPlan` level) stays.

**The `Equal→SameTerm` target representation (verified against the code, 2026-07-07):** HornDB has **no** `SameTerm` in either `Expr` or `Func`; `algebra/translate.rs:382-383` lowers *both* `Equal` and `sameTerm` to `Expr::Eq`, and `Expr::Eq` already evaluates as **structural term equality** (`eval_expr_to_term(a)? == eval_expr_to_term(c)?`, `exec/runtime.rs:1481`). So today `Eq` *is* `sameTerm`. The faithful, future-proof target is a new **`Expr::SameTerm(Box<Expr>, Box<Expr>)`** node whose runtime arm is identical to today's `Eq` term-compare (Task 1). The reduction is therefore result-invariant **now** (both term-compare) and becomes a genuine strength reduction the day `Expr::Eq` grows value-equality semantics (numeric type promotion) — at which point `SameTerm` already carries the cheaper, correct term-equality meaning. The reduction is gated so it *never* fires on literal operands or on variables whose kind is not provably a single `NamedNode`/`BlankNode`, which keeps the existing count-pushdown equality-inlining (`plan/pushdown.rs::eq_conjuncts`, which matches `Expr::Eq` under a `_` wildcard) intact.

**Consumed Phase-1 surface (do not redefine; import verbatim):**

Modules under `crates/sparql/src/plan/`: `logical.rs`, `types.rs`, `pass.rs`, `lower.rs`.

```rust
// logical.rs
pub enum LogicalPlan {
    Bgp { patterns: Vec<TriplePattern> },
    Join { left: Box<LogicalPlan>, right: Box<LogicalPlan> },
    LeftJoin { left: Box<LogicalPlan>, right: Box<LogicalPlan>, expr: Option<Expr> },
    Filter { expr: Expr, inner: Box<LogicalPlan> },
    Union { left: Box<LogicalPlan>, right: Box<LogicalPlan> },
    Project { vars: Vec<Var>, inner: Box<LogicalPlan> },
    Distinct { inner: Box<LogicalPlan> },
    Slice { inner: Box<LogicalPlan>, start: usize, length: Option<usize> },
    OrderBy { inner: Box<LogicalPlan>, keys: Vec<(Expr, OrderDir)> },
    Extend { inner: Box<LogicalPlan>, var: Var, expr: Expr },
    Values { vars: Vec<Var>, rows: Vec<Vec<Option<Term>>> },
    Group { inner: Box<LogicalPlan>, keys: Vec<Var>, aggregates: Vec<Aggregate> },
    PathClosure { subject: Term, object: Term, edge: Box<LogicalPlan>, reflexive: bool },
}
impl LogicalPlan { // smart constructors fold identities/empties at build time
    pub fn join(l: LogicalPlan, r: LogicalPlan) -> LogicalPlan;
    pub fn filter(expr: Expr, inner: LogicalPlan) -> LogicalPlan;
    pub fn union(l: LogicalPlan, r: LogicalPlan) -> LogicalPlan;
}
// types.rs
pub struct TypeMask(u8);
impl TypeMask { pub fn is_bound(&self) -> bool; }
pub struct VarTypes(std::collections::HashMap<Var, TypeMask>);
pub fn infer(plan: &LogicalPlan) -> VarTypes;
// pass.rs
pub enum PassId { CoalesceBgp, Normalize, FilterPullup, FilterPushdown, ProjectionPushdown, JoinPlanning }
pub struct PlanCtx { pub disabled_passes: std::collections::HashSet<PassId> }
pub trait LogicalPass {
    fn id(&self) -> PassId;
    fn run(&self, plan: LogicalPlan, ctx: &PlanCtx) -> LogicalPlan;
    fn must_follow(&self) -> &'static [PassId] { &[] }
}
pub fn run_passes(plan: LogicalPlan, passes: &[Box<dyn LogicalPass>], ctx: &PlanCtx) -> LogicalPlan;
```

Additionally consumed from Phase 1 (established by PLAN-23-01, named here so this plan's tests compile):

- `crate::plan::logical::from_algebra(alg: &Algebra) -> LogicalPlan` — the `Algebra → LogicalPlan` entry (does the `CoalesceBgp` flattening as its first pass).
- `crate::plan::lower::lower(plan: LogicalPlan) -> Result<PhysicalPlan>` — logical → physical.
- `crate::plan::pass::default_passes() -> Vec<Box<dyn LogicalPass>>` — the registry's ordered default list. **Phase 2's wiring change is to append the four new passes to this function** (Tasks 2-5), so `crate::plan::planner::plan` (which calls `from_algebra` → `run_passes(default_passes())` → `lower`) picks them up with no further change.
- `crate::plan::types` provides per-variable kind bits behind the `TypeMask` bitset. Phase 2 adds the two accessors it needs (`is_named_node`, `is_blank_node`) as an *additive* extension to `TypeMask` in Task 1 — it does not rename or re-shape any pinned item.

Everything else Phase 2 needs (a `LogicalPlan` structural `schema`, conjunct split/join, a child-mapping combinator) is **Phase-2-local**, defined once in `plan/passes/mod.rs` (Task 2, Step 3) so the passes stay self-contained and do not reach into Phase-1 internals beyond the surface above.

**Tech Stack:** Rust 1.90 (workspace-pinned), `crates/sparql` only. **No new dependencies.** `std::collections::{HashSet, HashMap}` for variable sets; the binding/type lattice comes from `crate::plan::types`.

**Verification runner:** `cargo nextest run -p horndb-sparql` (production crates only; see root `CLAUDE.md`). Full SPARQL pass also `--features server`. Conformance / WCOJ differential fuzzer via `cargo nextest run --workspace`. **Benchmarks only on `hornbench`, never the laptop** — Phase 2 targets plan shape, not throughput, so no recorded bench is required (Task 6 states the smoke-check that is *not* recorded).

**File map (all under `crates/sparql/` unless noted):**

| File | Change |
|---|---|
| `src/algebra/mod.rs` | add `Expr::SameTerm(Box<Expr>, Box<Expr>)` variant |
| `src/exec/runtime.rs` | `SameTerm` arms in `referenced_vars`, `eval_expr`, `eval_expr_to_term` |
| `src/plan/types.rs` | **additive** `TypeMask::is_named_node` / `is_blank_node` accessors (extends Phase-1 file) |
| `src/plan/passes/mod.rs` | **new** — module root; Phase-2-local helpers (`schema`, `conjuncts`, `conjoin`, `map_children`, `bound_vars`) |
| `src/plan/passes/normalize.rs` | **new** — `Normalize` pass |
| `src/plan/passes/filter_pullup.rs` | **new** — `FilterPullup` pass |
| `src/plan/passes/filter_pushdown.rs` | **new** — `FilterPushdown` pass |
| `src/plan/passes/projection_pushdown.rs` | **new** — `ProjectionPushdown` pass |
| `src/plan/pass.rs` | append the four passes to `default_passes()` (Phase-1 file) |
| `src/plan/mod.rs` | `pub mod passes;` |
| `tests/rewrite_invariance.rs` | **new** — the slot-differential result-invariance battery + per-`PassId` bisection test |
| `INTEGRATION-NOTES.md` | record the four passes and the `Expr::SameTerm` seam |

Do **not** touch `TASKS.md`, `docs/benchmarks.md`, `docs/architecture.md`, `docs/metrics.md`, or `docs/index.md` — the integrating session syncs those (root `CLAUDE.md` doc-sync rule) when this branch merges. The `SPEC-23` status stays `draft` until the epic closes.

---

### Task 1: `Expr::SameTerm` node + runtime term-equality arm

The `Normalize` pass (Task 2) needs a target for `Equal→SameTerm`. Add the node and give it a runtime arm **identical to today's `Eq`** (structural term equality), so introducing it is result-invariant. This task changes only the algebra and the runtime; no pass exists yet.

**Files:**
- Modify: `crates/sparql/src/algebra/mod.rs:61-89` (the `Expr` enum)
- Modify: `crates/sparql/src/exec/runtime.rs` (`referenced_vars` ~986, `eval_expr` ~1481, `eval_expr_to_term` ~1587)
- Modify: `crates/sparql/src/plan/types.rs` (additive `TypeMask` accessors)
- Test: `crates/sparql/src/exec/runtime.rs` (in-file `#[cfg(test)]` module — append a test)

- [ ] **Step 1: Write the failing test**

Append to the existing `#[cfg(test)] mod tests` in `crates/sparql/src/exec/runtime.rs` (it already constructs `Bindings` and `Term`; if no test module exists yet, create `#[cfg(test)] mod sameterm_tests` at end of file with the imports shown):

```rust
#[cfg(test)]
mod sameterm_tests {
    use super::*;
    use crate::algebra::{Expr, Term, Var};

    fn bound(name: &str, t: Term) -> Bindings {
        let mut b = Bindings::new();
        b.set(name, t);
        b
    }

    /// `SameTerm(?x, <iri>)` evaluates exactly like `Eq(?x, <iri>)` today:
    /// structural term equality. This is the invariant that makes the
    /// Normalize Eq->SameTerm reduction result-preserving.
    #[test]
    fn sameterm_matches_eq_term_semantics() {
        let iri = Term::Iri("http://ex/a".into());
        let b_hit = bound("x", iri.clone());
        let b_miss = bound("x", Term::Iri("http://ex/b".into()));
        let x = || Box::new(Expr::Term(Term::Var(Var::new("x"))));
        let c = || Box::new(Expr::Term(iri.clone()));

        let same = Expr::SameTerm(x(), c());
        let eq = Expr::Eq(x(), c());
        assert_eq!(eval_expr(&same, &b_hit).unwrap(), eval_expr(&eq, &b_hit).unwrap());
        assert_eq!(eval_expr(&same, &b_miss).unwrap(), eval_expr(&eq, &b_miss).unwrap());
        assert!(eval_expr(&same, &b_hit).unwrap());
        assert!(!eval_expr(&same, &b_miss).unwrap());
    }

    /// `referenced_vars` must descend into `SameTerm` (else FilterPushdown
    /// would mis-scope a SameTerm conjunct).
    #[test]
    fn sameterm_referenced_vars() {
        let e = Expr::SameTerm(
            Box::new(Expr::Term(Term::Var(Var::new("p")))),
            Box::new(Expr::Term(Term::Var(Var::new("q")))),
        );
        let mut vars = std::collections::HashSet::new();
        referenced_vars(&e, &mut vars);
        assert_eq!(vars, ["p".to_string(), "q".to_string()].into_iter().collect());
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo nextest run -p horndb-sparql -E 'test(sameterm_matches_eq_term_semantics) or test(sameterm_referenced_vars)'`
Expected: build FAILURE — `no variant named SameTerm found for enum Expr`.

- [ ] **Step 3: Add the `Expr::SameTerm` variant**

In `crates/sparql/src/algebra/mod.rs`, add the variant right after `Ne` (keep it next to the other comparisons, `Expr` derives `Debug, Clone, PartialEq`):

```rust
    Eq(Box<Expr>, Box<Expr>),
    /// Term equality (`sameTerm`) — the strength-reduced form of `Eq` for
    /// operands the type lattice proves are the same non-literal kind
    /// (`plan::passes::normalize`). Evaluated as structural `Term`
    /// equality, identical to `Eq` today; the two diverge only once `Eq`
    /// gains value-equality (numeric promotion) semantics.
    SameTerm(Box<Expr>, Box<Expr>),
    Ne(Box<Expr>, Box<Expr>),
```

- [ ] **Step 4: Add the three runtime arms**

Only three exhaustive `match`es on `Expr` exist (`referenced_vars`, `eval_expr`, `eval_expr_to_term`; `plan/pushdown.rs::eq_conjuncts` has a `_` arm and needs no change). In `crates/sparql/src/exec/runtime.rs`:

`referenced_vars` (~986) — add `SameTerm` to the two-operand group:

```rust
        Expr::Eq(a, b)
        | Expr::SameTerm(a, b)
        | Expr::Ne(a, b)
        | Expr::Lt(a, b)
```

`eval_expr` (~1481) — add a `SameTerm` arm identical to `Eq`, immediately after the `Eq` arm:

```rust
        Expr::Eq(a, c) => eval_expr_to_term(a, b)? == eval_expr_to_term(c, b)?,
        Expr::SameTerm(a, c) => eval_expr_to_term(a, b)? == eval_expr_to_term(c, b)?,
        Expr::Ne(a, c) => eval_expr_to_term(a, b)? != eval_expr_to_term(c, b)?,
```

`eval_expr_to_term` (~1587) — add `SameTerm` to the boolean-typed group (returns the `"true"`/`"false"` literal like the other predicates):

```rust
        Expr::Eq(_, _)
        | Expr::SameTerm(_, _)
        | Expr::Ne(_, _)
        | Expr::Lt(_, _)
```

- [ ] **Step 5: Add the additive `TypeMask` kind accessors**

In `crates/sparql/src/plan/types.rs`, extend the `impl TypeMask` block (the bit layout is Phase-1's ported sparopt lattice `{undef, named_node, blank_node, literal, triple}`; use the constants Phase 1 defined for those bits — shown here as `NAMED_NODE` / `BLANK_NODE`):

```rust
impl TypeMask {
    // ... Phase-1 `is_bound` and constructors ...

    /// True iff the variable is provably bound to exactly a `NamedNode`
    /// (IRI) — bound, and no other kind bit set. Used by `Normalize` to
    /// prove an `Eq` is safe to reduce to `SameTerm` (term-equality and
    /// value-equality coincide on IRIs).
    pub fn is_named_node(&self) -> bool {
        self.is_bound() && self.0 & !Self::NAMED_NODE == 0 && self.0 & Self::NAMED_NODE != 0
    }

    /// True iff the variable is provably bound to exactly a `BlankNode`.
    pub fn is_blank_node(&self) -> bool {
        self.is_bound() && self.0 & !Self::BLANK_NODE == 0 && self.0 & Self::BLANK_NODE != 0
    }
}
```

If Phase 1 named the bit constants differently, use those names — the semantics ("bound and exactly this one kind bit") are what matter. Do not add these to the pinned interface list; they are internal helpers.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo nextest run -p horndb-sparql -E 'test(sameterm_matches_eq_term_semantics) or test(sameterm_referenced_vars)'`
Expected: PASS (2 tests).

Run: `cargo nextest run -p horndb-sparql`
Expected: PASS — no existing arm produces `SameTerm`, so behavior is unchanged; the enum addition compiles everywhere (only the 3 runtime matches were exhaustive).

- [ ] **Step 7: Commit**

```bash
cargo fmt --all
git add crates/sparql/src/algebra/mod.rs crates/sparql/src/exec/runtime.rs crates/sparql/src/plan/types.rs
git commit -m 'feat(sparql): add Expr::SameTerm term-equality node for Normalize (#185)'
```

---

### Task 2: `Normalize` pass — constant folding + `Equal→SameTerm`

`PassId::Normalize`, `must_follow: [CoalesceBgp]`. Two rewrites, both single-shot: (a) simplify boolean-connective identities and drop/empty constant filters; (b) reduce `Eq(a,b) → SameTerm(a,b)` where the lattice proves both operands the same non-literal kind. This task also creates the shared `passes/` module and its helpers.

**Files:**
- Create: `crates/sparql/src/plan/passes/mod.rs`, `crates/sparql/src/plan/passes/normalize.rs`
- Modify: `crates/sparql/src/plan/mod.rs` (`pub mod passes;`)
- Modify: `crates/sparql/src/plan/pass.rs` (`default_passes()` append)
- Test: unit tests inside `normalize.rs`

- [ ] **Step 1: Write the failing tests**

Create `crates/sparql/src/plan/passes/normalize.rs` with tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::algebra::{Expr, Term, TriplePattern, Var};
    use crate::plan::logical::LogicalPlan;
    use crate::plan::pass::{LogicalPass, PassId, PlanCtx};

    fn var(n: &str) -> Term { Term::Var(Var::new(n)) }
    fn ctx() -> PlanCtx { PlanCtx { disabled_passes: Default::default() } }

    /// A predicate-position variable is provably a NamedNode, so
    /// `Eq(?p, <iri>)` reduces to `SameTerm`.
    #[test]
    fn reduces_eq_on_provable_iri_to_sameterm() {
        let bgp = LogicalPlan::Bgp {
            patterns: vec![TriplePattern { subject: var("s"), predicate: var("p"), object: var("o") }],
        };
        let eq = Expr::Eq(
            Box::new(Expr::Term(var("p"))),
            Box::new(Expr::Term(Term::Iri("http://ex/knows".into()))),
        );
        let plan = LogicalPlan::Filter { expr: eq, inner: Box::new(bgp) };
        let out = Normalize.run(plan, &ctx());
        let LogicalPlan::Filter { expr, .. } = out else { panic!("expected Filter, got {out:?}") };
        assert!(matches!(expr, Expr::SameTerm(..)), "predicate-var Eq must reduce; got {expr:?}");
    }

    /// An object-position variable's kind is NOT provably singular (could be
    /// literal), so an `Eq` against a literal constant must NOT reduce —
    /// this preserves the physical count-pushdown equality inlining.
    #[test]
    fn keeps_eq_on_unprovable_kind() {
        let bgp = LogicalPlan::Bgp {
            patterns: vec![TriplePattern { subject: var("s"), predicate: Term::Iri("http://ex/name".into()), object: var("o") }],
        };
        let eq = Expr::Eq(
            Box::new(Expr::Term(var("o"))),
            Box::new(Expr::Term(Term::Literal("\"Alice\"".into()))),
        );
        let plan = LogicalPlan::Filter { expr: eq, inner: Box::new(bgp) };
        let out = Normalize.run(plan, &ctx());
        let LogicalPlan::Filter { expr, .. } = out else { panic!("expected Filter") };
        assert!(matches!(expr, Expr::Eq(..)), "literal-side Eq must NOT reduce; got {expr:?}");
    }

    /// A constant-true conjunct is dropped; if it was the whole predicate,
    /// the Filter is removed.
    #[test]
    fn drops_constant_true_filter() {
        let bgp = LogicalPlan::Bgp {
            patterns: vec![TriplePattern { subject: var("s"), predicate: var("p"), object: var("o") }],
        };
        // Eq(<iri>, <iri>) with two identical ground IRIs is constant true.
        let t = Expr::Eq(
            Box::new(Expr::Term(Term::Iri("http://ex/a".into()))),
            Box::new(Expr::Term(Term::Iri("http://ex/a".into()))),
        );
        let plan = LogicalPlan::Filter { expr: t, inner: Box::new(bgp) };
        let out = Normalize.run(plan, &ctx());
        assert!(matches!(out, LogicalPlan::Bgp { .. }), "true filter must be dropped; got {out:?}");
    }

    /// A constant-false filter becomes an empty relation carrying the inner
    /// schema.
    #[test]
    fn empties_constant_false_filter() {
        let bgp = LogicalPlan::Bgp {
            patterns: vec![TriplePattern { subject: var("s"), predicate: var("p"), object: var("o") }],
        };
        let f = Expr::Eq(
            Box::new(Expr::Term(Term::Iri("http://ex/a".into()))),
            Box::new(Expr::Term(Term::Iri("http://ex/b".into()))),
        );
        let plan = LogicalPlan::Filter { expr: f, inner: Box::new(bgp) };
        let out = Normalize.run(plan, &ctx());
        let LogicalPlan::Values { vars, rows } = out else { panic!("expected empty Values, got {out:?}") };
        assert!(rows.is_empty());
        assert_eq!(vars, vec![Var::new("s"), Var::new("p"), Var::new("o")]);
    }

    #[test]
    fn id_and_ordering() {
        assert_eq!(Normalize.id(), PassId::Normalize);
        assert_eq!(Normalize.must_follow(), &[PassId::CoalesceBgp]);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo nextest run -p horndb-sparql -E 'binary(horndb-sparql) and package(horndb-sparql)' 2>&1 | head` — or directly:
Run: `cargo nextest run -p horndb-sparql plan::passes::normalize`
Expected: build FAILURE — `plan::passes` module does not exist / `cannot find type Normalize`.

- [ ] **Step 3: Create the shared `passes/mod.rs` helpers**

Create `crates/sparql/src/plan/passes/mod.rs`:

```rust
//! SPEC-23 Phase 2 heuristic rewrite passes and their shared helpers.
//!
//! Each pass is a pure `LogicalPlan -> LogicalPlan` behind
//! [`crate::plan::pass::LogicalPass`], registered in source order after
//! `CoalesceBgp` by [`crate::plan::pass::default_passes`]. The helpers here
//! keep the passes self-contained: a structural [`schema`], conjunct
//! [`conjuncts`]/[`conjoin`] split-and-rebuild, a child-mapping combinator
//! [`map_children`], and the lattice-derived [`bound_vars`] set.

pub mod filter_pullup;
pub mod filter_pushdown;
pub mod normalize;
pub mod projection_pushdown;

pub use filter_pullup::FilterPullup;
pub use filter_pushdown::FilterPushdown;
pub use normalize::Normalize;
pub use projection_pushdown::ProjectionPushdown;

use crate::algebra::{Expr, Term, TriplePattern, Var};
use crate::plan::logical::LogicalPlan;
use crate::plan::types;
use std::collections::HashSet;

/// A node's natural output variables, structurally, in a deterministic
/// order (mirror of `plan::pushdown::output_vars` at the logical level).
/// Used to build empty-relation schemas and restricting `Project`s.
pub(crate) fn schema(node: &LogicalPlan) -> Vec<Var> {
    use LogicalPlan::*;
    let mut out: Vec<Var> = Vec::new();
    let mut push = |v: &Var, out: &mut Vec<Var>| {
        if !out.iter().any(|x| x == v) {
            out.push(v.clone());
        }
    };
    match node {
        Bgp { patterns } => {
            for p in patterns {
                collect_pattern_vars(p, &mut out);
            }
        }
        Join { left, right } | LeftJoin { left, right, .. } | Union { left, right } => {
            out = schema(left);
            for v in schema(right) {
                push(&v, &mut out);
            }
        }
        Filter { inner, .. } | Distinct { inner } | Slice { inner, .. } | OrderBy { inner, .. } => {
            out = schema(inner);
        }
        Project { vars, .. } => out = vars.clone(),
        Extend { inner, var, .. } => {
            out = schema(inner);
            push(var, &mut out);
        }
        Values { vars, .. } => out = vars.clone(),
        Group { keys, aggregates, .. } => {
            for k in keys {
                push(k, &mut out);
            }
            for a in aggregates {
                push(&a.out, &mut out);
            }
        }
        PathClosure { subject, object, .. } => {
            for t in [subject, object] {
                if let Term::Var(v) = t {
                    push(v, &mut out);
                }
            }
        }
    }
    out
}

fn collect_pattern_vars(p: &TriplePattern, out: &mut Vec<Var>) {
    for t in [&p.subject, &p.predicate, &p.object] {
        collect_term_vars(t, out);
    }
}
fn collect_term_vars(t: &Term, out: &mut Vec<Var>) {
    match t {
        Term::Var(v) => {
            if !out.iter().any(|x| x == v) {
                out.push(v.clone());
            }
        }
        Term::Triple(tp) => collect_pattern_vars(tp, out),
        _ => {}
    }
}

/// Flatten a conjunction into its top-level conjuncts (recursing only
/// through `And`). A non-`And` expression is a single conjunct.
pub(crate) fn conjuncts(expr: Expr, out: &mut Vec<Expr>) {
    match expr {
        Expr::And(a, b) => {
            conjuncts(*a, out);
            conjuncts(*b, out);
        }
        other => out.push(other),
    }
}

/// Rebuild a right-leaning `And` chain from conjuncts. Empty -> `None`.
pub(crate) fn conjoin(mut parts: Vec<Expr>) -> Option<Expr> {
    let first = parts.pop()?; // build from the back for a stable shape
    let mut acc = first;
    while let Some(next) = parts.pop() {
        acc = Expr::And(Box::new(next), Box::new(acc));
    }
    Some(acc)
}

/// The variables a subtree binds in *every* solution (lattice-derived):
/// `infer(plan)` filtered by `TypeMask::is_bound`. FilterPushdown legality
/// rests on this — a conjunct may only be pushed where all its vars are
/// bound.
pub(crate) fn bound_vars(node: &LogicalPlan) -> HashSet<String> {
    types::infer(node)
        .0
        .iter()
        .filter(|(_, m)| m.is_bound())
        .map(|(v, _)| v.name().to_owned())
        .collect()
}

/// Apply `f` to each direct child of `node`, rebuilding it. Leaves
/// (`Bgp`, `Values`) are returned unchanged. Mirror of
/// `plan::pushdown::map_children` at the logical level.
pub(crate) fn map_children(node: LogicalPlan, f: &dyn Fn(LogicalPlan) -> LogicalPlan) -> LogicalPlan {
    use LogicalPlan::*;
    match node {
        leaf @ (Bgp { .. } | Values { .. }) => leaf,
        Join { left, right } => Join { left: Box::new(f(*left)), right: Box::new(f(*right)) },
        LeftJoin { left, right, expr } => LeftJoin { left: Box::new(f(*left)), right: Box::new(f(*right)), expr },
        Union { left, right } => Union { left: Box::new(f(*left)), right: Box::new(f(*right)) },
        Filter { expr, inner } => Filter { expr, inner: Box::new(f(*inner)) },
        Project { vars, inner } => Project { vars, inner: Box::new(f(*inner)) },
        Distinct { inner } => Distinct { inner: Box::new(f(*inner)) },
        Slice { inner, start, length } => Slice { inner: Box::new(f(*inner)), start, length },
        OrderBy { inner, keys } => OrderBy { inner: Box::new(f(*inner)), keys },
        Extend { inner, var, expr } => Extend { inner: Box::new(f(*inner)), var, expr },
        Group { inner, keys, aggregates } => Group { inner: Box::new(f(*inner)), keys, aggregates },
        PathClosure { subject, object, edge, reflexive } => {
            PathClosure { subject, object, edge: Box::new(f(*edge)), reflexive }
        }
    }
}
```

(If Phase 1's `logical::LogicalPlan` variants differ in field order from the pinned contract, use its exact form — the pinned contract is the source of truth for names.)

Add to `crates/sparql/src/plan/mod.rs`:

```rust
pub mod passes;
```

- [ ] **Step 4: Implement the `Normalize` pass**

Prepend to `crates/sparql/src/plan/passes/normalize.rs` (above the test module):

```rust
//! `Normalize` (SPEC-23 §5.2): boolean-connective simplification + constant
//! filter folding + `Eq -> SameTerm` strength reduction. Single-shot,
//! statistics-free, result-invariant.

use crate::algebra::{Expr, Term};
use crate::plan::logical::LogicalPlan;
use crate::plan::pass::{LogicalPass, PassId, PlanCtx};
use crate::plan::passes::{map_children, schema};
use crate::plan::types::{self, VarTypes};

/// The `Normalize` logical pass. See module docs.
pub struct Normalize;

impl LogicalPass for Normalize {
    fn id(&self) -> PassId {
        PassId::Normalize
    }
    fn must_follow(&self) -> &'static [PassId] {
        &[PassId::CoalesceBgp]
    }
    fn run(&self, plan: LogicalPlan, _ctx: &PlanCtx) -> LogicalPlan {
        normalize(plan)
    }
}

fn normalize(node: LogicalPlan) -> LogicalPlan {
    // Bottom-up: normalize children first, then this node.
    let node = map_children(node, &normalize);
    match node {
        LogicalPlan::Filter { expr, inner } => normalize_filter(expr, *inner),
        other => other,
    }
}

/// Simplify a filter's predicate: reduce `Eq -> SameTerm` where legal, fold
/// constant conjuncts. A wholly-true predicate drops the `Filter`; a
/// definitely-false one collapses the subtree to an empty relation carrying
/// `inner`'s schema.
fn normalize_filter(expr: Expr, inner: LogicalPlan) -> LogicalPlan {
    let types = types::infer(&inner);
    let mut kept: Vec<Expr> = Vec::new();
    // Split top-level conjuncts, fold/reduce each.
    let mut parts = Vec::new();
    crate::plan::passes::conjuncts(expr, &mut parts);
    for part in parts {
        let part = reduce_expr(part, &types);
        match const_bool(&part) {
            Some(true) => {} // drop — always satisfied
            Some(false) => {
                // Whole filter can never match: empty relation, right schema.
                return LogicalPlan::Values {
                    vars: schema(&inner),
                    rows: Vec::new(),
                };
            }
            None => kept.push(part),
        }
    }
    match crate::plan::passes::conjoin(kept) {
        None => inner, // all conjuncts were constant-true
        Some(e) => LogicalPlan::Filter {
            expr: e,
            inner: Box::new(inner),
        },
    }
}

/// Recursively reduce `Eq(a,b) -> SameTerm(a,b)` where the lattice proves
/// both operands are the same non-literal kind (IRI or blank), where
/// term-equality and value-equality coincide. Everything else is returned
/// structurally unchanged (children reduced).
fn reduce_expr(e: Expr, types: &VarTypes) -> Expr {
    match e {
        Expr::And(a, b) => Expr::And(
            Box::new(reduce_expr(*a, types)),
            Box::new(reduce_expr(*b, types)),
        ),
        Expr::Or(a, b) => Expr::Or(
            Box::new(reduce_expr(*a, types)),
            Box::new(reduce_expr(*b, types)),
        ),
        Expr::Not(a) => Expr::Not(Box::new(reduce_expr(*a, types))),
        Expr::Eq(a, b) if same_nonliteral_kind(&a, &b, types) => Expr::SameTerm(a, b),
        other => other,
    }
}

/// True iff both operands are provably the same non-literal term kind:
/// both IRIs (a `NamedNode` constant or a var the lattice pins to
/// `NamedNode`) or both blank nodes. Literals are excluded — value- and
/// term-equality diverge there — which also leaves the physical
/// count-pushdown `?v = <const-literal>` inlining untouched.
fn same_nonliteral_kind(a: &Expr, b: &Expr, types: &VarTypes) -> bool {
    (is_iri(a, types) && is_iri(b, types)) || (is_blank(a, types) && is_blank(b, types))
}

fn is_iri(e: &Expr, types: &VarTypes) -> bool {
    match e {
        Expr::Term(Term::Iri(_)) => true,
        Expr::Term(Term::Var(v)) => types.0.get(v).map(|m| m.is_named_node()).unwrap_or(false),
        _ => false,
    }
}
fn is_blank(e: &Expr, types: &VarTypes) -> bool {
    match e {
        Expr::Term(Term::BlankNode(_)) => true,
        Expr::Term(Term::Var(v)) => types.0.get(v).map(|m| m.is_blank_node()).unwrap_or(false),
        _ => false,
    }
}

/// Constant boolean value of a *variable-free* predicate, if structurally
/// decidable: ground `Eq`/`SameTerm`/`Ne` over two non-variable `Term`s,
/// and `And`/`Or`/`Not` over such. `None` for anything referencing a
/// variable or otherwise undecidable at plan time (kept as-is, evaluated at
/// runtime — never guessed).
fn const_bool(e: &Expr) -> Option<bool> {
    match e {
        Expr::Eq(a, b) | Expr::SameTerm(a, b) => Some(ground_term(a)? == ground_term(b)?),
        Expr::Ne(a, b) => Some(ground_term(a)? != ground_term(b)?),
        Expr::Not(a) => Some(!const_bool(a)?),
        Expr::And(a, b) => match (const_bool(a), const_bool(b)) {
            (Some(false), _) | (_, Some(false)) => Some(false),
            (Some(true), Some(true)) => Some(true),
            _ => None,
        },
        Expr::Or(a, b) => match (const_bool(a), const_bool(b)) {
            (Some(true), _) | (_, Some(true)) => Some(true),
            (Some(false), Some(false)) => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// The ground `Term` of an operand that is a non-variable constant, else
/// `None` (a variable, or a compound expression, is not plan-time ground).
fn ground_term(e: &Expr) -> Option<&Term> {
    match e {
        Expr::Term(t @ (Term::Iri(_) | Term::BlankNode(_) | Term::Literal(_))) => Some(t),
        _ => None,
    }
}
```

Note: `const_bool` compares ground `Term`s by `PartialEq` (structural term identity), which matches the engine's `Expr::Eq` term-equality — so folding is exact for the shapes it decides and conservative (`None`) everywhere else.

- [ ] **Step 5: Register `Normalize` in the pipeline**

In `crates/sparql/src/plan/pass.rs`, append to the vec built by `default_passes()` (after the Phase-1 `CoalesceBgp` entry):

```rust
    passes.push(Box::new(crate::plan::passes::Normalize));
```

- [ ] **Step 6: Run the tests**

Run: `cargo nextest run -p horndb-sparql plan::passes::normalize`
Expected: PASS (5 tests).

Run: `cargo nextest run -p horndb-sparql`
Expected: PASS — `Normalize` only fires on provably-safe shapes; existing count-pushdown/pruning invariance batteries in `plan/pushdown.rs` stay green (the reduction never touches `?v = <const-literal>`).

- [ ] **Step 7: Commit**

```bash
cargo fmt --all
git add crates/sparql/src/plan/passes/mod.rs crates/sparql/src/plan/passes/normalize.rs crates/sparql/src/plan/mod.rs crates/sparql/src/plan/pass.rs
git commit -m 'feat(sparql): Normalize pass — constant folding + Eq->SameTerm (#185)'
```

---

### Task 3: `FilterPullup` pass

`PassId::FilterPullup`, `must_follow: [Normalize]`. Hoist filters upward so the next pass (`FilterPushdown`) sees the complete conjunct set at each join (DuckDB pull-up-then-push-down). Kept deliberately simple per the spec: **coalesce adjacent `Filter`s and pull a `Filter` that sits on one side of an inner `Join` up above the join, merging conjuncts into a single `Filter`.** Never hoist across a `LeftJoin`/`Union`/`Distinct`/`Group`/`Slice` boundary (that changes semantics).

**Files:**
- Create: `crates/sparql/src/plan/passes/filter_pullup.rs`
- Modify: `crates/sparql/src/plan/pass.rs` (`default_passes()` append)
- Test: unit tests inside `filter_pullup.rs`

- [ ] **Step 1: Write the failing tests**

Create `crates/sparql/src/plan/passes/filter_pullup.rs` with tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::algebra::{Expr, Term, TriplePattern, Var};
    use crate::plan::logical::LogicalPlan;
    use crate::plan::pass::{LogicalPass, PassId, PlanCtx};

    fn var(n: &str) -> Term { Term::Var(Var::new(n)) }
    fn ctx() -> PlanCtx { PlanCtx { disabled_passes: Default::default() } }
    fn bgp(p: &str) -> LogicalPlan {
        LogicalPlan::Bgp { patterns: vec![TriplePattern {
            subject: var("s"), predicate: Term::Iri(format!("http://ex/{p}")), object: var(p),
        }] }
    }
    fn pred(v: &str) -> Expr {
        Expr::Gt(Box::new(Expr::Term(var(v))), Box::new(Expr::Term(Term::Literal("\"0\"".into()))))
    }

    /// A Filter on the left arm of a Join is pulled above the Join.
    #[test]
    fn pulls_filter_above_join() {
        let left = LogicalPlan::Filter { expr: pred("a"), inner: Box::new(bgp("a")) };
        let plan = LogicalPlan::Join { left: Box::new(left), right: Box::new(bgp("b")) };
        let out = FilterPullup.run(plan, &ctx());
        assert!(matches!(out, LogicalPlan::Filter { inner, .. } if matches!(*inner, LogicalPlan::Join { .. })),
            "filter must sit above the join; got {out:?}");
    }

    /// Filters from both arms merge into one Filter (single conjunction).
    #[test]
    fn merges_both_arms_into_one_filter() {
        let left = LogicalPlan::Filter { expr: pred("a"), inner: Box::new(bgp("a")) };
        let right = LogicalPlan::Filter { expr: pred("b"), inner: Box::new(bgp("b")) };
        let plan = LogicalPlan::Join { left: Box::new(left), right: Box::new(right) };
        let out = FilterPullup.run(plan, &ctx());
        let LogicalPlan::Filter { expr, inner } = out else { panic!("expected one Filter, got {out:?}") };
        assert!(matches!(*inner, LogicalPlan::Join { .. }));
        // Two conjuncts hoisted.
        let mut parts = Vec::new();
        crate::plan::passes::conjuncts(expr, &mut parts);
        assert_eq!(parts.len(), 2, "both arm filters must be conjoined");
    }

    /// A Filter on the optional (right) arm of a LeftJoin must NOT be pulled
    /// up (semantics differ).
    #[test]
    fn never_pulls_across_leftjoin() {
        let right = LogicalPlan::Filter { expr: pred("b"), inner: Box::new(bgp("b")) };
        let plan = LogicalPlan::LeftJoin { left: Box::new(bgp("a")), right: Box::new(right), expr: None };
        let out = FilterPullup.run(plan, &ctx());
        assert!(matches!(out, LogicalPlan::LeftJoin { .. }), "no hoist across LeftJoin; got {out:?}");
    }

    #[test]
    fn id_and_ordering() {
        assert_eq!(FilterPullup.id(), PassId::FilterPullup);
        assert_eq!(FilterPullup.must_follow(), &[PassId::Normalize]);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo nextest run -p horndb-sparql plan::passes::filter_pullup`
Expected: build FAILURE — `cannot find type FilterPullup`.

- [ ] **Step 3: Implement `FilterPullup`**

Prepend to `crates/sparql/src/plan/passes/filter_pullup.rs`:

```rust
//! `FilterPullup` (SPEC-23 §5.2): hoist filters above inner joins so
//! `FilterPushdown` sees the complete conjunct set at each join. Conjuncts
//! are pulled through `Join` (both arms) only; `LeftJoin`, `Union`,
//! `Distinct`, `Group`, and `Slice` are hard boundaries.

use crate::algebra::Expr;
use crate::plan::logical::LogicalPlan;
use crate::plan::pass::{LogicalPass, PassId, PlanCtx};
use crate::plan::passes::{conjoin, map_children};

/// The `FilterPullup` logical pass. See module docs.
pub struct FilterPullup;

impl LogicalPass for FilterPullup {
    fn id(&self) -> PassId {
        PassId::FilterPullup
    }
    fn must_follow(&self) -> &'static [PassId] {
        &[PassId::Normalize]
    }
    fn run(&self, plan: LogicalPlan, _ctx: &PlanCtx) -> LogicalPlan {
        pullup(plan)
    }
}

/// Bottom-up: pull filters out of a node's children, then, at a `Join`,
/// strip any immediate `Filter` wrappers off each arm and re-emit their
/// conjuncts as one `Filter` above the rebuilt join.
fn pullup(node: LogicalPlan) -> LogicalPlan {
    let node = map_children(node, &pullup);
    match node {
        LogicalPlan::Join { left, right } => {
            let (left, mut preds) = strip_filter(*left);
            let (right, mut rpreds) = {
                let (r, p) = strip_filter(*right);
                (r, p)
            };
            preds.append(&mut rpreds);
            let join = LogicalPlan::Join {
                left: Box::new(left),
                right: Box::new(right),
            };
            match conjoin(preds) {
                Some(expr) => LogicalPlan::Filter {
                    expr,
                    inner: Box::new(join),
                },
                None => join,
            }
        }
        other => other,
    }
}

/// Peel a chain of immediate `Filter` wrappers off `node`, returning the
/// unwrapped node and the collected conjuncts (in top-down order). A node
/// that is not a `Filter` yields itself and no predicates.
fn strip_filter(node: LogicalPlan) -> (LogicalPlan, Vec<Expr>) {
    let mut preds = Vec::new();
    let mut cur = node;
    while let LogicalPlan::Filter { expr, inner } = cur {
        crate::plan::passes::conjuncts(expr, &mut preds);
        cur = *inner;
    }
    (cur, preds)
}
```

- [ ] **Step 4: Register in the pipeline**

In `crates/sparql/src/plan/pass.rs`, append after `Normalize`:

```rust
    passes.push(Box::new(crate::plan::passes::FilterPullup));
```

- [ ] **Step 5: Run the tests**

Run: `cargo nextest run -p horndb-sparql plan::passes::filter_pullup`
Expected: PASS (4 tests).

Run: `cargo nextest run -p horndb-sparql`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
git add crates/sparql/src/plan/passes/filter_pullup.rs crates/sparql/src/plan/pass.rs
git commit -m 'feat(sparql): FilterPullup pass — hoist conjuncts above inner joins (#185)'
```

---

### Task 4: `FilterPushdown` pass

`PassId::FilterPushdown`, `must_follow: [FilterPullup]`. Split the hoisted `Filter` into conjuncts; push each conjunct to the **deepest** subtree whose output binds *all* of the conjunct's variables (legality gated on `types::infer` via `bound_vars`). **Respect the `LeftJoin` asymmetry** (spec §5.2): a conjunct may descend into the mandatory (left) child, but **never** into the optional (right) child of a `LeftJoin` — filtering optional rows before the join changes which left rows get a match. This is the *logical* filter pushdown; the physical `plan/pushdown.rs` (count-pushdown + column pruning) is unaffected and still runs at lowering time.

**Files:**
- Create: `crates/sparql/src/plan/passes/filter_pushdown.rs`
- Modify: `crates/sparql/src/plan/pass.rs` (`default_passes()` append)
- Test: unit tests inside `filter_pushdown.rs`

- [ ] **Step 1: Write the failing tests**

Create `crates/sparql/src/plan/passes/filter_pushdown.rs` with tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::algebra::{Expr, Term, TriplePattern, Var};
    use crate::plan::logical::LogicalPlan;
    use crate::plan::pass::{LogicalPass, PassId, PlanCtx};

    fn var(n: &str) -> Term { Term::Var(Var::new(n)) }
    fn ctx() -> PlanCtx { PlanCtx { disabled_passes: Default::default() } }
    fn scan(subj: &str, p: &str, obj: &str) -> LogicalPlan {
        LogicalPlan::Bgp { patterns: vec![TriplePattern {
            subject: var(subj), predicate: Term::Iri(format!("http://ex/{p}")), object: var(obj),
        }] }
    }
    fn gt0(v: &str) -> Expr {
        Expr::Gt(Box::new(Expr::Term(var(v))), Box::new(Expr::Term(Term::Literal("\"0\"".into()))))
    }

    /// A conjunct that mentions only left-arm vars pushes onto the left arm.
    #[test]
    fn pushes_single_var_conjunct_to_binding_arm() {
        // Join( scan(?a p1 ?x), scan(?a p2 ?y) ) with FILTER(?x > 0) on top.
        let join = LogicalPlan::Join {
            left: Box::new(scan("a", "p1", "x")),
            right: Box::new(scan("a", "p2", "y")),
        };
        let plan = LogicalPlan::Filter { expr: gt0("x"), inner: Box::new(join) };
        let out = FilterPushdown.run(plan, &ctx());
        // The FILTER(?x>0) must now wrap the left scan, not the whole join.
        let LogicalPlan::Join { left, right } = out else { panic!("expected Join at root, got {out:?}") };
        assert!(matches!(*left, LogicalPlan::Filter { .. }), "conjunct must push to the left arm; got {left:?}");
        assert!(matches!(*right, LogicalPlan::Bgp { .. }), "right arm unfiltered; got {right:?}");
    }

    /// A conjunct referencing a var bound only on the OPTIONAL side of a
    /// LeftJoin stays ABOVE the LeftJoin — never pushed into the right arm.
    #[test]
    fn respects_leftjoin_asymmetry() {
        let lj = LogicalPlan::LeftJoin {
            left: Box::new(scan("a", "p1", "x")),
            right: Box::new(scan("a", "p2", "y")),
            expr: None,
        };
        let plan = LogicalPlan::Filter { expr: gt0("y"), inner: Box::new(lj) };
        let out = FilterPushdown.run(plan, &ctx());
        // Must remain Filter(LeftJoin(...)) — the conjunct on ?y (optional)
        // cannot descend.
        let LogicalPlan::Filter { inner, .. } = out else { panic!("filter must stay above LeftJoin, got {out:?}") };
        assert!(matches!(*inner, LogicalPlan::LeftJoin { .. }));
    }

    /// A conjunct on a var bound on the MANDATORY side of a LeftJoin DOES
    /// push into the left arm.
    #[test]
    fn pushes_into_leftjoin_mandatory_arm() {
        let lj = LogicalPlan::LeftJoin {
            left: Box::new(scan("a", "p1", "x")),
            right: Box::new(scan("a", "p2", "y")),
            expr: None,
        };
        let plan = LogicalPlan::Filter { expr: gt0("x"), inner: Box::new(lj) };
        let out = FilterPushdown.run(plan, &ctx());
        let LogicalPlan::LeftJoin { left, .. } = out else { panic!("expected LeftJoin root, got {out:?}") };
        assert!(matches!(*left, LogicalPlan::Filter { .. }), "mandatory-arm conjunct must push; got {left:?}");
    }

    #[test]
    fn id_and_ordering() {
        assert_eq!(FilterPushdown.id(), PassId::FilterPushdown);
        assert_eq!(FilterPushdown.must_follow(), &[PassId::FilterPullup]);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo nextest run -p horndb-sparql plan::passes::filter_pushdown`
Expected: build FAILURE — `cannot find type FilterPushdown`.

- [ ] **Step 3: Implement `FilterPushdown`**

Prepend to `crates/sparql/src/plan/passes/filter_pushdown.rs`:

```rust
//! `FilterPushdown` (SPEC-23 §5.2): push each conjunct to the deepest
//! subtree that binds all its variables. Legality is lattice-gated
//! (`bound_vars`); the `LeftJoin` asymmetry is honored — a conjunct never
//! descends into the optional (right) arm.

use crate::algebra::Expr;
use crate::exec::runtime::referenced_vars;
use crate::plan::logical::LogicalPlan;
use crate::plan::pass::{LogicalPass, PassId, PlanCtx};
use crate::plan::passes::{bound_vars, conjoin, conjuncts, map_children};
use std::collections::HashSet;

/// The `FilterPushdown` logical pass. See module docs.
pub struct FilterPushdown;

impl LogicalPass for FilterPushdown {
    fn id(&self) -> PassId {
        PassId::FilterPushdown
    }
    fn must_follow(&self) -> &'static [PassId] {
        &[PassId::FilterPullup]
    }
    fn run(&self, plan: LogicalPlan, _ctx: &PlanCtx) -> LogicalPlan {
        pushdown(plan)
    }
}

/// Bottom-up rewrite: normalize children, then at each `Filter` try to sink
/// every conjunct as deep as it can legally go.
fn pushdown(node: LogicalPlan) -> LogicalPlan {
    let node = map_children(node, &pushdown);
    match node {
        LogicalPlan::Filter { expr, inner } => {
            let mut parts = Vec::new();
            conjuncts(expr, &mut parts);
            // Sink each conjunct into `inner`; conjuncts that cannot descend
            // are re-wrapped as a residual Filter at this level.
            let mut residual = Vec::new();
            let mut cur = *inner;
            for c in parts {
                match push_one(c, cur) {
                    Ok(sunk) => cur = sunk,
                    Err((c, unchanged)) => {
                        cur = unchanged;
                        residual.push(c);
                    }
                }
            }
            match conjoin(residual) {
                Some(e) => LogicalPlan::Filter {
                    expr: e,
                    inner: Box::new(cur),
                },
                None => cur,
            }
        }
        other => other,
    }
}

/// Try to push conjunct `c` into `node`. `Ok(node')` when it descended at
/// least one level; `Err((c, node))` when it cannot legally sink further
/// (caller keeps it as a residual at the current level).
fn push_one(c: Expr, node: LogicalPlan) -> Result<LogicalPlan, (Expr, LogicalPlan)> {
    let mut vars = HashSet::new();
    referenced_vars(&c, &mut vars);
    match node {
        // Push into whichever inner-join arm binds all of `c`'s vars; if
        // both do, prefer the left (deterministic). If neither, wrap the arm
        // set is a Cartesian shape — keep at this level.
        LogicalPlan::Join { left, right } => {
            if vars.is_subset(&bound_vars(&left)) {
                let left = wrap(c, *left);
                Ok(LogicalPlan::Join { left: Box::new(left), right })
            } else if vars.is_subset(&bound_vars(&right)) {
                let right = wrap(c, *right);
                Ok(LogicalPlan::Join { left, right: Box::new(right) })
            } else {
                Err((c, LogicalPlan::Join { left, right }))
            }
        }
        // ASYMMETRY: only the mandatory (left) arm is a legal target. A
        // conjunct touching optional (right-only) vars must stay above.
        LogicalPlan::LeftJoin { left, right, expr } => {
            if vars.is_subset(&bound_vars(&left)) {
                let left = wrap(c, *left);
                Ok(LogicalPlan::LeftJoin { left: Box::new(left), right, expr })
            } else {
                Err((c, LogicalPlan::LeftJoin { left, right, expr }))
            }
        }
        // Transparent to variable scope: push straight through.
        LogicalPlan::Project { vars: pv, inner } if vars.is_subset(&bound_vars(&inner)) => {
            let inner = wrap(c, *inner);
            Ok(LogicalPlan::Project { vars: pv, inner: Box::new(inner) })
        }
        // A `Bgp` is a leaf — wrapping it here IS the push (one level down
        // from the caller's Filter). Report success so the conjunct is
        // consumed exactly once.
        leaf @ LogicalPlan::Bgp { .. } => Ok(wrap(c, leaf)),
        // Everything else (Union, Distinct, Group, Slice, OrderBy, Extend,
        // Values, PathClosure): not a safe sink target — keep above.
        other => Err((c, other)),
    }
}

/// Recursively try to sink `c` further; wrap it here if it cannot go deeper.
fn wrap(c: Expr, node: LogicalPlan) -> LogicalPlan {
    match push_one(c, node) {
        Ok(sunk) => sunk,
        Err((c, node)) => LogicalPlan::Filter {
            expr: c,
            inner: Box::new(node),
        },
    }
}
```

Note the deliberate design: `push_one` returns `Ok` only when the conjunct moved *at least one level*; `wrap` recurses so a conjunct sinks to the deepest legal point in one pass. The `Bgp` leaf arm returns `Ok(wrap-here)` because from the caller's `Filter` that is already one level deeper — a conjunct always ends up on the deepest single-arm scan that binds it.

- [ ] **Step 4: Register in the pipeline**

In `crates/sparql/src/plan/pass.rs`, append after `FilterPullup`:

```rust
    passes.push(Box::new(crate::plan::passes::FilterPushdown));
```

- [ ] **Step 5: Run the tests**

Run: `cargo nextest run -p horndb-sparql plan::passes::filter_pushdown`
Expected: PASS (4 tests) — including `respects_leftjoin_asymmetry`.

Run: `cargo nextest run -p horndb-sparql`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
git add crates/sparql/src/plan/passes/filter_pushdown.rs crates/sparql/src/plan/pass.rs
git commit -m 'feat(sparql): FilterPushdown pass — sink conjuncts, honor LeftJoin asymmetry (#185)'
```

---

### Task 5: `ProjectionPushdown` pass

`PassId::ProjectionPushdown`, `must_follow: [FilterPushdown]`. Thread a demanded-variable set top-down and insert restricting `Project` nodes so each subtree binds only the variables an ancestor actually reads — high value in a columnar dictionary store (fewer `TermId → Term` decodes, narrower join rows).

**Relationship to `plan/pushdown.rs::prune` (the physical projection pruning at ~line 470):** the existing physical `prune` stays. It is coupled to physical count-pushdown (`push_aggregates`) and operates on the already-lowered `PhysicalPlan`. This new pass performs the *logical* projection pushdown so subsequent logical passes (ultimately `JoinPlanning`, Phase 4) see narrowed schemas, and so lowering starts from a narrower plan. Running both is safe and result-invariant — `prune` treats a `Project` as its restriction point and is idempotent over an already-narrowed tree. **Decision: keep both for Phase 2**; migrating/retiring the physical `prune` is deferred to Phase 4, when `JoinPlanning` consumes the logical narrowing directly (recorded in `INTEGRATION-NOTES.md`, Task 6). This mirrors the physical `prune`'s proven demanded-set discipline (DISTINCT/Group barriers, evaluate-wide/project-narrow) at the logical level.

**Files:**
- Create: `crates/sparql/src/plan/passes/projection_pushdown.rs`
- Modify: `crates/sparql/src/plan/pass.rs` (`default_passes()` append)
- Test: unit tests inside `projection_pushdown.rs`

- [ ] **Step 1: Write the failing tests**

Create `crates/sparql/src/plan/passes/projection_pushdown.rs` with tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::algebra::{Term, TriplePattern, Var};
    use crate::plan::logical::LogicalPlan;
    use crate::plan::pass::{LogicalPass, PassId, PlanCtx};
    use crate::plan::passes::schema;

    fn var(n: &str) -> Term { Term::Var(Var::new(n)) }
    fn ctx() -> PlanCtx { PlanCtx { disabled_passes: Default::default() } }

    /// `Project([?s], Bgp(?s ?p ?o))` narrows the scan so only ?s survives
    /// below the Project (a restricting Project wraps the scan).
    #[test]
    fn narrows_scan_under_project() {
        let bgp = LogicalPlan::Bgp { patterns: vec![TriplePattern {
            subject: var("s"), predicate: var("p"), object: var("o"),
        }] };
        let plan = LogicalPlan::Project { vars: vec![Var::new("s")], inner: Box::new(bgp) };
        let out = ProjectionPushdown.run(plan, &ctx());
        // The scan feeding the Project must now expose only ?s.
        let LogicalPlan::Project { inner, .. } = out else { panic!("expected Project root") };
        let sch = schema(&inner);
        assert_eq!(sch, vec![Var::new("s")], "scan must be narrowed to ?s; got {sch:?}");
    }

    /// DISTINCT is a barrier: its child keeps its full natural schema (else
    /// the dedup key set changes).
    #[test]
    fn distinct_is_a_barrier() {
        let bgp = LogicalPlan::Bgp { patterns: vec![TriplePattern {
            subject: var("s"), predicate: var("p"), object: var("o"),
        }] };
        let plan = LogicalPlan::Project {
            vars: vec![Var::new("s")],
            inner: Box::new(LogicalPlan::Distinct { inner: Box::new(bgp) }),
        };
        let out = ProjectionPushdown.run(plan, &ctx());
        // Descend to the Distinct's child; it must still carry ?s ?p ?o.
        fn distinct_child(p: &LogicalPlan) -> Option<&LogicalPlan> {
            match p {
                LogicalPlan::Distinct { inner } => Some(inner),
                LogicalPlan::Project { inner, .. } | LogicalPlan::Filter { inner, .. } => distinct_child(inner),
                _ => None,
            }
        }
        let child = distinct_child(&out).expect("Distinct preserved");
        let mut sch = schema(child);
        sch.sort_by(|a, b| a.name().cmp(b.name()));
        assert_eq!(sch, vec![Var::new("o"), Var::new("p"), Var::new("s")],
            "Distinct child must keep full dedup key; got {sch:?}");
    }

    #[test]
    fn id_and_ordering() {
        assert_eq!(ProjectionPushdown.id(), PassId::ProjectionPushdown);
        assert_eq!(ProjectionPushdown.must_follow(), &[PassId::FilterPushdown]);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo nextest run -p horndb-sparql plan::passes::projection_pushdown`
Expected: build FAILURE — `cannot find type ProjectionPushdown`.

- [ ] **Step 3: Implement `ProjectionPushdown`**

Prepend to `crates/sparql/src/plan/passes/projection_pushdown.rs`. The demanded-set logic mirrors `plan/pushdown.rs::prune` — DISTINCT and non-count Group are barriers; Filter/OrderBy/Extend evaluate wide; Join/LeftJoin add the shared keys; a leaf wider than demanded is wrapped in a restricting `Project`:

```rust
//! `ProjectionPushdown` (SPEC-23 §5.2): thread a demanded-variable set
//! top-down, inserting restricting `Project`s so each subtree binds only
//! what an ancestor reads. Logical mirror of `plan::pushdown::prune`; both
//! run in Phase 2 (see PLAN-23-02 Task 5 rationale).

use crate::algebra::{Aggregate, Expr, Var};
use crate::exec::runtime::{agg_inner_exprs, referenced_vars};
use crate::plan::logical::LogicalPlan;
use crate::plan::pass::{LogicalPass, PassId, PlanCtx};
use crate::plan::passes::schema;
use std::collections::HashSet;

/// The `ProjectionPushdown` logical pass. See module docs.
pub struct ProjectionPushdown;

impl LogicalPass for ProjectionPushdown {
    fn id(&self) -> PassId {
        PassId::ProjectionPushdown
    }
    fn must_follow(&self) -> &'static [PassId] {
        &[PassId::FilterPushdown]
    }
    fn run(&self, plan: LogicalPlan, _ctx: &PlanCtx) -> LogicalPlan {
        // The root demands its own full natural output — never narrowed
        // below what it already emits.
        let demanded: HashSet<String> = schema(&plan).into_iter().map(|v| v.name().to_owned()).collect();
        prune(plan, &demanded)
    }
}

/// Wrap `node` in a restricting `Project` if it produces more than
/// `demanded`. Empty intersection is left unwrapped (an empty `Project`
/// vars list is `SELECT *`); the surplus rides upward harmlessly.
fn restrict(node: LogicalPlan, demanded: &HashSet<String>) -> LogicalPlan {
    let nat = schema(&node);
    let kept: Vec<Var> = nat.iter().filter(|v| demanded.contains(v.name())).cloned().collect();
    if kept.len() < nat.len() && !kept.is_empty() {
        LogicalPlan::Project { vars: kept, inner: Box::new(node) }
    } else {
        node
    }
}

fn set_of(vars: &[Var]) -> HashSet<String> {
    vars.iter().map(|v| v.name().to_owned()).collect()
}

fn prune(node: LogicalPlan, demanded: &HashSet<String>) -> LogicalPlan {
    use LogicalPlan::*;
    match node {
        leaf @ (Bgp { .. } | Values { .. }) => restrict(leaf, demanded),

        // Project IS the restriction point: it forwards exactly its own vars.
        Project { vars, inner } => {
            let want = set_of(&vars);
            Project { vars, inner: Box::new(prune(*inner, &want)) }
        }

        // Filter evaluates its expr's vars; wrap surplus above.
        Filter { expr, inner } => {
            let mut d = demanded.clone();
            referenced_vars(&expr, &mut d);
            let pi = prune(*inner, &d);
            restrict(Filter { expr, inner: Box::new(pi) }, demanded)
        }

        // Inner join: both arms keep the shared join keys plus their share of
        // `demanded`; project the keys back off above.
        Join { left, right } => {
            let (lo, ro) = (schema(&left), schema(&right));
            let mut base = demanded.clone();
            add_shared_keys(&lo, &ro, &mut base);
            let pl = prune(*left, &intersect(&base, &lo));
            let pr = prune(*right, &intersect(&base, &ro));
            restrict(Join { left: Box::new(pl), right: Box::new(pr) }, demanded)
        }
        LeftJoin { left, right, expr } => {
            let (lo, ro) = (schema(&left), schema(&right));
            let mut base = demanded.clone();
            add_shared_keys(&lo, &ro, &mut base);
            if let Some(e) = &expr {
                referenced_vars(e, &mut base);
            }
            let pl = prune(*left, &intersect(&base, &lo));
            let pr = prune(*right, &intersect(&base, &ro));
            restrict(LeftJoin { left: Box::new(pl), right: Box::new(pr), expr }, demanded)
        }

        // Union: conservative — both branches share the merged schema; recurse
        // with `demanded`, never wrap.
        Union { left, right } => Union {
            left: Box::new(prune(*left, demanded)),
            right: Box::new(prune(*right, demanded)),
        },

        // DISTINCT barrier: pruning before it changes the dedup key set.
        Distinct { inner } => {
            let nat = set_of(&schema(&inner));
            Distinct { inner: Box::new(prune(*inner, &nat)) }
        }

        Slice { inner, start, length } => Slice { inner: Box::new(prune(*inner, demanded)), start, length },

        OrderBy { inner, keys } => {
            let mut d = demanded.clone();
            for (e, _) in &keys {
                referenced_vars(e, &mut d);
            }
            let pi = prune(*inner, &d);
            restrict(OrderBy { inner: Box::new(pi), keys }, demanded)
        }

        Extend { inner, var, expr } => {
            let mut d = demanded.clone();
            d.remove(var.name());
            referenced_vars(&expr, &mut d);
            let pi = prune(*inner, &d);
            restrict(Extend { inner: Box::new(pi), var, expr }, demanded)
        }

        // Group: natural restriction point unless a DISTINCT-star aggregate
        // reads whole rows (then it is a full barrier, like Distinct).
        Group { inner, keys, aggregates } => {
            let d = group_demand(&keys, &aggregates, &inner);
            Group { inner: Box::new(prune(*inner, &d)), keys, aggregates }
        }

        // PathClosure: keep the edge's full natural output (BFS endpoints).
        PathClosure { subject, object, edge, reflexive } => {
            let nat = set_of(&schema(&edge));
            PathClosure { subject, object, edge: Box::new(prune(*edge, &nat)), reflexive }
        }
    }
}

fn add_shared_keys(lo: &[Var], ro: &[Var], base: &mut HashSet<String>) {
    for v in lo {
        if ro.iter().any(|r| r == v) {
            base.insert(v.name().to_owned());
        }
    }
}

fn intersect(superset: &HashSet<String>, scope: &[Var]) -> HashSet<String> {
    scope.iter().filter(|v| superset.contains(v.name())).map(|v| v.name().to_owned()).collect()
}

/// A Group's demand on its child: keys ∪ aggregate-input vars; but a
/// DISTINCT `COUNT(*)` dedups whole rows, so it demands the child's entire
/// natural schema.
fn group_demand(keys: &[Var], aggregates: &[Aggregate], inner: &LogicalPlan) -> HashSet<String> {
    use crate::algebra::AggFunc;
    let distinct_star = aggregates
        .iter()
        .any(|a| matches!(a.func, AggFunc::CountStar) && a.distinct);
    if distinct_star {
        return set_of(&schema(inner));
    }
    let mut d: HashSet<String> = keys.iter().map(|k| k.name().to_owned()).collect();
    for a in aggregates {
        for e in agg_inner_exprs(a) {
            let e: &Expr = e;
            referenced_vars(e, &mut d);
        }
    }
    d
}
```

(`agg_inner_exprs` and `referenced_vars` are the same `pub(crate)` helpers `plan/pushdown.rs` uses, imported from `crate::exec::runtime`. If Phase 1 relocated `Aggregate`/`AggFunc`, import from wherever the pinned `logical.rs` re-exports them.)

- [ ] **Step 4: Register in the pipeline**

In `crates/sparql/src/plan/pass.rs`, append after `FilterPushdown`:

```rust
    passes.push(Box::new(crate::plan::passes::ProjectionPushdown));
```

- [ ] **Step 5: Run the tests**

Run: `cargo nextest run -p horndb-sparql plan::passes::projection_pushdown`
Expected: PASS (3 tests) — including `distinct_is_a_barrier`.

Run: `cargo nextest run -p horndb-sparql`
Expected: PASS — the physical `prune` battery in `plan/pushdown.rs` still passes (logical narrowing then physical narrowing is idempotent and result-invariant).

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
git add crates/sparql/src/plan/passes/projection_pushdown.rs crates/sparql/src/plan/pass.rs
git commit -m 'feat(sparql): ProjectionPushdown pass — thread demanded vars, insert Projects (#185)'
```

---

### Task 6: Slot-differential invariance battery + per-`PassId` bisection + gates

The spec's Phase-2 guard (§6.2, §7.1, §7.2): every pass is individually disable-able, and a regression bisects to one `PassId`. This task adds the end-to-end result-invariance battery — run each query through the **full pipeline** and again with **each pass singly disabled** via `PlanCtx.disabled_passes`, asserting identical result multisets over `HornBackend`/`MemStore` fixtures (the `canon()` pattern from `plan/pushdown.rs`). Then run the conformance subset and WCOJ differential fuzzer.

**Files:**
- Create: `crates/sparql/tests/rewrite_invariance.rs`
- Modify: `crates/sparql/INTEGRATION-NOTES.md`

- [ ] **Step 1: Write the battery**

Create `crates/sparql/tests/rewrite_invariance.rs`:

```rust
//! SPEC-23 Phase 2 slot-differential suite: the four heuristic rewrite
//! passes must not change any result. For every query we compare the FULL
//! pipeline against the pipeline with EACH pass singly disabled — identical
//! result multisets prove (a) result-invariance and (b) that a future
//! regression bisects to exactly one `PassId`.

use horndb_sparql::algebra::translate::translate_query_with;
use horndb_sparql::algebra::Term;
use horndb_sparql::exec::horn::HornBackend;
use horndb_sparql::exec::runtime::Runtime;
use horndb_sparql::exec::{Bindings, Store};
use horndb_sparql::parser::{parse_query, ParsedQuery};
use horndb_sparql::plan::logical::from_algebra;
use horndb_sparql::plan::lower::lower;
use horndb_sparql::plan::pass::{default_passes, run_passes, PassId, PlanCtx};
use horndb_sparql::SparqlConfig;
use std::collections::HashSet;

/// The four Phase-2 passes, each toggled independently.
const PHASE2_PASSES: [PassId; 4] = [
    PassId::Normalize,
    PassId::FilterPullup,
    PassId::FilterPushdown,
    PassId::ProjectionPushdown,
];

fn fixture() -> HornBackend {
    let mut horn = HornBackend::new();
    let iri = |s: &str| Term::Iri(format!("http://ex/{s}"));
    let lit = |s: &str| Term::Literal(format!("\"{s}\""));
    horn.insert_triple(iri("a"), iri("name"), lit("Alice"));
    horn.insert_triple(iri("a"), iri("age"), Term::Literal("\"30\"".into()));
    horn.insert_triple(iri("a"), iri("knows"), iri("b"));
    horn.insert_triple(iri("b"), iri("name"), lit("Bob"));
    horn.insert_triple(iri("b"), iri("age"), Term::Literal("\"25\"".into()));
    horn.insert_triple(iri("b"), iri("knows"), iri("c"));
    horn.insert_triple(iri("c"), iri("name"), lit("Carol"));
    horn.insert_triple(iri("c"), iri("knows"), iri("a"));
    horn.insert_triple(iri("d"), iri("name"), lit("Alice"));
    horn
}

/// Order-independent multiset rendering (same shape as pushdown.rs::canon).
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

/// Run a SELECT through the logical pipeline with `disabled` passes skipped,
/// lower, execute, collect.
fn run_with(horn: &HornBackend, q: &str, disabled: HashSet<PassId>) -> Vec<Bindings> {
    let ParsedQuery::Select { inner } = parse_query(q).expect("parse") else {
        panic!("expected SELECT: {q}");
    };
    let alg = translate_query_with(&inner, &SparqlConfig::default()).expect("translate");
    let logical = from_algebra(&alg);
    let ctx = PlanCtx { disabled_passes: disabled };
    let logical = run_passes(logical, &default_passes(), &ctx);
    let physical = lower(logical).expect("lower");
    Runtime::new(horn).run(&physical).unwrap().collect()
}

const QUERIES: &[&str] = &[
    "SELECT * WHERE { ?s <http://ex/name> ?n }",
    "SELECT ?s WHERE { ?s ?p ?o }",
    "SELECT ?n WHERE { ?s <http://ex/knows> ?o . ?s <http://ex/name> ?n }",
    "SELECT ?s WHERE { ?s <http://ex/age> ?age FILTER(?age > \"20\") }",
    "SELECT ?s ?n WHERE { ?s <http://ex/knows> ?o . ?s <http://ex/name> ?n FILTER(?o = <http://ex/b>) }",
    "SELECT ?s WHERE { ?s <http://ex/name> ?n OPTIONAL { ?s <http://ex/age> ?age } }",
    "SELECT ?s ?age WHERE { ?s <http://ex/name> ?n OPTIONAL { ?s <http://ex/age> ?age FILTER(?age > \"20\") } }",
    "SELECT ?x WHERE { { ?x <http://ex/name> ?n } UNION { ?x <http://ex/age> ?a } }",
    "SELECT DISTINCT ?n WHERE { ?s <http://ex/name> ?n }",
    "SELECT ?s WHERE { ?s <http://ex/age> ?age } ORDER BY ?age",
    "SELECT (COUNT(*) AS ?c) WHERE { ?s <http://ex/name> ?n }",
    "SELECT ?n (COUNT(?s) AS ?c) WHERE { ?s <http://ex/name> ?n } GROUP BY ?n",
    // OPTIONAL-side filter: FilterPushdown must NOT sink it into the optional arm.
    "SELECT ?s ?age WHERE { ?s <http://ex/name> ?n OPTIONAL { ?s <http://ex/age> ?age } FILTER(?age > \"10\" || !BOUND(?age)) }",
];

#[test]
fn full_pipeline_matches_all_passes_disabled() {
    let horn = fixture();
    let all: HashSet<PassId> = PHASE2_PASSES.into_iter().collect();
    for q in QUERIES {
        let with = run_with(&horn, q, HashSet::new());
        let without = run_with(&horn, q, all.clone());
        assert_eq!(canon(with), canon(without), "pipeline changed results for:\n{q}");
    }
}

#[test]
fn each_pass_is_individually_result_invariant() {
    let horn = fixture();
    for q in QUERIES {
        let baseline = canon(run_with(&horn, q, HashSet::new()));
        for pass in PHASE2_PASSES {
            let disabled: HashSet<PassId> = [pass].into_iter().collect();
            let got = canon(run_with(&horn, q, disabled));
            assert_eq!(
                baseline, got,
                "disabling {pass:?} changed results for:\n{q}\n\
                 (a real result change here is a bug IN that pass — it bisects cleanly)"
            );
        }
    }
}
```

- [ ] **Step 2: Run the battery**

Run: `cargo nextest run -p horndb-sparql -E 'binary(rewrite_invariance)'`
Expected: PASS (2 tests). A failure in `each_pass_is_individually_result_invariant` names the offending `PassId` — that is the Phase-2 acceptance mechanism (§7.2: a regression bisects to one pass).

- [ ] **Step 3: Conformance subset + WCOJ differential fuzzer**

Run: `cargo nextest run -p horndb-sparql --features server`
Expected: PASS — full SPARQL suite including the server tests.

Run: `cargo nextest run --workspace`
Expected: PASS — the harness conformance run (`harness/selected.toml`) and the WCOJ differential fuzzer stay green (§7.1). First run pulls `oxrocksdb-sys` (several minutes; cached after).

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings (what CI runs).

- [ ] **Step 4: Record the seams in `INTEGRATION-NOTES.md`**

Append to `crates/sparql/INTEGRATION-NOTES.md`:

```markdown
## Heuristic rewrite passes (SPEC-23 Phase 2, #185, 2026-07-07)

- Four `LogicalPass`es registered after `CoalesceBgp` in
  `plan::pass::default_passes`, source order: `Normalize` →
  `FilterPullup` → `FilterPushdown` → `ProjectionPushdown`. Each declares
  its `must_follow` and is disable-able via `PlanCtx.disabled_passes`.
- `Normalize` reduces `Eq → SameTerm` only where the type lattice proves
  both operands the same non-literal kind (IRI/blank), plus structural
  constant folding of variable-free filters. It never touches
  `?v = <const-literal>`, so the physical count-pushdown equality inlining
  (`plan::pushdown::eq_conjuncts`) is preserved.
- New `Expr::SameTerm` node: structural term equality, identical to `Eq`
  today. Becomes a genuine strength reduction once `Expr::Eq` gains
  value-equality (numeric-promotion) semantics.
- `FilterPushdown` honors the `LeftJoin` asymmetry: a conjunct never sinks
  into the optional (right) arm.
- `ProjectionPushdown` is the logical mirror of `plan::pushdown::prune`.
  Both run in Phase 2 (idempotent, result-invariant). Retiring the physical
  `prune` is deferred to Phase 4, when `JoinPlanning` consumes the logical
  narrowing directly. The physical `prune` stays because it also carries
  count-pushdown (`push_aggregates`), which is physical.
- Guard: `tests/rewrite_invariance.rs` — full pipeline vs each pass singly
  disabled; a regression bisects to one `PassId`.

Full rationale: `docs/specs/SPEC-23-unified-ir.md` §5.2, §6.2, §7.
```

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/sparql/tests/rewrite_invariance.rs crates/sparql/INTEGRATION-NOTES.md
git commit -m 'test(sparql): slot-differential invariance battery + per-PassId bisection (#185)'
```

- [ ] **Step 6: Optional shape smoke-check (NOT recorded)**

Phase 2 changes plan shape, not throughput; no `docs/benchmarks.md` row moves. If you want a local sanity read that filters/projections actually pushed (fewer scanned columns), run `EXPLAIN` on a couple of the battery queries and eyeball the plan — this is a laptop-local smoke check, **not** a recorded bench. Recorded numbers only ever come from `hornbench` (root `CLAUDE.md`); Phase 2 records none.

---

## Self-review checklist (done at plan-writing time)

**SPEC-23 Phase-2 requirement coverage:**

- **§5.2 initial pass set** `Normalize → FilterPullup → FilterPushdown → ProjectionPushdown` after `CoalesceBgp` → Tasks 2, 3, 4, 5, each appended to `default_passes()` in that source order, each with the correct `must_follow` (`Normalize`⟶`[CoalesceBgp]`, `FilterPullup`⟶`[Normalize]`, `FilterPushdown`⟶`[FilterPullup]`, `ProjectionPushdown`⟶`[FilterPushdown]`).
- **§5.2 `Equal→SameTerm`** grounded in the actual `Expr` enum: verified no `Func::SameTerm` / `Expr::SameTerm` exists and that `translate.rs:382-383` lowers both `Equal` and `sameTerm` to `Expr::Eq`, which already evaluates as term equality (`runtime.rs:1481`). Target = new `Expr::SameTerm` (Task 1), lattice-gated reduction (Task 2), preserving count-pushdown inlining.
- **§5.2 constant folding** → Task 2 `const_bool`/`normalize_filter` (drop true / empty false / drop always-satisfied conjuncts), structural and self-contained.
- **§5.2 filter pull-up/push-down with the `LeftJoin`/`Minus` asymmetry** → Task 3 (pull-up, never across `LeftJoin`) + Task 4 (push-down, never into the optional arm; `respects_leftjoin_asymmetry` test + a battery query exercising an OPTIONAL-side filter).
- **§5.2 projection pushdown overlapping `plan/pushdown.rs`** → Task 5 explicitly reconciles: logical pass is new; physical `prune` stays (coupled to count-pushdown), retirement deferred to Phase 4 — decision justified.
- **§6.2 always-beneficial, no statistics** → every pass is a pure structural/lattice rewrite; none consults `Stats`/cardinality.
- **§7.1 no-regression** → Task 6 runs `harness/selected.toml` conformance + WCOJ differential fuzzer via `cargo nextest run --workspace`.
- **§7.2 pass legibility / bisection** → `PlanCtx.disabled_passes` toggling; `each_pass_is_individually_result_invariant` proves a regression bisects to one `PassId`; `must_follow` constraints declared on each pass (asserted by the Phase-1 driver at startup).

**Interface contract:** every consumed name matches the pinned Phase-1 contract verbatim — `LogicalPlan` variants and smart constructors, `TypeMask`/`VarTypes`/`infer`, `PassId` (all six variants used), `PlanCtx.disabled_passes`, `LogicalPass::{id,run,must_follow}`, `run_passes`. Additions are additive only: `Expr::SameTerm` (algebra), `TypeMask::{is_named_node,is_blank_node}` (helpers, not part of the pinned surface), and the four pass structs.

**No placeholders:** every step contains complete Rust; every commit message is single-quoted with no `Co-authored-by` trailer (shell-hygiene + user rule). Test-first ordering throughout (failing test → run FAIL → implement → run PASS → commit). File map lists every file touched; `TASKS.md`/`architecture.md`/`metrics.md`/`benchmarks.md`/`index.md` are explicitly left to the integrating session.

**Assumptions flagged for the executor:** Phase-1 seam names (`from_algebra`, `lower::lower`, `default_passes`, `types.rs` bit constants, `Aggregate`/`AggFunc` locations) are used as the most plausible forms consistent with the pinned contract; if PLAN-23-01 landed them under different names, substitute the real ones — the pinned enum/trait shapes are the source of truth.
