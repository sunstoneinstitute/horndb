# SSSOM mappings conformance subset (SPEC-11)

The mappings analogue of `owl2-rl-50.md`. Each fixture is one positive
entailment that isolates a SPEC-11 §F3/§F4 rule family, plus an
identity-isolation negative case (ADR-0017) and one end-to-end case loaded
from a real SSSOM/TSV slice via the §F9 harness loader.

| Fixture id | Exercises | SPEC-11 ref |
|---|---|---|
| `#sssom-rg1-exact` | owl:equivalentClass ⟹ skos:exactMatch | F3 RG1 |
| `#sssom-rg2-broad` | rdfs:subClassOf ⟹ skos:broadMatch | F3 RG2 |
| `#sssom-ri-narrow-broad` | narrowMatch ⟹ inverse broadMatch | F3 RI1/2 |
| `#sssom-rce1-broad` | exactMatch ∘ broadMatch ⟹ broadMatch | F3 RCE1 |
| `#sssom-t1-broad` | broadMatch transitivity | F3 T1 |
| `#sssom-neg-exact` | exactMatch ∘ Not ⟹ Not | F4 |
| `#sssom-mondo-slice` | real slice loads + chains | F9, acceptance #1 |

## SSSOM/TSV slice provenance

Source of the real slice: **SYNTHETIC STAND-IN** — `mondo-slice.sssom.tsv`
is a hand-authored ~20-row TSV representative of the Biomappings/Mondo
SSSOM format. It is NOT derived from a real Biomappings or Mondo release.

This stand-in was created because network access was unavailable during
fixture authoring. The file uses realistic CURIE prefixes (MONDO, HP,
UBERON, skos) and includes a chainable pair (MONDO→HP exactMatch,
HP→UBERON broadMatch) that exercises the §F9 loader + RCE1-broad rule.

**TASKS.md follow-up required:** Replace `mondo-slice.sssom.tsv` with a
genuine vendored slice from Biomappings (https://github.com/cthoyt/biomappings)
or Mondo (https://github.com/monarch-initiative/mondo). Record the source URL,
git commit SHA, licence (CC0 for Biomappings; CC BY 4.0 for Mondo), and
trim to ≤ a few hundred rows that include at least one chainable pair. Update
this section with the provenance once the real slice is vendored.

Keep the slice tiny (≤ a few hundred rows) — it is a correctness fixture, not
a benchmark corpus (benches run on hornbench per the project rule).
