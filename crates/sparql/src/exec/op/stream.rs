//! Streaming operators: one child, per-chunk transform, no buffering.

use super::Op;
use crate::algebra::{Expr, Var};
use crate::error::Result;
use crate::exec::runtime::Runtime;
use crate::exec::{Batch, Executor};

/// Streams its child, keeping rows that satisfy `expr`. Loops internally so it
/// never yields an empty chunk (a chunk fully filtered out pulls the next one).
pub struct FilterOp<'r, E: Executor + ?Sized> {
    rt: &'r Runtime<'r, E>,
    child: Box<dyn Op + 'r>,
    expr: Expr,
    schema: Vec<Var>,
}

impl<'r, E: Executor + ?Sized> FilterOp<'r, E> {
    pub fn new(rt: &'r Runtime<'r, E>, child: Box<dyn Op + 'r>, expr: Expr) -> Self {
        let schema = child.schema().to_vec();
        Self {
            rt,
            child,
            expr,
            schema,
        }
    }
}

impl<'r, E: Executor + ?Sized> Op for FilterOp<'r, E> {
    fn schema(&self) -> &[Var] {
        &self.schema
    }
    fn next(&mut self) -> Result<Option<Batch>> {
        while let Some(chunk) = self.child.next()? {
            let kept = self.rt.apply_filter(chunk, &self.expr)?;
            if !kept.rows.is_empty() {
                return Ok(Some(kept));
            }
        }
        Ok(None)
    }
}
