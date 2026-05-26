//! Dispatches each selected test case against a `Reasoner` and
//! classifies the outcome (SPEC-01 F1/F2).

use std::fs;
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result};
use oxrdf::{Dataset, Graph, GraphName, Quad};
use oxttl::{NTriplesParser, TurtleParser};

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
            // SPEC-01 Stage-1 hand-rolled rule-coverage subset.
            "owl2" => Suite::Owl2,
            // Synthesised from the W3C OWL 2 RL profile aggregate via
            // `harness extract-owl2-rl`; same shape as `owl2`, just
            // larger.
            "owl2-w3c-rl" => Suite::Owl2,
            "sparql11" => Suite::Sparql11,
            // W3C RDF 1.2 N-Triples syntax tests. The manifest uses the
            // rdft: vocabulary (`TestNTriplesPositiveSyntax` /
            // `TestNTriplesNegativeSyntax`), parsed by the same
            // `manifest::parse` entry point as the mf:* suites.
            "rdf12-n-triples" => Suite::Rdf12NTriples,
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
            // Selected IDs may be either exact (absolute IRI) or a
            // suffix (e.g. `#trivial-entail-true`) so they survive
            // moving the workspace root.
            if !suite_entry
                .include
                .iter()
                .any(|id| id == &case.id || case.id.ends_with(id.as_str()))
            {
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
        TestKind::PositiveEntailment {
            premise,
            conclusion,
        } => {
            let p = load_dataset(premise)?;
            let c = load_dataset(conclusion)?;
            engine.load(&p)?;
            if engine.entails(&c)? {
                (Status::Passed, None)
            } else {
                (
                    Status::Failed,
                    Some("premise did not entail conclusion".into()),
                )
            }
        }
        TestKind::NegativeEntailment {
            premise,
            conclusion,
        } => {
            let p = load_dataset(premise)?;
            let c = load_dataset(conclusion)?;
            engine.load(&p)?;
            if engine.entails(&c)? {
                (
                    Status::Failed,
                    Some("conclusion entailed but should not be".into()),
                )
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
                (
                    Status::Failed,
                    Some("expected consistent, got inconsistent".into()),
                )
            }
        }
        TestKind::Inconsistency { premise } => {
            let p = load_dataset(premise)?;
            engine.load(&p)?;
            if !engine.is_consistent()? {
                (Status::Passed, None)
            } else {
                (
                    Status::Failed,
                    Some("expected inconsistent, got consistent".into()),
                )
            }
        }
        TestKind::SyntaxPositive { input } => {
            // The W3C RDF 1.2 N-Triples syntax suite asserts only that
            // the parser accepts/rejects the input — no reasoner. We
            // use `oxttl::NTriplesParser` directly because it is the
            // same parser the storage crate's N-Triples loader uses;
            // running it here keeps the harness self-contained
            // (avoiding a `horndb-storage` dep just for this one suite).
            // I/O errors (missing fixture, unreadable file) propagate via
            // `?` so they surface as a harness error rather than a silent
            // test failure.
            let bytes = read_syntax_input(input)?;
            match parse_ntriples_bytes(&bytes) {
                Ok(_) => (Status::Passed, None),
                Err(e) => (
                    Status::Failed,
                    Some(format!("positive syntax test failed to parse: {e}")),
                ),
            }
        }
        TestKind::SyntaxNegative { input } => {
            // Read the file outside the parse call so an I/O error (e.g.
            // a missing fixture or a broken `mf:action` path) is *not*
            // silently turned into a passing rejection. Only a parse
            // failure on bytes we successfully read counts as the
            // expected outcome.
            let bytes = read_syntax_input(input)?;
            match parse_ntriples_bytes(&bytes) {
                Ok(_) => (
                    Status::Failed,
                    Some("negative syntax test parsed successfully but should have failed".into()),
                ),
                Err(_) => (Status::Passed, None),
            }
        }
        TestKind::SparqlAsk {
            query,
            data,
            expected,
        } => {
            let d = load_dataset(data)?;
            engine.load(&d)?;
            let q = fs::read_to_string(query)
                .with_context(|| format!("reading query {}", query.display()))?;
            let got = engine.ask(&q)?;
            if got == *expected {
                (Status::Passed, None)
            } else {
                (
                    Status::Failed,
                    Some(format!("ASK got {got}, expected {expected}")),
                )
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

/// Read a syntax-suite input file. I/O errors here mean a misconfigured
/// fixture / selection (file not found, permission denied) — never the
/// expected outcome of a negative test — so they propagate to the
/// caller and surface as a harness error rather than a silent pass.
fn read_syntax_input(path: &Path) -> Result<Vec<u8>> {
    fs::read(path).with_context(|| format!("reading nt {}", path.display()))
}

/// Parse already-read N-Triples bytes and return the number of triples
/// on success. Used by the W3C RDF 1.2 N-Triples syntax suite: a parse
/// failure here is *expected* for the negative cases and the caller
/// turns it into a Passed outcome.
fn parse_ntriples_bytes(bytes: &[u8]) -> Result<usize> {
    let parser = NTriplesParser::new();
    let mut count = 0;
    for t in parser.for_slice(bytes) {
        t.context("parsing N-Triples bytes")?;
        count += 1;
    }
    Ok(count)
}

fn load_dataset(path: &Path) -> Result<Dataset> {
    let bytes = fs::read(path).with_context(|| format!("reading rdf {}", path.display()))?;
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
        p.pop();
        p.pop(); // back to workspace root
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
        let selected = Selected {
            version: 1,
            suites,
            removed: vec![],
            sparql_query: None,
        };

        let mut engine = StubReasoner::new();
        let report = run_selected(&mut engine, &selected, &fixtures(), &|p, s| {
            crate::manifest::parse(p, s)
        })
        .unwrap();

        assert_eq!(
            report.outcomes.len(),
            cases.len(),
            "one outcome per case in the OWL 2 fixture manifest",
        );

        let by_id = |id_suffix: &str| -> &Outcome {
            report
                .outcomes
                .iter()
                .find(|o| o.test_id.ends_with(id_suffix))
                .unwrap_or_else(|| panic!("missing outcome for {id_suffix}"))
        };

        // Stub's contract (see crate::stub): entails returns true iff the
        // conclusion is empty, is_consistent flags only explicit
        // owl:Nothing membership. So:
        //  - trivial-entail-true (empty conclusion)         → Passed
        //  - subclass-entail (non-empty conclusion)         → Failed
        //  - inconsistent-001 (explicit owl:Nothing)        → Passed
        //  - negative-subclass-no-instance (negative ent.)  → Passed
        //    (stub's "not entailed" is the *correct* answer for the
        //    negative-entailment test, even though the stub got there
        //    by knowing nothing.)
        assert_eq!(by_id("trivial-entail-true").status, Status::Passed);
        assert_eq!(by_id("subclass-entail").status, Status::Failed);
        assert_eq!(by_id("inconsistent-001").status, Status::Passed);
        assert_eq!(
            by_id("negative-subclass-no-instance").status,
            Status::Passed
        );
    }

    #[test]
    fn negative_syntax_test_with_missing_input_does_not_silently_pass() {
        // Regression: a SyntaxNegative case whose input file is absent
        // must surface as an I/O error from `run_one` (which
        // `run_selected` then turns into a Failed outcome), *not* a
        // silent Passed. Before splitting I/O from parsing, the missing
        // file was swallowed by the "Err(_) => Passed" arm.
        let case = TestCase {
            id: "#missing-negative".to_string(),
            suite: Suite::Rdf12NTriples,
            name: "missing input".to_string(),
            kind: TestKind::SyntaxNegative {
                input: PathBuf::from("/nonexistent/path/to/fixture.nt"),
            },
        };
        let mut engine = StubReasoner::new();
        let err = run_one(&mut engine, &case)
            .expect_err("missing fixture must surface as a harness error, not a passing outcome");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("reading nt"),
            "expected I/O-error context, got {msg:?}",
        );
    }

    #[test]
    fn positive_syntax_test_passes_on_valid_ntriples() {
        // Smoke test for the SyntaxPositive arm using an in-tree
        // fixture from the RDF 1.2 N-Triples suite.
        let case = TestCase {
            id: "#positive-smoke".to_string(),
            suite: Suite::Rdf12NTriples,
            name: "positive smoke".to_string(),
            kind: TestKind::SyntaxPositive {
                input: fixtures()
                    .join("crates/harness/tests/fixtures/rdf12-n-triples/ntriples12-syntax-01.nt"),
            },
        };
        let mut engine = StubReasoner::new();
        let outcome = run_one(&mut engine, &case).unwrap();
        assert_eq!(outcome.status, Status::Passed);
    }

    #[test]
    fn negative_syntax_test_passes_when_parse_rejects() {
        // Pair the missing-input test: a bad-syntax fixture (parser
        // *should* reject) must produce a Passed outcome.
        let case = TestCase {
            id: "#negative-smoke".to_string(),
            suite: Suite::Rdf12NTriples,
            name: "negative smoke".to_string(),
            kind: TestKind::SyntaxNegative {
                input: fixtures().join(
                    "crates/harness/tests/fixtures/rdf12-n-triples/ntriples12-bad-syntax-01.nt",
                ),
            },
        };
        let mut engine = StubReasoner::new();
        let outcome = run_one(&mut engine, &case).unwrap();
        assert_eq!(outcome.status, Status::Passed);
    }
}
