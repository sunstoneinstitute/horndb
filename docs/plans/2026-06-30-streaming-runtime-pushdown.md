# Streaming SPARQL runtime + projection/aggregate pushdown — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the SPARQL runtime's fully-materializing `eval` with a batch-at-a-time pull operator tree (#143) and add two heuristic-safe planner rewrites — column pruning and COUNT-over-BGP aggregate pushdown (#144) — to close the SPB aggregation-qps gap (epic #128).

**Architecture:** Each physical operator becomes a pull-based `Op` yielding `Batch` chunks of `BATCH_ROWS` rows via `next() -> Result<Option<Batch>>`. Operators borrow `&Runtime` so they reuse the existing slot helpers (`normalize_columns`, `eval_group_native`, `decode_subset`, `merge_all`, `compare_by_keys`, …) unchanged; each arm's post-child computation is extracted once into a `compute_*` method that both the streaming and blocking paths call. A `MaterializedOp` adapter (wraps the legacy `eval` for one subtree) keeps the suite green while operators are converted one at a time. Pushdown runs as a `PhysicalPlan -> PhysicalPlan` pass before operators are built.

**Tech Stack:** Rust 1.90, `horndb-sparql` crate, `cargo nextest`, criterion (`agg_profile`).

**Reference design:** `docs/specs/2026-06-30-streaming-runtime-pushdown-design.md`.

**Op trait shape (decided in Task 1):** the `Op` trait is **lifetime-free** —
`pub trait Op { fn schema(&self) -> &[Var]; fn next(&mut self) -> Result<Option<Batch>>; }`.
Operators that borrow the runtime carry their own lifetime on the struct and impl
as `impl<'r, E: Executor + ?Sized> Op for FooOp<'r, E>`; `build<'r>(&'r self, …)`
boxes them as `Box<dyn Op + 'r>`. (Code blocks below that still show `Op<'a>`/`Op<'r>`
on the trait predate this decision — use the lifetime-free trait form.)

**Conventions for every task:**
- Run tests with `cargo nextest run -p horndb-sparql` (and `--features server` where noted). `cargo nextest` does not run doctests; this crate has none.
- Keep `cargo clippy -p horndb-sparql --all-targets -- -D warnings` and `cargo fmt --all -- --check` clean before each commit (pre-push runs clippy workspace-wide).
- The `slot_differential` proptest module lives at `crates/sparql/src/exec/runtime.rs:1964-2328`; it plus the Task 0 test are the "no result change" gate — they must stay green at every step.
- Commit messages: never add Co-authored-by trailers. Use a HEREDOC for multi-line bodies.

---

## File Structure

| Path | Responsibility | Tasks |
|---|---|---|
| `crates/sparql/src/exec/runtime.rs` | Extract `compute_*` arm helpers; host `build`; rewire `run`; delete legacy `eval` at the end | 1, 9–12 |
| `crates/sparql/src/exec/op/mod.rs` | `Op` trait, `BATCH_ROWS`, `MaterializedOp`, `Runtime::build` dispatch | 1 |
| `crates/sparql/src/exec/op/source.rs` | `ScanOp`, `ValuesOp`, `CountScanOp` | 2, 7, 15 |
| `crates/sparql/src/exec/op/stream.rs` | `FilterOp`, `ProjectOp`, `ExtendOp`, `SliceOp`, `DistinctOp` | 3–6, 8 |
| `crates/sparql/src/exec/op/blocking.rs` | `UnionOp`, `JoinOp`, `LeftJoinOp`, `GroupOp`, `OrderByOp`, `PathClosureOp` | 9–11 |
| `crates/sparql/src/exec/mod.rs` | Add `op` module; add `count_bgp` to `Executor` trait | 1, 15 |
| `crates/sparql/src/exec/horn.rs` | Implement `HornBackend::count_bgp` via WCOJ cardinality | 15 |
| `crates/sparql/src/plan/pushdown.rs` | `rewrite`, `needed_vars` (column pruning), aggregate pushdown to `CountScan` | 14, 15 |
| `crates/sparql/src/plan/mod.rs` | Add `pushdown` module; add `PhysicalPlan::CountScan` variant | 14, 15 |
| `crates/sparql/src/plan/explain.rs` | Render the new `CountScan` node | 15 |
| `docs/benchmarks.md`, `TASKS.md`, `docs/architecture.md` | Doc sync | 16 |

---

## Task 0: Deterministic GROUP BY + COUNT(DISTINCT *) test (#145, gate)

Lands on the **current** materialized runtime, before any refactor, pinning the
id-based distinct-key path (`KeyPart` over slot rows) directly.

**Files:**
- Test: `crates/sparql/src/exec/runtime.rs` (extend the `slot_differential` test module near line 2285, before the closing `}` of `mod slot_differential`).

- [ ] **Step 1: Write the failing test**

Add inside `mod slot_differential` (the helpers `HornBackend`, `plan_select` are already in scope there — confirm by reading lines 1964-1990):

```rust
#[test]
fn group_by_count_distinct_star_is_deterministic() {
    // Two categories; cat A has 3 distinct entities (one repeated via a
    // second predicate), cat B has 2. COUNT(DISTINCT *) over the grouped
    // rows must count distinct *solution mappings*, not raw rows.
    let horn = HornBackend::new();
    for (s, p, o) in [
        ("e1", "cat", "A"), ("e1", "kind", "x"),
        ("e2", "cat", "A"), ("e2", "kind", "x"),
        ("e3", "cat", "A"), ("e3", "kind", "y"),
        ("e1", "cat", "A"), // duplicate triple — must not double-count
        ("e4", "cat", "B"), ("e4", "kind", "x"),
        ("e5", "cat", "B"), ("e5", "kind", "y"),
    ] {
        horn.insert(s, p, o);
    }
    let plan = plan_select(
        "SELECT ?c (COUNT(DISTINCT *) AS ?n) WHERE { \
         ?e <cat> ?c . ?e <kind> ?k } GROUP BY ?c ORDER BY ?c",
    );
    let rows: Vec<_> = Runtime::new(&horn).run(&plan).unwrap().collect();
    let got: Vec<(String, String)> = rows
        .iter()
        .map(|b| {
            (
                b.get("c").unwrap().to_string(),
                b.get("n").unwrap().to_string(),
            )
        })
        .collect();
    // cat A: distinct (?e,?k) mappings = {(e1,x),(e2,x),(e3,y)} = 3
    // cat B: {(e4,x),(e5,y)} = 2
    assert_eq!(
        got,
        vec![
            ("A".to_string(), "\"3\"^^<http://www.w3.org/2001/XMLSchema#integer>".to_string()),
            ("B".to_string(), "\"2\"^^<http://www.w3.org/2001/XMLSchema#integer>".to_string()),
        ]
    );
}
```

- [ ] **Step 2: Run the test**

Run: `cargo nextest run -p horndb-sparql group_by_count_distinct_star_is_deterministic`
Expected: PASS if behavior is already correct (this is a characterization/regression test pinning current behavior). If it FAILS, the assertion encodes the *correct* SPARQL semantics — inspect the actual output, confirm the expected values above match the integer-literal lexical form `HornBackend` produces (check how an existing aggregate test in the module formats `COUNT`), adjust only the literal *formatting* in the expectation to match the engine's canonical form, never the counts (3 and 2).

- [ ] **Step 3: Commit**

```bash
git add crates/sparql/src/exec/runtime.rs
git commit -m "test(sparql): pin GROUP BY + COUNT(DISTINCT *) semantics (#145)"
```

---

## Task 1: `Op` trait, `build` scaffold, `MaterializedOp` adapter

Introduce the operator seam with zero behavior change: `run` builds a
`MaterializedOp` over the whole plan, which calls the legacy `eval`.

**Files:**
- Create: `crates/sparql/src/exec/op/mod.rs`
- Modify: `crates/sparql/src/exec/mod.rs` (add `pub mod op;` after `pub mod mem;`)
- Modify: `crates/sparql/src/exec/runtime.rs:26-30` (rewire `run`)

- [ ] **Step 1: Add the module declaration**

In `crates/sparql/src/exec/mod.rs`, after the existing `pub mod runtime;` line:

```rust
pub mod op;
```

- [ ] **Step 2: Write the `Op` trait + `MaterializedOp` + `build`**

Create `crates/sparql/src/exec/op/mod.rs`:

```rust
//! Pull-based physical operators (#143). Each `Op` yields `Batch` chunks of
//! at most `BATCH_ROWS` rows, all sharing `schema()`. `next` returns `None`
//! at end of stream and never yields a `Some(empty)` chunk mid-stream.

use crate::algebra::Var;
use crate::error::Result;
use crate::exec::{Batch, Executor, Row};
use crate::plan::PhysicalPlan;

/// Target rows per emitted chunk. Small in tests (see chunk-boundary tests)
/// to exercise multi-chunk operator state.
pub const BATCH_ROWS: usize = 4096;

/// A pull-based physical operator.
pub trait Op<'a> {
    /// The fixed output schema shared by every `Batch` this op yields.
    fn schema(&self) -> &[Var];
    /// Next chunk of rows, or `None` at end of stream.
    fn next(&mut self) -> Result<Option<Batch>>;
}

/// Adapter that wraps a not-yet-converted subtree: evaluates it once via the
/// legacy `Runtime::eval`, then hands the rows out in `BATCH_ROWS` chunks.
/// Deleted in Task 12 once every variant has a native `Op`.
pub struct MaterializedOp {
    schema: Vec<Var>,
    rows: std::vec::IntoIter<Row>,
}

impl MaterializedOp {
    pub fn new(batch: Batch) -> Self {
        Self {
            schema: batch.schema,
            rows: batch.rows.into_iter(),
        }
    }
}

impl<'a> Op<'a> for MaterializedOp {
    fn schema(&self) -> &[Var] {
        &self.schema
    }
    fn next(&mut self) -> Result<Option<Batch>> {
        let chunk: Vec<Row> = self.rows.by_ref().take(BATCH_ROWS).collect();
        if chunk.is_empty() {
            Ok(None)
        } else {
            Ok(Some(Batch {
                schema: self.schema.clone(),
                rows: chunk,
            }))
        }
    }
}

impl<'a, E: Executor + ?Sized> crate::exec::runtime::Runtime<'a, E> {
    /// Build the pull-based operator tree for `plan`. During conversion,
    /// unconverted variants fall through to a `MaterializedOp` wrapping the
    /// legacy `eval` of that subtree.
    pub(crate) fn build(&'a self, plan: &PhysicalPlan) -> Result<Box<dyn Op<'a> + 'a>> {
        // Converted variants are added here task-by-task. Until then, every
        // node is materialized via the legacy path.
        Ok(Box::new(MaterializedOp::new(self.eval(plan)?)))
    }
}
```

- [ ] **Step 3: Make `eval` and `Runtime` reachable from the `op` module**

In `crates/sparql/src/exec/runtime.rs`, change `fn eval` (line 34) visibility to `pub(crate) fn eval`, and confirm `pub struct Runtime` (line 16) is already crate-visible (it is `pub`). No other change.

- [ ] **Step 4: Rewire `run` through `build`**

Replace `crates/sparql/src/exec/runtime.rs:26-30` with:

```rust
    /// Execute the plan and return all solution mappings.
    pub fn run(&self, plan: &PhysicalPlan) -> Result<std::vec::IntoIter<Bindings>> {
        let mut op = self.build(plan)?;
        let mut out = Vec::new();
        while let Some(batch) = op.next()? {
            out.extend(batch.to_bindings(|id| self.exec.decode_term(id))?);
        }
        Ok(out.into_iter())
    }
```

**Lifetime note (read before fighting the borrow checker).** Operators borrow
the runtime to reach its helpers, so they need a borrow lifetime that does **not**
have to equal `exec`'s lifetime `'a`. Prefer a distinct lifetime on `build` and on
each operator:

```rust
// op/mod.rs
pub trait Op<'r> { fn schema(&self) -> &[Var]; fn next(&mut self) -> Result<Option<Batch>>; }

impl<'a, E: Executor + ?Sized> Runtime<'a, E> {
    pub(crate) fn build<'r>(&'r self, plan: &PhysicalPlan) -> Result<Box<dyn Op<'r> + 'r>>
    where E: 'r { /* ... */ }
}
// FilterOp<'r, E> holds rt: &'r Runtime<'a, E> — only 'r appears in the Op<'r> impl.
```

`run` is `&self`; the returned `Box<dyn Op<'r> + 'r>` is consumed within `run`,
so `'r` is just the body borrow and the call sites (`Runtime::new(&x).run(&p)`)
compile unchanged. Use a single `'a` only if the two-lifetime form proves
unnecessary — start with the two-lifetime form to avoid over-constraining.

- [ ] **Step 5: Run the full suite**

Run: `cargo nextest run -p horndb-sparql && cargo nextest run -p horndb-sparql --features server`
Expected: PASS — behavior is identical (every node still goes through `eval`).

- [ ] **Step 6: Commit**

```bash
git add crates/sparql/src/exec/op/mod.rs crates/sparql/src/exec/mod.rs crates/sparql/src/exec/runtime.rs
git commit -m "feat(sparql): introduce pull-based Op seam + MaterializedOp adapter (#143)"
```

---

## Task 2: `ScanOp` — chunk the BGP leaf

**Files:**
- Create: `crates/sparql/src/exec/op/source.rs`
- Modify: `crates/sparql/src/exec/op/mod.rs` (declare `mod source;`, dispatch `BgpScan`)

- [ ] **Step 1: Write a chunk-boundary test**

Add to `crates/sparql/src/exec/op/source.rs` under `#[cfg(test)] mod tests` (model `HornBackend` setup on the `slot_differential` tests in runtime.rs):

```rust
#[cfg(test)]
mod tests {
    use crate::exec::op::Op;
    use crate::exec::runtime::Runtime;
    use crate::exec::horn::HornBackend; // confirm this path via runtime.rs imports
    use crate::plan::PhysicalPlan;
    use crate::algebra::{Term, TriplePattern, Var};

    #[test]
    fn scan_emits_all_rows_across_chunks() {
        let horn = HornBackend::new();
        for i in 0..10 {
            horn.insert(&format!("e{i}"), "p", "o");
        }
        let plan = PhysicalPlan::BgpScan {
            patterns: vec![TriplePattern {
                subject: Term::Var(Var::new("s")),
                predicate: Term::Iri("p".into()),
                object: Term::Var(Var::new("o")),
            }],
        };
        let rt = Runtime::new(&horn);
        let mut op = rt.build(&plan).unwrap();
        let mut total = 0;
        while let Some(b) = op.next().unwrap() {
            assert!(!b.rows.is_empty(), "no empty chunks mid-stream");
            total += b.rows.len();
        }
        assert_eq!(total, 10);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo nextest run -p horndb-sparql scan_emits_all_rows_across_chunks`
Expected: PASS via `MaterializedOp` (10 rows in one chunk). This test guards the
contract; it stays green when `ScanOp` replaces the adapter. To prove the
multi-chunk path, the chunk-boundary suite in Task 13 lowers `BATCH_ROWS`.

- [ ] **Step 3: Implement `ScanOp`**

Create `crates/sparql/src/exec/op/source.rs` (above the test module):

```rust
//! Source operators: leaves with no child input.

use super::{Op, BATCH_ROWS};
use crate::algebra::Var;
use crate::error::Result;
use crate::exec::{Batch, Row};

/// Scans a BGP once via the executor, then hands the rows out in chunks.
/// The scan seam is unchanged (`scan_bgp_ids` returns a whole `Batch`); this
/// op only re-chunks it so parents pull incrementally.
pub struct ScanOp {
    schema: Vec<Var>,
    rows: std::vec::IntoIter<Row>,
}

impl ScanOp {
    pub fn new(batch: Batch) -> Self {
        Self {
            schema: batch.schema,
            rows: batch.rows.into_iter(),
        }
    }
}

impl<'a> Op<'a> for ScanOp {
    fn schema(&self) -> &[Var] {
        &self.schema
    }
    fn next(&mut self) -> Result<Option<Batch>> {
        let chunk: Vec<Row> = self.rows.by_ref().take(BATCH_ROWS).collect();
        if chunk.is_empty() {
            Ok(None)
        } else {
            Ok(Some(Batch {
                schema: self.schema.clone(),
                rows: chunk,
            }))
        }
    }
}
```

- [ ] **Step 4: Dispatch `BgpScan` in `build`**

In `crates/sparql/src/exec/op/mod.rs`: add `mod source;` near the top and
`use source::ScanOp;`. Change `build` to match before the fallback:

```rust
    pub(crate) fn build(&'a self, plan: &PhysicalPlan) -> Result<Box<dyn Op<'a> + 'a>> {
        match plan {
            PhysicalPlan::BgpScan { patterns } => {
                Ok(Box::new(ScanOp::new(self.exec().scan_bgp_ids(patterns)?)))
            }
            _ => Ok(Box::new(MaterializedOp::new(self.eval(plan)?))),
        }
    }
```

`self.exec()` does not exist yet — add a crate-visible accessor to `Runtime` in
`runtime.rs` after `new`:

```rust
    pub(crate) fn exec(&self) -> &'a E {
        self.exec
    }
```

- [ ] **Step 5: Run the suite**

Run: `cargo nextest run -p horndb-sparql`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/sparql/src/exec/op/source.rs crates/sparql/src/exec/op/mod.rs crates/sparql/src/exec/runtime.rs
git commit -m "feat(sparql): native ScanOp for BGP leaf (#143)"
```

---

## Task 3: Extract per-chunk transforms + `FilterOp`

`FilterOp` is the template for all streaming operators. First extract the
`Filter` arm body into a reusable per-batch method, then build the op on top.

**Files:**
- Modify: `crates/sparql/src/exec/runtime.rs` (extract `apply_filter`)
- Create/Modify: `crates/sparql/src/exec/op/stream.rs`
- Modify: `crates/sparql/src/exec/op/mod.rs` (declare `mod stream;`, dispatch `Filter`)

- [ ] **Step 1: Extract `apply_filter` from the `Filter` arm**

Read `crates/sparql/src/exec/runtime.rs:209-224` (the `Filter` arm). It evaluates
the child then keeps rows where the predicate holds, decoding the referenced
vars via `decode_subset` + `eval_expr`. Add a method on `Runtime` that does the
per-batch part (no child eval):

```rust
    /// Keep the rows of `batch` for which `expr` evaluates true. Decodes only
    /// the referenced columns (`decode_subset`), preserving `Slot::Id` for the
    /// rest. Mirrors the legacy `Filter` arm.
    pub(crate) fn apply_filter(&self, batch: Batch, expr: &Expr) -> Result<Batch> {
        let mut want = HashSet::new();
        referenced_vars(expr, &mut want);
        let mut kept = Vec::with_capacity(batch.rows.len());
        for row in batch.rows {
            let b = self.decode_subset(&row, &batch.schema, &want)?;
            if eval_expr(expr, &b)? {
                kept.push(row);
            }
        }
        Ok(Batch {
            schema: batch.schema,
            rows: kept,
        })
    }
```

Then replace the body of the `Filter` arm (lines 209-224) with:

```rust
            PhysicalPlan::Filter { expr, inner } => {
                let child = self.eval(inner)?;
                self.apply_filter(child, expr)
            }
```

(If the existing arm references vars differently, keep its exact predicate
logic — only the structure changes: child eval, then `apply_filter`.)

- [ ] **Step 2: Run the suite to confirm the extraction is behavior-preserving**

Run: `cargo nextest run -p horndb-sparql`
Expected: PASS (pure refactor).

- [ ] **Step 3: Write `FilterOp`**

Create `crates/sparql/src/exec/op/stream.rs`:

```rust
//! Streaming operators: one child, per-chunk transform, no buffering.

use super::Op;
use crate::algebra::{Expr, Var};
use crate::error::Result;
use crate::exec::runtime::Runtime;
use crate::exec::{Batch, Executor};

/// Streams its child, keeping rows that satisfy `expr`. Loops internally so it
/// never yields an empty chunk (a chunk fully filtered out pulls the next one).
pub struct FilterOp<'a, E: Executor + ?Sized> {
    rt: &'a Runtime<'a, E>,
    child: Box<dyn Op<'a> + 'a>,
    expr: Expr,
    schema: Vec<Var>,
}

impl<'a, E: Executor + ?Sized> FilterOp<'a, E> {
    pub fn new(rt: &'a Runtime<'a, E>, child: Box<dyn Op<'a> + 'a>, expr: Expr) -> Self {
        let schema = child.schema().to_vec();
        Self { rt, child, expr, schema }
    }
}

impl<'a, E: Executor + ?Sized> Op<'a> for FilterOp<'a, E> {
    fn schema(&self) -> &[Var] {
        &self.schema
    }
    fn next(&mut self) -> Result<Option<Batch>> {
        while let Some(chunk) = self.child.next()? {
            let kept = self.rt.apply_filter(chunk, &self.expr)?;
            if !kept.rows.is_empty() {
                return Ok(Some(kept));
            }
        }
        Ok(None)
    }
}
```

- [ ] **Step 4: Dispatch `Filter` in `build`**

In `op/mod.rs`: add `mod stream;` and `use stream::FilterOp;`. Add the arm:

```rust
            PhysicalPlan::Filter { expr, inner } => {
                let child = self.build(inner)?;
                Ok(Box::new(FilterOp::new(self, child, expr.clone())))
            }
```

- [ ] **Step 5: Run the suite**

Run: `cargo nextest run -p horndb-sparql`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/sparql/src/exec/runtime.rs crates/sparql/src/exec/op/stream.rs crates/sparql/src/exec/op/mod.rs
git commit -m "feat(sparql): streaming FilterOp + extract apply_filter (#143)"
```

---

## Task 4: `ProjectOp`

**Files:**
- Modify: `crates/sparql/src/exec/runtime.rs` (extract `apply_project`)
- Modify: `crates/sparql/src/exec/op/stream.rs` (add `ProjectOp`)
- Modify: `crates/sparql/src/exec/op/mod.rs` (dispatch `Project`)

- [ ] **Step 1: Extract `apply_project`**

Read `crates/sparql/src/exec/runtime.rs:262-288` (the `Project` arm: remaps slots
to the projected vars; if `vars.is_empty()` returns the child unchanged). Add:

```rust
    /// Restrict `batch` to `vars` (in projection order), remapping each row's
    /// slots. Empty `vars` (SELECT * / ASK) returns the batch unchanged.
    /// Mirrors the legacy `Project` arm.
    pub(crate) fn apply_project(&self, batch: Batch, vars: &[Var]) -> Result<Batch> {
        // Relocate the slot-remap logic from runtime.rs:262-288 here verbatim,
        // operating on `batch.schema`/`batch.rows` instead of the evaluated
        // child. Output schema = `vars.to_vec()` (or `batch.schema` if empty).
        // ... (exact body moved from the arm) ...
        todo!("move Project arm body here")
    }
```

Replace the `Project` arm (262-288) with `let c = self.eval(inner)?; self.apply_project(c, vars)`.

- [ ] **Step 2: Fill in `apply_project`**

Move the exact remap loop from the original arm into the method body, replacing
the `todo!`. Build, then run the suite:

Run: `cargo nextest run -p horndb-sparql`
Expected: PASS (pure refactor).

- [ ] **Step 3: Add `ProjectOp` to `stream.rs`**

```rust
/// Streams its child, projecting each chunk to `vars`.
pub struct ProjectOp<'a, E: Executor + ?Sized> {
    rt: &'a Runtime<'a, E>,
    child: Box<dyn Op<'a> + 'a>,
    vars: Vec<Var>,
    schema: Vec<Var>,
}

impl<'a, E: Executor + ?Sized> ProjectOp<'a, E> {
    pub fn new(rt: &'a Runtime<'a, E>, child: Box<dyn Op<'a> + 'a>, vars: Vec<Var>) -> Self {
        let schema = if vars.is_empty() { child.schema().to_vec() } else { vars.clone() };
        Self { rt, child, vars, schema }
    }
}

impl<'a, E: Executor + ?Sized> Op<'a> for ProjectOp<'a, E> {
    fn schema(&self) -> &[Var] { &self.schema }
    fn next(&mut self) -> Result<Option<Batch>> {
        match self.child.next()? {
            Some(chunk) => Ok(Some(self.rt.apply_project(chunk, &self.vars)?)),
            None => Ok(None),
        }
    }
}
```

- [ ] **Step 4: Dispatch `Project`**

In `op/mod.rs`, `use stream::ProjectOp;` and:

```rust
            PhysicalPlan::Project { vars, inner } => {
                let child = self.build(inner)?;
                Ok(Box::new(ProjectOp::new(self, child, vars.clone())))
            }
```

- [ ] **Step 5: Run & commit**

Run: `cargo nextest run -p horndb-sparql` (Expected: PASS)

```bash
git add -A && git commit -m "feat(sparql): streaming ProjectOp (#143)"
```

---

## Task 5: `ExtendOp`

**Files:** same pattern as Task 4 for the `Extend` arm (`runtime.rs:341-380`).

- [ ] **Step 1: Extract `apply_extend(&self, batch, var, expr) -> Result<Batch>`**

Move the `Extend` arm body (evaluate `expr` per row via `eval_expr_to_term` on a
`decode_subset` of the referenced vars; append a new `Slot::Term` column for
`var`, or overwrite if `var` already in schema). Output schema = input schema
with `var` appended when new. Replace the arm with child-eval + `apply_extend`.

- [ ] **Step 2: Run the suite** — `cargo nextest run -p horndb-sparql` (Expected: PASS, pure refactor).

- [ ] **Step 3: Add `ExtendOp` to `stream.rs`** (one child, per-chunk `apply_extend`, no internal loop needed — Extend never drops rows):

```rust
pub struct ExtendOp<'a, E: Executor + ?Sized> {
    rt: &'a Runtime<'a, E>,
    child: Box<dyn Op<'a> + 'a>,
    var: Var,
    expr: Expr,
    schema: Vec<Var>,
}

impl<'a, E: Executor + ?Sized> ExtendOp<'a, E> {
    pub fn new(rt: &'a Runtime<'a, E>, child: Box<dyn Op<'a> + 'a>, var: Var, expr: Expr) -> Self {
        let mut schema = child.schema().to_vec();
        if !schema.iter().any(|v| v.name() == var.name()) {
            schema.push(var.clone());
        }
        Self { rt, child, var, expr, schema }
    }
}

impl<'a, E: Executor + ?Sized> Op<'a> for ExtendOp<'a, E> {
    fn schema(&self) -> &[Var] { &self.schema }
    fn next(&mut self) -> Result<Option<Batch>> {
        match self.child.next()? {
            Some(chunk) => Ok(Some(self.rt.apply_extend(chunk, &self.var, &self.expr)?)),
            None => Ok(None),
        }
    }
}
```

- [ ] **Step 4: Dispatch `Extend`** in `op/mod.rs` (build child, wrap in `ExtendOp`).

- [ ] **Step 5: Run & commit** — `cargo nextest run -p horndb-sparql` (PASS); `git commit -m "feat(sparql): streaming ExtendOp (#143)"`.

---

## Task 6: `SliceOp` (OFFSET/LIMIT across chunks)

Stateful: carries `to_skip` and `remaining` across `next` calls.

**Files:**
- Modify: `crates/sparql/src/exec/op/stream.rs` (add `SliceOp`)
- Modify: `crates/sparql/src/exec/op/mod.rs` (dispatch `Slice`)

- [ ] **Step 1: Write a chunk-spanning test** in `stream.rs` tests:

```rust
#[test]
fn slice_spans_chunks() {
    // 10 rows, OFFSET 2 LIMIT 5 -> rows 2..7 regardless of chunk size.
    // (Run under the Task 13 tiny-BATCH_ROWS config to force multi-chunk.)
    // Build BgpScan -> Slice{start:2,length:Some(5)} and assert 5 rows,
    // first row == the 3rd inserted entity.
}
```

Fill the body using the `HornBackend` pattern (insert 10 `("e{i}","p","o")`,
plan via `PhysicalPlan::Slice { inner: Box::new(BgpScan…), start: 2, length: Some(5) }`,
collect through `rt.build(&plan)`, assert total == 5).

- [ ] **Step 2: Run** — Expected: PASS via `MaterializedOp` first.

- [ ] **Step 3: Implement `SliceOp`**

```rust
/// OFFSET/LIMIT. `to_skip` rows are dropped first, then up to `remaining` are
/// emitted; state persists across chunks so a window can span chunk boundaries.
pub struct SliceOp<'a> {
    child: Box<dyn Op<'a> + 'a>,
    to_skip: usize,
    remaining: Option<usize>, // None = unbounded LIMIT
    schema: Vec<Var>,
}

impl<'a> SliceOp<'a> {
    pub fn new(child: Box<dyn Op<'a> + 'a>, start: usize, length: Option<usize>) -> Self {
        let schema = child.schema().to_vec();
        Self { child, to_skip: start, remaining: length, schema }
    }
}

impl<'a> Op<'a> for SliceOp<'a> {
    fn schema(&self) -> &[Var] { &self.schema }
    fn next(&mut self) -> Result<Option<Batch>> {
        if self.remaining == Some(0) {
            return Ok(None);
        }
        while let Some(mut chunk) = self.child.next()? {
            // Drop offset rows still owed.
            if self.to_skip > 0 {
                let drop = self.to_skip.min(chunk.rows.len());
                chunk.rows.drain(0..drop);
                self.to_skip -= drop;
            }
            // Cap to remaining LIMIT.
            if let Some(rem) = self.remaining {
                if chunk.rows.len() > rem {
                    chunk.rows.truncate(rem);
                }
                self.remaining = Some(rem - chunk.rows.len());
            }
            if !chunk.rows.is_empty() {
                return Ok(Some(chunk));
            }
            if self.remaining == Some(0) {
                return Ok(None);
            }
        }
        Ok(None)
    }
}
```

- [ ] **Step 4: Dispatch `Slice`** in `op/mod.rs`:

```rust
            PhysicalPlan::Slice { inner, start, length } => {
                let child = self.build(inner)?;
                Ok(Box::new(SliceOp::new(child, *start, *length)))
            }
```

- [ ] **Step 5: Run & commit** — `cargo nextest run -p horndb-sparql` (PASS); `git commit -m "feat(sparql): streaming SliceOp across chunk boundaries (#143)"`.

---

## Task 7: `ValuesOp`

**Files:**
- Modify: `crates/sparql/src/exec/op/source.rs` (add `ValuesOp`)
- Modify: `crates/sparql/src/exec/op/mod.rs` (dispatch `Values`)

- [ ] **Step 1: Extract `build_values_batch`**

Read `runtime.rs:381-404` (the `Values` arm: builds rows of `Slot::Term` from the
literal cell list `Vec<Vec<Option<Term>>>`, `None` → `Slot::Unbound`). Add a
free function in `source.rs`:

```rust
/// Materialize VALUES rows into a `Batch` (all `Slot::Term`/`Slot::Unbound`).
/// Mirrors the legacy `Values` arm at runtime.rs:381-404.
pub fn build_values_batch(vars: &[Var], rows: &[Vec<Option<Term>>]) -> Batch {
    // Relocate the arm body here verbatim.
    todo!("move Values arm body here")
}
```

- [ ] **Step 2: Fill it in, then implement `ValuesOp`** (re-chunk like `ScanOp`):

```rust
pub struct ValuesOp { inner: ScanOp }
impl ValuesOp {
    pub fn new(vars: &[Var], rows: &[Vec<Option<Term>>]) -> Self {
        Self { inner: ScanOp::new(build_values_batch(vars, rows)) }
    }
}
impl<'a> Op<'a> for ValuesOp {
    fn schema(&self) -> &[Var] { self.inner.schema() }
    fn next(&mut self) -> Result<Option<Batch>> { self.inner.next() }
}
```

- [ ] **Step 3: Dispatch `Values`** in `op/mod.rs` (`Ok(Box::new(ValuesOp::new(vars, rows)))`).

- [ ] **Step 4: Run & commit** — `cargo nextest run -p horndb-sparql` (PASS); `git commit -m "feat(sparql): native ValuesOp (#143)"`.

---

## Task 8: `DistinctOp` (streaming dedup via persistent seen-set)

More streaming than today: keeps only a `HashSet<Vec<KeyPart>>`, not the rows.

**Files:**
- Modify: `crates/sparql/src/exec/op/stream.rs` (add `DistinctOp`)
- Modify: `crates/sparql/src/exec/op/mod.rs` (dispatch `Distinct`)

- [ ] **Step 1: Write a cross-chunk dedup test** in `stream.rs` tests: insert
duplicate-producing data, plan `Distinct { BgpScan }`, assert the collected count
equals the distinct count (and, under Task 13's tiny `BATCH_ROWS`, that dedup
holds across chunk boundaries).

- [ ] **Step 2: Run** — Expected: PASS via `MaterializedOp`.

- [ ] **Step 3: Implement `DistinctOp`**

Read `runtime.rs:289-304` for the `KeyPart` key construction (`row.0.iter().map(Slot::key_part).collect::<Vec<_>>()`). Replicate that key per row:

```rust
use crate::exec::KeyPart;
use std::collections::HashSet;

/// Deduplicates rows by their `KeyPart` vector. The seen-set persists across
/// chunks, so only first-seen rows are emitted; loops internally to skip
/// fully-duplicate chunks.
pub struct DistinctOp<'a> {
    child: Box<dyn Op<'a> + 'a>,
    seen: HashSet<Vec<KeyPart>>,
    schema: Vec<Var>,
}

impl<'a> DistinctOp<'a> {
    pub fn new(child: Box<dyn Op<'a> + 'a>) -> Self {
        let schema = child.schema().to_vec();
        Self { child, seen: HashSet::new(), schema }
    }
}

impl<'a> Op<'a> for DistinctOp<'a> {
    fn schema(&self) -> &[Var] { &self.schema }
    fn next(&mut self) -> Result<Option<Batch>> {
        while let Some(chunk) = self.child.next()? {
            let mut kept = Vec::new();
            for row in chunk.rows {
                let key: Vec<KeyPart> = row.0.iter().map(|s| s.key_part()).collect();
                if self.seen.insert(key) {
                    kept.push(row);
                }
            }
            if !kept.is_empty() {
                return Ok(Some(Batch { schema: self.schema.clone(), rows: kept }));
            }
        }
        Ok(None)
    }
}
```

- [ ] **Step 4: Dispatch `Distinct`** in `op/mod.rs`.

- [ ] **Step 5: Run & commit** — `cargo nextest run -p horndb-sparql` (PASS); `git commit -m "feat(sparql): streaming DistinctOp with persistent seen-set (#143)"`.

---

## Task 9: `UnionOp`

Sequential drain: left to exhaustion, then right; each chunk remapped into the
merged schema with `normalize_columns`.

**Files:**
- Modify: `crates/sparql/src/exec/runtime.rs` (extract `apply_union_chunk`)
- Create: `crates/sparql/src/exec/op/blocking.rs`
- Modify: `crates/sparql/src/exec/op/mod.rs` (declare `mod blocking;`, dispatch `Union`)

- [ ] **Step 1: Extract the merged schema + per-child remap**

Read `runtime.rs:225-261` (the `Union` arm: computes merged schema, the inner
`place(child, schema)` fn at line 237 maps a child's rows into schema order with
`Slot::Unbound` for missing vars, then `normalize_columns`). Add two helpers:

```rust
    /// Merged Union schema: left schema ++ right-only vars (the legacy
    /// arm's schema rule). Pure; no `self` needed but kept as a method for
    /// locality.
    pub(crate) fn union_schema(&self, left: &[Var], right: &[Var]) -> Vec<Var> {
        let mut s = left.to_vec();
        for v in right {
            if !s.iter().any(|x| x.name() == v.name()) {
                s.push(v.clone());
            }
        }
        s
    }

    /// Remap one child's chunk into `merged` schema order (Unbound for absent
    /// vars) and restore column homogeneity. Wraps the `place` + `normalize`
    /// logic from runtime.rs:237-260.
    pub(crate) fn apply_union_chunk(&self, chunk: Batch, merged: &[Var]) -> Result<Batch> {
        // Relocate `place(&chunk, merged)` row-mapping here, then call
        // self.normalize_columns(&mut rows, merged.len())?.
        todo!("move Union place+normalize logic here")
    }
```

Rewrite the `Union` arm to: eval both, build `union_schema`, `apply_union_chunk`
each, concatenate rows. Run the suite (Expected: PASS, pure refactor).

- [ ] **Step 2: Implement `UnionOp`** in `blocking.rs`:

```rust
//! Operators that consume children eagerly (joins, group, sort, path, union).

use super::{Op, BATCH_ROWS};
use crate::algebra::Var;
use crate::error::Result;
use crate::exec::runtime::Runtime;
use crate::exec::{Batch, Executor, Row};

/// UNION: drains the left child, then the right, remapping every chunk into the
/// merged schema. Streams chunk-by-chunk within each side (no full buffering).
pub struct UnionOp<'a, E: Executor + ?Sized> {
    rt: &'a Runtime<'a, E>,
    left: Box<dyn Op<'a> + 'a>,
    right: Box<dyn Op<'a> + 'a>,
    on_left: bool,
    schema: Vec<Var>,
}

impl<'a, E: Executor + ?Sized> UnionOp<'a, E> {
    pub fn new(rt: &'a Runtime<'a, E>, left: Box<dyn Op<'a> + 'a>, right: Box<dyn Op<'a> + 'a>) -> Self {
        let schema = rt.union_schema(left.schema(), right.schema());
        Self { rt, left, right, on_left: true, schema }
    }
}

impl<'a, E: Executor + ?Sized> Op<'a> for UnionOp<'a, E> {
    fn schema(&self) -> &[Var] { &self.schema }
    fn next(&mut self) -> Result<Option<Batch>> {
        loop {
            if self.on_left {
                if let Some(chunk) = self.left.next()? {
                    return Ok(Some(self.rt.apply_union_chunk(chunk, &self.schema)?));
                }
                self.on_left = false;
            }
            match self.right.next()? {
                Some(chunk) => return Ok(Some(self.rt.apply_union_chunk(chunk, &self.schema)?)),
                None => return Ok(None),
            }
        }
    }
}
```

- [ ] **Step 3: Dispatch `Union`** in `op/mod.rs` (`mod blocking;`, build both children, wrap).

- [ ] **Step 4: Run & commit** — `cargo nextest run -p horndb-sparql` (PASS); `git commit -m "feat(sparql): UnionOp streaming both sides into merged schema (#143)"`.

---

## Task 10: `JoinOp` + `LeftJoinOp`

First cut drains **both** children, calls the extracted whole-batch join, then
emits the result in chunks. (True probe-side streaming is Task 10b, optional.)

**Files:**
- Modify: `crates/sparql/src/exec/runtime.rs` (extract `compute_join`, `compute_left_join`)
- Modify: `crates/sparql/src/exec/op/blocking.rs` (add `JoinOp`, `LeftJoinOp`, a `ChunkedBatch` emitter)
- Modify: `crates/sparql/src/exec/op/mod.rs` (dispatch `Join`, `LeftJoin`)

- [ ] **Step 1: Extract `compute_join` / `compute_left_join`**

Move the post-`eval` bodies of the `Join` arm (runtime.rs:40-103, everything after
the two `self.eval(...)?` lines) and the `LeftJoin` arm (runtime.rs:115-207) into:

```rust
    /// Hash inner join of two evaluated batches. Body relocated from the
    /// `Join` arm (runtime.rs:40-103), unchanged.
    pub(crate) fn compute_join(&self, l: Batch, r: Batch) -> Result<Batch> {
        todo!("move Join arm body (after the eval calls) here")
    }

    /// Hash left-outer join. Body relocated from the `LeftJoin` arm
    /// (runtime.rs:115-207), unchanged.
    pub(crate) fn compute_left_join(&self, l: Batch, r: Batch, expr: &Option<Expr>) -> Result<Batch> {
        todo!("move LeftJoin arm body (after the eval calls) here")
    }
```

Rewrite both arms to `let l = self.eval(left)?; let r = self.eval(right)?; self.compute_join(l, r)` (and the LeftJoin equivalent passing `expr`). Run the suite (Expected: PASS, pure refactor).

- [ ] **Step 2: Add a `ChunkedBatch` emitter helper** to `blocking.rs` (shared by Join/LeftJoin/Group/OrderBy/PathClosure — emits a finished `Batch` in `BATCH_ROWS` chunks):

```rust
/// Emits an already-computed `Batch` in `BATCH_ROWS` chunks. The shared tail
/// of every blocking operator.
pub(super) struct ChunkedBatch {
    schema: Vec<Var>,
    rows: std::vec::IntoIter<Row>,
}
impl ChunkedBatch {
    pub fn new(batch: Batch) -> Self {
        Self { schema: batch.schema, rows: batch.rows.into_iter() }
    }
    pub fn next_chunk(&mut self) -> Option<Batch> {
        let chunk: Vec<Row> = self.rows.by_ref().take(BATCH_ROWS).collect();
        if chunk.is_empty() { None } else { Some(Batch { schema: self.schema.clone(), rows: chunk }) }
    }
}
```

- [ ] **Step 3: Implement `JoinOp`** (drain both on first pull, then chunk):

```rust
pub struct JoinOp<'a, E: Executor + ?Sized> {
    rt: &'a Runtime<'a, E>,
    left: Box<dyn Op<'a> + 'a>,
    right: Box<dyn Op<'a> + 'a>,
    out: Option<ChunkedBatch>, // computed lazily on first next()
    schema: Vec<Var>,
}

impl<'a, E: Executor + ?Sized> JoinOp<'a, E> {
    pub fn new(rt: &'a Runtime<'a, E>, left: Box<dyn Op<'a> + 'a>, right: Box<dyn Op<'a> + 'a>) -> Self {
        let schema = rt.union_schema(left.schema(), right.schema());
        Self { rt, left, right, out: None, schema }
    }
}

impl<'a, E: Executor + ?Sized> Op<'a> for JoinOp<'a, E> {
    fn schema(&self) -> &[Var] { &self.schema }
    fn next(&mut self) -> Result<Option<Batch>> {
        if self.out.is_none() {
            let l = drain(&mut self.left)?;
            let r = drain(&mut self.right)?;
            self.out = Some(ChunkedBatch::new(self.rt.compute_join(l, r)?));
        }
        Ok(self.out.as_mut().unwrap().next_chunk())
    }
}
```

Add the `drain` helper to `blocking.rs`:

```rust
/// Pull an op to exhaustion, concatenating chunks into one `Batch`.
pub(super) fn drain<'a>(op: &mut Box<dyn Op<'a> + 'a>) -> Result<Batch> {
    let schema = op.schema().to_vec();
    let mut rows = Vec::new();
    while let Some(b) = op.next()? {
        rows.extend(b.rows);
    }
    Ok(Batch { schema, rows })
}
```

- [ ] **Step 4: Implement `LeftJoinOp`** — identical shape to `JoinOp`, holding an
`expr: Option<Expr>` and calling `self.rt.compute_left_join(l, r, &self.expr)`.
Its `schema` is also `union_schema(left, right)` (matches the legacy arm).

- [ ] **Step 5: Dispatch `Join` + `LeftJoin`** in `op/mod.rs`.

- [ ] **Step 6: Run & commit** — `cargo nextest run -p horndb-sparql` (PASS); `git commit -m "feat(sparql): JoinOp/LeftJoinOp over extracted compute helpers (#143)"`.

- [ ] **Step 7 (optional, Task 10b): stream the probe side.** Replace `JoinOp`'s
drain-both with: on first pull, `drain` only the **right** (build) side and call a
new `self.rt.build_join_index(r)` returning the `HashMap<Vec<String>, Vec<Row>>` +
`unkeyed` + `merge_plan`; then on each pull take one **left** chunk, probe it via a
new `self.rt.probe_join_chunk(&index, left_chunk)`, and emit. This removes the
left-side full materialization. Gate behind the same differential suite. Defer if
time-boxed — the drain-both version is correct and already removes parent-stack
materialization.

---

## Task 11: `GroupOp`, `OrderByOp`, `PathClosureOp` (blocking wrappers)

All three reuse existing whole-batch logic verbatim via `drain` + `ChunkedBatch`.

**Files:**
- Modify: `crates/sparql/src/exec/runtime.rs` (extract `compute_order_by`; `eval_group_native` and `eval_path_closure` already exist)
- Modify: `crates/sparql/src/exec/op/blocking.rs` (add the three ops)
- Modify: `crates/sparql/src/exec/op/mod.rs` (dispatch the three)

- [ ] **Step 1: Extract `compute_order_by`** — move the `OrderBy` arm body
(runtime.rs:316-340, after child eval: `decode_subset` order-key columns into
`Bindings`, sort via `compare_by_keys`) into
`pub(crate) fn compute_order_by(&self, batch: Batch, keys: &[(Expr, OrderDir)]) -> Result<Batch>`.
Rewrite the arm to child-eval + `compute_order_by`. (`Group` already delegates to
`eval_group_native(&self, b: Batch, …)`; `PathClosure` to `eval_path_closure`.)
Run the suite (Expected: PASS).

- [ ] **Step 2: Implement the three ops** in `blocking.rs`. Each holds `rt`, the
child op, an `Option<ChunkedBatch>`, and its `schema`. On first `next`, `drain` the
child, call the compute helper, wrap in `ChunkedBatch`. Example for `GroupOp`:

```rust
pub struct GroupOp<'a, E: Executor + ?Sized> {
    rt: &'a Runtime<'a, E>,
    child: Box<dyn Op<'a> + 'a>,
    keys: Vec<Var>,
    aggregates: Vec<crate::algebra::Aggregate>,
    out: Option<ChunkedBatch>,
    schema: Vec<Var>,
}

impl<'a, E: Executor + ?Sized> GroupOp<'a, E> {
    pub fn new(rt: &'a Runtime<'a, E>, child: Box<dyn Op<'a> + 'a>, keys: Vec<Var>, aggregates: Vec<crate::algebra::Aggregate>) -> Self {
        // Group output schema = keys ++ one var per aggregate. Derive it the
        // same way eval_group_native names its output columns (read that fn to
        // copy the naming) so schema() matches the produced batch.
        let schema = rt.group_output_schema(&keys, &aggregates);
        Self { rt, child, keys, aggregates, out: None, schema }
    }
}

impl<'a, E: Executor + ?Sized> Op<'a> for GroupOp<'a, E> {
    fn schema(&self) -> &[Var] { &self.schema }
    fn next(&mut self) -> Result<Option<Batch>> {
        if self.out.is_none() {
            let b = drain(&mut self.child)?;
            self.out = Some(ChunkedBatch::new(self.rt.eval_group_native(b, &self.keys, &self.aggregates)?));
        }
        Ok(self.out.as_mut().unwrap().next_chunk())
    }
}
```

Add `pub(crate) fn group_output_schema(&self, keys, aggregates) -> Vec<Var>` to
`runtime.rs` extracting the output-column naming `eval_group_native` already
computes (read runtime.rs:456-575 to copy the exact naming). `OrderByOp` and
`PathClosureOp` follow the same template (`OrderByOp.schema` = child schema;
`PathClosureOp.schema` = the closure's `subject`/`object` vars — read
`eval_path_closure` at runtime.rs:811-907 for the exact output schema).

- [ ] **Step 3: Dispatch** `Group`, `OrderBy`, `PathClosure` in `op/mod.rs`.

- [ ] **Step 4: Run & commit** — `cargo nextest run -p horndb-sparql` (PASS); `git commit -m "feat(sparql): blocking GroupOp/OrderByOp/PathClosureOp (#143)"`.

---

## Task 12: Delete the legacy `eval` and `MaterializedOp`

Every variant now has a native `Op`. Remove the fallback and the legacy path.

**Files:**
- Modify: `crates/sparql/src/exec/op/mod.rs` (remove `MaterializedOp` + the `_ =>` fallback arm in `build`; the match is now exhaustive over `PhysicalPlan`)
- Modify: `crates/sparql/src/exec/runtime.rs` (delete `fn eval` at line 34; keep all `compute_*`/`apply_*`/`eval_group_native`/`eval_path_closure` helpers)

- [ ] **Step 1: Make `build`'s match exhaustive**

Remove the `_ => MaterializedOp…` arm. The compiler will error on any unhandled
`PhysicalPlan` variant — confirm all 13 are covered (the `CountScan` variant does
not exist yet; it is added in Task 15).

- [ ] **Step 2: Delete `fn eval`** (runtime.rs:34-437). Build:

Run: `cargo build -p horndb-sparql`
Expected: compiles. If a `compute_*`/`apply_*` helper is now unused, that means a
variant still routes through a stale path — fix dispatch, do not delete the helper.

- [ ] **Step 3: Delete `MaterializedOp`** from `op/mod.rs` (struct + impl).

- [ ] **Step 4: Run the full suite, both feature sets**

Run: `cargo nextest run -p horndb-sparql && cargo nextest run -p horndb-sparql --features server`
Expected: PASS (the entire `slot_differential` suite + Task 0 test green on the
fully-streaming runtime).

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "refactor(sparql): delete legacy materializing eval; runtime is fully streaming (#143)"
```

---

## Task 13: Chunk-boundary test suite

Force tiny chunks to exercise cross-chunk state in every operator.

**Files:**
- Modify: `crates/sparql/src/exec/op/mod.rs` (make `BATCH_ROWS` test-overridable)
- Create: `crates/sparql/src/exec/op/chunk_tests.rs` (or a `#[cfg(test)] mod` in `mod.rs`)

- [ ] **Step 1: Make the chunk size test-overridable**

Replace the `const BATCH_ROWS` with a function reading a test override:

```rust
#[cfg(test)]
thread_local! {
    pub(crate) static TEST_BATCH_ROWS: std::cell::Cell<usize> = const { std::cell::Cell::new(4096) };
}

#[cfg(test)]
pub(crate) fn batch_rows() -> usize {
    TEST_BATCH_ROWS.with(|c| c.get())
}
#[cfg(not(test))]
pub(crate) const fn batch_rows() -> usize {
    4096
}
```

Replace every `BATCH_ROWS` use (`ScanOp`, `MaterializedOp` (already deleted),
`ChunkedBatch`, `ValuesOp`) with `batch_rows()`. Build to confirm.

- [ ] **Step 2: Write the cross-chunk differential test**

```rust
#[cfg(test)]
mod chunk_boundary {
    use super::batch_rows;
    // For each of: Distinct, Slice, Join, LeftJoin, Union, Group, OrderBy —
    // run the SAME query at TEST_BATCH_ROWS = 1, 2, and 4096 and assert the
    // collected, sorted result sets are byte-identical. Set the override with
    // super::TEST_BATCH_ROWS.with(|c| c.set(1)); before building the runtime.
    #[test]
    fn results_invariant_to_chunk_size() {
        // ... build HornBackend, run each plan at the three chunk sizes,
        //     assert_eq! on sorted Vec<Bindings> ...
    }
}
```

Fill in with the `HornBackend` + `plan_select` helpers. Cover at minimum
`Distinct` over a `Union`, a multi-row inner `Join`, and `Slice` with OFFSET that
lands mid-chunk at size 1.

- [ ] **Step 3: Run**

Run: `cargo nextest run -p horndb-sparql chunk_boundary`
Expected: PASS — results identical across chunk sizes.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "test(sparql): chunk-boundary invariance across operators (#143)"
```

---

## Task 14: Column pruning rewrite (#144, part 1)

**Files:**
- Create: `crates/sparql/src/plan/pushdown.rs`
- Modify: `crates/sparql/src/plan/mod.rs` (add `pub mod pushdown;`)
- Modify: `crates/sparql/src/exec/runtime.rs` (call `pushdown::rewrite` in `run`)

- [ ] **Step 1: Write a result-invariance test**

In `pushdown.rs` tests: a query with a `Project` that drops a variable, run with
and without `rewrite`, assert identical sorted results AND that `rewrite` shrinks
at least one scan/intermediate schema (assert structurally on the rewritten plan).

- [ ] **Step 2: Implement `needed_vars` + `rewrite` (pruning only)**

```rust
//! Heuristic-safe PhysicalPlan rewrites (#144): column pruning now, aggregate
//! pushdown in a later step. No cost model — every rewrite is always beneficial.

use crate::algebra::Var;
use crate::error::Result;
use crate::plan::PhysicalPlan;
use std::collections::HashSet;

/// Rewrite a plan for execution. Currently: prune columns no ancestor demands.
/// Result-identical by construction (only unreferenced columns are dropped).
pub fn rewrite(plan: &PhysicalPlan) -> Result<PhysicalPlan> {
    // `demanded = None` at the root means "all output columns are needed"
    // (the top Project/serialization defines them). prune() threads the
    // demanded set downward, narrowing BgpScan output and collapsing
    // redundant Projects.
    Ok(prune(plan, None))
}

fn prune(plan: &PhysicalPlan, demanded: Option<&HashSet<String>>) -> PhysicalPlan {
    // Implement per-variant: compute the vars this node needs from its
    // child(ren) (its own output cols ∪ vars referenced by its exprs/keys),
    // recurse, and drop child output columns outside that set. A `Project`
    // whose child already yields exactly the projected vars collapses to the
    // child. Conservative default for any node: pass `demanded ∪ own-refs`
    // through unchanged (never prune a column an expression might read).
    todo!("implement column-pruning rewrite per PhysicalPlan variant")
}
```

Implement `prune` variant-by-variant. Keep it conservative: when unsure whether a
column is needed, keep it (correctness over aggressiveness). Use the existing
`referenced_vars` (runtime.rs:923) pattern for expr var-extraction — either move it
to a shared module or duplicate the small helper in `pushdown.rs`.

- [ ] **Step 3: Wire `rewrite` into `run`**

In `runtime.rs::run`, before `self.build(plan)?`:

```rust
        let plan = crate::plan::pushdown::rewrite(plan)?;
        let mut op = self.build(&plan)?;
```

- [ ] **Step 4: Run the full suite + the new test**

Run: `cargo nextest run -p horndb-sparql && cargo nextest run -p horndb-sparql --features server`
Expected: PASS — pruning changes plans, never results.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(sparql): column-pruning plan rewrite (#144)"
```

---

## Task 15: Aggregate pushdown — `count_bgp` seam + `CountScan` (#144, part 2)

The COUNT win: `COUNT(*)`/`COUNT(?v)` over a bare BGP becomes a count, never
materializing rows. Scope guard: no DISTINCT, no GROUP key, no intervening Filter.

**Files:**
- Modify: `crates/sparql/src/exec/mod.rs` (add `count_bgp` to `Executor`)
- Modify: `crates/sparql/src/exec/horn.rs` (implement `HornBackend::count_bgp`)
- Modify: `crates/sparql/src/plan/mod.rs` (add `PhysicalPlan::CountScan`)
- Modify: `crates/sparql/src/plan/explain.rs` (render `CountScan`)
- Modify: `crates/sparql/src/plan/pushdown.rs` (recognize the pattern, lower to `CountScan`)
- Modify: `crates/sparql/src/exec/op/source.rs` (add `CountScanOp`)
- Modify: `crates/sparql/src/exec/op/mod.rs` (dispatch `CountScan`)

- [ ] **Step 1: Add the additive seam method**

In `crates/sparql/src/exec/mod.rs`, inside `trait Executor`, after
`cardinality_estimate`:

```rust
    /// Count solutions to a BGP without materializing rows. `None` means "no
    /// fast count" — callers fall back to a streaming `Group`. Additive: does
    /// not change `scan_bgp_ids`.
    fn count_bgp(&self, _patterns: &[TriplePattern]) -> Result<Option<usize>> {
        Ok(None)
    }
```

- [ ] **Step 2: Implement `HornBackend::count_bgp`**

In `horn.rs`, implement `count_bgp` by reusing the WCOJ cardinality path that
backs `cardinality_estimate` / `scan_bgp_ids` — count the join solutions without
decoding terms. Read `HornBackend::scan_bgp_ids` in `horn.rs` to find the WCOJ
solution-count entry point and return `Ok(Some(n))`. If an exact count is not
cheaply available for multi-pattern BGPs, return `Ok(None)` for those and
`Ok(Some(n))` only for the single-pattern / fast case — correctness falls back
safely either way.

- [ ] **Step 3: Add the `CountScan` plan node**

In `crates/sparql/src/plan/mod.rs`, add to `enum PhysicalPlan`:

```rust
    /// Pushed-down COUNT over a BGP (#144): yields one row binding `out_var`
    /// to the solution count, without materializing rows. Falls back to a
    /// streaming Group when the backend's `count_bgp` returns `None`.
    CountScan {
        patterns: Vec<TriplePattern>,
        out_var: Var,
    },
```

Render it in `explain.rs` (add a match arm mirroring `BgpScan`'s, labeled
`CountScan`).

- [ ] **Step 4: Recognize + lower the pattern in `pushdown::rewrite`**

Add a `push_aggregates(plan)` pass run by `rewrite` (after pruning). Match:
`Group { inner: BgpScan { patterns }, keys: [], aggregates: [agg] }` where `agg`
is `COUNT(*)` or `COUNT(?v)` with `distinct == false`. Lower to
`CountScan { patterns, out_var: <the aggregate's output var> }`. Read
`crate::algebra::Aggregate` / `AggFunc` (algebra.rs) to match the count variant
and extract the output var and the `distinct` flag. Leave every other shape
untouched.

```rust
fn push_aggregates(plan: PhysicalPlan) -> PhysicalPlan {
    // recurse; at each Group node test the scope guard and rewrite to CountScan
    // when it matches, else return the node with recursively-rewritten children.
    todo!("implement narrow COUNT-over-BGP pushdown")
}
```

- [ ] **Step 5: Implement `CountScanOp`**

In `source.rs`:

```rust
/// Pushed-down COUNT over a BGP: one row, count as a `Slot::Term` integer
/// literal. Falls back to scanning + counting if `count_bgp` returns `None`.
pub struct CountScanOp { batch: Option<Batch> }

impl CountScanOp {
    pub fn new<E: Executor + ?Sized>(
        exec: &E,
        patterns: &[TriplePattern],
        out_var: &Var,
    ) -> Result<Self> {
        let n = match exec.count_bgp(patterns)? {
            Some(n) => n,
            None => exec.scan_bgp_ids(patterns)?.rows.len(), // safe fallback
        };
        // Build a one-row batch: schema [out_var], row [Slot::Term(int literal)].
        // Use the same integer-literal constructor the aggregate path uses
        // (runtime::integer_literal) so COUNT lexical form is identical.
        let lit = crate::exec::runtime::integer_literal(n as i64);
        let batch = Batch {
            schema: vec![out_var.clone()],
            rows: vec![Row(vec![Slot::Term(lit)])],
        };
        Ok(Self { batch: Some(batch) })
    }
}

impl<'a> Op<'a> for CountScanOp {
    fn schema(&self) -> &[Var] {
        // store schema separately if `batch` is taken; simplest: keep a
        // `schema: Vec<Var>` field alongside `batch`.
        unimplemented!("return stored schema")
    }
    fn next(&mut self) -> Result<Option<Batch>> {
        Ok(self.batch.take())
    }
}
```

Adjust `CountScanOp` to hold a separate `schema: Vec<Var>` field (set in `new`) so
`schema()` works after `batch` is taken. Make `runtime::integer_literal` (line 982)
`pub(crate)` if it is not already.

- [ ] **Step 6: Write the pushdown test**

In `pushdown.rs` tests, with a `HornBackend` populated with N triples on predicate
`p`:
- `COUNT(*)` over `{ ?s p ?o }` returns N and the rewritten plan is `CountScan`.
- Add a counter to a test executor (or assert via `count_bgp` being exercised) that
  `decode_term` is **not** called per row on the count path.
- `COUNT(DISTINCT ?s)` and `COUNT(*) ... GROUP BY ?c` are **not** rewritten (stay
  `Group`), proving the scope guard.

- [ ] **Step 7: Dispatch `CountScan`** in `op/mod.rs`:

```rust
            PhysicalPlan::CountScan { patterns, out_var } => {
                Ok(Box::new(CountScanOp::new(self.exec(), patterns, out_var)?))
            }
```

- [ ] **Step 8: Run the full suite, both feature sets**

Run: `cargo nextest run -p horndb-sparql && cargo nextest run -p horndb-sparql --features server`
Expected: PASS, including the new pushdown tests and the parity check that the
pushed-down count equals the fallback `Group` count on the same data.

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "feat(sparql): COUNT-over-BGP aggregate pushdown via count_bgp + CountScan (#144)"
```

---

## Task 16: Benchmark + docs sync

**Files:**
- Modify: `docs/benchmarks.md`, `TASKS.md`, `docs/architecture.md`

- [ ] **Step 1: Run `agg_profile` locally (smoke only, not recorded)**

Run: `cargo run -p horndb-sparql --release --example agg_profile`
Capture the `COUNT(*)`, `GROUP BY`, and `COUNT(DISTINCT *)` timings vs the issue
#128 baseline (269 ms / 115 ms / 3.79 s). Sanity-check that `COUNT(*)` dropped
sharply (pushdown) and intermediate-heavy queries improved.

- [ ] **Step 2: Record the authoritative numbers on hornbench**

Per root `CLAUDE.md`: `ssh hornbench`, repo at `~/src/horndb`, `git fetch` +
checkout this branch (or `rsync` uncommitted files), run the SPB-256
aggregation-qps bench there, and record the before/after in `docs/benchmarks.md`
(note the env). Do **not** record laptop numbers.

- [ ] **Step 3: Sync `TASKS.md` + `docs/architecture.md`**

Check off the #143/#144 items, mirror to their GitHub issues (procedure in the
`TASKS.md` header), and flip the SPARQL runtime row in `docs/architecture.md`
from **planned** → **implemented** for streaming + pushdown (keep the wording
honest about the Task 10b probe-side-streaming follow-up and the narrow
aggregate-pushdown scope).

- [ ] **Step 4: Commit**

```bash
git add docs/benchmarks.md TASKS.md docs/architecture.md
git commit -m "docs: record streaming runtime + pushdown bench + sync TASKS/architecture (#143, #144)"
```

---

## Sequencing & open follow-ups

- **Order:** Task 0 → 1 → 2 → 3 → 4 → 5 → 6 → 7 → 8 → 9 → 10 → 11 → 12 → 13 → 14 → 15 → 16. Tasks 3–11 each keep the suite green via the `MaterializedOp` fallback until Task 12 removes it.
- **Task 10b (probe-side streaming)** is the one optional optimization; land it only if the bench shows join-tree materialization is the bottleneck. The drain-both `JoinOp` is correct without it.
- **Out of scope (file as follow-ups if wanted):** filter-aware / grouped / multi-aggregate pushdown; streaming results out to the HTTP layer; pushing expression eval onto ids. All noted in the design's Non-goals.
- **#142** (Group micro-opts) is independent and not part of this plan.
