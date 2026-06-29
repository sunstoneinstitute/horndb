//! Iterator-style runtime over [`PhysicalPlan`]. `eval` returns [`Batch`]
//! (id-carrying slot rows); `run` decodes once at the boundary via
//! `decode_term`. Each operator arm still materialises per-node (true
//! streaming is Future Work). Every operator runs native on slot rows —
//! there is a single runtime (the test-only string oracle that gated the slot
//! port was removed once Slice 2 landed).

use crate::algebra::{
    AggFunc, Aggregate, Expr, Func, OrderDir, Term, Var, PATH_DST_VAR, PATH_SRC_VAR,
};
use crate::error::{Result, SparqlError};
use crate::exec::{Batch, Bindings, Executor, KeyPart, Row, Slot};
use crate::plan::PhysicalPlan;
use std::collections::{HashMap, HashSet};

pub struct Runtime<'a, E: Executor + ?Sized> {
    exec: &'a E,
}

impl<'a, E: Executor + ?Sized> Runtime<'a, E> {
    pub fn new(exec: &'a E) -> Self {
        Self { exec }
    }

    /// Execute the plan and return all solution mappings.
    pub fn run(&self, plan: &PhysicalPlan) -> Result<std::vec::IntoIter<Bindings>> {
        let batch = self.eval(plan)?;
        let rows = batch.to_bindings(|id| self.exec.decode_term(id))?;
        Ok(rows.into_iter())
    }

    /// Evaluate a plan node to a [`Batch`]. All operator arms run native on
    /// slot rows — one runtime, no decode adapter.
    fn eval(&self, plan: &PhysicalPlan) -> Result<Batch> {
        match plan {
            PhysicalPlan::BgpScan { patterns } => self.exec.scan_bgp_ids(patterns),
            PhysicalPlan::Join { left, right } => {
                let l = self.eval(left)?;
                let r = self.eval(right)?;
                // Output schema = left schema ++ right-only vars.
                let mut out_schema = l.schema.clone();
                for v in &r.schema {
                    if !out_schema.iter().any(|x| x.name() == v.name()) {
                        out_schema.push(v.clone());
                    }
                }
                let mut rows = Vec::new();
                for a in &l.rows {
                    for b in &r.rows {
                        if let Some(m) = self.merge_rows(&l.schema, a, &r.schema, b, &out_schema)? {
                            rows.push(m);
                        }
                    }
                }
                // Restore within-column homogeneity: an adapter-backed child
                // (LeftJoin/Union → all Slot::Term) may leave a shared var as
                // Slot::Unbound on some rows; the native BGP child has Slot::Id
                // for that var. merge_rows takes the non-Unbound side, so the
                // output column holds both Term and Id for the same logical
                // value. Distinct/Group key on KeyPart where Id(x) ≠ Lex(x)
                // → equal solutions hash differently → wrong results.
                // Only genuinely mixed (Id ∧ Term) columns are decoded; the
                // pure-Id BGP-only aggregation hot path pays zero decode.
                self.normalize_columns(&mut rows, out_schema.len())?;
                Ok(Batch {
                    schema: out_schema,
                    rows,
                })
            }
            PhysicalPlan::LeftJoin { left, right, expr } => {
                // Native slot hash-left-join: O(|l|+|r|) via a hash index on
                // the join-variable key, with a
                // conservative `unkeyed` bucket for rows that leave a jvar
                // unbound. Output provenance mixes exactly like Join (matched
                // rows carry right slots, unmatched carry Unbound), so the
                // merged rows are `normalize_columns`'d before returning.
                let l = self.eval(left)?;
                let r = self.eval(right)?;

                // Output schema = left schema ++ right-only vars (like Join).
                let mut out_schema = l.schema.clone();
                for v in &r.schema {
                    if !out_schema.iter().any(|x| x.name() == v.name()) {
                        out_schema.push(v.clone());
                    }
                }

                let jvars = batch_join_vars(&l, &r);

                // Index the right relation by its decoded join key (option (b)
                // — see `row_join_key`); rows missing a jvar fall to `unkeyed`.
                let mut index: HashMap<Vec<String>, Vec<&Row>> = HashMap::new();
                let mut unkeyed: Vec<&Row> = Vec::new();
                for b in &r.rows {
                    match self.row_join_key(b, &r.schema, &jvars)? {
                        Some(k) => index.entry(k).or_default().push(b),
                        None => unkeyed.push(b),
                    }
                }

                // Columns the inner FILTER reads (decoded per merged row).
                let mut want = HashSet::new();
                if let Some(e) = expr.as_ref() {
                    referenced_vars(e, &mut want);
                }

                let mut rows = Vec::new();
                for a in &l.rows {
                    let mut matched = false;
                    match self.row_join_key(a, &l.schema, &jvars)? {
                        Some(k) => {
                            if let Some(bucket) = index.get(&k) {
                                matched |= self.probe_into_slots(
                                    &l.schema,
                                    a,
                                    &r.schema,
                                    bucket,
                                    &out_schema,
                                    expr.as_ref(),
                                    &want,
                                    &mut rows,
                                )?;
                            }
                            if !unkeyed.is_empty() {
                                matched |= self.probe_into_slots(
                                    &l.schema,
                                    a,
                                    &r.schema,
                                    &unkeyed,
                                    &out_schema,
                                    expr.as_ref(),
                                    &want,
                                    &mut rows,
                                )?;
                            }
                        }
                        // Left row missing a join var: may still be compatible
                        // with any right row on the remaining shared vars.
                        None => {
                            let all: Vec<&Row> = r.rows.iter().collect();
                            matched |= self.probe_into_slots(
                                &l.schema,
                                a,
                                &r.schema,
                                &all,
                                &out_schema,
                                expr.as_ref(),
                                &want,
                                &mut rows,
                            )?;
                        }
                    }
                    if !matched {
                        // OPTIONAL: the left row survives with right-only vars
                        // unbound. Merging with an all-Unbound right row takes
                        // the left side and leaves right vars Unbound (emit the
                        // left row; right vars simply absent → decoded as
                        // Unbound).
                        let unbound = Row(vec![Slot::Unbound; r.schema.len()]);
                        if let Some(m) =
                            self.merge_rows(&l.schema, a, &r.schema, &unbound, &out_schema)?
                        {
                            rows.push(m);
                        }
                    }
                }

                self.normalize_columns(&mut rows, out_schema.len())?;
                Ok(Batch {
                    schema: out_schema,
                    rows,
                })
            }
            PhysicalPlan::Filter { expr, inner } => {
                let b = self.eval(inner)?;
                let mut want = std::collections::HashSet::new();
                referenced_vars(expr, &mut want);
                let mut rows = Vec::with_capacity(b.rows.len());
                for r in b.rows {
                    let env = self.decode_subset(&r, &b.schema, &want)?;
                    if eval_expr(expr, &env)? {
                        rows.push(r);
                    }
                }
                Ok(Batch {
                    schema: b.schema,
                    rows,
                })
            }
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
                // Place each child row's slots by var name, Unbound where the
                // branch does not bind that schema var.
                fn place(child: &Batch, schema: &[Var]) -> Vec<Row> {
                    child
                        .rows
                        .iter()
                        .map(|row| {
                            Row(schema
                                .iter()
                                .map(|v| match child.col(v.name()) {
                                    Some(i) => row.0[i].clone(),
                                    None => Slot::Unbound,
                                })
                                .collect())
                        })
                        .collect()
                }
                let mut rows = place(&l, &schema);
                rows.extend(place(&r, &schema));
                // Branches of differing provenance (native BGP → Slot::Id vs
                // adapter-backed → Slot::Term, or Unbound where a branch omits
                // a var) can leave a column mixing Id and Term for one logical
                // value; Distinct/Group would then key Id(x) ≠ Lex(x). Restore
                // within-column homogeneity (pure-Id columns pay zero decode).
                self.normalize_columns(&mut rows, schema.len())?;
                Ok(Batch { schema, rows })
            }
            PhysicalPlan::Project { vars, inner } => {
                let b = self.eval(inner)?;
                if vars.is_empty() {
                    // SELECT * / ASK: keep everything (parity with project()).
                    return Ok(b);
                }
                // New schema = projected vars that exist in the input, in
                // projection order; remap each row's slots by index.
                let idx: Vec<Option<usize>> = vars.iter().map(|v| b.col(v.name())).collect();
                let schema: Vec<Var> = vars
                    .iter()
                    .zip(&idx)
                    .filter(|(_, i)| i.is_some())
                    .map(|(v, _)| v.clone())
                    .collect();
                let rows = b
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
            PhysicalPlan::Distinct { inner } => {
                let b = self.eval(inner)?;
                let mut seen: std::collections::HashSet<Vec<KeyPart>> =
                    std::collections::HashSet::with_capacity(b.rows.len());
                let mut rows = Vec::with_capacity(b.rows.len());
                for r in b.rows {
                    let key: Vec<KeyPart> = r.0.iter().map(|s| s.key_part()).collect();
                    if seen.insert(key) {
                        rows.push(r);
                    }
                }
                Ok(Batch {
                    schema: b.schema,
                    rows,
                })
            }
            PhysicalPlan::Slice {
                inner,
                start,
                length,
            } => {
                let mut b = self.eval(inner)?;
                let s = (*start).min(b.rows.len());
                let take = length.unwrap_or(b.rows.len() - s);
                b.rows = b.rows.into_iter().skip(s).take(take).collect();
                Ok(b)
            }
            PhysicalPlan::OrderBy { inner, keys } => {
                let b = self.eval(inner)?;
                let mut want = HashSet::new();
                for (e, _) in keys {
                    referenced_vars(e, &mut want);
                }
                // Pull schema and rows apart so the borrow checker sees two
                // independent moves (no partial-move ambiguity on `b`).
                let schema = b.schema;
                let mut tagged: Vec<(Bindings, Row)> = b
                    .rows
                    .into_iter()
                    .map(|r| {
                        let env = self.decode_subset(&r, &schema, &want)?;
                        Ok((env, r))
                    })
                    .collect::<Result<Vec<_>>>()?;
                // stable sort: equal-key rows keep their input order,
                // matching the old Vec::sort_by behaviour.
                tagged.sort_by(|(ea, _), (eb, _)| compare_by_keys(ea, eb, keys));
                Ok(Batch {
                    schema,
                    rows: tagged.into_iter().map(|(_, r)| r).collect(),
                })
            }
            PhysicalPlan::Extend { inner, var, expr } => {
                // Native slot path: decode only the vars the expression reads,
                // preserving Slot::Id for all other columns (e.g. BGP scan ids).
                // Mirrors the Filter arm — same referenced_vars + decode_subset seam.
                //
                // re-BIND semantics: SPARQL 1.1 §18.1.10 forbids BIND targeting a
                // var already in scope; spargebra enforces this at parse time
                // ("BIND is overriding an existing variable"). Therefore `existing`
                // is always `None` in production — the `Some` branch is dead code
                // kept for safety.  The adapter's None-result behaviour (leave the
                // prior binding unchanged) differs from the native path (overwrite
                // with Slot::Unbound); since the branch is unreachable, the
                // divergence never surfaces.
                let b = self.eval(inner)?;
                let mut want = HashSet::new();
                referenced_vars(expr, &mut want);
                let existing = b.col(var.name()); // Some(i) ⇒ re-BIND (dead code)
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
                Ok(Batch {
                    schema,
                    rows: out_rows,
                })
            }
            PhysicalPlan::Values { vars, rows } => {
                // Rows are guaranteed full-width by the spargebra parser (it
                // rejects `VALUES` clauses where any row length != vars.len()),
                // so `zip` stops correctly and no trailing-Unbound padding is
                // needed.
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
                Ok(Batch {
                    schema,
                    rows: out_rows,
                })
            }
            PhysicalPlan::Group {
                inner,
                keys,
                aggregates,
            } => {
                let b = self.eval(inner)?;
                self.eval_group_native(b, keys, aggregates)
            }
            PhysicalPlan::PathClosure {
                subject,
                object,
                edge,
                reflexive,
            } => {
                let eb = self.eval(edge)?;
                // The edge batch binds exactly the two synthetic endpoint vars; decode
                // only those, then reuse the string BFS unchanged.
                // deferred: id-native BFS (#128)
                let want: HashSet<String> = [PATH_SRC_VAR, PATH_DST_VAR]
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                let edge_rows: Vec<Bindings> = eb
                    .rows
                    .iter()
                    .map(|r| self.decode_subset(r, &eb.schema, &want))
                    .collect::<Result<Vec<_>>>()?;
                Ok(Batch::from_bindings(eval_path_closure(
                    subject, object, &edge_rows, *reflexive,
                )?))
            }
        }
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

    fn eval_group_native(&self, b: Batch, keys: &[Var], aggregates: &[Aggregate]) -> Result<Batch> {
        let key_idx: Vec<Option<usize>> = keys.iter().map(|k| b.col(k.name())).collect();

        struct Grp {
            key_slots: Vec<Slot>,
            members: Vec<Row>,
        }
        let mut groups: HashMap<Vec<KeyPart>, Grp> = HashMap::new();
        for r in b.rows {
            let gkey: Vec<KeyPart> = key_idx
                .iter()
                .map(|i| i.map(|i| r.0[i].key_part()).unwrap_or(KeyPart::Unbound))
                .collect();
            let entry = groups.entry(gkey).or_insert_with(|| Grp {
                key_slots: key_idx
                    .iter()
                    .map(|i| i.map(|i| r.0[i].clone()).unwrap_or(Slot::Unbound))
                    .collect(),
                members: Vec::new(),
            });
            entry.members.push(r);
        }

        // Implicit grouping with no input rows still yields one empty group
        // (SPARQL §11.2: COUNT(*) of nothing is one row with 0).
        if keys.is_empty() && groups.is_empty() {
            groups.insert(
                Vec::new(),
                Grp {
                    key_slots: Vec::new(),
                    members: Vec::new(),
                },
            );
        }

        // Output schema = keys ++ aggregate output vars.
        let mut schema: Vec<Var> = keys.to_vec();
        for agg in aggregates {
            schema.push(agg.out.clone());
        }

        // Which input columns each aggregate's inner expression references.
        let agg_vars: Vec<HashSet<String>> = aggregates
            .iter()
            .map(|agg| {
                let mut s = HashSet::new();
                for e in agg_inner_exprs(agg) {
                    referenced_vars(e, &mut s);
                }
                s
            })
            .collect();

        let mut out: Vec<(Vec<Option<String>>, Row)> = Vec::with_capacity(groups.len());
        for grp in groups.into_values() {
            let mut slots: Vec<Slot> = grp.key_slots.clone();

            for (agg, want) in aggregates.iter().zip(&agg_vars) {
                let value = if matches!(agg.func, AggFunc::CountStar) && !agg.distinct {
                    // COUNT(*) fast path: member count needs no decode at all.
                    Some(integer_literal(grp.members.len() as i64))
                } else if matches!(agg.func, AggFunc::CountStar) {
                    // COUNT(DISTINCT *): distinct whole-solution rows.
                    // agg_inner_exprs returns empty for CountStar, so `want`
                    // is empty and decode_subset would yield empty Bindings
                    // for every row — all deduped to 1, wrong count.
                    // Instead, key on Vec<KeyPart> directly: within-column
                    // homogeneity ensures same-value cells hash identically,
                    // giving the same deduplication as the old
                    // `HashSet<&Bindings>` path without any decode.
                    let distinct: HashSet<Vec<KeyPart>> = grp
                        .members
                        .iter()
                        .map(|r| r.0.iter().map(|s| s.key_part()).collect())
                        .collect();
                    Some(integer_literal(distinct.len() as i64))
                } else {
                    let members_decoded: Vec<Bindings> = grp
                        .members
                        .iter()
                        .map(|r| self.decode_subset(r, &b.schema, want))
                        .collect::<Result<Vec<_>>>()?;
                    eval_aggregate(agg, &members_decoded)?
                };
                match value {
                    Some(t) => slots.push(Slot::Term(t)),
                    None => slots.push(Slot::Unbound),
                }
            }

            // Sort key: decoded lexical of each group key slot. Reproduces
            // the pre-#128 BTreeMap<Vec<Option<String>>> lexical ordering
            // exactly (None < Some(...) in BTreeMap order is the same as
            // Option<String> Ord ordering used in sort_by).
            let sort_key: Vec<Option<String>> = grp
                .key_slots
                .iter()
                .map(|s| match s {
                    Slot::Unbound => Ok(None),
                    Slot::Id(id) => self.exec.decode_term(*id).map(|t| Some(lex(&t))),
                    Slot::Term(t) => Ok(Some(lex(t))),
                })
                .collect::<Result<Vec<_>>>()?;
            out.push((sort_key, Row(slots)));
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));

        Ok(Batch {
            schema,
            rows: out.into_iter().map(|(_, r)| r).collect(),
        })
    }

    /// Decode Id cells in columns that mix Slot::Id and Slot::Term, restoring
    /// the within-column homogeneity invariant for any operator that unions or
    /// merges children of differing slot provenance (Join, Union, LeftJoin).
    ///
    /// See the comment in the Join arm for the full explanation of why mixing
    /// occurs (adapter-backed child leaves Slot::Unbound on some rows while
    /// the native BGP child has Slot::Id → merge_rows takes Id on those rows).
    fn normalize_columns(&self, rows: &mut [Row], width: usize) -> Result<()> {
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
    /// rule), producing the union row over `out_schema`. Returns None if any
    /// shared bound var disagrees. Mirrors `Bindings::extend_compat` on slots:
    /// an `Unbound` slot is treated as an absent var (a wildcard that never
    /// conflicts), matching how `Bindings` simply lacks an unbound key.
    fn merge_rows(
        &self,
        ls: &[Var],
        l: &Row,
        rs: &[Var],
        r: &Row,
        out_schema: &[Var],
    ) -> Result<Option<Row>> {
        let decode = |id| self.exec.decode_term(id);
        let lget = |name: &str| ls.iter().position(|v| v.name() == name).map(|i| &l.0[i]);
        let rget = |name: &str| rs.iter().position(|v| v.name() == name).map(|i| &r.0[i]);
        let mut slots = Vec::with_capacity(out_schema.len());
        for v in out_schema {
            let chosen = match (lget(v.name()), rget(v.name())) {
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

    /// Build the native `LeftJoin` hash-index key for one slot row.
    ///
    /// Provenance choice — **option (b): key on the DECODED lexical form of
    /// each join variable.** A left BGP row keys `?x` as `Slot::Id(5)` while a
    /// right row may key the same logical `?x` as `Slot::Term(...)`;
    /// `Slot::key_part()` would map those to `KeyPart::Id(5)` vs
    /// `KeyPart::Lex(...)` — *different* hash buckets — and a valid match would
    /// be missed. Decoding the jvar columns on both sides makes the bucket key
    /// provenance-independent, so equal values always collide in the same
    /// bucket. Only the (few) jvar columns decode; every non-jvar column stays
    /// native `Slot::Id` and is normalized only if the merge genuinely mixes Id
    /// and Term.
    ///
    /// Returns `None` if any jvar is `Unbound` in this row (such a row can't be
    /// keyed and takes the conservative `unkeyed` path).
    fn row_join_key(
        &self,
        row: &Row,
        schema: &[Var],
        jvars: &[Var],
    ) -> Result<Option<Vec<String>>> {
        let mut key = Vec::with_capacity(jvars.len());
        for jv in jvars {
            // jvars ⊆ schema by construction (batch_join_vars), so this is
            // always Some; treat a missing column conservatively as unkeyed.
            let Some(i) = schema.iter().position(|v| v.name() == jv.name()) else {
                return Ok(None);
            };
            match &row.0[i] {
                Slot::Unbound => return Ok(None),
                Slot::Id(id) => key.push(lex(&self.exec.decode_term(*id)?)),
                Slot::Term(t) => key.push(lex(t)),
            }
        }
        Ok(Some(key))
    }

    /// Merge the left row `a` against each candidate right row, apply the
    /// OPTIONAL's inner `FILTER` (`expr`) on the
    /// merged row by decoding just its referenced columns (`want`), push every
    /// kept merged row into `out`, and report whether any candidate matched.
    #[allow(clippy::too_many_arguments)]
    fn probe_into_slots(
        &self,
        ls: &[Var],
        a: &Row,
        rs: &[Var],
        candidates: &[&Row],
        out_schema: &[Var],
        expr: Option<&Expr>,
        want: &HashSet<String>,
        out: &mut Vec<Row>,
    ) -> Result<bool> {
        let mut matched = false;
        for b in candidates {
            if let Some(m) = self.merge_rows(ls, a, rs, b, out_schema)? {
                let keep = match expr {
                    Some(e) => {
                        let env = self.decode_subset(&m, out_schema, want)?;
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
}

/// The join-variable set for the native `LeftJoin`: the variables present in
/// *both* batch schemas, in deterministic (sorted, via `BTreeSet`) order — a
/// column listed in a batch schema is the slot-world analogue of "a variable
/// bound somewhere in the relation".
fn batch_join_vars(l: &Batch, r: &Batch) -> Vec<Var> {
    use std::collections::BTreeSet;
    let lvars: BTreeSet<&str> = l.schema.iter().map(|v| v.name()).collect();
    let rvars: BTreeSet<&str> = r.schema.iter().map(|v| v.name()).collect();
    lvars.intersection(&rvars).map(|s| Var::new(*s)).collect()
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
fn referenced_vars(e: &Expr, out: &mut HashSet<String>) {
    match e {
        Expr::Term(Term::Var(v)) => {
            out.insert(v.name().to_owned());
        }
        Expr::Term(_) => {}
        Expr::Bound(v) => {
            out.insert(v.name().to_owned());
        }
        Expr::Eq(a, b)
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

/// The inner expression(s) an aggregate evaluates over its members.
fn agg_inner_exprs(agg: &Aggregate) -> Vec<&Expr> {
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
fn integer_literal(n: i64) -> Term {
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

/// Best-effort numeric coercion of a term for SUM/AVG/MIN/MAX.
fn numeric_value(t: &Term) -> Option<f64> {
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

/// Wrap a lexical value as a plain (unquoted-form) literal term,
/// re-applying N-Triples string escapes so the stored form round-trips
/// through `literal_lexical`.
fn plain_literal(s: &str) -> Term {
    let escaped = s
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
        .replace('\u{0008}', "\\b")
        .replace('\u{000C}', "\\f");
    Term::Literal(format!("\"{escaped}\""))
}

/// Binary arithmetic over the Stage-1 f64 model. `None` (expression
/// error) when either side is non-numeric or on division by zero.
/// Overflow/NaN edge cases (e.g. inf - inf) are accepted Stage-1 f64-model
/// behavior and can render as "NaN"/"inf" literals.
fn arith(op: fn(f64, f64) -> f64, a: Option<f64>, b: Option<f64>) -> Option<Term> {
    Some(numeric_term(op(a?, b?)))
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
                members.iter().collect::<HashSet<&Bindings>>().len()
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
            let sum: f64 = vals.iter().filter_map(numeric_value).sum();
            Some(numeric_term(sum))
        }
        AggFunc::Avg(e) => {
            let vals = collect_values(e)?;
            let nums: Vec<f64> = vals.iter().filter_map(numeric_value).collect();
            if nums.is_empty() {
                Some(integer_literal(0))
            } else {
                Some(decimal_literal(
                    nums.iter().sum::<f64>() / nums.len() as f64,
                ))
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

/// Render a numeric aggregate result as an integer literal when it has
/// no fractional part, otherwise a decimal literal.
fn numeric_term(x: f64) -> Term {
    if x.fract() == 0.0 && x.abs() < 9.007e15 {
        integer_literal(x as i64)
    } else {
        decimal_literal(x)
    }
}

/// Deduplicate a term multiset by value, preserving first-seen order.
/// O(n) via a hash set (was an O(n^2) linear scan — the SPB aggregation
/// gap, #128).
fn dedup_terms(vals: &mut Vec<Term>) {
    let mut seen: HashSet<Term> = HashSet::with_capacity(vals.len());
    vals.retain(|t| seen.insert(t.clone()));
}

fn compare_by_keys(a: &Bindings, b: &Bindings, keys: &[(Expr, OrderDir)]) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    for (e, dir) in keys {
        let ta = eval_expr_to_term(e, a).ok().flatten();
        let tb = eval_expr_to_term(e, b).ok().flatten();
        let ord = match (ta, tb) {
            (Some(x), Some(y)) => compare_terms(&x, &y),
            (None, Some(_)) => Ordering::Less,
            (Some(_), None) => Ordering::Greater,
            (None, None) => Ordering::Equal,
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
    let (lx, ly) = (literal_value(x), literal_value(y));
    if let (Some(a), Some(b)) = (datetime_key(&lx), datetime_key(&ly)) {
        return a.cmp(&b);
    }
    lx.cmp(&ly)
}

/// Normalise an xsd:dateTime lexical form into a sortable key. Returns
/// `None` if the string does not look like an ISO-8601 instant. We do
/// not parse offsets fully; the lexical form sorts correctly for the
/// common `YYYY-MM-DDThh:mm:ss(.fff)?(Z)?` shape used by SPB, so we just
/// validate the prefix and key on the original string.
fn datetime_key(s: &str) -> Option<String> {
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
        Some(s.to_owned())
    } else {
        None
    }
}

fn eval_expr_to_term(e: &Expr, b: &Bindings) -> Result<Option<Term>> {
    // Evaluate an operand to its numeric value; an expression error
    // (non-numeric / unbound) surfaces as `Ok(None)`.
    let numof = |sub: &Expr| -> Result<Option<f64>> {
        Ok(eval_expr_to_term(sub, b)?.as_ref().and_then(numeric_value))
    };
    Ok(match e {
        Expr::Term(t) => match t {
            Term::Var(v) => b.get(v.name()).cloned(),
            other => Some(other.clone()),
        },
        // Boolean-typed expressions return a typed literal (lexical
        // form "true"/"false"); good enough for Stage 1 BIND tests.
        Expr::Eq(_, _)
        | Expr::Ne(_, _)
        | Expr::Lt(_, _)
        | Expr::Gt(_, _)
        | Expr::Le(_, _)
        | Expr::Ge(_, _)
        | Expr::In(_, _)
        | Expr::And(_, _)
        | Expr::Or(_, _)
        | Expr::Not(_)
        | Expr::Bound(_) => Some(Term::Literal(
            if eval_expr(e, b)? { "true" } else { "false" }.into(),
        )),
        Expr::Add(x, y) => arith(|a, b| a + b, numof(x)?, numof(y)?),
        Expr::Sub(x, y) => arith(|a, b| a - b, numof(x)?, numof(y)?),
        Expr::Mul(x, y) => arith(|a, b| a * b, numof(x)?, numof(y)?),
        Expr::Div(x, y) => match numof(y)? {
            Some(d) if d != 0.0 => arith(|a, b| a / b, numof(x)?, Some(d)),
            _ => None, // division by zero / non-numeric divisor
        },
        Expr::Neg(x) => numof(x)?.map(|n| numeric_term(-n)),
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
            let (text, start) = match (s(0)?, num(1)?) {
                (Some(t), Some(s)) => (t, s),
                _ => return Ok(None),
            };
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
            Some(plain_literal(&taken))
        }
        Func::UCase => s(0)?.map(|v| plain_literal(&v.to_uppercase())),
        Func::LCase => s(0)?.map(|v| plain_literal(&v.to_lowercase())),
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
        Func::StrBefore => match (s(0)?, s(1)?) {
            (Some(a), Some(b)) => Some(plain_literal(
                a.find(&b).map(|i| &a[..i]).unwrap_or_default(),
            )),
            _ => None,
        },
        Func::StrAfter => match (s(0)?, s(1)?) {
            (Some(a), Some(b)) => Some(plain_literal(
                a.find(&b).map(|i| &a[i + b.len()..]).unwrap_or_default(),
            )),
            _ => None,
        },
        Func::Concat => {
            let mut out = String::new();
            for (i, _) in args.iter().enumerate() {
                match s(i)? {
                    Some(v) => out.push_str(&v),
                    None => return Ok(None),
                }
            }
            Some(plain_literal(&out))
        }
        Func::Replace => {
            let (text, pat, repl) = match (s(0)?, s(1)?, s(2)?) {
                (Some(t), Some(p), Some(r)) => (t, p, r),
                _ => return Ok(None),
            };
            let flags = if args.len() == 4 {
                match s(3)? {
                    Some(f) => f,
                    None => return Ok(None),
                }
            } else {
                String::new()
            };
            compile_regex(&pat, &flags)
                .map(|re| plain_literal(&re.replace_all(&text, repl.as_str())))
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
        Func::Abs => num(0)?.map(|n| numeric_term(n.abs())),
        Func::Ceil => num(0)?.map(|n| numeric_term(n.ceil())),
        Func::Floor => num(0)?.map(|n| numeric_term(n.floor())),
        // fn:round rounds half toward positive infinity (ROUND(-2.5) =
        // -2), unlike Rust's round-half-away-from-zero.
        Func::Round => num(0)?.map(|n| numeric_term((n + 0.5).floor())),
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
pub fn describe_triples<E: Executor + ?Sized>(
    exec: &E,
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
        for b in exec.scan_bgp(std::slice::from_ref(&pattern))? {
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

    /// Build a `PhysicalPlan` from a SELECT query string, mirroring
    /// what `api::execute_query_with` does for the SELECT arm.
    fn plan_select(q: &str) -> PhysicalPlan {
        let parsed = parse_query(q).expect("query parse failed");
        let inner = match parsed {
            crate::parser::ParsedQuery::Select { inner } => inner,
            other => panic!("expected SELECT, got {:?}", other),
        };
        let alg =
            translate_query_with(&inner, &SparqlConfig::default()).expect("translation failed");
        planner::plan(&alg).expect("planning failed")
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
        let batch = rt.eval(&plan).unwrap();

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
        let batch = rt.eval(&plan).unwrap();

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
    /// semantics (None < Some) are baked into `compare_by_keys`; this guards
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
        let batch = rt.eval(&plan).unwrap();
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
}
