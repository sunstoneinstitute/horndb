//! ORE 2015 runner wrapper (SPEC-01 F3).
//!
//! Stage-1 scope: a hand-picked subset of 10 ontologies known to be
//! OWL 2 RL clean. We do not run the full 1,920-ontology corpus until
//! Stage 2. Time budget per ontology: 5 minutes wall clock, matching
//! the ORE 2015 competition rules.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::outcome::{Outcome, Report, Status};
use crate::reasoner::Reasoner;

#[derive(Debug, Deserialize)]
pub struct OreSelected {
    pub version: u32,
    pub ontologies: Vec<OreOntology>,
}

#[derive(Debug, Deserialize)]
pub struct OreOntology {
    pub id: String,
    pub path: String,
    pub task: OreTask,
    /// Optional `(ASK query, expected)` for realisation/classification spot-check.
    #[serde(default)]
    pub smoke: Option<OreSmoke>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OreTask {
    Consistency,
}

#[derive(Debug, Deserialize)]
pub struct OreSmoke {
    pub ask: String,
    pub expected: bool,
}

const PER_ONTOLOGY_BUDGET: Duration = Duration::from_secs(5 * 60);

pub fn load_selected(path: &Path) -> Result<OreSelected> {
    let raw =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let parsed: OreSelected = toml::from_str(&raw)?;
    if parsed.version != 1 {
        anyhow::bail!("unsupported ore selected version {}", parsed.version);
    }
    Ok(parsed)
}

pub fn run(engine: &mut dyn Reasoner, selected: &OreSelected, root: &Path) -> Result<Report> {
    let mut report = Report::new();
    for ont in &selected.ontologies {
        let start = Instant::now();
        let outcome = match run_one(engine, ont, root, start) {
            Ok(o) => o,
            Err(e) => Outcome {
                test_id: ont.id.clone(),
                suite: "ore2015".into(),
                status: Status::Failed,
                reason: Some(format!("harness error: {e:#}")),
                duration_ms: start.elapsed().as_millis() as u64,
            },
        };
        report.push(outcome);
    }
    Ok(report)
}

fn run_one(
    engine: &mut dyn Reasoner,
    ont: &OreOntology,
    root: &Path,
    start: Instant,
) -> Result<Outcome> {
    let path: PathBuf = root.join(&ont.path);
    let bytes = std::fs::read(&path)?;
    let base_iri = format!("file://{}", path.display());
    let mut graph = oxrdf::Graph::new();
    let parser = oxttl::TurtleParser::new()
        .with_base_iri(&base_iri)?
        .for_slice(&bytes);
    for triple in parser {
        let t = triple?;
        graph.insert(&t);
    }
    let mut dataset = oxrdf::Dataset::new();
    for triple in graph.iter() {
        dataset.insert(&oxrdf::Quad::new(
            triple.subject.into_owned(),
            triple.predicate.into_owned(),
            triple.object.into_owned(),
            oxrdf::GraphName::DefaultGraph,
        ));
    }

    engine.load(&dataset)?;
    if start.elapsed() > PER_ONTOLOGY_BUDGET {
        return Ok(Outcome {
            test_id: ont.id.clone(),
            suite: "ore2015".into(),
            status: Status::Failed,
            reason: Some("exceeded 5-minute per-ontology budget".into()),
            duration_ms: start.elapsed().as_millis() as u64,
        });
    }

    let consistent = engine.is_consistent()?;
    let mut status = if consistent {
        Status::Passed
    } else {
        Status::Failed
    };
    let mut reason = if consistent {
        None
    } else {
        Some("expected consistent, got inconsistent".into())
    };

    if let Some(smoke) = &ont.smoke {
        let got = engine.ask(&smoke.ask)?;
        if got != smoke.expected {
            status = Status::Failed;
            reason = Some(format!("smoke ASK got {got}, expected {}", smoke.expected));
        }
    }

    Ok(Outcome {
        test_id: ont.id.clone(),
        suite: "ore2015".into(),
        status,
        reason,
        duration_ms: start.elapsed().as_millis() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ore_selected_toml() {
        let raw = r#"version = 1
[[ontologies]]
id = "fma-lite"
path = "ore2015/fma-lite.ttl"
task = "consistency"
"#;
        let parsed: OreSelected = toml::from_str(raw).unwrap();
        assert_eq!(parsed.ontologies.len(), 1);
        assert!(matches!(parsed.ontologies[0].task, OreTask::Consistency));
    }
}
