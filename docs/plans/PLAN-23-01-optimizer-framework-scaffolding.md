---
status: draft
date: 2026-07-07
scope: "Phase-1 optimizer-framework scaffolding: logical IR, binding/type lattice, smart constructors, pass registry + driver, and a no-behavior-change wiring of planner::plan onto it"
---

# Optimizer framework scaffolding (SPEC-23 Phase 1) — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stand up the optimizer *framework* — a logical IR (`LogicalPlan`) distinct from the physical plan, a binding/type lattice, smart constructors, and a typed/ordered/toggleable pass registry with a driver — and rewire `plan::planner::plan` to run `Algebra → LogicalPlan → run_passes → PhysicalPlan`. **No behavior change:** the emitted `PhysicalPlan` for every existing query is structurally identical to today's 1:1 lowering, proven by golden-plan tests. This is SPEC-23 §6 item 1 (the scaffolding everything else in [#185](https://github.com/sunstoneinstitute/horndb/issues/185) hangs on). Design detail: SPEC-23 §5.1 (Plan IR) and §5.2 (Pass registry).

**Architecture:** A new logical layer under `crates/sparql/src/plan/`, borrowing three well-worn ideas: Oxigraph `sparopt`'s binding/type lattice (`type_inference.rs`) + smart constructors that fold trivia at build time; DuckDB's `RunOptimizer`/`OptimizerType` typed, individually-disable-able pass registry with debug plan verification after each pass; ClickHouse's `IQueryTreePass` "resolve-once, validate-after-each-pass" discipline and its lesson that pass ordering must be *declared*, not left in "must run before X" comments. The critical departure from `sparopt`: HornDB's BGP is a **flat n-ary `Bgp { patterns }`** (the WCOJ unit), not a tree of binary joins. Phase 1's lowering is deliberately *naive* (a 1:1 image of today's `planner::plan`); the only registered pass is `CoalesceBgp`, which folds `Join(Bgp, Bgp)` into one flat `Bgp`. Because spargebra already merges contiguous triple patterns into a single `Algebra::Bgp` (verified: `tests/algebra_translate.rs:45-52`), `Join(Bgp, Bgp)` never arises from real queries, so `CoalesceBgp` is a *no-op on today's corpus* — that is exactly what makes the golden-plan gate hold while the coalescing machinery is nonetheless real, tested, and toggleable. The existing `PhysicalPlan`-level `plan/pushdown.rs` rewrite (runs inside `Runtime::run_stream`) is **untouched** in Phase 1.

**Tech Stack:** Rust 1.90, `horndb-sparql` crate only. Uses the existing `crate::algebra` (`Algebra`, `LogicalPlan` mirror types `TriplePattern`/`Term`/`Var`/`Expr`/`OrderDir`/`Aggregate`) and `crate::plan::PhysicalPlan`. **No new dependencies.** `std::collections::{HashMap, HashSet}` only.

**Verification runner:** `cargo nextest run -p horndb-sparql` (and `--features server` for the server tests; see root `CLAUDE.md`). Doctests, if any, via `cargo test -p horndb-sparql --doc`. Benchmarks only ever on `hornbench` — none are recorded in this plan (Phase 1 is structural, not perf).

**File map (all under `crates/sparql/` unless noted):**

| File | Change |
|---|---|
| `src/plan/logical.rs` | **new** — `LogicalPlan` enum + smart constructors (`join`/`filter`/`union`) |
| `src/plan/types.rs` | **new** — `TypeMask`, `VarTypes`, `infer` (binding/type lattice) |
| `src/plan/pass.rs` | **new** — `PassId`, `LogicalPass`, `PlanCtx`, `run_passes`, `standard_passes`, `CoalesceBgp`, ordering assertion, debug validation |
| `src/plan/lower.rs` | **new** — `lower_algebra` (naive `Algebra → LogicalPlan`) + `lower_physical` (`LogicalPlan → PhysicalPlan`) |
| `src/plan/mod.rs` | declare the four new modules; keep `PhysicalPlan` unchanged |
| `src/plan/planner.rs` | rewrite `plan` onto the pipeline; add `plan_with_ctx` |
| `src/parser.rs` | add `strip_plan_pragmas` (`PRAGMA disable-pass=<PassId>` leading pragma) |
| `src/api.rs` | strip plan pragmas, build `PlanCtx`, thread it through the SELECT / EXPLAIN planning calls |
| `tests/logical_pipeline.rs` | **new** — golden-plan equivalence, coalescing result-invariance, pragma end-to-end |

Do **not** touch `TASKS.md`, `docs/benchmarks.md`, `docs/architecture.md`, `docs/index.md`, or `docs/metrics.md` — the integrating session syncs those (root `CLAUDE.md` doc-sync rule) when this branch merges. No metric is added or changed in Phase 1.

---

### Task 1: `logical.rs` — the `LogicalPlan` IR and smart constructors

The logical IR mirrors `PhysicalPlan`'s variants but with the **flat n-ary `Bgp`** as the join unit. Smart constructors fold trivia at build time; Phase-1 lowering does **not** use them (it stays naive so the golden gate holds), but `CoalesceBgp` (Task 4) and Phase-2 passes do — so they must exist and be tested now.

**Files:**
- Create: `crates/sparql/src/plan/logical.rs`
- Modify: `crates/sparql/src/plan/mod.rs:4-6` (add `pub mod logical;`)

- [ ] **Step 1: Declare the module and write the failing tests**

In `crates/sparql/src/plan/mod.rs`, add after line 6 (`pub mod pushdown;`):

```rust
pub mod logical;
pub mod lower;
pub mod pass;
pub mod types;
```

Then create `crates/sparql/src/plan/logical.rs` with the smart-constructor tests first (the type does not exist yet, so this fails to build):

```rust
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

/// A logical query plan node.
#[derive(Debug, Clone, PartialEq)]
pub enum LogicalPlan {
    /// Flat, n-ary basic graph pattern — the WCOJ unit.
    Bgp { patterns: Vec<TriplePattern> },
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
    /// Boolean filter.
    Filter {
        expr: Expr,
        inner: Box<LogicalPlan>,
    },
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
    /// Recursive Kleene property path `p+`/`p*`.
    PathClosure {
        subject: Term,
        object: Term,
        edge: Box<LogicalPlan>,
        reflexive: bool,
    },
}

impl LogicalPlan {
    /// Join two subtrees, coalescing **adjacent flat `Bgp`s into one flat
    /// `Bgp`** (SPEC-23 §5.1 — the inverse of `sparopt`'s flatten-and-rebuild,
    /// done once). Any non-`Bgp` operand keeps a real `Join`.
    pub fn join(left: LogicalPlan, right: LogicalPlan) -> LogicalPlan {
        match (left, right) {
            (LogicalPlan::Bgp { patterns: mut l }, LogicalPlan::Bgp { patterns: r }) => {
                l.extend(r);
                LogicalPlan::Bgp { patterns: l }
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

/// True iff `expr` is the constant boolean `true` in any of its canonical
/// spargebra-printed literal forms.
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
        LogicalPlan::Bgp { patterns: pats }
    }

    #[test]
    fn join_coalesces_adjacent_bgps_into_one_flat_bgp() {
        let left = bgp(vec![pat("s", "p", "o")]);
        let right = bgp(vec![pat("o", "q", "z")]);
        match LogicalPlan::join(left, right) {
            LogicalPlan::Bgp { patterns } => assert_eq!(patterns.len(), 2),
            other => panic!("expected coalesced Bgp, got {other:?}"),
        }
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
        let out = LogicalPlan::filter(
            Expr::Term(Term::Literal("true".into())),
            inner.clone(),
        );
        assert_eq!(out, inner, "constant-true filter must fold away");
    }

    #[test]
    fn filter_keeps_a_real_predicate() {
        let inner = bgp(vec![pat("s", "p", "o")]);
        let out = LogicalPlan::filter(Expr::Bound(Var::new("o")), inner);
        assert!(matches!(out, LogicalPlan::Filter { .. }));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p horndb-sparql -E 'package(horndb-sparql) and test(join_coalesces_adjacent_bgps_into_one_flat_bgp)'`
Expected: build FAILURE — `cannot find … LogicalPlan` is resolved once `logical.rs` exists, but the module was only just declared; the test module compiles and the four asserts run. If you staged only `mod.rs`, expect `file not found for module logical`.

- [ ] **Step 3: Implement**

The code in Step 1 *is* the implementation (the enum, `impl LogicalPlan`, and `is_constant_true` are above the `#[cfg(test)]` block). No further code needed — this task is the type definition plus its unit tests.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run -p horndb-sparql -E 'package(horndb-sparql) and binary_id(horndb-sparql)' -E 'test(join_) or test(filter_)'`
Expected: PASS (4 tests). Then `cargo nextest run -p horndb-sparql` — the new module must not break the crate build (the other new modules are declared but not yet created, so create empty stubs if you split commits; simplest is to land Tasks 1–4 before running the full suite). For a clean single-task build, temporarily comment the `pass`/`types`/`lower` module lines and restore them in their tasks, **or** land Tasks 1–4 together.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/sparql/src/plan/logical.rs crates/sparql/src/plan/mod.rs
git commit -m 'feat(sparql): LogicalPlan IR with flat n-ary Bgp + smart constructors (#185)'
```

---

### Task 2: `types.rs` — binding/type lattice and `infer`

A 5-bit per-variable lattice (`UNDEF, NAMED_NODE, BLANK_NODE, LITERAL, TRIPLE`), ported from `sparopt`'s `type_inference.rs`. "Bound" is derived: a var is bound iff its `UNDEF` bit is clear. `infer` propagates bottom-up — `Join` intersects shared-var masks, `LeftJoin` marks right-only vars `UNDEF`, `Union` unions-with-`UNDEF` for one-sided vars. Phase 1 builds and validates the lattice; Phase-2 passes consume it (filter-pushdown legality, join-key discovery).

**Files:**
- Create: `crates/sparql/src/plan/types.rs`

- [ ] **Step 1: Write the failing tests**

Create `crates/sparql/src/plan/types.rs`:

```rust
//! Binding/type lattice (SPEC-23 §5.1, ported from Oxigraph `sparopt`'s
//! `type_inference.rs`). A [`TypeMask`] is a 5-bit set over
//! `{UNDEF, NAMED_NODE, BLANK_NODE, LITERAL, TRIPLE}`; a variable is *bound*
//! iff its `UNDEF` bit is clear. [`infer`] propagates masks bottom-up over a
//! [`LogicalPlan`]. This single sound-by-construction pass is what
//! filter-pushdown legality, join-key discovery, and the WCOJ-vs-hash
//! decision (Phase 2+) will read.

use crate::algebra::{Term, TriplePattern, Var};
use crate::plan::logical::LogicalPlan;
use std::collections::HashMap;

/// A set of possible term kinds for one variable. Bit layout matches
/// `sparopt`'s `VariableType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TypeMask(u8);

impl TypeMask {
    /// Variable may be *unbound* in some solution.
    pub const UNDEF: u8 = 0b0_0001;
    /// May be an IRI.
    pub const NAMED_NODE: u8 = 0b0_0010;
    /// May be a blank node.
    pub const BLANK_NODE: u8 = 0b0_0100;
    /// May be a literal.
    pub const LITERAL: u8 = 0b0_1000;
    /// May be an RDF 1.2 triple term.
    pub const TRIPLE: u8 = 0b1_0000;
    /// Any bound RDF term (no `UNDEF`).
    pub const ANY: u8 = Self::NAMED_NODE | Self::BLANK_NODE | Self::LITERAL | Self::TRIPLE;

    /// Construct from a raw bit set.
    pub fn from_bits(bits: u8) -> Self {
        Self(bits)
    }
    /// The raw bit set.
    pub fn bits(&self) -> u8 {
        self.0
    }
    /// A definitely-bound variable is one whose `UNDEF` bit is clear.
    pub fn is_bound(&self) -> bool {
        self.0 & Self::UNDEF == 0
    }
    /// Union two masks (set of possibilities on either branch).
    pub fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
    /// Intersect the *type* bits (both branches agree) while a variable stays
    /// bound iff it is bound on either side — matching a join, where a shared
    /// var is produced by both patterns.
    pub fn intersect(self, other: Self) -> Self {
        let types = self.0 & other.0 & Self::ANY;
        let undef = (self.0 & other.0) & Self::UNDEF;
        Self(types | undef)
    }
    /// Return this mask with the `UNDEF` bit set (may be absent).
    pub fn with_undef(self) -> Self {
        Self(self.0 | Self::UNDEF)
    }
}

/// Inferred [`TypeMask`] per variable produced by a plan.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VarTypes(HashMap<Var, TypeMask>);

impl VarTypes {
    /// Mask for `var`, if the plan produces it.
    pub fn get(&self, var: &Var) -> Option<TypeMask> {
        self.0.get(var).copied()
    }
    /// The set of variables the plan may bind.
    pub fn vars(&self) -> impl Iterator<Item = &Var> {
        self.0.keys()
    }
    fn insert_union(&mut self, var: Var, mask: TypeMask) {
        self.0
            .entry(var)
            .and_modify(|m| *m = m.union(mask))
            .or_insert(mask);
    }
}

/// The bound mask a variable receives when it appears in a given triple-pattern
/// position (subject / predicate / object).
fn subject_mask() -> TypeMask {
    TypeMask::from_bits(TypeMask::NAMED_NODE | TypeMask::BLANK_NODE)
}
fn predicate_mask() -> TypeMask {
    TypeMask::from_bits(TypeMask::NAMED_NODE)
}
fn object_mask() -> TypeMask {
    TypeMask::from_bits(TypeMask::ANY)
}

fn add_pattern_vars(p: &TriplePattern, out: &mut VarTypes) {
    add_term_vars(&p.subject, subject_mask(), out);
    add_term_vars(&p.predicate, predicate_mask(), out);
    add_term_vars(&p.object, object_mask(), out);
}

fn add_term_vars(t: &Term, mask: TypeMask, out: &mut VarTypes) {
    match t {
        Term::Var(v) => out.insert_union(v.clone(), mask),
        // RDF 1.2 triple term: its inner variables are bound by the outer
        // pattern's position; recurse with the same mask (sound over-approx).
        Term::Triple(tp) => add_pattern_vars(tp, out),
        _ => {}
    }
}

/// Bottom-up type inference over a [`LogicalPlan`].
pub fn infer(plan: &LogicalPlan) -> VarTypes {
    use LogicalPlan::*;
    match plan {
        Bgp { patterns } => {
            let mut vt = VarTypes::default();
            for p in patterns {
                add_pattern_vars(p, &mut vt);
            }
            vt
        }
        // Join: shared vars intersect; each side's own vars pass through.
        Join { left, right } => {
            let l = infer(left);
            let r = infer(right);
            let mut out = l.clone();
            for (v, rm) in r.0 {
                match out.0.get(&v).copied() {
                    Some(lm) => {
                        out.0.insert(v, lm.intersect(rm));
                    }
                    None => {
                        out.0.insert(v, rm);
                    }
                }
            }
            out
        }
        // LeftJoin: left as-is; right-only vars become optional (UNDEF).
        LeftJoin { left, right, .. } => {
            let l = infer(left);
            let r = infer(right);
            let mut out = l.clone();
            for (v, rm) in r.0 {
                match out.0.get(&v).copied() {
                    Some(lm) => {
                        out.0.insert(v, lm.union(rm));
                    }
                    None => {
                        out.0.insert(v, rm.with_undef());
                    }
                }
            }
            out
        }
        // Union: shared vars union; one-sided vars union-with-UNDEF.
        Union { left, right } => {
            let l = infer(left);
            let r = infer(right);
            let mut out = VarTypes::default();
            for (v, m) in l.0.iter() {
                let mask = if r.0.contains_key(v) { *m } else { m.with_undef() };
                out.0.insert(v.clone(), mask);
            }
            for (v, m) in r.0.iter() {
                match out.0.get(v).copied() {
                    Some(existing) => {
                        out.0.insert(v.clone(), existing.union(*m));
                    }
                    None => {
                        out.0.insert(v.clone(), m.with_undef());
                    }
                }
            }
            out
        }
        Filter { inner, .. } | Slice { inner, .. } | OrderBy { inner, .. } | Distinct { inner } => {
            infer(inner)
        }
        Project { vars, inner } => {
            let child = infer(inner);
            let mut out = VarTypes::default();
            for v in vars {
                let mask = child
                    .get(v)
                    .unwrap_or_else(|| TypeMask::from_bits(TypeMask::UNDEF | TypeMask::ANY));
                out.0.insert(v.clone(), mask);
            }
            out
        }
        // BIND: the bound expression may error → the new var is optional.
        Extend { inner, var, .. } => {
            let mut out = infer(inner);
            out.insert_union(
                var.clone(),
                TypeMask::from_bits(TypeMask::UNDEF | TypeMask::ANY),
            );
            out
        }
        Values { vars, rows } => {
            let mut out = VarTypes::default();
            for (col, v) in vars.iter().enumerate() {
                let mut mask = 0u8;
                for row in rows {
                    match row.get(col).and_then(|c| c.as_ref()) {
                        None => mask |= TypeMask::UNDEF,
                        Some(t) => mask |= term_kind_bit(t),
                    }
                }
                out.0.insert(v.clone(), TypeMask::from_bits(mask));
            }
            out
        }
        Group {
            inner,
            keys,
            aggregates,
        } => {
            let child = infer(inner);
            let mut out = VarTypes::default();
            for k in keys {
                let mask = child
                    .get(k)
                    .unwrap_or_else(|| TypeMask::from_bits(TypeMask::UNDEF | TypeMask::ANY));
                out.0.insert(k.clone(), mask);
            }
            // Aggregate outputs are bound literals (numbers / group_concat).
            for a in aggregates {
                out.0
                    .insert(a.out.clone(), TypeMask::from_bits(TypeMask::LITERAL));
            }
            out
        }
        PathClosure {
            subject, object, ..
        } => {
            let mut out = VarTypes::default();
            for t in [subject, object] {
                if let Term::Var(v) = t {
                    out.insert_union(
                        v.clone(),
                        TypeMask::from_bits(TypeMask::NAMED_NODE | TypeMask::BLANK_NODE),
                    );
                }
            }
            out
        }
    }
}

fn term_kind_bit(t: &Term) -> u8 {
    match t {
        Term::Iri(_) => TypeMask::NAMED_NODE,
        Term::BlankNode(_) => TypeMask::BLANK_NODE,
        Term::Literal(_) => TypeMask::LITERAL,
        Term::Triple(_) => TypeMask::TRIPLE,
        // A Var in a VALUES cell is not a ground term; treat as any-bound.
        Term::Var(_) => TypeMask::ANY,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algebra::Expr;

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

    #[test]
    fn bgp_binds_positions() {
        let vt = infer(&bgp(vec![pat("s", "p", "o")]));
        let s = vt.get(&Var::new("s")).unwrap();
        assert!(s.is_bound());
        // subject can be IRI or blank, never literal
        assert_eq!(s.bits() & TypeMask::LITERAL, 0);
        let o = vt.get(&Var::new("o")).unwrap();
        assert!(o.is_bound());
        assert_ne!(o.bits() & TypeMask::LITERAL, 0);
    }

    #[test]
    fn leftjoin_marks_rhs_only_vars_undef() {
        // { ?s :p ?o OPTIONAL { ?s :q ?x } } — ?x may be unbound.
        let left = bgp(vec![pat("s", "p", "o")]);
        let right = bgp(vec![pat("s", "q", "x")]);
        let vt = infer(&LogicalPlan::LeftJoin {
            left: Box::new(left),
            right: Box::new(right),
            expr: None,
        });
        assert!(vt.get(&Var::new("s")).unwrap().is_bound(), "?s stays bound");
        assert!(
            !vt.get(&Var::new("x")).unwrap().is_bound(),
            "?x is optional → UNDEF set"
        );
    }

    #[test]
    fn union_marks_one_sided_vars_undef() {
        // { { ?x :p ?a } UNION { ?x :q ?b } } — ?a and ?b each one-sided.
        let left = bgp(vec![pat("x", "p", "a")]);
        let right = bgp(vec![pat("x", "q", "b")]);
        let vt = infer(&LogicalPlan::Union {
            left: Box::new(left),
            right: Box::new(right),
        });
        assert!(vt.get(&Var::new("x")).unwrap().is_bound(), "?x on both sides");
        assert!(!vt.get(&Var::new("a")).unwrap().is_bound());
        assert!(!vt.get(&Var::new("b")).unwrap().is_bound());
    }

    #[test]
    fn join_keeps_shared_var_bound() {
        // Join of two BGPs sharing ?o (built raw, not coalesced).
        let left = bgp(vec![pat("s", "p", "o")]);
        let right = bgp(vec![pat("o", "q", "z")]);
        let vt = infer(&LogicalPlan::Join {
            left: Box::new(left),
            right: Box::new(right),
        });
        assert!(vt.get(&Var::new("o")).unwrap().is_bound());
        // ?o is a subject on the right, an object on the left → intersection
        // excludes LITERAL (subject can't be a literal).
        assert_eq!(vt.get(&Var::new("o")).unwrap().bits() & TypeMask::LITERAL, 0);
    }

    #[test]
    fn extend_output_is_optional() {
        let inner = bgp(vec![pat("s", "p", "o")]);
        let vt = infer(&LogicalPlan::Extend {
            inner: Box::new(inner),
            var: Var::new("b"),
            expr: Expr::Bound(Var::new("o")),
        });
        assert!(!vt.get(&Var::new("b")).unwrap().is_bound());
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p horndb-sparql -E 'test(bgp_binds_positions) or test(leftjoin_marks_rhs_only_vars_undef) or test(union_marks_one_sided_vars_undef)'`
Expected: build FAILURE first time (module `types` referenced by `mod.rs` but file absent) — then, once created, the tests compile and run.

- [ ] **Step 3: Implement**

The code above is the implementation (everything before the `#[cfg(test)]` block). No further code.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run -p horndb-sparql -E 'test(bgp_binds_positions) or test(leftjoin_marks_rhs_only_vars_undef) or test(union_marks_one_sided_vars_undef) or test(join_keeps_shared_var_bound) or test(extend_output_is_optional)'`
Expected: PASS (5 tests).

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/sparql/src/plan/types.rs
git commit -m 'feat(sparql): binding/type lattice + infer over LogicalPlan (#185)'
```

---

### Task 3: `lower.rs` — naive `Algebra → LogicalPlan` and `LogicalPlan → PhysicalPlan`

Both directions are a **naive 1:1** mapping (`Bgp → Bgp`, `Join → Join`, …). `lower_algebra` does **not** coalesce (that is the `CoalesceBgp` pass's job, Task 4) and does **not** call the folding smart constructors — so `lower_physical(lower_algebra(alg))` reproduces today's `planner::plan(alg)` byte-for-byte. That round-trip equality is the phase gate and is tested here.

**Files:**
- Create: `crates/sparql/src/plan/lower.rs`

- [ ] **Step 1: Write the failing tests**

Create `crates/sparql/src/plan/lower.rs`:

```rust
//! Algebra ⇄ physical bridging through the logical IR (SPEC-23 §5.1).
//!
//! [`lower_algebra`] is a **naive** 1:1 image of `crate::algebra::Algebra`
//! into [`LogicalPlan`] — no coalescing, no folding — so that
//! `lower_physical(lower_algebra(alg))` is structurally identical to the
//! pre-refactor `planner::plan(alg)`. Coalescing is a *pass* (`CoalesceBgp`),
//! keeping the transformation in one bisectable place. [`lower_physical`]
//! maps a (possibly coalesced) [`LogicalPlan`] back to
//! [`crate::plan::PhysicalPlan`]; a flat `Bgp { patterns }` lowers to
//! `BgpScan { patterns }`, which the WCOJ executor runs as the natural join
//! of the whole pattern set — result-equivalent to the nested
//! `Join(BgpScan, BgpScan)` today's lowering emits (proven in
//! `tests/logical_pipeline.rs`).

use crate::algebra::Algebra;
use crate::plan::logical::LogicalPlan;
use crate::plan::PhysicalPlan;

/// Naive `Algebra → LogicalPlan` (no coalescing, no folding).
pub fn lower_algebra(alg: &Algebra) -> LogicalPlan {
    match alg {
        Algebra::Bgp { patterns } => LogicalPlan::Bgp {
            patterns: patterns.clone(),
        },
        Algebra::Join { left, right } => LogicalPlan::Join {
            left: Box::new(lower_algebra(left)),
            right: Box::new(lower_algebra(right)),
        },
        Algebra::LeftJoin { left, right, expr } => LogicalPlan::LeftJoin {
            left: Box::new(lower_algebra(left)),
            right: Box::new(lower_algebra(right)),
            expr: expr.clone(),
        },
        Algebra::Filter { expr, inner } => LogicalPlan::Filter {
            expr: expr.clone(),
            inner: Box::new(lower_algebra(inner)),
        },
        Algebra::Union { left, right } => LogicalPlan::Union {
            left: Box::new(lower_algebra(left)),
            right: Box::new(lower_algebra(right)),
        },
        Algebra::Project { vars, inner } => LogicalPlan::Project {
            vars: vars.clone(),
            inner: Box::new(lower_algebra(inner)),
        },
        Algebra::Distinct { inner } => LogicalPlan::Distinct {
            inner: Box::new(lower_algebra(inner)),
        },
        Algebra::Slice {
            inner,
            start,
            length,
        } => LogicalPlan::Slice {
            inner: Box::new(lower_algebra(inner)),
            start: *start,
            length: *length,
        },
        Algebra::OrderBy { inner, keys } => LogicalPlan::OrderBy {
            inner: Box::new(lower_algebra(inner)),
            keys: keys.clone(),
        },
        Algebra::Extend { inner, var, expr } => LogicalPlan::Extend {
            inner: Box::new(lower_algebra(inner)),
            var: var.clone(),
            expr: expr.clone(),
        },
        Algebra::Values { vars, rows } => LogicalPlan::Values {
            vars: vars.clone(),
            rows: rows.clone(),
        },
        Algebra::Group {
            inner,
            keys,
            aggregates,
        } => LogicalPlan::Group {
            inner: Box::new(lower_algebra(inner)),
            keys: keys.clone(),
            aggregates: aggregates.clone(),
        },
        Algebra::PathClosure {
            subject,
            object,
            edge,
            reflexive,
        } => LogicalPlan::PathClosure {
            subject: subject.clone(),
            object: object.clone(),
            edge: Box::new(lower_algebra(edge)),
            reflexive: *reflexive,
        },
    }
}

/// `LogicalPlan → PhysicalPlan`. A flat `Bgp` lowers to `BgpScan` (the WCOJ
/// executor runs the whole pattern set as one natural join).
pub fn lower_physical(plan: &LogicalPlan) -> PhysicalPlan {
    match plan {
        LogicalPlan::Bgp { patterns } => PhysicalPlan::BgpScan {
            patterns: patterns.clone(),
        },
        LogicalPlan::Join { left, right } => PhysicalPlan::Join {
            left: Box::new(lower_physical(left)),
            right: Box::new(lower_physical(right)),
        },
        LogicalPlan::LeftJoin { left, right, expr } => PhysicalPlan::LeftJoin {
            left: Box::new(lower_physical(left)),
            right: Box::new(lower_physical(right)),
            expr: expr.clone(),
        },
        LogicalPlan::Filter { expr, inner } => PhysicalPlan::Filter {
            expr: expr.clone(),
            inner: Box::new(lower_physical(inner)),
        },
        LogicalPlan::Union { left, right } => PhysicalPlan::Union {
            left: Box::new(lower_physical(left)),
            right: Box::new(lower_physical(right)),
        },
        LogicalPlan::Project { vars, inner } => PhysicalPlan::Project {
            vars: vars.clone(),
            inner: Box::new(lower_physical(inner)),
        },
        LogicalPlan::Distinct { inner } => PhysicalPlan::Distinct {
            inner: Box::new(lower_physical(inner)),
        },
        LogicalPlan::Slice {
            inner,
            start,
            length,
        } => PhysicalPlan::Slice {
            inner: Box::new(lower_physical(inner)),
            start: *start,
            length: *length,
        },
        LogicalPlan::OrderBy { inner, keys } => PhysicalPlan::OrderBy {
            inner: Box::new(lower_physical(inner)),
            keys: keys.clone(),
        },
        LogicalPlan::Extend { inner, var, expr } => PhysicalPlan::Extend {
            inner: Box::new(lower_physical(inner)),
            var: var.clone(),
            expr: expr.clone(),
        },
        LogicalPlan::Values { vars, rows } => PhysicalPlan::Values {
            vars: vars.clone(),
            rows: rows.clone(),
        },
        LogicalPlan::Group {
            inner,
            keys,
            aggregates,
        } => PhysicalPlan::Group {
            inner: Box::new(lower_physical(inner)),
            keys: keys.clone(),
            aggregates: aggregates.clone(),
        },
        LogicalPlan::PathClosure {
            subject,
            object,
            edge,
            reflexive,
        } => PhysicalPlan::PathClosure {
            subject: subject.clone(),
            object: object.clone(),
            edge: Box::new(lower_physical(edge)),
            reflexive: *reflexive,
        },
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

    #[test]
    fn bgp_round_trips_to_bgp_scan() {
        let alg = Algebra::Bgp {
            patterns: vec![pat("s", "http://ex/p", "o")],
        };
        let phys = lower_physical(&lower_algebra(&alg));
        assert_eq!(
            phys,
            PhysicalPlan::BgpScan {
                patterns: vec![pat("s", "http://ex/p", "o")]
            }
        );
    }

    #[test]
    fn naive_join_stays_a_nested_join() {
        // lower_algebra must NOT coalesce — that is CoalesceBgp's job.
        let alg = Algebra::Join {
            left: Box::new(Algebra::Bgp {
                patterns: vec![pat("s", "http://ex/p", "o")],
            }),
            right: Box::new(Algebra::Bgp {
                patterns: vec![pat("o", "http://ex/q", "z")],
            }),
        };
        let log = lower_algebra(&alg);
        assert!(
            matches!(log, LogicalPlan::Join { .. }),
            "naive lowering keeps the Join; got {log:?}"
        );
        assert!(matches!(
            lower_physical(&log),
            PhysicalPlan::Join { .. }
        ));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p horndb-sparql -E 'test(bgp_round_trips_to_bgp_scan) or test(naive_join_stays_a_nested_join)'`
Expected: build FAILURE until the file exists, then PASS-shaped once implemented (the code above is the implementation).

- [ ] **Step 3: Implement**

The code above is the implementation.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run -p horndb-sparql -E 'test(bgp_round_trips_to_bgp_scan) or test(naive_join_stays_a_nested_join)'`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/sparql/src/plan/lower.rs
git commit -m 'feat(sparql): naive Algebra<->LogicalPlan<->PhysicalPlan lowering (#185)'
```

---

### Task 4: `pass.rs` — pass registry, driver, `CoalesceBgp`, ordering assertion, debug validation

The typed pipeline: `PassId` (all six SPEC-23 §5.2 names — only `CoalesceBgp` has a registered pass in Phase 1), the `LogicalPass` trait, `PlanCtx` (carrying `disabled_passes`), and `run_passes`. The driver skips disabled passes, asserts each pass's declared `must_follow` ordering at startup, and — in debug builds — re-runs `infer` + a structural `validate` after every pass (ClickHouse `ValidationChecker` discipline). `CoalesceBgp` folds `Join(Bgp, Bgp)` bottom-up via the `LogicalPlan::join` smart constructor.

**Files:**
- Create: `crates/sparql/src/plan/pass.rs`

- [ ] **Step 1: Write the failing tests**

Create `crates/sparql/src/plan/pass.rs`:

```rust
//! Pass registry + driver (SPEC-23 §5.2), modeled on DuckDB's
//! `RunOptimizer`/`OptimizerType` and ClickHouse's `IQueryTreePass`.
//!
//! * Typed, ordered, individually **disable-able** passes (`PlanCtx`).
//! * Ordering constraints are **declared** (`LogicalPass::must_follow`) and
//!   asserted at startup — not left as "must run before X" comments.
//! * Debug builds re-infer the lattice and structurally **validate** the IR
//!   after every pass, so a plan regression bisects to one `PassId`.
//!
//! Phase 1 registers exactly one pass, [`CoalesceBgp`]. The other `PassId`
//! variants exist so Phase-2+ passes slot in without an enum change and so a
//! pragma can name them.

use crate::plan::logical::LogicalPlan;
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
        match s {
            "coalesce-bgp" => Ok(PassId::CoalesceBgp),
            "normalize" => Ok(PassId::Normalize),
            "filter-pullup" => Ok(PassId::FilterPullup),
            "filter-pushdown" => Ok(PassId::FilterPushdown),
            "projection-pushdown" => Ok(PassId::ProjectionPushdown),
            "join-planning" => Ok(PassId::JoinPlanning),
            other => Err(format!("unknown pass id `{other}`")),
        }
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

/// The Phase-1 pipeline. Source order == run order.
pub fn standard_passes() -> Vec<Box<dyn LogicalPass>> {
    let passes: Vec<Box<dyn LogicalPass>> = vec![Box::new(CoalesceBgp)];
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
/// builds the IR is validated after each pass.
pub fn run_passes(
    mut plan: LogicalPlan,
    passes: &[Box<dyn LogicalPass>],
    ctx: &PlanCtx,
) -> LogicalPlan {
    for p in passes {
        if ctx.disabled_passes.contains(&p.id()) {
            continue;
        }
        plan = p.run(plan, ctx);
        #[cfg(debug_assertions)]
        validate(&plan).unwrap_or_else(|e| panic!("IR invalid after pass {:?}: {e}", p.id()));
    }
    plan
}

/// Structural post-pass check: `infer` must succeed and every variable a node
/// *references* (Project list, Filter/OrderBy/Extend expression vars) must be
/// produced somewhere below it — no dangling variables.
#[cfg(debug_assertions)]
pub(crate) fn validate(plan: &LogicalPlan) -> Result<(), String> {
    use crate::exec::runtime::referenced_vars;
    use crate::algebra::Var;
    use std::collections::HashSet as Set;

    // A var is "known" if the whole plan's inferred output binds it, OR it is
    // bound deeper (Project may hide it). We take the conservative union of the
    // node's own inferred vars for each referencing node.
    fn check(node: &LogicalPlan) -> Result<(), String> {
        let produced: Set<Var> = infer(node).vars().cloned().collect();
        match node {
            LogicalPlan::Project { vars, inner } => {
                let inner_vars: Set<Var> = infer(inner).vars().cloned().collect();
                for v in vars {
                    if !inner_vars.contains(v) {
                        return Err(format!("Project references unbound ?{}", v.name()));
                    }
                }
                check(inner)
            }
            LogicalPlan::Filter { expr, inner } => {
                let mut refs: Set<String> = Set::new();
                referenced_vars(expr, &mut refs);
                let inner_vars: Set<String> =
                    infer(inner).vars().map(|v| v.name().to_owned()).collect();
                for r in &refs {
                    if !inner_vars.contains(r) {
                        return Err(format!("Filter references unbound ?{r}"));
                    }
                }
                let _ = produced;
                check(inner)
            }
            // Structural recursion into every child; leaf nodes are trivially ok.
            LogicalPlan::Join { left, right }
            | LogicalPlan::LeftJoin { left, right, .. }
            | LogicalPlan::Union { left, right } => {
                check(left)?;
                check(right)
            }
            LogicalPlan::Distinct { inner }
            | LogicalPlan::Slice { inner, .. }
            | LogicalPlan::OrderBy { inner, .. }
            | LogicalPlan::Extend { inner, .. }
            | LogicalPlan::Group { inner, .. } => check(inner),
            LogicalPlan::PathClosure { edge, .. } => check(edge),
            LogicalPlan::Bgp { .. } | LogicalPlan::Values { .. } => Ok(()),
        }
    }
    check(plan)
}

/// `CoalesceBgp` (SPEC-23 §5.2): fold contiguous `Join(Bgp, Bgp)` into one
/// flat `Bgp`, bottom-up, via the [`LogicalPlan::join`] smart constructor.
/// Idempotent. On today's corpus this never fires — spargebra already merges
/// adjacent triple patterns into one `Algebra::Bgp` — so it is a no-op that
/// preserves every existing plan (the Phase-1 gate). It becomes load-bearing
/// once passes below it (Phase 2) split and recombine BGPs.
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
    fn pass_id_round_trips_through_str() {
        for id in [
            PassId::CoalesceBgp,
            PassId::Normalize,
            PassId::FilterPullup,
            PassId::FilterPushdown,
            PassId::ProjectionPushdown,
            PassId::JoinPlanning,
        ] {
            assert_eq!(id.as_str().parse::<PassId>().unwrap(), id);
        }
    }
}
```

Note: `validate` reuses `crate::exec::runtime::referenced_vars` (already `pub(crate)` — used by `plan/pushdown.rs:63`) and `crate::algebra::Var`. Confirm the import path resolves; if `referenced_vars` is not visible from `plan::pass`, add a thin re-export rather than duplicating it.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p horndb-sparql -E 'test(coalesce_folds_join_of_bgps) or test(disabling_coalesce_keeps_the_join) or test(assert_pass_order_rejects_a_violated_constraint)'`
Expected: build FAILURE until the file exists; then the tests compile and run.

- [ ] **Step 3: Implement**

The code above is the implementation.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run -p horndb-sparql -E 'test(coalesce_folds_join_of_bgps) or test(disabling_coalesce_keeps_the_join) or test(assert_pass_order_accepts_a_satisfied_constraint) or test(assert_pass_order_rejects_a_violated_constraint) or test(pass_id_round_trips_through_str)'`
Expected: PASS (5 tests). Then run the whole crate to confirm the four new modules integrate: `cargo nextest run -p horndb-sparql`. Expected: PASS (planner is not yet rewired, so nothing else changed).

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/sparql/src/plan/pass.rs
git commit -m 'feat(sparql): pass registry, driver, CoalesceBgp, ordering + IR validation (#185)'
```

---

### Task 5: rewire `planner::plan` onto the pipeline + golden-plan gate

Replace the body of `plan` with `lower_algebra → run_passes(standard_passes) → lower_physical`, and add `plan_with_ctx` so a caller (the pragma path, Task 6) can pass a `PlanCtx`. The public signature `plan(alg: &Algebra) -> Result<PhysicalPlan>` is unchanged, so `api.rs`, `plan_of`, and `plan_select` need no edits here. The golden gate lives in a new integration test.

**Files:**
- Modify: `crates/sparql/src/plan/planner.rs:12-84` (replace `plan`'s body; add `plan_with_ctx`; keep the inline tests)
- Create: `crates/sparql/tests/logical_pipeline.rs`

- [ ] **Step 1: Write the failing golden-plan tests**

Create `crates/sparql/tests/logical_pipeline.rs`:

```rust
//! SPEC-23 Phase-1 gate: the logical pipeline reproduces today's physical
//! plans (no behavior change), coalescing a flat BGP is result-invariant, and
//! passes are individually disable-able.

use horndb_sparql::algebra::translate::translate_query_with;
use horndb_sparql::algebra::{Algebra, Term, TriplePattern, Var};
use horndb_sparql::exec::horn::HornBackend;
use horndb_sparql::exec::runtime::Runtime;
use horndb_sparql::exec::{Bindings, Store};
use horndb_sparql::parser::{parse_query, ParsedQuery};
use horndb_sparql::plan::lower::{lower_algebra, lower_physical};
use horndb_sparql::plan::pass::{run_passes, standard_passes, PassId, PlanCtx};
use horndb_sparql::plan::{planner, PhysicalPlan};
use horndb_sparql::SparqlConfig;
use std::collections::HashSet;

fn algebra_of(q: &str) -> Algebra {
    let inner = match parse_query(q).expect("parse") {
        ParsedQuery::Select { inner } => inner,
        other => panic!("expected SELECT, got {other:?}"),
    };
    translate_query_with(&inner, &SparqlConfig::default()).expect("translate")
}

/// The pre-refactor reference: a straight 1:1 Algebra → PhysicalPlan lowering,
/// frozen here so the golden comparison is self-contained.
fn reference_plan(alg: &Algebra) -> PhysicalPlan {
    match alg {
        Algebra::Bgp { patterns } => PhysicalPlan::BgpScan {
            patterns: patterns.clone(),
        },
        Algebra::Join { left, right } => PhysicalPlan::Join {
            left: Box::new(reference_plan(left)),
            right: Box::new(reference_plan(right)),
        },
        Algebra::LeftJoin { left, right, expr } => PhysicalPlan::LeftJoin {
            left: Box::new(reference_plan(left)),
            right: Box::new(reference_plan(right)),
            expr: expr.clone(),
        },
        Algebra::Filter { expr, inner } => PhysicalPlan::Filter {
            expr: expr.clone(),
            inner: Box::new(reference_plan(inner)),
        },
        Algebra::Union { left, right } => PhysicalPlan::Union {
            left: Box::new(reference_plan(left)),
            right: Box::new(reference_plan(right)),
        },
        Algebra::Project { vars, inner } => PhysicalPlan::Project {
            vars: vars.clone(),
            inner: Box::new(reference_plan(inner)),
        },
        Algebra::Distinct { inner } => PhysicalPlan::Distinct {
            inner: Box::new(reference_plan(inner)),
        },
        Algebra::Slice {
            inner,
            start,
            length,
        } => PhysicalPlan::Slice {
            inner: Box::new(reference_plan(inner)),
            start: *start,
            length: *length,
        },
        Algebra::OrderBy { inner, keys } => PhysicalPlan::OrderBy {
            inner: Box::new(reference_plan(inner)),
            keys: keys.clone(),
        },
        Algebra::Extend { inner, var, expr } => PhysicalPlan::Extend {
            inner: Box::new(reference_plan(inner)),
            var: var.clone(),
            expr: expr.clone(),
        },
        Algebra::Values { vars, rows } => PhysicalPlan::Values {
            vars: vars.clone(),
            rows: rows.clone(),
        },
        Algebra::Group {
            inner,
            keys,
            aggregates,
        } => PhysicalPlan::Group {
            inner: Box::new(reference_plan(inner)),
            keys: keys.clone(),
            aggregates: aggregates.clone(),
        },
        Algebra::PathClosure {
            subject,
            object,
            edge,
            reflexive,
        } => PhysicalPlan::PathClosure {
            subject: subject.clone(),
            object: object.clone(),
            edge: Box::new(reference_plan(edge)),
            reflexive: *reflexive,
        },
    }
}

/// Representative query battery spanning every algebra operator.
const GOLDEN_QUERIES: &[&str] = &[
    "SELECT * WHERE { ?s <http://ex/name> ?n }",
    "SELECT ?s WHERE { ?s ?p ?o }",
    "SELECT ?s ?n WHERE { ?s <http://ex/knows> ?o . ?s <http://ex/name> ?n }",
    "SELECT ?s WHERE { ?s <http://ex/age> ?age FILTER(?age > \"20\") }",
    "SELECT ?n (COUNT(?s) AS ?c) WHERE { ?s <http://ex/name> ?n } GROUP BY ?n",
    "SELECT ?s WHERE { ?s <http://ex/name> ?n OPTIONAL { ?s <http://ex/age> ?age } }",
    "SELECT ?x WHERE { { ?x <http://ex/name> ?n } UNION { ?x <http://ex/age> ?age } }",
    "SELECT ?s WHERE { ?s <http://ex/age> ?age BIND(?age AS ?b) }",
    "SELECT DISTINCT ?n WHERE { ?s <http://ex/name> ?n } ORDER BY ?n LIMIT 2 OFFSET 1",
    "SELECT ?s ?n WHERE { ?s <http://ex/knows> ?o . { SELECT ?s ?n WHERE { ?s <http://ex/name> ?n } } }",
    "SELECT ?x ?y WHERE { ?x <http://ex/sco>+ ?y }",
];

#[test]
fn pipeline_reproduces_todays_physical_plans() {
    for q in GOLDEN_QUERIES {
        let alg = algebra_of(q);
        assert_eq!(
            planner::plan(&alg).expect("plan"),
            reference_plan(&alg),
            "logical pipeline changed the physical plan for:\n{q}"
        );
    }
}

fn pat(s: &str, p: &str, o: &str) -> TriplePattern {
    TriplePattern {
        subject: Term::Var(Var::new(s)),
        predicate: Term::Iri(p.to_owned()),
        object: Term::Var(Var::new(o)),
    }
}

/// Hand-built `Algebra::Join { Bgp, Bgp }` (spargebra never emits this, so it
/// is the only way to exercise coalescing end-to-end). The coalesced flat
/// `BgpScan{[p1,p2]}` and the nested `Join(BgpScan{[p1]},BgpScan{[p2]})` must
/// produce identical result multisets — the WCOJ executor runs the whole
/// pattern set as one natural join, same as the hash join over shared vars.
#[test]
fn coalesced_bgp_is_result_equivalent_to_nested_join() {
    let mut horn = HornBackend::new();
    let iri = |s: &str| Term::Iri(format!("http://ex/{s}"));
    horn.insert_triple(iri("a"), iri("p"), iri("b"));
    horn.insert_triple(iri("b"), iri("q"), iri("c"));
    horn.insert_triple(iri("a"), iri("p"), iri("x"));
    horn.insert_triple(iri("x"), iri("q"), iri("y"));

    let join_alg = Algebra::Join {
        left: Box::new(Algebra::Bgp {
            patterns: vec![pat("s", "http://ex/p", "o")],
        }),
        right: Box::new(Algebra::Bgp {
            patterns: vec![pat("o", "http://ex/q", "z")],
        }),
    };

    // Coalesced (CoalesceBgp on) vs nested (CoalesceBgp disabled).
    let coalesced = lower_physical(&run_passes(
        lower_algebra(&join_alg),
        &standard_passes(),
        &PlanCtx::default(),
    ));
    let nested = lower_physical(&run_passes(
        lower_algebra(&join_alg),
        &standard_passes(),
        &PlanCtx {
            disabled_passes: HashSet::from([PassId::CoalesceBgp]),
        },
    ));
    assert!(matches!(coalesced, PhysicalPlan::BgpScan { .. }));
    assert!(matches!(nested, PhysicalPlan::Join { .. }));

    let canon = |mut rows: Vec<Bindings>| -> Vec<String> {
        let mut v: Vec<String> = rows
            .drain(..)
            .map(|b| {
                b.vars()
                    .map(|(k, t)| format!("{k}={t:?}"))
                    .collect::<Vec<_>>()
                    .join("\u{1}")
            })
            .collect();
        v.sort();
        v
    };
    let a: Vec<Bindings> = Runtime::new(&horn).run(&coalesced).unwrap().collect();
    let b: Vec<Bindings> = Runtime::new(&horn).run(&nested).unwrap().collect();
    assert_eq!(canon(a), canon(b), "coalescing changed the result set");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p horndb-sparql -E 'binary(logical_pipeline)'`
Expected: `pipeline_reproduces_todays_physical_plans` — currently PASSES only by accident if `plan` were already rewired; before the rewrite it uses the *old* `plan` which equals `reference_plan`, so it PASSES trivially. To make it a genuine gate, do Step 3 first is wrong — instead confirm the *equivalence* test drives the rewrite: `coalesced_bgp_is_result_equivalent_to_nested_join` FAILS to compile until `run_passes`/`standard_passes`/`lower_*` are public (they are, from Tasks 3–4) — so this test compiles now and PASSES. The real behavioral guard is Step 4: after rewiring `plan`, the golden test must still pass. If it flips to FAIL, coalescing leaked into a real query and must be reconciled.

- [ ] **Step 3: Rewrite `plan` and add `plan_with_ctx`**

In `crates/sparql/src/plan/planner.rs`, replace the module doc + the `plan` function (lines 1-84) with:

```rust
//! Algebra → PhysicalPlan, via the logical IR + pass pipeline (SPEC-23 §5).
//!
//! Phase 1 wires `Algebra → LogicalPlan → run_passes → PhysicalPlan`. The
//! pipeline is deliberately behavior-preserving: the only registered pass,
//! `CoalesceBgp`, does not fire on spargebra-produced algebra (which already
//! merges adjacent triple patterns into one BGP), so the emitted plan is
//! structurally identical to the pre-refactor 1:1 lowering. Cost-based
//! ordering and the heuristic rewrite passes land in later phases behind the
//! same registry.

use crate::algebra::Algebra;
use crate::error::Result;
use crate::plan::lower::{lower_algebra, lower_physical};
use crate::plan::pass::{run_passes, standard_passes, PlanCtx};
use crate::plan::PhysicalPlan;

/// Plan `alg` with the default context (no passes disabled).
pub fn plan(alg: &Algebra) -> Result<PhysicalPlan> {
    plan_with_ctx(alg, &PlanCtx::default())
}

/// Plan `alg` under an explicit [`PlanCtx`] (e.g. with passes disabled by a
/// query pragma). Lowers to the logical IR, runs the pass pipeline, then
/// lowers to the physical plan.
pub fn plan_with_ctx(alg: &Algebra, ctx: &PlanCtx) -> Result<PhysicalPlan> {
    let logical = lower_algebra(alg);
    let optimized = run_passes(logical, &standard_passes(), ctx);
    Ok(lower_physical(&optimized))
}
```

Keep the existing `#[cfg(test)] mod tests` block (lines 86-123) unchanged — `empty_bgp_plans_to_empty_scan` and `join_lowers_both_sides` still hold under the pipeline (empty BGP → `BgpScan{[]}`; a `Join` over non-coalescible children stays a `Join`). The `err_path` witness stays as-is.

- [ ] **Step 4: Run the full suite**

Run: `cargo nextest run -p horndb-sparql`
Expected: PASS. Specifically:
- `tests/logical_pipeline.rs` — both tests PASS (golden identity holds; coalescing is result-invariant).
- `tests/planner_smoke.rs`, `tests/explain_pragma.rs`, `src/plan/pushdown.rs` inline tests, `src/plan/explain.rs` inline tests — all PASS unchanged (the physical plan they see is byte-identical).
- `src/plan/planner.rs` inline tests — PASS.

Run: `cargo nextest run -p horndb-sparql --features server`
Expected: PASS (server tests exercise the same `plan` path).

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/sparql/src/plan/planner.rs crates/sparql/tests/logical_pipeline.rs
git commit -m 'refactor(sparql): route planner::plan through the logical pass pipeline (#185)'
```

---

### Task 6: query pragma to disable passes (`PRAGMA disable-pass=<id>`)

The SPEC-23 §7.2 legibility criterion requires each pass be disable-able via a **query pragma**, so a plan regression bisects to one `PassId` from the query surface (not just config). Mirror the EXPLAIN pragma mechanism (`parser.rs:120` strips a leading keyword before spargebra sees the text): strip zero or more leading `PRAGMA disable-pass=<id>` directives into a `HashSet<PassId>`, build a `PlanCtx`, and thread it through `execute_query_with`'s planning calls.

**Files:**
- Modify: `crates/sparql/src/parser.rs` (add `strip_plan_pragmas`, after `strip_explain_pragma`, ~line 130)
- Modify: `crates/sparql/src/api.rs:75-159` (strip pragmas in `execute_query_with`; thread `PlanCtx` into the SELECT + EXPLAIN planning; `plan_of` gains a `ctx` param)
- Test: `crates/sparql/tests/logical_pipeline.rs` (append)

- [ ] **Step 1: Write the failing tests**

Append to `crates/sparql/tests/logical_pipeline.rs`:

```rust
mod pragma {
    use horndb_sparql::api::{execute_query, QueryAnswer};
    use horndb_sparql::exec::mem::MemStore;
    use horndb_sparql::exec::Store;
    use horndb_sparql::parser::strip_plan_pragmas;
    use horndb_sparql::plan::pass::PassId;

    #[test]
    fn strips_one_disable_pass_pragma() {
        let (rest, disabled) =
            strip_plan_pragmas("PRAGMA disable-pass=coalesce-bgp SELECT * WHERE { ?s ?p ?o }")
                .expect("pragma parses");
        assert!(rest.trim_start().starts_with("SELECT"));
        assert!(disabled.contains(&PassId::CoalesceBgp));
    }

    #[test]
    fn strips_multiple_pragmas() {
        let (rest, disabled) = strip_plan_pragmas(
            "PRAGMA disable-pass=coalesce-bgp PRAGMA disable-pass=join-planning ASK { ?s ?p ?o }",
        )
        .expect("pragmas parse");
        assert!(rest.trim_start().starts_with("ASK"));
        assert!(disabled.contains(&PassId::CoalesceBgp));
        assert!(disabled.contains(&PassId::JoinPlanning));
    }

    #[test]
    fn no_pragma_is_identity() {
        let (rest, disabled) =
            strip_plan_pragmas("SELECT * WHERE { ?s ?p ?o }").expect("no pragma");
        assert_eq!(rest, "SELECT * WHERE { ?s ?p ?o }");
        assert!(disabled.is_empty());
    }

    #[test]
    fn unknown_pass_id_is_an_error() {
        assert!(strip_plan_pragmas("PRAGMA disable-pass=nope SELECT * WHERE { ?s ?p ?o }").is_err());
    }

    /// End-to-end: a pragma-carrying query still runs and returns results
    /// (the pragma is consumed, not passed to spargebra).
    #[test]
    fn pragma_query_executes_and_returns_results() {
        let mut s = MemStore::default();
        s.insert(("a".into(), "p".into(), "b".into()));
        let ans = execute_query(
            "PRAGMA disable-pass=coalesce-bgp SELECT * WHERE { ?s ?p ?o }",
            &s,
        )
        .expect("pragma query runs");
        match ans {
            QueryAnswer::Solutions { rows, .. } => assert_eq!(rows.len(), 1),
            other => panic!("expected Solutions, got {other:?}"),
        }
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p horndb-sparql -E 'binary(logical_pipeline) and test(pragma)'`
Expected: build FAILURE — `unresolved import horndb_sparql::parser::strip_plan_pragmas`.

- [ ] **Step 3: Implement `strip_plan_pragmas`**

In `crates/sparql/src/parser.rs`, add (after `strip_explain_pragma`, ~line 130) and export the `PassId` dependency:

```rust
use crate::plan::pass::PassId;
use std::collections::HashSet;

/// Strip any leading `PRAGMA disable-pass=<id>` directives (SPEC-23 §7.2),
/// returning the remaining query text and the set of disabled passes.
///
/// Like [`strip_explain_pragma`], pragmas must lead the request (before any
/// PREFIX/BASE prologue and before `EXPLAIN`) so spargebra never sees them.
/// `<id>` is a [`PassId`] kebab name (e.g. `coalesce-bgp`); an unknown id is a
/// parse error. Matching is case-insensitive on the `PRAGMA` keyword.
pub fn strip_plan_pragmas(input: &str) -> Result<(String, HashSet<PassId>)> {
    let mut rest = input;
    let mut disabled = HashSet::new();
    loop {
        let trimmed = rest.trim_start();
        let Some(after_pragma) = strip_keyword_ci(trimmed, "PRAGMA") else {
            break;
        };
        // Expect `disable-pass=<id>` as the next whitespace-delimited token.
        let token = after_pragma.trim_start();
        let end = token
            .find(|c: char| c.is_ascii_whitespace())
            .unwrap_or(token.len());
        let (directive, tail) = token.split_at(end);
        let id_str = directive.strip_prefix("disable-pass=").ok_or_else(|| {
            SparqlError::Parse(format!("unrecognized PRAGMA directive `{directive}`"))
        })?;
        let id = id_str
            .parse::<PassId>()
            .map_err(SparqlError::Parse)?;
        disabled.insert(id);
        rest = tail;
    }
    Ok((rest.to_owned(), disabled))
}
```

- [ ] **Step 4: Thread the `PlanCtx` through `execute_query_with`**

In `crates/sparql/src/api.rs`, update `execute_query_with` (line 75) to strip pragmas first and build a `PlanCtx`, then use `plan_with_ctx` in the SELECT and EXPLAIN arms. Concretely:

Add the import near the top (after line 12, `use crate::plan::planner;`):

```rust
use crate::plan::pass::PlanCtx;
use crate::parser::strip_plan_pragmas;
```

At the top of `execute_query_with`'s body (replace line 80's single `parse_query` call):

```rust
    let (query, ctx) = {
        let (body, disabled) = strip_plan_pragmas(query)?;
        (body, PlanCtx { disabled_passes: disabled })
    };
    let parsed = timed(Stage::Parse, || parse_query(&query))?;
```

In the `ParsedQuery::Select` arm (line 92), swap the plan call:

```rust
            let plan = timed(Stage::Plan, || planner::plan_with_ctx(&alg, &ctx))?;
```

Do the same in the `Ask`, `Construct`, and `Describe` arms (lines 100, 112, 135) — each `planner::plan(&alg)` becomes `planner::plan_with_ctx(&alg, &ctx)`. In the `Explain` arm (line 149), pass `ctx` into `plan_of`:

```rust
            let plan = timed(Stage::Plan, || plan_of(&inner, cfg, &ctx))?;
```

And update `plan_of`'s signature (line 194) to take and forward the context:

```rust
fn plan_of(parsed: &ParsedQuery, cfg: &SparqlConfig, ctx: &PlanCtx) -> Result<crate::plan::PhysicalPlan> {
    // …unchanged body until the final two lines…
    let alg = translate_query_with(inner, cfg)?;
    planner::plan_with_ctx(&alg, ctx)
}
```

Leave `plan_select` (line 170) unchanged in Phase 1 — the streaming SELECT path does not yet accept pragmas (a small, self-contained follow-up; the `execute_query` path fully exercises the mechanism). Note this boundary in `INTEGRATION-NOTES.md` if you touch it, but do not expand scope here.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo nextest run -p horndb-sparql -E 'binary(logical_pipeline) and test(pragma)'`
Expected: PASS (5 tests).

Run: `cargo nextest run -p horndb-sparql` and `cargo nextest run -p horndb-sparql --features server`
Expected: PASS — existing queries (no pragma) are unaffected; `strip_plan_pragmas` on pragma-free input returns the text verbatim with an empty disabled set.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
git add crates/sparql/src/parser.rs crates/sparql/src/api.rs crates/sparql/tests/logical_pipeline.rs
git commit -m 'feat(sparql): PRAGMA disable-pass=<id> query pragma for pass bisection (#185)'
```

---

### Task 7: no-regression harness + fuzzer sweep

The Phase-1 gate (SPEC-23 §7.1) requires the conformance subset and the WCOJ differential fuzzer to stay green. No code change — this is the verification step that must run before the branch is considered done.

**Files:** none (verification only).

- [ ] **Step 1: Conformance subset**

Run: `cargo nextest run --workspace` (includes `harness`; pulls `oxrocksdb-sys` on a cold cache — several minutes first time).
Expected: PASS. The SPARQL conformance run over `harness/selected.toml` must be green — the pipeline is result-invariant, so no conformance case changes.

- [ ] **Step 2: WCOJ differential fuzzer**

Run: `cargo nextest run -p horndb-wcoj -E 'binary(differential_fuzz)'`
Expected: PASS (proptest, 256 cases, WCOJ vs BinaryHash row-set equality). Phase 1 does not touch `horndb-wcoj`, so this is a guard against an accidental cross-crate change, not a behavior gate.

- [ ] **Step 3: Lint + full build (pre-push parity)**

Run: `cargo clippy --workspace --all-targets -- -D warnings` and `cargo build --workspace`.
Expected: clean. In particular, the new `#[cfg(debug_assertions)]` `validate` path must not warn under a release build (no unused imports leak) — if clippy flags an unused `referenced_vars` import in a release configuration, gate the import behind `#[cfg(debug_assertions)]` too.

- [ ] **Step 4: No commit**

This task produces no diff. If any check fails, the failure is a real regression to fix in the owning task — do not paper over it here.

---

## Self-Review

**Every Phase-1 SPEC-23 requirement maps to a task:**

- **§5.1 Plan IR — flat n-ary `Bgp`, resolved once.** `LogicalPlan` with `Bgp { patterns }` as the join unit — Task 1. Binding/type lattice (`TypeMask`/`VarTypes`/`infer`, `Join` intersects / `LeftJoin` marks RHS undef / `Union` unions-with-undef) — Task 2. Smart constructors (`join` coalesces adjacent BGPs, `filter` drops constant-true, `union`) — Task 1. Coalesce-contiguous-`Join(Bgp,Bgp)` realized as the first pass — Task 4 (`CoalesceBgp`), applied on entry to the pipeline in Task 5.
- **§5.2 Pass registry.** `PassId` enum (all six names), `LogicalPass` trait with `must_follow`, `PlanCtx`, `run_passes` driver, declared-ordering startup assertion (`assert_pass_order`), debug-build post-pass `infer`+`validate` — Task 4. Individually toggleable via `disabled_passes` — Task 4 (config) + Task 6 (pragma).
- **§6.1 Framework scaffolding, no behavior change; port existing heuristic rewrites; golden-plan tests.** Pipeline wired into `planner::plan` (`Algebra → LogicalPlan → run_passes → PhysicalPlan`) with the `PhysicalPlan`-level `plan/pushdown.rs` left untouched — Task 5. Golden-plan structural equality against a frozen `reference_plan` over an 11-query battery — Task 5. (The "port existing heuristic rewrites" line: in Phase 1 the only rewrite ported is `CoalesceBgp`; `plan/pushdown.rs` stays a runtime `PhysicalPlan` rewrite by design — SPEC-23 §5.2 explicitly notes projection-pushdown "overlaps existing `plan/pushdown.rs`" and defers the port to Phase 2, so keeping it unchanged is correct for Phase 1's no-behavior-change contract.)
- **§7.1 No-regression baseline.** Conformance subset (`harness/selected.toml`) + WCOJ differential fuzzer green; structural golden-plan tests exist — Task 5 (golden) + Task 7 (harness/fuzzer sweep). Coalescing proven result-invariant against the nested join — Task 5 (`coalesced_bgp_is_result_equivalent_to_nested_join`, verified against `HornBackend` execution, grounded in the read of `scan_bgp_ids`/`JoinOp` both computing the natural join over shared variables).
- **§7.2 Pass legibility.** Every pass disable-able via `PlanCtx.disabled_passes` (config) and `PRAGMA disable-pass=<id>` (pragma) — Task 4 + Task 6. Driver asserts declared ordering at startup — Task 4 (`assert_pass_order`, tested for both accept and panic). Debug builds validate the IR after each pass — Task 4 (`validate`). A regression bisects to one `PassId` — the disable mechanism + `coalesced_bgp_is_result_equivalent_to_nested_join`'s enable/disable A/B demonstrate it.

**Type names match the pinned contract verbatim:** modules `logical.rs` / `types.rs` / `pass.rs` / `lower.rs`; `LogicalPlan` (13 variants exactly as pinned); `LogicalPlan::{join, filter, union}`; `TypeMask(u8)` with `UNDEF`/`NAMED_NODE`/`BLANK_NODE`/`LITERAL`/`TRIPLE` consts and `is_bound`; `VarTypes(HashMap<Var, TypeMask>)`; `infer(&LogicalPlan) -> VarTypes`; `PassId { CoalesceBgp, Normalize, FilterPullup, FilterPushdown, ProjectionPushdown, JoinPlanning }`; `PlanCtx { disabled_passes }`; `LogicalPass { id, run, must_follow }`; `run_passes(LogicalPlan, &[Box<dyn LogicalPass>], &PlanCtx) -> LogicalPlan`. `lower.rs` exposes `lower_algebra` (`Algebra → LogicalPlan`, with coalescing done by the `CoalesceBgp` pass rather than in the lowering itself — a deliberate choice, permitted by the task's "during lowering AND/OR as the first pass", that keeps the transformation bisectable and the disable observable).

**No placeholders.** Every implementation step shows complete, compilable-looking Rust; every test step shows the exact `cargo nextest` command and the expected FAIL/PASS outcome. Where a fact was load-bearing (spargebra pre-merges BGPs so `CoalesceBgp` is a no-op on the corpus; `scan_bgp_ids` runs the whole pattern set as one natural join; `referenced_vars` is `pub(crate)` and reusable) it was verified by reading the source, not assumed.

**One flagged risk for the executing agent:** `run_passes` calls `standard_passes()` which calls `assert_pass_order` on every plan — cheap (one small `Vec` alloc + O(passes²) check) but on a hot planning path. If planning latency regresses (watch `stage_duration_seconds{stage=plan}`), hoist `standard_passes()` into a `OnceLock` in `planner.rs`. Not needed for Phase-1 correctness; noted so it is a conscious choice, not a surprise.
