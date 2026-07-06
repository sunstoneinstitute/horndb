---
status: executed
date: 2026-07-06
scope: "Probe-side streaming Join/LeftJoin + bound-key join-variable selection"
---

# Join Probe-Side Streaming + Bound-Key Selection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the native SPARQL `Join`/`LeftJoin` operators stream their probe (left) side instead of draining both children, and key their hash index on the build side's *actually-bound* columns instead of the schema intersection — deferred items 1 and 4 of [#128](https://github.com/sunstoneinstitute/horndb/issues/128).

**Architecture:** Both joins drain only the build side (right child) on first `next()` into a `JoinState` (owned batch + index of row *indices* + bound-key jvars + merge plan + forced-decode column set), then pull probe chunks and emit per chunk through a `pending` carry buffer. A new required `Op::may_emit_term` static-provenance method lets the join decide, before its first emission, which output columns must be decoded `Slot::Id → Slot::Term` so the stream-wide no-Id∧Term-mix invariant (which cross-chunk `DISTINCT`/`GROUP BY` keying relies on) survives without whole-output `normalize_columns`. Design rationale: `docs/specs/SPEC-20-join-probe-streaming.md`.

**Tech Stack:** Rust 1.90 workspace, crate `horndb-sparql` (`crates/sparql/`), `cargo nextest` as the runner.

**Read first:** `docs/specs/SPEC-20-join-probe-streaming.md` (the design this plan implements), `docs/specs/SPEC-19-streaming-runtime-pushdown.md` §2 (the operator model), `crates/sparql/CLAUDE.md`.

**Environment notes:**

- Work from the repo root. All paths below are repo-relative.
- Line anchors are as of commit `bb9c6f8`; earlier tasks shift later anchors, so locate by the named symbol if a line number is stale.
- Run tests with `cargo nextest run -p horndb-sparql <substring-filter>`. Nextest positional args are substring filters on test names.
- Do NOT run recorded benchmarks on a laptop; official numbers come from the `hornbench` host (see Task 5).

---

## File Structure

| File | Change |
|---|---|
| `crates/sparql/src/exec/runtime.rs` | Replace `batch_join_vars` with `bound_join_vars`; add `JoinState`, `build_join_state`, `probe_join_chunk`, `probe_left_join_chunk`, `merge_all_indexed`, `probe_into_indexed`, `force_term_columns`; delete `compute_join`, `compute_left_join`, `merge_all`, `probe_into_slots`; add `join_key_tests` module |
| `crates/sparql/src/exec/op/mod.rs` | Add required `Op::may_emit_term`; register `provenance_tests` module |
| `crates/sparql/src/exec/op/source.rs` | `may_emit_term` for `ScanOp`, `CountScanOp`, `ValuesOp` |
| `crates/sparql/src/exec/op/stream.rs` | `may_emit_term` for `ExtendOp`, `ProjectOp`, `SliceOp`, `FilterOp`, `DistinctOp` |
| `crates/sparql/src/exec/op/blocking.rs` | `may_emit_term` for `UnionOp`, `GroupOp`, `OrderByOp`, `PathClosureOp`; `merged_term_columns` helper; rewrite `JoinOp` and `LeftJoinOp` as streaming; add `tests` module with `CountingOp` |
| `crates/sparql/src/exec/op/provenance_tests.rs` | New: static-provenance unit tests |
| `crates/sparql/src/exec/op/chunk_tests.rs` | New chunk-boundary tests: unbound build var, fan-out carry, mixed-provenance DISTINCT (Join + LeftJoin), empty OPTIONAL |

---

### Task 1: Bound-key join-variable selection (`bound_join_vars`)

Fixes deferred item 4: the join key must come from the build side's actually-bound columns, not the schema intersection. Lands independently of streaming — both eager `compute_join`/`compute_left_join` switch to it, and Task 3 inherits it.

**Files:**
- Modify: `crates/sparql/src/exec/runtime.rs:769-778` (replace `batch_join_vars` incl. its doc comment), `:394` and `:465` (call sites in `compute_join`/`compute_left_join`)
- Test: `crates/sparql/src/exec/runtime.rs` (new `join_key_tests` module at end of file), `crates/sparql/src/exec/op/chunk_tests.rs` (semantics pin)

- [x] **Step 1: Write the failing unit tests**

Append at the very end of `crates/sparql/src/exec/runtime.rs` (after the closing brace of `mod slot_differential`):

```rust
#[cfg(test)]
mod join_key_tests {
    use super::*;
    use horndb_storage::TermId;

    fn batch(schema: &[&str], rows: Vec<Vec<Slot>>) -> Batch {
        Batch {
            schema: schema.iter().map(|s| Var::new(*s)).collect(),
            rows: rows.into_iter().map(Row).collect(),
        }
    }

    fn vars(names: &[&str]) -> Vec<Var> {
        names.iter().map(|n| Var::new(*n)).collect()
    }

    fn names(vs: &[Var]) -> Vec<&str> {
        vs.iter().map(|v| v.name()).collect()
    }

    /// ?v is shared but unbound in EVERY build row → dropped from the key
    /// (it carries zero selectivity and would unkey the whole build side);
    /// ?w keys normally.
    #[test]
    fn all_unbound_shared_var_is_dropped_from_key() {
        let build = batch(
            &["v", "w", "b"],
            vec![
                vec![Slot::Unbound, Slot::Id(TermId(1)), Slot::Id(TermId(10))],
                vec![Slot::Unbound, Slot::Id(TermId(2)), Slot::Id(TermId(20))],
            ],
        );
        let jvars = bound_join_vars(&vars(&["v", "w"]), &build);
        assert_eq!(names(&jvars), ["w"]);
    }

    /// ?v bound in one of two build rows → kept (its unbound row goes to the
    /// unkeyed bucket, which SPARQL compatibility semantics force anyway).
    #[test]
    fn partially_bound_shared_var_stays_in_key() {
        let build = batch(
            &["v", "w"],
            vec![
                vec![Slot::Unbound, Slot::Id(TermId(1))],
                vec![Slot::Id(TermId(7)), Slot::Id(TermId(2))],
            ],
        );
        let jvars = bound_join_vars(&vars(&["v", "w"]), &build);
        assert_eq!(names(&jvars), ["v", "w"]);
    }

    /// Non-shared bound vars never key; an empty build side yields an empty
    /// key set (every row then keys to Some(vec![]) — one bucket).
    #[test]
    fn non_shared_and_empty_build_yield_expected_keys() {
        let build = batch(&["b"], vec![vec![Slot::Id(TermId(1))]]);
        assert!(bound_join_vars(&vars(&["v", "w"]), &build).is_empty());

        let empty = batch(&["v"], vec![]);
        assert!(bound_join_vars(&vars(&["v"]), &empty).is_empty());
    }

    /// Slot::Term counts as bound, same as Slot::Id.
    #[test]
    fn term_slots_count_as_bound() {
        let build = batch(
            &["v"],
            vec![vec![Slot::Term(Term::Iri("http://ex/x".into()))]],
        );
        assert_eq!(names(&bound_join_vars(&vars(&["v"]), &build)), ["v"]);
    }
}
```

- [x] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p horndb-sparql join_key_tests`
Expected: FAIL to compile — `error[E0425]: cannot find function 'bound_join_vars' in this scope`

- [x] **Step 3: Implement `bound_join_vars` and delete `batch_join_vars`**

In `crates/sparql/src/exec/runtime.rs`, replace the whole `batch_join_vars` function and its doc comment (lines 769-778, starting `/// The join-variable set for the native LeftJoin ...`) with:

```rust
/// Join-key variables for the hash joins: the variables present in both
/// sides' schemas that are bound (non-`Unbound`) in at least one build-side
/// row, sorted by name (deterministic key order).
///
/// Keying on *bound* columns rather than the raw schema intersection fixes
/// the #128 pathological probe: `row_join_key` returns `None` for any row
/// whose key touches an `Unbound` slot, so a shared variable that is unbound
/// in EVERY build row (an OPTIONAL-produced column, VALUES UNDEF, …) would
/// send the entire build side to the `unkeyed` bucket that every probe row
/// scans — O(|l|·|r|) with correct results. Such a variable carries zero
/// selectivity; dropping it restores hashing on the remaining key vars.
///
/// Correctness is unaffected: `merge_rows`/`merge_rows_with` still check
/// every shared variable per candidate pair, and an unbound variable is
/// compatible with anything (SPARQL §18.3), so key selection only shapes the
/// candidate buckets, never the match set. A *partially* bound variable
/// stays in the key (its unbound rows go to `unkeyed`, which is semantically
/// forced). An empty build side yields an empty key set: every row keys to
/// `Some(vec![])` — one bucket, the cross-compatibility scan the semantics
/// require.
fn bound_join_vars(left_schema: &[Var], build: &Batch) -> Vec<Var> {
    let lvars: std::collections::BTreeSet<&str> =
        left_schema.iter().map(|v| v.name()).collect();
    let mut out: Vec<Var> = build
        .schema
        .iter()
        .enumerate()
        .filter(|(i, v)| {
            lvars.contains(v.name())
                && build
                    .rows
                    .iter()
                    .any(|r| !matches!(r.0[*i], Slot::Unbound))
        })
        .map(|(_, v)| v.clone())
        .collect();
    out.sort_by(|a, b| a.name().cmp(b.name()));
    out
}
```

Then update the two call sites:

In `compute_join` (line 394), change

```rust
        let jvars = batch_join_vars(&l, &r);
```

to

```rust
        let jvars = bound_join_vars(&l.schema, &r);
```

In `compute_left_join` (line 465), change

```rust
        let jvars = batch_join_vars(&l, &r);
```

to

```rust
        let jvars = bound_join_vars(&l.schema, &r);
```

- [x] **Step 4: Run the unit tests to verify they pass**

Run: `cargo nextest run -p horndb-sparql join_key_tests`
Expected: PASS (4 tests)

- [x] **Step 5: Add the end-to-end semantics pin (chunk-boundary suite)**

Append at the end of `crates/sparql/src/exec/op/chunk_tests.rs`:

```rust
// ---------------------------------------------------------------------------
// Join: shared var unbound in every build-side row (#128 bound-key selection)
// ---------------------------------------------------------------------------

/// ?v is shared but UNDEF in every right (build) row while ?w is bound
/// everywhere: the join must key on ?w alone and still honor SPARQL
/// compatibility (an unbound ?v matches anything, so each left row pairs
/// with its ?w partner). 2 rows, invariant across chunk sizes. This test is
/// a semantics pin: it passes before AND after the bound-key change — the
/// change is a complexity fix, not a result change.
#[test]
fn join_unbound_build_var_cross_chunk() {
    let horn = HornBackend::new();

    let left = PhysicalPlan::Values {
        vars: vec![Var::new("v"), Var::new("w")],
        rows: vec![
            vec![some_iri("v1"), some_iri("w1")],
            vec![some_iri("v2"), some_iri("w2")],
        ],
    };
    let right = PhysicalPlan::Values {
        vars: vec![Var::new("v"), Var::new("w"), Var::new("b")],
        rows: vec![
            vec![None, some_iri("w1"), some_iri("b1")],
            vec![None, some_iri("w2"), some_iri("b2")],
        ],
    };
    let plan = PhysicalPlan::Join {
        left: Box::new(left),
        right: Box::new(right),
    };

    assert_chunk_invariant!(&horn, &plan, "Join unbound build var");

    let big = run_sorted(&horn, &plan, 4096);
    assert_eq!(big.len(), 2, "each left row joins exactly its ?w partner");
}
```

- [x] **Step 6: Run the no-change gates**

Run: `cargo nextest run -p horndb-sparql`
Expected: PASS — all tests including `slot_differential`, `chunk_tests`, and the new `join_unbound_build_var_cross_chunk`

- [x] **Step 7: Commit**

```bash
git add crates/sparql/src/exec/runtime.rs crates/sparql/src/exec/op/chunk_tests.rs
git commit -m 'perf(sparql): key hash joins on build-side bound columns, not schemas (#128)'
```

---

### Task 2: `Op::may_emit_term` static column provenance

Infrastructure for Task 3: every operator declares, per output column, whether it may ever yield a `Slot::Term`. Required (no default) so a future operator that forgets is a compile error, not a silent cross-chunk `DISTINCT` bug. No behavior change in this task.

**Files:**
- Modify: `crates/sparql/src/exec/op/mod.rs:34-40` (trait) and `:73-74` (module registration)
- Modify: `crates/sparql/src/exec/op/source.rs:23-31, 66-74, 115-123` (the three `impl Op` blocks)
- Modify: `crates/sparql/src/exec/op/stream.rs:38-48, 83-93, 116-149, 172-185, 207-229` (the five `impl Op` blocks)
- Modify: `crates/sparql/src/exec/op/blocking.rs` (all six `impl Op` blocks + new helper)
- Create: `crates/sparql/src/exec/op/provenance_tests.rs`

- [x] **Step 1: Write the failing tests**

Create `crates/sparql/src/exec/op/provenance_tests.rs`:

```rust
//! Unit tests for `Op::may_emit_term` — the static per-column provenance
//! claim the streaming joins use to pick forced-decode columns (#128).

use crate::algebra::{Expr, Term, TriplePattern, Var};
use crate::exec::horn::HornBackend;
use crate::exec::runtime::Runtime;
use crate::exec::Store;
use crate::plan::PhysicalPlan;

fn iri(s: &str) -> Term {
    Term::Iri(format!("http://ex/{s}"))
}

fn cell(s: &str) -> Option<Term> {
    Some(iri(s))
}

fn scan_plan() -> PhysicalPlan {
    PhysicalPlan::BgpScan {
        patterns: vec![TriplePattern {
            subject: Term::Var(Var::new("s")),
            predicate: iri("p"),
            object: Term::Var(Var::new("o")),
        }],
    }
}

fn one_triple_store() -> HornBackend {
    let mut horn = HornBackend::new();
    horn.insert_triple(iri("s"), iri("p"), iri("o"));
    horn
}

/// BGP scan columns come straight from the dictionary: never Term.
#[test]
fn scan_columns_never_emit_term() {
    let horn = one_triple_store();
    let rt = Runtime::new(&horn);
    let op = rt.build(&scan_plan()).unwrap();
    assert_eq!(op.schema().len(), 2);
    assert_eq!(op.may_emit_term(), vec![false; 2]);
}

/// VALUES cells are Slot::Term (or Unbound): every column may emit Term.
#[test]
fn values_columns_may_emit_term() {
    let horn = HornBackend::new();
    let rt = Runtime::new(&horn);
    let plan = PhysicalPlan::Values {
        vars: vec![Var::new("x"), Var::new("y")],
        rows: vec![vec![cell("a"), None]],
    };
    let op = rt.build(&plan).unwrap();
    assert_eq!(op.may_emit_term(), vec![true, true]);
}

/// BIND marks only its output column; inherited scan columns stay Id-only.
#[test]
fn extend_marks_only_the_bind_column() {
    let horn = one_triple_store();
    let rt = Runtime::new(&horn);
    let plan = PhysicalPlan::Extend {
        inner: Box::new(scan_plan()),
        var: Var::new("x"),
        expr: Expr::Term(iri("c")),
    };
    let op = rt.build(&plan).unwrap();
    let terms = op.may_emit_term();
    for (i, v) in op.schema().iter().enumerate() {
        assert_eq!(terms[i], v.name() == "x", "column ?{}", v.name());
    }
}

/// A join ORs its children per column: scan-only columns stay false,
/// Values-fed columns (including a shared one) become true.
#[test]
fn join_ors_children_per_column() {
    let horn = one_triple_store();
    let rt = Runtime::new(&horn);
    let values = PhysicalPlan::Values {
        vars: vec![Var::new("o"), Var::new("z")],
        rows: vec![vec![cell("o"), cell("z1")]],
    };
    let plan = PhysicalPlan::Join {
        left: Box::new(scan_plan()),
        right: Box::new(values),
    };
    let op = rt.build(&plan).unwrap();
    let terms = op.may_emit_term();
    for (i, v) in op.schema().iter().enumerate() {
        let want = matches!(v.name(), "o" | "z"); // the Values side feeds ?o and ?z
        assert_eq!(terms[i], want, "column ?{}", v.name());
    }
}

/// GROUP BY: key columns inherit child provenance, aggregate outputs are
/// computed terms.
#[test]
fn group_keys_inherit_aggregates_are_term() {
    use crate::algebra::{AggFunc, Aggregate};
    let horn = one_triple_store();
    let rt = Runtime::new(&horn);
    let plan = PhysicalPlan::Group {
        inner: Box::new(scan_plan()),
        keys: vec![Var::new("s")],
        aggregates: vec![Aggregate {
            out: Var::new("cnt"),
            func: AggFunc::CountStar,
            distinct: false,
        }],
    };
    let op = rt.build(&plan).unwrap();
    // Schema is [?s, ?cnt] (group_output_schema: keys ++ aggregate outs).
    assert_eq!(op.may_emit_term(), vec![false, true]);
}
```

Register the module in `crates/sparql/src/exec/op/mod.rs` — change lines 73-74 from:

```rust
#[cfg(test)]
mod chunk_tests;
```

to:

```rust
#[cfg(test)]
mod chunk_tests;
#[cfg(test)]
mod provenance_tests;
```

- [x] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p horndb-sparql provenance_tests`
Expected: FAIL to compile — `error[E0599]: no method named 'may_emit_term' found for ... Box<dyn Op ...>`

- [x] **Step 3: Add the trait method and all thirteen implementations**

In `crates/sparql/src/exec/op/mod.rs`, replace the `Op` trait (lines 34-40) with:

```rust
/// A pull-based physical operator. The trait itself is lifetime-free; an
/// operator that borrows the runtime carries its own lifetime on the struct
/// (`impl<'r, …> Op for FooOp<'r, …>`) and `build` boxes it as `dyn Op + 'r`.
///
/// Stream-wide column-provenance invariant: across ALL chunks an op ever
/// yields, a given column never mixes `Slot::Id` and `Slot::Term`
/// (`Slot::Unbound` may appear anywhere). Cross-chunk keyed consumers
/// (`DistinctOp`'s seen-set, `GroupOp`) rely on this — `KeyPart::Id(x)` and
/// `KeyPart::Lex(lex(x))` hash differently for the same logical term.
/// `may_emit_term` is the static contract that lets the streaming joins
/// uphold it without seeing their whole output first.
pub trait Op {
    fn schema(&self) -> &[Var];
    /// Static per-column provenance claim, parallel to `schema()`: `true` at
    /// index `i` means column `i` MAY yield a `Slot::Term` somewhere in this
    /// op's output stream. Over-approximation; `false` is a guarantee (the
    /// column only ever holds `Slot::Id`/`Slot::Unbound`). Required — a new
    /// operator that forgets to declare provenance must fail to compile, not
    /// silently break cross-chunk DISTINCT/GROUP BY keying.
    fn may_emit_term(&self) -> Vec<bool>;
    fn next(&mut self) -> Result<Option<Batch>>;
}
```

In `crates/sparql/src/exec/op/source.rs`, add to `impl Op for ScanOp` (after `schema`):

```rust
    fn may_emit_term(&self) -> Vec<bool> {
        // Scan rows come straight from the dictionary: always Slot::Id.
        vec![false; self.inner.schema().len()]
    }
```

Add to `impl Op for CountScanOp`:

```rust
    fn may_emit_term(&self) -> Vec<bool> {
        // The count is a computed xsd:integer literal (Slot::Term).
        vec![true; self.schema.len()]
    }
```

Add to `impl Op for ValuesOp`:

```rust
    fn may_emit_term(&self) -> Vec<bool> {
        // VALUES cells are Slot::Term (or Slot::Unbound for UNDEF).
        vec![true; self.inner.schema().len()]
    }
```

In `crates/sparql/src/exec/op/stream.rs`, add to `impl Op for ExtendOp`:

```rust
    fn may_emit_term(&self) -> Vec<bool> {
        // Child columns keep their provenance; the BIND output column is a
        // computed Slot::Term (or Unbound). Covers both the appended-column
        // case (index past the child schema) and the re-BIND overwrite case.
        let child = self.child.may_emit_term();
        self.schema
            .iter()
            .enumerate()
            .map(|(i, v)| v.name() == self.var.name() || child.get(i).copied().unwrap_or(true))
            .collect()
    }
```

Add to `impl Op for ProjectOp`:

```rust
    fn may_emit_term(&self) -> Vec<bool> {
        // Remap the child's claims into projection order.
        let child_terms = self.child.may_emit_term();
        let child_schema = self.child.schema();
        self.schema
            .iter()
            .map(|v| {
                child_schema
                    .iter()
                    .position(|c| c.name() == v.name())
                    .map(|i| child_terms[i])
                    .unwrap_or(false)
            })
            .collect()
    }
```

Add to each of `impl Op for SliceOp`, `impl Op for FilterOp`, and `impl Op for DistinctOp` (all three are schema-preserving row filters):

```rust
    fn may_emit_term(&self) -> Vec<bool> {
        self.child.may_emit_term()
    }
```

In `crates/sparql/src/exec/op/blocking.rs`, add this free function right after `drain` (after line 27):

```rust
/// Static `may_emit_term` for a two-child merge (`Union`, `Join`, `LeftJoin`):
/// an output column may yield `Slot::Term` iff either contributing child
/// claims it may. A var absent from a side contributes only `Slot::Unbound`
/// there. (For Union this also covers `normalize_columns`: it only decodes
/// Id→Term when a Term is actually present, i.e. when a child claimed one.)
pub(super) fn merged_term_columns(
    out_schema: &[Var],
    left: &dyn Op,
    right: &dyn Op,
) -> Vec<bool> {
    let lt = left.may_emit_term();
    let rt = right.may_emit_term();
    let ls = left.schema();
    let rs = right.schema();
    out_schema
        .iter()
        .map(|v| {
            let l = ls
                .iter()
                .position(|x| x.name() == v.name())
                .map(|i| lt[i])
                .unwrap_or(false);
            let r = rs
                .iter()
                .position(|x| x.name() == v.name())
                .map(|i| rt[i])
                .unwrap_or(false);
            l || r
        })
        .collect()
}
```

Add to each of `impl Op for UnionOp`, `impl Op for JoinOp`, and `impl Op for LeftJoinOp`:

```rust
    fn may_emit_term(&self) -> Vec<bool> {
        merged_term_columns(&self.schema, self.left.as_ref(), self.right.as_ref())
    }
```

Add to `impl Op for GroupOp`:

```rust
    fn may_emit_term(&self) -> Vec<bool> {
        // Key columns clone a representative input slot (child provenance);
        // aggregate outputs are computed Slot::Term values. Schema order is
        // keys ++ aggregate outs (group_output_schema).
        let child_terms = self.child.may_emit_term();
        let child_schema = self.child.schema();
        self.schema
            .iter()
            .enumerate()
            .map(|(i, v)| {
                if i < self.keys.len() {
                    child_schema
                        .iter()
                        .position(|c| c.name() == v.name())
                        .map(|ci| child_terms[ci])
                        .unwrap_or(false)
                } else {
                    true
                }
            })
            .collect()
    }
```

Add to `impl Op for OrderByOp`:

```rust
    fn may_emit_term(&self) -> Vec<bool> {
        // Sort only reorders rows; slots pass through untouched.
        self.child.may_emit_term()
    }
```

Add to `impl Op for PathClosureOp`:

```rust
    fn may_emit_term(&self) -> Vec<bool> {
        // Closure endpoints are rebuilt via Batch::from_bindings: all Term.
        vec![true; self.schema.len()]
    }
```

- [x] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run -p horndb-sparql provenance_tests`
Expected: PASS (5 tests)

- [x] **Step 5: Run the full crate suite**

Run: `cargo nextest run -p horndb-sparql`
Expected: PASS — no behavior changed, only a new trait method

- [x] **Step 6: Commit**

```bash
git add crates/sparql/src/exec/op/
git commit -m 'feat(sparql): add Op::may_emit_term static column provenance (#128)'
```

---

### Task 3: Streaming `JoinOp` (probe-side streaming, inner join)

The build side (right) is drained into a `JoinState` on first `next()`; the probe side (left) streams chunk-by-chunk. Forced-term columns keep the output stream provenance-homogeneous. Deletes `compute_join` and `merge_all`.

**Files:**
- Modify: `crates/sparql/src/exec/runtime.rs` (add `JoinState` + `build_join_state` + `probe_join_chunk` + `merge_all_indexed` + `force_term_columns`; delete `compute_join` at :375-440 and `merge_all` at :647-660)
- Modify: `crates/sparql/src/exec/op/blocking.rs` (rewrite `JoinOp` at :90-126; module doc; new `tests` module)
- Test: `crates/sparql/src/exec/op/blocking.rs` (`join_streams_probe_side`), `crates/sparql/src/exec/op/chunk_tests.rs` (fan-out + mixed provenance)

- [x] **Step 1: Write the failing streaming-behavior test**

Append at the end of `crates/sparql/src/exec/op/blocking.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::super::source::ValuesOp;
    use super::super::{Op, TEST_BATCH_ROWS};
    use super::JoinOp;
    use crate::algebra::{Term, Var};
    use crate::error::Result;
    use crate::exec::horn::HornBackend;
    use crate::exec::runtime::Runtime;
    use crate::exec::Batch;
    use std::cell::Cell;
    use std::rc::Rc;

    /// Wraps an op and counts `next()` pulls, to observe streaming behavior.
    struct CountingOp<'r> {
        inner: Box<dyn Op + 'r>,
        pulls: Rc<Cell<usize>>,
    }

    impl<'r> Op for CountingOp<'r> {
        fn schema(&self) -> &[Var] {
            self.inner.schema()
        }
        fn may_emit_term(&self) -> Vec<bool> {
            self.inner.may_emit_term()
        }
        fn next(&mut self) -> Result<Option<Batch>> {
            self.pulls.set(self.pulls.get() + 1);
            self.inner.next()
        }
    }

    fn some_iri(s: &str) -> Option<Term> {
        Some(Term::Iri(format!("http://ex/{s}")))
    }

    /// The first `next()` on a Join must drain the build side, pull exactly
    /// ONE probe chunk, and emit — not drain the probe side. RED against the
    /// drain-both implementation (which pulls the probe side to exhaustion:
    /// 4 chunks + the final None = 5 pulls at chunk size 1).
    #[test]
    fn join_streams_probe_side() {
        TEST_BATCH_ROWS.with(|c| c.set(1));
        let horn = HornBackend::new();
        let rt = Runtime::new(&horn);

        let left_rows: Vec<Vec<Option<Term>>> =
            (0u8..4).map(|i| vec![some_iri(&format!("a{i}"))]).collect();
        let right_rows: Vec<Vec<Option<Term>>> = (0u8..4)
            .map(|i| vec![some_iri(&format!("a{i}")), some_iri(&format!("b{i}"))])
            .collect();

        let pulls = Rc::new(Cell::new(0));
        let left = CountingOp {
            inner: Box::new(ValuesOp::new(&[Var::new("a")], &left_rows)),
            pulls: Rc::clone(&pulls),
        };
        let right = ValuesOp::new(&[Var::new("a"), Var::new("b")], &right_rows);
        let mut join = JoinOp::new(&rt, Box::new(left), Box::new(right));

        let first = join.next().unwrap().expect("join must produce output");
        assert!(!first.rows.is_empty(), "no empty chunks");
        assert_eq!(
            pulls.get(),
            1,
            "first next() must pull exactly ONE probe chunk, not drain the probe side"
        );

        let mut total = first.rows.len();
        while let Some(b) = join.next().unwrap() {
            total += b.rows.len();
        }
        assert_eq!(total, 4, "all probe rows must still join");
        TEST_BATCH_ROWS.with(|c| c.set(4096));
    }
}
```

- [x] **Step 2: Run the test to verify it fails**

Run: `cargo nextest run -p horndb-sparql join_streams_probe_side`
Expected: FAIL — assertion `pulls.get() == 1` fails with left `5` (drain-both pulls the probe side to exhaustion before the first emission)

- [x] **Step 3: Add `JoinState` and the probe machinery to the runtime**

In `crates/sparql/src/exec/runtime.rs`, first **delete** `compute_join` (lines 374-440, from `/// Hash inner join of two materialized batches. Called by 'JoinOp'.` through its closing brace) and **delete** `merge_all` (lines 644-660, from `/// Merge the left row 'a' against each candidate right row using a` through its closing brace).

Then add, inside the `impl<'a, E: Executor + ?Sized> Runtime<'a, E>` block (right before its closing brace, after `probe_into_slots`):

```rust
    /// Drain-side setup for the streaming hash joins (#128): index the build
    /// batch by its bound join-variable key and precompute the merge plan and
    /// the forced-decode column set. `left_may_term` is the probe child's
    /// `Op::may_emit_term()`.
    pub(crate) fn build_join_state(
        &self,
        left_schema: &[Var],
        left_may_term: &[bool],
        build: Batch,
    ) -> Result<JoinState> {
        let out_schema = self.union_schema(left_schema, &build.schema);
        let jvars = bound_join_vars(left_schema, &build);
        let merge_plan = build_merge_plan(left_schema, &build.schema, &out_schema);

        // Index the build rows by decoded join key (option (b) — see
        // `row_join_key`); rows with an unbound jvar fall to `unkeyed`.
        let mut index: HashMap<Vec<String>, Vec<usize>> = HashMap::new();
        let mut unkeyed: Vec<usize> = Vec::new();
        for (i, row) in build.rows.iter().enumerate() {
            match self.row_join_key(row, &build.schema, &jvars)? {
                Some(k) => index.entry(k).or_default().push(i),
                None => unkeyed.push(i),
            }
        }

        // forced_term[c]: decode Slot::Id → Slot::Term on emit. Only SHARED
        // columns can mix provenance (a one-sided column passes a single
        // stream-homogeneous source through); a shared column is forced iff a
        // Term source exists on either side — statically on the probe side
        // (may_emit_term), actually on the drained build side. Deciding this
        // BEFORE the first emission is what keeps the whole output stream
        // free of Id∧Term mixing (per-chunk normalize_columns cannot: an
        // all-Id chunk followed by an all-Term chunk is mixed stream-wide but
        // homogeneous per chunk). BGP⋈BGP (no Term source) forces nothing
        // and pays zero decode.
        let forced_term: Vec<bool> = out_schema
            .iter()
            .map(|v| {
                let li = left_schema.iter().position(|x| x.name() == v.name());
                let ri = build.schema.iter().position(|x| x.name() == v.name());
                match (li, ri) {
                    (Some(l), Some(r)) => {
                        left_may_term[l]
                            || build
                                .rows
                                .iter()
                                .any(|row| matches!(row.0[r], Slot::Term(_)))
                    }
                    _ => false,
                }
            })
            .collect();

        Ok(JoinState {
            build,
            index,
            unkeyed,
            jvars,
            out_schema,
            merge_plan,
            forced_term,
        })
    }

    /// Probe one left-side chunk against the build state (inner join),
    /// returning the merged rows with forced columns decoded. May return an
    /// empty vec — the calling op loops (the Op contract forbids emitting
    /// `Some(empty)`).
    pub(crate) fn probe_join_chunk(&self, st: &JoinState, chunk: &Batch) -> Result<Vec<Row>> {
        let mut out = Vec::new();
        for a in &chunk.rows {
            match self.row_join_key(a, &chunk.schema, &st.jvars)? {
                Some(k) => {
                    if let Some(bucket) = st.index.get(&k) {
                        self.merge_all_indexed(a, st, bucket, &mut out)?;
                    }
                    if !st.unkeyed.is_empty() {
                        self.merge_all_indexed(a, st, &st.unkeyed, &mut out)?;
                    }
                }
                // Probe row with an unbound jvar: compatible with any value
                // of that var (SPARQL §18.3), so it must be checked against
                // ALL build rows; merge_rows_with still arbitrates each pair.
                None => {
                    let all: Vec<usize> = (0..st.build.rows.len()).collect();
                    self.merge_all_indexed(a, st, &all, &mut out)?;
                }
            }
        }
        self.force_term_columns(&mut out, &st.forced_term)?;
        Ok(out)
    }

    /// Merge probe row `a` against the build rows at `candidates`, appending
    /// every compatible union row to `out`.
    fn merge_all_indexed(
        &self,
        a: &Row,
        st: &JoinState,
        candidates: &[usize],
        out: &mut Vec<Row>,
    ) -> Result<()> {
        for &i in candidates {
            if let Some(m) = self.merge_rows_with(a, &st.build.rows[i], &st.merge_plan)? {
                out.push(m);
            }
        }
        Ok(())
    }

    /// Decode `Slot::Id → Slot::Term` in every forced column. The streaming
    /// replacement for the joins' old whole-batch `normalize_columns` call:
    /// it keeps a join's output stream free of Id∧Term mixing without ever
    /// seeing the whole output. Id→Term decoding is semantically the
    /// identity at the Bindings boundary.
    fn force_term_columns(&self, rows: &mut [Row], forced: &[bool]) -> Result<()> {
        for (c, &f) in forced.iter().enumerate() {
            if !f {
                continue;
            }
            for row in rows.iter_mut() {
                if let Slot::Id(id) = row.0[c] {
                    row.0[c] = Slot::Term(self.exec.decode_term(id)?);
                }
            }
        }
        Ok(())
    }
```

And add the state struct at file scope, immediately after the `Runtime` impl block's closing brace (before `bound_join_vars`):

```rust
/// Hash-join build state shared by the streaming `JoinOp`/`LeftJoinOp`
/// (#128): the fully-drained build side (right child) plus everything
/// derived from it. Built once on the operator's first `next()`; immutable
/// while the probe side streams. The index stores row *indices* into
/// `build.rows` (not `&Row`) so the state can own the batch it indexes.
pub(crate) struct JoinState {
    build: Batch,
    index: HashMap<Vec<String>, Vec<usize>>,
    unkeyed: Vec<usize>,
    jvars: Vec<Var>,
    out_schema: Vec<Var>,
    merge_plan: Vec<(Option<usize>, Option<usize>)>,
    /// Per-output-column: decode `Slot::Id → Slot::Term` on emit (see
    /// `force_term_columns` and the design doc §3).
    forced_term: Vec<bool>,
}
```

- [x] **Step 4: Rewrite `JoinOp` as a streaming operator**

In `crates/sparql/src/exec/op/blocking.rs`:

Update the import line 16 from:

```rust
use crate::exec::runtime::Runtime;
```

to:

```rust
use crate::exec::runtime::{JoinState, Runtime};
```

Update the module doc's first two lines (lines 1-2) from:

```rust
//! Operators that consume children eagerly or sequentially (union, joins,
//! group, sort, path).
```

to:

```rust
//! Operators that consume at least one child eagerly (union, group, sort,
//! path) plus the hybrid hash joins, which drain only their build side
//! (right) and stream their probe side (left) chunk-by-chunk (#128).
```

Replace the whole `JoinOp` section (struct + `impl JoinOp` + `impl Op for JoinOp`, lines 90-126 — keep the `may_emit_term` you added in Task 2, it appears again below) with:

```rust
/// Inner hash join, probe-side streaming (#128): the build side (right) is
/// drained into a `JoinState` on the first `next()`; the probe side (left)
/// is pulled chunk-by-chunk and never fully materialized. `pending` carries
/// a probe chunk's fan-out when it exceeds `batch_rows()`.
pub struct JoinOp<'r, E: Executor + ?Sized> {
    rt: &'r Runtime<'r, E>,
    left: Box<dyn Op + 'r>,
    right: Box<dyn Op + 'r>,
    state: Option<JoinState>,
    pending: Option<ChunkedBatch>,
    done: bool,
    schema: Vec<Var>,
}

impl<'r, E: Executor + ?Sized> JoinOp<'r, E> {
    pub fn new(rt: &'r Runtime<'r, E>, left: Box<dyn Op + 'r>, right: Box<dyn Op + 'r>) -> Self {
        let schema = rt.union_schema(left.schema(), right.schema());
        Self {
            rt,
            left,
            right,
            state: None,
            pending: None,
            done: false,
            schema,
        }
    }
}

impl<'r, E: Executor + ?Sized> Op for JoinOp<'r, E> {
    fn schema(&self) -> &[Var] {
        &self.schema
    }
    fn may_emit_term(&self) -> Vec<bool> {
        merged_term_columns(&self.schema, self.left.as_ref(), self.right.as_ref())
    }
    fn next(&mut self) -> Result<Option<Batch>> {
        loop {
            // 1. Serve buffered fan-out from the previous probe chunk.
            if let Some(buf) = self.pending.as_mut() {
                if let Some(chunk) = buf.next_chunk() {
                    return Ok(Some(chunk));
                }
                self.pending = None;
            }
            if self.done {
                return Ok(None);
            }
            // 2. First call: drain the build side and index it.
            if self.state.is_none() {
                let build = drain(&mut self.right)?;
                if build.rows.is_empty() {
                    // Inner join over an empty build side is empty; end the
                    // stream without pulling the probe side at all.
                    self.done = true;
                    return Ok(None);
                }
                let left_may_term = self.left.may_emit_term();
                self.state = Some(self.rt.build_join_state(
                    self.left.schema(),
                    &left_may_term,
                    build,
                )?);
            }
            // 3. Stream the probe side, one chunk per iteration; loop so a
            //    fully-unmatched chunk never yields Some(empty).
            match self.left.next()? {
                None => {
                    self.done = true;
                    return Ok(None);
                }
                Some(chunk) => {
                    let st = self.state.as_ref().expect("join state built above");
                    let rows = self.rt.probe_join_chunk(st, &chunk)?;
                    if !rows.is_empty() {
                        self.pending = Some(ChunkedBatch::new(Batch {
                            schema: self.schema.clone(),
                            rows,
                        }));
                    }
                }
            }
        }
    }
}
```

- [x] **Step 5: Run the streaming test to verify it passes**

Run: `cargo nextest run -p horndb-sparql join_streams_probe_side`
Expected: PASS

- [x] **Step 6: Add the chunk-boundary regression tests**

Append at the end of `crates/sparql/src/exec/op/chunk_tests.rs` (this needs one new import — change the first `use` line of the file from `use crate::algebra::{AggFunc, Aggregate, Expr, OrderDir, Term, Var};` to `use crate::algebra::{AggFunc, Aggregate, Expr, OrderDir, Term, TriplePattern, Var};` and add `use crate::exec::Store;` below the existing imports):

```rust
// ---------------------------------------------------------------------------
// Join: probe-side streaming (#128)
// ---------------------------------------------------------------------------

/// Each probe row matches 4 build rows: at chunk size 1/2 the merged output
/// of ONE probe chunk exceeds the chunk size, exercising the pending-buffer
/// carry inside the streaming JoinOp.
#[test]
fn join_fanout_exceeds_chunk_size() {
    let horn = HornBackend::new();

    let left = PhysicalPlan::Values {
        vars: vec![Var::new("a")],
        rows: (0u8..3).map(|i| vec![some_iri(&format!("a{i}"))]).collect(),
    };
    let mut right_rows: Vec<Vec<Option<Term>>> = Vec::new();
    for i in 0u8..3 {
        for j in 0u8..4 {
            right_rows.push(vec![
                some_iri(&format!("a{i}")),
                some_iri(&format!("b{i}{j}")),
            ]);
        }
    }
    let right = PhysicalPlan::Values {
        vars: vec![Var::new("a"), Var::new("b")],
        rows: right_rows,
    };
    let plan = PhysicalPlan::Join {
        left: Box::new(left),
        right: Box::new(right),
    };

    assert_chunk_invariant!(&horn, &plan, "Join fan-out");

    let big = run_sorted(&horn, &plan, 4096);
    assert_eq!(big.len(), 12, "3 probe rows x 4 matches");
}

/// Mixed-provenance regression for the streamed Join (design doc §3): the
/// probe side (VALUES, Term provenance) has an UNDEF ?v row FIRST; the build
/// side (BGP scan) binds ?v as Slot::Id. At chunk size 1 the UNDEF probe row
/// merges the build side's Id(v1) into the output stream before any probe
/// Term(v1) appears — per-chunk normalize_columns would leave chunk 1 as Id
/// and chunk 2 as Term, and the cross-chunk DISTINCT seen-set would count
/// one logical ?v twice. The forced-term column set keeps the whole stream
/// Term-homogeneous. Goes RED if the force_term_columns call is dropped
/// from probe_join_chunk.
#[test]
fn distinct_over_streamed_join_mixed_provenance() {
    let mut horn = HornBackend::new();
    horn.insert_triple(iri("v1"), iri("p"), iri("o1"));

    let left = PhysicalPlan::Values {
        vars: vec![Var::new("v")],
        rows: vec![vec![None], vec![some_iri("v1")]],
    };
    let right = PhysicalPlan::BgpScan {
        patterns: vec![TriplePattern {
            subject: Term::Var(Var::new("v")),
            predicate: iri("p"),
            object: Term::Var(Var::new("o")),
        }],
    };
    let plan = PhysicalPlan::Distinct {
        inner: Box::new(PhysicalPlan::Project {
            vars: vec![Var::new("v")],
            inner: Box::new(PhysicalPlan::Join {
                left: Box::new(left),
                right: Box::new(right),
            }),
        }),
    };

    assert_chunk_invariant!(&horn, &plan, "Join mixed provenance");

    let big = run_sorted(&horn, &plan, 4096);
    assert_eq!(big.len(), 1, "both probe rows bind the same logical ?v=v1");
}
```

- [x] **Step 7: Run the full crate suite (no-change gates)**

Run: `cargo nextest run -p horndb-sparql`
Expected: PASS — including `slot_differential` (esp. `inner_join_multi_row_shared_var`, `distinct_join_over_optional_no_column_mixing`), `join_cross_chunk`, `join_unbound_build_var_cross_chunk`, and the two new tests

- [x] **Step 8: Commit**

```bash
git add crates/sparql/src/exec/runtime.rs crates/sparql/src/exec/op/blocking.rs crates/sparql/src/exec/op/chunk_tests.rs
git commit -m 'perf(sparql): stream the probe side of the native Join (#128)'
```

---

### Task 4: Streaming `LeftJoinOp` (probe-side streaming, OPTIONAL)

Same skeleton as Task 3 with two differences: an unmatched probe row is emitted immediately with build-only columns `Unbound` (matched/unmatched is per-probe-row, so nothing spans chunks), and an empty build side does NOT end the stream. Deletes `compute_left_join` and `probe_into_slots`.

**Files:**
- Modify: `crates/sparql/src/exec/runtime.rs` (add `probe_left_join_chunk` + `probe_into_indexed`; delete `compute_left_join` and `probe_into_slots`; touch up the `normalize_columns` doc)
- Modify: `crates/sparql/src/exec/op/blocking.rs` (rewrite `LeftJoinOp`; extend `tests`)
- Test: `crates/sparql/src/exec/op/blocking.rs` (`left_join_streams_probe_side`), `crates/sparql/src/exec/op/chunk_tests.rs` (mixed provenance + empty OPTIONAL)

- [x] **Step 1: Write the failing streaming-behavior test**

In the `mod tests` block at the end of `crates/sparql/src/exec/op/blocking.rs`, change the import line `use super::JoinOp;` to `use super::{JoinOp, LeftJoinOp};` and append inside the module:

```rust
    /// Same probe-pull discipline for LeftJoin: first `next()` drains the
    /// build side, pulls ONE probe chunk, emits. Right side matches only
    /// a0/a1; a2/a3 must still come out with ?b unbound. RED against the
    /// drain-both implementation (5 pulls at chunk size 1).
    #[test]
    fn left_join_streams_probe_side() {
        TEST_BATCH_ROWS.with(|c| c.set(1));
        let horn = HornBackend::new();
        let rt = Runtime::new(&horn);

        let left_rows: Vec<Vec<Option<Term>>> =
            (0u8..4).map(|i| vec![some_iri(&format!("a{i}"))]).collect();
        let right_rows: Vec<Vec<Option<Term>>> = (0u8..2)
            .map(|i| vec![some_iri(&format!("a{i}")), some_iri(&format!("b{i}"))])
            .collect();

        let pulls = Rc::new(Cell::new(0));
        let left = CountingOp {
            inner: Box::new(ValuesOp::new(&[Var::new("a")], &left_rows)),
            pulls: Rc::clone(&pulls),
        };
        let right = ValuesOp::new(&[Var::new("a"), Var::new("b")], &right_rows);
        let mut lj = LeftJoinOp::new(&rt, Box::new(left), Box::new(right), None);

        let first = lj.next().unwrap().expect("left join must produce output");
        assert!(!first.rows.is_empty(), "no empty chunks");
        assert_eq!(
            pulls.get(),
            1,
            "first next() must pull exactly ONE probe chunk, not drain the probe side"
        );

        let mut total = first.rows.len();
        while let Some(b) = lj.next().unwrap() {
            total += b.rows.len();
        }
        assert_eq!(total, 4, "matched AND unmatched probe rows must come out");
        TEST_BATCH_ROWS.with(|c| c.set(4096));
    }
```

- [x] **Step 2: Run the test to verify it fails**

Run: `cargo nextest run -p horndb-sparql left_join_streams_probe_side`
Expected: FAIL — assertion `pulls.get() == 1` fails with left `5`

- [x] **Step 3: Add the left-join probe machinery to the runtime**

In `crates/sparql/src/exec/runtime.rs`, **delete** `compute_left_join` (the function starting `/// Hash left-outer join of two evaluated batches.` through its closing brace — lines 442-548 pre-Task-3) and **delete** `probe_into_slots` (starting `/// Merge the left row 'a' against each candidate right row, apply the` through its closing brace, including its `#[allow(clippy::too_many_arguments)]`).

Then add, inside the `impl<'a, E: Executor + ?Sized> Runtime<'a, E>` block right after `probe_join_chunk`:

```rust
    /// Probe one left-side chunk against the build state (left-outer join /
    /// OPTIONAL). `expr` is the OPTIONAL's inner FILTER, applied per merged
    /// row; a probe row with no surviving candidate is emitted with the
    /// build-side-only columns `Unbound`. Matched/unmatched is decided per
    /// probe row against the complete build state, so OPTIONAL semantics are
    /// chunk-independent. Forced columns are decoded before returning.
    pub(crate) fn probe_left_join_chunk(
        &self,
        st: &JoinState,
        chunk: &Batch,
        expr: Option<&Expr>,
    ) -> Result<Vec<Row>> {
        // Columns the inner FILTER reads (decoded per merged row).
        let mut want = HashSet::new();
        if let Some(e) = expr {
            referenced_vars(e, &mut want);
        }
        let mut out = Vec::new();
        for a in &chunk.rows {
            let mut matched = false;
            match self.row_join_key(a, &chunk.schema, &st.jvars)? {
                Some(k) => {
                    if let Some(bucket) = st.index.get(&k) {
                        matched |=
                            self.probe_into_indexed(a, st, bucket, expr, &want, &mut out)?;
                    }
                    if !st.unkeyed.is_empty() {
                        matched |=
                            self.probe_into_indexed(a, st, &st.unkeyed, expr, &want, &mut out)?;
                    }
                }
                // Probe row with an unbound jvar: may match any build row.
                None => {
                    let all: Vec<usize> = (0..st.build.rows.len()).collect();
                    matched |= self.probe_into_indexed(a, st, &all, expr, &want, &mut out)?;
                }
            }
            if !matched {
                // OPTIONAL: the probe row survives with build-only vars
                // unbound (merging with an all-Unbound build row takes the
                // probe side and leaves build-only vars Unbound).
                let unbound = Row(vec![Slot::Unbound; st.build.schema.len()]);
                if let Some(m) = self.merge_rows(
                    &chunk.schema,
                    a,
                    &st.build.schema,
                    &unbound,
                    &st.out_schema,
                )? {
                    out.push(m);
                }
            }
        }
        self.force_term_columns(&mut out, &st.forced_term)?;
        Ok(out)
    }

    /// Merge probe row `a` against the build rows at `candidates`, apply the
    /// OPTIONAL's inner FILTER on each merged row (decoding only the columns
    /// in `want`), push survivors to `out`, and report whether any candidate
    /// survived.
    fn probe_into_indexed(
        &self,
        a: &Row,
        st: &JoinState,
        candidates: &[usize],
        expr: Option<&Expr>,
        want: &HashSet<String>,
        out: &mut Vec<Row>,
    ) -> Result<bool> {
        let mut matched = false;
        for &i in candidates {
            if let Some(m) = self.merge_rows_with(a, &st.build.rows[i], &st.merge_plan)? {
                let keep = match expr {
                    Some(e) => {
                        let env = self.decode_subset(&m, &st.out_schema, want)?;
                        eval_expr(e, &env)?
                    }
                    None => true,
                };
                if keep {
                    matched = true;
                    out.push(m);
                }
            }
        }
        Ok(matched)
    }
```

Finally, update the `normalize_columns` doc comment (the line `/// merges children of differing slot provenance (Join, Union, LeftJoin).`) to reflect that only `Union` uses it now:

```rust
    /// Decode Id cells in columns that mix Slot::Id and Slot::Term, restoring
    /// the within-column homogeneity invariant. Now used only by `UnionOp`,
    /// which drains both children before normalizing; the streaming joins use
    /// `force_term_columns` instead (they never see their whole output).
```

- [x] **Step 4: Rewrite `LeftJoinOp` as a streaming operator**

In `crates/sparql/src/exec/op/blocking.rs`, replace the whole `LeftJoinOp` section (struct + `impl LeftJoinOp` + `impl Op for LeftJoinOp`, including the Task 2 `may_emit_term` which reappears below) with:

```rust
/// Left-outer hash join (OPTIONAL), probe-side streaming (#128): drains the
/// build side (right, the OPTIONAL pattern) into a `JoinState` on the first
/// `next()`, then streams the required (left) side chunk-by-chunk.
/// Matched/unmatched is decided per probe row against the complete build
/// state, so OPTIONAL semantics are chunk-independent. Unlike `JoinOp`, an
/// empty build side does NOT end the stream — every probe row is emitted
/// with build-only columns `Unbound`.
pub struct LeftJoinOp<'r, E: Executor + ?Sized> {
    rt: &'r Runtime<'r, E>,
    left: Box<dyn Op + 'r>,
    right: Box<dyn Op + 'r>,
    expr: Option<Expr>,
    state: Option<JoinState>,
    pending: Option<ChunkedBatch>,
    done: bool,
    schema: Vec<Var>,
}

impl<'r, E: Executor + ?Sized> LeftJoinOp<'r, E> {
    pub fn new(
        rt: &'r Runtime<'r, E>,
        left: Box<dyn Op + 'r>,
        right: Box<dyn Op + 'r>,
        expr: Option<Expr>,
    ) -> Self {
        let schema = rt.union_schema(left.schema(), right.schema());
        Self {
            rt,
            left,
            right,
            expr,
            state: None,
            pending: None,
            done: false,
            schema,
        }
    }
}

impl<'r, E: Executor + ?Sized> Op for LeftJoinOp<'r, E> {
    fn schema(&self) -> &[Var] {
        &self.schema
    }
    fn may_emit_term(&self) -> Vec<bool> {
        merged_term_columns(&self.schema, self.left.as_ref(), self.right.as_ref())
    }
    fn next(&mut self) -> Result<Option<Batch>> {
        loop {
            // 1. Serve buffered fan-out from the previous probe chunk.
            if let Some(buf) = self.pending.as_mut() {
                if let Some(chunk) = buf.next_chunk() {
                    return Ok(Some(chunk));
                }
                self.pending = None;
            }
            if self.done {
                return Ok(None);
            }
            // 2. First call: drain the build side and index it. An empty
            //    build side still streams (probe rows get Unbound fills).
            if self.state.is_none() {
                let build = drain(&mut self.right)?;
                let left_may_term = self.left.may_emit_term();
                self.state = Some(self.rt.build_join_state(
                    self.left.schema(),
                    &left_may_term,
                    build,
                )?);
            }
            // 3. Stream the probe side, one chunk per iteration.
            match self.left.next()? {
                None => {
                    self.done = true;
                    return Ok(None);
                }
                Some(chunk) => {
                    let st = self.state.as_ref().expect("join state built above");
                    let rows =
                        self.rt
                            .probe_left_join_chunk(st, &chunk, self.expr.as_ref())?;
                    if !rows.is_empty() {
                        self.pending = Some(ChunkedBatch::new(Batch {
                            schema: self.schema.clone(),
                            rows,
                        }));
                    }
                }
            }
        }
    }
}
```

- [x] **Step 5: Run the streaming test to verify it passes**

Run: `cargo nextest run -p horndb-sparql left_join_streams_probe_side`
Expected: PASS

- [x] **Step 6: Add the chunk-boundary regression tests**

Append at the end of `crates/sparql/src/exec/op/chunk_tests.rs`:

```rust
// ---------------------------------------------------------------------------
// LeftJoin: probe-side streaming (#128)
// ---------------------------------------------------------------------------

/// Mixed-provenance regression for the streamed LeftJoin, mirroring
/// distinct_over_streamed_join_mixed_provenance: UNDEF-first VALUES probe
/// (Term provenance) against a BGP build (Id provenance), DISTINCT ?v must
/// see ONE solution at every chunk size. Goes RED if the force_term_columns
/// call is dropped from probe_left_join_chunk.
#[test]
fn distinct_over_streamed_left_join_mixed_provenance() {
    let mut horn = HornBackend::new();
    horn.insert_triple(iri("v1"), iri("p"), iri("o1"));

    let left = PhysicalPlan::Values {
        vars: vec![Var::new("v")],
        rows: vec![vec![None], vec![some_iri("v1")]],
    };
    let right = PhysicalPlan::BgpScan {
        patterns: vec![TriplePattern {
            subject: Term::Var(Var::new("v")),
            predicate: iri("p"),
            object: Term::Var(Var::new("o")),
        }],
    };
    let plan = PhysicalPlan::Distinct {
        inner: Box::new(PhysicalPlan::Project {
            vars: vec![Var::new("v")],
            inner: Box::new(PhysicalPlan::LeftJoin {
                left: Box::new(left),
                right: Box::new(right),
                expr: None,
            }),
        }),
    };

    assert_chunk_invariant!(&horn, &plan, "LeftJoin mixed provenance");

    let big = run_sorted(&horn, &plan, 4096);
    assert_eq!(big.len(), 1, "both probe rows bind the same logical ?v=v1");
}

/// OPTIONAL over an empty build side: every probe row must stream through
/// with the build-only var unbound (the empty-build early-exit is an inner
/// Join fast path ONLY — LeftJoin must not take it).
#[test]
fn left_join_empty_optional_cross_chunk() {
    let horn = HornBackend::new();

    let left = PhysicalPlan::Values {
        vars: vec![Var::new("x")],
        rows: ["a", "b", "c"].iter().map(|s| vec![some_iri(s)]).collect(),
    };
    let right = PhysicalPlan::Values {
        vars: vec![Var::new("x"), Var::new("y")],
        rows: vec![],
    };
    let plan = PhysicalPlan::LeftJoin {
        left: Box::new(left),
        right: Box::new(right),
        expr: None,
    };

    assert_chunk_invariant!(&horn, &plan, "LeftJoin empty OPTIONAL");

    let big = run_sorted(&horn, &plan, 4096);
    assert_eq!(big.len(), 3, "all probe rows survive with ?y unbound");
}
```

- [x] **Step 7: Run the full crate suite (no-change gates)**

Run: `cargo nextest run -p horndb-sparql`
Expected: PASS — including `left_join_cross_chunk`, `distinct_join_over_optional_no_column_mixing`, `distinct_union_mixed_provenance_no_column_mixing`, and the two new tests

- [x] **Step 8: Commit**

```bash
git add crates/sparql/src/exec/runtime.rs crates/sparql/src/exec/op/blocking.rs crates/sparql/src/exec/op/chunk_tests.rs
git commit -m 'perf(sparql): stream the probe side of the native LeftJoin (#128)'
```

---

### Task 5: Final gates, smoke bench, docs sync

**Files:**
- Modify: `TASKS.md` (deferred items 1 and 4 under the #128 entry, lines 92-104)
- Modify: `docs/architecture.md` (§9 SPARQL streaming-runtime status note)
- Modify: `docs/index.md` (add the new design doc + plan, per `docs/CLAUDE.md`)

- [x] **Step 1: Format and lint**

Run: `cargo fmt --all`
Expected: no output (files already formatted or now fixed)

Run: `cargo clippy -p horndb-sparql --all-targets -- -D warnings`
Expected: clean, exit 0. (Pre-push runs the full `--workspace` clippy; this scoped run avoids rebuilding `oxrocksdb-sys` locally.)

- [x] **Step 2: Full test gates**

Run: `cargo nextest run -p horndb-sparql`
Expected: PASS, zero failures

Run: `cargo nextest run -p horndb-sparql --features server`
Expected: PASS, zero failures (required for a full SPARQL pass per `crates/sparql/CLAUDE.md`)

- [x] **Step 3: Local smoke bench (NOT recorded)**

Run: `cargo run -p horndb-sparql --release --example agg_profile`
Expected: completes; timings in the same ballpark as before this branch (Q1-Q5 have no OPTIONAL and small probe sides, so expect noise-level deltas). This is a laptop smoke check only — do **not** record these numbers anywhere.

- [x] **Step 4: Official benchmark on hornbench (record-keeping note)**

Per root `CLAUDE.md`, any recorded numbers must come from the `hornbench` host: `ssh hornbench`, repo at `~/src/horndb`, `git fetch` and check out this branch's commit, then run the SPB-256 suite there (or wait for the nightly on the merged commit). Expected outcome: **net-neutral aggregation-qps** (the SPB mix has small probe sides and no all-unbound shared join vars). Only update `docs/benchmarks.md` if the recorded number actually moves; otherwise leave it untouched.

- [x] **Step 5: Docs sync (same commit)**

In `TASKS.md`, under the #128 entry's "Remaining / deferred work" list (lines 92-104), remove items 1 and 4 and renumber, then add a LANDED note after the list. Replace the list block:

```markdown
  **Remaining / deferred work:**
  1. Probe-side streaming for `Join` — joins currently drain both children before
     emitting any output tuple (deferred; does not arise in the SPB mix but blocks
     full end-to-end streaming).
  2. Filter-aware / grouped / multi-aggregate count pushdown (deferred; only
     COUNT-over-full-BGP is pushed down today).
  3. Streaming results out to the HTTP layer — `Runtime::run` still collects a
     `Vec<Bindings>` before serializing (deferred).
  4. `batch_join_vars` intersects child *schemas*, not *bound* keys (native
     `LeftJoin`); a shared var unbound in every right row degrades the probe
     toward O(|l|·|r|) — correct but potentially slow on a pathological workload
     (does not arise in the SPB mix).
```

with:

```markdown
  **Join probe-side streaming + bound-key selection LANDED** (this branch):
  `JoinOp`/`LeftJoinOp` drain only their build side (right) into a hash index on
  first `next()` and stream the probe side (left) chunk-by-chunk; join keys come
  from the build side's actually-bound columns (`bound_join_vars`, replacing the
  schema-intersection `batch_join_vars` whose all-unbound shared vars degraded
  the probe toward O(|l|·|r|)); a new required `Op::may_emit_term` provenance
  method + forced-term columns preserve the stream-wide no-Id∧Term-mix invariant
  without whole-output normalization. Design:
  `docs/specs/SPEC-20-join-probe-streaming.md`; plan:
  `docs/plans/PLAN-20-01-join-probe-streaming.md`.

  **Remaining / deferred work:**
  1. Filter-aware / grouped / multi-aggregate count pushdown (deferred; only
     COUNT-over-full-BGP is pushed down today).
  2. Streaming results out to the HTTP layer — `Runtime::run` still collects a
     `Vec<Bindings>` before serializing (deferred).
```

In `docs/architecture.md` §9, update the streaming-runtime status text: where it describes the pull-based operator tree, note that `Join`/`LeftJoin` are now probe-side streaming (build side drained, probe side chunked) and that join keys are bound-column selected; keep the row's Status as **implemented**. Mirror the change to the #128 GitHub issue per the `TASKS.md` header procedure.

In `docs/index.md`, ensure `docs/specs/SPEC-20-join-probe-streaming.md` (purpose: streaming joins + bound-key selection design, read before touching `exec/op/blocking.rs` join code) and `docs/plans/PLAN-20-01-join-probe-streaming.md` (its implementation plan) are each listed with one line, per `docs/CLAUDE.md`. They may already be there from the plan-authoring branch — if so, skip.

- [x] **Step 6: Commit**

```bash
git add TASKS.md docs/architecture.md docs/index.md
git commit -m 'docs: sync TASKS/architecture/index for join probe streaming (#128)'
```

---

## Self-review notes

- **Spec coverage:** design §1 (sides) → Tasks 3-4; §2 (state machine, pending carry, empty-build fast path) → Tasks 3-4; §3 (`may_emit_term`, forced columns) → Tasks 2-4; §4 (`bound_join_vars`) → Task 1; testing section → the red tests in Tasks 1-4 plus gates in Task 5.
- **Deletions accounted for:** `batch_join_vars` (Task 1), `compute_join` + `merge_all` (Task 3), `compute_left_join` + `probe_into_slots` (Task 4). `merge_rows` was also deleted post-plan: a Task-4 review follow-up switched the OPTIONAL pad path to the precomputed merge plan, leaving it without callers. `merge_rows_with`, `row_join_key`, `build_merge_plan`, `union_schema`, `normalize_columns` (Union-only now), and `drain` survive.
- **Type consistency:** `JoinState` fields are private to `runtime.rs`; the ops hold it opaquely and go through `build_join_state` / `probe_join_chunk` / `probe_left_join_chunk`. `bound_join_vars(left_schema: &[Var], build: &Batch)` is used with that exact signature in Tasks 1 and 3.
