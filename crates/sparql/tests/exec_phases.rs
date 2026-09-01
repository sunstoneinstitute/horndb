//! Exclusivity check for per-operator exec-phase instrumentation (HDB-99).
//!
//! With `HORNDB_EXEC_PHASES=1`, every named phase this query touches must be
//! non-zero, and their sum must never exceed the `exec` stage's own elapsed
//! time. That inequality is the machine-checkable form of "phases are
//! exclusive by construction": every phase clocks only its own work
//! statement, never a child operator's `next()`.
#![cfg(feature = "server")]

use horndb_sparql::algebra::Term;
use horndb_sparql::api::execute_query;
use horndb_sparql::exec::horn::HornBackend;
use horndb_sparql::exec::Store;
use std::collections::HashMap;

fn iri(s: &str) -> Term {
    Term::Iri(s.into())
}

fn int_lit(n: u32) -> Term {
    Term::Literal(format!(
        "\"{n}\"^^<http://www.w3.org/2001/XMLSchema#integer>"
    ))
}

/// Enough rows across enough groups that scan/group/sort/stream/encode
/// phases all accumulate measurable nanoseconds, even at the host's clock
/// resolution. Uses `HornBackend` (not `MemStore`) because `scan_wcoj` /
/// `scan_row_build` are only instrumented inside `HornBackend::scan_bgp_ids`
/// — the WCOJ (leapfrog triejoin join engine) scan path.
///
/// The query below aggregates with `SUM`, not a plain `COUNT`: a plain-count
/// `GROUP BY` is pushdown-eligible (`plan::pushdown::lower_count_group`) and
/// would lower straight to a `GroupCountScanOp` that bypasses
/// `scan_bgp_ids`/`eval_group_native` entirely — exactly the operators this
/// test needs to exercise.
fn store() -> HornBackend {
    let mut st = HornBackend::new();
    for i in 0..200u32 {
        st.insert_triple(
            iri(&format!("http://ex/s{i}")),
            iri(&format!("http://ex/p{}", i % 5)),
            int_lit(i),
        );
    }
    st
}

/// Parse `horndb_sparql_exec_phase_nanoseconds_total{phase="x"} <n>` lines
/// out of scraped OpenMetrics text into a `phase -> ns` map.
fn phase_ns(text: &str) -> HashMap<String, u64> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("horndb_sparql_exec_phase_nanoseconds_total{") else {
            continue;
        };
        let Some((labels, val)) = rest.rsplit_once('}') else {
            continue;
        };
        let Some(phase) = labels.split(',').find_map(|kv| {
            kv.strip_prefix("phase=\"")
                .and_then(|v| v.strip_suffix('"'))
        }) else {
            continue;
        };
        let ns: u64 = val.trim().parse().expect("counter value is a number");
        out.insert(phase.to_owned(), ns);
    }
    out
}

/// Nanoseconds in the `exec` stage's `stage_duration_seconds` histogram sum
/// — the ceiling every named phase's total must stay under.
fn exec_stage_ns(text: &str) -> u64 {
    for line in text.lines() {
        if let Some(rest) =
            line.strip_prefix("horndb_sparql_stage_duration_seconds_sum{stage=\"exec\"} ")
        {
            let secs: f64 = rest.trim().parse().expect("sum value is a number");
            return (secs * 1e9) as u64;
        }
    }
    panic!("no exec-stage stage_duration_seconds_sum line in:\n{text}");
}

#[test]
fn exec_phases_are_exclusive() {
    std::env::set_var("HORNDB_EXEC_PHASES", "1");

    let st = store();
    let q = "SELECT ?p (SUM(?o) AS ?total) WHERE { ?s ?p ?o } GROUP BY ?p ORDER BY ?p";
    execute_query(q, &st).expect("query ok");

    let text = horndb_metrics::encode_metrics();
    let ns = phase_ns(&text);

    // Every phase a GROUP BY + ORDER BY query over one flat BGP touches.
    // `join_build`/`join_probe` are absent by design (Notes for the
    // implementer): a single flat BGP folds into one `BgpScan`, never a
    // `JoinOp`/`LeftJoinOp`.
    for phase in [
        "scan_wcoj",
        "scan_row_build",
        "scan_provenance",
        "group_key",
        "group_decode",
        "agg_fold",
        "sort",
        "result_encode",
    ] {
        assert!(
            ns.get(phase).copied().unwrap_or(0) > 0,
            "phase {phase} recorded 0 ns\n{text}"
        );
    }

    let sum_named: u64 = ns
        .iter()
        .filter(|(k, _)| k.as_str() != "residual")
        .map(|(_, v)| *v)
        .sum();
    let exec_ns = exec_stage_ns(&text);
    assert!(
        sum_named <= exec_ns,
        "sum(named phases) = {sum_named} ns exceeds exec = {exec_ns} ns — phases are not exclusive\n{text}"
    );
}
