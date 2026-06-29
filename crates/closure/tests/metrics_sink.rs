//! Each valued transitive-closure call must record its `ClosureMetrics` into
//! the global metrics sink (`horndb_metrics::metrics().closure`).

use horndb_closure::grb::{init_once, ValuedMatrix};
use horndb_closure::metrics::{valued_transitive_closure, ValuedKernel};

/// Parse the `horndb_closure_total_seconds_count <n>` value from the prometheus
/// text exposition. The histogram is registered at init, so the `_count` line
/// is always present — what proves a sample was recorded is its value being > 0.
fn total_seconds_count(text: &str) -> u64 {
    text.lines()
        .find_map(|l| l.strip_prefix("horndb_closure_total_seconds_count "))
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or_else(|| panic!("no horndb_closure_total_seconds_count line in:\n{text}"))
}

/// Build the smallest graph an existing closure test uses, run the closure, and
/// assert the global sink recorded a histogram sample.
#[test]
fn closure_call_records_metrics() {
    init_once().unwrap();
    let m = ValuedMatrix::from_weighted_edges(3, &[(0, 1, 0.9), (1, 2, 0.8), (0, 2, 0.5)]).unwrap();
    let _ = valued_transitive_closure(&m, ValuedKernel::Builtin).unwrap();

    let text = horndb_metrics::encode_metrics();
    assert!(
        text.contains("horndb_closure_total_seconds"),
        "got:\n{text}"
    );
    assert!(
        text.contains("horndb_closure_total_seconds_count"),
        "got:\n{text}"
    );
    assert!(
        total_seconds_count(&text) >= 1,
        "expected the closure call to record a sample, got count 0:\n{text}"
    );
    assert!(text.contains("horndb_closure_input_nnz"), "got:\n{text}");
}
