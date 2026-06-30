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
use crate::algebra::Var;
use crate::error::Result;
use crate::exec::runtime::Runtime;
use crate::exec::{Batch, Executor, Row};

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
