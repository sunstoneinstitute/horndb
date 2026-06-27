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
| `#sssom-biomappings-slice` | real slice loads + chains | F9, acceptance #1 |

## SSSOM/TSV slice provenance

`biomappings-slice.sssom.tsv` is a **real**, curated excerpt of the public
Biomappings SSSOM export — a genuine community mapping set, not a synthetic
stand-in.

- **Source:** Biomappings — <https://github.com/biopragmatics/biomappings>
- **File:** `biomappings.sssom.tsv`, fetched via the canonical PURL
  <https://w3id.org/biopragmatics/biomappings/sssom/biomappings.sssom.tsv>
- **Licence:** CC0 1.0 (public domain) — carried in the slice's own header.
- **Curation:** the first ~120 data rows of the export that use only `skos:*`
  mapping predicates, with prefix-resolvable subjects/objects, plus the slice's
  original commented-YAML `curie_map` (trimmed to the 8 prefixes the slice
  actually references). Non-`skos:` predicate rows (e.g. `RO:*`, `debio:*`) were
  dropped so every emitted triple is a clean mapping edge.

The slice is anchored on a **real transitive `skos:exactMatch` chain** present
in the export:

```
APOLLO_SV:00000142  →  IDOMAL:0001040  →  VO:0000002
```

The §F9 loader ingests it and the T1-exact closure derives
`APOLLO_SV:00000142 skos:exactMatch VO:0000002`, asserted in
`biomappings-slice-conclusion.ttl`.

To refresh the slice, re-fetch the PURL above and re-run the same curation
(first ~120 `skos:*` rows + the chain rows + a trimmed `curie_map`). Keep it
tiny (≤ a few hundred rows) — it is a correctness fixture, not a benchmark
corpus (benches run on hornbench per the project rule).
