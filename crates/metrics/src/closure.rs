//! Closure backend metrics, fed per closure call (not per iteration).
use prometheus_client::metrics::histogram::{exponential_buckets, Histogram};
use prometheus_client::registry::Registry;

#[derive(Clone)]
pub struct ClosureSink {
    pub mxm_seconds: Histogram,
    pub total_seconds: Histogram,
    pub iterations_to_fixpoint: Histogram,
    pub output_nnz: Histogram,
}

impl ClosureSink {
    pub fn register(reg: &mut Registry) -> Self {
        let mxm_seconds = Histogram::new(exponential_buckets(1e-4, 3.0, 12));
        let total_seconds = Histogram::new(exponential_buckets(1e-4, 3.0, 12));
        let iterations_to_fixpoint = Histogram::new(exponential_buckets(1.0, 2.0, 10));
        let output_nnz = Histogram::new(exponential_buckets(10.0, 10.0, 9));
        reg.register(
            "closure_mxm_seconds",
            "Time in GrB_mxm per closure call",
            mxm_seconds.clone(),
        );
        reg.register(
            "closure_total_seconds",
            "Total closure wall time per call",
            total_seconds.clone(),
        );
        reg.register(
            "closure_iterations_to_fixpoint",
            "Iterations to closure fixpoint",
            iterations_to_fixpoint.clone(),
        );
        reg.register(
            "closure_output_nnz",
            "Closure output non-zeros per call",
            output_nnz.clone(),
        );
        Self {
            mxm_seconds,
            total_seconds,
            iterations_to_fixpoint,
            output_nnz,
        }
    }

    pub fn observe(&self, mxm: f64, total: f64, iterations: u64, output_nnz: u64) {
        self.mxm_seconds.observe(mxm);
        self.total_seconds.observe(total);
        self.iterations_to_fixpoint.observe(iterations as f64);
        self.output_nnz.observe(output_nnz as f64);
    }
}
