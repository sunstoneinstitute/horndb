//! Pull-based physical operators (#143). Each `Op` yields `Batch` chunks of
//! at most `batch_rows()` rows, all sharing `schema()`. `next` returns `None`
//! at end of stream and never yields a `Some(empty)` chunk mid-stream.

mod blocking;
use blocking::{GroupOp, JoinOp, LeftJoinOp, OrderByOp, PathClosureOp, UnionOp};
mod source;
use source::{ScanOp, ValuesOp};
mod stream;
use stream::{DistinctOp, ExtendOp, FilterOp, ProjectOp, SliceOp};

use crate::algebra::Var;
use crate::error::Result;
use crate::exec::{Batch, Executor, Row};
use crate::plan::PhysicalPlan;

/// Target rows per emitted chunk. Test builds can shrink this via
/// `TEST_BATCH_ROWS` to force multi-chunk operator behavior; release builds
/// use a fixed constant.
#[cfg(not(test))]
pub(crate) fn batch_rows() -> usize {
    4096
}

#[cfg(test)]
thread_local! {
    pub(crate) static TEST_BATCH_ROWS: std::cell::Cell<usize> = const { std::cell::Cell::new(4096) };
}
#[cfg(test)]
pub(crate) fn batch_rows() -> usize {
    TEST_BATCH_ROWS.with(|c| c.get())
}

/// A pull-based physical operator. The trait itself is lifetime-free; an
/// operator that borrows the runtime carries its own lifetime on the struct
/// (`impl<'r, …> Op for FooOp<'r, …>`) and `build` boxes it as `dyn Op + 'r`.
pub trait Op {
    fn schema(&self) -> &[Var];
    fn next(&mut self) -> Result<Option<Batch>>;
}

/// Hands out the rows of a fully-materialized `Batch` in `batch_rows()` chunks.
/// Shared by source ops (`ScanOp`, `ValuesOp`) and the blocking ops.
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
    /// Next `batch_rows()`-sized chunk, or `None` when exhausted (never `Some(empty)`).
    pub(crate) fn next_chunk(&mut self) -> Option<Batch> {
        let chunk: Vec<Row> = self.rows.by_ref().take(batch_rows()).collect();
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

#[cfg(test)]
mod chunk_tests;

impl<'a, E: Executor + ?Sized> crate::exec::runtime::Runtime<'a, E> {
    /// Build the pull-based operator tree for `plan`. Every `PhysicalPlan`
    /// variant has a native `Op` — there is no longer a fallback path.
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
            PhysicalPlan::Join { left, right } => {
                let l = self.build(left)?;
                let r = self.build(right)?;
                Ok(Box::new(JoinOp::new(self, l, r)))
            }
            PhysicalPlan::LeftJoin { left, right, expr } => {
                let l = self.build(left)?;
                let r = self.build(right)?;
                Ok(Box::new(LeftJoinOp::new(self, l, r, expr.clone())))
            }
            PhysicalPlan::Group {
                inner,
                keys,
                aggregates,
            } => {
                let child = self.build(inner)?;
                Ok(Box::new(GroupOp::new(
                    self,
                    child,
                    keys.clone(),
                    aggregates.clone(),
                )))
            }
            PhysicalPlan::OrderBy { inner, keys } => {
                let child = self.build(inner)?;
                Ok(Box::new(OrderByOp::new(self, child, keys.clone())))
            }
            PhysicalPlan::PathClosure {
                subject,
                object,
                edge,
                reflexive,
            } => {
                let edge_op = self.build(edge)?;
                Ok(Box::new(PathClosureOp::new(
                    self,
                    subject.clone(),
                    object.clone(),
                    edge_op,
                    *reflexive,
                )))
            }
        }
    }
}
