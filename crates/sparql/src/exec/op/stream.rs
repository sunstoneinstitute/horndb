//! Streaming operators: one child, per-chunk transform, no buffering.

use super::Op;
use crate::algebra::{Expr, Var};
use crate::error::Result;
use crate::exec::runtime::Runtime;
use crate::exec::{Batch, Executor};

/// Streams its child, evaluating `expr` and binding the result to `var` (BIND).
/// Never drops rows — an unbound expr result leaves the slot as `Slot::Unbound`.
pub struct ExtendOp<'r, E: Executor + ?Sized> {
    rt: &'r Runtime<'r, E>,
    child: Box<dyn Op + 'r>,
    var: Var,
    expr: Expr,
    schema: Vec<Var>,
}

impl<'r, E: Executor + ?Sized> ExtendOp<'r, E> {
    pub fn new(rt: &'r Runtime<'r, E>, child: Box<dyn Op + 'r>, var: Var, expr: Expr) -> Self {
        // Mirror apply_extend's output schema rule exactly: append var iff
        // absent from child schema (re-BIND overwrites in place, no schema change).
        let mut schema = child.schema().to_vec();
        if !schema.iter().any(|v| v.name() == var.name()) {
            schema.push(var.clone());
        }
        Self {
            rt,
            child,
            var,
            expr,
            schema,
        }
    }
}

impl<'r, E: Executor + ?Sized> Op for ExtendOp<'r, E> {
    fn schema(&self) -> &[Var] {
        &self.schema
    }
    fn next(&mut self) -> Result<Option<Batch>> {
        match self.child.next()? {
            Some(chunk) => Ok(Some(self.rt.apply_extend(chunk, &self.var, &self.expr)?)),
            None => Ok(None),
        }
    }
}

/// Streams its child, projecting each chunk to `vars`.
pub struct ProjectOp<'r, E: Executor + ?Sized> {
    rt: &'r Runtime<'r, E>,
    child: Box<dyn Op + 'r>,
    vars: Vec<Var>,
    schema: Vec<Var>,
}

impl<'r, E: Executor + ?Sized> ProjectOp<'r, E> {
    pub fn new(rt: &'r Runtime<'r, E>, child: Box<dyn Op + 'r>, vars: Vec<Var>) -> Self {
        // Mirror `apply_project`'s output schema exactly: empty `vars` is a
        // passthrough; otherwise keep only projected vars present in the child
        // schema (absent vars are dropped from both schema and rows). This
        // keeps `schema()` consistent with every Batch `next` yields, honoring
        // the Op contract even for an ill-scoped Project.
        let schema = if vars.is_empty() {
            child.schema().to_vec()
        } else {
            let child_schema = child.schema();
            vars.iter()
                .filter(|v| child_schema.iter().any(|cv| cv.name() == v.name()))
                .cloned()
                .collect()
        };
        Self {
            rt,
            child,
            vars,
            schema,
        }
    }
}

impl<'r, E: Executor + ?Sized> Op for ProjectOp<'r, E> {
    fn schema(&self) -> &[Var] {
        &self.schema
    }
    fn next(&mut self) -> Result<Option<Batch>> {
        match self.child.next()? {
            Some(chunk) => Ok(Some(self.rt.apply_project(chunk, &self.vars)?)),
            None => Ok(None),
        }
    }
}

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
