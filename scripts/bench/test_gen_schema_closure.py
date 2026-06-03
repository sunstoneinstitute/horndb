"""Stdlib unittest for gen_schema_closure.py — no external deps (repo style)."""
import tempfile
import unittest
from pathlib import Path

import gen_schema_closure as g


def tbox_from(text):
    return g.Tbox(g.parse_ntriples(text))


class ParseAndWalk(unittest.TestCase):
    def test_parse_iri_and_bnode_and_literal(self):
        text = (
            "<http://e/s> <http://e/p> <http://e/o> .\n"
            '_:b0 <http://e/q> "lit with spaces" .\n'
        )
        triples = list(g.parse_ntriples(text))
        self.assertEqual(triples[0], ("<http://e/s>", "<http://e/p>", "<http://e/o>"))
        self.assertEqual(triples[1][0], "_:b0")
        self.assertEqual(triples[1][2], '"lit with spaces"')

    def test_walk_list(self):
        f, r = g.RDF_FIRST, g.RDF_REST
        text = (
            f"_:l0 <{f}> <http://e/A> .\n_:l0 <{r}> _:l1 .\n"
            f"_:l1 <{f}> <http://e/B> .\n_:l1 <{r}> {g.RDF_NIL} .\n"
        )
        tb = tbox_from(text)
        self.assertEqual(tb.walk_list("_:l0"), ["<http://e/A>", "<http://e/B>"])

    def test_walk_list_cycle_is_none(self):
        f, r = g.RDF_FIRST, g.RDF_REST
        text = f"_:l0 <{f}> <http://e/A> .\n_:l0 <{r}> _:l0 .\n"
        self.assertIsNone(tbox_from(text).walk_list("_:l0"))


class Intersection(unittest.TestCase):
    def _intersection_tbox(self, members):
        f, r = g.RDF_FIRST, g.RDF_REST
        lines = [f"<http://e/C> <{g.OWL_INTERSECTION_OF}> _:l0 ."]
        for i, m in enumerate(members):
            cell = f"_:l{i}"
            nxt = f"_:l{i + 1}" if i + 1 < len(members) else g.RDF_NIL
            lines.append(f"{cell} <{f}> {m} .")
            lines.append(f"{cell} <{r}> {nxt} .")
        return tbox_from("\n".join(lines) + "\n")

    def test_named_members_emit_scmint_facts_and_clsint1_rule(self):
        tb = self._intersection_tbox(["<http://e/Person>", "<http://e/Employee>"])
        rules, facts, _ = g.generate(tb)
        # scm-int: C ⊑ each member
        self.assertIn(g.fact("<http://e/C>", g.RDFS_SUBCLASSOF, "<http://e/Person>"), facts)
        self.assertIn(g.fact("<http://e/C>", g.RDFS_SUBCLASSOF, "<http://e/Employee>"), facts)
        # cls-int1 rule present (all members named)
        joined = "\n".join(rules)
        self.assertIn(":-", joined)
        self.assertIn("http://e/Person", joined)

    def test_bnode_member_emits_scmint_fact_but_no_clsint1_rule(self):
        tb = self._intersection_tbox(["<http://e/Person>", "_:restriction"])
        rules, facts, _ = g.generate(tb)
        self.assertIn(g.fact("<http://e/C>", g.RDFS_SUBCLASSOF, "_:restriction"), facts)
        # No cls-int1 rule, because a bnode cannot be a Datalog body constant.
        self.assertNotIn(":-", "\n".join(rules))


class DatatypeBase(unittest.TestCase):
    def test_datatype_base_counts(self):
        facts = g.datatype_base_facts()
        self.assertEqual(len(facts), len(g.XSD_DATATYPES) + len(g.XSD_SUBCLASS_EDGES))
        self.assertIn(
            g.fact(f"<{g.XSD}integer>", g.RDFS_SUBCLASSOF, f"<{g.XSD}decimal>"), facts
        )
        self.assertIn(
            g.fact(f"<{g.XSD}byte>", g.RDF_TYPE, f"<{g.RDFS_DATATYPE}>"), facts
        )


class EndToEndMain(unittest.TestCase):
    def test_main_writes_facts_and_rules(self):
        with tempfile.TemporaryDirectory() as d:
            tbox = Path(d) / "tbox.nt"
            tbox.write_text(
                f"<http://e/C> <{g.OWL_INTERSECTION_OF}> _:l0 .\n"
                f"_:l0 <{g.RDF_FIRST}> <http://e/Person> .\n"
                f"_:l0 <{g.RDF_REST}> {g.RDF_NIL} .\n"
            )
            facts_out = Path(d) / "facts.nt"
            import io
            import contextlib
            buf = io.StringIO()
            with contextlib.redirect_stdout(buf):
                g.main(["--tbox", str(tbox), "--facts-out", str(facts_out)])
            facts_text = facts_out.read_text()
            self.assertIn("subClassOf", facts_text)
            self.assertIn(f"{g.XSD}integer", facts_text)  # datatype base present
            self.assertIn("cls-int1", buf.getvalue())


if __name__ == "__main__":
    unittest.main()
