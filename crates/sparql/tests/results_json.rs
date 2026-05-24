use reasoner_sparql::algebra::Term;
use reasoner_sparql::exec::Bindings;
use reasoner_sparql::results::json::write_select_json;

#[test]
fn select_json_shape() {
    let mut b = Bindings::new();
    b.set("x", Term::Iri("http://ex/a".into()));
    b.set(
        "y",
        Term::Literal("\"42\"^^<http://www.w3.org/2001/XMLSchema#integer>".into()),
    );
    let rows = vec![b];
    let vars = vec!["x".to_string(), "y".to_string()];
    let json = write_select_json(&vars, &rows);
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["head"]["vars"], serde_json::json!(["x", "y"]));
    let binding = &parsed["results"]["bindings"][0];
    assert_eq!(binding["x"]["type"], "uri");
    assert_eq!(binding["x"]["value"], "http://ex/a");
    assert!(binding["y"]["type"] == "literal" || binding["y"]["type"] == "typed-literal");
}
