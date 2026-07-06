# Count-Pushdown Extensions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend the SPARQL planner's count pushdown (#128) so equality-filtered, grouped, and multi-count aggregation shapes avoid materializing solution rows.

**Architecture:** Three additive pieces on top of the landed `CountScan`/`count_bgp` pushdown: (1) an equality-filter inlining pre-step in `plan/pushdown.rs` that substitutes `FILTER(?v = <const>)` constants into the BGP patterns under count-only `Group`s; (2) a new `PhysicalPlan::GroupCountScan` leaf + `Executor::count_bgp_grouped` seam method (default `None`) whose `HornBackend` implementation hashes raw u64 WCOJ key columns without building rows; (3) a `GroupCountScanOp` operator that emits per-group counts in the same decoded-lexical key order as the streaming `eval_group_native` (order is observable under LIMIT). Design rationale, scope line, and deferrals: `docs/specs/2026-07-06-count-pushdown-extensions-design.md`.

**Tech Stack:** Rust 1.90.0 workspace, `horndb-sparql` crate only (`horndb-wcoj`/`horndb-storage` consumed read-only through existing APIs), `cargo nextest` for tests.

**Doc-sync note for executors:** `TASKS.md`, `docs/architecture.md`, `BENCHMARKS.md`, and `docs/index.md` are synced by the coordinating session — do NOT edit them from this plan. This plan touches only `crates/sparql/` (plus `crates/sparql/INTEGRATION-NOTES.md` in the final task). Reference only issue `#128` in commit messages; no other issue numbers exist for this work.

**Commit-message rule (from user CLAUDE.md):** never `git commit -m "…"` with double quotes around a message containing backticks — use the single-quoted-HEREDOC form shown in each commit step. Never add `Co-Authored-By` or similar trailers.

---

## File map

| File | Change |
|---|---|
| `crates/sparql/src/plan/mod.rs` | new `PhysicalPlan::GroupCountScan` variant |
| `crates/sparql/src/plan/pushdown.rs` | filter-inlining helpers, `lower_count_group` rewrite, arms + tests |
| `crates/sparql/src/plan/explain.rs` | `estimate` / `node_label` / `children` arms for the new variant |
| `crates/sparql/src/exec/mod.rs` | additive `Executor::count_bgp_grouped` default method |
| `crates/sparql/src/exec/op/source.rs` | `GroupCountScanOp` + `fallback_group_counts` |
| `crates/sparql/src/exec/op/mod.rs` | `build` arm for `GroupCountScan` |
| `crates/sparql/src/exec/horn.rs` | `HornBackend::count_bgp_grouped` fast path + seam tests |
| `crates/sparql/src/exec/runtime.rs` | one new leaf arm in the `contains_inner_join` test helper |
| `crates/sparql/INTEGRATION-NOTES.md` | short "Count pushdown" section (final task) |

---

### Task 1: `GroupCountScan` node, `count_bgp_grouped` seam default, and `GroupCountScanOp`

Adds the plan node, the seam method (default `None`), and the operator with its scan+hash-count fallback — no rewrite produces the node yet; tests drive it with hand-built plans (mirroring the landed `count_scan_falls_back_when_count_bgp_is_none` pattern).

**Files:**
- Modify: `crates/sparql/src/plan/mod.rs:17-20` (add variant after `CountScan`)
- Modify: `crates/sparql/src/exec/mod.rs:14,117-119` (import `Var`; add seam method after `count_bgp`)
- Modify: `crates/sparql/src/exec/op/source.rs` (new operator after `CountScanOp`, line 74)
- Modify: `crates/sparql/src/exec/op/mod.rs:8,87-89` (import + `build` arm)
- Modify: `crates/sparql/src/plan/pushdown.rs:156-159,243,369-371` (`map_children`, `output_vars`, `prune` arms) and its test helpers at lines 742-758, 885-956
- Modify: `crates/sparql/src/plan/explain.rs:77,137-144,209-211` (three arms)
- Modify: `crates/sparql/src/exec/runtime.rs:2093-2095` (`contains_inner_join` leaf arm)
- Test: `crates/sparql/src/plan/pushdown.rs` (`mod tests`)

- [ ] **Step 1: Write the failing tests**

Append to `mod tests` in `crates/sparql/src/plan/pushdown.rs` (after `count_scan_falls_back_when_count_bgp_is_none`, line 881):

```rust
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
        assert!(out.is_empty(), "keyed grouping of nothing has no groups: {out:?}");
    }
```

- [ ] **Step 2: Run the new tests to verify they fail**

Run: `cargo nextest run -p horndb-sparql group_count_scan`
Expected: **build FAILS** with `no variant named 'GroupCountScan' found for enum 'PhysicalPlan'` (a compile failure is the red state here — the variant does not exist yet).

- [ ] **Step 3: Add the `GroupCountScan` variant**

In `crates/sparql/src/plan/mod.rs`, insert after the `CountScan` variant (line 20):

```rust
    /// Pushed-down grouped / multi-output COUNT over a BGP (#128): one row
    /// per group — the key slots followed by one `xsd:integer` count per
    /// `out_vars` entry. Every aggregate this node replaces is a plain
    /// (non-DISTINCT) count of the group size, so all outputs carry the same
    /// number. `keys` may be empty (implicit grouping with ≥2 counts). Falls
    /// back to scan + hash-count when the backend has no fast
    /// `count_bgp_grouped`. Output rows are sorted by the decoded lexical
    /// form of the key slots — the same order the streaming `Group` emits.
    GroupCountScan {
        patterns: Vec<TriplePattern>,
        keys: Vec<Var>,
        out_vars: Vec<Var>,
    },
```

- [ ] **Step 4: Add the `count_bgp_grouped` seam default**

In `crates/sparql/src/exec/mod.rs`, change line 14 from

```rust
use crate::algebra::{Term, TriplePattern};
```

to

```rust
use crate::algebra::{Term, TriplePattern, Var};
```

and insert after the `count_bgp` default method (line 119, still inside `trait Executor`):

```rust
    /// Per-group solution counts for a BGP grouped by `keys`, without
    /// materializing rows. `None` = "no fast grouped count available" (the
    /// caller falls back to scanning + hash-counting the key columns).
    /// Additive; does not change `scan_bgp_ids`. When `Some`, the groups
    /// MUST partition the rows `scan_bgp_ids` would produce, keyed by term
    /// identity of the key columns: each entry carries one group's key slots
    /// (scan provenance preserved) and its row count, in no particular order.
    fn count_bgp_grouped(
        &self,
        _patterns: &[TriplePattern],
        _keys: &[Var],
    ) -> Result<Option<Vec<(Vec<Slot>, usize)>>> {
        Ok(None)
    }
```

- [ ] **Step 5: Implement `GroupCountScanOp`**

In `crates/sparql/src/exec/op/source.rs`, replace the import block (lines 3-6)

```rust
use super::{ChunkedBatch, Op};
use crate::algebra::{Term, TriplePattern, Var};
use crate::error::Result;
use crate::exec::{Batch, Executor, Row, Slot};
```

with

```rust
use super::{ChunkedBatch, Op};
use crate::algebra::{Term, TriplePattern, Var};
use crate::error::Result;
use crate::exec::runtime::{integer_literal, lex};
use crate::exec::{Batch, Executor, KeyPart, Row, Slot};
use std::collections::HashMap;
```

and append after `impl Op for CountScanOp` (line 74):

```rust
/// Pushed-down grouped / multi-output `COUNT` over a BGP (#128). One row per
/// group: the key slots, then one `xsd:integer` count per output var (every
/// replaced aggregate is a plain non-DISTINCT count, so all outputs carry the
/// group size). Rows are sorted by the decoded lexical form of the key slots
/// — the same deterministic order `eval_group_native` produces, which is
/// observable under a parent `Slice` (LIMIT).
pub struct GroupCountScanOp {
    schema: Vec<Var>,
    inner: ChunkedBatch,
}

impl GroupCountScanOp {
    pub fn new<E: Executor + ?Sized>(
        exec: &E,
        patterns: &[TriplePattern],
        keys: &[Var],
        out_vars: &[Var],
    ) -> Result<Self> {
        let mut schema: Vec<Var> = keys.to_vec();
        schema.extend(out_vars.iter().cloned());

        // Implicit grouping (no keys): exactly one row, even over zero
        // solutions (SPARQL §11.2 — COUNT of nothing is 0), answered by the
        // existing count_bgp seam (with its scan+len correctness fallback).
        if keys.is_empty() {
            let n = match exec.count_bgp(patterns)? {
                Some(n) => n,
                None => exec.scan_bgp_ids(patterns)?.rows.len(),
            };
            let lit = integer_literal(i64::try_from(n).unwrap_or(i64::MAX));
            let rows = vec![Row(out_vars
                .iter()
                .map(|_| Slot::Term(lit.clone()))
                .collect())];
            let batch = Batch {
                schema: schema.clone(),
                rows,
            };
            return Ok(Self {
                schema,
                inner: ChunkedBatch::new(batch),
            });
        }

        // Per-key counts: fast seam when the backend has one, else scan the
        // id-rows once and hash-count on the key columns only.
        let groups = match exec.count_bgp_grouped(patterns, keys)? {
            Some(groups) => groups,
            None => fallback_group_counts(exec, patterns, keys)?,
        };

        // Sort by decoded-lexical key — byte-identical ordering to
        // eval_group_native's sort_key (None sorts before Some, matching the
        // Unbound-first convention there).
        let mut tagged: Vec<(Vec<Option<String>>, Row)> = Vec::with_capacity(groups.len());
        for (key_slots, n) in groups {
            let sort_key: Vec<Option<String>> = key_slots
                .iter()
                .map(|s| match s {
                    Slot::Unbound => Ok(None),
                    Slot::Id(id) => exec.decode_term(*id).map(|t| Some(lex(&t))),
                    Slot::Term(t) => Ok(Some(lex(t))),
                })
                .collect::<Result<Vec<_>>>()?;
            let lit = integer_literal(i64::try_from(n).unwrap_or(i64::MAX));
            let mut slots = key_slots;
            slots.extend(out_vars.iter().map(|_| Slot::Term(lit.clone())));
            tagged.push((sort_key, Row(slots)));
        }
        tagged.sort_by(|a, b| a.0.cmp(&b.0));
        let batch = Batch {
            schema: schema.clone(),
            rows: tagged.into_iter().map(|(_, r)| r).collect(),
        };
        Ok(Self {
            schema,
            inner: ChunkedBatch::new(batch),
        })
    }
}

impl Op for GroupCountScanOp {
    fn schema(&self) -> &[Var] {
        &self.schema
    }

    fn next(&mut self) -> Result<Option<Batch>> {
        Ok(self.inner.next_chunk())
    }
}

/// Correctness fallback when the backend has no fast grouped count: scan the
/// id-rows once and hash-count on the key columns, never decoding non-key
/// columns. Grouping semantics are identical to `eval_group_native`:
/// `KeyPart` per key slot, `Unbound` for a key column the scan does not
/// produce, first-seen key slots kept per group.
fn fallback_group_counts<E: Executor + ?Sized>(
    exec: &E,
    patterns: &[TriplePattern],
    keys: &[Var],
) -> Result<Vec<(Vec<Slot>, usize)>> {
    let batch = exec.scan_bgp_ids(patterns)?;
    let key_idx: Vec<Option<usize>> = keys.iter().map(|k| batch.col(k.name())).collect();
    let mut groups: HashMap<Vec<KeyPart>, (Vec<Slot>, usize)> = HashMap::new();
    for r in &batch.rows {
        let gkey: Vec<KeyPart> = key_idx
            .iter()
            .map(|i| i.map(|i| r.0[i].key_part()).unwrap_or(KeyPart::Unbound))
            .collect();
        let entry = groups.entry(gkey).or_insert_with(|| {
            (
                key_idx
                    .iter()
                    .map(|i| i.map(|i| r.0[i].clone()).unwrap_or(Slot::Unbound))
                    .collect(),
                0,
            )
        });
        entry.1 += 1;
    }
    Ok(groups.into_values().collect())
}
```

- [ ] **Step 6: Wire the `build` arm**

In `crates/sparql/src/exec/op/mod.rs`, change line 8 from

```rust
use source::{CountScanOp, ScanOp, ValuesOp};
```

to

```rust
use source::{CountScanOp, GroupCountScanOp, ScanOp, ValuesOp};
```

and insert after the `CountScan` arm (line 89):

```rust
            PhysicalPlan::GroupCountScan {
                patterns,
                keys,
                out_vars,
            } => Ok(Box::new(GroupCountScanOp::new(
                self.exec(),
                patterns,
                keys,
                out_vars,
            )?)),
```

- [ ] **Step 7: Add the remaining exhaustive-match arms**

The new variant breaks every exhaustive `match` on `PhysicalPlan`. Fix each (the compiler will point at exactly these sites):

`crates/sparql/src/plan/pushdown.rs` — `map_children` leaf arm (line 159):

```rust
        leaf @ (BgpScan { .. } | CountScan { .. } | GroupCountScan { .. } | Values { .. }) => leaf,
```

`crates/sparql/src/plan/pushdown.rs` — `output_vars`, insert after the `CountScan` arm (line 243):

```rust
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
```

`crates/sparql/src/plan/pushdown.rs` — `prune`, replace the `CountScan` arm (lines 369-371):

```rust
        // Count leaves: nothing below them to prune, and their output columns
        // are exactly the replaced Group's output (narrowed, if at all, by an
        // ancestor Project). Unchanged.
        CountScan { .. } | GroupCountScan { .. } => node.clone(),
```

`crates/sparql/src/plan/explain.rs` — `estimate`, insert after the `CountScan` arm (line 77):

```rust
        // Grouped-count leaf: at most one row per underlying solution, so the
        // scan estimate is a sound upper bound (the same signal a Group over
        // the scan reports today).
        PhysicalPlan::GroupCountScan { patterns, .. } => exec.cardinality_estimate(patterns),
```

`crates/sparql/src/plan/explain.rs` — `node_label`, insert after the `CountScan` arm (line 144):

```rust
        PhysicalPlan::GroupCountScan {
            patterns,
            keys,
            out_vars,
        } => {
            format!(
                "GroupCountScan({} pattern{}, {} key{} -> {} count{})",
                patterns.len(),
                plural(patterns.len()),
                keys.len(),
                plural(keys.len()),
                out_vars.len(),
                plural(out_vars.len())
            )
        }
```

`crates/sparql/src/plan/explain.rs` — `children` leaf arm (lines 209-211):

```rust
        PhysicalPlan::BgpScan { .. }
        | PhysicalPlan::CountScan { .. }
        | PhysicalPlan::GroupCountScan { .. }
        | PhysicalPlan::Values { .. } => vec![],
```

`crates/sparql/src/exec/runtime.rs` — `contains_inner_join` leaf arm (lines 2093-2095):

```rust
            PhysicalPlan::BgpScan { .. }
            | PhysicalPlan::CountScan { .. }
            | PhysicalPlan::GroupCountScan { .. }
            | PhysicalPlan::Values { .. } => false,
```

`crates/sparql/src/plan/pushdown.rs` test helpers — add `GroupCountScan` to each leaf arm:

- `has_count_scan` (line 756): `PhysicalPlan::BgpScan { .. } | PhysicalPlan::GroupCountScan { .. } | PhysicalPlan::Values { .. } => false,`
- `scan_is_narrowed_to` (lines 906-908): `PhysicalPlan::BgpScan { .. } | PhysicalPlan::CountScan { .. } | PhysicalPlan::GroupCountScan { .. } | PhysicalPlan::Values { .. } => false,`
- `find_bgp_vars` (line 933): `PhysicalPlan::CountScan { .. } | PhysicalPlan::GroupCountScan { .. } | PhysicalPlan::Values { .. } => {}`
- `distinct_inner` (lines 952-954): `PhysicalPlan::BgpScan { .. } | PhysicalPlan::CountScan { .. } | PhysicalPlan::GroupCountScan { .. } | PhysicalPlan::Values { .. } => None,`

- [ ] **Step 8: Run the new tests to verify they pass**

Run: `cargo nextest run -p horndb-sparql group_count_scan`
Expected: PASS — 3 tests (`group_count_scan_falls_back_when_seam_is_none`, `group_count_scan_no_keys_is_implicit_group`, `group_count_scan_zero_solutions_with_keys_yields_no_rows`).

- [ ] **Step 9: Run the full crate suite**

Run: `cargo nextest run -p horndb-sparql`
Expected: PASS (no regressions — the rewrite does not produce the node yet, so all existing behavior is unchanged).

- [ ] **Step 10: Commit**

```bash
git add crates/sparql/src/plan/mod.rs crates/sparql/src/plan/pushdown.rs \
        crates/sparql/src/plan/explain.rs crates/sparql/src/exec/mod.rs \
        crates/sparql/src/exec/op/source.rs crates/sparql/src/exec/op/mod.rs \
        crates/sparql/src/exec/runtime.rs
git commit -m "$(cat <<'EOF'
feat(sparql): GroupCountScan node + count_bgp_grouped seam + operator (#128)

Adds the physical leaf, the additive Executor seam method (default None),
and GroupCountScanOp with a scan+hash-count fallback that groups on key
columns only. Output rows sort by decoded-lexical key, matching
eval_group_native (observable under LIMIT). No rewrite produces the node
yet; hand-built-plan tests pin fallback semantics, implicit-group zeros,
and keyed-empty-input behavior.

Design: docs/specs/2026-07-06-count-pushdown-extensions-design.md
EOF
)"
```

---

### Task 2: Equality-filter inlining → `CountScan`

Restructures `push_aggregates` around a `lower_count_group` recognizer and adds the `FILTER(?v = <const>)` inlining pre-step. In this task the lowering still only emits the landed `CountScan` shape (`keys == []`, single aggregate); the `GroupCountScan` lowering is Task 3.

**Files:**
- Modify: `crates/sparql/src/plan/pushdown.rs:61` (imports), `:85-152` (replace `push_aggregates`)
- Test: `crates/sparql/src/plan/pushdown.rs` (`mod tests`)

- [ ] **Step 1: Write the failing tests**

Append to `mod tests` in `crates/sparql/src/plan/pushdown.rs`:

```rust
    // ---- equality-filter inlining (#128 count-pushdown extensions) ----

    #[test]
    fn eq_filter_variants_push_down_and_count() {
        let horn = fixture();
        for (q, want) in [
            // Literal constant: names Alice ×2 (a and d).
            (
                "SELECT (COUNT(*) AS ?n) WHERE { ?s <http://ex/name> ?o FILTER(?o = \"Alice\") }",
                2,
            ),
            // IRI constant: only a knows b.
            (
                "SELECT (COUNT(*) AS ?n) WHERE { ?s <http://ex/knows> ?o FILTER(?o = <http://ex/b>) }",
                1,
            ),
            // Reversed operand order.
            (
                "SELECT (COUNT(*) AS ?n) WHERE { ?s <http://ex/knows> ?o FILTER(<http://ex/b> = ?o) }",
                1,
            ),
            // sameTerm lowers to Expr::Eq (translate.rs), so it inlines too.
            (
                "SELECT (COUNT(*) AS ?n) WHERE { ?s <http://ex/knows> ?o FILTER(sameTerm(?o, <http://ex/b>)) }",
                1,
            ),
            // Conjunction over two distinct vars.
            (
                "SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o FILTER(?p = <http://ex/name> && ?o = \"Alice\") }",
                2,
            ),
            // COUNT(?s): counting a var the substitution does not touch.
            (
                "SELECT (COUNT(?s) AS ?n) WHERE { ?s <http://ex/name> ?o FILTER(?o = \"Alice\") }",
                2,
            ),
            // COUNT(?o): counting the substituted var itself — every
            // surviving row has it bound, so the count is the row count.
            (
                "SELECT (COUNT(?o) AS ?n) WHERE { ?s <http://ex/name> ?o FILTER(?o = \"Alice\") }",
                2,
            ),
        ] {
            let plan = plan_select(q);
            let rewritten = rewrite(&plan).unwrap();
            assert!(
                has_count_scan(&rewritten),
                "equality-filtered count must push down:\n{q}\ngot {rewritten:#?}"
            );
            let with: Vec<Bindings> = Runtime::new(&horn).run(&plan).unwrap().collect();
            assert_eq!(
                single_count(&with),
                format!("{:?}", crate::exec::runtime::integer_literal(want)),
                "wrong count for:\n{q}"
            );
            let without = run_raw(&horn, &plan);
            assert_eq!(canon(with), canon(without), "parity broke for:\n{q}");
        }
    }

    #[test]
    fn eq_filter_guards_keep_group() {
        let horn = fixture();
        let cases = [
            // Var the BGP does not bind: the filter drops EVERY row (engine
            // Eq on unbound is false) while substitution would be a no-op.
            "SELECT (COUNT(*) AS ?n) WHERE { ?s <http://ex/name> ?o FILTER(?z = <http://ex/b>) }",
            // Same var equated twice: possibly unsatisfiable; we don't reason
            // about constant-vs-constant.
            "SELECT (COUNT(*) AS ?n) WHERE { ?s <http://ex/name> ?o FILTER(?o = \"Alice\" && ?o = \"Bob\") }",
            // Disjunction is not a conjunction of equalities.
            "SELECT (COUNT(*) AS ?n) WHERE { ?s <http://ex/name> ?o FILTER(?o = \"Alice\" || ?o = \"Bob\") }",
            // Var-to-var equality has no constant to substitute.
            "SELECT (COUNT(*) AS ?n) WHERE { ?s <http://ex/knows> ?o FILTER(?s = ?o) }",
            // Negated equality is not an equality.
            "SELECT (COUNT(*) AS ?n) WHERE { ?s <http://ex/name> ?o FILTER(?o != \"Alice\") }",
        ];
        for q in cases {
            let plan = plan_select(q);
            let rewritten = rewrite(&plan).unwrap();
            assert!(
                !has_count_scan(&rewritten),
                "inlining guard failed — this must stay a Group:\n{q}\ngot {rewritten:#?}"
            );
            let with: Vec<Bindings> = Runtime::new(&horn).run(&plan).unwrap().collect();
            let without = run_raw(&horn, &plan);
            assert_eq!(canon(with), canon(without), "result parity broke for:\n{q}");
        }
    }

    /// Engine `=` is TERM equality (runtime.rs eval_expr, Expr::Eq arm) and
    /// BGP constant matching is dictionary term identity — value-equal but
    /// term-distinct literals ("42" vs "042") match on neither path, so the
    /// inlining is exact. If Expr::Eq ever gains numeric VALUE semantics,
    /// this test fails and the literal-constant case of the inlining must be
    /// restricted to IRIs (see the 2026-07-06 design spec coupling note).
    #[test]
    fn eq_filter_literal_term_identity_pin() {
        let mut horn = HornBackend::new();
        let iri = |s: &str| Term::Iri(format!("http://ex/{s}"));
        let int = |s: &str| {
            Term::Literal(format!(
                "\"{s}\"^^<http://www.w3.org/2001/XMLSchema#integer>"
            ))
        };
        horn.insert_triple(iri("a"), iri("v"), int("42"));
        horn.insert_triple(iri("b"), iri("v"), int("042")); // value-equal, term-distinct
        let q = "SELECT (COUNT(*) AS ?n) WHERE { ?s <http://ex/v> ?o \
                 FILTER(?o = \"42\"^^<http://www.w3.org/2001/XMLSchema#integer>) }";
        let plan = plan_select(q);
        assert!(has_count_scan(&rewrite(&plan).unwrap()));
        let with: Vec<Bindings> = Runtime::new(&horn).run(&plan).unwrap().collect();
        let without = run_raw(&horn, &plan);
        assert_eq!(canon(with.clone()), canon(without));
        assert_eq!(
            single_count(&with),
            format!("{:?}", crate::exec::runtime::integer_literal(1)),
            "term-identity equality must count only the exact term"
        );
    }
```

Also **delete** this entry from the `cases` array of the existing `scope_guard_keeps_group_and_stays_correct` test (line 836-837) — it flips from negative to positive in this task:

```rust
            // Inner is Filter(BGP), not a bare BgpScan.
            "SELECT (COUNT(*) AS ?n) WHERE { ?s <http://ex/age> ?o FILTER(?o > \"20\") }",
```

(the range-filter guard is re-asserted by `eq_filter_guards_keep_group`'s sibling case in Task 3's updated guard list; equality filters are now expected to push down, so this list must not contain any filter case that inlines).

Then add the range-filter case to `eq_filter_guards_keep_group`'s `cases` array (it stays guarded — `>` is not an equality):

```rust
            // Range comparison: not expressible as a pattern constant.
            "SELECT (COUNT(*) AS ?n) WHERE { ?s <http://ex/age> ?o FILTER(?o > \"20\") }",
```

- [ ] **Step 2: Run the new tests to verify the positives fail**

Run: `cargo nextest run -p horndb-sparql eq_filter`
Expected: `eq_filter_variants_push_down_and_count` and `eq_filter_literal_term_identity_pin` FAIL with `equality-filtered count must push down`; `eq_filter_guards_keep_group` PASSES (guards trivially hold before the rewrite exists).

- [ ] **Step 3: Implement the inlining rewrite**

In `crates/sparql/src/plan/pushdown.rs`, change the import line (61) from

```rust
use crate::algebra::{AggFunc, Expr, Term, TriplePattern, Var};
```

to

```rust
use crate::algebra::{AggFunc, Aggregate, Expr, Term, TriplePattern, Var};
```

Replace the whole `push_aggregates` function **and its doc comment** (lines 85-152) with:

```rust
/// Recognize count-only aggregation shapes over a — possibly
/// equality-filtered — bare `BgpScan` and lower them to count leaves via
/// [`lower_count_group`]; recurse into every other node unchanged.
fn push_aggregates(plan: PhysicalPlan) -> PhysicalPlan {
    use PhysicalPlan::*;
    match plan {
        Group {
            inner,
            keys,
            aggregates,
        } => {
            if let Some(lowered) = lower_count_group(&inner, &keys, &aggregates) {
                return lowered;
            }
            Group {
                inner: Box::new(push_aggregates(*inner)),
                keys,
                aggregates,
            }
        }
        other => map_children(other, push_aggregates),
    }
}

/// `Some(lowered)` when `Group { inner, keys, aggregates }` is a count-only
/// shape over a bare — or equality-filtered — `BgpScan`; `None` otherwise
/// (the caller keeps the `Group` and recurses into its child).
///
/// Lowered here: `keys == []` + a single plain count → [`PhysicalPlan::CountScan`]
/// (the shape landed with #144), now also reachable through an inlinable
/// `FILTER`. Grouped / multi-count lowering to `GroupCountScan` is the next
/// task of this plan and currently returns `None`.
fn lower_count_group(
    inner: &PhysicalPlan,
    keys: &[Var],
    aggregates: &[Aggregate],
) -> Option<PhysicalPlan> {
    use PhysicalPlan::*;
    // 1. Peel the child: a bare scan, or Filter(scan) whose expression is an
    //    inlinable conjunction of `?v = <const>` equalities.
    let (patterns, subst): (&Vec<TriplePattern>, Vec<(String, Term)>) = match inner {
        BgpScan { patterns } => (patterns, Vec::new()),
        Filter { expr, inner: f } => {
            let BgpScan { patterns } = &**f else {
                return None;
            };
            let mut subst = Vec::new();
            if !eq_conjuncts(expr, &mut subst) {
                return None;
            }
            (patterns, subst)
        }
        _ => return None,
    };

    // 2. Vars bound in every solution of the PRE-substitution BGP. Group
    //    keys, `COUNT(?v)` inner vars, and substituted filter vars must all
    //    come from this set. (An equality-substituted var stays bound in
    //    every surviving solution, so counting it still counts every row —
    //    which is why the check runs against the pre-substitution vars.)
    let bgp_vars: HashSet<String> = {
        let mut names = Vec::new();
        for p in patterns {
            collect_pattern_vars(p, &mut names);
        }
        names.into_iter().collect()
    };

    // 3. Every aggregate must be a plain count of the group size.
    if aggregates.is_empty() || !aggregates.iter().all(|a| is_plain_count(a, &bgp_vars)) {
        return None;
    }
    // 4. Keys must be BGP-bound (an unbound key groups everything under
    //    Unbound — the streaming Group handles that rare shape) and never
    //    substituted away (the substituted column vanishes from the scan).
    if !keys.iter().all(|k| bgp_vars.contains(k.name())) {
        return None;
    }
    if subst
        .iter()
        .any(|(v, _)| keys.iter().any(|k| k.name() == v))
    {
        return None;
    }
    // 5. A substituted var the BGP does not bind means the filter drops
    //    EVERY row (engine Eq on unbound is false) while substitution would
    //    be a no-op — bail.
    if !subst.iter().all(|(v, _)| bgp_vars.contains(v.as_str())) {
        return None;
    }

    let patterns: Vec<TriplePattern> = patterns
        .iter()
        .map(|p| substitute_pattern(p, &subst))
        .collect();

    // 6. Lower. The landed single implicit-group count keeps CountScan.
    if keys.is_empty() && aggregates.len() == 1 {
        return Some(CountScan {
            patterns,
            out_var: aggregates[0].out.clone(),
        });
    }
    // Grouped / multi-count lowering (GroupCountScan) lands in the next task.
    None
}

/// True iff `agg` is a plain (non-DISTINCT) count whose value equals the
/// group size over a BGP binding `bgp_vars` in every solution: `COUNT(*)`,
/// or `COUNT(?v)` where the BGP binds `?v`. (A `COUNT(?z)` over a var the
/// BGP does not produce counts 0, so it is deliberately NOT covered.)
fn is_plain_count(agg: &Aggregate, bgp_vars: &HashSet<String>) -> bool {
    !agg.distinct
        && match &agg.func {
            AggFunc::CountStar => true,
            AggFunc::Count(e) => {
                matches!(&**e, Expr::Term(Term::Var(v)) if bgp_vars.contains(v.name()))
            }
            _ => false,
        }
}

/// Decompose `expr` as a conjunction of `?v = <const>` / `<const> = ?v`
/// equalities, appending `(var name, constant)` pairs to `out`. Returns
/// `false` (inlining bails) when any conjunct is not such an equality, a
/// variable repeats across conjuncts (possibly unsatisfiable), or a constant
/// is not an IRI/literal. `sameTerm` lowers to `Expr::Eq` in translate.rs,
/// so it is covered.
///
/// Result-invariance rests on engine `Expr::Eq` being structural `Term`
/// equality over oxrdf-normalized forms, which coincides with the dictionary
/// term identity BGP constants match by — full argument and the coupling
/// note about future value-equality semantics in
/// docs/specs/2026-07-06-count-pushdown-extensions-design.md.
fn eq_conjuncts(expr: &Expr, out: &mut Vec<(String, Term)>) -> bool {
    match expr {
        Expr::And(a, b) => eq_conjuncts(a, out) && eq_conjuncts(b, out),
        Expr::Eq(a, b) => {
            let (v, c) = match (&**a, &**b) {
                (Expr::Term(Term::Var(v)), Expr::Term(c)) => (v, c),
                (Expr::Term(c), Expr::Term(Term::Var(v))) => (v, c),
                _ => return false,
            };
            if !matches!(c, Term::Iri(_) | Term::Literal(_)) {
                return false;
            }
            if out.iter().any(|(name, _)| name == v.name()) {
                return false;
            }
            out.push((v.name().to_owned(), c.clone()));
            true
        }
        _ => false,
    }
}

/// Replace substituted variables by their constants in one pattern,
/// recursing into RDF 1.2 triple-term sub-patterns.
fn substitute_pattern(p: &TriplePattern, subst: &[(String, Term)]) -> TriplePattern {
    TriplePattern {
        subject: substitute_term(&p.subject, subst),
        predicate: substitute_term(&p.predicate, subst),
        object: substitute_term(&p.object, subst),
    }
}

fn substitute_term(t: &Term, subst: &[(String, Term)]) -> Term {
    match t {
        Term::Var(v) => subst
            .iter()
            .find(|(name, _)| name == v.name())
            .map(|(_, c)| c.clone())
            .unwrap_or_else(|| t.clone()),
        Term::Triple(tp) => Term::Triple(Box::new(substitute_pattern(tp, subst))),
        other => other.clone(),
    }
}
```

Also update the `rewrite` doc comment's first bullet (lines 68-71) to:

```rust
/// 1. [`push_aggregates`] — lower count-only `Group`s over a (possibly
///    equality-filtered) bare `BgpScan` to count leaves, answering them
///    without materializing rows (#144 + the 2026-07-06 extensions).
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run -p horndb-sparql eq_filter`
Expected: PASS — all 3 new tests.

Run: `cargo nextest run -p horndb-sparql pushdown`
Expected: PASS — the whole pushdown module including the pre-existing battery (`rewrite_is_result_invariant`, `scope_guard_keeps_group_and_stays_correct`, `count_pushdown_parity_with_streaming_group`).

- [ ] **Step 5: Run the full crate suite**

Run: `cargo nextest run -p horndb-sparql`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/sparql/src/plan/pushdown.rs
git commit -m "$(cat <<'EOF'
feat(sparql): inline equality FILTERs into count pushdown (#128)

FILTER(?v = <const>) / sameTerm conjunctions over a bare BGP under a
count-only Group now substitute their constants into the triple patterns,
so the landed CountScan/count_bgp fast path applies. Result-invariant
because engine Expr::Eq is term equality over oxrdf-normalized forms,
which coincides with the dictionary term identity BGP constants match by
(pinned by eq_filter_literal_term_identity_pin). Guards: conjuncts must be
Var=Const over distinct BGP-bound vars, never a GROUP BY key.

Design: docs/specs/2026-07-06-count-pushdown-extensions-design.md
EOF
)"
```

---

### Task 3: Grouped / multi-count lowering → `GroupCountScan`

Extends `lower_count_group` so grouped counts and multi-count aggregates lower to the Task-1 node, and flips the two now-covered scope-guard cases to positives.

**Files:**
- Modify: `crates/sparql/src/plan/pushdown.rs` (`lower_count_group` step 6; `scope_guard_keeps_group_and_stays_correct`)
- Test: `crates/sparql/src/plan/pushdown.rs` (`mod tests`)

- [ ] **Step 1: Write the failing tests and update the scope guard**

Append to `mod tests` in `crates/sparql/src/plan/pushdown.rs`:

```rust
    // ---- grouped / multi-count pushdown (#128 count-pushdown extensions) ----

    /// True iff the plan tree contains a `GroupCountScan` node anywhere.
    fn has_group_count_scan(p: &PhysicalPlan) -> bool {
        match p {
            PhysicalPlan::GroupCountScan { .. } => true,
            PhysicalPlan::Project { inner, .. }
            | PhysicalPlan::Filter { inner, .. }
            | PhysicalPlan::Distinct { inner }
            | PhysicalPlan::Slice { inner, .. }
            | PhysicalPlan::OrderBy { inner, .. }
            | PhysicalPlan::Extend { inner, .. }
            | PhysicalPlan::Group { inner, .. } => has_group_count_scan(inner),
            PhysicalPlan::Join { left, right }
            | PhysicalPlan::LeftJoin { left, right, .. }
            | PhysicalPlan::Union { left, right } => {
                has_group_count_scan(left) || has_group_count_scan(right)
            }
            PhysicalPlan::PathClosure { edge, .. } => has_group_count_scan(edge),
            PhysicalPlan::BgpScan { .. }
            | PhysicalPlan::CountScan { .. }
            | PhysicalPlan::Values { .. } => false,
        }
    }

    /// Structural + order/value parity for every grouped / multi-count shape.
    /// Full `Vec` equality (NOT `canon`): the decoded-lexical group order is
    /// part of the contract because a parent LIMIT observes it.
    #[test]
    fn grouped_count_parity_battery() {
        let horn = fixture();
        for q in [
            // GROUP BY + COUNT(*) over a 1-pattern BGP.
            "SELECT ?o (COUNT(*) AS ?c) WHERE { ?s <http://ex/name> ?o } GROUP BY ?o",
            // GROUP BY + COUNT(?s) over a 2-pattern BGP (agg_profile Q3 shape).
            "SELECT ?o (COUNT(?s) AS ?c) WHERE { ?s <http://ex/name> ?o . ?s <http://ex/knows> ?k } GROUP BY ?o",
            // Multi-key GROUP BY.
            "SELECT ?p ?o (COUNT(*) AS ?c) WHERE { ?s ?p ?o } GROUP BY ?p ?o",
            // Multiple plain counts, implicit group.
            "SELECT (COUNT(*) AS ?n) (COUNT(?o) AS ?m) WHERE { ?s <http://ex/name> ?o }",
            // Multiple plain counts WITH a key.
            "SELECT ?o (COUNT(*) AS ?n) (COUNT(?s) AS ?m) WHERE { ?s <http://ex/name> ?o } GROUP BY ?o",
            // Composed with Task 2's equality-filter inlining.
            "SELECT ?o (COUNT(*) AS ?c) WHERE { ?s <http://ex/knows> ?k . ?s <http://ex/name> ?o FILTER(?k = <http://ex/b>) } GROUP BY ?o",
            // Zero solutions with keys: no groups, no rows.
            "SELECT ?o (COUNT(*) AS ?c) WHERE { ?s <http://ex/nope> ?o } GROUP BY ?o",
            // Zero solutions, implicit group, two counts: one row of zeros.
            "SELECT (COUNT(*) AS ?n) (COUNT(?o) AS ?m) WHERE { ?s <http://ex/nope> ?o }",
        ] {
            let plan = plan_select(q);
            let rewritten = rewrite(&plan).unwrap();
            assert!(
                has_group_count_scan(&rewritten),
                "must lower to GroupCountScan:\n{q}\ngot {rewritten:#?}"
            );
            let with: Vec<Bindings> = Runtime::new(&horn).run(&plan).unwrap().collect();
            let without = run_raw(&horn, &plan);
            assert_eq!(with, without, "order/value parity broke for:\n{q}");
        }
    }

    #[test]
    fn grouped_count_order_observable_under_limit() {
        let horn = fixture();
        let q =
            "SELECT ?o (COUNT(?s) AS ?c) WHERE { ?s <http://ex/name> ?o } GROUP BY ?o LIMIT 2";
        let plan = plan_select(q);
        assert!(has_group_count_scan(&rewrite(&plan).unwrap()));
        let with: Vec<Bindings> = Runtime::new(&horn).run(&plan).unwrap().collect();
        let without = run_raw(&horn, &plan);
        assert_eq!(with, without, "LIMIT over a grouped count must see the same order");
        assert_eq!(with.len(), 2);
        // eval_group_native sorts groups by decoded key lexical: Alice first
        // (fixture names: Alice ×2, Bob, Carol).
        assert_eq!(
            with[0].get("o"),
            Some(&Term::Literal("\"Alice\"".into())),
            "first group must be Alice (lexical sort): {with:?}"
        );
        assert_eq!(
            format!("{:?}", with[0].get("c").expect("?c bound")),
            format!("{:?}", crate::exec::runtime::integer_literal(2))
        );
    }

    /// A GROUP BY key the BGP does not bind stays a streaming Group (the
    /// query level can't easily express this, so hand-build the plan).
    #[test]
    fn grouped_count_key_not_bound_by_bgp_stays_group() {
        use crate::algebra::TriplePattern;
        let var = |n: &str| Term::Var(Var::new(n));
        let plan = PhysicalPlan::Group {
            inner: Box::new(PhysicalPlan::BgpScan {
                patterns: vec![TriplePattern {
                    subject: var("s"),
                    predicate: Term::Iri("http://ex/p".into()),
                    object: var("o"),
                }],
            }),
            keys: vec![Var::new("z")], // not produced by the BGP
            aggregates: vec![Aggregate {
                out: Var::new("c"),
                func: AggFunc::CountStar,
                distinct: false,
            }],
        };
        let rewritten = rewrite(&plan).unwrap();
        assert!(
            !has_group_count_scan(&rewritten),
            "unbound key must keep the streaming Group; got {rewritten:#?}"
        );
    }
```

Then rewrite the `cases` array of `scope_guard_keeps_group_and_stays_correct` (the "GROUP BY: non-empty keys" and "Two aggregates" entries flip to positives covered by `grouped_count_parity_battery`; new deferred-shape guards take their place), and strengthen its assertion to exclude both count nodes:

```rust
        // Each of these must NOT become a CountScan or GroupCountScan, and
        // must still be correct.
        let cases = [
            // DISTINCT count: not a plain count.
            "SELECT (COUNT(DISTINCT ?s) AS ?n) WHERE { ?s <http://ex/name> ?o }",
            // DISTINCT count with a key.
            "SELECT ?o (COUNT(DISTINCT ?s) AS ?n) WHERE { ?s <http://ex/name> ?o } GROUP BY ?o",
            // Mixed count + value aggregate: SUM needs member values.
            "SELECT ?o (COUNT(*) AS ?n) (SUM(?age) AS ?t) WHERE { ?s <http://ex/name> ?o . ?s <http://ex/age> ?age } GROUP BY ?o",
            // COUNT over a var the BGP does not bind: must count 0, not solutions.
            "SELECT (COUNT(?z) AS ?n) WHERE { ?s <http://ex/name> ?o }",
            // Equality filter on the GROUP BY key itself: substitution would
            // erase the key column, so the streaming Group stays.
            "SELECT ?o (COUNT(*) AS ?c) WHERE { ?s <http://ex/name> ?o FILTER(?o = \"Alice\") } GROUP BY ?o",
        ];
        for q in cases {
            let plan = plan_select(q);
            let rewritten = rewrite(&plan).unwrap();
            assert!(
                !has_count_scan(&rewritten) && !has_group_count_scan(&rewritten),
                "scope guard failed — this must stay a Group:\n{q}\ngot {rewritten:#?}"
            );
            let with: Vec<Bindings> = Runtime::new(&horn).run(&plan).unwrap().collect();
            let without = run_raw(&horn, &plan);
            assert_eq!(canon(with), canon(without), "result parity broke for:\n{q}");
        }
```

- [ ] **Step 2: Run the new tests to verify the positives fail**

Run: `cargo nextest run -p horndb-sparql grouped_count`
Expected: `grouped_count_parity_battery` and `grouped_count_order_observable_under_limit` FAIL with `must lower to GroupCountScan`; `grouped_count_key_not_bound_by_bgp_stays_group` PASSES.

Run: `cargo nextest run -p horndb-sparql scope_guard`
Expected: PASS (all remaining guard cases hold before and after the change).

- [ ] **Step 3: Implement the grouped lowering**

In `crates/sparql/src/plan/pushdown.rs`, inside `lower_count_group`, replace the final lowering block (step 6 from Task 2, i.e. everything from the `// 6. Lower.` comment through the trailing `None`) with:

```rust
    // 6. Lower. The landed single implicit-group count keeps CountScan;
    //    every other qualifying shape becomes a GroupCountScan.
    let out_vars: Vec<Var> = aggregates.iter().map(|a| a.out.clone()).collect();
    if keys.is_empty() && out_vars.len() == 1 {
        let out_var = out_vars.into_iter().next().expect("len checked == 1");
        return Some(CountScan { patterns, out_var });
    }
    Some(GroupCountScan {
        patterns,
        keys: keys.to_vec(),
        out_vars,
    })
```

and update the function's doc comment (the two sentences about the next task) to:

```rust
/// Lowered shapes: `keys == []` + a single plain count →
/// [`PhysicalPlan::CountScan`] (landed with #144); any other combination of
/// keys and ≥1 plain counts → [`PhysicalPlan::GroupCountScan`]. Both are
/// reachable through an inlinable equality `FILTER`.
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run -p horndb-sparql pushdown`
Expected: PASS — the whole pushdown module: the new grouped tests, the updated scope guard, and every pre-existing test (`rewrite_is_result_invariant` covers `GROUP BY ?n` + `COUNT(DISTINCT *)` cases that now exercise both the lowered and guarded paths).

- [ ] **Step 5: Run the full crate suite**

Run: `cargo nextest run -p horndb-sparql`
Expected: PASS. Pay attention to `exec_aggregate` and the `slot_differential` proptests in `exec/runtime.rs` — they are the no-result-change gate for the rewrite.

- [ ] **Step 6: Commit**

```bash
git add crates/sparql/src/plan/pushdown.rs
git commit -m "$(cat <<'EOF'
feat(sparql): lower grouped and multi-count aggregations to GroupCountScan (#128)

Group { keys, aggregates } over a bare (or equality-filtered) BgpScan now
lowers to GroupCountScan whenever every aggregate is a plain non-DISTINCT
count of a BGP-bound var (or *): grouped counts, multi-count implicit
groups, and their filter-inlined compositions. Guards keep the streaming
Group for DISTINCT counts, mixed count+value aggregates, unbound keys/count
vars, and key-substituting filters. Parity battery asserts full Vec
equality (order matters under LIMIT) against the unrewritten runtime.

Design: docs/specs/2026-07-06-count-pushdown-extensions-design.md
EOF
)"
```

---

### Task 4: `HornBackend::count_bgp_grouped` fast path

The production win: count per group by hashing raw u64 WCOJ key columns — no `Row` construction, no term decode. Follows the same verbatim pattern-compilation convention as `scan_bgp` / `scan_bgp_ids` / `count_bgp` (each carries a `keep in sync` marker).

**Files:**
- Modify: `crates/sparql/src/exec/horn.rs:157-158` (imports), after `count_bgp` (line 943)
- Test: `crates/sparql/src/exec/horn.rs` (`mod tests`)

- [ ] **Step 1: Write the failing tests**

Append to `mod tests` in `crates/sparql/src/exec/horn.rs`:

```rust
    #[test]
    fn count_bgp_grouped_matches_scan_grouping() {
        use crate::algebra::TriplePattern;
        use crate::exec::{Executor, KeyPart, Slot};
        use std::collections::HashMap;
        let mut b = HornBackend::new();
        let iri = |s: &str| Term::Iri(format!("http://ex/{s}"));
        // cat0: two works, cat1: one.
        b.insert_triple(iri("w0"), iri("cat"), iri("cat0"));
        b.insert_triple(iri("w1"), iri("cat"), iri("cat0"));
        b.insert_triple(iri("w2"), iri("cat"), iri("cat1"));
        let var = |n: &str| Term::Var(Var::new(n));
        let patterns = vec![TriplePattern {
            subject: var("s"),
            predicate: iri("cat"),
            object: var("cat"),
        }];
        let keys = [Var::new("cat")];

        let fast = b
            .count_bgp_grouped(&patterns, &keys)
            .unwrap()
            .expect("HornBackend must provide a fast grouped count");

        // Oracle: group the id-rows scan_bgp_ids yields on the key column.
        let batch = b.scan_bgp_ids(&patterns).unwrap();
        let key_col = batch.col("cat").expect("?cat column");
        let mut want: HashMap<KeyPart, usize> = HashMap::new();
        for r in &batch.rows {
            *want.entry(r.0[key_col].key_part()).or_insert(0) += 1;
        }
        assert_eq!(fast.len(), want.len(), "one entry per group: {fast:?}");
        for (key_slots, n) in &fast {
            assert_eq!(key_slots.len(), 1);
            assert!(
                matches!(key_slots[0], Slot::Id(_)),
                "keys keep scan provenance (Slot::Id): {key_slots:?}"
            );
            assert_eq!(
                want.get(&key_slots[0].key_part()),
                Some(n),
                "count mismatch for {key_slots:?}"
            );
        }

        // A constant the dictionary has never seen: zero groups (matches the
        // empty scan), not None.
        let missing = vec![TriplePattern {
            subject: var("s"),
            predicate: iri("nope"),
            object: var("cat"),
        }];
        assert_eq!(b.count_bgp_grouped(&missing, &keys).unwrap(), Some(Vec::new()));
    }

    #[test]
    fn count_bgp_grouped_falls_back_on_diagonal_and_unbound_key() {
        use crate::algebra::TriplePattern;
        use crate::exec::Executor;
        let mut b = HornBackend::new();
        let iri = |s: &str| Term::Iri(format!("http://ex/{s}"));
        b.insert_triple(iri("x"), iri("p"), iri("x"));
        let var = |n: &str| Term::Var(Var::new(n));
        // A var repeated within one pattern needs the per-row diagonal
        // filter, which a key-column hash cannot apply: fall back (None).
        let diag = vec![TriplePattern {
            subject: var("v"),
            predicate: iri("p"),
            object: var("v"),
        }];
        assert!(b
            .count_bgp_grouped(&diag, &[Var::new("v")])
            .unwrap()
            .is_none());
        // A key the BGP does not bind has no WCOJ column: fall back (None).
        let plain = vec![TriplePattern {
            subject: var("s"),
            predicate: iri("p"),
            object: var("o"),
        }];
        assert!(b
            .count_bgp_grouped(&plain, &[Var::new("z")])
            .unwrap()
            .is_none());
    }
```

- [ ] **Step 2: Run the new tests to verify they fail**

Run: `cargo nextest run -p horndb-sparql count_bgp_grouped`
Expected: `count_bgp_grouped_matches_scan_grouping` FAILS with the panic `HornBackend must provide a fast grouped count` (the trait default returns `Ok(None)`); `count_bgp_grouped_falls_back_on_diagonal_and_unbound_key` PASSES (the default is trivially `None`).

- [ ] **Step 3: Implement the fast path**

In `crates/sparql/src/exec/horn.rs`, change the two import lines (157-158) from

```rust
use crate::algebra::TriplePattern;
use crate::exec::{Bindings, Executor, Store};
```

to

```rust
use crate::algebra::{TriplePattern, Var};
use crate::exec::{Bindings, Executor, Slot, Store};
```

and insert inside `impl Executor for HornBackend`, directly after `count_bgp` (line 943):

```rust
    /// Per-group BGP solution counts without decoding terms or building rows:
    /// hash the raw u64 key columns of the WCOJ batches. Same fallback cases
    /// as `count_bgp` (diagonal repeats), plus: an all-ground BGP or a key
    /// with no WCOJ column returns `Ok(None)` so the caller's scan-based
    /// fallback supplies the (identical) semantics. Empty `patterns`/`keys`
    /// are the caller's job (`GroupCountScanOp` routes no-key shapes through
    /// `count_bgp`).
    // keep in sync with scan_bgp_ids
    fn count_bgp_grouped(
        &self,
        patterns: &[TriplePattern],
        keys: &[Var],
    ) -> Result<Option<Vec<(Vec<Slot>, usize)>>> {
        if patterns.is_empty() || keys.is_empty() {
            return Ok(None);
        }

        let snapshot = self.wcoj_snapshot();
        let dict = self.store.dictionary();

        // === VERBATIM copy from scan_bgp: pattern compilation ===
        let mut var_index: HashMap<String, u8> = HashMap::new();
        let mut diagonal_filters: Vec<(String, String)> = Vec::new();
        let mut wpatterns: Vec<WPattern> = Vec::new();
        let mut ground: Vec<WTriple> = Vec::new();

        for pattern in patterns {
            let mut seen_here: HashSet<&str> = HashSet::new();
            let mut slots = [WTerm::Var(WVar(0)); 3];
            let mut all_bound = true;
            let slot_terms = [&pattern.subject, &pattern.predicate, &pattern.object];
            for (slot_no, term) in slot_terms.into_iter().enumerate() {
                slots[slot_no] = match term {
                    Term::Var(v) => {
                        all_bound = false;
                        let name = v.name();
                        let effective = if seen_here.contains(name) {
                            let alias = format!(" dup_{name}_{slot_no}");
                            diagonal_filters.push((name.to_owned(), alias.clone()));
                            alias
                        } else {
                            seen_here.insert(name);
                            name.to_owned()
                        };
                        let idx = match var_index.get(&effective) {
                            Some(&i) => i,
                            None => {
                                let next = var_index.len();
                                if next > u8::MAX as usize {
                                    return Err(SparqlError::Executor(
                                        "BGP exceeds 256 distinct variables".into(),
                                    ));
                                }
                                var_index.insert(effective, next as u8);
                                next as u8
                            }
                        };
                        WTerm::Var(WVar(idx))
                    }
                    constant => {
                        let ox = algebra_to_oxrdf(constant)?;
                        match dict.get(&ox) {
                            Some(id) => WTerm::Bound(id.0),
                            // Unknown constant: no stored triple can match —
                            // zero groups (parity with the empty scan).
                            None => return Ok(Some(Vec::new())),
                        }
                    }
                };
            }
            if all_bound {
                let ids: Vec<u64> = slots.iter().map(|t| t.as_bound().unwrap()).collect();
                ground.push(WTriple::new(ids[0], ids[1], ids[2]));
            } else {
                wpatterns.push(WPattern::new(slots[0], slots[1], slots[2]));
            }
        }

        if ground.iter().any(|t| !snapshot.contains(t)) {
            return Ok(Some(Vec::new()));
        }
        // === END verbatim copy ===

        // All patterns ground (unit relation) — no key columns exist here;
        // let the scan-based fallback supply the Unbound-key semantics.
        if wpatterns.is_empty() {
            return Ok(None);
        }
        // A within-pattern repeated variable needs the per-row diagonal
        // filter, which a key-column hash cannot apply. Fall back.
        if !diagonal_filters.is_empty() {
            return Ok(None);
        }
        // Resolve each key's WCOJ var index; a key the BGP does not bind has
        // no column (the rewrite guards this; stay defensive).
        let mut key_wvars: Vec<u8> = Vec::with_capacity(keys.len());
        for k in keys {
            match var_index.get(k.name()) {
                Some(&i) => key_wvars.push(i),
                None => return Ok(None),
            }
        }

        let bgp = WBgp::new(wpatterns);
        let mut counts: HashMap<Vec<u64>, usize> = HashMap::new();
        for batch in WcojExecutor::for_bgp(
            snapshot.as_ref(),
            &bgp,
            &Planner::default(),
            CancelToken::new(),
        ) {
            let batch = batch.map_err(|e| SparqlError::Executor(format!("wcoj: {e}")))?;
            let arrow_schema = batch.schema();
            let mut key_cols: Vec<&UInt64Array> = Vec::with_capacity(key_wvars.len());
            for idx in &key_wvars {
                let Some((col_idx, _)) = arrow_schema.column_with_name(&format!("v{idx}")) else {
                    // Executor produced no column for a key var — fall back
                    // wholesale rather than fabricate Unbound groups.
                    return Ok(None);
                };
                let arr = batch
                    .column(col_idx)
                    .as_any()
                    .downcast_ref::<UInt64Array>()
                    .ok_or_else(|| {
                        SparqlError::Executor(format!("wcoj batch column v{idx} is not UInt64"))
                    })?;
                key_cols.push(arr);
            }
            for r in 0..batch.num_rows() {
                let key: Vec<u64> = key_cols.iter().map(|c| c.value(r)).collect();
                *counts.entry(key).or_insert(0) += 1;
            }
        }
        Ok(Some(
            counts
                .into_iter()
                .map(|(ids, n)| {
                    (
                        ids.into_iter().map(|id| Slot::Id(TermId(id))).collect(),
                        n,
                    )
                })
                .collect(),
        ))
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run -p horndb-sparql count_bgp_grouped`
Expected: PASS — both tests.

- [ ] **Step 5: Run the full crate suite**

Run: `cargo nextest run -p horndb-sparql`
Expected: PASS — in particular `grouped_count_parity_battery` and `grouped_count_order_observable_under_limit` now execute through the fast seam on `HornBackend` and must still match the streaming baseline exactly.

- [ ] **Step 6: Commit**

```bash
git add crates/sparql/src/exec/horn.rs
git commit -m "$(cat <<'EOF'
feat(sparql): fast grouped BGP counts in HornBackend (#128)

count_bgp_grouped hashes the raw u64 WCOJ key columns per batch — no Row
construction, no term decode — mirroring count_bgp's verbatim pattern
compilation. Falls back to None on diagonal repeats, all-ground BGPs, and
keys without a WCOJ column; unknown constants and ground misses return
zero groups, matching the empty scan. Seam test checks parity against a
scan_bgp_ids-derived oracle.
EOF
)"
```

---

### Task 5: Crate docs, full gates, and smoke check

**Files:**
- Modify: `crates/sparql/INTEGRATION-NOTES.md` (append section at end, after the "EXPLAIN pragma" section)

- [ ] **Step 1: Document the seam in INTEGRATION-NOTES.md**

Append to `crates/sparql/INTEGRATION-NOTES.md`:

```markdown
## Count pushdown (#128: #144 first cut + 2026-07-06 extensions)

The pushdown pass (`plan/pushdown.rs::rewrite`) lowers count-only aggregation
shapes into scan-side count leaves so the runtime never materializes solution
rows for them:

- `COUNT(*)` / `COUNT(?bound-bgp-var)` over a bare BGP → `CountScan` +
  `Executor::count_bgp` (landed 2026-06-30).
- The same with an intervening `FILTER` that is a conjunction of
  `?v = <const>` / `sameTerm(?v, <const>)` equalities → the constants are
  substituted into the BGP first. Result-invariant because engine `Expr::Eq`
  is structural term equality over oxrdf-normalized forms, which coincides
  with the dictionary term identity BGP constants match by; if `Expr::Eq`
  ever gains numeric *value* semantics, the literal-constant case must be
  restricted to IRIs (pinned by `eq_filter_literal_term_identity_pin`).
- `GROUP BY` keys and/or multiple plain counts → `GroupCountScan` +
  `Executor::count_bgp_grouped`. `HornBackend` answers it by hashing the raw
  u64 WCOJ key columns (no `Row` build, no decode); other backends fall back
  to scan + hash-count on the key columns. Output rows sort by
  decoded-lexical key, byte-identical to `eval_group_native`'s order
  (observable under LIMIT).

Deferred with reasons (mixed count+value aggregates, `COUNT(DISTINCT …)`,
non-equality filters, partial inlining, zero-aggregate `GROUP BY`):
`docs/specs/2026-07-06-count-pushdown-extensions-design.md`.
```

- [ ] **Step 2: Format and lint**

Run: `cargo fmt --all`
Expected: exits 0 (then `git status` shows only intended files, if any reformats).

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: exits 0, no warnings. (First run after a fresh checkout builds `oxrocksdb-sys` and takes several minutes — that is expected, per root `CLAUDE.md`.)

- [ ] **Step 3: Full test gates**

Run: `cargo nextest run -p horndb-sparql`
Expected: PASS, 0 failures.

Run: `cargo nextest run -p horndb-sparql --features server`
Expected: PASS, 0 failures (required for the full SPARQL pass — the HTTP server tests execute the same runtime).

- [ ] **Step 4: agg_profile smoke check (local only, NOT recorded)**

Run: `cargo run -p horndb-sparql --release --example agg_profile -- 100000`
Expected: completes without error; Q2 ("GROUP BY cat COUNT") and Q3 ("join type+cat GROUP BY") report substantially higher qps than before this plan (Q1 is unchanged — it was already pushed down; Q4/Q5 are the deferred SUM / COUNT-DISTINCT shapes and stay put). Do **not** record these numbers anywhere: official aggregation-qps comes from the SPB-256 nightly on the hornbench host, and `BENCHMARKS.md` is updated by the coordinating session.

- [ ] **Step 5: Commit**

```bash
git add crates/sparql/INTEGRATION-NOTES.md
git commit -m "$(cat <<'EOF'
docs(sparql): record the count-pushdown seam contract in INTEGRATION-NOTES (#128)

Covers CountScan/count_bgp, the equality-filter inlining invariant, and
GroupCountScan/count_bgp_grouped ordering + fallback semantics; points at
docs/specs/2026-07-06-count-pushdown-extensions-design.md for the deferral
rationale.
EOF
)"
```

---

## Completion criteria (gates)

- `cargo nextest run -p horndb-sparql` — green.
- `cargo nextest run -p horndb-sparql --features server` — green.
- `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `cargo fmt --all -- --check` — clean.
- `agg_profile` local run — smoke only (no recorded numbers). Official aggregation-qps is measured by the SPB-256 nightly on hornbench and recorded in `BENCHMARKS.md` by the coordinating session, together with the `TASKS.md` / `docs/architecture.md` sync.
