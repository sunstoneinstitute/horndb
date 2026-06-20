//! Pure-Rust `RdfGraph`: the rdflib-`Graph` facade's engine.
//!
//! Wraps a SPEC-07 [`MemStore`] and presents add/remove/contains/iteration,
//! Turtle/N-Triples parse & serialise, and SPARQL query/update — all in terms
//! of the kind-preserving [`RdfTerm`]. No PyO3 here, so the whole facade is
//! exercised by `cargo test` without a Python interpreter.

use std::io::Cursor;

use horndb_sparql::api::{execute_query, execute_update, QueryAnswer};
use horndb_sparql::exec::mem::MemStore;
use oxrdf::{BlankNode, GraphName, NamedNode, NamedOrBlankNode, Quad, Term as OxTerm};
use oxrdfio::{RdfFormat, RdfParser, RdfSerializer};

use crate::term::RdfTerm;

/// Errors surfaced across the binding boundary. The Python layer maps these to
/// rdflib-like exception types (SPEC-10 F7).
#[derive(Debug)]
pub enum GraphError {
    /// A SPARQL parse/plan/exec error from the SPEC-07 frontend.
    Sparql(String),
    /// A parse or serialise error from the RDF I/O layer.
    Io(String),
    /// A term that cannot be used where the operation requires it (e.g. a
    /// literal as a triple subject).
    Term(String),
}

impl std::fmt::Display for GraphError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GraphError::Sparql(m) => write!(f, "SPARQL error: {m}"),
            GraphError::Io(m) => write!(f, "RDF I/O error: {m}"),
            GraphError::Term(m) => write!(f, "term error: {m}"),
        }
    }
}

impl std::error::Error for GraphError {}

type Result<T> = std::result::Result<T, GraphError>;

/// One of the two RDF serialisation formats this increment supports (F4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SerFormat {
    Turtle,
    NTriples,
}

impl SerFormat {
    /// Resolve an rdflib-style `format=` string. rdflib accepts a handful of
    /// aliases per format; we map the common ones and reject the rest with a
    /// clear error rather than silently guessing (NF4).
    pub fn from_name(name: &str) -> Result<Self> {
        match name.to_ascii_lowercase().as_str() {
            "turtle" | "ttl" | "n3" => Ok(SerFormat::Turtle),
            "nt" | "ntriples" | "nt11" | "ntriples-star" | "application/n-triples" => {
                Ok(SerFormat::NTriples)
            }
            other => Err(GraphError::Io(format!(
                "unsupported format {other:?}; this build supports 'turtle' and 'nt'"
            ))),
        }
    }

    fn rdf_format(self) -> RdfFormat {
        match self {
            SerFormat::Turtle => RdfFormat::Turtle,
            SerFormat::NTriples => RdfFormat::NTriples,
        }
    }
}

/// The rdflib `Graph` engine.
#[derive(Default)]
pub struct RdfGraph {
    store: MemStore,
}

impl RdfGraph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of distinct triples (rdflib `len(graph)`).
    pub fn len(&self) -> usize {
        self.store.len()
    }

    pub fn is_empty(&self) -> bool {
        self.store.is_empty()
    }

    /// Add a triple. The subject must be an IRI or blank node, the predicate an
    /// IRI; rdflib raises on a malformed triple, and so do we (F7).
    pub fn add(&mut self, s: &RdfTerm, p: &RdfTerm, o: &RdfTerm) -> Result<()> {
        check_subject(s)?;
        check_predicate(p)?;
        self.store.insert((
            s.to_store_lexical(),
            p.to_store_lexical(),
            o.to_store_lexical(),
        ));
        Ok(())
    }

    /// Remove a triple (no-op if absent, matching rdflib).
    pub fn remove(&mut self, s: &RdfTerm, p: &RdfTerm, o: &RdfTerm) {
        use horndb_sparql::algebra::Term as AlgTerm;
        let st = AlgTerm::from_lexical_kind(s);
        let pt = AlgTerm::from_lexical_kind(p);
        let ot = AlgTerm::from_lexical_kind(o);
        use horndb_sparql::exec::Store;
        self.store.delete_triple(&st, &pt, &ot);
    }

    /// True if the exact triple is asserted (rdflib `(s,p,o) in graph`).
    pub fn contains(&self, s: &RdfTerm, p: &RdfTerm, o: &RdfTerm) -> bool {
        let target = (
            s.to_store_lexical(),
            p.to_store_lexical(),
            o.to_store_lexical(),
        );
        self.store
            .iter_triples()
            .any(|t| t.0 == target.0 && t.1 == target.1 && t.2 == target.2)
    }

    /// Iterate the triples matching an optional pattern. `None` in a position
    /// is a wildcard, mirroring rdflib's `graph.triples((s, p, o))` where any
    /// of `s`/`p`/`o` may be `None`.
    pub fn triples(
        &self,
        s: Option<&RdfTerm>,
        p: Option<&RdfTerm>,
        o: Option<&RdfTerm>,
    ) -> Vec<(RdfTerm, RdfTerm, RdfTerm)> {
        let sl = s.map(RdfTerm::to_store_lexical);
        let pl = p.map(RdfTerm::to_store_lexical);
        let ol = o.map(RdfTerm::to_store_lexical);
        self.store
            .iter_triples()
            .filter(|t| {
                sl.as_ref().map(|v| v == &t.0).unwrap_or(true)
                    && pl.as_ref().map(|v| v == &t.1).unwrap_or(true)
                    && ol.as_ref().map(|v| v == &t.2).unwrap_or(true)
            })
            .map(|t| {
                (
                    RdfTerm::from_store_lexical(&t.0),
                    RdfTerm::from_store_lexical(&t.1),
                    RdfTerm::from_store_lexical(&t.2),
                )
            })
            .collect()
    }

    /// Parse RDF text into the graph, accumulating triples (rdflib `parse`).
    pub fn parse_str(&mut self, data: &str, format: SerFormat) -> Result<()> {
        let parser = RdfParser::from_format(format.rdf_format());
        for quad in parser.for_reader(Cursor::new(data.as_bytes())) {
            let quad = quad.map_err(|e| GraphError::Io(e.to_string()))?;
            self.store.insert((
                subject_to_rdfterm(&quad.subject).to_store_lexical(),
                RdfTerm::iri(quad.predicate.as_str()).to_store_lexical(),
                object_to_rdfterm(&quad.object).to_store_lexical(),
            ));
        }
        Ok(())
    }

    /// Serialise the whole default graph (rdflib `serialize`). `prefixes` are
    /// `(prefix, namespace-IRI)` pairs bound via `Graph.bind(...)`; for Turtle
    /// they are emitted as `@prefix` declarations so the output uses the
    /// caller's QNames, matching rdflib. N-Triples has no prefix concept, so
    /// they are ignored there.
    pub fn serialize_str(
        &self,
        format: SerFormat,
        prefixes: &[(String, String)],
    ) -> Result<String> {
        let mut ser = RdfSerializer::from_format(format.rdf_format());
        if format == SerFormat::Turtle {
            for (prefix, iri) in prefixes {
                // Skip namespaces oxrdf rejects rather than failing the whole
                // serialisation over one bad binding.
                if let Ok(s) = ser.clone().with_prefix(prefix, iri) {
                    ser = s;
                }
            }
        }
        let mut buf = Vec::new();
        let mut writer = ser.for_writer(&mut buf);
        for t in self.store.iter_triples() {
            let quad = lexical_to_quad(t)?;
            writer
                .serialize_quad(quad.as_ref())
                .map_err(|e| GraphError::Io(e.to_string()))?;
        }
        writer.finish().map_err(|e| GraphError::Io(e.to_string()))?;
        String::from_utf8(buf).map_err(|e| GraphError::Io(e.to_string()))
    }

    /// Run a SPARQL query (SELECT/ASK/CONSTRUCT/DESCRIBE) — F5.
    pub fn query(&self, sparql: &str) -> Result<QueryResult> {
        let answer =
            execute_query(sparql, &self.store).map_err(|e| GraphError::Sparql(e.to_string()))?;
        Ok(match answer {
            QueryAnswer::Solutions { vars, rows } => {
                let solutions = rows
                    .into_iter()
                    .map(|b| {
                        vars.iter()
                            .map(|v| b.get(v).map(alg_term_to_rdfterm))
                            .collect()
                    })
                    .collect();
                QueryResult::Select { vars, solutions }
            }
            QueryAnswer::Boolean(b) => QueryResult::Ask(b),
            QueryAnswer::Triples(ts) => QueryResult::Construct(
                ts.into_iter()
                    .map(|(s, p, o)| {
                        (
                            RdfTerm::from_store_lexical(&s),
                            RdfTerm::from_store_lexical(&p),
                            RdfTerm::from_store_lexical(&o),
                        )
                    })
                    .collect(),
            ),
            QueryAnswer::Explanation { text, .. } => QueryResult::Explanation(text),
        })
    }

    /// Apply a SPARQL update (F5).
    pub fn update(&mut self, sparql: &str) -> Result<()> {
        execute_update(sparql, &mut self.store).map_err(|e| GraphError::Sparql(e.to_string()))
    }
}

/// The result of [`RdfGraph::query`], shaped for the Python layer.
#[derive(Debug)]
pub enum QueryResult {
    Select {
        vars: Vec<String>,
        /// One row per solution; `None` for an unbound variable.
        solutions: Vec<Vec<Option<RdfTerm>>>,
    },
    Ask(bool),
    Construct(Vec<(RdfTerm, RdfTerm, RdfTerm)>),
    Explanation(String),
}

fn check_subject(t: &RdfTerm) -> Result<()> {
    match t {
        RdfTerm::Iri(_) | RdfTerm::Blank(_) => Ok(()),
        RdfTerm::Literal { .. } => Err(GraphError::Term(
            "a literal cannot be a triple subject".into(),
        )),
    }
}

fn check_predicate(t: &RdfTerm) -> Result<()> {
    match t {
        RdfTerm::Iri(_) => Ok(()),
        _ => Err(GraphError::Term("a predicate must be an IRI".into())),
    }
}

/// Convert a value bound into a SPARQL `Bindings` row to a kind-preserving
/// [`RdfTerm`], mapping from the algebra `Term` *variant* rather than
/// re-classifying a lexical string. This matters for SPARQL expression results
/// (e.g. `isIRI(...) AS ?b`) whose `Term::Literal` payload may be a bare
/// `true`/`false` that lexical reclassification would wrongly surface as an
/// IRI. A `Term::Literal` whose payload is already N-Triples-quoted is parsed
/// to recover its datatype/language; an unquoted payload becomes a plain
/// literal, preserving the literal kind either way.
pub(crate) fn alg_term_to_rdfterm(t: &horndb_sparql::algebra::Term) -> RdfTerm {
    use horndb_sparql::algebra::Term as T;
    match t {
        T::Iri(s) => {
            // MemStore's `classify_lexical` cannot tell a blank node from an
            // IRI (both are non-`"`-prefixed), so a blank node the binding
            // stored as `_:b` comes back as `Term::Iri("_:b")`. Recover the
            // blank-node kind here so SPARQL bindings stay kind-faithful (F1),
            // matching graph-iteration semantics.
            if let Some(label) = s.strip_prefix("_:") {
                RdfTerm::blank(label)
            } else {
                RdfTerm::iri(s.trim_start_matches('<').trim_end_matches('>'))
            }
        }
        T::BlankNode(s) => RdfTerm::blank(s.strip_prefix("_:").unwrap_or(s)),
        T::Literal(s) => {
            if s.starts_with('"') {
                // Quoted N-Triples literal: recover datatype/lang faithfully.
                RdfTerm::from_store_lexical(s)
            } else {
                // Bare payload (e.g. an effective-boolean expression result):
                // a plain literal, NOT an IRI.
                RdfTerm::plain_literal(s.clone())
            }
        }
        T::Var(v) => RdfTerm::plain_literal(v.name()),
        T::Triple(_) => RdfTerm::plain_literal(String::new()),
    }
}

/// Convert a parsed subject node to a kind-preserving [`RdfTerm`].
pub(crate) fn subject_to_rdfterm(s: &NamedOrBlankNode) -> RdfTerm {
    match s {
        NamedOrBlankNode::NamedNode(n) => RdfTerm::iri(n.as_str()),
        NamedOrBlankNode::BlankNode(b) => RdfTerm::blank(b.as_str()),
    }
}

/// Convert a parsed object term to a kind-preserving [`RdfTerm`]. RDF 1.2
/// triple terms (quoted triples in object position) are flattened to their
/// N-Triples lexical string and stored as an IRI-ish token; full triple-term
/// support is a later increment (Stage-2), so we don't lose the data but also
/// don't model it structurally.
pub(crate) fn object_to_rdfterm(o: &OxTerm) -> RdfTerm {
    match o {
        OxTerm::NamedNode(n) => RdfTerm::iri(n.as_str()),
        OxTerm::BlankNode(b) => RdfTerm::blank(b.as_str()),
        OxTerm::Literal(l) => RdfTerm::literal(
            l.value(),
            Some(l.datatype().as_str().to_string()),
            l.language().map(|s| s.to_string()),
        ),
        OxTerm::Triple(t) => RdfTerm::iri(t.to_string()),
    }
}

/// Rebuild an oxrdf [`Quad`] (in the default graph) from a stored lexical
/// triple, so the serialiser can emit it. Falls back to treating an
/// unparseable subject/object as an IRI.
fn lexical_to_quad(t: &(String, String, String)) -> Result<Quad> {
    let subject = match RdfTerm::from_store_lexical(&t.0) {
        RdfTerm::Iri(i) => NamedOrBlankNode::NamedNode(
            NamedNode::new(&i).map_err(|e| GraphError::Io(e.to_string()))?,
        ),
        RdfTerm::Blank(b) => NamedOrBlankNode::BlankNode(
            BlankNode::new(&b).map_err(|e| GraphError::Io(e.to_string()))?,
        ),
        RdfTerm::Literal { .. } => {
            return Err(GraphError::Io("literal in subject position".into()))
        }
    };
    let predicate = NamedNode::new(t.1.trim_start_matches('<').trim_end_matches('>'))
        .map_err(|e| GraphError::Io(e.to_string()))?;
    let object = rdfterm_to_oxterm(&RdfTerm::from_store_lexical(&t.2))?;
    Ok(Quad::new(
        subject,
        predicate,
        object,
        GraphName::DefaultGraph,
    ))
}

pub(crate) fn rdfterm_to_oxterm(t: &RdfTerm) -> Result<OxTerm> {
    Ok(match t {
        RdfTerm::Iri(i) => {
            OxTerm::NamedNode(NamedNode::new(i).map_err(|e| GraphError::Io(e.to_string()))?)
        }
        RdfTerm::Blank(b) => {
            OxTerm::BlankNode(BlankNode::new(b).map_err(|e| GraphError::Io(e.to_string()))?)
        }
        RdfTerm::Literal {
            value,
            datatype,
            language,
        } => {
            let lit = match (datatype, language) {
                (_, Some(lang)) => oxrdf::Literal::new_language_tagged_literal(value, lang)
                    .map_err(|e| GraphError::Io(e.to_string()))?,
                (Some(dt), None) => oxrdf::Literal::new_typed_literal(
                    value,
                    NamedNode::new(dt).map_err(|e| GraphError::Io(e.to_string()))?,
                ),
                (None, None) => oxrdf::Literal::new_simple_literal(value),
            };
            OxTerm::Literal(lit)
        }
    })
}

// Bridge `RdfTerm` to the SPARQL algebra `Term` for the delete path. This lives
// here (not in `term.rs`) so `term.rs` stays free of the sparql dependency.
trait AlgTermFromKind {
    fn from_lexical_kind(t: &RdfTerm) -> Self;
}

impl AlgTermFromKind for horndb_sparql::algebra::Term {
    fn from_lexical_kind(t: &RdfTerm) -> Self {
        use horndb_sparql::algebra::Term as T;
        // The algebra `Term`'s inner string is the *stored lexical form*, so
        // the store's `delete_triple` keys match what `add` inserted.
        match t {
            RdfTerm::Iri(_) => T::Iri(t.to_store_lexical()),
            RdfTerm::Blank(_) => T::BlankNode(t.to_store_lexical()),
            RdfTerm::Literal { .. } => T::Literal(t.to_store_lexical()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iri(s: &str) -> RdfTerm {
        RdfTerm::iri(s)
    }

    #[test]
    fn add_len_contains() {
        let mut g = RdfGraph::new();
        g.add(
            &iri("http://ex/s"),
            &iri("http://ex/p"),
            &iri("http://ex/o"),
        )
        .unwrap();
        assert_eq!(g.len(), 1);
        assert!(g.contains(
            &iri("http://ex/s"),
            &iri("http://ex/p"),
            &iri("http://ex/o")
        ));
        assert!(!g.contains(
            &iri("http://ex/s"),
            &iri("http://ex/p"),
            &iri("http://ex/x")
        ));
    }

    #[test]
    fn add_is_idempotent() {
        let mut g = RdfGraph::new();
        let (s, p, o) = (iri("http://ex/s"), iri("http://ex/p"), iri("http://ex/o"));
        g.add(&s, &p, &o).unwrap();
        g.add(&s, &p, &o).unwrap();
        assert_eq!(g.len(), 1);
    }

    #[test]
    fn remove_triple() {
        let mut g = RdfGraph::new();
        let (s, p, o) = (iri("http://ex/s"), iri("http://ex/p"), iri("http://ex/o"));
        g.add(&s, &p, &o).unwrap();
        g.remove(&s, &p, &o);
        assert_eq!(g.len(), 0);
    }

    #[test]
    fn literal_subject_rejected() {
        let mut g = RdfGraph::new();
        let err = g.add(
            &RdfTerm::plain_literal("x"),
            &iri("http://ex/p"),
            &iri("http://ex/o"),
        );
        assert!(matches!(err, Err(GraphError::Term(_))));
    }

    #[test]
    fn triples_wildcard_and_filter() {
        let mut g = RdfGraph::new();
        g.add(
            &iri("http://ex/s1"),
            &iri("http://ex/p"),
            &iri("http://ex/o"),
        )
        .unwrap();
        g.add(
            &iri("http://ex/s2"),
            &iri("http://ex/p"),
            &iri("http://ex/o"),
        )
        .unwrap();
        assert_eq!(g.triples(None, None, None).len(), 2);
        let only_s1 = g.triples(Some(&iri("http://ex/s1")), None, None);
        assert_eq!(only_s1.len(), 1);
        assert_eq!(only_s1[0].0, iri("http://ex/s1"));
    }

    #[test]
    fn blank_node_round_trips_through_triples() {
        let mut g = RdfGraph::new();
        g.add(
            &RdfTerm::blank("b0"),
            &iri("http://ex/p"),
            &RdfTerm::plain_literal("v"),
        )
        .unwrap();
        let ts = g.triples(None, None, None);
        assert_eq!(ts.len(), 1);
        assert_eq!(ts[0].0, RdfTerm::blank("b0"));
        assert_eq!(ts[0].2, RdfTerm::plain_literal("v"));
    }

    #[test]
    fn parse_and_serialize_ntriples() {
        let mut g = RdfGraph::new();
        g.parse_str(
            "<http://ex/s> <http://ex/p> \"hi\" .\n",
            SerFormat::NTriples,
        )
        .unwrap();
        assert_eq!(g.len(), 1);
        let out = g.serialize_str(SerFormat::NTriples, &[]).unwrap();
        assert!(out.contains("<http://ex/s>"));
        assert!(out.contains("\"hi\""));
    }

    #[test]
    fn serialize_turtle_emits_bound_prefix() {
        let mut g = RdfGraph::new();
        g.add(
            &iri("http://ex/s"),
            &iri("http://ex/p"),
            &iri("http://ex/o"),
        )
        .unwrap();
        let prefixes = vec![("ex".to_string(), "http://ex/".to_string())];
        let out = g.serialize_str(SerFormat::Turtle, &prefixes).unwrap();
        assert!(out.contains("@prefix ex:"), "turtle output: {out}");
        // The QName form should appear, not the full IRI.
        assert!(out.contains("ex:s"), "turtle output: {out}");
    }

    #[test]
    fn parse_turtle_with_prefix() {
        let mut g = RdfGraph::new();
        g.parse_str(
            "@prefix ex: <http://ex/> .\nex:s ex:p ex:o .\n",
            SerFormat::Turtle,
        )
        .unwrap();
        assert!(g.contains(
            &iri("http://ex/s"),
            &iri("http://ex/p"),
            &iri("http://ex/o")
        ));
    }

    #[test]
    fn select_query() {
        let mut g = RdfGraph::new();
        g.add(
            &iri("http://ex/s"),
            &iri("http://ex/p"),
            &iri("http://ex/o"),
        )
        .unwrap();
        match g
            .query("SELECT ?s WHERE { ?s <http://ex/p> <http://ex/o> }")
            .unwrap()
        {
            QueryResult::Select { vars, solutions } => {
                assert_eq!(vars, vec!["s"]);
                assert_eq!(solutions.len(), 1);
                assert_eq!(solutions[0][0], Some(iri("http://ex/s")));
            }
            other => panic!("expected Select, got {other:?}"),
        }
    }

    #[test]
    fn ask_query() {
        let mut g = RdfGraph::new();
        g.add(
            &iri("http://ex/s"),
            &iri("http://ex/p"),
            &iri("http://ex/o"),
        )
        .unwrap();
        match g
            .query("ASK { <http://ex/s> <http://ex/p> <http://ex/o> }")
            .unwrap()
        {
            QueryResult::Ask(b) => assert!(b),
            other => panic!("expected Ask, got {other:?}"),
        }
    }

    #[test]
    fn update_insert_data() {
        let mut g = RdfGraph::new();
        g.update("INSERT DATA { <http://ex/s> <http://ex/p> <http://ex/o> . }")
            .unwrap();
        assert_eq!(g.len(), 1);
    }
}
