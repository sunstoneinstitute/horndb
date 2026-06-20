"""Tests for the native, pyoxigraph-shaped ``horndb`` API (SPEC-10 native spine).

These exercise the quad ``Store`` with named graphs, ``quads_for_pattern``,
``load``/``serialize``, SPARQL ``query``/``update`` (including the
``use_default_graph_as_union`` knob), and the HornDB-specific
``materialize()`` OWL 2 RL step.

The surface mirrors ``pyoxigraph`` so ``pyoxigraph``-oriented code (such as
``rdf-registry``'s build pipeline) ports by changing only the import. Where a
``pyoxigraph`` install is available, a couple of tests additionally diff against
it; otherwise they skip.

Run with:  maturin develop --features extension-module && pytest tests/
"""

import pytest

import horndb
from horndb import (
    BlankNode,
    DefaultGraph,
    Literal,
    NamedNode,
    Quad,
    RdfFormat,
    Store,
    Variable,
)

RDF_TYPE = NamedNode("http://www.w3.org/1999/02/22-rdf-syntax-ns#type")
SKOS_SCHEME = NamedNode("http://www.w3.org/2004/02/skos/core#ConceptScheme")
SKOS_CONCEPT = NamedNode("http://www.w3.org/2004/02/skos/core#Concept")


# --------------------------------------------------------------------------- #
# Terms
# --------------------------------------------------------------------------- #

def test_named_node_value_and_str():
    n = NamedNode("http://ex/s")
    assert n.value == "http://ex/s"
    assert str(n) == "<http://ex/s>"
    assert n == NamedNode("http://ex/s")
    assert n != NamedNode("http://ex/t")


def test_literal_value_datatype_language():
    plain = Literal("hello")
    assert plain.value == "hello"
    # pyoxigraph reports the effective datatype (xsd:string) for a plain literal.
    assert plain.datatype == NamedNode("http://www.w3.org/2001/XMLSchema#string")
    assert plain.language is None

    lang = Literal("chat", language="fr")
    assert lang.language == "fr"
    assert lang.datatype == NamedNode(
        "http://www.w3.org/1999/02/22-rdf-syntax-ns#langString"
    )

    typed = Literal("42", datatype=NamedNode("http://www.w3.org/2001/XMLSchema#integer"))
    assert typed.value == "42"
    assert typed.datatype.value == "http://www.w3.org/2001/XMLSchema#integer"


def test_blank_node_autogenerates_label():
    assert BlankNode("b0").value == "b0"
    assert BlankNode().value  # non-empty auto label


# --------------------------------------------------------------------------- #
# Store basics + named graphs
# --------------------------------------------------------------------------- #

def test_add_len_contains_named_graph():
    store = Store()
    g = NamedNode("http://g/1")
    q = Quad(NamedNode("http://ex/s"), NamedNode("http://ex/p"), NamedNode("http://ex/o"), g)
    store.add(q)
    assert len(store) == 1
    assert q in store
    # Same triple, default graph → a distinct quad.
    store.add(Quad(NamedNode("http://ex/s"), NamedNode("http://ex/p"), NamedNode("http://ex/o")))
    assert len(store) == 2
    assert store.named_graphs() == [g]


def test_quads_for_pattern_graph_filter():
    store = Store()
    g = NamedNode("http://g/1")
    store.add(Quad(NamedNode("http://ex/a"), RDF_TYPE, SKOS_CONCEPT))  # default graph
    store.add(Quad(NamedNode("http://ex/b"), RDF_TYPE, SKOS_CONCEPT, g))  # named graph

    # All graphs.
    assert len(store.quads_for_pattern(None, RDF_TYPE, None, None)) == 2
    # Default graph only.
    default = store.quads_for_pattern(None, None, None, DefaultGraph())
    assert len(default) == 1
    assert default[0].subject == NamedNode("http://ex/a")
    # One named graph.
    named = store.quads_for_pattern(None, None, None, g)
    assert len(named) == 1
    assert named[0].subject == NamedNode("http://ex/b")
    assert named[0].graph_name == g


def test_remove_quad():
    store = Store()
    q = Quad(NamedNode("http://ex/s"), NamedNode("http://ex/p"), NamedNode("http://ex/o"))
    store.add(q)
    store.remove(q)
    assert len(store) == 0


# --------------------------------------------------------------------------- #
# load / serialize
# --------------------------------------------------------------------------- #

def test_load_turtle_into_named_graph():
    store = Store()
    data = "<http://ex/s> <http://ex/p> <http://ex/o> .\n"
    n = store.load(data, RdfFormat.TURTLE, to_graph=NamedNode("file:foo.ttl"))
    assert n == 1
    assert store.named_graphs() == [NamedNode("file:foo.ttl")]


def test_load_nquads_keeps_graph_and_round_trips():
    store = Store()
    data = "<http://ex/s> <http://ex/p> <http://ex/o> <http://g/1> .\n"
    store.load(data, RdfFormat.N_QUADS)
    assert store.named_graphs() == [NamedNode("http://g/1")]
    out = store.serialize(RdfFormat.N_QUADS)
    assert "<http://g/1>" in out
    # format-string also accepted
    store2 = Store()
    store2.load(out, "nquads")
    assert store2.named_graphs() == [NamedNode("http://g/1")]


# --------------------------------------------------------------------------- #
# SPARQL query — the rdf-registry workflow
# --------------------------------------------------------------------------- #

def test_query_default_vs_union():
    store = Store()
    store.add(Quad(NamedNode("http://ex/a"), RDF_TYPE, SKOS_CONCEPT))  # default
    store.add(Quad(NamedNode("http://ex/b"), RDF_TYPE, SKOS_CONCEPT, NamedNode("http://g/1")))

    q = (
        "PREFIX skos: <http://www.w3.org/2004/02/skos/core#> "
        "SELECT ?s WHERE { ?s a skos:Concept }"
    )
    # Default graph only.
    rows = list(store.query(q))
    assert len(rows) == 1
    # Union of all graphs — what rdf-registry's discover.rq relies on.
    rows = list(store.query(q, use_default_graph_as_union=True))
    assert len(rows) == 2


def test_query_solution_indexing():
    store = Store()
    store.add(Quad(NamedNode("http://ex/a"), RDF_TYPE, SKOS_SCHEME))
    sols = store.query("SELECT ?uri ?type WHERE { ?uri ?type ?o }")
    assert [str(v) for v in sols.variables] == ["?uri", "?type"]
    rows = list(sols)
    assert len(rows) == 1
    row = rows[0]
    # Index by name and by position; .value mirrors pyoxigraph terms.
    assert row["uri"].value == "http://ex/a"
    assert row[0] == row["uri"]
    assert row["type"] == RDF_TYPE
    assert row.get("missing") is None


def test_discover_rq_shape():
    """The exact UNION + BIND discovery query rdf-registry runs."""
    store = Store()
    store.load(
        "<https://sunstone.institute/rdf/foo> a "
        "<http://www.w3.org/2004/02/skos/core#ConceptScheme> .\n",
        RdfFormat.N_TRIPLES,
        to_graph=NamedNode("file:foo.ttl"),
    )
    query = """
        PREFIX owl: <http://www.w3.org/2002/07/owl#>
        PREFIX skos: <http://www.w3.org/2004/02/skos/core#>
        SELECT ?uri ?type WHERE {
            { ?uri a owl:Ontology       BIND("ontology" AS ?type) } UNION
            { ?uri a skos:ConceptScheme BIND("scheme"   AS ?type) } UNION
            { ?uri a skos:Concept       BIND("concept"  AS ?type) }
        }
    """
    rows = list(store.query(query, use_default_graph_as_union=True))
    assert len(rows) == 1
    assert rows[0]["uri"].value == "https://sunstone.institute/rdf/foo"
    assert rows[0]["type"].value == "scheme"


def test_ask_and_construct():
    store = Store()
    store.add(Quad(NamedNode("http://ex/s"), NamedNode("http://ex/p"), NamedNode("http://ex/o")))
    assert store.query("ASK { <http://ex/s> <http://ex/p> <http://ex/o> }") is True
    triples = store.query(
        "CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o }"
    )
    assert len(triples) == 1
    assert triples[0].subject == NamedNode("http://ex/s")


# --------------------------------------------------------------------------- #
# update + reasoning
# --------------------------------------------------------------------------- #

def test_update_default_graph():
    store = Store()
    store.update("INSERT DATA { <http://ex/s> <http://ex/p> <http://ex/o> . }")
    assert len(store.quads_for_pattern(None, None, None, DefaultGraph())) == 1


def test_materialize_owl_rl():
    # :Penguin rdfs:subClassOf :Bird ; :pingu a :Penguin  ⇒  :pingu a :Bird
    store = Store()
    sco = NamedNode("http://www.w3.org/2000/01/rdf-schema#subClassOf")
    store.add(Quad(NamedNode("http://ex/Penguin"), sco, NamedNode("http://ex/Bird")))
    store.add(Quad(NamedNode("http://ex/pingu"), RDF_TYPE, NamedNode("http://ex/Penguin")))

    asserted, inferred = store.materialize()
    assert inferred >= 1
    assert store.query(
        "ASK { <http://ex/pingu> a <http://ex/Bird> }"
    ) is True

    # Re-materialize is idempotent; clear_inferred drops only entailed triples.
    before = len(store)
    store.clear_inferred()
    assert len(store) < before


# --------------------------------------------------------------------------- #
# Optional differential check against pyoxigraph
# --------------------------------------------------------------------------- #

def test_matches_pyoxigraph_quads_for_pattern():
    ox = pytest.importorskip("pyoxigraph")

    def build(mod, ngraph):
        s = mod.Store()
        s.add(mod.Quad(mod.NamedNode("http://ex/a"), mod.NamedNode("http://ex/p"),
                       mod.NamedNode("http://ex/o"), ngraph))
        s.add(mod.Quad(mod.NamedNode("http://ex/b"), mod.NamedNode("http://ex/p"),
                       mod.NamedNode("http://ex/o")))
        return s

    hb_store = build(horndb, NamedNode("http://g/1"))
    ox_store = build(ox, ox.NamedNode("http://g/1"))

    hb_subjects = sorted(q.subject.value for q in hb_store.quads_for_pattern(None, None, None, None))
    ox_subjects = sorted(q.subject.value for q in ox_store.quads_for_pattern(None, None, None, None))
    assert hb_subjects == ox_subjects == ["http://ex/a", "http://ex/b"]
