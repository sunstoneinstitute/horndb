//! Shared RDF I/O helpers used by the runners.

use std::path::Path;

use anyhow::{Context, Result};
use oxrdf::{Dataset, GraphName, Quad};
use oxttl::TurtleParser;

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
