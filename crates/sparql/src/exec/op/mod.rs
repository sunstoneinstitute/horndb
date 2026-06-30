//! Pull-based physical operators (#143). Each `Op` yields `Batch` chunks of
//! at most `BATCH_ROWS` rows, all sharing `schema()`. `next` returns `None`
//! at end of stream and never yields a `Some(empty)` chunk mid-stream.

mod source;
use source::ScanOp;

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

/// Adapter wrapping a not-yet-converted subtree: evaluates it once via the
/// legacy `Runtime::eval`, then hands rows out in `BATCH_ROWS` chunks.
/// Deleted in a later task once every variant has a native `Op`.
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

impl Op for MaterializedOp {
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
    pub(crate) fn build<'r>(&'r self, plan: &PhysicalPlan) -> Result<Box<dyn Op + 'r>>
    where
        E: 'r,
    {
        match plan {
            PhysicalPlan::BgpScan { patterns } => {
                Ok(Box::new(ScanOp::new(self.exec().scan_bgp_ids(patterns)?)))
            }
            _ => Ok(Box::new(MaterializedOp::new(self.eval(plan)?))),
        }
    }
}
