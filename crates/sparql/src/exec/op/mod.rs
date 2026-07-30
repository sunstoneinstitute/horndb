//! Pull-based physical operators (#143). Each `Op` yields `Batch` chunks of
//! at most `batch_rows()` rows, all sharing `schema()`. `next` returns `None`
//! at end of stream and never yields a `Some(empty)` chunk mid-stream.

mod blocking;
use blocking::{GroupOp, JoinOp, LeftJoinOp, OrderByOp, PathClosureOp, UnionOp};
pub(crate) mod source;
use source::{scan_scoped, CountScanOp, GroupCountScanOp, ScanOp, ValuesOp};
mod stream;
use stream::{DistinctOp, ExtendOp, FilterOp, ProjectOp, SliceOp};

use crate::algebra::Var;
use crate::error::{Result, SparqlError};
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
///
/// Stream-wide column-provenance invariant: across ALL chunks an op ever
/// yields, a given column never mixes `Slot::Id` and `Slot::Term`
/// (`Slot::Unbound` may appear anywhere). Cross-chunk keyed consumers
/// (`DistinctOp`'s seen-set, `GroupOp`) rely on this — `KeyPart::Id(x)` and
/// `KeyPart::Lex(lex(x))` hash differently for the same logical term.
/// `may_emit_term` is the static contract that lets the streaming joins
/// uphold it without seeing their whole output first.
pub trait Op {
    fn schema(&self) -> &[Var];
    /// Static per-column provenance claim, parallel to `schema()`: `true` at
    /// index `i` means column `i` MAY yield a `Slot::Term` somewhere in this
    /// op's output stream. Over-approximation; `false` is a guarantee (the
    /// column only ever holds `Slot::Id`/`Slot::Unbound`). Required — a new
    /// operator that forgets to declare provenance must fail to compile, not
    /// silently break cross-chunk DISTINCT/GROUP BY keying.
    fn may_emit_term(&self) -> Vec<bool>;
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

/// The graph variable of the first `GRAPH ?g` scan leaf in `plan`, if it has
/// one. Used to spot the sub-plans whose per-row graph column an operator
/// cannot yet honour.
fn per_graph_leaf(plan: &PhysicalPlan) -> Option<&Var> {
    let own = match plan {
        PhysicalPlan::BgpScan { scope, .. }
        | PhysicalPlan::CountScan { scope, .. }
        | PhysicalPlan::GroupCountScan { scope, .. } => scope.graph_var(),
        _ => None,
    };
    own.or_else(|| {
        crate::plan::explain::children(plan)
            .into_iter()
            .find_map(per_graph_leaf)
    })
}

#[cfg(test)]
mod chunk_tests;
#[cfg(test)]
mod provenance_tests;

impl<'a, E: Executor + ?Sized> crate::exec::runtime::Runtime<'a, E> {
    /// Build the pull-based operator tree for `plan`. Every `PhysicalPlan`
    /// variant has a native `Op` — there is no longer a fallback path.
    pub(crate) fn build<'r>(&'r self, plan: &PhysicalPlan) -> Result<Box<dyn Op + 'r>>
    where
        E: 'r,
    {
        match plan {
            // `scan_scoped`, not `scan_bgp_ids`: `GRAPH ?g` is one scan node
            // whose operator loops over the graphs and appends the `?g`
            // column, so the plan never grows with the graph count (D6).
            PhysicalPlan::BgpScan { patterns, scope } => Ok(Box::new(ScanOp::new(scan_scoped(
                self.exec(),
                patterns,
                &self.scan_scope(scope),
            )?))),
            PhysicalPlan::CountScan {
                patterns,
                out_var,
                scope,
            } => Ok(Box::new(CountScanOp::new(
                self.exec(),
                patterns,
                out_var,
                &self.scan_scope(scope),
            )?)),
            PhysicalPlan::GroupCountScan {
                patterns,
                keys,
                out_vars,
                scope,
            } => Ok(Box::new(GroupCountScanOp::new(
                self.exec(),
                patterns,
                keys,
                out_vars,
                &self.scan_scope(scope),
            )?)),
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
                // SPEC-28 S3 wants the scope applied *before* the closure:
                // one closure per graph. Under `GRAPH ?g` the edge relation
                // instead arrives as every graph's edges in one batch with a
                // `?g` column the closure flattens away — which would join
                // a hop in one graph to a hop in another and drop the graph
                // binding. Refuse; PLAN-28-03 Task 5 scopes paths.
                if let Some(v) = per_graph_leaf(edge) {
                    return Err(SparqlError::UnsupportedAlgebra(format!(
                        "a property path inside GRAPH ?{} is not implemented yet \
                         (SPEC-28 S3, #266): the closure has to be computed per \
                         graph, and one merged closure would connect paths that \
                         leave the graph",
                        v.name()
                    )));
                }
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
