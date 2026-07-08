//! Shared RDF I/O helpers used by the runners.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{bail, Context, Result};
use oxrdf::{Dataset, GraphName, NamedNode, Quad, TermRef};
use oxttl::TurtleParser;
use serde::Deserialize;

/// `owl:imports` predicate IRI.
const OWL_IMPORTS: &str = "http://www.w3.org/2002/07/owl#imports";

/// Filename of the per-directory import catalog (see [`expand_imports`]).
const IMPORTS_CATALOG: &str = "imports-catalog.toml";

/// Parse a Turtle file into a [`Dataset`] in the default graph.
///
/// The parser's base IRI is seeded from the file path (`file://…`) so
/// relative IRIs in the document resolve the same way every caller
/// expects.
pub(crate) fn load_turtle_dataset(path: &Path) -> Result<Dataset> {
    let bytes = std::fs::read(path).with_context(|| format!("reading rdf {}", path.display()))?;
    let base_iri = format!("file://{}", path.display());
    let parser = TurtleParser::new()
        .with_base_iri(&base_iri)?
        .for_slice(&bytes);
    let mut dataset = Dataset::new();
    for triple in parser {
        let t = triple?;
        dataset.insert(&Quad::new(
            t.subject,
            t.predicate,
            t.object,
            GraphName::DefaultGraph,
        ));
    }
    Ok(dataset)
}

/// Load a premise dataset and resolve its `owl:imports` against the
/// directory-local catalog.
///
/// A premise ontology may `owl:imports` another ontology whose axioms are
/// needed for the entailment to hold (e.g. `WebOnt-imports-011`). The harness
/// runs hermetically — no network — so imports are resolved through a checked-in
/// [`IMPORTS_CATALOG`] that maps each import IRI to a mirrored Turtle fixture.
/// See [`expand_imports`] for the resolution rules.
pub(crate) fn load_premise(path: &Path) -> Result<Dataset> {
    let mut dataset = load_turtle_dataset(path)?;
    resolve_imports(&mut dataset, path)?;
    Ok(dataset)
}

/// Resolve `owl:imports` in an already-loaded premise dataset in place, using
/// the catalog in the premise file's directory.
///
/// Separate from [`load_premise`] so callers that must route the initial parse
/// elsewhere (e.g. a `.sssom.tsv` premise) can still expand imports afterwards.
/// A dataset with no `owl:imports` is untouched (no catalog is even read), so
/// this is safe to call on every premise regardless of format.
pub(crate) fn resolve_imports(dataset: &mut Dataset, premise_path: &Path) -> Result<()> {
    let dir = premise_path.parent().unwrap_or_else(|| Path::new("."));
    expand_imports(dataset, dir)
}

/// The `[imports]` table of an [`IMPORTS_CATALOG`] file: import IRI → fixture
/// path (relative to the catalog's directory).
#[derive(Debug, Deserialize)]
struct ImportsCatalog {
    #[serde(default)]
    imports: HashMap<String, String>,
}

/// Load the import catalog from `dir`, or `None` if the directory has none.
///
/// Absent catalog ⇒ no imports to resolve; a directory without the file is the
/// common case (only the owl2-w3c-rl fixtures ship one), so this is not an
/// error.
fn load_catalog(dir: &Path) -> Result<Option<ImportsCatalog>> {
    let path = dir.join(IMPORTS_CATALOG);
    if !path.exists() {
        return Ok(None);
    }
    let raw =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let catalog: ImportsCatalog =
        toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
    Ok(Some(catalog))
}

/// Merge every ontology `dataset` (transitively) `owl:imports` into it.
///
/// Rules:
/// - Each `?s owl:imports ?o` with an IRI object `?o` is resolved through the
///   directory-local [`IMPORTS_CATALOG`] to a Turtle fixture, whose triples are
///   merged in. Imports discovered in a merged ontology are followed too.
/// - A visited-set keyed on the import IRI makes resolution cycle-safe and
///   avoids re-loading a shared import.
/// - An `owl:imports` with no catalog entry is a hard error: a premise that
///   references an unmirrored ontology cannot be graded soundly offline, so we
///   fail loud rather than silently drop the import.
///
/// If the directory has no catalog, any `owl:imports` present is likewise an
/// error (the fixture claims an import the suite can't resolve).
fn expand_imports(dataset: &mut Dataset, dir: &Path) -> Result<()> {
    let mut pending = collect_imports(dataset);
    if pending.is_empty() {
        return Ok(());
    }
    let catalog = load_catalog(dir)?.map(|c| c.imports).unwrap_or_default();

    let mut visited: HashSet<String> = HashSet::new();
    while let Some(iri) = pending.pop() {
        if !visited.insert(iri.clone()) {
            continue;
        }
        let Some(rel) = catalog.get(&iri) else {
            bail!(
                "owl:imports <{iri}> has no entry in {}/{IMPORTS_CATALOG}; \
                 add a mirrored fixture (the harness resolves imports offline)",
                dir.display()
            );
        };
        let imported = load_turtle_dataset(&dir.join(rel))?;
        pending.extend(collect_imports(&imported));
        for quad in imported.iter() {
            dataset.insert(quad);
        }
    }
    Ok(())
}

/// Collect the IRIs named by `?s owl:imports ?o` triples in `dataset`.
///
/// Non-IRI objects (blank nodes, literals) are ignored: `owl:imports` targets
/// an ontology IRI, and anything else is not resolvable.
fn collect_imports(dataset: &Dataset) -> Vec<String> {
    let imports = NamedNode::new(OWL_IMPORTS).expect("owl:imports is a valid IRI");
    dataset
        .quads_for_predicate(&imports)
        .filter_map(|quad| match quad.object {
            TermRef::NamedNode(n) => Some(n.as_str().to_owned()),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixtures_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/owl2-w3c-rl")
    }

    fn has_triple(ds: &Dataset, s: &str, p: &str, o: &str) -> bool {
        let s = NamedNode::new(s).unwrap();
        let p = NamedNode::new(p).unwrap();
        let o = NamedNode::new(o).unwrap();
        ds.iter().any(|q| {
            q.subject == (&s).into() && q.predicate == p.as_ref() && q.object == (&o).into()
        })
    }

    #[test]
    fn load_premise_merges_imported_ontology() {
        let premise = fixtures_dir().join("WebOnt-imports-011.premise.ttl");
        let ds = load_premise(&premise).unwrap();

        // The imported support011-A axioms must now be present…
        assert!(
            has_triple(
                &ds,
                "http://www.w3.org/2002/03owlt/imports/support011-A#Man",
                "http://www.w3.org/2000/01/rdf-schema#subClassOf",
                "http://www.w3.org/2002/03owlt/imports/support011-A#Mortal",
            ),
            "imported Man ⊑ Mortal axiom missing after import expansion",
        );
        assert!(
            has_triple(
                &ds,
                "http://www.w3.org/2002/03owlt/imports/support011-A#Mortal",
                "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
                "http://www.w3.org/2002/07/owl#Class",
            ),
            "imported `Mortal a owl:Class` missing after import expansion",
        );
        // …alongside the premise's own assertion (Socrates a Man).
        assert!(has_triple(
            &ds,
            "http://example.org/data#Socrates",
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
            "http://www.w3.org/2002/03owlt/imports/support011-A#Man",
        ));
    }

    #[test]
    fn premise_without_imports_is_unchanged() {
        // A fixture with no owl:imports loads identically through either path.
        let premise = fixtures_dir().join("WebOnt-imports-011.conclusion.ttl");
        let plain = load_turtle_dataset(&premise).unwrap();
        let via_premise = load_premise(&premise).unwrap();
        assert_eq!(plain.len(), via_premise.len());
    }

    #[test]
    fn unmapped_import_is_a_hard_error() {
        let dir = tempfile::tempdir().unwrap();
        let premise = dir.path().join("p.ttl");
        std::fs::write(
            &premise,
            "<http://ex/o> <http://www.w3.org/2002/07/owl#imports> <http://ex/unmapped> .",
        )
        .unwrap();
        let err = load_premise(&premise).unwrap_err();
        assert!(
            err.to_string().contains("has no entry"),
            "expected a loud unmapped-import error, got: {err}",
        );
    }

    #[test]
    fn import_cycle_terminates() {
        // A ⇒ B ⇒ A must not loop forever; the visited-set breaks the cycle.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(IMPORTS_CATALOG),
            "[imports]\n\
             \"http://ex/A\" = \"a.ttl\"\n\
             \"http://ex/B\" = \"b.ttl\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("a.ttl"),
            "<http://ex/A> <http://www.w3.org/2002/07/owl#imports> <http://ex/B> .\n\
             <http://ex/A> <http://ex/p> <http://ex/inA> .",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("b.ttl"),
            "<http://ex/B> <http://www.w3.org/2002/07/owl#imports> <http://ex/A> .\n\
             <http://ex/B> <http://ex/p> <http://ex/inB> .",
        )
        .unwrap();
        let premise = dir.path().join("p.ttl");
        std::fs::write(
            &premise,
            "<http://ex/o> <http://www.w3.org/2002/07/owl#imports> <http://ex/A> .",
        )
        .unwrap();

        let ds = load_premise(&premise).unwrap();
        assert!(has_triple(
            &ds,
            "http://ex/A",
            "http://ex/p",
            "http://ex/inA"
        ));
        assert!(has_triple(
            &ds,
            "http://ex/B",
            "http://ex/p",
            "http://ex/inB"
        ));
    }
}
