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
pub enum Executor<'src> {
    Wcoj(wcoj::BatchIter<'src>),
    BinaryHash(binary_hash::BatchIter<'src>),
}

impl<'src> Executor<'src> {
    pub fn for_bgp(
        source: &'src dyn TripleSource,
        bgp: &Bgp,
        planner: &Planner,
        cancel: CancelToken,
    ) -> Self {
        let est = UniformEstimator::from_source(source);
        let plan = planner.choose(bgp, &est);
        match plan.kind {
            PlanKind::Wcoj => {
                let exec = wcoj::WcojExecutor::new(source, bgp, &plan, cancel);
                Executor::Wcoj(exec.into_iter())
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

impl<'src> Iterator for Executor<'src> {
    type Item = Result<RecordBatch>;
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Executor::Wcoj(it) => it.next(),
            Executor::BinaryHash(it) => it.next(),
        }
    }
}
