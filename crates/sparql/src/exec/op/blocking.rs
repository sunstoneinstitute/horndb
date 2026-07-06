//! Operators that consume at least one child eagerly (union, group, sort,
//! path) plus the hybrid hash joins, which drain only their build side
//! (right) and stream their probe side (left) chunk-by-chunk (#128).
//!
//! UNION normalize-scope note: `normalize_columns` must run over the
//! fully-combined (left + right) row set, not per-child. A column that is
//! all-Id in the left child and all-Term in the right child looks homogeneous
//! per-child — per-chunk normalize would leave it mixed in the union output,
//! causing DISTINCT/GROUP BY to treat Id(x) and Term(x) as different keys for
//! the same logical value. `UnionOp` therefore drains both children eagerly,
//! normalizes the combined rows, and then hands them out in `batch_rows()`-sized
//! chunks via `ChunkedBatch`.

use super::{ChunkedBatch, Op};
use crate::algebra::{Aggregate, Expr, OrderDir, Term, Var};
use crate::error::Result;
use crate::exec::runtime::{referenced_vars, JoinState, Runtime};
use crate::exec::{Batch, Executor, Row};
use std::collections::HashSet;

/// Pull an op to exhaustion, concatenating its chunks into one `Batch`.
pub(super) fn drain<'r>(op: &mut Box<dyn Op + 'r>) -> Result<Batch> {
    let schema = op.schema().to_vec();
    let mut rows: Vec<Row> = Vec::new();
    while let Some(b) = op.next()? {
        rows.extend(b.rows);
    }
    Ok(Batch { schema, rows })
}

/// Static `may_emit_term` for a two-child merge (`Union`, `Join`, `LeftJoin`):
/// an output column may yield `Slot::Term` iff either contributing child
/// claims it may. A var absent from a side contributes only `Slot::Unbound`
/// there. (For Union this also covers `normalize_columns`: it only decodes
/// Id→Term when a Term is actually present, i.e. when a child claimed one.)
pub(super) fn merged_term_columns(out_schema: &[Var], left: &dyn Op, right: &dyn Op) -> Vec<bool> {
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

/// UNION: drains both children eagerly to guarantee that `normalize_columns`
/// sees the full combined row set (required for correctness when the two
/// children have differing slot provenance — see module doc). Rows are
/// remapped into the merged schema per-child using `apply_union_chunk`, then
/// the combined set is normalized once, and finally handed out in
/// `batch_rows()`-sized chunks via `ChunkedBatch`.
pub struct UnionOp<'r, E: Executor + ?Sized> {
    rt: &'r Runtime<'r, E>,
    left: Box<dyn Op + 'r>,
    right: Box<dyn Op + 'r>,
    schema: Vec<Var>,
    /// Populated on the first `next()` call; `None` before that point.
    buffer: Option<ChunkedBatch>,
}

impl<'r, E: Executor + ?Sized> UnionOp<'r, E> {
    pub fn new(rt: &'r Runtime<'r, E>, left: Box<dyn Op + 'r>, right: Box<dyn Op + 'r>) -> Self {
        let schema = rt.union_schema(left.schema(), right.schema());
        Self {
            rt,
            left,
            right,
            schema,
            buffer: None,
        }
    }
}

impl<'r, E: Executor + ?Sized> Op for UnionOp<'r, E> {
    fn schema(&self) -> &[Var] {
        &self.schema
    }

    fn may_emit_term(&self) -> Vec<bool> {
        merged_term_columns(&self.schema, self.left.as_ref(), self.right.as_ref())
    }

    fn next(&mut self) -> Result<Option<Batch>> {
        // Once the buffer is populated, emit from it.
        if let Some(ref mut buf) = self.buffer {
            return Ok(buf.next_chunk());
        }

        // First call: drain both children and build the combined row set.
        let mut rows: Vec<Row> = Vec::new();
        while let Some(chunk) = self.left.next()? {
            rows.extend(self.rt.apply_union_chunk(chunk, &self.schema)?);
        }
        while let Some(chunk) = self.right.next()? {
            rows.extend(self.rt.apply_union_chunk(chunk, &self.schema)?);
        }

        // Normalize over the combined row set (see module-level doc for why
        // per-child normalization is insufficient).
        self.rt.normalize_columns(&mut rows, self.schema.len())?;

        let batch = Batch {
            schema: self.schema.clone(),
            rows,
        };
        self.buffer = Some(ChunkedBatch::new(batch));
        Ok(self.buffer.as_mut().unwrap().next_chunk())
    }
}

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
    /// Vars referenced by `expr` (constant for the operator's lifetime;
    /// computed once here so the probe path doesn't rebuild it per chunk).
    want: HashSet<String>,
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
        let mut want = HashSet::new();
        if let Some(e) = &expr {
            referenced_vars(e, &mut want);
        }
        Self {
            rt,
            left,
            right,
            expr,
            want,
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
                    let rows = self.rt.probe_left_join_chunk(
                        st,
                        &chunk,
                        self.expr.as_ref(),
                        &self.want,
                    )?;
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

/// GROUP BY + aggregates. Drains the child eagerly, calls
/// `eval_group_native` on the full batch, then emits via `ChunkedBatch`.
pub struct GroupOp<'r, E: Executor + ?Sized> {
    rt: &'r Runtime<'r, E>,
    child: Box<dyn Op + 'r>,
    keys: Vec<Var>,
    aggregates: Vec<Aggregate>,
    buffer: Option<ChunkedBatch>,
    schema: Vec<Var>,
}

impl<'r, E: Executor + ?Sized> GroupOp<'r, E> {
    pub fn new(
        rt: &'r Runtime<'r, E>,
        child: Box<dyn Op + 'r>,
        keys: Vec<Var>,
        aggregates: Vec<Aggregate>,
    ) -> Self {
        let schema = rt.group_output_schema(&keys, &aggregates);
        Self {
            rt,
            child,
            keys,
            aggregates,
            buffer: None,
            schema,
        }
    }
}

impl<'r, E: Executor + ?Sized> Op for GroupOp<'r, E> {
    fn schema(&self) -> &[Var] {
        &self.schema
    }
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
    fn next(&mut self) -> Result<Option<Batch>> {
        if self.buffer.is_none() {
            let b = drain(&mut self.child)?;
            self.buffer = Some(ChunkedBatch::new(self.rt.eval_group_native(
                b,
                &self.keys,
                &self.aggregates,
            )?));
        }
        Ok(self.buffer.as_mut().unwrap().next_chunk())
    }
}

/// ORDER BY. Drains the child eagerly, sorts via `compute_order_by`, then
/// emits via `ChunkedBatch`. Output schema = child schema (sort is
/// schema-preserving).
pub struct OrderByOp<'r, E: Executor + ?Sized> {
    rt: &'r Runtime<'r, E>,
    child: Box<dyn Op + 'r>,
    keys: Vec<(Expr, OrderDir)>,
    buffer: Option<ChunkedBatch>,
    schema: Vec<Var>,
}

impl<'r, E: Executor + ?Sized> OrderByOp<'r, E> {
    pub fn new(
        rt: &'r Runtime<'r, E>,
        child: Box<dyn Op + 'r>,
        keys: Vec<(Expr, OrderDir)>,
    ) -> Self {
        let schema = child.schema().to_vec();
        Self {
            rt,
            child,
            keys,
            buffer: None,
            schema,
        }
    }
}

impl<'r, E: Executor + ?Sized> Op for OrderByOp<'r, E> {
    fn schema(&self) -> &[Var] {
        &self.schema
    }
    fn may_emit_term(&self) -> Vec<bool> {
        // Sort only reorders rows; slots pass through untouched.
        self.child.may_emit_term()
    }
    fn next(&mut self) -> Result<Option<Batch>> {
        if self.buffer.is_none() {
            let b = drain(&mut self.child)?;
            self.buffer = Some(ChunkedBatch::new(self.rt.compute_order_by(b, &self.keys)?));
        }
        Ok(self.buffer.as_mut().unwrap().next_chunk())
    }
}

/// Compute the static output schema of a `PathClosure` operator: the
/// BTreeSet-sorted list of variable names from `subject` and `object` that
/// are `Term::Var`. This matches `Batch::from_bindings`'s schema derivation
/// exactly (it also collects names into a BTreeSet and sorts them).
fn path_closure_schema(subject: &Term, object: &Term) -> Vec<Var> {
    use std::collections::BTreeSet;
    let mut names: BTreeSet<String> = BTreeSet::new();
    if let Term::Var(v) = subject {
        names.insert(v.name().to_owned());
    }
    if let Term::Var(v) = object {
        names.insert(v.name().to_owned());
    }
    names.iter().map(|n| Var::new(n.as_str())).collect()
}

/// Kleene path closure (`p+` / `p*`). Drains the edge child eagerly,
/// delegates to `compute_path_closure`, then emits via `ChunkedBatch`.
pub struct PathClosureOp<'r, E: Executor + ?Sized> {
    rt: &'r Runtime<'r, E>,
    subject: Term,
    object: Term,
    edge: Box<dyn Op + 'r>,
    reflexive: bool,
    buffer: Option<ChunkedBatch>,
    schema: Vec<Var>,
}

impl<'r, E: Executor + ?Sized> PathClosureOp<'r, E> {
    pub fn new(
        rt: &'r Runtime<'r, E>,
        subject: Term,
        object: Term,
        edge: Box<dyn Op + 'r>,
        reflexive: bool,
    ) -> Self {
        let schema = path_closure_schema(&subject, &object);
        Self {
            rt,
            subject,
            object,
            edge,
            reflexive,
            buffer: None,
            schema,
        }
    }
}

impl<'r, E: Executor + ?Sized> Op for PathClosureOp<'r, E> {
    fn schema(&self) -> &[Var] {
        &self.schema
    }
    fn may_emit_term(&self) -> Vec<bool> {
        // Closure endpoints are rebuilt via Batch::from_bindings: all Term.
        vec![true; self.schema.len()]
    }
    fn next(&mut self) -> Result<Option<Batch>> {
        if self.buffer.is_none() {
            let edge_batch = drain(&mut self.edge)?;
            self.buffer = Some(ChunkedBatch::new(self.rt.compute_path_closure(
                edge_batch,
                &self.subject,
                &self.object,
                self.reflexive,
            )?));
        }
        Ok(self.buffer.as_mut().unwrap().next_chunk())
    }
}

#[cfg(test)]
mod tests {
    use super::super::source::ValuesOp;
    use super::super::{Op, TEST_BATCH_ROWS};
    use super::{JoinOp, LeftJoinOp};
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
}
