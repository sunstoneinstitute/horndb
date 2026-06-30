//! Operators that consume children eagerly or sequentially (union, joins,
//! group, sort, path).
//!
//! UNION normalize-scope note: `normalize_columns` must run over the
//! fully-combined (left + right) row set, not per-child. A column that is
//! all-Id in the left child and all-Term in the right child looks homogeneous
//! per-child — per-chunk normalize would leave it mixed in the union output,
//! causing DISTINCT/GROUP BY to treat Id(x) and Term(x) as different keys for
//! the same logical value. `UnionOp` therefore drains both children eagerly,
//! normalizes the combined rows, and then hands them out in `BATCH_ROWS`
//! chunks via `ChunkedBatch`.

use super::{ChunkedBatch, Op};
use crate::algebra::{Aggregate, Expr, OrderDir, Term, Var};
use crate::error::Result;
use crate::exec::runtime::Runtime;
use crate::exec::{Batch, Executor, Row};

/// Pull an op to exhaustion, concatenating its chunks into one `Batch`.
pub(super) fn drain<'r>(op: &mut Box<dyn Op + 'r>) -> Result<Batch> {
    let schema = op.schema().to_vec();
    let mut rows: Vec<Row> = Vec::new();
    while let Some(b) = op.next()? {
        rows.extend(b.rows);
    }
    Ok(Batch { schema, rows })
}

/// UNION: drains both children eagerly to guarantee that `normalize_columns`
/// sees the full combined row set (required for correctness when the two
/// children have differing slot provenance — see module doc). Rows are
/// remapped into the merged schema per-child using `apply_union_chunk`, then
/// the combined set is normalized once, and finally handed out in `BATCH_ROWS`
/// chunks via `ChunkedBatch`.
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

/// Inner hash join. First cut: drains both children, runs the whole-batch
/// `compute_join`, then emits the result via `ChunkedBatch`. (Probe-side
/// streaming is a possible future optimization.)
pub struct JoinOp<'r, E: Executor + ?Sized> {
    rt: &'r Runtime<'r, E>,
    left: Box<dyn Op + 'r>,
    right: Box<dyn Op + 'r>,
    buffer: Option<ChunkedBatch>,
    schema: Vec<Var>,
}

impl<'r, E: Executor + ?Sized> JoinOp<'r, E> {
    pub fn new(rt: &'r Runtime<'r, E>, left: Box<dyn Op + 'r>, right: Box<dyn Op + 'r>) -> Self {
        let schema = rt.union_schema(left.schema(), right.schema());
        Self {
            rt,
            left,
            right,
            buffer: None,
            schema,
        }
    }
}

impl<'r, E: Executor + ?Sized> Op for JoinOp<'r, E> {
    fn schema(&self) -> &[Var] {
        &self.schema
    }
    fn next(&mut self) -> Result<Option<Batch>> {
        if self.buffer.is_none() {
            let l = drain(&mut self.left)?;
            let r = drain(&mut self.right)?;
            self.buffer = Some(ChunkedBatch::new(self.rt.compute_join(l, r)?));
        }
        Ok(self.buffer.as_mut().unwrap().next_chunk())
    }
}

/// Left-outer hash join (OPTIONAL). First cut: drains both children, runs the
/// whole-batch `compute_left_join`, then emits the result via `ChunkedBatch`.
pub struct LeftJoinOp<'r, E: Executor + ?Sized> {
    rt: &'r Runtime<'r, E>,
    left: Box<dyn Op + 'r>,
    right: Box<dyn Op + 'r>,
    expr: Option<Expr>,
    buffer: Option<ChunkedBatch>,
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
            buffer: None,
            schema,
        }
    }
}

impl<'r, E: Executor + ?Sized> Op for LeftJoinOp<'r, E> {
    fn schema(&self) -> &[Var] {
        &self.schema
    }
    fn next(&mut self) -> Result<Option<Batch>> {
        if self.buffer.is_none() {
            let l = drain(&mut self.left)?;
            let r = drain(&mut self.right)?;
            self.buffer = Some(ChunkedBatch::new(
                self.rt.compute_left_join(l, r, &self.expr)?,
            ));
        }
        Ok(self.buffer.as_mut().unwrap().next_chunk())
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
