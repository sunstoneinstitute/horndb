//! Query executors. `WcojExecutor` and `BinaryHashExecutor` both produce
//! a stream of Arrow `RecordBatch`es. `Executor` is the planner-driven
//! dispatch enum.

pub mod binary_hash;
pub mod wcoj;

use arrow::record_batch::RecordBatch;

use crate::cancel::CancelToken;
use crate::cardinality::UniformEstimator;
use crate::error::Result;
use crate::pattern::Bgp;
use crate::plan::PlanKind;
use crate::planner::Planner;
use crate::source::TripleSource;

/// Common output type — a fallible iterator over batches.
pub type BatchStream<'a> = Box<dyn Iterator<Item = Result<RecordBatch>> + 'a>;

/// Dispatch enum: the planner picks WCOJ or BinaryHash and this wrapper
/// hides the choice from callers.
pub enum Executor<'src, S: TripleSource + ?Sized + 'src> {
    // Boxed: the WCOJ `BatchIter` carries the per-depth leapfrog state stack
    // and its SIMD intersect buffers, so it is much larger than the BinaryHash
    // variant. Boxing keeps the enum compact (`large_enum_variant`) at the
    // cost of one indirection per *batch* — not per tuple.
    Wcoj(Box<wcoj::BatchIter<'src, S>>),
    BinaryHash(binary_hash::BatchIter<'src, S>),
}

impl<'src, S: TripleSource + ?Sized + 'src> Executor<'src, S> {
    pub fn for_bgp(source: &'src S, bgp: &Bgp, planner: &Planner, cancel: CancelToken) -> Self {
        let est = UniformEstimator::from_source(source);
        let plan = planner.choose(bgp, &est);
        match plan.kind {
            PlanKind::Wcoj => {
                let exec = wcoj::WcojExecutor::new(source, bgp, &plan, cancel);
                Executor::Wcoj(Box::new(exec.into_iter()))
            }
            PlanKind::BinaryHash => {
                let out_vars = if plan.var_order.is_empty() {
                    bgp.variables()
                } else {
                    plan.var_order.clone()
                };
                let exec = binary_hash::BinaryHashExecutor::new(source, bgp, out_vars, cancel);
                Executor::BinaryHash(exec.into_iter())
            }
        }
    }
}

impl<'src, S: TripleSource + ?Sized + 'src> Iterator for Executor<'src, S> {
    type Item = Result<RecordBatch>;
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Executor::Wcoj(it) => it.next(),
            Executor::BinaryHash(it) => it.next(),
        }
    }
}
