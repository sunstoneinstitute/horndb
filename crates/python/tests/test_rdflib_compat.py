"""Differential compatibility tests: HornDB's binding vs upstream rdflib.

SPEC-10 acceptance #2 and #6. Each test asserts that `horndb.rdflib` behaves
the same as `rdflib` on a representative, graph-centric example. Where HornDB
intentionally diverges, the divergence is asserted explicitly and documented in
SPEC-10 / the crate README rather than silently approximated (NF4).

The rdflib-compatible facade lives under the `horndb.rdflib` submodule; the
top-level `horndb` package is the native, pyoxigraph-shaped API.

Run with:  maturin develop --features extension-module && pytest tests/
"""

import pytest

import rdflib
import horndb.rdflib as hb


# --------------------------------------------------------------------------- #
# F1 — term semantics
# --------------------------------------------------------------------------- #

def test_uriref_equality_and_str():
    assert str(hb.URIRef("http://ex/s")) == str(rdflib.URIRef("http://ex/s"))
    assert hb.URIRef("http://ex/s") == hb.URIRef("http://ex/s")
    assert hb.URIRef("http://ex/s") != hb.URIRef("http://ex/t")


def test_uriref_hashes_consistently_within_each_lib():
    # Hash need not match rdflib's value, but must be stable & usable as a key.
    d = {hb.URIRef("http://ex/s"): 1}
    assert d[hb.URIRef("http://ex/s")] == 1


def test_uriref_concat_matches_rdflib():
    assert str(hb.URIRef("http://ex/") + "x") == str(rdflib.URIRef("http://ex/") + "x")


def test_plain_literal_matches_rdflib():
    assert str(hb.Literal("hello")) == str(rdflib.Literal("hello"))
    # rdflib: a plain literal has datatype None.
    assert hb.Literal("hello").datatype is None
    assert rdflib.Literal("hello").datatype is None


def test_typed_literal_datatype_matches_rdflib():
    xsd_int = "http://www.w3.org/2001/XMLSchema#integer"
    hl = hb.Literal("42", datatype=hb.URIRef(xsd_int))
    rl = rdflib.Literal("42", datatype=rdflib.URIRef(xsd_int))
    assert str(hl.datatype) == str(rl.datatype)
    assert str(hl) == str(rl)


def test_lang_literal_matches_rdflib():
    hl = hb.Literal("chat", lang="en")
    rl = rdflib.Literal("chat", lang="en")
    assert hl.language == rl.language == "en"
    assert str(hl) == str(rl)


def test_bnode_distinct_labels_differ():
    assert hb.BNode("a") != hb.BNode("b")
    assert hb.BNode("a") == hb.BNode("a")


def test_fresh_bnodes_are_unique():
    assert hb.BNode() != hb.BNode()


def test_variable_strips_sigil_like_rdflib():
    assert str(hb.Variable("?x")) == str(rdflib.Variable("x")) == "x"


def test_uriref_n3_matches_rdflib_angle_brackets():
    # rdflib wraps IRIs in <> for n3()/SPARQL/Turtle; the bare form is invalid.
    assert hb.URIRef("http://ex/s").n3() == rdflib.URIRef("http://ex/s").n3() == "<http://ex/s>"


def test_literal_n3_matches_rdflib():
    assert hb.Literal("hi").n3() == rdflib.Literal("hi").n3() == '"hi"'
    assert hb.Literal("chat", lang="en").n3() == rdflib.Literal("chat", lang="en").n3()


def test_bnode_n3_is_underscore_colon():
    assert hb.BNode("b0").n3() == "_:b0"


# --------------------------------------------------------------------------- #
# F2 — Graph mutation, len, contains, iteration
# --------------------------------------------------------------------------- #

def build_pair():
    """Same triples added to an rdflib.Graph and a horndb Graph."""
    triples = [
        ("http://ex/alice", "http://ex/knows", "http://ex/bob"),
        ("http://ex/alice", "http://ex/knows", "http://ex/carol"),
    ]
    rg = rdflib.Graph()
    hg = hb.Graph()
    for s, p, o in triples:
        rg.add((rdflib.URIRef(s), rdflib.URIRef(p), rdflib.URIRef(o)))
        hg.add((hb.URIRef(s), hb.URIRef(p), hb.URIRef(o)))
    return rg, hg


def test_len_matches_rdflib():
    rg, hg = build_pair()
    assert len(hg) == len(rg) == 2


def test_add_is_idempotent_like_rdflib():
    rg, hg = build_pair()
    rg.add((rdflib.URIRef("http://ex/alice"), rdflib.URIRef("http://ex/knows"), rdflib.URIRef("http://ex/bob")))
    hg.add((hb.URIRef("http://ex/alice"), hb.URIRef("http://ex/knows"), hb.URIRef("http://ex/bob")))
    assert len(hg) == len(rg) == 2


def test_contains_matches_rdflib():
    rg, hg = build_pair()
    t_r = (rdflib.URIRef("http://ex/alice"), rdflib.URIRef("http://ex/knows"), rdflib.URIRef("http://ex/bob"))
    t_h = (hb.URIRef("http://ex/alice"), hb.URIRef("http://ex/knows"), hb.URIRef("http://ex/bob"))
    assert (t_h in hg) == (t_r in rg) is True
    miss_r = (rdflib.URIRef("http://ex/alice"), rdflib.URIRef("http://ex/knows"), rdflib.URIRef("http://ex/dave"))
    miss_h = (hb.URIRef("http://ex/alice"), hb.URIRef("http://ex/knows"), hb.URIRef("http://ex/dave"))
    assert (miss_h in hg) == (miss_r in rg) is False


def test_remove_matches_rdflib():
    rg, hg = build_pair()
    rg.remove((rdflib.URIRef("http://ex/alice"), rdflib.URIRef("http://ex/knows"), rdflib.URIRef("http://ex/bob")))
    hg.remove((hb.URIRef("http://ex/alice"), hb.URIRef("http://ex/knows"), hb.URIRef("http://ex/bob")))
    assert len(hg) == len(rg) == 1


def test_iteration_yields_same_triple_set():
    rg, hg = build_pair()

    def norm(triples):
        return sorted((str(s), str(p), str(o)) for s, p, o in triples)

    assert norm(hg) == norm(rg)


def test_objects_matches_rdflib():
    rg, hg = build_pair()
    r_objs = sorted(str(o) for o in rg.objects(rdflib.URIRef("http://ex/alice"), rdflib.URIRef("http://ex/knows")))
    h_objs = sorted(str(o) for o in hg.objects(hb.URIRef("http://ex/alice"), hb.URIRef("http://ex/knows")))
    assert h_objs == r_objs == ["http://ex/bob", "http://ex/carol"]


def test_subjects_and_value():
    rg, hg = build_pair()
    r_subj = sorted(str(s) for s in rg.subjects(rdflib.URIRef("http://ex/knows"), rdflib.URIRef("http://ex/bob")))
    h_subj = sorted(str(s) for s in hg.subjects(hb.URIRef("http://ex/knows"), hb.URIRef("http://ex/bob")))
    assert h_subj == r_subj == ["http://ex/alice"]
    # value(): single object for (s, p, *) — rdflib returns one (any) match.
    v = hg.value(hb.URIRef("http://ex/alice"), hb.URIRef("http://ex/knows"))
    assert str(v) in {"http://ex/bob", "http://ex/carol"}


def test_mutators_return_self_like_rdflib():
    # rdflib: add/remove/set all return the graph (verified against rdflib).
    g = hb.Graph()
    r = g.add((hb.URIRef("http://ex/s"), hb.URIRef("http://ex/p"), hb.URIRef("http://ex/o")))
    assert r is g
    r2 = g.set((hb.URIRef("http://ex/s"), hb.URIRef("http://ex/p"), hb.URIRef("http://ex/o2")))
    assert r2 is g
    r3 = g.remove((hb.URIRef("http://ex/s"), hb.URIRef("http://ex/p"), None))
    assert r3 is g


def test_remove_with_none_wildcard_matches_rdflib():
    rg, hg = build_pair()
    # rdflib: remove((s, p, None)) deletes every matching object.
    rg.remove((rdflib.URIRef("http://ex/alice"), rdflib.URIRef("http://ex/knows"), None))
    hg.remove((hb.URIRef("http://ex/alice"), hb.URIRef("http://ex/knows"), None))
    assert len(hg) == len(rg) == 0


def test_contains_with_none_wildcard_matches_rdflib():
    rg, hg = build_pair()
    pat_r = (rdflib.URIRef("http://ex/alice"), rdflib.URIRef("http://ex/knows"), None)
    pat_h = (hb.URIRef("http://ex/alice"), hb.URIRef("http://ex/knows"), None)
    assert (pat_h in hg) == (pat_r in rg) is True
    miss_r = (rdflib.URIRef("http://ex/nobody"), None, None)
    miss_h = (hb.URIRef("http://ex/nobody"), None, None)
    assert (miss_h in hg) == (miss_r in rg) is False


def test_literal_subject_rejected():
    hg = hb.Graph()
    with pytest.raises(ValueError):
        hg.add((hb.Literal("x"), hb.URIRef("http://ex/p"), hb.URIRef("http://ex/o")))


# --------------------------------------------------------------------------- #
# F4 — parse / serialize
# --------------------------------------------------------------------------- #

NT_DOC = '<http://ex/s> <http://ex/p> "hi" .\n'
TTL_DOC = "@prefix ex: <http://ex/> .\nex:s ex:p ex:o .\n"


def test_parse_ntriples_matches_rdflib_triple_set():
    rg = rdflib.Graph(); rg.parse(data=NT_DOC, format="nt")
    hg = hb.Graph(); hg.parse(data=NT_DOC, format="nt")
    assert len(hg) == len(rg) == 1
    rh = sorted((str(s), str(p), str(o)) for s, p, o in rg)
    hh = sorted((str(s), str(p), str(o)) for s, p, o in hg)
    assert hh == rh


def test_parse_returns_self_for_chaining():
    # rdflib: Graph().parse(...) returns the graph, enabling the common idiom.
    g = hb.Graph().parse(data=NT_DOC, format="nt")
    assert g is not None
    assert len(g) == 1


def test_parse_turtle_with_prefix():
    rg = rdflib.Graph(); rg.parse(data=TTL_DOC, format="turtle")
    hg = hb.Graph(); hg.parse(data=TTL_DOC, format="turtle")
    assert len(hg) == len(rg) == 1
    assert (hb.URIRef("http://ex/s"), hb.URIRef("http://ex/p"), hb.URIRef("http://ex/o")) in hg


def test_serialize_round_trips_through_rdflib():
    # Serialize from HornDB, re-parse with rdflib: the triple set must survive.
    hg = hb.Graph()
    hg.add((hb.URIRef("http://ex/s"), hb.URIRef("http://ex/p"), hb.Literal("hi")))
    out = hg.serialize(format="nt")
    rg = rdflib.Graph(); rg.parse(data=out, format="nt")
    assert len(rg) == 1
    s, p, o = next(iter(rg))
    assert str(s) == "http://ex/s" and str(o) == "hi"


def test_unsupported_format_raises():
    hg = hb.Graph()
    with pytest.raises(ValueError):
        hg.serialize(format="json-ld")


# --------------------------------------------------------------------------- #
# F5 — SPARQL query / update passthrough
# --------------------------------------------------------------------------- #

def test_select_matches_rdflib_bindings():
    rg, hg = build_pair()
    q = "SELECT ?o WHERE { <http://ex/alice> <http://ex/knows> ?o }"
    r_objs = sorted(str(row[0]) for row in rg.query(q))
    h_objs = sorted(str(row[0]) for row in hg.query(q))
    assert h_objs == r_objs == ["http://ex/bob", "http://ex/carol"]


def test_select_preserves_blank_node_kind():
    # A blank node bound by a SELECT must come back as a BNode, not a URIRef.
    hg = hb.Graph()
    hg.add((hb.BNode("b0"), hb.URIRef("http://ex/p"), hb.URIRef("http://ex/o")))
    rows = list(hg.query("SELECT ?s WHERE { ?s <http://ex/p> <http://ex/o> }"))
    assert len(rows) == 1
    s = rows[0][0]
    assert isinstance(s, hb.BNode), f"expected BNode, got {type(s)}"
    assert not isinstance(s, hb.URIRef)


def test_ask_matches_rdflib():
    rg, hg = build_pair()
    q_true = "ASK { <http://ex/alice> <http://ex/knows> <http://ex/bob> }"
    q_false = "ASK { <http://ex/alice> <http://ex/knows> <http://ex/dave> }"
    assert bool(hg.query(q_true)) == bool(rg.query(q_true)) is True
    assert bool(hg.query(q_false)) == bool(rg.query(q_false)) is False


def test_construct_matches_rdflib_triple_set():
    rg, hg = build_pair()
    q = ("CONSTRUCT { ?s <http://ex/friend> ?o } "
         "WHERE { ?s <http://ex/knows> ?o }")
    r_set = sorted((str(s), str(p), str(o)) for s, p, o in rg.query(q))
    h_set = sorted((str(s), str(p), str(o)) for s, p, o in hg.query(q))
    assert h_set == r_set
    assert len(h_set) == 2


def test_result_type_attribute_matches_rdflib():
    rg, hg = build_pair()
    q = "SELECT ?o WHERE { ?s <http://ex/knows> ?o }"
    # rdflib exposes the query form as `result.type`.
    assert hg.query(q).type == rg.query(q).type == "SELECT"
    qa = "ASK { <http://ex/alice> <http://ex/knows> <http://ex/bob> }"
    assert hg.query(qa).type == rg.query(qa).type == "ASK"
    qc = "CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o }"
    assert hg.query(qc).type == rg.query(qc).type == "CONSTRUCT"


def test_update_insert_then_query():
    hg = hb.Graph()
    hg.update("INSERT DATA { <http://ex/s> <http://ex/p> <http://ex/o> }")
    assert len(hg) == 1
    assert bool(hg.query("ASK { <http://ex/s> <http://ex/p> <http://ex/o> }"))


def test_update_delete():
    rg, hg = build_pair()
    upd = "DELETE DATA { <http://ex/alice> <http://ex/knows> <http://ex/bob> }"
    rg.update(upd)
    hg.update(upd)
    assert len(hg) == len(rg) == 1


# --------------------------------------------------------------------------- #
# F6 — namespaces
# --------------------------------------------------------------------------- #

def test_select_expression_result_is_literal_not_uriref():
    # A SPARQL effective-boolean expression result must surface as a Literal,
    # not a URIRef — matching rdflib's term typing for expression bindings.
    hg = hb.Graph()
    hg.add((hb.URIRef("http://ex/s"), hb.URIRef("http://ex/p"), hb.URIRef("http://ex/o")))
    rows = list(hg.query("SELECT (isIRI(<http://ex/s>) AS ?b) WHERE { ?s ?p ?o }"))
    assert len(rows) == 1
    val = rows[0][0]
    assert isinstance(val, hb.Literal), f"expected Literal, got {type(val)}"
    assert not isinstance(val, hb.URIRef)


def test_serialize_turtle_uses_bound_prefix():
    hg = hb.Graph()
    hg.bind("ex", hb.Namespace("http://ex/"))
    hg.add((hb.URIRef("http://ex/s"), hb.URIRef("http://ex/p"), hb.URIRef("http://ex/o")))
    out = hg.serialize(format="turtle")
    assert "@prefix ex:" in out, out
    assert "ex:s" in out, out


def test_namespace_term_access_matches_rdflib():
    H = hb.Namespace("http://ex/")
    R = rdflib.Namespace("http://ex/")
    assert str(H.foo) == str(R.foo) == "http://ex/foo"
    assert str(H["bar"]) == str(R["bar"]) == "http://ex/bar"


def test_bind_round_trips():
    hg = hb.Graph()
    hg.bind("ex", hb.Namespace("http://ex/"))
    prefixes = {pfx: str(ns) for pfx, ns in hg.namespaces()}
    assert prefixes["ex"] == "http://ex/"
