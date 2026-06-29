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
    pub request_bytes: Family<EndpointLabel, Counter>,
    pub response_bytes: Family<EndpointLabel, Counter>,
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
        let request_bytes = Family::<EndpointLabel, Counter>::default();
        let response_bytes = Family::<EndpointLabel, Counter>::default();
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

        Self {
            requests,
            request_duration_seconds,
            request_bytes,
            response_bytes,
            query_total,
            query_errors,
            stage_duration_seconds,
        }
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
