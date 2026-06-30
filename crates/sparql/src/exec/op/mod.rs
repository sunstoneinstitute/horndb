//! Pull-based physical operators (#143). Each `Op` yields `Batch` chunks of
//! at most `BATCH_ROWS` rows, all sharing `schema()`. `next` returns `None`
//! at end of stream and never yields a `Some(empty)` chunk mid-stream.

mod blocking;
use blocking::UnionOp;
mod source;
pub(crate) use source::build_values_batch;
use source::{ScanOp, ValuesOp};
mod stream;
use stream::{DistinctOp, ExtendOp, FilterOp, ProjectOp, SliceOp};

use crate::algebra::Var;
use crate::error::Result;
use crate::exec::{Batch, Executor, Row};
use crate::plan::PhysicalPlan;

/// Target rows per emitted chunk.
pub const BATCH_ROWS: usize = 4096;

/// A pull-based physical operator. The trait itself is lifetime-free; an
/// operator that borrows the runtime carries its own lifetime on the struct
/// (`impl<'r, …> Op for FooOp<'r, …>`) and `build` boxes it as `dyn Op + 'r`.
pub trait Op {
    fn schema(&self) -> &[Var];
    fn next(&mut self) -> Result<Option<Batch>>;
}

/// Hands out the rows of a fully-materialized `Batch` in `BATCH_ROWS` chunks.
/// The shared chunker for source ops (`ScanOp`, `ValuesOp`) and the
/// `MaterializedOp` adapter; later reused as the emit tail of blocking ops.
pub(crate) struct ChunkedBatch {
    schema: Vec<Var>,
    rows: std::vec::IntoIter<Row>,
}

impl ChunkedBatch {
    pub(crate) fn new(batch: Batch) -> Self {
        Self {
            schema: batch.schema,
            rows: batch.rows.into_iter(),
        }
    }
    /// Next `BATCH_ROWS`-sized chunk, or `None` when exhausted (never `Some(empty)`).
    pub(crate) fn next_chunk(&mut self) -> Option<Batch> {
        let chunk: Vec<Row> = self.rows.by_ref().take(BATCH_ROWS).collect();
        if chunk.is_empty() {
            None
        } else {
            Some(Batch {
                schema: self.schema.clone(),
                rows: chunk,
            })
        }
    }
    pub(crate) fn schema(&self) -> &[Var] {
        &self.schema
    }
}

/// Adapter wrapping a not-yet-converted subtree: evaluates it once via the
/// legacy `Runtime::eval`, then hands rows out in `BATCH_ROWS` chunks.
/// Deleted in a later task once every variant has a native `Op`.
pub struct MaterializedOp {
    inner: ChunkedBatch,
}

impl MaterializedOp {
    pub fn new(batch: Batch) -> Self {
        Self {
            inner: ChunkedBatch::new(batch),
        }
    }
}

impl Op for MaterializedOp {
    fn schema(&self) -> &[Var] {
        self.inner.schema()
    }
    fn next(&mut self) -> Result<Option<Batch>> {
        Ok(self.inner.next_chunk())
    }
}

impl<'a, E: Executor + ?Sized> crate::exec::runtime::Runtime<'a, E> {
    /// Build the pull-based operator tree for `plan`. During conversion,
    /// unconverted variants fall through to a `MaterializedOp` wrapping the
    /// legacy `eval` of that subtree.
    pub(crate) fn build<'r>(&'r self, plan: &PhysicalPlan) -> Result<Box<dyn Op + 'r>>
    where
        E: 'r,
    {
        match plan {
            PhysicalPlan::BgpScan { patterns } => {
                Ok(Box::new(ScanOp::new(self.exec().scan_bgp_ids(patterns)?)))
            }
            PhysicalPlan::Filter { expr, inner } => {
                let child = self.build(inner)?;
                Ok(Box::new(FilterOp::new(self, child, expr.clone())))
            }
            PhysicalPlan::Project { vars, inner } => {
                let child = self.build(inner)?;
                Ok(Box::new(ProjectOp::new(self, child, vars.clone())))
            }
            PhysicalPlan::Extend { inner, var, expr } => {
                let child = self.build(inner)?;
                Ok(Box::new(ExtendOp::new(
                    self,
                    child,
                    var.clone(),
                    expr.clone(),
                )))
            }
            PhysicalPlan::Slice {
                inner,
                start,
                length,
            } => {
                let child = self.build(inner)?;
                Ok(Box::new(SliceOp::new(child, *start, *length)))
            }
            PhysicalPlan::Values { vars, rows } => Ok(Box::new(ValuesOp::new(vars, rows))),
            PhysicalPlan::Distinct { inner } => {
                let child = self.build(inner)?;
                Ok(Box::new(DistinctOp::new(child)))
            }
            PhysicalPlan::Union { left, right } => {
                let l = self.build(left)?;
                let r = self.build(right)?;
                Ok(Box::new(UnionOp::new(self, l, r)))
            }
            _ => Ok(Box::new(MaterializedOp::new(self.eval(plan)?))),
        }
    }
}
