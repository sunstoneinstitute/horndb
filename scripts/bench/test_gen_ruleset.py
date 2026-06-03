"""Stdlib unittest for gen_ruleset.py — no external deps (matches repo style)."""
import unittest
from pathlib import Path

import gen_ruleset as g

REPO_ROOT = Path(__file__).resolve().parents[2]
RULES_TOML = REPO_ROOT / "crates" / "owlrl" / "rules.toml"


class TermAndAtom(unittest.TestCase):
    def test_variable_passthrough(self):
        self.assertEqual(g.term("?x"), "?x")

    def test_prefixed_name_passthrough(self):
        self.assertEqual(g.term("rdf:type"), "rdf:type")

    def test_atom_format(self):
        a = g.atom({"s": "?x", "p": "rdf:type", "o": "owl:Thing"})
        self.assertEqual(a, "[?x, rdf:type, owl:Thing]")

    def test_rule_to_dlog_two_body(self):
        rule = {
            "id": "cax-sco",
            "body": [
                {"s": "?c1", "p": "rdfs:subClassOf", "o": "?c2"},
                {"s": "?x", "p": "rdf:type", "o": "?c1"},
            ],
            "head": {"s": "?x", "p": "rdf:type", "o": "?c2"},
        }
        self.assertEqual(
            g.rule_to_dlog(rule),
            "[?x, rdf:type, ?c2] :- [?c1, rdfs:subClassOf, ?c2], [?x, rdf:type, ?c1] .",
        )


class Classification(unittest.TestCase):
    def test_inconsistency_rule_excluded(self):
        rule = {"id": "cax-dw", "body": [{"s": "?x", "p": "rdf:type", "o": "?c1"}],
                "head": {"s": "?x", "p": "rdf:type", "o": "owl:Nothing"}}
        self.assertFalse(g.included(rule))

    def test_empty_body_rule_excluded(self):
        rule = {"id": "eq-ref", "body": [],
                "head": {"s": "?s", "p": "owl:sameAs", "o": "?s"}}
        self.assertFalse(g.included(rule))

    def test_sameas_deriving_rule_included(self):
        rule = {"id": "prp-fp",
                "body": [{"s": "?p", "p": "rdf:type", "o": "owl:FunctionalProperty"},
                         {"s": "?x", "p": "?p", "o": "?y1"},
                         {"s": "?x", "p": "?p", "o": "?y2"}],
                "head": {"s": "?y1", "p": "owl:sameAs", "o": "?y2"}}
        self.assertTrue(g.included(rule))


class FullGeneration(unittest.TestCase):
    def setUp(self):
        self.rules = g.load_rules(RULES_TOML)
        self.text = g.generate(self.rules)

    def test_includes_cax_sco(self):
        self.assertIn("rdfs:subClassOf", self.text)
        self.assertTrue(any(r["id"] == "cax-sco" for r in self.rules))

    def test_omits_inconsistency_and_lists_them(self):
        # Inconsistency rules derive [?x, rdf:type, owl:Nothing] — that head
        # pattern must not appear in any emitted rule. The rule scm-cls-nothing
        # legitimately has owl:Nothing as a *subject* and must be kept.
        for line in self.text.splitlines():
            if line.strip().startswith("[") and ":-" in line:
                self.assertNotRegex(line, r"rdf:type,\s*owl:Nothing\]")
        self.assertIn("cax-dw", self.text)
        self.assertIn("OMITTED", self.text)

    def test_omits_eq_ref(self):
        for line in self.text.splitlines():
            if line.strip().startswith("[") and ":-" in line:
                self.assertNotRegex(line, r":-\s*\.")

    def test_has_prefix_header(self):
        self.assertIn("@prefix rdf:", self.text)
        self.assertIn("@prefix owl:", self.text)

    def test_delegate_closure_rule_included(self):
        # delegate="closure" rules (e.g. eq-trans) are additive and must appear:
        # HornDB does them via GraphBLAS, RDFox via native recursion.
        # Only included rules (non-empty body, no inconsistency head) matter here.
        ids = [r["id"] for r in self.rules
               if r.get("delegate") == "closure" and g.included(r)]
        self.assertTrue(ids, "expected at least one included delegate=closure rule in rules.toml")
        for cid in ids:
            self.assertIn(f"# {cid}", self.text)


if __name__ == "__main__":
    unittest.main()
