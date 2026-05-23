//! Dispatches each selected test case against a `Reasoner` and
//! classifies the outcome (SPEC-01 F1/F2).

use std::fs;
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result};
use oxrdf::{Dataset, Graph, GraphName, Quad};
use oxttl::TurtleParser;

use crate::outcome::{Outcome, Report, Status};
use crate::reasoner::Reasoner;
use crate::selected::Selected;
use crate::testcase::{Suite, TestCase, TestKind};

/// Loads each selected suite's manifest, filters down to the selected
/// IDs, runs each through `engine`, and produces a [`Report`].
pub fn run_selected(
    engine: &mut dyn Reasoner,
    selected: &Selected,
    workspace_root: &Path,
    manifest_loader: &dyn Fn(&Path, Suite) -> Result<Vec<TestCase>>,
) -> Result<Report> {
    let mut report = Report::new();
    for (suite_name, suite_entry) in &selected.suites {
        let suite = match suite_name.as_str() {
            "owl2" => Suite::Owl2,
            "sparql11" => Suite::Sparql11,
            other => {
                report.push(Outcome {
                    test_id: format!("<suite:{other}>"),
                    suite: other.to_string(),
                    status: Status::Skipped,
                    reason: Some(format!("unknown suite {other}")),
                    duration_ms: 0,
                });
                continue;
            }
        };
        let manifest_path = workspace_root.join(&suite_entry.manifest);
        let cases = manifest_loader(&manifest_path, suite)
            .with_context(|| format!("loading manifest {}", manifest_path.display()))?;
        for case in &cases {
            if !suite_entry.include.iter().any(|id| id == &case.id) {
                continue;
            }
            let start = Instant::now();
            let outcome = run_one(engine, case).unwrap_or_else(|e| Outcome {
                test_id: case.id.clone(),
                suite: suite_name.clone(),
                status: Status::Failed,
                reason: Some(format!("harness error: {e:#}")),
                duration_ms: start.elapsed().as_millis() as u64,
            });
            report.push(outcome);
        }
    }
    Ok(report)
}

fn run_one(engine: &mut dyn Reasoner, case: &TestCase) -> Result<Outcome> {
    let start = Instant::now();
    let suite = case.suite.as_str().to_string();
    let id = case.id.clone();

    let (status, reason) = match &case.kind {
        TestKind::PositiveEntailment { premise, conclusion } => {
            let p = load_dataset(premise)?;
            let c = load_dataset(conclusion)?;
            engine.load(&p)?;
            if engine.entails(&c)? {
                (Status::Passed, None)
            } else {
                (Status::Failed, Some("premise did not entail conclusion".into()))
            }
        }
        TestKind::NegativeEntailment { premise, conclusion } => {
            let p = load_dataset(premise)?;
            let c = load_dataset(conclusion)?;
            engine.load(&p)?;
            if engine.entails(&c)? {
                (Status::Failed, Some("conclusion entailed but should not be".into()))
            } else {
                (Status::Passed, None)
            }
        }
        TestKind::Consistency { premise } => {
            let p = load_dataset(premise)?;
            engine.load(&p)?;
            if engine.is_consistent()? {
                (Status::Passed, None)
            } else {
                (Status::Failed, Some("expected consistent, got inconsistent".into()))
            }
        }
        TestKind::Inconsistency { premise } => {
            let p = load_dataset(premise)?;
            engine.load(&p)?;
            if !engine.is_consistent()? {
                (Status::Passed, None)
            } else {
                (Status::Failed, Some("expected inconsistent, got consistent".into()))
            }
        }
        TestKind::SparqlAsk { query, data, expected } => {
            let d = load_dataset(data)?;
            engine.load(&d)?;
            let q = fs::read_to_string(query)
                .with_context(|| format!("reading query {}", query.display()))?;
            let got = engine.ask(&q)?;
            if got == *expected {
                (Status::Passed, None)
            } else {
                (Status::Failed, Some(format!("ASK got {got}, expected {expected}")))
            }
        }
    };

    Ok(Outcome {
        test_id: id,
        suite,
        status,
        reason,
        duration_ms: start.elapsed().as_millis() as u64,
    })
}

fn load_dataset(path: &Path) -> Result<Dataset> {
    let bytes = fs::read(path)
        .with_context(|| format!("reading rdf {}", path.display()))?;
    let base_iri = format!("file://{}", path.display());
    let mut graph = Graph::new();
    let parser = TurtleParser::new()
        .with_base_iri(&base_iri)?
        .for_slice(&bytes);
    for triple in parser {
        let t = triple?;
        graph.insert(&t);
    }
    let mut dataset = Dataset::new();
    for triple in graph.iter() {
        dataset.insert(&Quad::new(
            triple.subject.into_owned(),
            triple.predicate.into_owned(),
            triple.object.into_owned(),
            GraphName::DefaultGraph,
        ));
    }
    Ok(dataset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stub::StubReasoner;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn fixtures() -> PathBuf {
        let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        p.pop(); p.pop(); // back to workspace root
        p
    }

    #[test]
    fn stub_passes_trivial_and_inconsistent_fails_subclass() {
        // Build a Selected programmatically that matches the fixture IDs.
        let cases = crate::manifest::parse(
            &fixtures().join("crates/harness/tests/fixtures/owl2/manifest.ttl"),
            Suite::Owl2,
        )
        .unwrap();
        let mut suites = BTreeMap::new();
        suites.insert(
            "owl2".to_string(),
            crate::selected::SuiteEntry {
                manifest: "crates/harness/tests/fixtures/owl2/manifest.ttl".to_string(),
                include: cases.iter().map(|c| c.id.clone()).collect(),
            },
        );
        let selected = Selected { version: 1, suites, removed: vec![] };

        let mut engine = StubReasoner::new();
        let report = run_selected(
            &mut engine,
            &selected,
            &fixtures(),
            &|p, s| crate::manifest::parse(p, s),
        )
        .unwrap();

        assert_eq!(report.outcomes.len(), 3, "all three OWL2 fixtures run");

        let by_id = |id_suffix: &str| -> &Outcome {
            report
                .outcomes
                .iter()
                .find(|o| o.test_id.ends_with(id_suffix))
                .unwrap_or_else(|| panic!("missing outcome for {id_suffix}"))
        };

        assert_eq!(by_id("trivial-entail-true").status, Status::Passed);
        assert_eq!(by_id("subclass-entail").status, Status::Failed);
        assert_eq!(by_id("inconsistent-001").status, Status::Passed);
    }
}
