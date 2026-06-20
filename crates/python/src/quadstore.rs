//! Pure-Rust `QuadStore`: the engine behind the pyoxigraph-shaped `Store`.
//!
//! Where [`crate::graph::RdfGraph`] is the rdflib `Graph` facade (a single
//! default graph of triples), `QuadStore` is the *native* HornDB Python spine:
//! a quad store with **named graphs**, pyoxigraph-style `quads_for_pattern`
//! iteration, multi-format `load`/`dump` (including the quad formats
//! N-Quads / TriG), SPARQL `query`/`update` passthrough, and — the HornDB
//! differentiator — an explicit OWL 2 RL [`materialize`](QuadStore::materialize)
//! step that saturates the store with entailed triples.
//!
//! Like `graph.rs` and `term.rs`, this module is **PyO3-free** so the whole
//! engine is exercised by `cargo test` without a Python interpreter. The thin
//! `#[pyclass] Store` adapter lives in [`crate::store_py`].
//!
//! ## Named graphs vs. the Stage-1 SPARQL engine
//!
//! The store models quads faithfully (default graph + named graphs), and the
//! non-SPARQL surface — `add`/`remove`/`quads_for_pattern`/`load`/`dump` — is
//! fully graph-aware. SPARQL evaluation, however, runs on the Stage-1
//! triple-only executor ([`MemStore`]), which has no named-graph scoping
//! (`GRAPH` patterns are flattened; see `crates/sparql/INTEGRATION-NOTES.md`).
//! `query` therefore picks the *active triple set* up front:
//!
//! * `use_default_graph_as_union = false` (default) → the default graph only;
//! * `use_default_graph_as_union = true` → the union of every graph.
//!
//! This is exactly the pyoxigraph `Store.query(..., use_default_graph_as_union=)`
//! knob, and it is what `rdf-registry`'s discovery query relies on. `GRAPH`-
//! scoped patterns *inside* a query body are not graph-isolated yet; that is a
//! documented Stage-1 divergence, tracked with the SPARQL named-graph work.

use std::collections::HashSet;
use std::io::Cursor;

use horndb_sparql::api::{execute_query, execute_update, QueryAnswer};
use horndb_sparql::exec::mem::MemStore;
use oxrdf::{
    BlankNode, GraphName as OxGraphName, GraphNameRef, NamedNode, NamedOrBlankNode, Quad as OxQuad,
};
use oxrdfio::{RdfFormat, RdfParser, RdfSerializer};

use crate::graph::{
    alg_term_to_rdfterm, object_to_rdfterm, rdfterm_to_oxterm, subject_to_rdfterm, GraphError,
    QueryResult,
};
use crate::term::RdfTerm;

type Result<T> = std::result::Result<T, GraphError>;

// ---------------------------------------------------------------------------
// Graph names
// ---------------------------------------------------------------------------

/// The graph a quad belongs to: the unnamed default graph, or a graph named by
/// an IRI or (less commonly) a blank node — mirroring `oxrdf::GraphName` and
/// pyoxigraph's `DefaultGraph` / `NamedNode` / `BlankNode` graph names.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum GraphName {
    Default,
    Named(String),
    Blank(String),
}

impl GraphName {
    /// A stable key that disambiguates the default graph from a same-named
    /// IRI/blank graph in the dedup set.
    fn key(&self) -> String {
        match self {
            GraphName::Default => "\0d".to_string(),
            GraphName::Named(iri) => format!("U{iri}"),
            GraphName::Blank(label) => format!("B{label}"),
        }
    }

    fn to_ox(&self) -> Result<OxGraphName> {
        Ok(match self {
            GraphName::Default => OxGraphName::DefaultGraph,
            GraphName::Named(iri) => OxGraphName::NamedNode(
                NamedNode::new(iri).map_err(|e| GraphError::Io(e.to_string()))?,
            ),
            GraphName::Blank(label) => OxGraphName::BlankNode(
                BlankNode::new(label).map_err(|e| GraphError::Io(e.to_string()))?,
            ),
        })
    }

    fn from_ox(g: &GraphNameRef<'_>) -> GraphName {
        match g {
            GraphNameRef::DefaultGraph => GraphName::Default,
            GraphNameRef::NamedNode(n) => GraphName::Named(n.as_str().to_string()),
            GraphNameRef::BlankNode(b) => GraphName::Blank(b.as_str().to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// Serialisation formats
// ---------------------------------------------------------------------------

/// The RDF formats the native `Store` can `load`/`dump`. Unlike the rdflib
/// facade's `SerFormat` (Turtle + N-Triples only), this set includes the quad
/// formats so named graphs survive a round-trip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoFormat {
    Turtle,
    NTriples,
    NQuads,
    TriG,
    RdfXml,
}

impl IoFormat {
    /// Resolve a pyoxigraph/rdflib-style `format=` string or media type.
    pub fn from_name(name: &str) -> Result<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "turtle" | "ttl" | "text/turtle" => Ok(IoFormat::Turtle),
            "ntriples" | "nt" | "n-triples" | "application/n-triples" => Ok(IoFormat::NTriples),
            "nquads" | "nq" | "n-quads" | "application/n-quads" => Ok(IoFormat::NQuads),
            "trig" | "application/trig" => Ok(IoFormat::TriG),
            "rdfxml" | "xml" | "rdf/xml" | "application/rdf+xml" => Ok(IoFormat::RdfXml),
            other => Err(GraphError::Io(format!(
                "unsupported format {other:?}; supported: turtle, ntriples, nquads, trig, rdfxml"
            ))),
        }
    }

    fn rdf_format(self) -> RdfFormat {
        match self {
            IoFormat::Turtle => RdfFormat::Turtle,
            IoFormat::NTriples => RdfFormat::NTriples,
            IoFormat::NQuads => RdfFormat::NQuads,
            IoFormat::TriG => RdfFormat::TriG,
            IoFormat::RdfXml => RdfFormat::RdfXml,
        }
    }

    /// Whether the format can carry graph names (N-Quads, TriG). Triple-only
    /// formats serialise every selected quad as if in the default graph.
    fn supports_graphs(self) -> bool {
        matches!(self, IoFormat::NQuads | IoFormat::TriG)
    }
}

// ---------------------------------------------------------------------------
// The store
// ---------------------------------------------------------------------------

/// One stored quad, kind-preserving in subject/predicate/object and tagged with
/// its graph. `inferred` marks triples produced by [`QuadStore::materialize`]
/// so they can be dropped and re-derived without touching asserted data.
#[derive(Debug, Clone)]
struct Entry {
    s: RdfTerm,
    p: RdfTerm,
    o: RdfTerm,
    g: GraphName,
    inferred: bool,
}

impl Entry {
    fn key(&self) -> (String, String, String, String) {
        (
            self.s.to_store_lexical(),
            self.p.to_store_lexical(),
            self.o.to_store_lexical(),
            self.g.key(),
        )
    }
}

/// A quad store with named-graph support — the engine behind the native
/// pyoxigraph-shaped `Store`.
#[derive(Default)]
pub struct QuadStore {
    entries: Vec<Entry>,
    /// Dedup index over `(s, p, o, graph)` lexical keys.
    index: HashSet<(String, String, String, String)>,
}

impl QuadStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Total number of quads across all graphs (pyoxigraph `len(store)`).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn insert_entry(&mut self, e: Entry) -> bool {
        let key = e.key();
        if self.index.insert(key) {
            self.entries.push(e);
            true
        } else {
            false
        }
    }

    /// Add a quad (rdflib/pyoxigraph `add`). Subject must be an IRI/blank node
    /// and predicate an IRI, matching RDF; returns whether it was new.
    pub fn add(&mut self, s: &RdfTerm, p: &RdfTerm, o: &RdfTerm, g: &GraphName) -> Result<bool> {
        check_subject(s)?;
        check_predicate(p)?;
        Ok(self.insert_entry(Entry {
            s: s.clone(),
            p: p.clone(),
            o: o.clone(),
            g: g.clone(),
            inferred: false,
        }))
    }

    /// Remove one exact quad (no-op if absent); returns whether it was present.
    pub fn remove(&mut self, s: &RdfTerm, p: &RdfTerm, o: &RdfTerm, g: &GraphName) -> bool {
        let key = (
            s.to_store_lexical(),
            p.to_store_lexical(),
            o.to_store_lexical(),
            g.key(),
        );
        if self.index.remove(&key) {
            self.entries.retain(|e| e.key() != key);
            true
        } else {
            false
        }
    }

    /// Whether the exact quad is present.
    pub fn contains(&self, s: &RdfTerm, p: &RdfTerm, o: &RdfTerm, g: &GraphName) -> bool {
        let key = (
            s.to_store_lexical(),
            p.to_store_lexical(),
            o.to_store_lexical(),
            g.key(),
        );
        self.index.contains(&key)
    }

    /// pyoxigraph `quads_for_pattern(subject, predicate, object, graph_name)`:
    /// every quad matching the given positions, where `None` is a wildcard. A
    /// `None` `graph` matches every graph; `Some(g)` restricts to that graph
    /// (use `Some(GraphName::Default)` for the default graph alone).
    #[allow(clippy::type_complexity)]
    pub fn quads_for_pattern(
        &self,
        s: Option<&RdfTerm>,
        p: Option<&RdfTerm>,
        o: Option<&RdfTerm>,
        g: Option<&GraphName>,
    ) -> Vec<(RdfTerm, RdfTerm, RdfTerm, GraphName)> {
        self.entries
            .iter()
            .filter(|e| {
                s.map(|v| v == &e.s).unwrap_or(true)
                    && p.map(|v| v == &e.p).unwrap_or(true)
                    && o.map(|v| v == &e.o).unwrap_or(true)
                    && g.map(|v| v == &e.g).unwrap_or(true)
            })
            .map(|e| (e.s.clone(), e.p.clone(), e.o.clone(), e.g.clone()))
            .collect()
    }

    /// The distinct named graphs (excluding the default graph).
    pub fn named_graphs(&self) -> Vec<GraphName> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for e in &self.entries {
            if e.g != GraphName::Default && seen.insert(e.g.key()) {
                out.push(e.g.clone());
            }
        }
        out
    }

    /// Drop every quad in a graph; returns how many were removed.
    pub fn clear_graph(&mut self, g: &GraphName) -> usize {
        let before = self.entries.len();
        let gk = g.key();
        self.entries.retain(|e| {
            let keep = e.g.key() != gk;
            if !keep {
                self.index.remove(&e.key());
            }
            keep
        });
        before - self.entries.len()
    }

    /// Drop every quad in every graph.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.index.clear();
    }

    /// Drop only materialized (inferred) triples, keeping the asserted base —
    /// the re-derivation path before a fresh [`materialize`](Self::materialize).
    pub fn clear_inferred(&mut self) {
        self.entries.retain(|e| {
            let drop = e.inferred;
            if drop {
                self.index.remove(&e.key());
            }
            !drop
        });
    }

    /// Parse RDF into the store. For the triple formats, `to_graph` (if given)
    /// chooses the target graph (default: the default graph). For the quad
    /// formats (N-Quads / TriG) the parsed graph names are honoured unless
    /// `to_graph` overrides them — matching pyoxigraph's `load(..., to_graph=)`.
    /// Returns the number of newly added quads.
    pub fn load(
        &mut self,
        data: &[u8],
        format: IoFormat,
        to_graph: Option<&GraphName>,
    ) -> Result<usize> {
        let parser = RdfParser::from_format(format.rdf_format());
        let mut added = 0usize;
        for quad in parser.for_reader(Cursor::new(data.to_vec())) {
            let quad = quad.map_err(|e| GraphError::Io(e.to_string()))?;
            let s = subject_to_rdfterm(&quad.subject);
            let p = RdfTerm::iri(quad.predicate.as_str());
            let o = object_to_rdfterm(&quad.object);
            let g = match to_graph {
                Some(g) => g.clone(),
                None => GraphName::from_ox(&quad.graph_name.as_ref()),
            };
            if self.insert_entry(Entry {
                s,
                p,
                o,
                g,
                inferred: false,
            }) {
                added += 1;
            }
        }
        Ok(added)
    }

    /// Serialise the store (pyoxigraph `dump` / rdflib `serialize`). `from_graph`
    /// restricts output to one graph; `None` serialises every graph. Quad
    /// formats carry graph names; triple formats flatten every selected quad
    /// into the default graph.
    pub fn serialize(&self, format: IoFormat, from_graph: Option<&GraphName>) -> Result<String> {
        // Triple formats reject non-default graph names, so the graph is
        // forced to default per-quad below; quad formats keep the real graph.
        let ser = RdfSerializer::from_format(format.rdf_format());
        let mut buf = Vec::new();
        let mut writer = ser.for_writer(&mut buf);
        for e in &self.entries {
            if let Some(g) = from_graph {
                if &e.g != g {
                    continue;
                }
            }
            let subject = rdfterm_to_subject(&e.s)?;
            let predicate = rdfterm_to_named(&e.p)?;
            let object = rdfterm_to_oxterm(&e.o)?;
            let graph = if format.supports_graphs() {
                e.g.to_ox()?
            } else {
                OxGraphName::DefaultGraph
            };
            let quad = OxQuad::new(subject, predicate, object, graph);
            writer
                .serialize_quad(quad.as_ref())
                .map_err(|e| GraphError::Io(e.to_string()))?;
        }
        writer.finish().map_err(|e| GraphError::Io(e.to_string()))?;
        String::from_utf8(buf).map_err(|e| GraphError::Io(e.to_string()))
    }

    /// Build the triple set a SPARQL query should see: the default graph only,
    /// or the union of every graph when `union` is set (pyoxigraph's
    /// `use_default_graph_as_union`).
    fn query_store(&self, union: bool) -> MemStore {
        let mut ms = MemStore::default();
        for e in &self.entries {
            if union || e.g == GraphName::Default {
                ms.insert((
                    e.s.to_store_lexical(),
                    e.p.to_store_lexical(),
                    e.o.to_store_lexical(),
                ));
            }
        }
        ms
    }

    /// Run a SPARQL query (SELECT/ASK/CONSTRUCT). `use_default_graph_as_union`
    /// chooses whether the active dataset is the default graph or the union of
    /// all graphs (see the module docs).
    pub fn query(&self, sparql: &str, use_default_graph_as_union: bool) -> Result<QueryResult> {
        let store = self.query_store(use_default_graph_as_union);
        let answer =
            execute_query(sparql, &store).map_err(|e| GraphError::Sparql(e.to_string()))?;
        Ok(answer_to_result(answer))
    }

    /// Apply a SPARQL Update. Stage-1 updates operate on the **default graph**
    /// (the engine is default-graph only); named graphs are left untouched and
    /// inferred triples in the default graph are dropped (re-materialize after).
    pub fn update(&mut self, sparql: &str) -> Result<()> {
        let mut ms = self.default_graph_memstore();
        execute_update(sparql, &mut ms).map_err(|e| GraphError::Sparql(e.to_string()))?;
        // Rebuild the default graph from the updated MemStore.
        self.clear_graph(&GraphName::Default);
        for t in ms.iter_triples() {
            let s = RdfTerm::from_store_lexical(&t.0);
            let p = RdfTerm::from_store_lexical(&t.1);
            let o = RdfTerm::from_store_lexical(&t.2);
            self.insert_entry(Entry {
                s,
                p,
                o,
                g: GraphName::Default,
                inferred: false,
            });
        }
        Ok(())
    }

    fn default_graph_memstore(&self) -> MemStore {
        let mut ms = MemStore::default();
        for e in &self.entries {
            if e.g == GraphName::Default {
                ms.insert((
                    e.s.to_store_lexical(),
                    e.p.to_store_lexical(),
                    e.o.to_store_lexical(),
                ));
            }
        }
        ms
    }

    /// Run OWL 2 RL forward chaining over the asserted base and add the entailed
    /// triples to the default graph as `inferred`. Reasoning is over the merged
    /// graph (all asserted quads, default + named), matching the SPEC-04 engine.
    ///
    /// Returns `(asserted, inferred_added)`: the asserted triple count the
    /// reasoner saw, and how many genuinely new triples were materialized.
    /// Idempotent: existing inferred triples are cleared and re-derived.
    pub fn materialize(&mut self) -> Result<(usize, usize)> {
        use oxrdf::Dataset;

        self.clear_inferred();

        // Feed every asserted quad to the reasoner in one merged graph.
        let mut dataset = Dataset::default();
        let mut asserted_keys: HashSet<(String, String, String)> = HashSet::new();
        for e in &self.entries {
            let subject = rdfterm_to_subject(&e.s)?;
            let predicate = rdfterm_to_named(&e.p)?;
            let object = rdfterm_to_oxterm(&e.o)?;
            dataset.insert(&OxQuad::new(
                subject,
                predicate,
                object,
                OxGraphName::DefaultGraph,
            ));
            asserted_keys.insert((
                e.s.to_store_lexical(),
                e.p.to_store_lexical(),
                e.o.to_store_lexical(),
            ));
        }

        let mut engine = horndb_owlrl::integration::Engine::new();
        engine
            .load(&dataset)
            .map_err(|e| GraphError::Sparql(format!("owlrl load: {e}")))?;
        let asserted = engine.asserted_len().unwrap_or(asserted_keys.len());
        let closure = engine
            .materialized_triples()
            .ok_or_else(|| GraphError::Sparql("owlrl produced no materialized state".into()))?;

        // Everything in the closure that was not asserted is a new inference.
        let mut added = 0usize;
        for (s, p, o) in closure {
            if asserted_keys.contains(&(s.clone(), p.clone(), o.clone())) {
                continue;
            }
            let entry = Entry {
                s: RdfTerm::from_store_lexical(&s),
                p: RdfTerm::from_store_lexical(&p),
                o: RdfTerm::from_store_lexical(&o),
                g: GraphName::Default,
                inferred: true,
            };
            if self.insert_entry(entry) {
                added += 1;
            }
        }
        Ok((asserted, added))
    }
}

// ---------------------------------------------------------------------------
// Conversions & validation
// ---------------------------------------------------------------------------

fn answer_to_result(answer: QueryAnswer) -> QueryResult {
    match answer {
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
    }
}

fn rdfterm_to_subject(t: &RdfTerm) -> Result<NamedOrBlankNode> {
    match t {
        RdfTerm::Iri(i) => Ok(NamedOrBlankNode::NamedNode(
            NamedNode::new(i).map_err(|e| GraphError::Io(e.to_string()))?,
        )),
        RdfTerm::Blank(b) => Ok(NamedOrBlankNode::BlankNode(
            BlankNode::new(b).map_err(|e| GraphError::Io(e.to_string()))?,
        )),
        RdfTerm::Literal { .. } => Err(GraphError::Term("a literal cannot be a subject".into())),
    }
}

fn rdfterm_to_named(t: &RdfTerm) -> Result<NamedNode> {
    match t {
        RdfTerm::Iri(i) => NamedNode::new(i).map_err(|e| GraphError::Io(e.to_string())),
        _ => Err(GraphError::Term("a predicate must be an IRI".into())),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn iri(s: &str) -> RdfTerm {
        RdfTerm::iri(s)
    }
    fn named(s: &str) -> GraphName {
        GraphName::Named(s.to_string())
    }

    #[test]
    fn add_len_and_named_graphs() {
        let mut s = QuadStore::new();
        assert!(s
            .add(
                &iri("http://ex/s"),
                &iri("http://ex/p"),
                &iri("http://ex/o"),
                &GraphName::Default
            )
            .unwrap());
        assert!(s
            .add(
                &iri("http://ex/s"),
                &iri("http://ex/p"),
                &iri("http://ex/o"),
                &named("http://g/1")
            )
            .unwrap());
        // Same triple in two graphs = two quads.
        assert_eq!(s.len(), 2);
        assert_eq!(s.named_graphs(), vec![named("http://g/1")]);
    }

    #[test]
    fn add_is_idempotent_per_graph() {
        let mut s = QuadStore::new();
        let (a, b, c) = (iri("http://ex/s"), iri("http://ex/p"), iri("http://ex/o"));
        assert!(s.add(&a, &b, &c, &GraphName::Default).unwrap());
        assert!(!s.add(&a, &b, &c, &GraphName::Default).unwrap());
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn quads_for_pattern_filters_by_graph() {
        let mut s = QuadStore::new();
        s.add(
            &iri("http://ex/s1"),
            &iri("http://ex/p"),
            &iri("http://ex/o"),
            &GraphName::Default,
        )
        .unwrap();
        s.add(
            &iri("http://ex/s2"),
            &iri("http://ex/p"),
            &iri("http://ex/o"),
            &named("http://g/1"),
        )
        .unwrap();
        // All graphs.
        assert_eq!(s.quads_for_pattern(None, None, None, None).len(), 2);
        // Default graph only.
        let d = s.quads_for_pattern(None, None, None, Some(&GraphName::Default));
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].0, iri("http://ex/s1"));
        // One named graph.
        let g = s.quads_for_pattern(None, None, None, Some(&named("http://g/1")));
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].0, iri("http://ex/s2"));
        // Subject filter across graphs.
        assert_eq!(
            s.quads_for_pattern(Some(&iri("http://ex/s2")), None, None, None)
                .len(),
            1
        );
    }

    #[test]
    fn remove_quad_exact() {
        let mut s = QuadStore::new();
        let (a, b, c) = (iri("http://ex/s"), iri("http://ex/p"), iri("http://ex/o"));
        s.add(&a, &b, &c, &named("http://g/1")).unwrap();
        assert!(!s.remove(&a, &b, &c, &GraphName::Default)); // wrong graph
        assert!(s.remove(&a, &b, &c, &named("http://g/1")));
        assert_eq!(s.len(), 0);
    }

    #[test]
    fn load_nquads_into_named_graphs() {
        let mut s = QuadStore::new();
        let data = concat!(
            "<http://ex/s> <http://ex/p> <http://ex/o> <http://g/1> .\n",
            "<http://ex/s2> <http://ex/p> \"v\" .\n"
        );
        let n = s.load(data.as_bytes(), IoFormat::NQuads, None).unwrap();
        assert_eq!(n, 2);
        assert_eq!(s.named_graphs(), vec![named("http://g/1")]);
        // The triple with no graph went to the default graph.
        assert_eq!(
            s.quads_for_pattern(None, None, None, Some(&GraphName::Default))
                .len(),
            1
        );
    }

    #[test]
    fn load_turtle_to_named_graph_override() {
        let mut s = QuadStore::new();
        let data = "<http://ex/s> <http://ex/p> <http://ex/o> .\n";
        s.load(
            data.as_bytes(),
            IoFormat::NTriples,
            Some(&named("http://g/2")),
        )
        .unwrap();
        assert_eq!(
            s.quads_for_pattern(None, None, None, Some(&named("http://g/2")))
                .len(),
            1
        );
    }

    #[test]
    fn serialize_nquads_round_trips_graph() {
        let mut s = QuadStore::new();
        s.add(
            &iri("http://ex/s"),
            &iri("http://ex/p"),
            &iri("http://ex/o"),
            &named("http://g/1"),
        )
        .unwrap();
        let out = s.serialize(IoFormat::NQuads, None).unwrap();
        assert!(out.contains("<http://g/1>"), "nquads: {out}");
        let mut s2 = QuadStore::new();
        s2.load(out.as_bytes(), IoFormat::NQuads, None).unwrap();
        assert_eq!(s2.named_graphs(), vec![named("http://g/1")]);
    }

    #[test]
    fn query_default_vs_union() {
        let mut s = QuadStore::new();
        // One match in the default graph, one only in a named graph.
        s.add(
            &iri("http://ex/a"),
            &iri("http://ex/p"),
            &iri("http://ex/o"),
            &GraphName::Default,
        )
        .unwrap();
        s.add(
            &iri("http://ex/b"),
            &iri("http://ex/p"),
            &iri("http://ex/o"),
            &named("http://g/1"),
        )
        .unwrap();
        let q = "SELECT ?s WHERE { ?s <http://ex/p> <http://ex/o> }";
        // Default graph only → 1 row.
        match s.query(q, false).unwrap() {
            QueryResult::Select { solutions, .. } => assert_eq!(solutions.len(), 1),
            other => panic!("expected select, got {other:?}"),
        }
        // Union of graphs → 2 rows.
        match s.query(q, true).unwrap() {
            QueryResult::Select { solutions, .. } => assert_eq!(solutions.len(), 2),
            other => panic!("expected select, got {other:?}"),
        }
    }

    #[test]
    fn discover_style_union_query() {
        // Mirrors rdf-registry's discover.rq shape: one named graph per file,
        // queried as the union default graph.
        let mut s = QuadStore::new();
        let rdf_type = iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#type");
        let scheme = iri("http://www.w3.org/2004/02/skos/core#ConceptScheme");
        s.add(
            &iri("https://ex/foo"),
            &rdf_type,
            &scheme,
            &named("file:foo.ttl"),
        )
        .unwrap();
        let q = "PREFIX skos: <http://www.w3.org/2004/02/skos/core#> \
                 SELECT ?uri WHERE { ?uri a skos:ConceptScheme }";
        match s.query(q, true).unwrap() {
            QueryResult::Select { solutions, .. } => {
                assert_eq!(solutions.len(), 1);
                assert_eq!(solutions[0][0], Some(iri("https://ex/foo")));
            }
            other => panic!("expected select, got {other:?}"),
        }
    }

    #[test]
    fn update_default_graph() {
        let mut s = QuadStore::new();
        s.update("INSERT DATA { <http://ex/s> <http://ex/p> <http://ex/o> . }")
            .unwrap();
        assert_eq!(
            s.quads_for_pattern(None, None, None, Some(&GraphName::Default))
                .len(),
            1
        );
    }

    #[test]
    fn materialize_owl_rl_subclass() {
        // :Penguin rdfs:subClassOf :Bird ; :pingu a :Penguin  ⇒  :pingu a :Bird
        let mut s = QuadStore::new();
        let sco = iri("http://www.w3.org/2000/01/rdf-schema#subClassOf");
        let ty = iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#type");
        s.add(
            &iri("http://ex/Penguin"),
            &sco,
            &iri("http://ex/Bird"),
            &GraphName::Default,
        )
        .unwrap();
        s.add(
            &iri("http://ex/pingu"),
            &ty,
            &iri("http://ex/Penguin"),
            &GraphName::Default,
        )
        .unwrap();
        let (_asserted, added) = s.materialize().unwrap();
        assert!(added >= 1, "expected at least one inferred triple");
        // The inferred type triple is now queryable.
        assert!(s.contains(
            &iri("http://ex/pingu"),
            &ty,
            &iri("http://ex/Bird"),
            &GraphName::Default
        ));
        // Re-materialize is idempotent (clears + re-derives, same count).
        let (_a2, added2) = s.materialize().unwrap();
        assert_eq!(added, added2);
    }

    #[test]
    fn clear_inferred_keeps_asserted() {
        let mut s = QuadStore::new();
        let sco = iri("http://www.w3.org/2000/01/rdf-schema#subClassOf");
        let ty = iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#type");
        s.add(
            &iri("http://ex/Penguin"),
            &sco,
            &iri("http://ex/Bird"),
            &GraphName::Default,
        )
        .unwrap();
        s.add(
            &iri("http://ex/pingu"),
            &ty,
            &iri("http://ex/Penguin"),
            &GraphName::Default,
        )
        .unwrap();
        let asserted_before = s.len();
        s.materialize().unwrap();
        assert!(s.len() > asserted_before);
        s.clear_inferred();
        assert_eq!(s.len(), asserted_before);
    }
}
