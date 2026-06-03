#!/usr/bin/env python3
"""Emit the *list-axiom* schema closure RDFox needs to match HornDB.

HornDB's OWL 2 RL engine fires two families of rules that the fixed-arity
`gen_ruleset.py` translation **cannot** express, because they are not in
`crates/owlrl/rules.toml` at all:

  1. The list-axiom rules in `crates/owlrl/src/list_rules.rs` — `scm-int`,
     `cls-int1`, `cls-uni`, `prp-spo2`, `eq-diff2/3` — whose arity depends on
     the ontology's `rdf:List` lengths. (`cax-adc` derives `owl:Nothing`, like
     the inconsistency rules `gen_ruleset.py` already omits, so it is skipped.)
  2. The load-time XSD datatype base injected by `crates/owlrl/src/datatypes.rs`
     (dt-type1 / dt-type2).

Without these, RDFox under-derives relative to HornDB on any ontology with
`owl:intersectionOf` / `owl:unionOf` / `owl:propertyChainAxiom` / ... or that
leans on the XSD datatype lattice — which makes a faithful HornDB closure look
like *over-derivation* in the parity gate (see issue #59). This script closes
that gap so the HornDB-vs-RDFox comparison is genuinely apples-to-apples.

It resolves the same `rdf:List` structures `list_rules::resolve` walks and
emits, for RDFox:

  * **Facts** (`--facts-out`, N-Triples): `scm-int` subclass edges
    (`c rdfs:subClassOf member` per intersection member, including blank-node
    restriction members — RDFox preserves bnode labels across imports, so these
    reference the same nodes as the TBox), `eq-diff2/3` `owl:differentFrom`
    pairs, and the constant XSD datatype base.
  * **Rules** (stdout, RDFox Datalog): `cls-int1`, `cls-uni`, and `prp-spo2`,
    but only when every body term is an IRI. A blank node cannot be a constant
    in a Datalog rule body; for the intersection/union members that are
    restriction bnodes, the `scm-int` subclass *fact* plus the already-exported
    `cax-sco` reproduce HornDB's instance closure, so no bnode-bodied rule is
    needed (and HornDB itself has no `cls-svf`, so nothing is ever typed by a
    bare restriction bnode independently).

Full IRIs are written verbatim as `<...>`; the script emits no `@prefix`
header so its output is self-contained and order-independent when concatenated
after `gen_ruleset.py`'s ruleset.

Usage:
    gen_schema_closure.py --tbox TBOX.nt --facts-out FACTS.nt   # rules -> stdout
"""
import argparse
import re
import sys
from pathlib import Path

RDF = "http://www.w3.org/1999/02/22-rdf-syntax-ns#"
RDFS = "http://www.w3.org/2000/01/rdf-schema#"
OWL = "http://www.w3.org/2002/07/owl#"
XSD = "http://www.w3.org/2001/XMLSchema#"

RDF_TYPE = RDF + "type"
RDF_FIRST = RDF + "first"
RDF_REST = RDF + "rest"
RDF_NIL = "<" + RDF + "nil>"
RDFS_SUBCLASSOF = RDFS + "subClassOf"
OWL_INTERSECTION_OF = OWL + "intersectionOf"
OWL_UNION_OF = OWL + "unionOf"
OWL_PROPERTY_CHAIN_AXIOM = OWL + "propertyChainAxiom"
OWL_HAS_KEY = OWL + "hasKey"
OWL_ALL_DIFFERENT = OWL + "AllDifferent"
OWL_MEMBERS = OWL + "members"
OWL_DISTINCT_MEMBERS = OWL + "distinctMembers"
OWL_DIFFERENT_FROM = OWL + "differentFrom"
RDFS_DATATYPE = RDFS + "Datatype"

# Mirror of crates/owlrl/src/datatypes.rs: XSD_DATATYPES + XSD_SUBCLASS_EDGES.
# Kept in lockstep with that file — if the Rust lattice changes, change here.
XSD_DATATYPES = [
    "string", "boolean", "decimal", "integer", "dateTime", "dateTimeStamp",
    "long", "int", "short", "byte", "nonNegativeInteger", "positiveInteger",
    "unsignedLong", "unsignedInt", "unsignedShort", "unsignedByte",
    "nonPositiveInteger", "negativeInteger",
]
XSD_SUBCLASS_EDGES = [
    ("integer", "decimal"),
    ("dateTimeStamp", "dateTime"),
    ("long", "integer"), ("int", "long"), ("short", "int"), ("byte", "short"),
    ("nonNegativeInteger", "integer"),
    ("positiveInteger", "nonNegativeInteger"),
    ("unsignedLong", "nonNegativeInteger"), ("unsignedInt", "unsignedLong"),
    ("unsignedShort", "unsignedInt"), ("unsignedByte", "unsignedShort"),
    ("nonPositiveInteger", "integer"),
    ("negativeInteger", "nonPositiveInteger"),
]

# subject + predicate are <IRI> or _:bnode; object is the rest of the line
# minus the trailing " .". Literals (object only) survive verbatim.
_TRIPLE_RE = re.compile(
    r"^\s*(?P<s><[^>]*>|_:[^\s]+)\s+(?P<p><[^>]*>)\s+(?P<o>.*?)\s*\.\s*$"
)


def parse_ntriples(text):
    """Yield (s, p, o) token strings for each N-Triples line. p is `<IRI>`;
    s/o keep their `<IRI>` / `_:b` / literal forms."""
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        m = _TRIPLE_RE.match(line)
        if m:
            yield m.group("s"), m.group("p"), m.group("o")


def iri(token):
    """Strip the `<>` around an IRI token; None if not an IRI (bnode/literal)."""
    if token.startswith("<") and token.endswith(">"):
        return token[1:-1]
    return None


def is_iri(token):
    return token.startswith("<") and token.endswith(">")


class Tbox:
    """Indexed view of a parsed TBox for list-axiom resolution."""

    def __init__(self, triples):
        self.first = {}   # node token -> object token
        self.rest = {}    # node token -> object token
        self.by_pred = {}  # predicate IRI -> list[(s, o)]
        for s, p, o in triples:
            pi = iri(p)
            if pi == RDF_FIRST:
                self.first[s] = o
            elif pi == RDF_REST:
                self.rest[s] = o
            self.by_pred.setdefault(pi, []).append((s, o))

    def subj_obj(self, pred_iri):
        return self.by_pred.get(pred_iri, [])

    def walk_list(self, head):
        """Resolve an rdf:first/rdf:rest chain to a list of member tokens.
        Returns None on a malformed/cyclic list (mirrors list_rules::walk_list)."""
        out = []
        seen = set()
        cur = head
        while cur != RDF_NIL:
            if cur in seen:
                return None
            seen.add(cur)
            if cur not in self.first or cur not in self.rest:
                return None
            out.append(self.first[cur])
            cur = self.rest[cur]
        return out


def atom(s, p, o):
    return f"[{s}, {p}, {o}]"


def ty(node):
    return atom("?x", f"<{RDF_TYPE}>", node)


def fact(s, p, o):
    return f"{s} <{p}> {o} ."


def generate(tbox):
    """Return (rules_lines, facts_lines) for the resolved list axioms."""
    rules = []
    facts = []
    warnings = []

    # owl:intersectionOf -> scm-int (subclass facts) + cls-int1 (rule).
    for c, head in tbox.subj_obj(OWL_INTERSECTION_OF):
        members = tbox.walk_list(head)
        if not members:
            continue
        # scm-int: c rdfs:subClassOf member, for every member (named or bnode).
        for m in members:
            facts.append(fact(c, RDFS_SUBCLASSOF, m))
        # cls-int1: x:c :- x:m1, ..., x:mn — only if every member is an IRI.
        if all(is_iri(m) for m in members):
            body = ", ".join(ty(m) for m in members)
            rules.append(f"# cls-int1 for {c}")
            rules.append(f"{ty(c)} :- {body} .")

    # owl:unionOf -> cls-uni (one rule per IRI member).
    for c, head in tbox.subj_obj(OWL_UNION_OF):
        members = tbox.walk_list(head)
        if not members:
            continue
        for m in members:
            if is_iri(m):
                rules.append(f"# cls-uni for {c} via {m}")
                rules.append(f"{ty(c)} :- {ty(m)} .")
            else:
                warnings.append(f"cls-uni member is a bnode for {c}; skipped")

    # owl:propertyChainAxiom -> prp-spo2 (chained join rule).
    for p, head in tbox.subj_obj(OWL_PROPERTY_CHAIN_AXIOM):
        chain = tbox.walk_list(head)
        if not chain or not all(is_iri(e) for e in chain):
            if chain:
                warnings.append(f"prp-spo2 chain has a non-IRI element for {p}; skipped")
            continue
        # x0 p xn :- x0 p1 x1, x1 p2 x2, ..., x(n-1) pn xn
        body = ", ".join(
            atom(f"?u{i}", chain[i], f"?u{i + 1}") for i in range(len(chain))
        )
        rules.append(f"# prp-spo2 for {p}")
        rules.append(f"{atom('?u0', p, f'?u{len(chain)}')} :- {body} .")

    # owl:hasKey -> prp-key. Not expressible as a single fixed Datalog rule
    # here; surface it loudly so a future key-bearing ontology is not silently
    # under-derived by the reference (the parity gate would then flag it).
    for c, _ in tbox.subj_obj(OWL_HAS_KEY):
        warnings.append(f"owl:hasKey on {c} not translated (prp-key); parity may diverge")

    # owl:AllDifferent -> eq-diff2/3 (pairwise differentFrom facts).
    all_diff_subjects = [
        s for s, o in tbox.subj_obj(RDF_TYPE) if o == f"<{OWL_ALL_DIFFERENT}>"
    ]
    for ad in all_diff_subjects:
        head = None
        for pred in (OWL_DISTINCT_MEMBERS, OWL_MEMBERS):
            for s, o in tbox.subj_obj(pred):
                if s == ad:
                    head = o
                    break
            if head:
                break
        if not head:
            continue
        members = tbox.walk_list(head)
        if not members or len(members) < 2:
            continue
        for i, xi in enumerate(members):
            for j, xj in enumerate(members):
                if i != j:
                    facts.append(fact(xi, OWL_DIFFERENT_FROM, xj))

    return rules, facts, warnings


def datatype_base_facts():
    """The constant XSD datatype base HornDB injects (datatypes.rs)."""
    facts = []
    for dt in XSD_DATATYPES:
        facts.append(fact(f"<{XSD}{dt}>", RDF_TYPE, f"<{RDFS_DATATYPE}>"))
    for sub, sup in XSD_SUBCLASS_EDGES:
        facts.append(fact(f"<{XSD}{sub}>", RDFS_SUBCLASSOF, f"<{XSD}{sup}>"))
    return facts


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--tbox", type=Path, required=True, help="TBox N-Triples file")
    ap.add_argument("--facts-out", type=Path, required=True,
                    help="where to write the schema-closure + datatype N-Triples")
    args = ap.parse_args(argv)

    tbox = Tbox(parse_ntriples(args.tbox.read_text()))
    rules, facts, warnings = generate(tbox)
    facts = datatype_base_facts() + facts

    args.facts_out.write_text(
        "# GENERATED by scripts/bench/gen_schema_closure.py — do not edit.\n"
        "# List-axiom schema closure + XSD datatype base HornDB derives that\n"
        "# the fixed-arity rules.toml ruleset cannot express. INTERNAL artifact.\n"
        + "\n".join(facts) + ("\n" if facts else "")
    )

    out = []
    out.append("# GENERATED by scripts/bench/gen_schema_closure.py — do not edit.")
    out.append("# List-axiom Datalog rules (cls-int1 / cls-uni / prp-spo2) resolved")
    out.append(f"# from {args.tbox}. INTERNAL benchmarking artifact.")
    out.extend(rules)
    sys.stdout.write("\n".join(out) + "\n")

    for w in warnings:
        print(f"gen_schema_closure: WARNING: {w}", file=sys.stderr)


if __name__ == "__main__":
    main()
