//! SPARQL HTTP + pipeline metrics.
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::metrics::histogram::{exponential_buckets, Histogram};
use prometheus_client::registry::Registry;
use std::time::Duration;

use crate::labels::{
    EndpointLabel, ExecPhase, ExecPhaseLabel, QueryKindLabel, RequestLabels, StageLabel,
};

#[derive(Clone)]
pub struct SparqlMetrics {
    pub requests: Family<RequestLabels, Counter>,
    pub request_duration_seconds: Family<EndpointLabel, Histogram>,
    pub request_bytes: Family<EndpointLabel, Counter>,
    pub response_bytes: Family<EndpointLabel, Counter>,
    pub query_total: Family<QueryKindLabel, Counter>,
    pub query_errors: Family<StageLabel, Counter>,
    pub stage_duration_seconds: Family<StageLabel, Histogram>,
    /// Nanoseconds spent in each per-operator execution phase, and rows each
    /// phase handled (HDB-99). A count+sum pair per SPEC-17 §5.4.1, emitted
    /// only when `HORNDB_EXEC_PHASES=1`. Written once per phase per query by
    /// [`SparqlMetrics::record_exec_phase`], never from inside a per-row loop
    /// — see `crates/sparql/src/exec/phases.rs`.
    pub exec_phase_nanoseconds: Family<ExecPhaseLabel, Counter>,
    pub exec_phase_rows: Family<ExecPhaseLabel, Counter>,
    /// HDB-118 admission control: queries currently holding an execution
    /// permit, and requests shed with 503 because no permit came free within
    /// `[server.limits].queue_timeout`. Unlabelled — only `/query` is
    /// admission-controlled, and a rejection has one cause.
    pub queries_in_flight: Gauge,
    pub queries_rejected: Counter,
}

fn latency_hist() -> Histogram {
    Histogram::new(exponential_buckets(1e-4, 3.0, 12))
}

impl SparqlMetrics {
    pub fn register(reg: &mut Registry) -> Self {
        let requests = Family::<RequestLabels, Counter>::default();
        let request_duration_seconds =
            Family::<EndpointLabel, Histogram>::new_with_constructor(latency_hist);
        let request_bytes = Family::<EndpointLabel, Counter>::default();
        let response_bytes = Family::<EndpointLabel, Counter>::default();
        let query_total = Family::<QueryKindLabel, Counter>::default();
        let query_errors = Family::<StageLabel, Counter>::default();
        let stage_duration_seconds =
            Family::<StageLabel, Histogram>::new_with_constructor(latency_hist);
        let exec_phase_nanoseconds = Family::<ExecPhaseLabel, Counter>::default();
        let exec_phase_rows = Family::<ExecPhaseLabel, Counter>::default();
        let queries_in_flight = Gauge::default();
        let queries_rejected = Counter::default();

        reg.register(
            "sparql_requests",
            "Total SPARQL HTTP requests",
            requests.clone(),
        );
        reg.register(
            "sparql_request_duration_seconds",
            "SPARQL request latency",
            request_duration_seconds.clone(),
        );
        reg.register(
            "sparql_request_bytes",
            "SPARQL request body bytes",
            request_bytes.clone(),
        );
        reg.register(
            "sparql_response_bytes",
            "SPARQL response body bytes",
            response_bytes.clone(),
        );
        reg.register(
            "sparql_query",
            "SPARQL operations by kind",
            query_total.clone(),
        );
        reg.register(
            "sparql_query_errors",
            "SPARQL pipeline errors by stage",
            query_errors.clone(),
        );
        reg.register(
            "sparql_stage_duration_seconds",
            "SPARQL pipeline stage latency",
            stage_duration_seconds.clone(),
        );
        reg.register(
            "sparql_exec_phase_nanoseconds",
            "Nanoseconds spent in each SPARQL execution-time operator phase",
            exec_phase_nanoseconds.clone(),
        );
        reg.register(
            "sparql_exec_phase_rows",
            "Rows handled by each SPARQL execution-time operator phase",
            exec_phase_rows.clone(),
        );
        reg.register(
            "sparql_queries_in_flight",
            "SPARQL queries currently holding an admission-control permit",
            queries_in_flight.clone(),
        );
        reg.register(
            "sparql_queries_rejected",
            "SPARQL queries shed with 503 after waiting past the admission queue timeout",
            queries_rejected.clone(),
        );

        Self {
            requests,
            request_duration_seconds,
            request_bytes,
            response_bytes,
            query_total,
            query_errors,
            stage_duration_seconds,
            exec_phase_nanoseconds,
            exec_phase_rows,
            queries_in_flight,
            queries_rejected,
        }
    }

    /// Record one execution-time phase: its elapsed time and the rows it
    /// handled. Call this **once per phase per query** (per HDB-99's
    /// thread-local flush), never per row (SPEC-17 §5.4). Mirrors
    /// [`crate::storage::StorageMetrics::record_load_phase`].
    pub fn record_exec_phase(&self, phase: ExecPhase, elapsed: Duration, rows: u64) {
        let label = ExecPhaseLabel { phase };
        self.exec_phase_nanoseconds
            .get_or_create(&label)
            .inc_by(elapsed.as_nanos() as u64);
        self.exec_phase_rows.get_or_create(&label).inc_by(rows);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::labels::{Endpoint, EndpointLabel};

    #[test]
    fn registers_byte_counters() {
        let mut reg = Registry::with_prefix("horndb");
        let m = SparqlMetrics::register(&mut reg);
        m.request_bytes
            .get_or_create(&EndpointLabel {
                endpoint: Endpoint::Query,
            })
            .inc_by(42);
        m.response_bytes
            .get_or_create(&EndpointLabel {
                endpoint: Endpoint::Query,
            })
            .inc_by(7);

        let mut buf = String::new();
        prometheus_client::encoding::text::encode(&mut buf, &reg).unwrap();
        assert!(
            buf.contains("horndb_sparql_request_bytes_total"),
            "got:\n{buf}"
        );
        assert!(
            buf.contains("horndb_sparql_response_bytes_total"),
            "got:\n{buf}"
        );
        assert!(buf.contains("endpoint=\"query\""), "got:\n{buf}");
    }
}
