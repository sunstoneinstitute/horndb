//! Pull-based physical operators (#143). Each `Op` yields `Batch` chunks of
//! at most `batch_rows()` rows, all sharing `schema()`. `next` returns `None`
//! at end of stream and never yields a `Some(empty)` chunk mid-stream.

mod blocking;
use blocking::{GroupOp, JoinOp, LeftJoinOp, OrderByOp, PathClosureOp, UnionOp};
mod source;
/// The one scan-side helper outside this module needs (`GRAPH ?g`'s
/// per-graph read); the operators themselves stay private.
pub(crate) use source::scan_scoped;
use source::{CountScanOp, GroupCountScanOp, ScanOp, ValuesOp};
mod stream;
use stream::{DistinctOp, ExtendOp, FilterOp, ProjectOp, SliceOp};

use crate::algebra::Var;
use crate::error::Result;
use crate::exec::phases;
use crate::exec::{Batch, Executor, Row};
use crate::plan::PhysicalPlan;
use horndb_metrics::labels::ExecPhase;

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
    ///
    /// Timed as `chunk_pull` (HDB-109): the `collect` and the per-chunk
    /// `schema.clone()` are real work on every materialized operator's
    /// output path, and HDB-99 left them outside every named phase. Clocked
    /// by hand rather than via `phases::timed` because the row count is only
    /// known after the `collect` — see the `enabled()` note in `phases`. One
    /// clock per chunk, never per row (SPEC-17 §5.3).
    pub(crate) fn next_chunk(&mut self) -> Option<Batch> {
        let t0 = phases::enabled().then(std::time::Instant::now);
        let chunk: Vec<Row> = self.rows.by_ref().take(batch_rows()).collect();
        let out = (!chunk.is_empty()).then(|| Batch {
            schema: self.schema.clone(),
            rows: chunk,
        });
        if let Some(t0) = t0 {
            let n = out.as_ref().map_or(0, |b| b.rows.len() as u64);
            phases::add(ExecPhase::ChunkPull, t0.elapsed().as_nanos() as u64, n);
        }
        out
    }
    pub(crate) fn schema(&self) -> &[Var] {
        &self.schema
    }
}

#[cfg(test)]
mod chunk_tests;
#[cfg(test)]
mod provenance_tests;
#[cfg(test)]
mod top_k_tests;

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
                // Top-k fusion (HDB-101): a bounded LIMIT directly above an
                // ORDER BY means only `offset + limit` rows can survive, so
                // the sort does not have to order the rest. `SliceOp` still
                // applies the offset and limit — `build_top_k` only narrows
                // what reaches it.
                let fused = match length.and_then(|len| start.checked_add(len)) {
                    Some(n) => self.build_top_k(inner, n)?,
                    None => None,
                };
                let child = match fused {
                    Some(op) => op,
                    None => self.build(inner)?,
                };
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
                // A path under `GRAPH ?g` would flatten the edge relation's
                // graph column and connect hops from different graphs.
                // `plan::lower` refuses that shape before it gets here — the
                // single refusal site (SPEC-28 S3).
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

    /// Build `plan` as a bounded (top-k) `OrderByOp` keeping `n` rows, or
    /// `None` when the plan shape does not permit the fusion.
    ///
    /// Only two shapes do. `OrderBy` is the fusion itself. `Project` is
    /// see-through because SPARQL algebra (§18.2.5) puts the sort *under* the
    /// projection — `Slice(Project(OrderBy(..)))` is the plan a plain
    /// `ORDER BY .. LIMIT ..` produces — and projection preserves both row
    /// order and row count, so an `n`-row bound above it is an `n`-row bound
    /// below it. Nothing else is see-through: `Distinct`, `Filter` and the
    /// rest drop rows *after* the sort, where `n` sorted rows are no longer
    /// enough to answer the limit.
    fn build_top_k<'r>(&'r self, plan: &PhysicalPlan, n: usize) -> Result<Option<Box<dyn Op + 'r>>>
    where
        E: 'r,
    {
        match plan {
            PhysicalPlan::OrderBy { inner, keys } => {
                let child = self.build(inner)?;
                Ok(Some(Box::new(OrderByOp::top_k(
                    self,
                    child,
                    keys.clone(),
                    n,
                ))))
            }
            PhysicalPlan::Project { vars, inner } => match self.build_top_k(inner, n)? {
                Some(child) => Ok(Some(Box::new(ProjectOp::new(self, child, vars.clone())))),
                None => Ok(None),
            },
            _ => Ok(None),
        }
    }
}
