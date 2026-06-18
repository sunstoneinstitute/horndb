//! PyO3 surface: the rdflib-shaped Python classes.
//!
//! Term classes (`URIRef`, `BNode`, `Literal`, `Variable`, `Namespace`) and
//! the `Graph` facade. The heavy lifting lives in [`crate::graph`] and
//! [`crate::term`]; this module is the thin Python-object adapter and is the
//! only part that needs a Python interpreter to run.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use pyo3::exceptions::{PyKeyError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyString, PyTuple};

use crate::graph::{GraphError, QueryResult, RdfGraph, SerFormat};
use crate::term::{RdfTerm, XSD_STRING};

/// Map a [`GraphError`] to a Python exception. SPEC-10 F7 asks for rdflib-like
/// errors; rdflib raises `ValueError`/`KeyError`/`Exception` subclasses, so we
/// route to the closest builtin and keep the message informative.
fn to_py_err(e: GraphError) -> PyErr {
    match e {
        GraphError::Term(m) => PyValueError::new_err(m),
        GraphError::Io(m) | GraphError::Sparql(m) => PyValueError::new_err(m),
    }
}

// ---------------------------------------------------------------------------
// Term classes
// ---------------------------------------------------------------------------

/// `rdflib.URIRef` — an IRI. Subclasses `str` in rdflib; here it wraps the IRI
/// string and reproduces the equality/hash/str behaviour the compat suite
/// relies on (F1).
#[pyclass(module = "horndb_rdflib", frozen)]
#[derive(Clone)]
pub struct URIRef {
    pub(crate) iri: String,
}

#[pymethods]
impl URIRef {
    #[new]
    fn new(value: &str) -> Self {
        URIRef {
            iri: value.to_string(),
        }
    }

    fn __str__(&self) -> &str {
        &self.iri
    }

    fn __repr__(&self) -> String {
        format!("rdflib.term.URIRef({:?})", self.iri)
    }

    /// `URIRef('a') + 'b' == URIRef('ab')`, matching rdflib's str-concat.
    fn __add__(&self, other: &str) -> URIRef {
        URIRef {
            iri: format!("{}{}", self.iri, other),
        }
    }

    fn __hash__(&self) -> u64 {
        kind_hash("U", &self.iri)
    }

    fn __eq__(&self, other: &Bound<'_, PyAny>) -> bool {
        other
            .extract::<URIRef>()
            .map(|o| o.iri == self.iri)
            .unwrap_or(false)
    }

    fn __ne__(&self, other: &Bound<'_, PyAny>) -> bool {
        !self.__eq__(other)
    }

    /// rdflib's `n3()` serialisation — an IRI wrapped in angle brackets, e.g.
    /// `<http://ex/s>`. This is the SPARQL/Turtle syntax form, NOT the bare
    /// store-lexical form (`to_store_lexical()`), so code that splices `n3()`
    /// into query/Turtle strings stays valid.
    fn n3(&self) -> String {
        format!("<{}>", self.iri)
    }
}

/// `rdflib.BNode` — a blank node identified by a label.
#[pyclass(module = "horndb_rdflib", frozen)]
#[derive(Clone)]
pub struct BNode {
    pub(crate) label: String,
}

#[pymethods]
impl BNode {
    #[new]
    #[pyo3(signature = (value=None))]
    fn new(value: Option<&str>) -> Self {
        BNode {
            label: value
                .map(|v| v.to_string())
                .unwrap_or_else(fresh_bnode_label),
        }
    }

    fn __str__(&self) -> &str {
        &self.label
    }

    fn __repr__(&self) -> String {
        format!("rdflib.term.BNode({:?})", self.label)
    }

    fn __hash__(&self) -> u64 {
        kind_hash("B", &self.label)
    }

    fn __eq__(&self, other: &Bound<'_, PyAny>) -> bool {
        other
            .extract::<BNode>()
            .map(|o| o.label == self.label)
            .unwrap_or(false)
    }

    fn __ne__(&self, other: &Bound<'_, PyAny>) -> bool {
        !self.__eq__(other)
    }

    fn n3(&self) -> String {
        format!("_:{}", self.label)
    }
}

/// `rdflib.Literal` — a literal with an optional datatype and language tag.
#[pyclass(module = "horndb_rdflib", frozen)]
#[derive(Clone)]
pub struct Literal {
    pub(crate) inner: RdfTerm,
}

#[pymethods]
impl Literal {
    #[new]
    #[pyo3(signature = (value, lang=None, datatype=None))]
    fn new(
        value: &Bound<'_, PyAny>,
        lang: Option<&str>,
        datatype: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Self> {
        let v: String = value.str()?.to_string_lossy().into_owned();
        let dt = match datatype {
            Some(d) => Some(extract_iri_string(d)?),
            None => None,
        };
        Ok(Literal {
            inner: RdfTerm::literal(v, dt, lang.map(|s| s.to_string())),
        })
    }

    fn __str__(&self) -> String {
        match &self.inner {
            RdfTerm::Literal { value, .. } => value.clone(),
            _ => unreachable!("Literal always wraps RdfTerm::Literal"),
        }
    }

    fn __repr__(&self) -> String {
        format!("rdflib.term.Literal({:?})", self.__str__())
    }

    /// The literal's datatype as a `URIRef`, or `None` for a plain/lang literal
    /// — matching rdflib (`Literal('x').datatype is None`).
    #[getter]
    fn datatype(&self) -> Option<URIRef> {
        match &self.inner {
            RdfTerm::Literal {
                datatype: Some(dt), ..
            } => Some(URIRef { iri: dt.clone() }),
            _ => None,
        }
    }

    /// The language tag, or `None`.
    #[getter]
    fn language(&self) -> Option<String> {
        match &self.inner {
            RdfTerm::Literal { language, .. } => language.clone(),
            _ => None,
        }
    }

    /// The effective datatype rdflib reports (`xsd:string`/`rdf:langString` for
    /// the implicit cases). Exposed as a helper for the compat tests.
    fn effective_datatype(&self) -> Option<URIRef> {
        self.inner.effective_datatype().map(|dt| URIRef {
            iri: dt.to_string(),
        })
    }

    fn __hash__(&self) -> u64 {
        kind_hash("L", &self.inner.to_store_lexical())
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

    fn n3(&self) -> String {
        self.inner.to_store_lexical()
    }
}

/// `rdflib.Variable` — a SPARQL variable name.
#[pyclass(module = "horndb_rdflib", frozen)]
#[derive(Clone)]
pub struct Variable {
    pub(crate) name: String,
}

#[pymethods]
impl Variable {
    #[new]
    fn new(value: &str) -> Self {
        Variable {
            name: value.trim_start_matches(['?', '$']).to_string(),
        }
    }

    fn __str__(&self) -> &str {
        &self.name
    }

    fn __repr__(&self) -> String {
        format!("rdflib.term.Variable({:?})", self.name)
    }

    fn __hash__(&self) -> u64 {
        kind_hash("V", &self.name)
    }

    fn __eq__(&self, other: &Bound<'_, PyAny>) -> bool {
        other
            .extract::<Variable>()
            .map(|o| o.name == self.name)
            .unwrap_or(false)
    }

    fn __ne__(&self, other: &Bound<'_, PyAny>) -> bool {
        !self.__eq__(other)
    }

    fn n3(&self) -> String {
        format!("?{}", self.name)
    }
}

/// `rdflib.Namespace` — a base IRI from which terms are built by attribute or
/// item access (`EX.foo`, `EX['foo']`) (F6).
#[pyclass(module = "horndb_rdflib", frozen)]
#[derive(Clone)]
pub struct Namespace {
    pub(crate) base: String,
}

#[pymethods]
impl Namespace {
    #[new]
    fn new(value: &str) -> Self {
        Namespace {
            base: value.to_string(),
        }
    }

    fn __str__(&self) -> &str {
        &self.base
    }

    fn __repr__(&self) -> String {
        format!("rdflib.namespace.Namespace({:?})", self.base)
    }

    fn term(&self, name: &str) -> URIRef {
        URIRef {
            iri: format!("{}{}", self.base, name),
        }
    }

    fn __getattr__(&self, name: &str) -> PyResult<URIRef> {
        if name.starts_with("__") && name.ends_with("__") {
            return Err(pyo3::exceptions::PyAttributeError::new_err(
                name.to_string(),
            ));
        }
        Ok(self.term(name))
    }

    fn __getitem__(&self, name: &str) -> URIRef {
        self.term(name)
    }

    fn __hash__(&self) -> u64 {
        kind_hash("N", &self.base)
    }

    fn __eq__(&self, other: &Bound<'_, PyAny>) -> bool {
        other
            .extract::<Namespace>()
            .map(|o| o.base == self.base)
            .unwrap_or(false)
    }
}

// ---------------------------------------------------------------------------
// Graph facade
// ---------------------------------------------------------------------------

/// `rdflib.Graph` — the core facade (F2/F4/F5/F6). Cheaply clonable handle
/// sharing one in-memory store, so the Python object can be passed around like
/// rdflib's mutable graph.
#[pyclass(module = "horndb_rdflib")]
pub struct Graph {
    inner: Arc<Mutex<RdfGraph>>,
    /// prefix -> namespace IRI, for serialisation and QName helpers (F6).
    namespaces: Arc<Mutex<Vec<(String, String)>>>,
}

#[pymethods]
impl Graph {
    #[new]
    fn new() -> Self {
        Graph {
            inner: Arc::new(Mutex::new(RdfGraph::new())),
            namespaces: Arc::new(Mutex::new(default_namespaces())),
        }
    }

    fn __len__(&self) -> usize {
        self.inner.lock().unwrap().len()
    }

    /// `graph.add((s, p, o))`.
    fn add(&self, triple: &Bound<'_, PyTuple>) -> PyResult<()> {
        let (s, p, o) = extract_triple(triple)?;
        self.inner
            .lock()
            .unwrap()
            .add(&s, &p, &o)
            .map_err(to_py_err)
    }

    /// `graph.remove((s, p, o))`. Like rdflib, any position may be `None` to
    /// remove every matching triple (e.g. `remove((s, p, None))`).
    fn remove(&self, triple: &Bound<'_, PyTuple>) -> PyResult<()> {
        let (s, p, o) = extract_triple_pattern(triple)?;
        let mut g = self.inner.lock().unwrap();
        for (ts, tp, to) in g.triples(s.as_ref(), p.as_ref(), o.as_ref()) {
            g.remove(&ts, &tp, &to);
        }
        Ok(())
    }

    /// `graph.set((s, p, o))` — replace all `(s, p, *)` with the one object.
    fn set(&self, triple: &Bound<'_, PyTuple>) -> PyResult<()> {
        let (s, p, o) = extract_triple(triple)?;
        let mut g = self.inner.lock().unwrap();
        for (ts, tp, to) in g.triples(Some(&s), Some(&p), None) {
            g.remove(&ts, &tp, &to);
        }
        g.add(&s, &p, &o).map_err(to_py_err)
    }

    /// `(s, p, o) in graph`. Like rdflib, `None` positions act as wildcards, so
    /// `(s, p, None) in graph` is a pattern-membership test.
    fn __contains__(&self, triple: &Bound<'_, PyTuple>) -> PyResult<bool> {
        let (s, p, o) = extract_triple_pattern(triple)?;
        // Exact triple (no wildcard) → cheap direct membership check.
        if let (Some(s), Some(p), Some(o)) = (&s, &p, &o) {
            return Ok(self.inner.lock().unwrap().contains(s, p, o));
        }
        Ok(!self
            .inner
            .lock()
            .unwrap()
            .triples(s.as_ref(), p.as_ref(), o.as_ref())
            .is_empty())
    }

    /// `graph.triples((s, p, o))` with `None` wildcards; also backs
    /// `iter(graph)`.
    #[pyo3(signature = (triple=None))]
    fn triples(&self, py: Python<'_>, triple: Option<&Bound<'_, PyTuple>>) -> PyResult<Py<PyAny>> {
        let (s, p, o) = match triple {
            Some(t) => extract_triple_pattern(t)?,
            None => (None, None, None),
        };
        let rows = self
            .inner
            .lock()
            .unwrap()
            .triples(s.as_ref(), p.as_ref(), o.as_ref());
        let out: Vec<Py<PyAny>> = rows
            .into_iter()
            .map(|(ts, tp, to)| {
                let tup = PyTuple::new(
                    py,
                    [
                        term_to_py(py, &ts)?,
                        term_to_py(py, &tp)?,
                        term_to_py(py, &to)?,
                    ],
                )?;
                Ok::<Py<PyAny>, PyErr>(tup.into_any().unbind())
            })
            .collect::<PyResult<_>>()?;
        Ok(out.into_pyobject(py)?.into_any().unbind())
    }

    fn __iter__(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let all = self.triples(py, None)?;
        let iter = all.bind(py).try_iter()?;
        Ok(iter.into_any().unbind())
    }

    /// `graph.subjects(predicate, object)` — distinct subjects matching the
    /// optional `(p, o)` filter.
    #[pyo3(signature = (predicate=None, object=None))]
    fn subjects(
        &self,
        py: Python<'_>,
        predicate: Option<&Bound<'_, PyAny>>,
        object: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Vec<Py<PyAny>>> {
        self.project(py, predicate, object, Position::Subject)
    }

    /// `graph.predicates(subject, object)`.
    #[pyo3(signature = (subject=None, object=None))]
    fn predicates(
        &self,
        py: Python<'_>,
        subject: Option<&Bound<'_, PyAny>>,
        object: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Vec<Py<PyAny>>> {
        self.project_sp(py, subject, object, Position::Predicate)
    }

    /// `graph.objects(subject, predicate)`.
    #[pyo3(signature = (subject=None, predicate=None))]
    fn objects(
        &self,
        py: Python<'_>,
        subject: Option<&Bound<'_, PyAny>>,
        predicate: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Vec<Py<PyAny>>> {
        self.project_sp(py, subject, predicate, Position::Object)
    }

    /// `graph.value(subject, predicate)` — the single object for `(s, p, *)`,
    /// or `None`. Matches rdflib's common 2-arg form.
    #[pyo3(signature = (subject=None, predicate=None))]
    fn value(
        &self,
        py: Python<'_>,
        subject: Option<&Bound<'_, PyAny>>,
        predicate: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Option<Py<PyAny>>> {
        let s = opt_term(subject)?;
        let p = opt_term(predicate)?;
        let g = self.inner.lock().unwrap();
        let mut rows = g.triples(s.as_ref(), p.as_ref(), None);
        match rows.len() {
            0 => Ok(None),
            _ => {
                let o = rows.remove(0).2;
                Ok(Some(term_to_py(py, &o)?))
            }
        }
    }

    /// `graph.bind(prefix, namespace)` (F6).
    fn bind(&self, prefix: &str, namespace: &Bound<'_, PyAny>) -> PyResult<()> {
        let ns = extract_iri_string(namespace)?;
        let mut map = self.namespaces.lock().unwrap();
        map.retain(|(pfx, _)| pfx != prefix);
        map.push((prefix.to_string(), ns));
        Ok(())
    }

    /// `list(graph.namespaces())` -> `(prefix, URIRef)` pairs (F6).
    fn namespaces(&self, py: Python<'_>) -> PyResult<Vec<Py<PyAny>>> {
        let map = self.namespaces.lock().unwrap();
        map.iter()
            .map(|(pfx, ns)| {
                let tup = PyTuple::new(
                    py,
                    [
                        PyString::new(py, pfx).into_any(),
                        Bound::new(py, URIRef { iri: ns.clone() })?.into_any(),
                    ],
                )?;
                Ok(tup.into_any().unbind())
            })
            .collect()
    }

    /// `graph.parse(data=..., format=...)` (F4). Only the `data`/`format`
    /// keyword path is supported in this increment.
    ///
    /// Returns the graph itself, matching rdflib's `Graph.parse()` so the
    /// common `g = Graph().parse(data=..., format="nt")` chaining idiom works.
    #[pyo3(signature = (data=None, format="turtle"))]
    fn parse<'py>(
        slf: Bound<'py, Self>,
        data: Option<&str>,
        format: &str,
    ) -> PyResult<Bound<'py, Self>> {
        let data = data.ok_or_else(|| {
            PyValueError::new_err(
                "Graph.parse: only the data=... keyword is supported in this build",
            )
        })?;
        let fmt = SerFormat::from_name(format).map_err(to_py_err)?;
        slf.borrow()
            .inner
            .lock()
            .unwrap()
            .parse_str(data, fmt)
            .map_err(to_py_err)?;
        Ok(slf)
    }

    /// `graph.serialize(format=...)` -> `str` (F4).
    #[pyo3(signature = (format="turtle"))]
    fn serialize(&self, format: &str) -> PyResult<String> {
        let fmt = SerFormat::from_name(format).map_err(to_py_err)?;
        self.inner
            .lock()
            .unwrap()
            .serialize_str(fmt)
            .map_err(to_py_err)
    }

    /// `graph.query(sparql)` -> a `Result` (F5).
    fn query(&self, py: Python<'_>, sparql: &str) -> PyResult<QueryResultPy> {
        let res = self
            .inner
            .lock()
            .unwrap()
            .query(sparql)
            .map_err(to_py_err)?;
        QueryResultPy::from_result(py, res)
    }

    /// `graph.update(sparql)` (F5).
    fn update(&self, sparql: &str) -> PyResult<()> {
        self.inner.lock().unwrap().update(sparql).map_err(to_py_err)
    }
}

#[derive(Clone, Copy)]
enum Position {
    Subject,
    Predicate,
    Object,
}

impl Graph {
    fn project(
        &self,
        py: Python<'_>,
        predicate: Option<&Bound<'_, PyAny>>,
        object: Option<&Bound<'_, PyAny>>,
        which: Position,
    ) -> PyResult<Vec<Py<PyAny>>> {
        let p = opt_term(predicate)?;
        let o = opt_term(object)?;
        let g = self.inner.lock().unwrap();
        let rows = g.triples(None, p.as_ref(), o.as_ref());
        distinct_position(py, rows, which)
    }

    fn project_sp(
        &self,
        py: Python<'_>,
        first: Option<&Bound<'_, PyAny>>,
        second: Option<&Bound<'_, PyAny>>,
        which: Position,
    ) -> PyResult<Vec<Py<PyAny>>> {
        let a = opt_term(first)?;
        let b = opt_term(second)?;
        let g = self.inner.lock().unwrap();
        let rows = match which {
            // predicates(subject, object)
            Position::Predicate => g.triples(a.as_ref(), None, b.as_ref()),
            // objects(subject, predicate)
            Position::Object => g.triples(a.as_ref(), b.as_ref(), None),
            Position::Subject => unreachable!(),
        };
        distinct_position(py, rows, which)
    }
}

fn distinct_position(
    py: Python<'_>,
    rows: Vec<(RdfTerm, RdfTerm, RdfTerm)>,
    which: Position,
) -> PyResult<Vec<Py<PyAny>>> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for (s, p, o) in rows {
        let t = match which {
            Position::Subject => s,
            Position::Predicate => p,
            Position::Object => o,
        };
        if seen.insert(t.to_store_lexical()) {
            out.push(term_to_py(py, &t)?);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Query result object
// ---------------------------------------------------------------------------

/// `graph.query(...)` return value. Iterating a SELECT yields one tuple of
/// bound terms per solution; `bool(result)` gives an ASK answer; iterating a
/// CONSTRUCT yields `(s, p, o)` tuples (F5).
#[pyclass(module = "horndb_rdflib", name = "Result")]
pub struct QueryResultPy {
    vars: Vec<String>,
    /// Each row is the Python objects for one solution (SELECT/CONSTRUCT) — a
    /// tuple already built. ASK has an empty `rows` and sets `ask`.
    rows: Vec<Py<PyAny>>,
    ask: Option<bool>,
    kind: &'static str,
}

impl QueryResultPy {
    fn from_result(py: Python<'_>, res: QueryResult) -> PyResult<Self> {
        match res {
            QueryResult::Select { vars, solutions } => {
                let rows = solutions
                    .into_iter()
                    .map(|row| {
                        let cells: Vec<Py<PyAny>> = row
                            .iter()
                            .map(|cell| match cell {
                                Some(t) => term_to_py(py, t),
                                None => Ok(py.None()),
                            })
                            .collect::<PyResult<_>>()?;
                        Ok(PyTuple::new(py, cells)?.into_any().unbind())
                    })
                    .collect::<PyResult<_>>()?;
                Ok(QueryResultPy {
                    vars,
                    rows,
                    ask: None,
                    kind: "SELECT",
                })
            }
            QueryResult::Ask(b) => Ok(QueryResultPy {
                vars: vec![],
                rows: vec![],
                ask: Some(b),
                kind: "ASK",
            }),
            QueryResult::Construct(triples) => {
                let rows = triples
                    .into_iter()
                    .map(|(s, p, o)| {
                        let tup = PyTuple::new(
                            py,
                            [
                                term_to_py(py, &s)?,
                                term_to_py(py, &p)?,
                                term_to_py(py, &o)?,
                            ],
                        )?;
                        Ok(tup.into_any().unbind())
                    })
                    .collect::<PyResult<_>>()?;
                Ok(QueryResultPy {
                    vars: vec![],
                    rows,
                    ask: None,
                    kind: "CONSTRUCT",
                })
            }
            QueryResult::Explanation(text) => Ok(QueryResultPy {
                vars: vec![],
                rows: vec![text_to_py(py, &text)?],
                ask: None,
                kind: "EXPLAIN",
            }),
        }
    }
}

#[pymethods]
impl QueryResultPy {
    /// The projected variable names (SELECT only).
    #[getter]
    fn vars(&self) -> Vec<String> {
        self.vars.clone()
    }

    /// The query form: "SELECT", "ASK", "CONSTRUCT" or "EXPLAIN". Exposed under
    /// rdflib's attribute name `result.type` (`type` is a Rust keyword, so the
    /// method is `type_` but the Python getter is named `type`).
    #[getter]
    #[pyo3(name = "type")]
    fn type_(&self) -> &str {
        self.kind
    }

    fn __len__(&self) -> usize {
        self.rows.len()
    }

    fn __bool__(&self) -> bool {
        match self.ask {
            Some(b) => b,
            None => !self.rows.is_empty(),
        }
    }

    fn __iter__(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let cloned: Vec<Py<PyAny>> = self.rows.iter().map(|r| r.clone_ref(py)).collect();
        let list = cloned.into_pyobject(py)?;
        Ok(list.try_iter()?.into_any().unbind())
    }
}

// ---------------------------------------------------------------------------
// Conversions
// ---------------------------------------------------------------------------

/// Turn a Python term object (`URIRef`/`BNode`/`Literal`) into an [`RdfTerm`].
/// A bare `str` is treated as a `URIRef`, matching rdflib's leniency.
fn py_to_term(obj: &Bound<'_, PyAny>) -> PyResult<RdfTerm> {
    if let Ok(u) = obj.extract::<URIRef>() {
        return Ok(RdfTerm::iri(u.iri));
    }
    if let Ok(b) = obj.extract::<BNode>() {
        return Ok(RdfTerm::blank(b.label));
    }
    if let Ok(l) = obj.extract::<Literal>() {
        return Ok(l.inner);
    }
    if let Ok(s) = obj.extract::<String>() {
        return Ok(RdfTerm::iri(s));
    }
    Err(PyValueError::new_err(
        "expected a URIRef, BNode, Literal, or str term",
    ))
}

fn opt_term(obj: Option<&Bound<'_, PyAny>>) -> PyResult<Option<RdfTerm>> {
    match obj {
        None => Ok(None),
        Some(o) if o.is_none() => Ok(None),
        Some(o) => py_to_term(o).map(Some),
    }
}

/// Build the Python term object for an [`RdfTerm`].
fn term_to_py(py: Python<'_>, t: &RdfTerm) -> PyResult<Py<PyAny>> {
    Ok(match t {
        RdfTerm::Iri(i) => Bound::new(py, URIRef { iri: i.clone() })?
            .into_any()
            .unbind(),
        RdfTerm::Blank(b) => Bound::new(py, BNode { label: b.clone() })?
            .into_any()
            .unbind(),
        RdfTerm::Literal { .. } => Bound::new(py, Literal { inner: t.clone() })?
            .into_any()
            .unbind(),
    })
}

fn text_to_py(py: Python<'_>, s: &str) -> PyResult<Py<PyAny>> {
    Ok(PyString::new(py, s).into_any().unbind())
}

/// Pull an IRI string out of a `URIRef`, `Namespace`, or plain `str`.
fn extract_iri_string(obj: &Bound<'_, PyAny>) -> PyResult<String> {
    if let Ok(u) = obj.extract::<URIRef>() {
        return Ok(u.iri);
    }
    if let Ok(n) = obj.extract::<Namespace>() {
        return Ok(n.base);
    }
    if let Ok(s) = obj.extract::<String>() {
        return Ok(s);
    }
    Err(PyValueError::new_err(
        "expected a URIRef, Namespace, or str",
    ))
}

fn extract_triple(triple: &Bound<'_, PyTuple>) -> PyResult<(RdfTerm, RdfTerm, RdfTerm)> {
    if triple.len() != 3 {
        return Err(PyKeyError::new_err("a triple must have exactly 3 elements"));
    }
    Ok((
        py_to_term(&triple.get_item(0)?)?,
        py_to_term(&triple.get_item(1)?)?,
        py_to_term(&triple.get_item(2)?)?,
    ))
}

/// Like [`extract_triple`] but each position may be `None` (a wildcard).
#[allow(clippy::type_complexity)]
fn extract_triple_pattern(
    triple: &Bound<'_, PyTuple>,
) -> PyResult<(Option<RdfTerm>, Option<RdfTerm>, Option<RdfTerm>)> {
    if triple.len() != 3 {
        return Err(PyKeyError::new_err("a triple pattern must have 3 elements"));
    }
    Ok((
        opt_term(Some(&triple.get_item(0)?))?,
        opt_term(Some(&triple.get_item(1)?))?,
        opt_term(Some(&triple.get_item(2)?))?,
    ))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn kind_hash(tag: &str, body: &str) -> u64 {
    let mut h = DefaultHasher::new();
    tag.hash(&mut h);
    body.hash(&mut h);
    h.finish()
}

fn default_namespaces() -> Vec<(String, String)> {
    vec![
        (
            "rdf".to_string(),
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#".to_string(),
        ),
        (
            "rdfs".to_string(),
            "http://www.w3.org/2000/01/rdf-schema#".to_string(),
        ),
        ("xsd".to_string(), XSD_STRING.replace("string", "")),
        (
            "owl".to_string(),
            "http://www.w3.org/2002/07/owl#".to_string(),
        ),
    ]
}

fn fresh_bnode_label() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("N{n:016x}")
}

// ---------------------------------------------------------------------------
// Module init
// ---------------------------------------------------------------------------

/// The `horndb_rdflib` extension module.
#[pymodule]
fn horndb_rdflib(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<URIRef>()?;
    m.add_class::<BNode>()?;
    m.add_class::<Literal>()?;
    m.add_class::<Variable>()?;
    m.add_class::<Namespace>()?;
    m.add_class::<Graph>()?;
    m.add_class::<QueryResultPy>()?;
    m.add(
        "RDF",
        Namespace::new("http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
    )?;
    m.add(
        "RDFS",
        Namespace::new("http://www.w3.org/2000/01/rdf-schema#"),
    )?;
    m.add("XSD", Namespace::new("http://www.w3.org/2001/XMLSchema#"))?;
    m.add("OWL", Namespace::new("http://www.w3.org/2002/07/owl#"))?;
    Ok(())
}
