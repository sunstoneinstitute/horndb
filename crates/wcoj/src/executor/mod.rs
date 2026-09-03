//! Query executors. `WcojExecutor` and `BinaryHashExecutor` both produce
//! a stream of Arrow `RecordBatch`es. `Executor` is the planner-driven
//! dispatch enum.

pub mod binary_hash;
pub mod wcoj;

use arrow::record_batch::RecordBatch;

use crate::cancel::CancelToken;
use crate::error::Result;
use crate::pattern::Bgp;
use crate::plan::{ExecutionPlan, JoinSpec, PlanKind};
use crate::planner::Planner;
use crate::source::TripleSource;
use crate::stats::Stats;

/// Common output type — a fallible iterator over batches.
pub type BatchStream<'a> = Box<dyn Iterator<Item = Result<RecordBatch>> + 'a>;

/// Dispatch enum: a whole-BGP WCOJ plan streams straight from the leapfrog
/// executor; any other [`JoinSpec`] runs on the hash-join tree evaluator.
pub enum Executor<'src, S: TripleSource + ?Sized + 'src> {
    // Boxed: the WCOJ `BatchIter` carries the per-depth leapfrog state stack
    // and its SIMD intersect buffers, so it is much larger than the BinaryHash
    // variant. Boxing keeps the enum compact (`large_enum_variant`) at the
    // cost of one indirection per *batch* — not per tuple.
    Wcoj(Box<wcoj::BatchIter<'src, S>>),
    BinaryHash(Box<binary_hash::BatchIter<'src, S>>),
}

impl<'src, S: TripleSource + ?Sized + 'src> Executor<'src, S> {
    pub fn for_bgp(
        source: &'src S,
        bgp: &Bgp,
        planner: &Planner,
        stats: &dyn Stats,
        cancel: CancelToken,
    ) -> Self {
        let spec = planner.choose(bgp, stats);
        Self::for_spec(source, bgp, &spec, cancel)
    }

    pub fn for_spec(source: &'src S, bgp: &Bgp, spec: &JoinSpec, cancel: CancelToken) -> Self {
        if let Some(var_order) = spec.as_whole_wcoj(bgp) {
            let plan = ExecutionPlan {
                kind: PlanKind::Wcoj,
                var_order: var_order.to_vec(),
            };
            let exec = wcoj::WcojExecutor::new(source, bgp, &plan, cancel);
            return Executor::Wcoj(Box::new(exec.into_iter()));
        }
        let exec = binary_hash::BinaryHashExecutor::for_spec(
            source,
            bgp,
            spec.clone(),
            bgp.variables(),
            cancel,
        );
        Executor::BinaryHash(Box::new(exec.into_iter()))
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
