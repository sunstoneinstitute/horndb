//! SPARQL HTTP + pipeline metrics.
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::histogram::{exponential_buckets, Histogram};
use prometheus_client::registry::Registry;

use crate::labels::{EndpointLabel, QueryKindLabel, RequestLabels, StageLabel};

#[derive(Clone)]
pub struct SparqlMetrics {
    pub requests: Family<RequestLabels, Counter>,
    pub request_duration_seconds: Family<EndpointLabel, Histogram>,
    // Response-byte accounting is deferred to the fan-out (issue #148): it wants a
    // body-counting tower layer, not a middleware that can't see the serialized
    // size cheaply. Omitted here rather than shipped as a permanently-zero series.
    pub query_total: Family<QueryKindLabel, Counter>,
    pub query_errors: Family<StageLabel, Counter>,
    pub stage_duration_seconds: Family<StageLabel, Histogram>,
}

fn latency_hist() -> Histogram {
    Histogram::new(exponential_buckets(1e-4, 3.0, 12))
}

impl SparqlMetrics {
    pub fn register(reg: &mut Registry) -> Self {
        let requests = Family::<RequestLabels, Counter>::default();
        let request_duration_seconds =
            Family::<EndpointLabel, Histogram>::new_with_constructor(latency_hist);
        let query_total = Family::<QueryKindLabel, Counter>::default();
        let query_errors = Family::<StageLabel, Counter>::default();
        let stage_duration_seconds =
            Family::<StageLabel, Histogram>::new_with_constructor(latency_hist);

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

        Self {
            requests,
            request_duration_seconds,
            query_total,
            query_errors,
            stage_duration_seconds,
        }
    }
}
