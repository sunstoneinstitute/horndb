//! Streaming runtime over [`PhysicalPlan`]. `run` drives the pull-based
//! operator tree built by `build` and decodes slot ids once at the boundary
//! via `decode_term`. Every operator runs native on slot rows — there is a
//! single runtime (the test-only string oracle that gated the slot port was
//! removed once Slice 2 landed).

use crate::algebra::{
    AggFunc, Aggregate, DatasetSpec, Expr, Func, OrderDir, Term, Var, PATH_DST_VAR, PATH_SRC_VAR,
};
use crate::error::{Result, SparqlError};
use crate::exec::numeric::Numeric;
use crate::exec::phases;
use crate::exec::{Batch, Bindings, Executor, KeyPart, Row, ScanScope, Slot};
use crate::plan::{GraphScope, PhysicalPlan};
use crate::DefaultGraphMode;
use horndb_metrics::labels::ExecPhase;
use horndb_storage::TermId;
use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::HashSet;

pub struct Runtime<'a, E: Executor + ?Sized> {
    exec: &'a E,
    /// The query's `FROM`/`FROM NAMED` clause and default-graph mode. Every
    /// scan leaf pairs its own [`GraphScope`] with these to name the graphs
    /// it reads (SPEC-28 S3).
    dataset: DatasetSpec,
    mode: DefaultGraphMode,
}

impl<'a, E: Executor + ?Sized> Runtime<'a, E> {
    /// A runtime for a query with no dataset clause, under the default
    /// (`union`) default-graph mode. Callers that have a translated query
    /// should chain [`Self::with_dataset`].
    pub fn new(exec: &'a E) -> Self {
        Self {
            exec,
            dataset: DatasetSpec::default(),
            mode: DefaultGraphMode::default(),
        }
    }

    /// Attach the query's resolved dataset and default-graph mode.
    pub fn with_dataset(mut self, dataset: DatasetSpec, mode: DefaultGraphMode) -> Self {
        self.dataset = dataset;
        self.mode = mode;
        self
    }

    pub(crate) fn exec(&self) -> &'a E {
        self.exec
    }

    /// Pair a scan leaf's plan-level scope with this query's dataset.
    pub(crate) fn scan_scope<'s>(&'s self, graph: &'s GraphScope) -> ScanScope<'s> {
        ScanScope::new(graph, &self.dataset, self.mode)
    }

    // NOTE: `build` (the pull-based operator-tree constructor that `run` uses)
    // is defined in `crate::exec::op` alongside the `Op` trait.

    /// Execute the plan and return all solution mappings.
    pub fn run(&self, plan: &PhysicalPlan) -> Result<std::vec::IntoIter<Bindings>> {
        let mut stream = self.run_stream(plan)?;
        let mut out = Vec::new();
        while let Some(chunk) = stream.next_chunk()? {
            out.extend(chunk);
        }
        Ok(out.into_iter())
    }

    /// Execute the plan as a stream of decoded row chunks (#128 HTTP
    /// streaming): applies the pushdown rewrite, builds the operator tree,
    /// and hands back a lazy handle. `run` collects this; the HTTP layer
    /// serializes chunk-by-chunk without ever holding the full result.
    ///
    /// The stream borrows the `Runtime` (operators hold `&Runtime`
    /// internally), so keep the runtime binding alive:
    /// `let rt = Runtime::new(exec); let mut s = rt.run_stream(&plan)?;`
    pub fn run_stream<'r>(&'r self, plan: &PhysicalPlan) -> Result<BindingsStream<'r, E>>
    where
        E: 'r,
    {
        let plan = crate::plan::pushdown::rewrite(plan)?;
        // Same debug-only postcondition the planner asserts, re-checked after
        // the pushdown rewrite — it is the other pass that inserts a
        // narrowing `Project` (SPEC-28 S3/D6). Free in release.
        #[cfg(debug_assertions)]
        debug_assert!(
            crate::plan::lower::per_graph_columns_survive(&plan).is_ok(),
            "{:?}",
            crate::plan::lower::per_graph_columns_survive(&plan)
        );
        let op = self.build(&plan)?;
        Ok(BindingsStream {
            exec: self.exec,
            op,
            buf: Vec::new().into_iter(),
        })
    }

    /// Execute the plan WITHOUT the column-pruning rewrite. Used only in tests
    /// to provide a no-rewrite baseline for the result-invariance check in
    /// `crate::plan::pushdown`.
    #[cfg(test)]
    pub(crate) fn run_unpruned_for_test(&self, plan: &PhysicalPlan) -> Vec<Bindings> {
        let mut op = self
            .build(plan)
            .expect("build failed in run_unpruned_for_test");
        let mut out = Vec::new();
        while let Some(batch) = op.next().expect("op.next failed in run_unpruned_for_test") {
            out.extend(
                batch
                    .to_bindings(|id| self.exec.decode_term(id))
                    .expect("to_bindings failed in run_unpruned_for_test"),
            );
        }
        out
    }

    /// Keep the rows of `batch` for which `expr` evaluates true. Decodes only
    /// the referenced columns (`decode_subset`), preserving `Slot::Id` for the
    /// rest. Shared helper called by the streaming `FilterOp`.
    pub(crate) fn apply_filter(&self, batch: Batch, expr: &Expr) -> Result<Batch> {
        let mut want = HashSet::new();
        referenced_vars(expr, &mut want);
        // Pessimistic for a selective filter (frees the slack on return), but
        // avoids reallocs in the common high-pass-rate case. Filter is the one
        // streaming op that drops rows; transform-like ops (Project/Extend)
        // keep every row, so for those the same hint is exact, not pessimistic.
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

    /// Restrict `batch` to `vars` (in projection order), remapping each row's
    /// slots. Empty `vars` (SELECT * / ASK) returns the batch unchanged.
    /// Shared helper called by the streaming `ProjectOp`.
    pub(crate) fn apply_project(&self, batch: Batch, vars: &[Var]) -> Result<Batch> {
        if vars.is_empty() {
            // SELECT * / ASK: keep everything (parity with project()).
            return Ok(batch);
        }
        // New schema = projected vars that exist in the input, in
        // projection order; remap each row's slots by index.
        let idx: Vec<Option<usize>> = vars.iter().map(|v| batch.col(v.name())).collect();
        let schema: Vec<Var> = vars
            .iter()
            .zip(&idx)
            .filter(|(_, i)| i.is_some())
            .map(|(v, _)| v.clone())
            .collect();
        let rows = batch
            .rows
            .iter()
            .map(|r| {
                Row(idx
                    .iter()
                    .filter_map(|i| i.map(|i| r.0[i].clone()))
                    .collect())
            })
            .collect();
        Ok(Batch { schema, rows })
    }

    /// Evaluate `expr` per row and bind the result to `var` (BIND). Appends a
    /// new column when `var` is new, overwrites it when already present.
    /// Shared helper called by the streaming `ExtendOp`.
    ///
    /// re-BIND semantics: SPARQL 1.1 §18.1.10 forbids BIND targeting a var
    /// already in scope; spargebra enforces this at parse time so the `Some`
    /// branch is dead code kept for safety. An unbound expr result maps to
    /// `Slot::Unbound` — Extend never drops rows.
    pub(crate) fn apply_extend(&self, batch: Batch, var: &Var, expr: &Expr) -> Result<Batch> {
        let mut want = HashSet::new();
        referenced_vars(expr, &mut want);
        let existing = batch.col(var.name()); // Some(i) ⇒ re-BIND (dead code)
        let mut schema = batch.schema.clone();
        if existing.is_none() {
            schema.push(var.clone());
        }
        let mut out_rows = Vec::with_capacity(batch.rows.len());
        for r in &batch.rows {
            let env = self.decode_subset(r, &batch.schema, &want)?;
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
        Ok(Batch {
            schema,
            rows: out_rows,
        })
    }

    /// Decode just the named columns of a slot row into a `Bindings`, for
    /// reusing the string expression/aggregate evaluator verbatim.
    fn decode_subset(&self, row: &Row, schema: &[Var], want: &HashSet<String>) -> Result<Bindings> {
        let mut b = Bindings::new();
        for (i, v) in schema.iter().enumerate() {
            if !want.contains(v.name()) {
                continue;
            }
            match &row.0[i] {
                Slot::Id(id) => b.set(v.name().to_owned(), self.exec.decode_term(*id)?),
                Slot::Term(t) => b.set(v.name().to_owned(), t.clone()),
                Slot::Unbound => {}
            }
        }
        Ok(b)
    }

    /// Output schema of a GROUP BY: the grouping keys followed by each
    /// aggregate's output var. Must match `eval_group_native`'s output batch
    /// schema exactly (and `eval_group_native` uses this to stay in sync).
    pub(crate) fn group_output_schema(&self, keys: &[Var], aggregates: &[Aggregate]) -> Vec<Var> {
        let mut schema: Vec<Var> = keys.to_vec();
        for agg in aggregates {
            schema.push(agg.out.clone());
        }
        schema
    }

    /// Sort `batch` by `keys` (ORDER BY). Output schema = input schema (sort is
    /// schema-preserving). Shared by `OrderByOp`.
    ///
    /// Decorate-sort-undecorate (HDB-101): each row's sort value is resolved
    /// once, up front, into a [`SortCol`]; the comparator then only compares
    /// already-resolved values. The previous comparator re-evaluated the key
    /// expression and re-parsed its lexical form on both sides of every
    /// comparison, so an n-row sort paid O(n log n) decodes for what is O(n)
    /// work.
    pub(crate) fn compute_order_by(
        &self,
        batch: Batch,
        keys: &[(Expr, OrderDir)],
    ) -> Result<Batch> {
        // Pull schema and rows apart so the borrow checker sees two
        // independent moves (no partial-move ambiguity on `batch`).
        let schema = batch.schema;
        let mut rows = batch.rows;
        let n = rows.len() as u64;
        // Decorate + sort as one "sort" phase (HDB-99): the decode pays for
        // `ORDER BY`'s expressions, so it belongs with the sort it feeds,
        // not with `group_decode`/`agg_fold` (which decode for aggregates).
        let order = phases::timed(ExecPhase::Sort, n, || -> Result<Vec<usize>> {
            let cols = self.sort_columns(&rows, &schema, keys)?;
            Ok(sorted_order(&cols, rows.len()))
        })?;
        Ok(Batch {
            schema,
            rows: permute(&mut rows, &order),
        })
    }

    /// `ORDER BY` fused with the `LIMIT`/`OFFSET` directly above it
    /// (HDB-101): the first `n` rows of the full sort, without sorting the
    /// rest. The caller's `SliceOp` still applies `OFFSET` and `LIMIT`, so
    /// `n` is `offset + limit` — never just `limit`.
    ///
    /// Row-for-row identical to `compute_order_by` truncated to `n`. The
    /// bounded heap keeps the `n` smallest `(key, input position)` pairs, and
    /// the input-position tie-break reproduces the stable sort's tie order
    /// exactly. It runs only when every sort column is a strict total order
    /// (see [`SortCol::is_total_order`]); otherwise this falls back to the
    /// full sort, because "the n smallest" is not well defined under an
    /// inconsistent comparator and the two paths could then disagree.
    pub(crate) fn compute_top_k(
        &self,
        batch: Batch,
        keys: &[(Expr, OrderDir)],
        n: usize,
    ) -> Result<Batch> {
        let schema = batch.schema;
        let mut rows = batch.rows;
        // `LIMIT 0` (valid SPARQL) reaches here as `n == 0`. Answer it without
        // resolving a single sort key — no ordering of an empty answer is
        // observable. This is also what keeps `top_k_order`'s `heap[0]` in
        // range; `SliceOp` happens to short-circuit `remaining == Some(0)`
        // before it ever pulls this operator, but that is a guarantee in
        // another file and nothing ties the two together.
        if n == 0 {
            return Ok(Batch {
                schema,
                rows: Vec::new(),
            });
        }
        let row_count = rows.len() as u64;
        let order = phases::timed(ExecPhase::Sort, row_count, || -> Result<Vec<usize>> {
            let cols = self.sort_columns(&rows, &schema, keys)?;
            if n >= rows.len() || !cols.iter().all(|(c, _)| c.is_total_order()) {
                let mut order = sorted_order(&cols, rows.len());
                order.truncate(n);
                return Ok(order);
            }
            Ok(top_k_order(&cols, rows.len(), n))
        })?;
        Ok(Batch {
            schema,
            rows: permute(&mut rows, &order),
        })
    }

    /// Resolve every `ORDER BY` key into one [`SortCol`] over `rows` — the
    /// "decorate" step. A key that is a bare batch-column variable reads
    /// straight off the slots; only computed keys need a decoded `Bindings`
    /// per row, and only for the variables they actually reference.
    fn sort_columns(
        &self,
        rows: &[Row],
        schema: &[Var],
        keys: &[(Expr, OrderDir)],
    ) -> Result<Vec<(SortCol, OrderDir)>> {
        let mut want: HashSet<String> = HashSet::new();
        for (e, _) in keys {
            if bare_var_col(e, schema).is_none() {
                referenced_vars(e, &mut want);
            }
        }
        let envs: Vec<Bindings> = if want.is_empty() {
            Vec::new()
        } else {
            rows.iter()
                .map(|r| self.decode_subset(r, schema, &want))
                .collect::<Result<Vec<_>>>()?
        };

        keys.iter()
            .map(|(e, dir)| {
                let col = match bare_var_col(e, schema) {
                    Some(idx) => self.slot_sort_col(rows, idx)?,
                    None => SortCol::classify(
                        envs.iter()
                            .map(|env| {
                                eval_expr_to_term(e, env)
                                    .ok()
                                    .flatten()
                                    .map(|t| SortVal::of(&t))
                            })
                            .collect(),
                    ),
                };
                Ok((col, *dir))
            })
            .collect()
    }

    /// The sort column for a bare-variable key, read straight off column
    /// `idx` of the batch slots.
    ///
    /// Tries the all-numeric shape first through
    /// [`Executor::decode_numeric`] (HDB-100's seam: reads the dictionary's
    /// stored value in place — no `Term` clone, no N-Triples round trip).
    /// The first bound slot that is not a number abandons that attempt for
    /// the general column, whose id decodes are batched through
    /// [`Executor::decode_terms`] (one dictionary lock for the whole column).
    ///
    /// Order-equivalence with the general path: `decode_numeric` yields
    /// `Some` exactly where `numeric_value(&decode_term(id))` does, and with
    /// the same value, so an all-`Some` column is one where `compare_terms`
    /// would take its numeric branch for every pair — which is what
    /// [`SortCol::Num`] means.
    fn slot_sort_col(&self, rows: &[Row], idx: usize) -> Result<SortCol> {
        let mut nums: Vec<Option<f64>> = Vec::with_capacity(rows.len());
        let mut all_numeric = true;
        for r in rows {
            let slot = &r.0[idx];
            let v = match slot {
                Slot::Unbound => None,
                Slot::Id(id) => self.exec.decode_numeric(*id)?,
                Slot::Term(t) => numeric_value(t),
            };
            if v.is_none() && !matches!(slot, Slot::Unbound) {
                all_numeric = false;
                break;
            }
            nums.push(v);
        }
        if all_numeric {
            return Ok(SortCol::Num(nums));
        }

        let ids: Vec<TermId> = rows
            .iter()
            .filter_map(|r| match &r.0[idx] {
                Slot::Id(id) => Some(*id),
                _ => None,
            })
            .collect();
        let decoded = self.exec.decode_terms(&ids)?;
        let mut next = 0usize;
        let vals: Vec<Option<SortVal>> = rows
            .iter()
            .map(|r| match &r.0[idx] {
                Slot::Unbound => None,
                Slot::Id(_) => {
                    let v = SortVal::of(&decoded[next]);
                    next += 1;
                    Some(v)
                }
                Slot::Term(t) => Some(SortVal::of(t)),
            })
            .collect();
        Ok(SortCol::classify(vals))
    }

    /// Evaluate the transitive closure of the edge relation: decodes the edge
    /// batch's endpoint vars (the two synthetic `?pp_*` vars) and delegates to
    /// `eval_path_closure`. Shared by `PathClosureOp`.
    pub(crate) fn compute_path_closure(
        &self,
        edge_batch: Batch,
        subject: &Term,
        object: &Term,
        reflexive: bool,
    ) -> Result<Batch> {
        let want: HashSet<String> = [PATH_SRC_VAR, PATH_DST_VAR]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let edge_rows: Vec<Bindings> = edge_batch
            .rows
            .iter()
            .map(|r| self.decode_subset(r, &edge_batch.schema, &want))
            .collect::<Result<Vec<_>>>()?;
        Ok(Batch::from_bindings(eval_path_closure(
            subject, object, &edge_rows, reflexive,
        )?))
    }

    pub(crate) fn eval_group_native(
        &self,
        b: Batch,
        keys: &[Var],
        aggregates: &[Aggregate],
    ) -> Result<Batch> {
        let key_idx: Vec<Option<usize>> = keys.iter().map(|k| b.col(k.name())).collect();

        struct Grp {
            key_slots: Vec<Slot>,
            members: Vec<Row>,
        }

        // HDB-100 Do-3 "narrow": a single-column GROUP BY over an id-keyed
        // column (the common case — grouping by a scanned column, not a
        // computed one) keys the group map on a raw `u64`/`Option<u64>`
        // instead of allocating a `Vec<KeyPart>` per row. Id-vs-Term mixing
        // within one column cannot happen (join/union operators normalize it
        // away before `Group` ever sees the batch — see `exec::batch`'s
        // within-column homogeneity docs), so peeking any one *bound* row is
        // enough to know every bound row's kind; `Unbound` may still appear
        // in the same column (e.g. grouping on an `OPTIONAL` variable) and is
        // handled as its own bucket (`None`), not folded into the peek.
        let scalar_col: Option<usize> = if keys.len() == 1 {
            key_idx[0].filter(|&idx| {
                matches!(
                    b.rows.iter().find_map(|r| match &r.0[idx] {
                        Slot::Unbound => None,
                        other => Some(other),
                    }),
                    Some(Slot::Id(_))
                )
            })
        } else {
            None
        };

        let group_key_rows = b.rows.len() as u64;
        // A modest, always-safe reserve that skips the first several hashmap
        // growth doublings without guessing at the eventual group count. We
        // deliberately do NOT size this from `Executor::cardinality_estimate`
        // (HDB-100 brief): that estimates the *input row* count of the
        // underlying BGP, not the number of distinct *groups* — for exactly
        // the aggregation shape this task targets (q2/q4 fold ~1.3M rows into
        // 50 groups), reserving to the row estimate would over-allocate by
        // four orders of magnitude and pay for it in allocation + zeroing
        // cost on every query, hurting the very queries this task speeds up.
        // No group/NDV cardinality estimator exists in this codebase to give
        // a better number, so we cap the hint instead of guessing high.
        let reserve_hint = (group_key_rows as usize).min(1024);

        let mut groups: Vec<Grp> =
            phases::timed(ExecPhase::GroupKey, group_key_rows, || match scalar_col {
                Some(idx) => {
                    let mut map: FxHashMap<Option<u64>, Grp> = FxHashMap::default();
                    map.reserve(reserve_hint);
                    for r in b.rows {
                        let k = match &r.0[idx] {
                            Slot::Id(id) => Some(id.0),
                            Slot::Unbound => None,
                            Slot::Term(_) => unreachable!(
                                "within-column homogeneity: a column peeked as \
                                 Slot::Id cannot also hold Slot::Term"
                            ),
                        };
                        map.entry(k)
                            .or_insert_with(|| Grp {
                                key_slots: vec![match k {
                                    Some(id) => Slot::Id(TermId(id)),
                                    None => Slot::Unbound,
                                }],
                                members: Vec::new(),
                            })
                            .members
                            .push(r);
                    }
                    map.into_values().collect()
                }
                None => {
                    let mut map: FxHashMap<Vec<KeyPart>, Grp> = FxHashMap::default();
                    map.reserve(reserve_hint);
                    for r in b.rows {
                        let gkey: Vec<KeyPart> = key_idx
                            .iter()
                            .map(|i| i.map(|i| r.0[i].key_part()).unwrap_or(KeyPart::Unbound))
                            .collect();
                        let entry = map.entry(gkey).or_insert_with(|| Grp {
                            key_slots: key_idx
                                .iter()
                                .map(|i| i.map(|i| r.0[i].clone()).unwrap_or(Slot::Unbound))
                                .collect(),
                            members: Vec::new(),
                        });
                        entry.members.push(r);
                    }
                    map.into_values().collect()
                }
            });

        // Implicit grouping with no input rows still yields one empty group
        // (SPARQL §11.2: COUNT(*) of nothing is one row with 0).
        if keys.is_empty() && groups.is_empty() {
            groups.push(Grp {
                key_slots: Vec::new(),
                members: Vec::new(),
            });
        }

        // Output schema = keys ++ aggregate output vars (via group_output_schema
        // so the two cannot drift apart).
        let schema = self.group_output_schema(keys, aggregates);

        // HDB-100 Do-2: which aggregates can fold straight off raw slots —
        // no `Bindings` decode at all — because their inner expression is a
        // bare scan-column variable. `None` means the general
        // `eval_aggregate` path over decoded members is still needed for
        // that aggregate (a computed expression, GROUP_CONCAT, SAMPLE, or a
        // DISTINCT SUM/AVG/MIN/MAX, none of which this task's fast paths
        // cover). `CountStar` is always `None` here — it already has its own
        // no-decode branch below and never reaches `detect_fast_agg`.
        let fast_aggs: Vec<Option<FastAgg>> = aggregates
            .iter()
            .map(|agg| {
                if matches!(agg.func, AggFunc::CountStar) {
                    None
                } else {
                    detect_fast_agg(agg, &b.schema)
                }
            })
            .collect();

        // The union of input columns referenced by aggregates that still need
        // the general decode path. Aggregates with a fast path never touch
        // this — decoding it is exactly the per-row string round trip
        // HDB-100 exists to skip. `eval_aggregate` reads only its own
        // inner-expression vars, so extra keys in the shared `Bindings` are
        // inert; the `CountStar` arms never take the decode path.
        let mut union_want: HashSet<String> = HashSet::new();
        for (agg, fast) in aggregates.iter().zip(&fast_aggs) {
            if fast.is_none() {
                for e in agg_inner_exprs(agg) {
                    referenced_vars(e, &mut union_want);
                }
            }
        }
        // COUNT(*) / COUNT(DISTINCT *) are answered without decoding; a
        // fast-pathed aggregate is answered off raw slots; any other
        // aggregate needs the decoded members.
        let needs_decode = aggregates
            .iter()
            .zip(&fast_aggs)
            .any(|(agg, fast)| !matches!(agg.func, AggFunc::CountStar) && fast.is_none());

        let mut out: Vec<(Vec<Option<String>>, Row)> = Vec::with_capacity(groups.len());
        for grp in groups {
            let Grp { key_slots, members } = grp;
            let member_rows = members.len() as u64;

            // HDB-90-style empty interval: one Instant::now() pair with no
            // work between them, so the per-group instrumentation's own cost
            // is visible and subtractable from `group_decode`/`agg_fold`.
            if let Some(t0) = phases::enabled().then(std::time::Instant::now) {
                phases::add(ExecPhase::Clock, t0.elapsed().as_nanos() as u64, 1);
            }

            // Sort key + shared column decode as one "group_decode" phase
            // (HDB-99): decoded lexical of each group key slot, reproducing
            // the pre-#128 BTreeMap<Vec<Option<String>>> lexical ordering
            // exactly (None < Some(...) in BTreeMap order is the same as
            // Option<String> Ord ordering used in sort_by), plus the union of
            // referenced columns every decoding aggregate shares — empty,
            // and so free, when every aggregate has a fast path (HDB-100).
            // Computed before `key_slots` is moved into `slots`, which lets
            // us avoid cloning it.
            let (sort_key, members_decoded): (Vec<Option<String>>, Vec<Bindings>) =
                phases::timed(ExecPhase::GroupDecode, member_rows, || -> Result<_> {
                    let sort_key: Vec<Option<String>> = key_slots
                        .iter()
                        .map(|s| match s {
                            Slot::Unbound => Ok(None),
                            Slot::Id(id) => self.exec.decode_term(*id).map(|t| Some(lex(&t))),
                            Slot::Term(t) => Ok(Some(lex(t))),
                        })
                        .collect::<Result<Vec<_>>>()?;
                    let members_decoded: Vec<Bindings> = if needs_decode {
                        members
                            .iter()
                            .map(|r| self.decode_subset(r, &b.schema, &union_want))
                            .collect::<Result<Vec<_>>>()?
                    } else {
                        Vec::new()
                    };
                    Ok((sort_key, members_decoded))
                })?;

            let mut slots: Vec<Slot> = key_slots;
            phases::timed(ExecPhase::AggFold, member_rows, || -> Result<()> {
                for (i, agg) in aggregates.iter().enumerate() {
                    let value = if matches!(agg.func, AggFunc::CountStar) && !agg.distinct {
                        // COUNT(*) fast path: member count needs no decode at all.
                        Some(integer_literal(members.len() as i64))
                    } else if matches!(agg.func, AggFunc::CountStar) {
                        // COUNT(DISTINCT *): distinct whole-solution rows.
                        // agg_inner_exprs returns empty for CountStar, so the
                        // decoded union would yield empty Bindings for every row —
                        // all deduped to 1, wrong count. Instead, key on
                        // Vec<KeyPart> directly: within-column homogeneity ensures
                        // same-value cells hash identically, giving the same
                        // deduplication as the old `HashSet<&Bindings>` path without
                        // any decode.
                        let distinct: FxHashSet<Vec<KeyPart>> = members
                            .iter()
                            .map(|r| r.0.iter().map(|s| s.key_part()).collect())
                            .collect();
                        Some(integer_literal(distinct.len() as i64))
                    } else if let Some(fast) = &fast_aggs[i] {
                        // HDB-100 Do-2: COUNT/COUNT(DISTINCT)/SUM/AVG/MIN/MAX
                        // over a bare scan-column var, folded off raw slots.
                        self.eval_fast_agg(fast, &members)?
                    } else {
                        eval_aggregate(agg, &members_decoded)?
                    };
                    match value {
                        Some(t) => slots.push(Slot::Term(t)),
                        None => slots.push(Slot::Unbound),
                    }
                }
                Ok(())
            })?;

            // HDB-109: freeing a group's member rows is bulk per-row work
            // (one heap free per `Row`'s slot `Vec`), and the implicit
            // end-of-iteration drop left it outside every named phase. Made
            // explicit so it is attributed rather than landing in `residual`.
            // One clock per group, never per row (SPEC-17 §5.3).
            phases::timed(ExecPhase::RowDrop, member_rows, || {
                drop(members);
                drop(members_decoded);
            });

            out.push((sort_key, Row(slots)));
        }
        let out_rows = out.len() as u64;
        phases::timed(ExecPhase::Sort, out_rows, || {
            out.sort_by(|a, b| a.0.cmp(&b.0));
        });

        Ok(Batch {
            schema,
            rows: out.into_iter().map(|(_, r)| r).collect(),
        })
    }

    /// One member slot's numeric value for a fast-path aggregate fold
    /// (HDB-100 Do-2): `Slot::Id` reads through [`Executor::decode_numeric`]
    /// (skips the `Term` clone + N-Triples round trip), `Slot::Term` calls
    /// [`numeric_value`] directly, `Slot::Unbound` contributes nothing —
    /// matching `eval_aggregate`'s `collect_values`, which likewise drops
    /// unbound members before any numeric coercion.
    fn fast_numeric(&self, slot: &Slot) -> Result<Option<f64>> {
        match slot {
            Slot::Unbound => Ok(None),
            Slot::Id(id) => self.exec.decode_numeric(*id),
            Slot::Term(t) => Ok(numeric_value(t)),
        }
    }

    /// One member slot's *typed* numeric value, for `SUM`/`AVG` — which need
    /// the `xsd` type to promote correctly, not just an `f64`.
    ///
    /// An inline-int id carries its value in the id itself (SPEC-21), so the
    /// `xsd:integer` case stays what it was: pure arithmetic on the `TermId`,
    /// no dictionary lock and no decode at all. Every other id decodes.
    fn fast_numeric_typed(&self, slot: &Slot) -> Result<Option<Numeric>> {
        match slot {
            Slot::Unbound => Ok(None),
            Slot::Id(id) => match id.as_inline_int() {
                Some(v) => Ok(Some(Numeric::from_i64(i64::from(v)))),
                None => Ok(numeric_of(&self.exec.decode_term(*id)?)),
            },
            Slot::Term(t) => Ok(numeric_of(t)),
        }
    }

    /// Fold column `col` of a group into `(sum, bound-member count)`, shared
    /// by the `SUM` and `AVG` fast paths. `None` is the expression error: a
    /// bound member that is not a numeric literal, or an overflow — the same
    /// rule [`numeric_sum`] applies on the general path.
    fn fast_sum(&self, col: usize, members: &[Row]) -> Result<Option<(Numeric, usize)>> {
        let mut sum = Numeric::zero();
        let mut n = 0usize;
        for r in members {
            let slot = &r.0[col];
            if matches!(slot, Slot::Unbound) {
                continue;
            }
            let Some(v) = self.fast_numeric_typed(slot)? else {
                return Ok(None);
            };
            let Some(next) = sum.add(v) else {
                return Ok(None);
            };
            sum = next;
            n += 1;
        }
        Ok(Some((sum, n)))
    }

    /// Full term decode of one member slot. Used only on the rare path where
    /// a fast MIN/MAX cannot stay numeric (`eval_fast_extreme`'s fallback)
    /// and by the fast path's own winner recovery, so the returned `Term` is
    /// byte-identical to what `decode_subset` + `eval_expr_to_term` would
    /// have produced for the same slot.
    fn decode_slot_term(&self, slot: &Slot) -> Result<Term> {
        match slot {
            Slot::Id(id) => self.exec.decode_term(*id),
            Slot::Term(t) => Ok(t.clone()),
            Slot::Unbound => Err(SparqlError::Executor(
                "decode_slot_term called on an Unbound slot".into(),
            )),
        }
    }

    /// Evaluate one column-bound fast aggregate over a group's raw member
    /// rows (HDB-100 Do-2). `COUNT`/`COUNT(DISTINCT)` never decode at all;
    /// `SUM`/`AVG` fold through [`Executor::decode_numeric`]; `MIN`/`MAX`
    /// delegate to [`Self::eval_fast_extreme`], which stays numeric-only
    /// when it safely can and otherwise falls back to full decode.
    fn eval_fast_agg(&self, fast: &FastAgg, members: &[Row]) -> Result<Option<Term>> {
        Ok(match fast {
            FastAgg::Count(col) => {
                let n = members
                    .iter()
                    .filter(|r| !matches!(r.0[*col], Slot::Unbound))
                    .count();
                Some(integer_literal(n as i64))
            }
            FastAgg::CountDistinct(col) => {
                // Raw KeyPart identity, not a decoded Term set: TermIds are
                // term identity (SPEC-21), so this dedupes exactly like
                // `dedup_terms` over the decoded values, without decoding.
                let set: FxHashSet<KeyPart> = members
                    .iter()
                    .filter_map(|r| match &r.0[*col] {
                        Slot::Unbound => None,
                        s => Some(s.key_part()),
                    })
                    .collect();
                Some(integer_literal(set.len() as i64))
            }
            FastAgg::Sum(col) => self.fast_sum(*col, members)?.map(|(sum, _)| sum.to_term()),
            FastAgg::Avg(col) => match self.fast_sum(*col, members)? {
                // AVG of the empty multiset is 0 (SPARQL 1.1 §18.5.1.4).
                Some((_, 0)) => Some(integer_literal(0)),
                Some((sum, n)) => sum.div(Numeric::from_i64(n as i64)).map(Numeric::to_term),
                None => None,
            },
            FastAgg::Min(col) => self.eval_fast_extreme(*col, members, true)?,
            FastAgg::Max(col) => self.eval_fast_extreme(*col, members, false)?,
        })
    }

    /// MIN (`min == true`) / MAX over one column's bound members. Stays
    /// numeric-only (one [`Executor::decode_numeric`] call per bound member,
    /// then one [`Self::decode_slot_term`] call for just the winner) as long
    /// as every bound member parses as a number — the common case (e.g.
    /// `xsd:double` amounts). The winner is the *original* term, matching
    /// `aggregate_extreme`'s `vals[best_idx].clone()` exactly, not a
    /// recomputed numeric literal, and ties keep the first occurrence in
    /// member order, same as `aggregate_extreme`'s linear scan.
    ///
    /// The rare time a bound member is not numeric, falls back to decoding
    /// every bound member's term (batched through [`Executor::decode_terms`]
    /// when the column is `Slot::Id`, one dictionary lock instead of one per
    /// member) and running [`aggregate_extreme`] itself, so the lexical-order
    /// rule for mixed-type columns matches the general path exactly.
    fn eval_fast_extreme(&self, col: usize, members: &[Row], min: bool) -> Result<Option<Term>> {
        let mut best_idx: Option<usize> = None;
        let mut best_val = 0.0_f64;
        let mut any_bound = false;
        let mut all_numeric = true;
        for (i, r) in members.iter().enumerate() {
            let slot = &r.0[col];
            if matches!(slot, Slot::Unbound) {
                continue;
            }
            any_bound = true;
            match self.fast_numeric(slot)? {
                Some(v) => {
                    if best_idx.is_none() || (min && v < best_val) || (!min && v > best_val) {
                        best_idx = Some(i);
                        best_val = v;
                    }
                }
                None => {
                    all_numeric = false;
                    break;
                }
            }
        }
        if !any_bound {
            return Ok(None);
        }
        if all_numeric {
            let winner = best_idx.expect("any_bound + all_numeric implies a winner was set");
            return Ok(Some(self.decode_slot_term(&members[winner].0[col])?));
        }

        // Rare mixed-type fallback: matches `aggregate_extreme`'s lexical
        // rule exactly. Within-column homogeneity means the bound slots here
        // are either all `Id` or all `Term` (see `exec::batch` docs), so the
        // `Id` case batches through one dictionary lock via `decode_terms`
        // instead of one `decode_term` per member.
        let bound_slots: Vec<&Slot> = members
            .iter()
            .map(|r| &r.0[col])
            .filter(|s| !matches!(s, Slot::Unbound))
            .collect();
        let vals: Vec<Term> = if let Some(Slot::Id(_)) = bound_slots.first() {
            let ids: Vec<TermId> = bound_slots
                .iter()
                .map(|s| match s {
                    Slot::Id(id) => *id,
                    _ => unreachable!("within-column homogeneity checked above"),
                })
                .collect();
            self.exec.decode_terms(&ids)?
        } else {
            bound_slots
                .iter()
                .map(|s| match s {
                    Slot::Term(t) => t.clone(),
                    _ => unreachable!("within-column homogeneity checked above"),
                })
                .collect()
        };
        Ok(aggregate_extreme(&vals, min))
    }

    /// Merged UNION schema: left schema followed by right-only vars. Also the
    /// output schema of `Join`/`LeftJoin` (shared by `UnionOp`/`JoinOp`/`LeftJoinOp`).
    pub(crate) fn union_schema(&self, left: &[Var], right: &[Var]) -> Vec<Var> {
        let mut s = left.to_vec();
        for v in right {
            if !s.iter().any(|x| x.name() == v.name()) {
                s.push(v.clone());
            }
        }
        s
    }

    /// Remap one child batch into `merged` schema order, placing `Slot::Unbound`
    /// for vars absent from the child. Does NOT call `normalize_columns` —
    /// normalization must run over the fully combined row set (left + right
    /// concatenated) because a column that is all-Id in one child and all-Term
    /// in the other looks homogeneous per-child but is mixed in the union.
    /// Callers are responsible for calling `normalize_columns` after combining
    /// both children's output.
    pub(crate) fn apply_union_chunk(&self, chunk: Batch, merged: &[Var]) -> Result<Vec<Row>> {
        let Batch { schema, rows } = chunk;
        let out = rows
            .into_iter()
            .map(|row| {
                Row(merged
                    .iter()
                    .map(|v| match schema.iter().position(|c| c.name() == v.name()) {
                        Some(i) => row.0[i].clone(),
                        None => Slot::Unbound,
                    })
                    .collect())
            })
            .collect();
        Ok(out)
    }

    /// Decode Id cells in columns that mix Slot::Id and Slot::Term, restoring
    /// the within-column homogeneity invariant. Now used only by `UnionOp`,
    /// which drains both children before normalizing; the streaming joins use
    /// `force_term_columns` instead (they never see their whole output).
    pub(crate) fn normalize_columns(&self, rows: &mut [Row], width: usize) -> Result<()> {
        for c in 0..width {
            let mut has_id = false;
            let mut has_term = false;
            for row in rows.iter() {
                match &row.0[c] {
                    Slot::Id(_) => has_id = true,
                    Slot::Term(_) => has_term = true,
                    Slot::Unbound => {}
                }
                if has_id && has_term {
                    break;
                }
            }
            if has_id && has_term {
                for row in rows.iter_mut() {
                    if let Slot::Id(id) = row.0[c] {
                        row.0[c] = Slot::Term(self.exec.decode_term(id)?);
                    }
                }
            }
        }
        Ok(())
    }

    /// Merge two slot rows if compatible (shared vars equal by the slot
    /// rule), producing the union row, with a precomputed `merge_plan`: for
    /// each output column, its `(left_col, right_col)` index in the
    /// respective schemas (`None` when the var is absent on that side).
    /// Returns None if any shared bound var disagrees. Mirrors
    /// `Bindings::extend_compat` on slots: an `Unbound` slot is treated as an
    /// absent var (a wildcard that never conflicts), matching how `Bindings`
    /// simply lacks an unbound key. See [`build_merge_plan`].
    fn merge_rows_with(
        &self,
        l: &Row,
        r: &Row,
        merge_plan: &[(Option<usize>, Option<usize>)],
    ) -> Result<Option<Row>> {
        let decode = |id| self.exec.decode_term(id);
        let mut slots = Vec::with_capacity(merge_plan.len());
        for &(li, ri) in merge_plan {
            let chosen = match (li.map(|i| &l.0[i]), ri.map(|i| &r.0[i])) {
                (Some(a), Some(b)) => match (a, b) {
                    (Slot::Unbound, x) | (x, Slot::Unbound) => x.clone(),
                    _ => {
                        if Slot::eq(a, b, decode)? {
                            a.clone()
                        } else {
                            return Ok(None);
                        }
                    }
                },
                (Some(a), None) => a.clone(),
                (None, Some(b)) => b.clone(),
                (None, None) => Slot::Unbound,
            };
            slots.push(chosen);
        }
        Ok(Some(Row(slots)))
    }

    /// Build the hash-join index key for one slot row.
    ///
    /// Provenance choice — **canonicalize each join variable to its dictionary
    /// id.** A left BGP row keys `?x` as `Slot::Id(5)` while a right row may key
    /// the same logical `?x` as `Slot::Term(...)`. `Slot::key_part()` would map
    /// those to `KeyPart::Id(5)` vs `KeyPart::Lex(...)` — *different* hash
    /// buckets — and a valid match would be missed. So `Slot::Id` keys on its
    /// raw id directly (no decode) and `Slot::Term` is encoded back to its id
    /// via [`Executor::encode_term`] when the dictionary holds it,
    /// `KeyPart::Lex` otherwise. Equal values then share a bucket regardless of
    /// provenance: a stored value is `KeyPart::Id` on both sides; a value with
    /// no stored id can only appear as a `Term` and keys `KeyPart::Lex` on both
    /// sides. This replaces the previous decode-both-sides-to-string key, which
    /// paid one `decode_term` + `String` alloc per jvar per build+probe row.
    ///
    /// Only the (few) jvar columns are keyed; every non-jvar column stays
    /// native `Slot::Id` and is normalized only if the merge genuinely mixes Id
    /// and Term.
    ///
    /// Returns `None` if any jvar is `Unbound` in this row (such a row can't be
    /// keyed and takes the conservative `unkeyed` path). The `Result` wrapper is
    /// kept for signature stability with the merge/probe call sites; keying no
    /// longer decodes, so it never errors.
    fn row_join_key(
        &self,
        row: &Row,
        schema: &[Var],
        jvars: &[Var],
    ) -> Result<Option<Vec<KeyPart>>> {
        let mut key = Vec::with_capacity(jvars.len());
        for jv in jvars {
            // jvars ⊆ schema by construction (bound_join_vars), so this is
            // always Some; treat a missing column conservatively as unkeyed.
            let Some(i) = schema.iter().position(|v| v.name() == jv.name()) else {
                return Ok(None);
            };
            match &row.0[i] {
                Slot::Unbound => return Ok(None),
                Slot::Id(id) => key.push(KeyPart::Id(id.0)),
                Slot::Term(t) => key.push(match self.exec.encode_term(t) {
                    Some(id) => KeyPart::Id(id.0),
                    None => KeyPart::Lex(lex(t)),
                }),
            }
        }
        Ok(Some(key))
    }

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

        // Index the build rows by canonicalized join key (see
        // `row_join_key`); rows with an unbound jvar fall to `unkeyed`.
        let mut index: FxHashMap<Vec<KeyPart>, Vec<usize>> = FxHashMap::default();
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

    /// Probe one left-side chunk against the build state (left-outer join /
    /// OPTIONAL). `expr` is the OPTIONAL's inner FILTER, applied per merged
    /// row; `want` is its referenced-vars set (constant per operator, so the
    /// caller computes it once). A probe row with no surviving candidate is
    /// emitted with the build-side-only columns `Unbound`. Matched/unmatched
    /// is decided per probe row against the complete build state, so OPTIONAL
    /// semantics are chunk-independent. Forced columns are decoded before
    /// returning.
    pub(crate) fn probe_left_join_chunk(
        &self,
        st: &JoinState,
        chunk: &Batch,
        expr: Option<&Expr>,
        want: &HashSet<String>,
    ) -> Result<Vec<Row>> {
        let mut out = Vec::new();
        // OPTIONAL pad for unmatched probe rows (allocated once per chunk).
        let unbound = Row(vec![Slot::Unbound; st.build.schema.len()]);
        for a in &chunk.rows {
            let mut matched = false;
            match self.row_join_key(a, &chunk.schema, &st.jvars)? {
                Some(k) => {
                    if let Some(bucket) = st.index.get(&k) {
                        matched |= self.probe_into_indexed(a, st, bucket, expr, want, &mut out)?;
                    }
                    if !st.unkeyed.is_empty() {
                        matched |=
                            self.probe_into_indexed(a, st, &st.unkeyed, expr, want, &mut out)?;
                    }
                }
                // Probe row with an unbound jvar: may match any build row.
                None => {
                    let all: Vec<usize> = (0..st.build.rows.len()).collect();
                    matched |= self.probe_into_indexed(a, st, &all, expr, want, &mut out)?;
                }
            }
            if !matched {
                // OPTIONAL: the probe row survives with build-only vars
                // unbound (merging with an all-Unbound build row takes the
                // probe side and leaves build-only vars Unbound).
                if let Some(m) = self.merge_rows_with(a, &unbound, &st.merge_plan)? {
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

    /// Whether MINUS right-side row `b` disqualifies left-side row `a`
    /// (SPARQL 1.1 §18.5): true iff `a` and `b` are compatible on every
    /// column they share (`Slot::Unbound` is a wildcard, as in
    /// `merge_rows_with`) AND at least one shared column is actually bound
    /// on both sides. That second clause is the domain-intersection guard —
    /// without it, a `left`/`right` pair that shares no bound variable would
    /// count as "compatible" (vacuously) and wrongly disqualify every `left`
    /// row. `shared_cols` is the schema-level shared-column list (from
    /// `JoinState::merge_plan`), not `JoinState::jvars` (a hash-keying
    /// subset) — this check must see every shared variable, not just the
    /// ones selected for indexing.
    fn minus_disqualifies(&self, a: &Row, b: &Row, shared_cols: &[(usize, usize)]) -> Result<bool> {
        let decode = |id| self.exec.decode_term(id);
        let mut shares_bound_var = false;
        for &(li, ri) in shared_cols {
            match (&a.0[li], &b.0[ri]) {
                (Slot::Unbound, _) | (_, Slot::Unbound) => {}
                (x, y) => {
                    if !Slot::eq(x, y, decode)? {
                        return Ok(false);
                    }
                    shares_bound_var = true;
                }
            }
        }
        Ok(shares_bound_var)
    }

    /// `true` iff any of `st.build.rows[candidates]` disqualifies `a` (see
    /// `minus_disqualifies`).
    fn any_minus_disqualifies(
        &self,
        a: &Row,
        st: &JoinState,
        candidates: &[usize],
        shared_cols: &[(usize, usize)],
    ) -> Result<bool> {
        for &i in candidates {
            if self.minus_disqualifies(a, &st.build.rows[i], shared_cols)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Probe one left-side chunk against the build state (MINUS, §18.5): a
    /// survives unchanged unless some build row disqualifies it (see
    /// `minus_disqualifies`). Unlike `probe_join_chunk`/
    /// `probe_left_join_chunk`, output rows are `a.clone()` — `right`'s
    /// columns never surface, so there is no merge and no forced-column
    /// decode.
    pub(crate) fn probe_minus_chunk(&self, st: &JoinState, chunk: &Batch) -> Result<Vec<Row>> {
        let shared_cols: Vec<(usize, usize)> = st
            .merge_plan
            .iter()
            .filter_map(|&(li, ri)| li.zip(ri))
            .collect();
        // §18.5 domain-intersection: `left` and `right` share no variable,
        // so every `right` row's domain is disjoint from every `left` row's
        // — nothing is ever excluded. Skips the build-side scan entirely
        // (the naive "compatible ⇒ drop" bug this whole function exists to
        // avoid would, wrongly, drop everything here instead).
        if shared_cols.is_empty() {
            return Ok(chunk.rows.clone());
        }
        let mut out = Vec::with_capacity(chunk.rows.len());
        for a in &chunk.rows {
            let disqualified = match self.row_join_key(a, &chunk.schema, &st.jvars)? {
                Some(k) => {
                    let mut d = false;
                    if let Some(bucket) = st.index.get(&k) {
                        d = self.any_minus_disqualifies(a, st, bucket, &shared_cols)?;
                    }
                    if !d && !st.unkeyed.is_empty() {
                        d = self.any_minus_disqualifies(a, st, &st.unkeyed, &shared_cols)?;
                    }
                    d
                }
                None => {
                    let all: Vec<usize> = (0..st.build.rows.len()).collect();
                    self.any_minus_disqualifies(a, st, &all, &shared_cols)?
                }
            };
            if !disqualified {
                out.push(a.clone());
            }
        }
        Ok(out)
    }
}

/// Hash-join build state shared by the streaming `JoinOp`/`LeftJoinOp`
/// (#128): the fully-drained build side (right child) plus everything
/// derived from it. Built once on the operator's first `next()`; immutable
/// while the probe side streams. The index stores row *indices* into
/// `build.rows` (not `&Row`) so the state can own the batch it indexes.
pub(crate) struct JoinState {
    build: Batch,
    index: FxHashMap<Vec<KeyPart>, Vec<usize>>,
    unkeyed: Vec<usize>,
    jvars: Vec<Var>,
    out_schema: Vec<Var>,
    merge_plan: Vec<(Option<usize>, Option<usize>)>,
    /// Per-output-column: decode `Slot::Id → Slot::Term` on emit (see
    /// `force_term_columns` and the design doc §3).
    forced_term: Vec<bool>,
}

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
/// Correctness is unaffected: `merge_rows_with` still checks
/// every shared variable per candidate pair, and an unbound variable is
/// compatible with anything (SPARQL §18.3), so key selection only shapes the
/// candidate buckets, never the match set. A *partially* bound variable
/// stays in the key (its unbound rows go to `unkeyed`, which is semantically
/// forced). An empty build side yields an empty key set: every row keys to
/// `Some(vec![])` — one bucket, the cross-compatibility scan the semantics
/// require.
fn bound_join_vars(left_schema: &[Var], build: &Batch) -> Vec<Var> {
    let lvars: std::collections::BTreeSet<&str> = left_schema.iter().map(|v| v.name()).collect();
    let mut out: Vec<Var> = build
        .schema
        .iter()
        .enumerate()
        .filter(|(i, v)| {
            lvars.contains(v.name()) && build.rows.iter().any(|r| !matches!(r.0[*i], Slot::Unbound))
        })
        .map(|(_, v)| v.clone())
        .collect();
    out.sort_by(|a, b| a.name().cmp(b.name()));
    out
}

/// Streaming query handle returned by [`Runtime::run_stream`]: pulls one
/// operator `Batch` at a time and decodes `Slot::Id → Term` at the boundary,
/// chunk-by-chunk instead of all-at-once.
pub struct BindingsStream<'r, E: Executor + ?Sized> {
    exec: &'r E,
    op: Box<dyn crate::exec::op::Op + 'r>,
    /// Rows pulled by `next_chunk` but not yet handed out by the row-wise
    /// `Iterator` view. `next_chunk` drains this first, so mixing the two
    /// access styles never loses or reorders rows.
    buf: std::vec::IntoIter<Bindings>,
}

impl<'r, E: Executor + ?Sized> BindingsStream<'r, E> {
    /// Decoded rows of the next operator chunk (≤ `batch_rows()` rows), or
    /// `None` at end of stream. Chunks are never empty (`Op` invariant:
    /// operators never yield `Some(empty)` mid-stream).
    pub fn next_chunk(&mut self) -> Result<Option<Vec<Bindings>>> {
        let buffered: Vec<Bindings> = self.buf.by_ref().collect();
        if !buffered.is_empty() {
            return Ok(Some(buffered));
        }
        match self.op.next()? {
            Some(batch) => {
                let n = batch.rows.len() as u64;
                Ok(Some(phases::timed(ExecPhase::ResultEncode, n, || {
                    batch.to_bindings(|id| self.exec.decode_term(id))
                })?))
            }
            None => Ok(None),
        }
    }
}

/// Row-at-a-time convenience view (ASK, library callers).
impl<'r, E: Executor + ?Sized> Iterator for BindingsStream<'r, E> {
    type Item = Result<Bindings>;
    fn next(&mut self) -> Option<Result<Bindings>> {
        loop {
            if let Some(b) = self.buf.next() {
                return Some(Ok(b));
            }
            match self.next_chunk() {
                Ok(Some(rows)) => self.buf = rows.into_iter(),
                Ok(None) => return None,
                Err(e) => return Some(Err(e)),
            }
        }
    }
}

/// Precompute, once per join, each `out_schema` column's source index in the
/// left schema (`Option<usize>`) and the right schema (`Option<usize>`). These
/// positions depend only on `(ls, rs, out_schema)`, not on the row pair, so
/// hoisting them out of [`Runtime::merge_rows_with`]'s per-pair loop turns the
/// O(width²) per-merged-row `position` scan into O(width) indexing.
fn build_merge_plan(
    ls: &[Var],
    rs: &[Var],
    out_schema: &[Var],
) -> Vec<(Option<usize>, Option<usize>)> {
    out_schema
        .iter()
        .map(|v| {
            let li = ls.iter().position(|x| x.name() == v.name());
            let ri = rs.iter().position(|x| x.name() == v.name());
            (li, ri)
        })
        .collect()
}

/// Evaluate a recursive Kleene path `p+`/`p*`.
///
/// `edge_rows` are the one-step relation `p` denotes, each row binding
/// the hidden endpoint variables [`PATH_SRC_VAR`]/[`PATH_DST_VAR`].
/// We take the transitive closure of that relation by BFS to a fixpoint
/// (a `seen` set per source guarantees termination on cyclic data), and
/// — for `*` — add the reflexive pairs over every node the relation
/// touches. The resulting `(src, dst)` pairs are matched against the
/// query endpoints `subject`/`object`, each of which may be ground
/// (filter) or a variable (bind).
///
/// Stage-1 reflexive note: `p*`'s zero-length match is seeded only over
/// nodes that appear in the path relation (plus a ground endpoint, if
/// pinned), not over every node in the active graph. This matches the
/// documented approximation in [`crate::algebra::translate`]'s
/// `zero_length_path`; full graph-node enumeration for `*` is deferred.
fn eval_path_closure(
    subject: &Term,
    object: &Term,
    edge_rows: &[Bindings],
    reflexive: bool,
) -> Result<Vec<Bindings>> {
    use crate::algebra::{PATH_DST_VAR, PATH_SRC_VAR};
    use std::collections::{BTreeMap, BTreeSet, VecDeque};

    // The hidden endpoint variables are stored in `Bindings` under their
    // full names (the `?pp_*` sigil is part of the stored variable name,
    // since these are user-unspellable synthetic vars).
    let src_key = PATH_SRC_VAR;
    let dst_key = PATH_DST_VAR;

    // Adjacency over the lexical forms of the endpoint terms. We key on
    // the term's serialised form (`lex`) to dedupe, and keep a
    // representative `Term` for each node so we can rebuild bindings.
    let mut adj: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut node_term: BTreeMap<String, Term> = BTreeMap::new();
    for row in edge_rows {
        let (Some(s), Some(o)) = (row.get(src_key), row.get(dst_key)) else {
            continue;
        };
        let (sk, ok) = (lex(s), lex(o));
        node_term.entry(sk.clone()).or_insert_with(|| s.clone());
        node_term.entry(ok.clone()).or_insert_with(|| o.clone());
        adj.entry(sk).or_default().insert(ok);
    }

    // Transitive closure: for each source, BFS over `adj`. Pairs are
    // keyed by lexical form; `closure` holds `(src_key, dst_key)`.
    let mut closure: BTreeSet<(String, String)> = BTreeSet::new();
    let sources: Vec<String> = adj.keys().cloned().collect();
    for start in sources {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut queue: VecDeque<String> = VecDeque::new();
        if let Some(nbrs) = adj.get(&start) {
            for n in nbrs {
                if seen.insert(n.clone()) {
                    queue.push_back(n.clone());
                }
            }
        }
        while let Some(cur) = queue.pop_front() {
            closure.insert((start.clone(), cur.clone()));
            if let Some(nbrs) = adj.get(&cur) {
                for n in nbrs {
                    if seen.insert(n.clone()) {
                        queue.push_back(n.clone());
                    }
                }
            }
        }
    }

    // `*` adds the reflexive pairs over every node the relation touches.
    if reflexive {
        for k in node_term.keys() {
            closure.insert((k.clone(), k.clone()));
        }
        // A ground endpoint pinned to a node absent from the relation
        // still self-matches under the zero-length branch.
        for ep in [subject, object] {
            if !matches!(ep, Term::Var(_)) {
                let k = lex(ep);
                node_term.entry(k.clone()).or_insert_with(|| ep.clone());
                closure.insert((k.clone(), k));
            }
        }
    }

    // Bind/filter each closure pair against the query endpoints.
    let mut out = Vec::new();
    for (sk, ok) in &closure {
        let s_term = node_term.get(sk).cloned().unwrap();
        let o_term = node_term.get(ok).cloned().unwrap();
        let mut b = Bindings::new();
        if !bind_endpoint(subject, &s_term, &mut b) {
            continue;
        }
        if !bind_endpoint(object, &o_term, &mut b) {
            continue;
        }
        out.push(b);
    }
    Ok(out)
}

/// Match a closure endpoint against a query endpoint term, recording any
/// variable binding into `b`. Returns `false` if a ground query endpoint
/// does not equal the closure node (the pair is filtered out).
///
/// A repeated variable across both endpoints (e.g. `?x p+ ?x`) is handled
/// by `Bindings::set` overwriting with the same value only when the two
/// nodes agree — we guard that explicitly so an inconsistent pair is
/// dropped rather than silently keeping the second binding.
fn bind_endpoint(endpoint: &Term, node: &Term, b: &mut Bindings) -> bool {
    match endpoint {
        Term::Var(v) => {
            if let Some(existing) = b.get(v.name()) {
                return existing == node;
            }
            b.set(v.name().to_owned(), node.clone());
            true
        }
        ground => lex(ground) == lex(node),
    }
}

/// Collect the variable names an expression reads, so a slot operator can
/// decode only those columns into a transient `Bindings`.
pub(crate) fn referenced_vars(e: &Expr, out: &mut HashSet<String>) {
    match e {
        Expr::Term(Term::Var(v)) => {
            out.insert(v.name().to_owned());
        }
        Expr::Term(_) => {}
        Expr::Bound(v) => {
            out.insert(v.name().to_owned());
        }
        Expr::Eq(a, b)
        | Expr::SameTerm(a, b)
        | Expr::Ne(a, b)
        | Expr::Lt(a, b)
        | Expr::Gt(a, b)
        | Expr::Le(a, b)
        | Expr::Ge(a, b)
        | Expr::And(a, b)
        | Expr::Or(a, b)
        | Expr::Add(a, b)
        | Expr::Sub(a, b)
        | Expr::Mul(a, b)
        | Expr::Div(a, b) => {
            referenced_vars(a, out);
            referenced_vars(b, out);
        }
        Expr::Not(a) | Expr::Neg(a) => referenced_vars(a, out),
        Expr::If(a, b, c) => {
            referenced_vars(a, out);
            referenced_vars(b, out);
            referenced_vars(c, out);
        }
        Expr::In(a, list) => {
            referenced_vars(a, out);
            for x in list {
                referenced_vars(x, out);
            }
        }
        Expr::Coalesce(args) | Expr::Func(_, args) => {
            for x in args {
                referenced_vars(x, out);
            }
        }
    }
}

/// An aggregate `eval_group_native` can fold directly off raw slots, with no
/// `Bindings` decode (HDB-100 Do-2). Detected only for a bare scan-column
/// inner expression (`Expr::Term(Term::Var(_))`, see [`detect_fast_agg`]);
/// `SUM`/`AVG`/`MIN`/`MAX` further require `!agg.distinct` — the DISTINCT
/// variants keep the general decode path.
enum FastAgg {
    /// `COUNT(?v)` — count of bound (non-`Unbound`) slots.
    Count(usize),
    /// `COUNT(DISTINCT ?v)` — count of distinct `KeyPart`s among bound slots.
    CountDistinct(usize),
    Sum(usize),
    Avg(usize),
    Min(usize),
    Max(usize),
}

/// The batch column `e` resolves to when it is exactly a bare variable
/// (`?v`, not a computed expression) that names one of `schema`'s columns.
fn bare_var_col(e: &Expr, schema: &[Var]) -> Option<usize> {
    match e {
        Expr::Term(Term::Var(v)) => schema.iter().position(|c| c.name() == v.name()),
        _ => None,
    }
}

/// Whether `agg` qualifies for a [`FastAgg`] fold over `schema` (the input
/// batch's schema, before grouping). `None` means `eval_group_native` must
/// use the general `eval_aggregate` path for this aggregate.
fn detect_fast_agg(agg: &Aggregate, schema: &[Var]) -> Option<FastAgg> {
    match &agg.func {
        AggFunc::Count(e) => {
            let col = bare_var_col(e, schema)?;
            Some(if agg.distinct {
                FastAgg::CountDistinct(col)
            } else {
                FastAgg::Count(col)
            })
        }
        AggFunc::Sum(e) if !agg.distinct => Some(FastAgg::Sum(bare_var_col(e, schema)?)),
        AggFunc::Avg(e) if !agg.distinct => Some(FastAgg::Avg(bare_var_col(e, schema)?)),
        AggFunc::Min(e) if !agg.distinct => Some(FastAgg::Min(bare_var_col(e, schema)?)),
        AggFunc::Max(e) if !agg.distinct => Some(FastAgg::Max(bare_var_col(e, schema)?)),
        _ => None,
    }
}

/// The inner expression(s) an aggregate evaluates over its members.
pub(crate) fn agg_inner_exprs(agg: &Aggregate) -> Vec<&Expr> {
    match &agg.func {
        AggFunc::CountStar => Vec::new(),
        AggFunc::Count(e)
        | AggFunc::Sum(e)
        | AggFunc::Avg(e)
        | AggFunc::Min(e)
        | AggFunc::Max(e)
        | AggFunc::Sample(e) => vec![&**e],
        AggFunc::GroupConcat { expr, .. } => vec![&**expr],
    }
}

/// Render an `xsd:integer` typed literal in N-Triples lexical form.
pub(crate) fn integer_literal(n: i64) -> Term {
    Term::Literal(format!(
        "\"{n}\"^^<http://www.w3.org/2001/XMLSchema#integer>"
    ))
}

/// Render an `xsd:decimal` typed literal.
fn decimal_literal(x: f64) -> Term {
    Term::Literal(format!(
        "\"{x}\"^^<http://www.w3.org/2001/XMLSchema#decimal>"
    ))
}

/// Extract the lexical value of a literal term for numeric/string
/// comparison and aggregation. For a `"v"^^<dt>` or `"v"@lang` literal,
/// returns the inner `v`; for a plain literal (no quotes), returns it
/// as-is.
///
/// Stage-1 note: the `MemStore` erases term kinds on scan, so a bound
/// literal object arrives as `Term::Iri("\"10\"^^<…>")` — the literal's
/// full N-Triples form wrapped in the wrong variant. We therefore run
/// `literal_lexical` over the `Iri`/`BlankNode` lexical forms too; a
/// genuine IRI does not start with `"` so it is returned unchanged. Once
/// the term-kind preservation (rung 4 / SPEC-02) lands this collapses to
/// just the `Literal` arm.
fn literal_value(t: &Term) -> String {
    match t {
        Term::Literal(raw) => literal_lexical(raw),
        Term::Iri(s) | Term::BlankNode(s) => literal_lexical(s),
        Term::Var(v) => v.name().to_owned(),
        Term::Triple(_) => String::new(),
    }
}

/// Decode N-Triples string escapes (`\\`, `\"`, `\n`, `\t`, `\r`,
/// `\uXXXX`, `\UXXXXXXXX`) in a literal's lexical form. Unknown
/// escapes pass through verbatim (best-effort, mirroring the lenient
/// Stage-1 parsing elsewhere).
pub(crate) fn unescape_ntriples(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('b') => out.push('\u{0008}'),
            Some('f') => out.push('\u{000C}'),
            Some('"') => out.push('"'),
            Some('\'') => out.push('\''),
            Some('\\') => out.push('\\'),
            Some(u @ ('u' | 'U')) => {
                let len = if u == 'u' { 4 } else { 8 };
                let hex: String = chars.by_ref().take(len).collect();
                match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    Some(decoded) => out.push(decoded),
                    None => {
                        out.push('\\');
                        out.push(u);
                        out.push_str(&hex);
                    }
                }
            }
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Parse the lexical part out of an N-Triples literal string.
fn literal_lexical(raw: &str) -> String {
    let raw = raw.trim();
    if !raw.starts_with('"') {
        return raw.to_owned();
    }
    let bytes = raw.as_bytes();
    let mut i = 1;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            i += 2;
            continue;
        }
        if bytes[i] == b'"' {
            return unescape_ntriples(&raw[1..i]);
        }
        i += 1;
    }
    raw.to_owned()
}

/// Best-effort numeric coercion of a term for SUM/AVG/MIN/MAX. `pub(crate)`
/// so [`Executor::decode_numeric`](crate::exec::Executor::decode_numeric)'s
/// default implementation can define "not a number" identically to the
/// general aggregate path (HDB-100).
pub(crate) fn numeric_value(t: &Term) -> Option<f64> {
    literal_value(t).trim().parse::<f64>().ok()
}

/// The numeric XSD datatypes for datatype-aware EBV and `ISNUMERIC`.
fn is_numeric_datatype(dt: &str) -> bool {
    let Some(local) = dt.strip_prefix("http://www.w3.org/2001/XMLSchema#") else {
        return false;
    };
    matches!(
        local,
        "integer"
            | "decimal"
            | "double"
            | "float"
            | "long"
            | "int"
            | "short"
            | "byte"
            | "nonNegativeInteger"
            | "nonPositiveInteger"
            | "negativeInteger"
            | "positiveInteger"
            | "unsignedLong"
            | "unsignedInt"
            | "unsignedShort"
            | "unsignedByte"
    )
}

/// SPARQL effective boolean value (§17.2.2), datatype-aware:
/// `xsd:boolean` → its value, numeric datatypes → value ≠ 0, plain /
/// `xsd:string` / lang-tagged → non-empty lexical form (so the *string*
/// `"false"` is true). EBV of a non-literal (IRI / blank node) or of a
/// non-boolean/numeric/string datatype is a type error — under the
/// crate-wide error→false convention it yields `false`.
fn ebv(t: &Term) -> bool {
    if term_kind(t) != TermKind::Literal {
        return false;
    }
    let raw = lex(t);
    if !raw.starts_with('"') {
        // Internal unquoted boolean results (`bool_lit`, the
        // comparison-expression terms): not an N-Triples form, so
        // keep the legacy lexical rules.
        return match raw.as_str() {
            "true" => true,
            "false" => false,
            other => match other.trim().parse::<f64>() {
                Ok(n) => n != 0.0,
                Err(_) => !other.is_empty(),
            },
        };
    }
    let (value, _lang, dt) = literal_parts(&raw);
    match dt.as_deref() {
        Some("http://www.w3.org/2001/XMLSchema#boolean") => value == "true" || value == "1",
        Some(dt) if is_numeric_datatype(dt) => value
            .trim()
            .parse::<f64>()
            .map(|n| n != 0.0 && !n.is_nan())
            .unwrap_or(false),
        Some("http://www.w3.org/2001/XMLSchema#string") | None => !value.is_empty(),
        Some(_) => false, // other datatypes: type error
    }
}

/// Apply N-Triples string escapes so a lexical value round-trips through
/// `literal_lexical` once wrapped in quotes.
fn escape_ntriples(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
        .replace('\u{0008}', "\\b")
        .replace('\u{000C}', "\\f")
}

/// Wrap a lexical value as a plain (unquoted-form) literal term,
/// re-applying N-Triples string escapes so the stored form round-trips
/// through `literal_lexical`.
fn plain_literal(s: &str) -> Term {
    Term::Literal(format!("\"{}\"", escape_ntriples(s)))
}

/// Wrap a lexical value as a language-tagged literal.
fn lang_literal(s: &str, lang: &str) -> Term {
    Term::Literal(format!("\"{}\"@{lang}", escape_ntriples(s)))
}

const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
const XSD_BOOLEAN: &str = "http://www.w3.org/2001/XMLSchema#boolean";

/// Wrap a lexical value as an explicit `xsd:string`-typed literal.
fn typed_string_literal(s: &str) -> Term {
    Term::Literal(format!("\"{}\"^^<{XSD_STRING}>", escape_ntriples(s)))
}

/// Render an `xsd:boolean` typed literal — the SPARQL 1.1 §17.3 result
/// type of a comparison/logical expression bound as a value (`BIND`, a
/// projected expression), rather than used only as a `FILTER` condition.
fn bool_typed_literal(v: bool) -> Term {
    Term::Literal(format!("\"{v}\"^^<{XSD_BOOLEAN}>"))
}

/// A string literal's "kind" per SPARQL 1.1 §17.4.3.1.3: a simple literal
/// (no annotation), a language-tagged literal, or an explicit
/// `xsd:string`-typed literal. Several §17.4 string builtins — `SUBSTR`,
/// `UCASE`, `LCASE`, `STRBEFORE`, `STRAFTER`, `REPLACE` — return "a literal
/// of the same kind" as one of their arguments, and `CONCAT` derives its own
/// kind from every argument's; both route through this one type so the
/// typing rule is encoded once rather than copied into each builtin.
#[derive(Clone, PartialEq, Eq)]
enum StrKind {
    Simple,
    Lang(String),
    XsdString,
}

/// Classify a term's `StrKind`, or `None` if it isn't a "string literal" at
/// all. Every §17.4.3 string builtin below is typed to take only simple,
/// language-tagged, or `xsd:string` literals as its string argument(s); a
/// non-literal term or a literal typed something else (`xsd:integer`, say)
/// is a type error there, not a value to coerce.
fn str_kind(t: &Term) -> Option<StrKind> {
    if term_kind(t) != TermKind::Literal {
        return None;
    }
    let (_, lang, dt) = literal_parts(&lex(t));
    Some(match (lang, dt) {
        (Some(l), _) => StrKind::Lang(l),
        (None, None) => StrKind::Simple,
        (None, Some(dt)) if dt == XSD_STRING => StrKind::XsdString,
        (None, Some(_)) => return None,
    })
}

/// Build a result literal from a lexical value and a `StrKind` decision —
/// the inverse of `str_kind`.
fn literal_with_kind(value: &str, kind: &StrKind) -> Term {
    match kind {
        StrKind::Simple => plain_literal(value),
        StrKind::Lang(l) => lang_literal(value, l),
        StrKind::XsdString => typed_string_literal(value),
    }
}

/// `CONCAT`'s own multi-argument return-kind rule (§17.4.3.12, distinct
/// from the single-source "same kind" rule the other string builtins use):
/// a language tag survives only when every argument shares it verbatim;
/// `xsd:string` only when every argument is explicitly typed `xsd:string`;
/// a simple literal otherwise.
fn concat_kind(kinds: &[StrKind]) -> StrKind {
    if let Some(StrKind::Lang(tag)) = kinds.first() {
        if kinds
            .iter()
            .all(|k| matches!(k, StrKind::Lang(t) if t == tag))
        {
            return StrKind::Lang(tag.clone());
        }
    }
    if !kinds.is_empty() && kinds.iter().all(|k| *k == StrKind::XsdString) {
        return StrKind::XsdString;
    }
    StrKind::Simple
}

/// Argument compatibility for `STRBEFORE`/`STRAFTER` (§17.4.3.1.2, shared
/// with `STRSTARTS`/`STRENDS`/`CONTAINS` upstream though only wired into
/// the two functions below today): both simple/`xsd:string`, both
/// language-tagged with the identical tag, or arg1 language-tagged with
/// arg2 simple/`xsd:string`. Incompatible arguments are a type error.
fn str_args_compatible(a: &StrKind, b: &StrKind) -> bool {
    match (a, b) {
        (StrKind::Lang(x), StrKind::Lang(y)) => x == y,
        (StrKind::Lang(_), StrKind::Simple | StrKind::XsdString) => true,
        (StrKind::Simple | StrKind::XsdString, StrKind::Simple | StrKind::XsdString) => true,
        _ => false,
    }
}

/// Shared `STRBEFORE`/`STRAFTER` logic (§17.4.3.9/.10): find the first
/// occurrence of `needle`'s lexical form in `hay`'s, after checking
/// argument compatibility. An empty `needle` is a vacuous match at
/// position 0. On a match, the kept half is a literal of `hay`'s kind; on
/// no match (or incompatible arguments raising a type error becomes
/// `None`), an empty *simple* literal is returned regardless of `hay`'s
/// kind — the spec's examples are explicit that the no-match case does not
/// carry `hay`'s language tag or datatype forward.
fn str_before_after(hay: &Term, needle: &Term, before: bool) -> Option<Term> {
    let hay_kind = str_kind(hay)?;
    let needle_kind = str_kind(needle)?;
    if !str_args_compatible(&hay_kind, &needle_kind) {
        return None;
    }
    let hay_val = literal_value(hay);
    let needle_val = literal_value(needle);
    Some(match hay_val.find(&needle_val) {
        Some(i) => {
            let split = if before {
                &hay_val[..i]
            } else {
                &hay_val[i + needle_val.len()..]
            };
            literal_with_kind(split, &hay_kind)
        }
        None => plain_literal(""),
    })
}

/// Binary arithmetic under the SPARQL §17.4.1 operator mapping. `None` is the
/// expression error: either operand not a numeric literal, or the operation
/// itself undefined (integer/decimal overflow, exact division by zero).
fn arith(
    op: fn(Numeric, Numeric) -> Option<Numeric>,
    a: Option<Numeric>,
    b: Option<Numeric>,
) -> Option<Term> {
    op(a?, b?).map(Numeric::to_term)
}

/// A term's numeric value, or `None` when it is not a numeric literal.
///
/// Unlike [`numeric_value`] — which coerces any lexical form that parses as
/// `f64`, the ordering/comparison rule — this is *datatype-driven*: a plain
/// literal `"1"` is not a number, so `"1" + "2"` is a type error, exactly as
/// SPARQL 1.1 §17.4.1 requires (W3C `functions/plus-1`).
pub(crate) fn numeric_of(t: &Term) -> Option<Numeric> {
    let raw = lex(t);
    let (value, lang, datatype) = literal_parts(&raw);
    if lang.is_some() {
        return None;
    }
    Numeric::parse(&value, datatype.as_deref()?)
}

/// Fold a multiset into its numeric sum. `None` — the expression error — if
/// any member is not a numeric literal, because `SUM` is defined as a fold of
/// `op:numeric-add` and a non-numeric operand raises a type error.
fn numeric_sum(vals: &[Term]) -> Option<Numeric> {
    let mut acc = Numeric::zero();
    for t in vals {
        acc = acc.add(numeric_of(t)?)?;
    }
    Some(acc)
}

/// Split an N-Triples literal raw form into (lexical, lang, datatype).
/// Non-literal raw forms (no leading quote) yield (raw, None, None).
pub(crate) fn literal_parts(raw: &str) -> (String, Option<String>, Option<String>) {
    let raw = raw.trim();
    if !raw.starts_with('"') {
        return (raw.to_owned(), None, None);
    }
    let bytes = raw.as_bytes();
    let mut i = 1;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            i += 2;
            continue;
        }
        if bytes[i] == b'"' {
            let value = raw[1..i].to_owned();
            let tail = &raw[i + 1..];
            if let Some(lang) = tail.strip_prefix('@') {
                return (value, Some(lang.to_owned()), None);
            }
            if let Some(dt) = tail.strip_prefix("^^") {
                let dt = dt.trim_start_matches('<').trim_end_matches('>');
                return (value, None, Some(dt.to_owned()));
            }
            return (value, None, None);
        }
        i += 1;
    }
    (raw.to_owned(), None, None)
}

/// Best-effort term-kind classification on the raw lexical form. The
/// Stage-1 `MemStore` erases kinds on scan, so this looks at the string
/// shape rather than the enum variant.
fn term_kind(t: &Term) -> TermKind {
    match t {
        Term::Literal(_) => TermKind::Literal,
        Term::BlankNode(_) => TermKind::Blank,
        Term::Iri(s) => {
            if s.starts_with('"') {
                TermKind::Literal
            } else if s.starts_with("_:") {
                TermKind::Blank
            } else {
                TermKind::Iri
            }
        }
        Term::Var(_) | Term::Triple(_) => TermKind::Other,
    }
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum TermKind {
    Iri,
    Blank,
    Literal,
    Other,
}

/// Compile a SPARQL REGEX/REPLACE pattern with its flags string.
/// Unsupported flag characters or an invalid pattern yield `None`
/// (expression error). Called per evaluated row — a compiled-pattern
/// cache is a future optimisation once FILTER throughput matters
/// (Stage-1 result sets are small).
fn compile_regex(pattern: &str, flags: &str) -> Option<regex::Regex> {
    let mut b = regex::RegexBuilder::new(pattern);
    for f in flags.chars() {
        match f {
            'i' => {
                b.case_insensitive(true);
            }
            's' => {
                b.dot_matches_new_line(true);
            }
            'm' => {
                b.multi_line(true);
            }
            'x' => {
                b.ignore_whitespace(true);
            }
            _ => return None,
        }
    }
    b.build().ok()
}

/// Compute one aggregate over a group's member rows.
fn eval_aggregate(agg: &Aggregate, members: &[Bindings]) -> Result<Option<Term>> {
    // Collect the aggregate's input multiset (the values of the inner
    // expression over the members), applying DISTINCT if requested.
    // For COUNT(*) the "input" is the rows themselves.
    let collect_values = |expr: &Expr| -> Result<Vec<Term>> {
        let mut vals = Vec::new();
        for m in members {
            if let Some(t) = eval_expr_to_term(expr, m)? {
                vals.push(t);
            }
        }
        if agg.distinct {
            dedup_terms(&mut vals);
        }
        Ok(vals)
    };

    Ok(match &agg.func {
        AggFunc::CountStar => {
            let n = if agg.distinct {
                // COUNT(DISTINCT *) — distinct whole solution rows. O(n) via a
                // hash set (was an O(n^2) linear scan, #128); only the count is
                // needed, so order is irrelevant here.
                members.iter().collect::<FxHashSet<&Bindings>>().len()
            } else {
                members.len()
            };
            Some(integer_literal(n as i64))
        }
        AggFunc::Count(e) => {
            let vals = collect_values(e)?;
            Some(integer_literal(vals.len() as i64))
        }
        AggFunc::Sum(e) => {
            let vals = collect_values(e)?;
            numeric_sum(&vals).map(Numeric::to_term)
        }
        AggFunc::Avg(e) => {
            let vals = collect_values(e)?;
            if vals.is_empty() {
                // AVG of the empty multiset is 0 (SPARQL 1.1 §18.5.1.4).
                Some(integer_literal(0))
            } else {
                numeric_sum(&vals)
                    .and_then(|sum| sum.div(Numeric::from_i64(vals.len() as i64)))
                    .map(Numeric::to_term)
            }
        }
        AggFunc::Min(e) => {
            let vals = collect_values(e)?;
            aggregate_extreme(&vals, true)
        }
        AggFunc::Max(e) => {
            let vals = collect_values(e)?;
            aggregate_extreme(&vals, false)
        }
        AggFunc::Sample(e) => {
            let vals = collect_values(e)?;
            vals.into_iter().next()
        }
        AggFunc::GroupConcat { expr, separator } => {
            let vals = collect_values(expr)?;
            let joined = vals
                .iter()
                .map(literal_value)
                .collect::<Vec<_>>()
                .join(separator);
            Some(Term::Literal(format!(
                "\"{}\"",
                joined.replace('"', "\\\"")
            )))
        }
    })
}

/// Pick MIN (`min == true`) or MAX of an input multiset. Numeric when
/// every value parses as a number, otherwise lexical ordering.
fn aggregate_extreme(vals: &[Term], min: bool) -> Option<Term> {
    if vals.is_empty() {
        return None;
    }
    let all_numeric = vals.iter().all(|t| numeric_value(t).is_some());
    if all_numeric {
        let mut best_idx = 0;
        let mut best = numeric_value(&vals[0]).unwrap();
        for (i, t) in vals.iter().enumerate().skip(1) {
            let n = numeric_value(t).unwrap();
            if (min && n < best) || (!min && n > best) {
                best = n;
                best_idx = i;
            }
        }
        Some(vals[best_idx].clone())
    } else {
        let mut best = &vals[0];
        for t in &vals[1..] {
            let ord = lex(t).cmp(&lex(best));
            if (min && ord == std::cmp::Ordering::Less)
                || (!min && ord == std::cmp::Ordering::Greater)
            {
                best = t;
            }
        }
        Some(best.clone())
    }
}

/// Deduplicate a term multiset by value, preserving first-seen order.
/// O(n) via a hash set (was an O(n^2) linear scan — the SPB aggregation
/// gap, #128).
fn dedup_terms(vals: &mut Vec<Term>) {
    let mut seen: FxHashSet<Term> =
        FxHashSet::with_capacity_and_hasher(vals.len(), Default::default());
    vals.retain(|t| seen.insert(t.clone()));
}

/// One row's resolved `ORDER BY` value: the term's numeric coercion (when it
/// has one) and its lexical value. Holding both mirrors [`compare_terms`],
/// which picks its branch per *pair* — numeric when both sides are numbers,
/// lexical otherwise.
struct SortVal {
    num: Option<f64>,
    lex: String,
}

impl SortVal {
    fn of(t: &Term) -> Self {
        let lex = literal_value(t);
        // Same coercion as `numeric_value`, reusing the lexical form we
        // already have instead of computing it twice.
        let num = lex.trim().parse::<f64>().ok();
        SortVal { num, lex }
    }
}

/// One `ORDER BY` key's value for every row of a batch, resolved once — the
/// "decorate" of decorate-sort-undecorate (HDB-101). `None` at a row means
/// the key is unbound there (or its expression errored), which sorts first.
///
/// The three variants exist to record *how* the column compares, not just
/// what it holds: `Num` and `Lex` are strict total orders and so can drive
/// the top-k heap, `Mixed` cannot (see [`SortCol::is_total_order`]).
enum SortCol {
    /// Every bound row coerces to a number.
    Num(Vec<Option<f64>>),
    /// No bound row coerces to a number, so every comparison is lexical.
    Lex(Vec<Option<String>>),
    /// Both kinds present: the branch `compare_terms` takes depends on the
    /// pair being compared.
    Mixed(Vec<Option<SortVal>>),
}

impl SortCol {
    /// Narrow a resolved column to its cheapest faithful representation.
    fn classify(vals: Vec<Option<SortVal>>) -> Self {
        let bound = || vals.iter().flatten();
        if bound().all(|v| v.num.is_some()) {
            SortCol::Num(vals.into_iter().map(|v| v.and_then(|v| v.num)).collect())
        } else if bound().all(|v| v.num.is_none()) {
            SortCol::Lex(vals.into_iter().map(|v| v.map(|v| v.lex)).collect())
        } else {
            SortCol::Mixed(vals)
        }
    }

    /// Whether this column's comparison is a strict total order. The top-k
    /// heap needs one: "the n smallest rows" is only well defined — and only
    /// agrees with a full sort — when the comparator is consistent.
    ///
    /// A `Mixed` column is not, because `compare_terms` chooses numeric vs
    /// lexical per pair and the two can disagree about transitivity. A `Num`
    /// column carrying a NaN is not either: NaN compares `Equal` to
    /// everything, so `a == NaN` and `b == NaN` while `a < b`.
    ///
    /// `Lex` is total only because [`datetime_key`] is a no-op, which makes
    /// [`compare_lexical`]'s two branches one comparison. Both carry a note
    /// pointing back here.
    fn is_total_order(&self) -> bool {
        match self {
            SortCol::Num(v) => !v.iter().flatten().any(|f| f.is_nan()),
            SortCol::Lex(_) => true,
            SortCol::Mixed(_) => false,
        }
    }
}

/// Compare rows `a` and `b` by their decorated keys. Reproduces
/// `compare_by_keys` + [`compare_terms`] exactly: unbound sorts before bound,
/// a pair of numbers compares numerically, anything else compares lexically.
fn compare_decorated(cols: &[(SortCol, OrderDir)], a: usize, b: usize) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    for (col, dir) in cols {
        let ord = match col {
            SortCol::Num(v) => match (v[a], v[b]) {
                (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(Ordering::Equal),
                (None, Some(_)) => Ordering::Less,
                (Some(_), None) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            },
            SortCol::Lex(v) => match (&v[a], &v[b]) {
                (Some(x), Some(y)) => compare_lexical(x, y),
                (None, Some(_)) => Ordering::Less,
                (Some(_), None) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            },
            SortCol::Mixed(v) => match (&v[a], &v[b]) {
                (Some(x), Some(y)) => match (x.num, y.num) {
                    (Some(p), Some(q)) => p.partial_cmp(&q).unwrap_or(Ordering::Equal),
                    _ => compare_lexical(&x.lex, &y.lex),
                },
                (None, Some(_)) => Ordering::Less,
                (Some(_), None) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            },
        };
        if ord != Ordering::Equal {
            return match dir {
                OrderDir::Asc => ord,
                OrderDir::Desc => ord.reverse(),
            };
        }
    }
    std::cmp::Ordering::Equal
}

/// Row indices in full sorted order. Stable: equal-key rows keep their input
/// order, matching the pre-HDB-101 `Vec::sort_by` over decoded rows.
fn sorted_order(cols: &[(SortCol, OrderDir)], rows: usize) -> Vec<usize> {
    let mut order: Vec<usize> = (0..rows).collect();
    order.sort_by(|&a, &b| compare_decorated(cols, a, b));
    order
}

/// Indices of the `n` rows a full stable sort would place first, in that same
/// order, without sorting the other `rows - n`. The caller is expected to have
/// checked `n < rows` and that every column is a total order; `n == 0` is
/// handled here as well, so the heap below can index its root unconditionally.
///
/// Keeps a bounded max-heap of the best `n` rows seen so far, ordered by
/// `(key, input position)`. The position tie-break is what makes both the
/// selection *and* the final order identical to the stable sort's first `n`
/// rows: among equal keys the stable sort keeps the earliest, and so does
/// this.
fn top_k_order(cols: &[(SortCol, OrderDir)], rows: usize, n: usize) -> Vec<usize> {
    use std::cmp::Ordering;
    if n == 0 {
        return Vec::new();
    }
    // True when row `a` sorts strictly after row `b` in the full stable sort.
    let worse = |a: usize, b: usize| match compare_decorated(cols, a, b) {
        Ordering::Less => false,
        Ordering::Greater => true,
        Ordering::Equal => a > b,
    };
    // Max-heap: its root is the worst of the rows kept so far, so a new row
    // only has to beat the root to earn a place.
    let mut heap: Vec<usize> = Vec::with_capacity(n);
    for i in 0..rows {
        if heap.len() < n {
            heap.push(i);
            let last = heap.len() - 1;
            sift_up(&mut heap, last, &worse);
        } else if worse(heap[0], i) {
            heap[0] = i;
            sift_down(&mut heap, 0, &worse);
        }
    }
    // The heap holds the right rows in heap order; put them in sorted order.
    // `then` on the index reproduces the stable sort's tie order — the heap
    // itself carries no memory of input position.
    heap.sort_by(|&a, &b| compare_decorated(cols, a, b).then(a.cmp(&b)));
    heap
}

fn sift_up(heap: &mut [usize], mut i: usize, worse: &impl Fn(usize, usize) -> bool) {
    while i > 0 {
        let parent = (i - 1) / 2;
        if !worse(heap[i], heap[parent]) {
            return;
        }
        heap.swap(i, parent);
        i = parent;
    }
}

fn sift_down(heap: &mut [usize], mut i: usize, worse: &impl Fn(usize, usize) -> bool) {
    loop {
        let mut m = i;
        for child in [2 * i + 1, 2 * i + 2] {
            if child < heap.len() && worse(heap[child], heap[m]) {
                m = child;
            }
        }
        if m == i {
            return;
        }
        heap.swap(i, m);
        i = m;
    }
}

/// Reorder `rows` into `order`, which must name each index at most once.
/// Leaves an empty `Row` behind at every index it takes, so no `Row` is
/// cloned.
fn permute(rows: &mut [Row], order: &[usize]) -> Vec<Row> {
    order
        .iter()
        .map(|&i| std::mem::replace(&mut rows[i], Row(Vec::new())))
        .collect()
}

pub(crate) fn lex(t: &Term) -> String {
    match t {
        Term::Iri(s) | Term::Literal(s) | Term::BlankNode(s) => s.clone(),
        Term::Var(v) => v.name().to_owned(),
        // RDF 1.2 triple terms have no canonical lexical form in the
        // Stage-1 String-based representation. Emitting the empty
        // string here is consistent with how unbound `Var` patterns
        // surface in lexicographic comparisons; SPEC-07 RDF 1.2
        // follow-up will route this through the dictionary instead.
        Term::Triple(_) => String::new(),
    }
}

fn eval_expr(e: &Expr, b: &Bindings) -> Result<bool> {
    use std::cmp::Ordering;
    let cmp = |a: &Expr, c: &Expr| -> Result<Option<Ordering>> {
        Ok(match (eval_expr_to_term(a, b)?, eval_expr_to_term(c, b)?) {
            (Some(x), Some(y)) => Some(compare_terms(&x, &y)),
            _ => None,
        })
    };
    Ok(match e {
        Expr::Eq(a, c) => eval_expr_to_term(a, b)? == eval_expr_to_term(c, b)?,
        // Identical to `Eq` today (structural `Term` equality) — see the
        // doc comment on `Expr::SameTerm`.
        Expr::SameTerm(a, c) => eval_expr_to_term(a, b)? == eval_expr_to_term(c, b)?,
        Expr::Ne(a, c) => eval_expr_to_term(a, b)? != eval_expr_to_term(c, b)?,
        Expr::Lt(a, c) => cmp(a, c)? == Some(Ordering::Less),
        Expr::Gt(a, c) => cmp(a, c)? == Some(Ordering::Greater),
        Expr::Le(a, c) => matches!(cmp(a, c)?, Some(Ordering::Less | Ordering::Equal)),
        Expr::Ge(a, c) => matches!(cmp(a, c)?, Some(Ordering::Greater | Ordering::Equal)),
        Expr::And(a, c) => eval_expr(a, b)? && eval_expr(c, b)?,
        Expr::Or(a, c) => eval_expr(a, b)? || eval_expr(c, b)?,
        Expr::Not(a) => !eval_expr(a, b)?,
        Expr::Bound(v) => b.get(v.name()).is_some(),
        Expr::In(a, list) => {
            let lhs = eval_expr_to_term(a, b)?;
            match lhs {
                None => false,
                Some(x) => {
                    let mut found = false;
                    for item in list {
                        if let Some(y) = eval_expr_to_term(item, b)? {
                            // Value equality (not variant equality): the
                            // Stage-1 store may bind the LHS as a
                            // different term kind than the constant RHS.
                            if x == y || compare_terms(&x, &y) == std::cmp::Ordering::Equal {
                                found = true;
                                break;
                            }
                        }
                    }
                    found
                }
            }
        }
        Expr::Add(..)
        | Expr::Sub(..)
        | Expr::Mul(..)
        | Expr::Div(..)
        | Expr::Neg(..)
        | Expr::If(..)
        | Expr::Coalesce(..)
        | Expr::Func(..) => match eval_expr_to_term(e, b)? {
            Some(t) => ebv(&t),
            None => false,
        },
        Expr::Term(t) => match t {
            // Bare term in boolean context: SPARQL effective boolean
            // value of the bound value (unbound var is an error →
            // false) or of the constant itself.
            Term::Var(v) => b.get(v.name()).map(ebv).unwrap_or(false),
            other => ebv(other),
        },
    })
}

/// Order two terms for SPARQL relational operators. Numeric when both
/// coerce to numbers, then xsd:dateTime when both look like ISO-8601
/// instants, otherwise lexical comparison of the literal value. This is
/// a Stage-1 best effort — it covers the SPB datetime-range filters and
/// ordinary numeric/string comparisons without a full XSD type lattice.
fn compare_terms(x: &Term, y: &Term) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    if let (Some(a), Some(b)) = (numeric_value(x), numeric_value(y)) {
        return a.partial_cmp(&b).unwrap_or(Ordering::Equal);
    }
    compare_lexical(&literal_value(x), &literal_value(y))
}

/// The non-numeric half of [`compare_terms`], over two already-extracted
/// lexical values. Shared with `compare_decorated` so the decorated
/// comparator cannot drift away from the term comparator.
///
/// **This is a total order only while [`datetime_key`] returns its input
/// unchanged.** Both branches below are then the same comparison, and
/// [`SortCol::is_total_order`] can call a `SortCol::Lex` column unconditionally
/// total. If `datetime_key` ever starts normalising (stripping an offset,
/// padding fractional seconds), this becomes a per-pair conditional — the same
/// non-transitive shape the numeric/lexical split already has to guard
/// against — and `is_total_order` must stop trusting `Lex`, or the top-k heap
/// will quietly answer differently from a full sort.
fn compare_lexical(lx: &str, ly: &str) -> std::cmp::Ordering {
    if let (Some(a), Some(b)) = (datetime_key(lx), datetime_key(ly)) {
        return a.cmp(b);
    }
    lx.cmp(ly)
}

/// Normalise an xsd:dateTime lexical form into a sortable key. Returns
/// `None` if the string does not look like an ISO-8601 instant. We do
/// not parse offsets fully; the lexical form sorts correctly for the
/// common `YYYY-MM-DDThh:mm:ss(.fff)?(Z)?` shape used by SPB, so we just
/// validate the prefix and key on the original string.
///
/// **Making this actually rewrite the key is not a local change.** It is a
/// no-op today, which is why [`compare_lexical`]'s two branches are the same
/// comparison and why [`SortCol::is_total_order`] calls a `SortCol::Lex`
/// column total. Revisit both before returning anything but `s`.
fn datetime_key(s: &str) -> Option<&str> {
    let bytes = s.as_bytes();
    // Minimum `YYYY-MM-DDThh:mm:ss` is 19 chars.
    if bytes.len() < 19 {
        return None;
    }
    let is_shape = bytes[4] == b'-'
        && bytes[7] == b'-'
        && (bytes[10] == b'T' || bytes[10] == b' ')
        && bytes[13] == b':'
        && bytes[16] == b':'
        && bytes[..4].iter().all(|c| c.is_ascii_digit());
    if is_shape {
        Some(s)
    } else {
        None
    }
}

fn eval_expr_to_term(e: &Expr, b: &Bindings) -> Result<Option<Term>> {
    // Evaluate an operand to its numeric value; an expression error
    // (non-numeric / unbound) surfaces as `Ok(None)`.
    let numof = |sub: &Expr| -> Result<Option<Numeric>> {
        Ok(eval_expr_to_term(sub, b)?.as_ref().and_then(numeric_of))
    };
    Ok(match e {
        Expr::Term(t) => match t {
            Term::Var(v) => b.get(v.name()).cloned(),
            other => Some(other.clone()),
        },
        // Comparison/logical expressions return an `xsd:boolean` typed
        // literal (SPARQL 1.1 §17.3) when bound as a value rather than used
        // only as a `FILTER` condition — e.g. `BIND(?y = ?z AS ?eq)`.
        Expr::Eq(_, _)
        | Expr::SameTerm(_, _)
        | Expr::Ne(_, _)
        | Expr::Lt(_, _)
        | Expr::Gt(_, _)
        | Expr::Le(_, _)
        | Expr::Ge(_, _)
        | Expr::In(_, _)
        | Expr::And(_, _)
        | Expr::Or(_, _)
        | Expr::Not(_)
        | Expr::Bound(_) => Some(bool_typed_literal(eval_expr(e, b)?)),
        Expr::Add(x, y) => arith(Numeric::add, numof(x)?, numof(y)?),
        Expr::Sub(x, y) => arith(Numeric::sub, numof(x)?, numof(y)?),
        Expr::Mul(x, y) => arith(Numeric::mul, numof(x)?, numof(y)?),
        // `Numeric::div` is the whole rule: integer/integer yields
        // xsd:decimal, and division by zero errors for the exact types.
        Expr::Div(x, y) => arith(Numeric::div, numof(x)?, numof(y)?),
        Expr::Neg(x) => numof(x)?.and_then(Numeric::neg).map(Numeric::to_term),
        // Stage-1 note: an erroring condition evaluates as false (the
        // crate-wide error→false EBV convention) and takes the else
        // branch, rather than propagating the error as SPARQL §17.4.1.2
        // specifies.
        Expr::If(c, t, f) => {
            if eval_expr(c, b)? {
                eval_expr_to_term(t, b)?
            } else {
                eval_expr_to_term(f, b)?
            }
        }
        Expr::Coalesce(args) => {
            // `?` is safe here because runtime expression errors are represented
            // as Ok(None), never Err — so error-skipping per SPARQL §17.4.1.6
            // still holds.
            let mut found = None;
            for a in args {
                if let Some(t) = eval_expr_to_term(a, b)? {
                    found = Some(t);
                    break;
                }
            }
            found
        }
        Expr::Func(f, args) => eval_func(*f, args, b)?,
    })
}

/// Evaluate a builtin function call. `Ok(None)` is "expression error"
/// (the SPARQL error value): the binding stays unbound / the filter
/// row drops. All value extraction goes through the raw lexical form
/// because the Stage-1 `MemStore` erases term kinds on scan.
fn eval_func(f: Func, args: &[Expr], b: &Bindings) -> Result<Option<Term>> {
    // Evaluate one argument to a term; `None` short-circuits the call.
    let term = |i: usize| -> Result<Option<Term>> {
        match args.get(i) {
            Some(e) => eval_expr_to_term(e, b),
            None => Ok(None),
        }
    };
    // The argument's plain string value (literal lexical form).
    let s = |i: usize| -> Result<Option<String>> { Ok(term(i)?.as_ref().map(literal_value)) };
    // The argument as a number.
    let num = |i: usize| -> Result<Option<f64>> { Ok(term(i)?.as_ref().and_then(numeric_value)) };
    // The argument as a *typed* number, for the operators whose result type
    // follows the argument's (§17.4.4).
    let numv = |i: usize| -> Result<Option<Numeric>> { Ok(term(i)?.as_ref().and_then(numeric_of)) };
    let bool_lit = |v: bool| Some(Term::Literal(if v { "true" } else { "false" }.into()));

    Ok(match f {
        Func::Str => term(0)?.map(|t| plain_literal(&literal_value(&t))),
        Func::Lang => term(0)?.and_then(|t| {
            // LANG on a non-literal is a type error (SPARQL §17.4.1.1),
            // mirroring the DATATYPE arm below.
            if term_kind(&t) != TermKind::Literal {
                return None;
            }
            let (_, lang, _) = literal_parts(&lex(&t));
            Some(plain_literal(&lang.unwrap_or_default()))
        }),
        // RFC 4647 *basic* filtering per SPARQL §17.4.3.7: "*" matches
        // any non-empty tag, otherwise exact or prefix-before-'-'
        // match. Extended ranges with embedded wildcards ("en-*") are
        // deliberately out of scope — basic filtering does not define
        // them.
        Func::LangMatches => match (s(0)?, s(1)?) {
            (Some(tag), Some(range)) => {
                let tag = tag.to_ascii_lowercase();
                let range = range.to_ascii_lowercase();
                let ok = if range == "*" {
                    !tag.is_empty()
                } else {
                    tag == range || tag.starts_with(&format!("{range}-"))
                };
                bool_lit(ok)
            }
            _ => None,
        },
        Func::Datatype => term(0)?.and_then(|t| {
            if term_kind(&t) != TermKind::Literal {
                return None;
            }
            let (_, lang, dt) = literal_parts(&lex(&t));
            let iri = if let Some(dt) = dt {
                dt
            } else if lang.is_some() {
                "http://www.w3.org/1999/02/22-rdf-syntax-ns#langString".to_owned()
            } else {
                "http://www.w3.org/2001/XMLSchema#string".to_owned()
            };
            Some(Term::Iri(iri))
        }),
        Func::StrLen => s(0)?.map(|v| integer_literal(v.chars().count() as i64)),
        Func::SubStr => {
            let (source, start) = match (term(0)?, num(1)?) {
                (Some(t), Some(s)) => (t, s),
                _ => return Ok(None),
            };
            let kind = match str_kind(&source) {
                Some(k) => k,
                None => return Ok(None),
            };
            let text = literal_value(&source);
            // SPARQL SUBSTR is 1-based; len is optional (to end).
            let start = (start.round() as i64 - 1).max(0) as usize;
            let chars: Vec<char> = text.chars().collect();
            let taken: String = match args.len() {
                2 => chars.iter().skip(start).collect(),
                3 => match num(2)? {
                    Some(l) => chars
                        .iter()
                        .skip(start)
                        .take(l.round().max(0.0) as usize)
                        .collect(),
                    None => return Ok(None),
                },
                _ => return Ok(None),
            };
            Some(literal_with_kind(&taken, &kind))
        }
        Func::UCase => match term(0)? {
            Some(t) => {
                str_kind(&t).map(|k| literal_with_kind(&literal_value(&t).to_uppercase(), &k))
            }
            None => None,
        },
        Func::LCase => match term(0)? {
            Some(t) => {
                str_kind(&t).map(|k| literal_with_kind(&literal_value(&t).to_lowercase(), &k))
            }
            None => None,
        },
        Func::StrStarts => match (s(0)?, s(1)?) {
            (Some(a), Some(b)) => bool_lit(a.starts_with(&b)),
            _ => None,
        },
        Func::StrEnds => match (s(0)?, s(1)?) {
            (Some(a), Some(b)) => bool_lit(a.ends_with(&b)),
            _ => None,
        },
        Func::Contains => match (s(0)?, s(1)?) {
            (Some(a), Some(b)) => bool_lit(a.contains(&b)),
            _ => None,
        },
        Func::StrBefore => match (term(0)?, term(1)?) {
            (Some(a), Some(b)) => str_before_after(&a, &b, true),
            _ => None,
        },
        Func::StrAfter => match (term(0)?, term(1)?) {
            (Some(a), Some(b)) => str_before_after(&a, &b, false),
            _ => None,
        },
        Func::Concat => {
            let mut out = String::new();
            let mut kinds = Vec::with_capacity(args.len());
            for i in 0..args.len() {
                match term(i)? {
                    Some(t) => match str_kind(&t) {
                        Some(k) => {
                            out.push_str(&literal_value(&t));
                            kinds.push(k);
                        }
                        None => return Ok(None),
                    },
                    None => return Ok(None),
                }
            }
            Some(literal_with_kind(&out, &concat_kind(&kinds)))
        }
        Func::Replace => {
            let (source, pat, repl) = match (term(0)?, s(1)?, s(2)?) {
                (Some(t), Some(p), Some(r)) => (t, p, r),
                _ => return Ok(None),
            };
            let kind = match str_kind(&source) {
                Some(k) => k,
                None => return Ok(None),
            };
            let text = literal_value(&source);
            let flags = if args.len() == 4 {
                match s(3)? {
                    Some(f) => f,
                    None => return Ok(None),
                }
            } else {
                String::new()
            };
            compile_regex(&pat, &flags)
                .map(|re| literal_with_kind(&re.replace_all(&text, repl.as_str()), &kind))
        }
        Func::Regex => {
            let (text, pat) = match (s(0)?, s(1)?) {
                (Some(t), Some(p)) => (t, p),
                _ => return Ok(None),
            };
            let flags = if args.len() == 3 {
                match s(2)? {
                    Some(f) => f,
                    None => return Ok(None),
                }
            } else {
                String::new()
            };
            compile_regex(&pat, &flags).and_then(|re| bool_lit(re.is_match(&text)))
        }
        // ABS/CEIL/FLOOR/ROUND all return the argument's own xsd type —
        // CEIL of an xsd:decimal is an xsd:decimal, not an xsd:integer.
        Func::Abs => numv(0)?.and_then(Numeric::abs).map(Numeric::to_term),
        Func::Ceil => numv(0)?.and_then(Numeric::ceil).map(Numeric::to_term),
        Func::Floor => numv(0)?.and_then(Numeric::floor).map(Numeric::to_term),
        Func::Round => numv(0)?.and_then(Numeric::round).map(Numeric::to_term),
        Func::IsIri => term(0)?.and_then(|t| bool_lit(term_kind(&t) == TermKind::Iri)),
        Func::IsBlank => term(0)?.and_then(|t| bool_lit(term_kind(&t) == TermKind::Blank)),
        Func::IsLiteral => term(0)?.and_then(|t| bool_lit(term_kind(&t) == TermKind::Literal)),
        // ISNUMERIC is true only for literals with a numeric XSD
        // datatype whose lexical form parses (§17.4.2.4) — a plain
        // string that merely looks numeric ("42") is false.
        Func::IsNumeric => term(0)?.and_then(|t| {
            if term_kind(&t) != TermKind::Literal {
                return bool_lit(false);
            }
            let (value, _, dt) = literal_parts(&lex(&t));
            let ok = dt.as_deref().is_some_and(is_numeric_datatype)
                && value.trim().parse::<f64>().is_ok();
            bool_lit(ok)
        }),
        Func::Year | Func::Month | Func::Day | Func::Hours | Func::Minutes | Func::Seconds => {
            // The accessors are defined on xsd:dateTime — a plain
            // string that merely looks like a timestamp is a type
            // error, matching the ISNUMERIC datatype strictness.
            let t = match term(0)? {
                Some(t) => t,
                None => return Ok(None),
            };
            let (v, _, dt) = literal_parts(&lex(&t));
            if dt.as_deref() != Some("http://www.w3.org/2001/XMLSchema#dateTime") {
                return Ok(None);
            }
            if datetime_key(&v).is_none() {
                return Ok(None);
            }
            // Validated shape: YYYY-MM-DDThh:mm:ss(.fff…)?
            let field = |a: usize, z: usize| v[a..z].parse::<i64>().ok();
            match f {
                Func::Year => field(0, 4).map(integer_literal),
                Func::Month => field(5, 7).map(integer_literal),
                Func::Day => field(8, 10).map(integer_literal),
                Func::Hours => field(11, 13).map(integer_literal),
                Func::Minutes => field(14, 16).map(integer_literal),
                _ => {
                    // SECONDS — keep any fractional part. Always
                    // xsd:decimal per SPARQL §17.4.5.6 (numeric_term
                    // would promote whole seconds to xsd:integer).
                    let tail: String = v[17..]
                        .chars()
                        .take_while(|c| c.is_ascii_digit() || *c == '.')
                        .collect();
                    tail.parse::<f64>().ok().map(decimal_literal)
                }
            }
        }
    })
}

/// Render a CONSTRUCT template against a stream of solution mappings.
///
/// Returns concrete `(s, p, o)` lexical-form triples. Triples whose
/// template references an unbound variable in the row are skipped
/// (W3C: "ground triple results only").
pub fn construct_triples(
    query: &spargebra::Query,
    rows: &[Bindings],
) -> Result<Vec<(String, String, String)>> {
    use spargebra::term::{NamedNodePattern, TermPattern};
    let template = match query {
        spargebra::Query::Construct { template, .. } => template,
        _ => {
            return Err(SparqlError::Executor(
                "construct_triples called on non-CONSTRUCT query".into(),
            ))
        }
    };

    fn resolve_term(t: &TermPattern, row: &Bindings) -> Option<String> {
        match t {
            TermPattern::NamedNode(n) => Some(n.as_str().to_owned()),
            TermPattern::BlankNode(b) => Some(b.as_str().to_owned()),
            TermPattern::Literal(l) => Some(l.to_string()),
            TermPattern::Variable(v) => match row.get(v.as_str()) {
                Some(Term::Iri(s)) | Some(Term::Literal(s)) | Some(Term::BlankNode(s)) => {
                    Some(s.clone())
                }
                _ => None,
            },
            // RDF 1.2 ground triple-term templates in CONSTRUCT are not
            // emitted by the Stage-1 lexical-form path (a `Term::Triple`
            // has no canonical `String` form here). Skip the slot so the
            // outer (s, p, o) tuple is dropped. See SPEC-07 / TASKS.md
            // for the dictionary-backed CONSTRUCT follow-up.
            TermPattern::Triple(_) => None,
        }
    }
    // See also update.rs::resolve_pred — same "predicate var binding must
    // be an IRI" invariant; keep the two in lockstep.
    fn resolve_pred(p: &NamedNodePattern, row: &Bindings) -> Option<String> {
        match p {
            NamedNodePattern::NamedNode(n) => Some(n.as_str().to_owned()),
            NamedNodePattern::Variable(v) => match row.get(v.as_str()) {
                Some(Term::Iri(s)) => Some(s.clone()),
                _ => None,
            },
        }
    }

    let mut out = Vec::new();
    for row in rows {
        for tp in template {
            if let (Some(s), Some(p), Some(o)) = (
                resolve_term(&tp.subject, row),
                resolve_pred(&tp.predicate, row),
                resolve_term(&tp.object, row),
            ) {
                out.push((s, p, o));
            }
        }
    }
    Ok(out)
}

/// Build a DESCRIBE result graph from explicit-IRI seeds plus
/// already-projected solution rows.
///
/// `seeds` are resources named directly by IRI in the DESCRIBE clause
/// (SPARQL 1.1 §16.4); they are described unconditionally — even when the
/// WHERE clause yields zero rows. The `rows` arrive projected to the
/// DESCRIBE-target variables (the planner runs the same projection as a
/// SELECT), so every value bound to *any* variable in a row is also a
/// resource to describe. The final resource set is (seeds) ∪ (row
/// bindings), deduplicated. We emit a
/// **forward, one-level Concise Bounded Description**: for each distinct
/// resource, every stored triple with that resource as subject.
///
/// Output is deduplicated and returned in deterministic sorted order
/// (via `BTreeSet`). Literals bound to a projected variable are never
/// subjects of stored triples, so they naturally contribute nothing —
/// no special-casing needed.
///
/// Each describe-target resource is scanned with its **original term**
/// (kind preserved), so a type-preserving backend that binds a target
/// to a `Term::BlankNode` is scanned as a blank node, not coerced to an
/// IRI. The Stage-1 `MemStore` erases term kinds on scan (`unify_one`
/// binds every value as `Term::Iri(lexical)`), which masks the
/// distinction there but not for richer backends.
///
/// Deferred (out of scope, see SPEC-07 / TASKS.md): recursive
/// blank-node CBD closure and symmetric CBD (would require reliably
/// detecting blank-node objects to recurse into, which the term-kind
/// erasure in `MemStore` defeats). Typed-literal / Turtle serialisation
/// is likewise a separate increment (#57); this reuses the N-Triples
/// path.
///
/// `scope` is the query's default graph: `DESCRIBE` has no `GRAPH`
/// wrapper, so the expansion reads whatever the dataset clause and
/// `default_graph` mode make the default graph (SPEC-28 S3).
pub fn describe_triples<E: Executor + ?Sized>(
    exec: &E,
    scope: &ScanScope<'_>,
    seeds: &[Term],
    rows: &[Bindings],
) -> Result<Vec<(String, String, String)>> {
    use crate::algebra::{Term, TriplePattern, Var};
    use std::collections::{BTreeSet, HashSet};

    // Variable names used in the forward-scan pattern below. Defined once
    // so the pattern construction and the binding lookups can't drift.
    const PRED_VAR: &str = "p";
    const OBJ_VAR: &str = "o";

    // Distinct resource *terms* (kind preserved) bound across all rows /
    // all vars. Scanning with the original term keeps a `Term::BlankNode`
    // target from being silently coerced to a `Term::Iri`, which would
    // miss its triples on a kind-preserving backend.
    let mut resources: HashSet<Term> = HashSet::new();
    // Resources named directly by IRI in the DESCRIBE clause (SPARQL 1.1
    // §16.4). These are described unconditionally, independent of whether
    // the WHERE clause produced any solution rows.
    for term in seeds {
        match term {
            Term::Iri(_) | Term::Literal(_) | Term::BlankNode(_) => {
                resources.insert(term.clone());
            }
            Term::Var(_) | Term::Triple(_) => {}
        }
    }
    for row in rows {
        for (_name, term) in row.vars() {
            match term {
                Term::Iri(_) | Term::Literal(_) | Term::BlankNode(_) => {
                    resources.insert(term.clone());
                }
                // An unbound var or a triple-term can't be a describe
                // subject, so it carries no describable resource here.
                Term::Var(_) | Term::Triple(_) => {}
            }
        }
    }

    // Lexical form of a resource term, used as the subject of every
    // emitted triple. Only the three scannable kinds reach here.
    fn subject_lex(term: &Term) -> Option<&str> {
        match term {
            Term::Iri(s) | Term::Literal(s) | Term::BlankNode(s) => Some(s),
            Term::Var(_) | Term::Triple(_) => None,
        }
    }

    let mut out: BTreeSet<(String, String, String)> = BTreeSet::new();
    for resource in &resources {
        let Some(subject) = subject_lex(resource) else {
            continue;
        };
        let pattern = TriplePattern {
            subject: resource.clone(),
            predicate: Term::Var(Var::new(PRED_VAR)),
            object: Term::Var(Var::new(OBJ_VAR)),
        };
        for b in exec.scan_bgp(std::slice::from_ref(&pattern), scope)? {
            let p = match b.get(PRED_VAR) {
                Some(Term::Iri(s)) | Some(Term::Literal(s)) | Some(Term::BlankNode(s)) => s.clone(),
                _ => continue,
            };
            let o = match b.get(OBJ_VAR) {
                Some(Term::Iri(s)) | Some(Term::Literal(s)) | Some(Term::BlankNode(s)) => s.clone(),
                _ => continue,
            };
            out.insert((subject.to_owned(), p, o));
        }
    }
    Ok(out.into_iter().collect())
}

#[cfg(test)]
mod slot_differential {
    use super::*;
    use crate::algebra::translate::translate_query_with;
    use crate::exec::horn::HornBackend;
    use crate::exec::Store;
    use crate::parser::parse_query;
    use crate::plan::planner;
    use crate::SparqlConfig;

    /// Run a plan through the streaming operator tree and concatenate all
    /// chunks into a single `Batch`, preserving slot provenance (Slot::Id vs
    /// Slot::Term). Replaces the removed `Runtime::eval` at the test call sites.
    fn eval_to_batch<E: crate::exec::Executor + ?Sized>(
        rt: &Runtime<'_, E>,
        plan: &PhysicalPlan,
    ) -> Batch {
        let mut op = rt.build(plan).unwrap();
        let schema = op.schema().to_vec();
        let mut rows = Vec::new();
        while let Some(b) = op.next().unwrap() {
            rows.extend(b.rows);
        }
        Batch { schema, rows }
    }

    /// Build a `PhysicalPlan` from a SELECT query string, mirroring
    /// what `api::execute_query_with` does for the SELECT arm.
    fn plan_select(q: &str) -> PhysicalPlan {
        let parsed = parse_query(q).expect("query parse failed");
        let inner = match parsed {
            crate::parser::ParsedQuery::Select { inner } => inner,
            other => panic!("expected SELECT, got {:?}", other),
        };
        let translated =
            translate_query_with(&inner, &SparqlConfig::default()).expect("translation failed");
        planner::plan(&translated.algebra).expect("planning failed")
    }

    /// Native `Extend` (BIND) must not decode the columns it inherits from
    /// the child batch. A BGP-scan column must remain `Slot::Id` in the
    /// batch; only the freshly-computed BIND column is `Slot::Term`.
    #[test]
    fn extend_preserves_id_slots() {
        let mut horn = HornBackend::new();
        let iri = |s: &str| Term::Iri(format!("http://ex/{s}"));
        horn.insert_triple(iri("s"), iri("p"), iri("o"));

        // SELECT ?s ?x WHERE { ?s <p> <o> . BIND(<c> AS ?x) }
        // Plan: Project { vars:[?s,?x], inner: Extend { var:?x, inner: BgpScan } }
        let plan = plan_select(
            "SELECT ?s ?x WHERE { ?s <http://ex/p> <http://ex/o> . BIND(<http://ex/c> AS ?x) }",
        );

        let rt = Runtime::new(&horn);
        let batch = eval_to_batch(&rt, &plan);

        assert_eq!(batch.rows.len(), 1, "expected exactly one result row");

        // ?s comes from a BGP scan. Native Extend preserves Slot::Id.
        let s_idx = batch.col("s").expect("?s must be in output schema");
        assert!(
            matches!(batch.rows[0].0[s_idx], Slot::Id(_)),
            "?s from BGP scan should remain Slot::Id after native Extend; got {:?}",
            batch.rows[0].0[s_idx]
        );

        // ?x is the BIND result: always Slot::Term (computed, never Id).
        let x_idx = batch.col("x").expect("?x must be in output schema");
        assert!(
            matches!(batch.rows[0].0[x_idx], Slot::Term(_)),
            "?x from BIND should be Slot::Term; got {:?}",
            batch.rows[0].0[x_idx]
        );
    }

    /// Regression: Join(LeftJoin(A,B), BGP(C)) where the OPTIONAL makes ?v
    /// Slot::Term-or-Unbound on the left and Slot::Id on the right →
    /// column mixing → DISTINCT deduplication failure (bug fixed in #128).
    #[test]
    fn distinct_join_over_optional_no_column_mixing() {
        let mut horn = HornBackend::new();
        let iri = |s: &str| Term::Iri(format!("http://ex/{s}"));
        horn.insert_triple(iri("s1"), iri("p"), iri("a0"));
        horn.insert_triple(iri("s1"), iri("opt"), iri("X"));
        horn.insert_triple(iri("s2"), iri("p"), iri("a0"));
        horn.insert_triple(iri("X"), iri("r"), iri("o1"));

        let q = "SELECT DISTINCT ?v WHERE { \
            ?a <http://ex/p> ?a0 . \
            OPTIONAL { ?a <http://ex/opt> ?v } \
            ?v <http://ex/r> ?o }";
        let plan = plan_select(q);

        let got: Vec<Bindings> = Runtime::new(&horn).run(&plan).unwrap().collect();
        assert_eq!(
            got.len(),
            1,
            "DISTINCT must deduplicate: got {} rows, want 1\nrows: {got:?}",
            got.len()
        );
        let v = got[0].get("v").expect("?v must be bound");
        assert_eq!(
            v,
            &Term::Iri("http://ex/X".into()),
            "?v must be <http://ex/X>"
        );
    }

    /// Recursively check whether a physical plan contains an inner `Join` node.
    fn contains_inner_join(p: &PhysicalPlan) -> bool {
        match p {
            PhysicalPlan::Join { .. } => true,
            PhysicalPlan::LeftJoin { left, right, .. }
            | PhysicalPlan::Union { left, right }
            | PhysicalPlan::Minus { left, right } => {
                contains_inner_join(left) || contains_inner_join(right)
            }
            PhysicalPlan::Filter { inner, .. }
            | PhysicalPlan::Project { inner, .. }
            | PhysicalPlan::Distinct { inner }
            | PhysicalPlan::Slice { inner, .. }
            | PhysicalPlan::OrderBy { inner, .. }
            | PhysicalPlan::Extend { inner, .. }
            | PhysicalPlan::Group { inner, .. } => contains_inner_join(inner),
            PhysicalPlan::PathClosure { edge, .. } => contains_inner_join(edge),
            PhysicalPlan::BgpScan { .. }
            | PhysicalPlan::CountScan { .. }
            | PhysicalPlan::GroupCountScan { .. }
            | PhysicalPlan::Values { .. } => false,
        }
    }

    /// Multi-row inner `Join` on a shared bound variable: some left rows match
    /// several right rows, some match none. The hash-join refactor must produce
    /// the exact same result multiset as the nested loop, so correct bucketing
    /// by the decoded join-var key is required. Forced through the inner `Join`
    /// arm by joining an outer BGP with a sub-SELECT (which spargebra keeps as a
    /// `Join`, not a merged BGP).
    #[test]
    fn inner_join_multi_row_shared_var() {
        let mut horn = HornBackend::new();
        let iri = |s: &str| Term::Iri(format!("http://ex/{s}"));
        // ?s <p> ?o
        horn.insert_triple(iri("s1"), iri("p"), iri("o1"));
        horn.insert_triple(iri("s2"), iri("p"), iri("o2"));
        horn.insert_triple(iri("s3"), iri("p"), iri("o3")); // o3 has no <q> → no match
                                                            // ?o <q> ?o2
        horn.insert_triple(iri("o1"), iri("q"), iri("a1")); // o1 joins to two o2 values
        horn.insert_triple(iri("o1"), iri("q"), iri("a2"));
        horn.insert_triple(iri("o2"), iri("q"), iri("b1"));

        // Outer BGP joined with a sub-SELECT on the shared bound var ?o.
        let q = "SELECT ?s ?o2 WHERE { \
            ?s <http://ex/p> ?o . \
            { SELECT ?o ?o2 WHERE { ?o <http://ex/q> ?o2 } } }";
        let plan = plan_select(q);
        assert!(
            contains_inner_join(&plan),
            "test must exercise the inner Join arm; plan: {plan:?}"
        );

        let got: Vec<Bindings> = Runtime::new(&horn).run(&plan).unwrap().collect();
        let mut pairs: Vec<(String, String)> = got
            .iter()
            .map(|b| {
                let s = match b.get("s").expect("?s bound") {
                    Term::Iri(i) => i.clone(),
                    other => panic!("?s not IRI: {other:?}"),
                };
                let o2 = match b.get("o2").expect("?o2 bound") {
                    Term::Iri(i) => i.clone(),
                    other => panic!("?o2 not IRI: {other:?}"),
                };
                (s, o2)
            })
            .collect();
        pairs.sort();
        let want = vec![
            ("http://ex/s1".to_string(), "http://ex/a1".to_string()),
            ("http://ex/s1".to_string(), "http://ex/a2".to_string()),
            ("http://ex/s2".to_string(), "http://ex/b1".to_string()),
        ];
        assert_eq!(pairs, want, "inner join result multiset mismatch");
    }

    /// Regression: `Union` of a native-BGP branch (binds ?v as Slot::Id) and
    /// an adapter-backed branch (LeftJoin → Slot::Term for ?v), both yielding
    /// the SAME logical ?v, followed by DISTINCT ?v. Without restoring column
    /// homogeneity on the merged rows (`normalize_columns`), the ?v column
    /// mixes Id(x) and Term(x) for one logical value; DISTINCT keys them
    /// differently (KeyPart::Id ≠ KeyPart::Lex) → two rows instead of one.
    ///
    /// Green on the adapter-backed Union (all Slot::Term, trivially
    /// homogeneous) AND on the native port (where `normalize_columns` is what
    /// keeps it homogeneous). Drop the normalize call from the native Union
    /// arm and this test goes RED (got 2, want 1).
    #[test]
    fn distinct_union_mixed_provenance_no_column_mixing() {
        let mut horn = HornBackend::new();
        let iri = |s: &str| Term::Iri(format!("http://ex/{s}"));
        horn.insert_triple(iri("v1"), iri("p"), iri("o1"));
        horn.insert_triple(iri("v1"), iri("q"), iri("z1"));

        // Branch 1: native BGP → ?v is Slot::Id.
        // Branch 2: BGP + OPTIONAL (LeftJoin, adapter-backed) → ?v is Slot::Term.
        // Both bind ?v = <http://ex/v1>; DISTINCT must collapse to one row.
        let q = "SELECT DISTINCT ?v WHERE { \
            { ?v <http://ex/p> ?o } \
            UNION \
            { ?v <http://ex/p> ?o OPTIONAL { ?v <http://ex/q> ?z } } }";
        let plan = plan_select(q);

        let got: Vec<Bindings> = Runtime::new(&horn).run(&plan).unwrap().collect();
        assert_eq!(
            got.len(),
            1,
            "DISTINCT over mixed-provenance UNION must deduplicate: got {} rows, want 1\nrows: {got:?}",
            got.len()
        );
        let v = got[0].get("v").expect("?v must be bound");
        assert_eq!(
            v,
            &Term::Iri("http://ex/v1".into()),
            "?v must be <http://ex/v1>"
        );
    }

    /// Regression for the canonicalizing join key (#128 lever 3): a `Join`
    /// whose two sides bind the join variable with DIFFERENT provenance — the
    /// BGP scans `?v` as `Slot::Id`, the `VALUES` clause binds it as
    /// `Slot::Term` — must still match on the shared value. The old key decoded
    /// both sides to a lexical `String`, so provenance never leaked into the
    /// bucket. `row_join_key` now keys `Slot::Id` on its raw id and encodes
    /// `Slot::Term` back to its id via `encode_term`; drop that encode (key
    /// `Term` as `KeyPart::Lex` while `Id` keys `KeyPart::Id`) and the two rows
    /// land in different buckets — the join returns 0 rows instead of 1.
    #[test]
    fn join_key_canonicalizes_across_provenance() {
        let mut horn = HornBackend::new();
        let iri = |s: &str| Term::Iri(format!("http://ex/{s}"));
        horn.insert_triple(iri("s1"), iri("p"), iri("o1"));
        horn.insert_triple(iri("s2"), iri("p"), iri("o2"));

        // BGP `?v <p> ?o` → ?v is Slot::Id (native scan).
        // VALUES ?v { <s1> } → ?v is Slot::Term. The Join keys ?v across the
        // Id/Term provenance split; only <s1> may survive.
        let q = "SELECT ?v WHERE { \
            ?v <http://ex/p> ?o . \
            VALUES ?v { <http://ex/s1> } }";
        let plan = plan_select(q);

        let got: Vec<Bindings> = Runtime::new(&horn).run(&plan).unwrap().collect();
        assert_eq!(
            got.len(),
            1,
            "cross-provenance join must match on ?v: got {} rows, want 1\nrows: {got:?}",
            got.len()
        );
        assert_eq!(
            got[0].get("v"),
            Some(&Term::Iri("http://ex/s1".into())),
            "?v must be <http://ex/s1>"
        );
    }

    /// Native `OrderBy` must not decode the columns it does not use for sorting.
    /// BGP-scan columns must remain `Slot::Id` in the output batch — OrderBy
    /// only reorders rows, it never touches the slot contents. Only the
    /// transient `Bindings` built for comparison inside `sort_by` decode the
    /// order-key columns; those are dropped immediately after the sort.
    #[test]
    fn order_by_preserves_id_slots() {
        let mut horn = HornBackend::new();
        let iri = |s: &str| Term::Iri(format!("http://ex/{s}"));
        horn.insert_triple(iri("s1"), iri("p"), iri("b"));
        horn.insert_triple(iri("s2"), iri("p"), iri("a"));

        // SELECT ?s ?o WHERE { ?s <p> ?o } ORDER BY ?o
        // The order key (?o) is also a BGP column — after a native port it
        // must still be Slot::Id in the output (only decoded transiently for
        // the comparator, never written back).
        let plan = plan_select("SELECT ?s ?o WHERE { ?s <http://ex/p> ?o } ORDER BY ?o");

        let rt = Runtime::new(&horn);
        let batch = eval_to_batch(&rt, &plan);

        assert_eq!(batch.rows.len(), 2, "expected two result rows");

        let s_idx = batch.col("s").expect("?s must be in output schema");
        let o_idx = batch.col("o").expect("?o must be in output schema");

        for (i, row) in batch.rows.iter().enumerate() {
            assert!(
                matches!(row.0[s_idx], Slot::Id(_)),
                "row {i}: ?s from BGP scan should remain Slot::Id after OrderBy; \
                 got {:?}",
                row.0[s_idx]
            );
            assert!(
                matches!(row.0[o_idx], Slot::Id(_)),
                "row {i}: ?o (order key, BGP scan) should remain Slot::Id after OrderBy; \
                 got {:?}",
                row.0[o_idx]
            );
        }
    }

    /// ORDER BY over a multi-key DESC/ASC and over an unbound sort key, pinned
    /// to the explicit expected ordering for a fixed input. Unbound-sorts-first
    /// semantics (None < Some) are baked into `compare_decorated`; this guards
    /// the native arm's transient-decode path against regressions.
    #[test]
    fn order_by_multi_key_and_unbound() {
        let iri = |s: &str| Term::Iri(format!("http://ex/{s}"));

        // Multi-key: ORDER BY DESC(?p) ASC(?o). Two predicates so DESC(?p) is
        // non-trivial; ties on ?p broken by ASC(?o).
        let mut horn = HornBackend::new();
        horn.insert_triple(iri("s1"), iri("p1"), iri("o1"));
        horn.insert_triple(iri("s2"), iri("p2"), iri("o1"));
        horn.insert_triple(iri("s3"), iri("p1"), iri("o2"));

        let plan = plan_select("SELECT ?s ?p ?o WHERE { ?s ?p ?o } ORDER BY DESC(?p) ASC(?o)");
        let got: Vec<Bindings> = Runtime::new(&horn).run(&plan).unwrap().collect();

        let triple = |b: &Bindings| {
            (
                b.get("s").cloned(),
                b.get("p").cloned(),
                b.get("o").cloned(),
            )
        };
        // DESC(?p): p2 group first; within p1, ASC(?o): o1 before o2.
        let expected = vec![
            (Some(iri("s2")), Some(iri("p2")), Some(iri("o1"))),
            (Some(iri("s1")), Some(iri("p1")), Some(iri("o1"))),
            (Some(iri("s3")), Some(iri("p1")), Some(iri("o2"))),
        ];
        assert_eq!(
            got.iter().map(triple).collect::<Vec<_>>(),
            expected,
            "ORDER BY DESC(?p) ASC(?o) produced the wrong order"
        );

        // Unbound key sorts first (None < Some): s1 gets ?extra, s3 does not;
        // ORDER BY ?extra ASC must place the unbound (s3) row first.
        let mut horn2 = HornBackend::new();
        horn2.insert_triple(iri("s1"), iri("p1"), iri("o1"));
        horn2.insert_triple(iri("s1"), iri("p2"), iri("e1")); // s1 → ?extra = e1
        horn2.insert_triple(iri("s3"), iri("p1"), iri("o2")); // s3 → ?extra unbound

        let plan2 = plan_select(
            "SELECT ?s ?extra WHERE { \
             ?s <http://ex/p1> ?o \
             OPTIONAL { ?s <http://ex/p2> ?extra } \
             } ORDER BY ?extra",
        );
        let got2: Vec<Bindings> = Runtime::new(&horn2).run(&plan2).unwrap().collect();

        let pair = |b: &Bindings| (b.get("s").cloned(), b.get("extra").cloned());
        let expected2 = vec![
            (Some(iri("s3")), None), // ?extra unbound → sorts first
            (Some(iri("s1")), Some(iri("e1"))),
        ];
        assert_eq!(
            got2.iter().map(pair).collect::<Vec<_>>(),
            expected2,
            "ORDER BY over an unbound key must sort the unbound row first"
        );
    }

    /// Native `LeftJoin` (OPTIONAL) must not decode the columns it inherits
    /// from its children. A matched left row carries the left BGP-scan columns
    /// as `Slot::Id` (only the right-side columns come from the right child,
    /// also `Slot::Id` here); an unmatched left row carries `Slot::Unbound`
    /// for the right-only var. The OPTIONAL's join var (?s) is keyed by
    /// decoded lexical but the *output* column is not rewritten, so it stays
    /// `Slot::Id`.
    #[test]
    fn left_join_preserves_id_slots_and_unbound() {
        let mut horn = HornBackend::new();
        let iri = |s: &str| Term::Iri(format!("http://ex/{s}"));
        // s1 has a label (matched); s2 has none (unmatched → ?l Unbound).
        horn.insert_triple(iri("s1"), iri("type"), iri("T"));
        horn.insert_triple(iri("s2"), iri("type"), iri("T"));
        horn.insert_triple(iri("s1"), iri("label"), iri("L"));

        let plan = plan_select(
            "SELECT ?s ?l WHERE { \
             ?s <http://ex/type> <http://ex/T> . \
             OPTIONAL { ?s <http://ex/label> ?l } }",
        );

        let rt = Runtime::new(&horn);
        let batch = eval_to_batch(&rt, &plan);
        assert_eq!(batch.rows.len(), 2, "two left rows survive the OPTIONAL");

        let s_idx = batch.col("s").expect("?s must be in output schema");
        let l_idx = batch.col("l").expect("?l must be in output schema");

        // ?s is a left BGP-scan column on every row → Slot::Id (native port).
        for (i, row) in batch.rows.iter().enumerate() {
            assert!(
                matches!(row.0[s_idx], Slot::Id(_)),
                "row {i}: ?s from BGP scan should remain Slot::Id after native \
                 LeftJoin; got {:?}",
                row.0[s_idx]
            );
        }

        // Exactly one matched (?l = Slot::Id <L>) and one unmatched (?l Unbound).
        let mut matched = 0;
        let mut unbound = 0;
        for row in &batch.rows {
            match &row.0[l_idx] {
                Slot::Id(_) => matched += 1,
                Slot::Unbound => unbound += 1,
                Slot::Term(t) => panic!("?l should be Id or Unbound, got Term({t:?})"),
            }
        }
        assert_eq!((matched, unbound), (1, 1), "one matched, one unmatched");
    }

    /// GROUP BY ?c with COUNT(DISTINCT *) must count distinct *solution
    /// mappings*, not raw BGP rows. Pins the id-based distinct-key path
    /// (KeyPart over slot rows) on the current materialised runtime.
    ///
    /// cat A: distinct (?e,?c,?k) mappings = {(e1,A,x),(e2,A,x),(e3,A,y)} = 3
    /// cat B: {(e4,B,x),(e5,B,y)} = 2
    ///
    /// The duplicate insert of (e1,cat,A) exercises storage-level dedup;
    /// it must not inflate the COUNT.
    #[test]
    fn group_by_count_distinct_star_is_deterministic() {
        let mut horn = HornBackend::new();
        let iri = |s: &str| Term::Iri(format!("http://ex/{s}"));
        for (s, p, o) in [
            ("e1", "cat", "A"),
            ("e1", "kind", "x"),
            ("e2", "cat", "A"),
            ("e2", "kind", "x"),
            ("e3", "cat", "A"),
            ("e3", "kind", "y"),
            ("e1", "cat", "A"), // duplicate triple — must not double-count
            ("e4", "cat", "B"),
            ("e4", "kind", "x"),
            ("e5", "cat", "B"),
            ("e5", "kind", "y"),
        ] {
            horn.insert_triple(iri(s), iri(p), iri(o));
        }
        let plan = plan_select(
            "SELECT ?c (COUNT(DISTINCT *) AS ?n) WHERE { \
             ?e <http://ex/cat> ?c . ?e <http://ex/kind> ?k } \
             GROUP BY ?c ORDER BY ?c",
        );
        let rows: Vec<_> = Runtime::new(&horn).run(&plan).unwrap().collect();
        assert_eq!(rows.len(), 2, "expected two groups (A and B)");

        let expected = vec![
            (iri("A"), integer_literal(3)),
            (iri("B"), integer_literal(2)),
        ];
        let got: Vec<_> = rows
            .iter()
            .map(|b| {
                (
                    b.get("c").cloned().expect("?c must be bound"),
                    b.get("n").cloned().expect("?n must be bound"),
                )
            })
            .collect();
        assert_eq!(
            got, expected,
            "GROUP BY ?c ORDER BY ?c with COUNT(DISTINCT *): wrong counts or order"
        );
    }

    // -----------------------------------------------------------------
    // HDB-101: decorate-sort-undecorate + ORDER BY/LIMIT top-k fusion
    // -----------------------------------------------------------------

    /// Rows of a SELECT, rendered in emission order.
    fn ordered_rows(horn: &HornBackend, q: &str) -> Vec<String> {
        let plan = plan_select(q);
        Runtime::new(horn)
            .run(&plan)
            .unwrap()
            .map(|b| format!("{b:?}"))
            .collect()
    }

    /// The core HDB-101 correctness property: for every OFFSET/LIMIT window,
    /// the fused top-k plan must return exactly the window the *full* sort
    /// would have returned, in the same order. `base` is the query without a
    /// slice; `windows` are `(offset, limit)` pairs to check.
    fn assert_slice_matches_full_sort(horn: &HornBackend, base: &str, windows: &[(usize, usize)]) {
        let full = ordered_rows(horn, base);
        for &(offset, limit) in windows {
            let sliced = ordered_rows(horn, &format!("{base} LIMIT {limit} OFFSET {offset}"));
            let want: Vec<String> = full.iter().skip(offset).take(limit).cloned().collect();
            assert_eq!(
                sliced, want,
                "{base} LIMIT {limit} OFFSET {offset} diverged from the full sort"
            );
        }
    }

    /// Every window of an all-ties sort key. Ties are where a top-k heap is
    /// easiest to get wrong: `ORDER BY` is stable, so the surviving rows must
    /// be the *earliest* equal-key rows, in input order — not an arbitrary
    /// `n` of them.
    #[test]
    fn top_k_ties_match_full_sort_at_every_window() {
        let iri = |s: &str| Term::Iri(format!("http://ex/{s}"));
        let mut horn = HornBackend::new();
        // 12 subjects, 3 distinct sort keys -> 4-way ties on each key.
        for i in 0..12 {
            horn.insert_triple(
                iri(&format!("s{i:02}")),
                iri("k"),
                Term::Literal(format!("\"{}\"", i % 3)),
            );
        }
        let base = "SELECT ?s ?k WHERE { ?s <http://ex/k> ?k } ORDER BY ?k ?s";
        // Includes `LIMIT 0` at every offset (`l == 0`) — the shape that
        // reaches `compute_top_k` with `n == 0`.
        let windows: Vec<(usize, usize)> = (0..=12)
            .flat_map(|o| (0..=13).map(move |l| (o, l)))
            .collect();
        assert_slice_matches_full_sort(&horn, base, &windows);

        // Same, but with the tie left unbroken by a second key: the stable
        // sort's input order is the only thing deciding which rows survive.
        let base = "SELECT ?s ?k WHERE { ?s <http://ex/k> ?k } ORDER BY ?k";
        assert_slice_matches_full_sort(&horn, base, &windows);
    }

    /// The q3 shape: DESC over a numeric column, which resolves to a
    /// `SortCol::Num` and so takes the top-k heap.
    #[test]
    fn top_k_desc_numeric_matches_full_sort() {
        let iri = |s: &str| Term::Iri(format!("http://ex/{s}"));
        let mut horn = HornBackend::new();
        // Values chosen so lexical and numeric order disagree (9 > 10
        // lexically, 9 < 10 numerically) — a decorated key that lost the
        // numeric coercion would show up here.
        for (i, v) in [7, 100, 9, 42, 10, 3, 88, 1, 55, 9].iter().enumerate() {
            horn.insert_triple(
                iri(&format!("s{i}")),
                iri("amount"),
                Term::Literal(format!(
                    "\"{v}\"^^<http://www.w3.org/2001/XMLSchema#integer>"
                )),
            );
        }
        let base = "SELECT ?s ?a WHERE { ?s <http://ex/amount> ?a } ORDER BY DESC(?a)";
        assert_slice_matches_full_sort(&horn, base, &[(0, 1), (0, 3), (2, 3), (5, 5), (0, 20)]);

        // Pin the actual order, so a comparator that silently went lexical
        // fails here rather than agreeing with an equally-wrong full sort.
        let top = ordered_rows(&horn, &format!("{base} LIMIT 3"));
        assert!(
            top[0].contains("100") && top[1].contains("88") && top[2].contains("55"),
            "DESC numeric order wrong: {top:?}"
        );
    }

    /// A key column holding both numbers and non-numbers is a `SortCol::Mixed`
    /// — `compare_terms` picks its branch per pair there, so the comparator is
    /// not guaranteed transitive and `compute_top_k` must fall back to the
    /// full sort rather than trust a heap.
    #[test]
    fn top_k_mixed_numeric_and_lexical_column_matches_full_sort() {
        let iri = |s: &str| Term::Iri(format!("http://ex/{s}"));
        let mut horn = HornBackend::new();
        for (i, v) in ["10", "abc", "9", "zed", "2", "beta"].iter().enumerate() {
            horn.insert_triple(
                iri(&format!("s{i}")),
                iri("k"),
                Term::Literal(format!("\"{v}\"")),
            );
        }
        let base = "SELECT ?s ?k WHERE { ?s <http://ex/k> ?k } ORDER BY ?k";
        assert_slice_matches_full_sort(&horn, base, &[(0, 1), (0, 2), (1, 3), (0, 6), (3, 10)]);
    }

    /// An unbound sort key (OPTIONAL) sorts first, and must keep doing so
    /// under the fusion — including when the whole LIMIT window falls inside
    /// the unbound rows.
    #[test]
    fn top_k_unbound_key_matches_full_sort() {
        let iri = |s: &str| Term::Iri(format!("http://ex/{s}"));
        let mut horn = HornBackend::new();
        for i in 0..6 {
            horn.insert_triple(iri(&format!("s{i}")), iri("p"), iri("o"));
        }
        // Only half the subjects get the sort key.
        for i in [1, 3, 5] {
            horn.insert_triple(
                iri(&format!("s{i}")),
                iri("k"),
                Term::Literal(format!("\"{i}\"")),
            );
        }
        let base = "SELECT ?s ?k WHERE { ?s <http://ex/p> <http://ex/o> \
                    OPTIONAL { ?s <http://ex/k> ?k } } ORDER BY ?k ?s";
        assert_slice_matches_full_sort(&horn, base, &[(0, 1), (0, 3), (2, 2), (4, 4), (0, 9)]);
    }

    /// A computed (non-bare-variable) sort key still goes through the
    /// decorate step, and DISTINCT between the sort and the slice must block
    /// the fusion rather than truncate the wrong rows.
    #[test]
    fn top_k_computed_key_and_distinct_match_full_sort() {
        let iri = |s: &str| Term::Iri(format!("http://ex/{s}"));
        let mut horn = HornBackend::new();
        for i in 0..8 {
            horn.insert_triple(
                iri(&format!("s{i}")),
                iri("n"),
                Term::Literal(format!(
                    "\"{i}\"^^<http://www.w3.org/2001/XMLSchema#integer>"
                )),
            );
        }
        let base = "SELECT ?s ?n WHERE { ?s <http://ex/n> ?n } ORDER BY DESC(?n + 1)";
        assert_slice_matches_full_sort(&horn, base, &[(0, 2), (3, 3), (0, 8)]);

        // DISTINCT drops rows *after* the sort: only three distinct ?n values
        // survive, so a top-k that fed it 2 sorted rows would be short.
        let mut dup = HornBackend::new();
        for i in 0..9 {
            dup.insert_triple(
                iri(&format!("s{i}")),
                iri("n"),
                Term::Literal(format!("\"{}\"", i % 3)),
            );
        }
        let full = ordered_rows(
            &dup,
            "SELECT DISTINCT ?n WHERE { ?s <http://ex/n> ?n } ORDER BY ?n",
        );
        let limited = ordered_rows(
            &dup,
            "SELECT DISTINCT ?n WHERE { ?s <http://ex/n> ?n } ORDER BY ?n LIMIT 2",
        );
        assert_eq!(limited, full[..2].to_vec(), "DISTINCT + LIMIT lost rows");
    }

    /// `top_k_order` must be safe on its own terms. `LIMIT 0` reaches
    /// `compute_top_k` as `n == 0`, and this is the exact call that indexed
    /// an empty heap at `heap[0]` before the guard went in — it did not panic
    /// in a real query only because `SliceOp` short-circuits `remaining ==
    /// Some(0)` before ever pulling its child, a guarantee in another file
    /// that nothing tied to this one.
    #[test]
    fn top_k_order_handles_zero_n() {
        let cols = vec![(SortCol::Num(vec![Some(1.0), Some(2.0)]), OrderDir::Asc)];
        assert_eq!(top_k_order(&cols, 2, 0), Vec::<usize>::new());
        assert_eq!(top_k_order(&cols, 2, 1), vec![0]);
    }

    /// `SortCol::classify` decides whether the heap may run at all, so pin
    /// what it decides. A parity test alone cannot tell "the fallback fired"
    /// from "the heap happened to agree".
    #[test]
    fn sort_col_classify_gates_the_heap() {
        let val = |s: &str| Some(SortVal::of(&Term::Literal(format!("\"{s}\""))));

        let numeric = SortCol::classify(vec![val("1"), None, val("2")]);
        assert!(matches!(numeric, SortCol::Num(_)));
        assert!(numeric.is_total_order());

        let lexical = SortCol::classify(vec![val("abc"), None, val("zed")]);
        assert!(matches!(lexical, SortCol::Lex(_)));
        assert!(lexical.is_total_order());

        // What `top_k_mixed_numeric_and_lexical_column_matches_full_sort`
        // exercises end to end: `compare_terms` picks its branch per pair
        // here, so the heap must be refused.
        let mixed = SortCol::classify(vec![val("10"), val("abc")]);
        assert!(matches!(mixed, SortCol::Mixed(_)));
        assert!(
            !mixed.is_total_order(),
            "a mixed column must refuse the heap"
        );

        // The other half of the refusal: NaN compares Equal to everything, so
        // an all-numeric column is still not transitive with one in it.
        let nan = SortCol::classify(vec![val("1"), val("NaN"), val("2")]);
        assert!(matches!(nan, SortCol::Num(_)));
        assert!(
            !nan.is_total_order(),
            "a NaN-carrying numeric column must refuse the heap"
        );
    }

    /// The NaN fallback end to end: the fused window must still equal the
    /// full sort's, even though the comparator is not transitive.
    #[test]
    fn top_k_nan_column_matches_full_sort() {
        let iri = |s: &str| Term::Iri(format!("http://ex/{s}"));
        let mut horn = HornBackend::new();
        for (i, v) in ["3", "NaN", "1", "2", "NaN", "5"].iter().enumerate() {
            horn.insert_triple(
                iri(&format!("s{i}")),
                iri("k"),
                Term::Literal(format!(
                    "\"{v}\"^^<http://www.w3.org/2001/XMLSchema#double>"
                )),
            );
        }
        let base = "SELECT ?s ?k WHERE { ?s <http://ex/k> ?k } ORDER BY ?k";
        assert_slice_matches_full_sort(&horn, base, &[(0, 1), (0, 3), (2, 2), (0, 6), (4, 5)]);
    }

    /// Multi-key with the two directions disagreeing: a fused window must
    /// still match, and the second key's direction must survive the
    /// tie-break. `DESC(?a)` groups the rows, `ASC(?b)` orders within a
    /// group, and equal `(?a, ?b)` pairs fall back to input order.
    #[test]
    fn top_k_mixed_asc_desc_multi_key_matches_full_sort() {
        let iri = |s: &str| Term::Iri(format!("http://ex/{s}"));
        let mut horn = HornBackend::new();
        // Three ?a groups of three, with ?b repeating inside each group so
        // both the second key and the input-order tie-break are exercised.
        for i in 0..9 {
            let s = iri(&format!("s{i}"));
            horn.insert_triple(
                s.clone(),
                iri("a"),
                Term::Literal(format!(
                    "\"{}\"^^<http://www.w3.org/2001/XMLSchema#integer>",
                    i / 3
                )),
            );
            horn.insert_triple(s, iri("b"), Term::Literal(format!("\"b{}\"", i % 2)));
        }
        let base = "SELECT ?s ?a ?b WHERE { ?s <http://ex/a> ?a ; <http://ex/b> ?b } \
                    ORDER BY DESC(?a) ASC(?b)";
        let windows: Vec<(usize, usize)> = (0..=9)
            .flat_map(|o| (0..=10).map(move |l| (o, l)))
            .collect();
        assert_slice_matches_full_sort(&horn, base, &windows);
    }
}

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
        assert_eq!(
            eval_expr(&same, &b_hit).unwrap(),
            eval_expr(&eq, &b_hit).unwrap()
        );
        assert_eq!(
            eval_expr(&same, &b_miss).unwrap(),
            eval_expr(&eq, &b_miss).unwrap()
        );
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
        assert_eq!(
            vars,
            ["p".to_string(), "q".to_string()].into_iter().collect()
        );
    }
}
