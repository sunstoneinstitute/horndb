# id-based slot rows for the SPARQL runtime — Slice 2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. TDD per task: write/extend a failing test, make it green, commit.

**Goal:** Finish #128's id-based slot-row migration. Slice 1 ported BgpScan/Slice/Project/Distinct/Group/Filter/Join to native slots; the other six operators still ride the decode-adapter (`eval_rows` → today's string code → `Batch::from_bindings`). Slice 2 ports **LeftJoin, Union, OrderBy, Extend, Values, PathClosure** to native slots, then **removes the decode-adapter (`eval_rows`) and the `cfg(test)` `eval_legacy` oracle**, leaving one runtime over slot rows.

**Non-goal (still deferred under #128):** streaming (every node still buffers a whole `Batch`) and planner projection/aggregate pushdown. Those are independent follow-ups; do **not** start them here.

**Source of truth:** `docs/specs/2026-06-28-id-based-slot-rows-design.md` §5 ("Slice 2. Port the remaining six operators to native slots; remove the decode-adapter") and §6 (correctness invariants). Read it before starting. The Slice 1 plan (`docs/plans/2026-06-28-id-based-slot-rows-slice1.md`) is the companion — its `merge_rows`, `decode_subset`, `referenced_vars`, `normalize_join_columns`, and `KeyPart` helpers are the building blocks Slice 2 reuses.

**Tech Stack:** Rust 1.90 (pinned), `horndb-sparql` crate, `horndb-storage::TermId`, `proptest` (already a dev-dep), `cargo nextest`.

---

## Current state (verified against `crates/sparql/src/exec/runtime.rs` @ origin/main)

- `eval` returns `Batch`. Native arms: `BgpScan` (`scan_bgp_ids`), `Join` (`merge_rows` + `normalize_join_columns`), `Filter` (`referenced_vars` + `decode_subset` + `eval_expr`), `Project`, `Distinct` (`KeyPart`), `Slice`, `Group` (`eval_group_native`).
- **Adapter-backed arms (this slice's targets):**
  - `LeftJoin` → `Batch::from_bindings(hash_left_join(eval_rows(l), eval_rows(r), expr))`
  - `Union` → `Batch::from_bindings(eval_rows(l) ++ eval_rows(r))`
  - `OrderBy` → `from_bindings(sort eval_rows(inner) by compare_by_keys)`
  - `Extend` → `from_bindings(eval_rows(inner) with eval_expr_to_term appended)`
  - `Values` → builds `Bindings` rows then `from_bindings`
  - `PathClosure` → `from_bindings(eval_path_closure(subject, object, eval_rows(edge), reflexive))`
- `eval_rows` (runtime.rs:36) is the adapter input half: `self.eval(plan)?.to_bindings(...)`. Used **only** by the six arms above. Goes away in Task 8.
- `eval_legacy` (runtime.rs:1592, `#[cfg(test)]`) is the differential oracle; `slot_differential` proptests (runtime.rs:1887) compare `run()` against it. Removed in Task 8 per design §6.
- Dead-code helpers used **only** by `eval_legacy`: `eval_group` (718), `project` (1163). Become removable once `eval_legacy` goes. `hash_left_join`/`probe_into`/`join_vars`/`join_key` (433–521) are used by the `LeftJoin` adapter arm **and** `eval_legacy`; Task 6 stops using them in `eval`, Task 8 removes them with `eval_legacy`.
- Reused by native ports, must **stay**: `eval_expr`, `eval_expr_to_term`, `compare_by_keys`, `compare_terms`, `lex` (`pub(crate)`), `integer_literal`, `eval_aggregate`, `referenced_vars`, `decode_subset`, `merge_rows`, `KeyPart`.

---

## Assumptions / decisions (decided during planning — flag if you disagree)

1. **`normalize_join_columns` is generalized to `normalize_columns(rows, ncols)` and reused by `Union` and `LeftJoin`.** Slice 1 added it for `Join` to restore within-column homogeneity when an adapter child mixed `Slot::Id` (native BGP) with `Slot::Term`/`Unbound` (adapter). `Union` and native `LeftJoin` produce the *same* mixing (one branch native-Id, the other Term/Unbound), so the identical fix applies. Rename in place (keep behavior); update the `Join` call site. The pure-`Id` aggregation hot path still pays zero decode (the function only decodes genuinely mixed columns).

2. **Native `LeftJoin` mirrors `hash_left_join`'s structure on slots, not a decode round-trip.** It indexes the right `Batch` by a `Vec<KeyPart>` over the join vars, probes per left row, merges with `merge_rows`, and evaluates the inner `OPTIONAL` `FILTER` (`expr`) via `decode_subset` of the filter's `referenced_vars` over the merged row — exactly how native `Filter` already reuses `eval_expr`. Output columns are normalized (decision 1). `LeftJoin` is **not** on the SPB aggregation hot path, so the goal here is correctness + column homogeneity + adapter removal, not raw speed; the structure stays O(|l|+|r|) like today.

3. **Native `OrderBy` decodes — ids are not value-ordered (design §3, §6).** It decodes each row's order-key `referenced_vars` into a transient `Bindings` via `decode_subset`, then reuses `compare_by_keys` verbatim. Schema is unchanged and rows are only reordered, so no homogeneity concern. Decode is unavoidable and correct here.

4. **Native `Extend` (BIND) appends one uniformly-`Slot::Term` column.** Decode the expr's `referenced_vars` per row, compute via `eval_expr_to_term`; bound → `Slot::Term(t)`, unbound → `Slot::Unbound`. Schema grows by `var`. The new column is computed (never `Id`), so it is homogeneous by construction. If `var` already exists in the schema (re-BIND), overwrite that column in place rather than appending (match `Bindings::set` replace semantics — verify against `eval_legacy`'s `b.set`).

5. **Native `Values` builds a `Batch` of `Slot::Term`/`Slot::Unbound` directly.** Schema = `vars`; each cell is `Slot::Term(term)` when present else `Slot::Unbound`. Columns are uniformly `Term`-or-`Unbound` (homogeneous). No decode, no adapter.

6. **Native `PathClosure` keeps `eval_path_closure`'s BFS but on the decoded endpoint columns only.** The edge `Batch` binds exactly the two synthetic endpoint vars (`PATH_SRC_VAR`/`PATH_DST_VAR`). Decode just those two columns per edge row (`decode_subset`) to feed `eval_path_closure` unchanged, then re-encode its `Vec<Bindings>` output via `Batch::from_bindings`. This removes the *general* `eval_rows` adapter dependency while keeping the proven BFS. A fully id-native BFS (BFS over `TermId`s, decode only at endpoint binding) is a **deferred** optimization — note it in the architecture sync, do not build it here. PathClosure is not on the SPB hot path. `Batch::from_bindings` stays public (also used by the default `scan_bgp_ids`), so this is legitimate, not a leftover adapter.

7. **Expand the differential proptest *before* deleting the oracle (Task 7 precedes Task 8).** The design says delete `eval_legacy` when Slice 2 lands; the safe sequencing is to first broaden `slot_differential::QUERIES` to exercise all six newly-native operators against the still-present oracle (catches port bugs at their riskiest), bump case counts, then delete `eval_legacy` and the oracle-dependent proptests in Task 8. The permanent backstop after deletion is the byte-identical snapshot/result-format suite plus the concrete regression tests (e.g. `distinct_join_over_optional_no_column_mixing`), whose oracle comparison is replaced by the explicit expected-value asserts they already carry.

8. **No public API change.** `Runtime::run` already decodes at the boundary; `api.rs`, serializers, and the HTTP server stay untouched. The whole slice is internal to `runtime.rs` (+ the `normalize` rename). Acceptance is byte-identical suite green.

---

## File structure

- **Modify** `crates/sparql/src/exec/runtime.rs` — six native operator arms + supporting helpers; rename `normalize_join_columns`→`normalize_columns`; expand then remove the differential oracle; delete `eval_rows`, `eval_legacy`, `eval_group`, `project`, `hash_left_join`/`probe_into`/`join_vars`/`join_key`.
- **Modify** docs (final task): `TASKS.md`, `docs/architecture.md`, `BENCHMARKS.md`, GitHub issue #128.

Single-file change (plus docs). Every task commits independently and keeps `cargo nextest run -p horndb-sparql` (+ `--features server`) green.

**Dependency order:**

| Task | Depends on | Notes |
|---|---|---|
| 1 Generalize `normalize_columns` | — | Pure rename + call-site; no behavior change |
| 2 Native `Values` | 1 | Simplest port; warm-up |
| 3 Native `Extend` (BIND) | 1 | |
| 4 Native `OrderBy` | 1 | Decodes order keys |
| 5 Native `Union` | 1 | Uses `normalize_columns` |
| 6 Native `LeftJoin` (OPTIONAL) | 1 | Most complex; uses `normalize_columns` + `merge_rows` |
| 7 Native `PathClosure` | 1 | Endpoint-only decode |
| 8 Expand differential proptest | 2–7 | While `eval_legacy` still exists |
| 9 Remove adapter + oracle + dead helpers | 8 | `eval_rows`, `eval_legacy`, `eval_group`, `project`, `hash_left_join` family |
| 10 Measure + docs-sync | 9 | agg_profile/nightly note; architecture §9; TASKS #128; BENCHMARKS |

> Tasks 2–7 each touch only their own `eval` arm plus private helpers. If run by one worker, do them in order and commit between each. They all edit `runtime.rs`, so **do not** parallelize them in separate worktrees (guaranteed merge conflicts) — sequential commits on the one branch.

---

## Task 1: Generalize `normalize_join_columns` → `normalize_columns`

No behavior change — this is the shared homogeneity-restoration helper Union and LeftJoin will reuse.

- [ ] **Step 1:** Read `normalize_join_columns` (search `runtime.rs`). Rename it to `normalize_columns` (the body is already generic over "rows + column count"). Update its doc comment to say it restores within-column homogeneity for **any** operator that unions/merges children of differing slot provenance (Join, Union, LeftJoin), not just Join.
- [ ] **Step 2:** Update the `Join` arm call site (`self.normalize_join_columns(...)` → `self.normalize_columns(...)`).
- [ ] **Step 3:** `cargo nextest run -p horndb-sparql && cargo nextest run -p horndb-sparql --features server` → green (pure rename).
- [ ] **Step 4:** Commit:
  ```
  refactor(sparql): generalize normalize_join_columns → normalize_columns (#128)
  ```

---

## Task 2: Native `Values`

**Approach (decision 5):** build the `Batch` directly — schema `vars`, each cell `Slot::Term(term)` or `Slot::Unbound`.

- [ ] **Step 1 (test):** Add/extend a `runtime.rs` `#[cfg(test)]` test (or rely on existing `exec_*` VALUES tests — check `tests/` for VALUES coverage first; if thin, add one asserting a `VALUES (?x ?y){ (1 UNDEF)(2 3) }` query returns the right rows incl. the UNDEF→unbound). Run it; confirm it passes via the *current* adapter (baseline) so you can prove no regression after the port.
- [ ] **Step 2 (port):** Replace the `Values` arm:
  ```rust
  PhysicalPlan::Values { vars, rows } => {
      let schema: Vec<Var> = vars.clone();
      let out_rows = rows
          .iter()
          .map(|row| {
              Row(vars
                  .iter()
                  .zip(row.iter())
                  .map(|(_, cell)| match cell {
                      Some(t) => Slot::Term(t.clone()),
                      None => Slot::Unbound,
                  })
                  .collect())
          })
          .collect();
      Ok(Batch { schema, rows: out_rows })
  }
  ```
  > Verify `vars`/`row` shapes against the current arm (runtime.rs:176). If a `row` can be shorter than `vars`, pad missing trailing cells with `Slot::Unbound` (mirror the current `zip` — confirm it can't silently drop).
- [ ] **Step 3:** `cargo nextest run -p horndb-sparql && --features server` → green, byte-identical.
- [ ] **Step 4:** Commit: `perf(sparql): native slot Values (no adapter) (#128)`

---

## Task 3: Native `Extend` (BIND)

**Approach (decision 4):** decode only `referenced_vars(expr)` per row, compute via `eval_expr_to_term`, append (or overwrite) the `var` column as `Slot::Term`/`Slot::Unbound`.

- [ ] **Step 1 (test):** Confirm existing BIND coverage in `tests/exec_expressions.rs` (or similar); if a re-BIND-over-existing-var case is missing, add one. Baseline-green first.
- [ ] **Step 2 (port):** Replace the `Extend` arm:
  ```rust
  PhysicalPlan::Extend { inner, var, expr } => {
      let b = self.eval(inner)?;
      let mut want = HashSet::new();
      referenced_vars(expr, &mut want);
      let existing = b.col(var.name());            // Some(i) ⇒ re-BIND
      let mut schema = b.schema.clone();
      if existing.is_none() {
          schema.push(var.clone());
      }
      let mut out_rows = Vec::with_capacity(b.rows.len());
      for r in &b.rows {
          let env = self.decode_subset(r, &b.schema, &want)?;
          let slot = match eval_expr_to_term(expr, &env)? {
              Some(t) => Slot::Term(t),
              None => Slot::Unbound,
          };
          let mut slots = r.0.clone();
          match existing {
              Some(i) => slots[i] = slot,
              None => slots.push(slot),
          }
          out_rows.push(Row(slots));
      }
      Ok(Batch { schema, rows: out_rows })
  }
  ```
  > Cross-check re-BIND semantics against `eval_legacy`'s Extend arm (`b.set` replaces). If the planner guarantees `var` is always fresh (never re-bound), the `existing` branch is dead but harmless — keep it for safety and note it.
- [ ] **Step 3:** green (+ server), byte-identical.
- [ ] **Step 4:** Commit: `perf(sparql): native slot Extend/BIND (decode only referenced cols) (#128)`

---

## Task 4: Native `OrderBy`

**Approach (decision 3):** decode order-key referenced columns per row, reuse `compare_by_keys`. Schema unchanged.

- [ ] **Step 1 (test):** Baseline-green an ORDER BY test (check `tests/` — likely `exec_select`/snapshot covers ORDER BY; if a multi-key ASC/DESC + unbound-key case is thin, add one).
- [ ] **Step 2 (port):** Replace the `OrderBy` arm. Collect the union of `referenced_vars` across all order-key exprs once, decode each row's subset into a transient `Bindings`, then sort row indices (or `(env, row)` pairs) with `compare_by_keys`:
  ```rust
  PhysicalPlan::OrderBy { inner, keys } => {
      let b = self.eval(inner)?;
      let mut want = HashSet::new();
      for (e, _) in keys { referenced_vars(e, &mut want); }
      // Decode once per row, pair with the row, sort, drop the env.
      let mut tagged: Vec<(Bindings, Row)> = b.rows
          .into_iter()
          .map(|r| {
              let env = self.decode_subset(&r, &b.schema, &want)?;
              Ok((env, r))
          })
          .collect::<Result<Vec<_>>>()?;
      tagged.sort_by(|(ea, _), (eb, _)| compare_by_keys(ea, eb, keys));
      Ok(Batch { schema: b.schema, rows: tagged.into_iter().map(|(_, r)| r).collect() })
  }
  ```
  > `keys` is `&[(Expr, OrderDir)]` (confirmed at `compare_by_keys`, runtime.rs:1177). `sort_by` is stable, matching today's `Vec::sort_by` — preserves input order among equal keys (important for byte-identical output).
- [ ] **Step 3:** green (+ server), byte-identical (ORDER BY output is order-sensitive — this is the strict check).
- [ ] **Step 4:** Commit: `perf(sparql): native slot OrderBy (decode only order-key cols) (#128)`

---

## Task 5: Native `Union`

**Approach (decision 1):** union child schemas; place each child row's slots by var name, `Unbound` where absent; `normalize_columns` the result.

- [ ] **Step 1 (test):** Baseline-green a UNION test, including an **asymmetric** UNION (branches bind different vars) — the `slot_runtime_matches_legacy_joins` proptest already has `union_q`; ensure it passes before and after.
- [ ] **Step 2 (port):** Replace the `Union` arm:
  ```rust
  PhysicalPlan::Union { left, right } => {
      let l = self.eval(left)?;
      let r = self.eval(right)?;
      // Schema = left schema ++ right-only vars (deterministic order).
      let mut schema = l.schema.clone();
      for v in &r.schema {
          if !schema.iter().any(|x| x.name() == v.name()) {
              schema.push(v.clone());
          }
      }
      let place = |child: &Batch, schema: &[Var]| -> Vec<Row> {
          child.rows.iter().map(|row| {
              Row(schema.iter().map(|v| match child.col(v.name()) {
                  Some(i) => row.0[i].clone(),
                  None => Slot::Unbound,
              }).collect())
          }).collect()
      };
      let mut rows = place(&l, &schema);
      rows.extend(place(&r, &schema));
      self.normalize_columns(&mut rows, schema.len())?;
      Ok(Batch { schema, rows })
  }
  ```
  > `child.col` is `Batch::col` (returns slot index by var name; batch.rs). `normalize_columns` handles a column that holds `Id` from one branch and `Term`/`Unbound` from the other (decision 1).
- [ ] **Step 3:** green (+ server), byte-identical. Run the `slot_runtime_matches_legacy_joins` proptest specifically.
- [ ] **Step 4:** Commit: `perf(sparql): native slot Union (schema-aligned, normalized) (#128)`

---

## Task 6: Native `LeftJoin` (OPTIONAL)

**Approach (decision 2):** slot hash-left-join mirroring `hash_left_join`'s structure on `Vec<KeyPart>` keys + `merge_rows`, with the inner FILTER via `decode_subset`+`eval_expr`, output `normalize_columns`'d. The most error-prone task — lean on the differential test (Task 8 expands it; this task's regression test `distinct_join_over_optional_no_column_mixing` already exercises the column-mixing trap).

- [ ] **Step 1 (test):** Baseline-green `distinct_join_over_optional_no_column_mixing` and `slot_runtime_matches_legacy_joins` (`optional_q`). Both must stay green through the port — they are the trap detectors.
- [ ] **Step 2 (helpers):** Add a slot join-key over a `Batch`:
  - `fn batch_join_vars(l: &Batch, r: &Batch) -> Vec<Var>` — vars present in both schemas, deterministic (sort by name).
  - `fn row_join_key(row: &Row, schema: &[Var], jvars: &[Var]) -> Option<Vec<KeyPart>>` — `Some(keyparts)` if every join var is bound (non-`Unbound`) in the row, else `None` (conservative bucket, mirroring `hash_left_join`'s `join_key` returning `None`).
- [ ] **Step 3 (port):** Replace the `LeftJoin` arm with the slot analogue of `hash_left_join` (runtime.rs:433–501). Structure:
  1. `out_schema` = left schema ++ right-only vars.
  2. Index right rows by `row_join_key` into `HashMap<Vec<KeyPart>, Vec<&Row>>` + an `unkeyed: Vec<&Row>`.
  3. For each left row: probe its key's bucket (and `unkeyed`); for each right candidate, `merge_rows(&l.schema, a, &r.schema, b, &out_schema)?`; if `Some(m)`, apply the inner FILTER — `decode_subset(&m, &out_schema, &referenced_vars(expr))` then `eval_expr(expr, &env)?` — keep on true. Track `matched`.
  4. A left row with `None` key probes all right rows (conservative).
  5. Unmatched left rows are emitted padded to `out_schema` (left slots by name, `Unbound` elsewhere) — reuse the `place`-style projection from Task 5 or `merge_rows` against an all-`Unbound` right row.
  6. `self.normalize_columns(&mut rows, out_schema.len())?;`
  > Correctness anchors: `merge_rows` already implements the slot compatibility/merge rule (Unbound = wildcard; `Slot::eq` decodes only on genuine `Id`-vs-`Term` mix). The FILTER reuse mirrors native `Filter`. Keep it O(|l|+|r|) like `hash_left_join`; do not regress to the pre-#116 nested loop.
- [ ] **Step 4:** green (+ server), byte-identical. Run `exec_*` OPTIONAL tests + both join proptests.
- [ ] **Step 5:** Commit: `perf(sparql): native slot LeftJoin/OPTIONAL (hash probe, normalized) (#128)`

---

## Task 7: Native `PathClosure`

**Approach (decision 6):** decode only the two endpoint columns of the edge `Batch`, run `eval_path_closure` unchanged, re-encode via `from_bindings`.

- [ ] **Step 1 (test):** Baseline-green the property-path tests (search `tests/` for `p+`/`p*`/path closure; `exec_*paths*` or similar).
- [ ] **Step 2 (port):** Replace the `PathClosure` arm:
  ```rust
  PhysicalPlan::PathClosure { subject, object, edge, reflexive } => {
      let eb = self.eval(edge)?;
      // The edge batch binds exactly the two synthetic endpoint vars.
      let want: HashSet<String> =
          [PATH_SRC_VAR, PATH_DST_VAR].iter().map(|s| s.to_string()).collect();
      let edge_rows: Vec<Bindings> = eb.rows.iter()
          .map(|r| self.decode_subset(r, &eb.schema, &want))
          .collect::<Result<Vec<_>>>()?;
      Ok(Batch::from_bindings(eval_path_closure(subject, object, &edge_rows, *reflexive)?))
  }
  ```
  > Import `PATH_SRC_VAR`/`PATH_DST_VAR` from `crate::algebra` (already used inside `eval_path_closure`). `from_bindings` stays public (also backs the default `scan_bgp_ids`), so this is not a leftover adapter — it is the legitimate re-encode of a string-BFS result. Add a `// deferred: id-native BFS (#128)` comment.
- [ ] **Step 3:** green (+ server), byte-identical.
- [ ] **Step 4:** Commit: `perf(sparql): native slot PathClosure (endpoint-only decode) (#128)`

---

## Task 8: Expand the differential proptest (oracle still present)

Broaden coverage to the six now-native operators **before** deleting the oracle (decision 7).

- [ ] **Step 1:** In `slot_differential` (runtime.rs:1887), add query shapes to `QUERIES` (or a second const) exercising each ported operator over the generated graphs:
  - OPTIONAL: `SELECT ?s ?v WHERE { ?s ?p ?o OPTIONAL { ?s <http://ex/p1> ?v } }`
  - UNION (asymmetric): `SELECT ?x WHERE { { ?x ?p ?o } UNION { ?s ?p ?x } }`
  - ORDER BY + LIMIT: `SELECT ?s ?o WHERE { ?s ?p ?o } ORDER BY ?o ?s LIMIT 5`
  - BIND: `SELECT ?s ?b WHERE { ?s ?p ?o BIND(STR(?o) AS ?b) }`
  - VALUES: `SELECT ?p ?o WHERE { ?s ?p ?o VALUES ?p { <http://ex/p0> <http://ex/p1> } }`
  - (Property paths are awkward to generate meaningfully over the random graph; rely on the existing `tests/` path suite + the byte-identical snapshot gate for PathClosure rather than the proptest.)
  > These run through both `MemStore` (Slot::Term) and `HornBackend` (Slot::Id) against `eval_legacy`, the existing harness. Keep `prop_assert_eq!` on the sorted row sets. Bump `ProptestConfig::with_cases` to e.g. 128 for this final differential pass.
- [ ] **Step 2:** `cargo nextest run -p horndb-sparql slot_differential` → green. If a shape diverges, the bug is in the corresponding Task 2–7 port — fix there, not by weakening the test.
- [ ] **Step 3:** Commit: `test(sparql): expand slot/legacy differential to the six ported operators (#128)`

---

## Task 9: Remove the decode-adapter, the oracle, and dead helpers

Now there is exactly one runtime. Per design §6, delete `eval_legacy` (and what only it used).

- [ ] **Step 1:** Confirm no `eval` arm calls `eval_rows` any more (`grep -n eval_rows runtime.rs` → only the `fn eval_rows` definition). Delete `fn eval_rows`.
- [ ] **Step 2:** Delete `#[cfg(test)] eval_legacy` (runtime.rs:1591–1712) and the oracle-dependent proptests/tests in `slot_differential` that compare against it (`slot_runtime_matches_legacy`, `slot_runtime_matches_legacy_joins`, and the Task 8 additions). For the concrete regression test `distinct_join_over_optional_no_column_mixing`, **keep it** but drop its trailing `eval_legacy` comparison — its explicit `assert_eq!(got.len(), 1)` + `?v == <http://ex/X>` asserts are the permanent backstop (they don't need the oracle).
  > Rationale recorded in design §6: byte-identical snapshot/result-format suite + concrete regressions are the lasting net; maintaining a second full runtime is the cost the deletion removes.
- [ ] **Step 3:** Delete now-dead helpers used **only** by `eval_legacy`: `eval_group` (718), `project` (1163), `hash_left_join` (433), `probe_into` (481), `join_vars` (505), `join_key` (515). After deleting, `grep` each name to confirm zero remaining references. Remove their `#[allow(dead_code)]` neighbors. Check whether `_witness` (1586) is now removable too.
  > Do **not** delete `eval_expr`, `eval_expr_to_term`, `compare_by_keys`, `compare_terms`, `lex`, `integer_literal`, `eval_aggregate`, `referenced_vars`, `decode_subset`, `merge_rows`, `normalize_columns`, `Batch::from_bindings` — all reused by native ports or the boundary.
- [ ] **Step 4:** Full gate:
  ```
  cargo nextest run -p horndb-sparql
  cargo nextest run -p horndb-sparql --features server
  cargo clippy --workspace --all-targets -- -D warnings
  cargo build --workspace
  ```
  Expected: all green, no warnings, no dead-code escapes.
- [ ] **Step 5:** Commit:
  ```
  refactor(sparql): drop decode-adapter + eval_legacy oracle — one slot runtime (#128)

  All 13 operators now run native on slot rows; eval_rows, eval_legacy, and the
  helpers used only by the legacy oracle (eval_group, project, hash_left_join
  family) are removed. Concrete regression tests retained; byte-identical
  snapshot/result-format suite is the permanent backstop.
  ```

---

## Task 10: Measure + docs-sync

- [ ] **Step 1:** `agg_profile` directional smoke (laptop, *not* recorded): `cargo run -p horndb-sparql --release --example agg_profile 100000`. Slice 2 targets the non-aggregation operators, so Q1–Q5 should be **flat vs Slice 1** (no regression) — that's the expected result; the win was Slice 1's. Record this as "no aggregation regression; Slice 2 is correctness/consistency + adapter removal."
- [ ] **Step 2:** `BENCHMARKS.md` — update the `agg_profile` row note: Slice 2 landed (all operators native; adapter + oracle removed); aggregation numbers unchanged from Slice 1; the ~12× SPB gap remainder is owned by the still-deferred streaming + planner pushdown (state explicitly). The official nightly `aggregation-qps` continues to be tracked by the scheduled run.
- [ ] **Step 3:** `docs/architecture.md` §9 — flip the runtime row note: the six adapter-backed operators are now native; the decode-adapter and the `cfg(test)` legacy oracle are gone; the string `scan_bgp` remains only for DESCRIBE; streaming + planner pushdown remain the open #128 items.
- [ ] **Step 4:** `TASKS.md` (#128 task, line ~48) — check off the Slice 2 sub-bullet (remaining six operators ported; adapter removed). **Do not close #128** — streaming + planner projection/aggregate pushdown remain; re-scope the task body to exactly those two. Mirror the note to GitHub issue #128 per the `TASKS.md` header procedure (do not auto-close).
- [ ] **Step 5:** Commit:
  ```
  docs: record id-based slot rows Slice 2 (#128)

  All six remaining operators (LeftJoin/Union/OrderBy/Extend/Values/PathClosure)
  now native; decode-adapter and legacy oracle removed. #128 re-scoped to the
  remaining deferred work: streaming (no full per-node Batch) and planner
  projection/aggregate pushdown.
  ```

---

## Self-review notes (author)

- **Spec coverage (design §5):** six operator ports (T2–T7), decode-adapter removal (T9 `eval_rows`), `eval_legacy` deletion (T9, per §6), differential coverage expanded before deletion (T7, decision 7). Deferred streaming/pushdown explicitly out of scope and re-scoped in TASKS (T10).
- **Homogeneity invariant:** the column-mixing trap (Slice 1's `normalize_join_columns`) is generalized (T1) and applied to the two new mixing sites, Union (T5) and LeftJoin (T6). The `distinct_join_over_optional_no_column_mixing` regression is the live trap detector.
- **Reuse discipline:** every value-needing port reuses the existing string evaluator via `decode_subset`+`referenced_vars` (the seam Slice 1 established for Filter/Group) — no duplicate expression logic. OrderBy/MIN/MAX-style value ordering correctly always decodes (design §3).
- **Risk concentration:** LeftJoin (T6). Mitigated by keeping the oracle through T7 and the two concrete OPTIONAL regressions live across the port.
- **No API/serializer change:** boundary decode in `run` is unchanged; acceptance is byte-identical suite green + clippy + workspace build (T9 Step 4).
