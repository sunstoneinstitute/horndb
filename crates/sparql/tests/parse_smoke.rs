//! Smoke test for the public crate surface. Verifies the modules
//! and error type are exported as documented in the plan.

use horndb_sparql::SparqlError;

#[test]
fn error_type_displays() {
    let err = SparqlError::Parse("nope".into());
    let rendered = format!("{err}");
    assert!(rendered.contains("nope"), "got: {rendered}");
}
