//! PyO3 surface: the native, pyoxigraph-shaped Python classes.
//!
//! These are the **primary** `horndb.*` names — a quad `Store` with named
//! graphs, value objects (`NamedNode`/`BlankNode`/`Literal`/`Triple`/`Quad`/
//! `DefaultGraph`/`Variable`), pyoxigraph-style `QuerySolutions`/`QuerySolution`
//! results, and `RdfFormat`. The rdflib-shaped facade (`URIRef`/`Graph`/…) lives
//! in [`crate::py`] and is exposed under the `horndb.rdflib` submodule.
//!
//! Heavy lifting is in the PyO3-free [`crate::quadstore`]; this module is the
//! thin Python adapter. The design intentionally mirrors pyoxigraph so existing
//! `pyoxigraph`-oriented code (e.g. `rdf-registry`'s build pipeline) ports by
//! changing only the import.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use pyo3::exceptions::{PyIndexError, PyKeyError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyString, PyTuple};

use crate::graph::{GraphError, QueryResult};
use crate::quadstore::{GraphName, IoFormat, QuadStore};
use crate::term::RdfTerm;

fn to_py_err(e: GraphError) -> PyErr {
    PyValueError::new_err(e.to_string())
}

fn hash_of(tag: &str, body: &str) -> u64 {
    let mut h = DefaultHasher::new();
    tag.hash(&mut h);
    body.hash(&mut h);
    h.finish()
}

// ---------------------------------------------------------------------------
// Term value objects
// ---------------------------------------------------------------------------

/// An IRI. pyoxigraph `NamedNode` — `.value` is the bare IRI, `str()` is the
/// `<iri>` N-Triples form.
#[pyclass(module = "horndb", frozen)]
#[derive(Clone)]
pub struct NamedNode {
    pub(crate) value: String,
}

#[pymethods]
impl NamedNode {
    #[new]
    fn new(value: &str) -> Self {
        NamedNode {
            value: value.to_string(),
        }
    }

    #[getter]
    fn value(&self) -> &str {
        &self.value
    }

    fn __str__(&self) -> String {
        format!("<{}>", self.value)
    }

    fn __repr__(&self) -> String {
        format!("<NamedNode value={}>", self.value)
    }

    fn __hash__(&self) -> u64 {
        hash_of("U", &self.value)
    }

    fn __eq__(&self, other: &Bound<'_, PyAny>) -> bool {
        other
            .extract::<NamedNode>()
            .map(|o| o.value == self.value)
            .unwrap_or(false)
    }

    fn __ne__(&self, other: &Bound<'_, PyAny>) -> bool {
        !self.__eq__(other)
    }
}

/// A blank node. pyoxigraph `BlankNode` — `.value` is the bare label, `str()` is
/// the `_:label` form. A missing label is auto-generated.
#[pyclass(module = "horndb", frozen)]
#[derive(Clone)]
pub struct BlankNode {
    pub(crate) value: String,
}

#[pymethods]
impl BlankNode {
    #[new]
    #[pyo3(signature = (value=None))]
    fn new(value: Option<&str>) -> Self {
        BlankNode {
            value: value
                .map(|v| v.to_string())
                .unwrap_or_else(fresh_bnode_label),
        }
    }

    #[getter]
    fn value(&self) -> &str {
        &self.value
    }

    fn __str__(&self) -> String {
        format!("_:{}", self.value)
    }

    fn __repr__(&self) -> String {
        format!("<BlankNode value={}>", self.value)
    }

    fn __hash__(&self) -> u64 {
        hash_of("B", &self.value)
    }

    fn __eq__(&self, other: &Bound<'_, PyAny>) -> bool {
        other
            .extract::<BlankNode>()
            .map(|o| o.value == self.value)
            .unwrap_or(false)
    }

    fn __ne__(&self, other: &Bound<'_, PyAny>) -> bool {
        !self.__eq__(other)
    }
}

/// An RDF literal. pyoxigraph `Literal` — `.value`, `.datatype` (a `NamedNode`),
/// and `.language`. `str()` is the N-Triples form.
#[pyclass(module = "horndb", frozen)]
#[derive(Clone)]
pub struct Literal {
    pub(crate) inner: RdfTerm,
}

#[pymethods]
impl Literal {
    #[new]
    #[pyo3(signature = (value, datatype=None, language=None))]
    fn new(
        value: &str,
        datatype: Option<&Bound<'_, PyAny>>,
        language: Option<&str>,
    ) -> PyResult<Self> {
        let dt = match datatype {
            Some(d) => Some(extract_iri(d)?),
            None => None,
        };
        Ok(Literal {
            inner: RdfTerm::literal(value, dt, language.map(|s| s.to_string())),
        })
    }

    #[getter]
    fn value(&self) -> String {
        match &self.inner {
            RdfTerm::Literal { value, .. } => value.clone(),
            _ => String::new(),
        }
    }

    #[getter]
    fn datatype(&self) -> NamedNode {
        // pyoxigraph always reports an effective datatype (xsd:string /
        // rdf:langString for the implicit cases), unlike rdflib's `None`.
        let dt = self
            .inner
            .effective_datatype()
            .unwrap_or("http://www.w3.org/2001/XMLSchema#string");
        NamedNode {
            value: dt.to_string(),
        }
    }

    #[getter]
    fn language(&self) -> Option<String> {
        match &self.inner {
            RdfTerm::Literal { language, .. } => language.clone(),
            _ => None,
        }
    }

    fn __str__(&self) -> String {
        self.inner.to_store_lexical()
    }

    fn __repr__(&self) -> String {
        format!("<Literal {}>", self.inner.to_store_lexical())
    }

    fn __hash__(&self) -> u64 {
        hash_of("L", &self.inner.to_store_lexical())
    }

    fn __eq__(&self, other: &Bound<'_, PyAny>) -> bool {
        other
            .extract::<Literal>()
            .map(|o| o.inner == self.inner)
            .unwrap_or(false)
    }

    fn __ne__(&self, other: &Bound<'_, PyAny>) -> bool {
        !self.__eq__(other)
    }
}

/// The unnamed default graph, used as a quad's graph name. pyoxigraph
/// `DefaultGraph`.
#[pyclass(module = "horndb", frozen)]
#[derive(Clone)]
pub struct DefaultGraph;

#[pymethods]
impl DefaultGraph {
    #[new]
    fn new() -> Self {
        DefaultGraph
    }

    fn __str__(&self) -> &str {
        "DEFAULT"
    }

    fn __repr__(&self) -> &str {
        "<DefaultGraph>"
    }

    fn __hash__(&self) -> u64 {
        hash_of("G", "default")
    }

    fn __eq__(&self, other: &Bound<'_, PyAny>) -> bool {
        other.extract::<DefaultGraph>().is_ok()
    }
}

/// A SPARQL variable. pyoxigraph `Variable` — `.value` is the bare name.
#[pyclass(module = "horndb", frozen)]
#[derive(Clone)]
pub struct Variable {
    pub(crate) value: String,
}

#[pymethods]
impl Variable {
    #[new]
    fn new(value: &str) -> Self {
        Variable {
            value: value.trim_start_matches(['?', '$']).to_string(),
        }
    }

    #[getter]
    fn value(&self) -> &str {
        &self.value
    }

    fn __str__(&self) -> String {
        format!("?{}", self.value)
    }

    fn __repr__(&self) -> String {
        format!("<Variable value={}>", self.value)
    }

    fn __hash__(&self) -> u64 {
        hash_of("V", &self.value)
    }

    fn __eq__(&self, other: &Bound<'_, PyAny>) -> bool {
        other
            .extract::<Variable>()
            .map(|o| o.value == self.value)
            .unwrap_or(false)
    }

    fn __ne__(&self, other: &Bound<'_, PyAny>) -> bool {
        !self.__eq__(other)
    }
}

/// An RDF triple `(subject, predicate, object)`. pyoxigraph `Triple`.
#[pyclass(module = "horndb", frozen)]
pub struct Triple {
    #[pyo3(get)]
    subject: Py<PyAny>,
    #[pyo3(get)]
    predicate: Py<PyAny>,
    #[pyo3(get)]
    object: Py<PyAny>,
}

#[pymethods]
impl Triple {
    #[new]
    fn new(subject: Py<PyAny>, predicate: Py<PyAny>, object: Py<PyAny>) -> Self {
        Triple {
            subject,
            predicate,
            object,
        }
    }

    fn __repr__(&self, py: Python<'_>) -> String {
        format!(
            "<Triple {} {} {}>",
            repr_of(py, &self.subject),
            repr_of(py, &self.predicate),
            repr_of(py, &self.object)
        )
    }
}

/// An RDF quad `(subject, predicate, object, graph_name)`. pyoxigraph `Quad`.
#[pyclass(module = "horndb", frozen)]
pub struct Quad {
    #[pyo3(get)]
    subject: Py<PyAny>,
    #[pyo3(get)]
    predicate: Py<PyAny>,
    #[pyo3(get)]
    object: Py<PyAny>,
    #[pyo3(get)]
    graph_name: Py<PyAny>,
}

#[pymethods]
impl Quad {
    #[new]
    #[pyo3(signature = (subject, predicate, object, graph_name=None))]
    fn new(
        py: Python<'_>,
        subject: Py<PyAny>,
        predicate: Py<PyAny>,
        object: Py<PyAny>,
        graph_name: Option<Py<PyAny>>,
    ) -> PyResult<Self> {
        let graph_name = match graph_name {
            Some(g) => g,
            None => Bound::new(py, DefaultGraph)?.into_any().unbind(),
        };
        Ok(Quad {
            subject,
            predicate,
            object,
            graph_name,
        })
    }

    fn __repr__(&self, py: Python<'_>) -> String {
        format!(
            "<Quad {} {} {} {}>",
            repr_of(py, &self.subject),
            repr_of(py, &self.predicate),
            repr_of(py, &self.object),
            repr_of(py, &self.graph_name)
        )
    }
}

// ---------------------------------------------------------------------------
// Formats
// ---------------------------------------------------------------------------

/// Supported RDF serialisation formats. pyoxigraph `RdfFormat` — use the class
/// constants (`RdfFormat.TURTLE`, `RdfFormat.N_QUADS`, …) or a format string.
#[pyclass(module = "horndb", frozen, eq)]
#[derive(Clone, PartialEq)]
pub struct RdfFormat {
    pub(crate) format: IoFormatWrap,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct IoFormatWrap(IoFormat);

#[pymethods]
impl RdfFormat {
    #[classattr]
    #[allow(non_snake_case)]
    fn TURTLE() -> RdfFormat {
        RdfFormat {
            format: IoFormatWrap(IoFormat::Turtle),
        }
    }
    #[classattr]
    #[allow(non_snake_case)]
    fn N_TRIPLES() -> RdfFormat {
        RdfFormat {
            format: IoFormatWrap(IoFormat::NTriples),
        }
    }
    #[classattr]
    #[allow(non_snake_case)]
    fn N_QUADS() -> RdfFormat {
        RdfFormat {
            format: IoFormatWrap(IoFormat::NQuads),
        }
    }
    #[classattr]
    #[allow(non_snake_case)]
    fn TRIG() -> RdfFormat {
        RdfFormat {
            format: IoFormatWrap(IoFormat::TriG),
        }
    }
    #[classattr]
    #[allow(non_snake_case)]
    fn RDF_XML() -> RdfFormat {
        RdfFormat {
            format: IoFormatWrap(IoFormat::RdfXml),
        }
    }

    fn __repr__(&self) -> String {
        format!("<RdfFormat {:?}>", self.format.0)
    }
}

fn resolve_format(obj: &Bound<'_, PyAny>) -> PyResult<IoFormat> {
    if let Ok(f) = obj.extract::<RdfFormat>() {
        return Ok(f.format.0);
    }
    if let Ok(s) = obj.extract::<String>() {
        return IoFormat::from_name(&s).map_err(to_py_err);
    }
    Err(PyValueError::new_err(
        "format must be an RdfFormat or a format string",
    ))
}

// ---------------------------------------------------------------------------
// Query results
// ---------------------------------------------------------------------------

/// The iterable result of a SELECT query — pyoxigraph `QuerySolutions`. Yields
/// `QuerySolution` rows; `.variables` lists the projected `Variable`s.
#[pyclass(module = "horndb")]
pub struct QuerySolutions {
    vars: Arc<Vec<String>>,
    rows: Vec<Vec<Option<RdfTerm>>>,
    pos: usize,
}

#[pymethods]
impl QuerySolutions {
    #[getter]
    fn variables(&self) -> Vec<Variable> {
        self.vars
            .iter()
            .map(|v| Variable { value: v.clone() })
            .collect()
    }

    fn __len__(&self) -> usize {
        self.rows.len()
    }

    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __next__(mut slf: PyRefMut<'_, Self>) -> Option<QuerySolution> {
        if slf.pos >= slf.rows.len() {
            return None;
        }
        let cells = slf.rows[slf.pos].clone();
        slf.pos += 1;
        Some(QuerySolution {
            vars: slf.vars.clone(),
            cells,
        })
    }
}

/// One SELECT solution — pyoxigraph `QuerySolution`. Index by variable name
/// (`sol["x"]`) or position (`sol[0]`); a missing/unbound variable is `None`.
#[pyclass(module = "horndb")]
#[derive(Clone)]
pub struct QuerySolution {
    vars: Arc<Vec<String>>,
    cells: Vec<Option<RdfTerm>>,
}

#[pymethods]
impl QuerySolution {
    fn __len__(&self) -> usize {
        self.cells.len()
    }

    #[getter]
    fn variables(&self) -> Vec<Variable> {
        self.vars
            .iter()
            .map(|v| Variable { value: v.clone() })
            .collect()
    }

    fn __getitem__(&self, py: Python<'_>, key: &Bound<'_, PyAny>) -> PyResult<Option<Py<PyAny>>> {
        let idx = if let Ok(i) = key.extract::<isize>() {
            let n = self.cells.len() as isize;
            let i = if i < 0 { i + n } else { i };
            if i < 0 || i >= n {
                return Err(PyIndexError::new_err("solution index out of range"));
            }
            i as usize
        } else if let Ok(name) = key.extract::<String>() {
            let name = name.trim_start_matches(['?', '$']);
            match self.vars.iter().position(|v| v == name) {
                Some(i) => i,
                None => return Err(PyKeyError::new_err(name.to_string())),
            }
        } else if let Ok(var) = key.extract::<Variable>() {
            match self.vars.iter().position(|v| v == &var.value) {
                Some(i) => i,
                None => return Err(PyKeyError::new_err(var.value)),
            }
        } else {
            return Err(PyValueError::new_err(
                "index must be an int, str, or Variable",
            ));
        };
        match &self.cells[idx] {
            Some(t) => Ok(Some(term_to_py(py, t)?)),
            None => Ok(None),
        }
    }

    /// `sol.get(variable, default=None)` — like dict access but never raises.
    #[pyo3(signature = (key, default=None))]
    fn get(
        &self,
        py: Python<'_>,
        key: &Bound<'_, PyAny>,
        default: Option<Py<PyAny>>,
    ) -> PyResult<Option<Py<PyAny>>> {
        match self.__getitem__(py, key) {
            Ok(Some(v)) => Ok(Some(v)),
            Ok(None) | Err(_) => Ok(default),
        }
    }

    fn __iter__(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let items: Vec<Py<PyAny>> = self
            .cells
            .iter()
            .map(|c| match c {
                Some(t) => term_to_py(py, t),
                None => Ok(py.None()),
            })
            .collect::<PyResult<_>>()?;
        Ok(items.into_pyobject(py)?.try_iter()?.into_any().unbind())
    }
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// A quad store with named graphs and OWL 2 RL materialization — the native
/// HornDB Python API, shaped after pyoxigraph `Store`.
#[pyclass(module = "horndb")]
pub struct Store {
    inner: Arc<Mutex<QuadStore>>,
}

#[pymethods]
impl Store {
    #[new]
    fn new() -> Self {
        Store {
            inner: Arc::new(Mutex::new(QuadStore::new())),
        }
    }

    /// Total number of quads across all graphs.
    fn __len__(&self) -> usize {
        self.inner.lock().unwrap().len()
    }

    /// `quad in store` — exact-quad membership.
    fn __contains__(&self, quad: &Bound<'_, PyAny>) -> PyResult<bool> {
        let (s, p, o, g) = extract_quad(quad)?;
        Ok(self.inner.lock().unwrap().contains(&s, &p, &o, &g))
    }

    /// `store.add(Quad(...))`.
    fn add(&self, quad: &Bound<'_, PyAny>) -> PyResult<()> {
        let (s, p, o, g) = extract_quad(quad)?;
        self.inner
            .lock()
            .unwrap()
            .add(&s, &p, &o, &g)
            .map_err(to_py_err)?;
        Ok(())
    }

    /// `store.remove(Quad(...))`.
    fn remove(&self, quad: &Bound<'_, PyAny>) -> PyResult<()> {
        let (s, p, o, g) = extract_quad(quad)?;
        self.inner.lock().unwrap().remove(&s, &p, &o, &g);
        Ok(())
    }

    /// `store.quads_for_pattern(subject, predicate, object, graph_name)` — every
    /// matching quad, with `None` as a wildcard in any position. `graph_name`
    /// defaults to all graphs; pass `DefaultGraph()` for the default graph only.
    #[pyo3(signature = (subject=None, predicate=None, object=None, graph_name=None))]
    fn quads_for_pattern(
        &self,
        py: Python<'_>,
        subject: Option<&Bound<'_, PyAny>>,
        predicate: Option<&Bound<'_, PyAny>>,
        object: Option<&Bound<'_, PyAny>>,
        graph_name: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Vec<Py<PyAny>>> {
        let s = opt_term(subject)?;
        let p = opt_term(predicate)?;
        let o = opt_term(object)?;
        let g = match graph_name {
            None => None,
            Some(g) if g.is_none() => None,
            Some(g) => Some(extract_graphname(g)?),
        };
        let rows = self.inner.lock().unwrap().quads_for_pattern(
            s.as_ref(),
            p.as_ref(),
            o.as_ref(),
            g.as_ref(),
        );
        rows.into_iter()
            .map(|(s, p, o, g)| quad_to_py(py, &s, &p, &o, &g))
            .collect()
    }

    /// Iterate every quad in the store.
    fn __iter__(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let all = self.quads_for_pattern(py, None, None, None, None)?;
        Ok(all.into_pyobject(py)?.try_iter()?.into_any().unbind())
    }

    /// The distinct named graphs (excluding the default graph), as `NamedNode`s.
    fn named_graphs(&self, py: Python<'_>) -> PyResult<Vec<Py<PyAny>>> {
        self.inner
            .lock()
            .unwrap()
            .named_graphs()
            .iter()
            .map(|g| graphname_to_py(py, g))
            .collect()
    }

    /// Run a SPARQL query. SELECT → `QuerySolutions`, ASK → `bool`,
    /// CONSTRUCT → list of `Triple`. `use_default_graph_as_union` queries the
    /// union of all graphs instead of just the default graph.
    #[pyo3(signature = (query, use_default_graph_as_union=false))]
    fn query(
        &self,
        py: Python<'_>,
        query: &str,
        use_default_graph_as_union: bool,
    ) -> PyResult<Py<PyAny>> {
        let res = self
            .inner
            .lock()
            .unwrap()
            .query(query, use_default_graph_as_union)
            .map_err(to_py_err)?;
        query_result_to_py(py, res)
    }

    /// Apply a SPARQL Update (default-graph scope in Stage 1).
    fn update(&self, query: &str) -> PyResult<()> {
        self.inner.lock().unwrap().update(query).map_err(to_py_err)
    }

    /// `store.load(data, format=..., to_graph=...)`. `data` is `str` or `bytes`;
    /// `format` is an `RdfFormat` or format string; `to_graph` (optional) forces
    /// every loaded triple into one graph (quad formats otherwise keep their
    /// own graph names).
    #[pyo3(signature = (data, format, to_graph=None))]
    fn load(
        &self,
        data: &Bound<'_, PyAny>,
        format: &Bound<'_, PyAny>,
        to_graph: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<usize> {
        let bytes = extract_bytes(data)?;
        let fmt = resolve_format(format)?;
        let g = match to_graph {
            None => None,
            Some(g) if g.is_none() => None,
            Some(g) => Some(extract_graphname(g)?),
        };
        self.inner
            .lock()
            .unwrap()
            .load(&bytes, fmt, g.as_ref())
            .map_err(to_py_err)
    }

    /// `store.serialize(format=..., from_graph=...)` → `str`. `from_graph`
    /// (optional) restricts output to one graph.
    #[pyo3(signature = (format, from_graph=None))]
    fn serialize(
        &self,
        format: &Bound<'_, PyAny>,
        from_graph: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<String> {
        let fmt = resolve_format(format)?;
        let g = match from_graph {
            None => None,
            Some(g) if g.is_none() => None,
            Some(g) => Some(extract_graphname(g)?),
        };
        self.inner
            .lock()
            .unwrap()
            .serialize(fmt, g.as_ref())
            .map_err(to_py_err)
    }

    /// Run OWL 2 RL forward chaining and add the entailed triples to the default
    /// graph. Returns `(asserted, inferred)`: the asserted triples the reasoner
    /// saw and the number of new triples materialized. Idempotent.
    fn materialize(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let (asserted, inferred) = self
            .inner
            .lock()
            .unwrap()
            .materialize()
            .map_err(to_py_err)?;
        let tup = PyTuple::new(py, [asserted, inferred])?;
        Ok(tup.into_any().unbind())
    }

    /// Drop only the materialized (inferred) triples, keeping the asserted base.
    fn clear_inferred(&self) {
        self.inner.lock().unwrap().clear_inferred();
    }

    /// Drop every quad in every graph.
    fn clear(&self) {
        self.inner.lock().unwrap().clear();
    }
}

// ---------------------------------------------------------------------------
// Conversions
// ---------------------------------------------------------------------------

fn term_to_py(py: Python<'_>, t: &RdfTerm) -> PyResult<Py<PyAny>> {
    Ok(match t {
        RdfTerm::Iri(i) => Bound::new(py, NamedNode { value: i.clone() })?
            .into_any()
            .unbind(),
        RdfTerm::Blank(b) => Bound::new(py, BlankNode { value: b.clone() })?
            .into_any()
            .unbind(),
        RdfTerm::Literal { .. } => Bound::new(py, Literal { inner: t.clone() })?
            .into_any()
            .unbind(),
    })
}

fn py_to_term(obj: &Bound<'_, PyAny>) -> PyResult<RdfTerm> {
    if let Ok(n) = obj.extract::<NamedNode>() {
        return Ok(RdfTerm::iri(n.value));
    }
    if let Ok(b) = obj.extract::<BlankNode>() {
        return Ok(RdfTerm::blank(b.value));
    }
    if let Ok(l) = obj.extract::<Literal>() {
        return Ok(l.inner);
    }
    if let Ok(s) = obj.extract::<String>() {
        return Ok(RdfTerm::iri(s));
    }
    Err(PyValueError::new_err(
        "expected a NamedNode, BlankNode, Literal, or str",
    ))
}

fn opt_term(obj: Option<&Bound<'_, PyAny>>) -> PyResult<Option<RdfTerm>> {
    match obj {
        None => Ok(None),
        Some(o) if o.is_none() => Ok(None),
        Some(o) => py_to_term(o).map(Some),
    }
}

fn extract_iri(obj: &Bound<'_, PyAny>) -> PyResult<String> {
    if let Ok(n) = obj.extract::<NamedNode>() {
        return Ok(n.value);
    }
    if let Ok(s) = obj.extract::<String>() {
        return Ok(s);
    }
    Err(PyValueError::new_err("expected a NamedNode or str"))
}

fn extract_graphname(obj: &Bound<'_, PyAny>) -> PyResult<GraphName> {
    if obj.extract::<DefaultGraph>().is_ok() {
        return Ok(GraphName::Default);
    }
    if let Ok(n) = obj.extract::<NamedNode>() {
        return Ok(GraphName::Named(n.value));
    }
    if let Ok(b) = obj.extract::<BlankNode>() {
        return Ok(GraphName::Blank(b.value));
    }
    if let Ok(s) = obj.extract::<String>() {
        return Ok(GraphName::Named(s));
    }
    Err(PyValueError::new_err(
        "graph name must be a NamedNode, BlankNode, DefaultGraph, or str",
    ))
}

#[allow(clippy::type_complexity)]
fn extract_quad(obj: &Bound<'_, PyAny>) -> PyResult<(RdfTerm, RdfTerm, RdfTerm, GraphName)> {
    // Accept a Quad object, or a 3-/4-tuple.
    if let Ok(q) = obj.downcast::<Quad>() {
        let py = obj.py();
        let q = q.borrow();
        let s = py_to_term(q.subject.bind(py))?;
        let p = py_to_term(q.predicate.bind(py))?;
        let o = py_to_term(q.object.bind(py))?;
        let g = extract_graphname(q.graph_name.bind(py))?;
        return Ok((s, p, o, g));
    }
    if let Ok(t) = obj.downcast::<PyTuple>() {
        let s = py_to_term(&t.get_item(0)?)?;
        let p = py_to_term(&t.get_item(1)?)?;
        let o = py_to_term(&t.get_item(2)?)?;
        let g = if t.len() >= 4 {
            extract_graphname(&t.get_item(3)?)?
        } else {
            GraphName::Default
        };
        return Ok((s, p, o, g));
    }
    Err(PyValueError::new_err(
        "expected a Quad or a (s, p, o[, g]) tuple",
    ))
}

fn graphname_to_py(py: Python<'_>, g: &GraphName) -> PyResult<Py<PyAny>> {
    Ok(match g {
        GraphName::Default => Bound::new(py, DefaultGraph)?.into_any().unbind(),
        GraphName::Named(iri) => Bound::new(py, NamedNode { value: iri.clone() })?
            .into_any()
            .unbind(),
        GraphName::Blank(label) => Bound::new(
            py,
            BlankNode {
                value: label.clone(),
            },
        )?
        .into_any()
        .unbind(),
    })
}

fn quad_to_py(
    py: Python<'_>,
    s: &RdfTerm,
    p: &RdfTerm,
    o: &RdfTerm,
    g: &GraphName,
) -> PyResult<Py<PyAny>> {
    let quad = Quad {
        subject: term_to_py(py, s)?,
        predicate: term_to_py(py, p)?,
        object: term_to_py(py, o)?,
        graph_name: graphname_to_py(py, g)?,
    };
    Ok(Bound::new(py, quad)?.into_any().unbind())
}

fn query_result_to_py(py: Python<'_>, res: QueryResult) -> PyResult<Py<PyAny>> {
    Ok(match res {
        QueryResult::Select { vars, solutions } => {
            let qs = QuerySolutions {
                vars: Arc::new(vars),
                rows: solutions,
                pos: 0,
            };
            Bound::new(py, qs)?.into_any().unbind()
        }
        QueryResult::Ask(b) => b.into_pyobject(py)?.to_owned().into_any().unbind(),
        QueryResult::Construct(triples) => {
            let mut out: Vec<Py<PyAny>> = Vec::with_capacity(triples.len());
            for (s, p, o) in triples {
                let t = Triple {
                    subject: term_to_py(py, &s)?,
                    predicate: term_to_py(py, &p)?,
                    object: term_to_py(py, &o)?,
                };
                out.push(Bound::new(py, t)?.into_any().unbind());
            }
            out.into_pyobject(py)?.into_any().unbind()
        }
        QueryResult::Explanation(text) => PyString::new(py, &text).into_any().unbind(),
    })
}

fn extract_bytes(obj: &Bound<'_, PyAny>) -> PyResult<Vec<u8>> {
    if let Ok(b) = obj.downcast::<PyBytes>() {
        return Ok(b.as_bytes().to_vec());
    }
    if let Ok(s) = obj.extract::<String>() {
        return Ok(s.into_bytes());
    }
    Err(PyValueError::new_err("data must be str or bytes"))
}

fn repr_of(py: Python<'_>, obj: &Py<PyAny>) -> String {
    obj.bind(py)
        .repr()
        .map(|r| r.to_string())
        .unwrap_or_else(|_| "?".to_string())
}

fn fresh_bnode_label() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("n{n:016x}")
}

/// Register the native pyoxigraph-shaped classes on the top-level `horndb`
/// module.
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<NamedNode>()?;
    m.add_class::<BlankNode>()?;
    m.add_class::<Literal>()?;
    m.add_class::<DefaultGraph>()?;
    m.add_class::<Variable>()?;
    m.add_class::<Triple>()?;
    m.add_class::<Quad>()?;
    m.add_class::<RdfFormat>()?;
    m.add_class::<QuerySolutions>()?;
    m.add_class::<QuerySolution>()?;
    m.add_class::<Store>()?;
    Ok(())
}
