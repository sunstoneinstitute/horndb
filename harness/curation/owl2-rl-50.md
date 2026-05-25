# OWL 2 RL Stage-1 Selection

This document records *which* OWL 2 RL test cases the Stage-1 selected
subset names, and *why* each was picked.

## Stage-1 reality (today)

The `harness/selected.toml` `[suites.owl2]` block names 18 hand-rolled
fixtures under `crates/harness/tests/fixtures/owl2/`. Each fixture
exercises one Stage-1 rule (`rules.toml`) in isolation. Naming
convention: the fixture filename stem is the rule id, optionally
with `-positive` / `-negative` / a one-word qualifier for tests that
exercise the same rule in a different shape.

Coverage achieved (one positive entailment per rule unless noted):

| Rule          | Fixture                              |
|---------------|--------------------------------------|
| cax-sco       | `subclass-entail`                    |
| cax-eqc1      | `cax-eqc1`                           |
| cax-eqc2      | `cax-eqc2`                           |
| scm-sco       | `scm-sco-transitive`                 |
| scm-spo       | `scm-spo-transitive`                 |
| eq-sym        | `eq-sym`                             |
| eq-trans      | `eq-trans`                           |
| prp-symp      | `prp-symp`                           |
| prp-trp       | `prp-trp`                            |
| prp-inv1      | `prp-inv1`                           |
| prp-inv2      | `prp-inv2`                           |
| prp-spo1      | `prp-spo1`                           |
| prp-eqp1      | `prp-eqp1`                           |
| prp-dom       | `prp-dom`                            |
| prp-rng       | `prp-rng`                            |
| (vacuous)     | `trivial-entail-true`                |
| (negative)    | `negative-subclass-no-instance`      |
| (inconsist.)  | `inconsistent-001` (explicit Nothing)|

Rules not yet covered by a fixture (Stage-1 follow-up):
`eq-ref`, `prp-eqp2`, `cls-avf`, `cls-hv1`, `cls-hv2`, `cls-svf2`,
`scm-cls`, `scm-cls-thing`, `scm-cls-nothing`, `scm-dom1`, `scm-dom2`,
`scm-rng1`, `scm-rng2`, `scm-eqc1`, `scm-eqc2`, `scm-eqp1`,
`scm-eqp2`, `scm-op`.

## W3C reality (2026-05-25)

The four ingestion steps below were completed in a single pass; the
resulting fixtures and manifest live at
`crates/harness/tests/fixtures/owl2-w3c-rl/`, and the green subset is
listed in `harness/selected.toml`'s `[suites.owl2-w3c-rl]` block.

1. **`scripts/fetch-w3c-suites.sh` rewritten.** The dead
   `testOntology-20091022.zip` URL was replaced with the live
   per-profile aggregate at
   `https://www.w3.org/2009/11/owl-test/profile-RL.rdf` (~254 KB,
   91 `test:TestCase`s, all tagged
   `<test:profile rdf:resource="&test;RL"/>`).
2. **DOCTYPE quoting handled inside the extractor.** Rather than
   pre-processing the file on disk or patching oxrdfio, the new
   `crates/harness/src/owl2_rl_extract.rs` substitutes the four
   DOCTYPE-defined entities (`&rdf;`, `&rdfs;`, `&owl;`, `&test;`)
   with their expansions in-memory before parsing with `quick-xml`.
   The XML built-ins (`&lt;` / `&gt;` / `&amp;` / `&quot;` / `&apos;`)
   are left intact and decoded normally.
3. **Embedded ontologies materialised as sibling Turtle files.** The
   new `harness extract-owl2-rl --source --out` subcommand decodes
   each `test:rdfXmlPremiseOntology` / `test:rdfXmlConclusionOntology`
   literal, re-parses it via `oxrdfio` (`RdfFormat::RdfXml`), and
   re-serialises as Turtle to `<id>.premise.ttl` /
   `<id>.conclusion.ttl`. A synthesised `manifest.ttl` is emitted
   alongside, mapping each W3C `test:*Test` rdf:type to the matching
   `mf:*Test` so the existing manifest parser works unchanged. A
   W3C case carrying both `PositiveEntailmentTest` and
   `ConsistencyTest` produces two entries (`-pe` and `-cons`).
4. **Curation: 78 green, 37 red.** The full survey was run against
   `--features real-engine` on 2026-05-25; 78 of the 115 synthesised
   entries pass and are listed in `selected.toml`. The 37 failing
   cases are documented in `harness/KNOWN-MANIFEST-BUGS.md` grouped
   by the missing OWL 2 RL rule that gates each.

### Ingestion totals

| Quantity | Value |
|---|---|
| W3C `test:TestCase` elements scanned | 91 |
| Manifest entries emitted (per applicable `test:*Test` type) | 115 |
| Turtle files written | 115 |
| Cases skipped (no usable payload) | 11 |
| Green at Stage-1 (in `[suites.owl2-w3c-rl]`) | 78 |
| Red at Stage-1 (in `KNOWN-MANIFEST-BUGS.md`) | 37 |

### Green subset composition

Most of the green cases are `ConsistencyTest` flavours that pass
because the Stage-1 inconsistency rules do not fire — a meaningful
result on consistent inputs, but not new rule coverage. The
non-trivial entailment passes are:

- `#WebOnt-equivalentClass-002-pe` — `owl:equivalentClass` propagation.
- `#WebOnt-equivalentProperty-002-pe` — `owl:equivalentProperty` propagation.
- `#WebOnt-Nothing-001-incons` — explicit `owl:Nothing` membership.

The remaining 75 are `ConsistencyTest`s acting as a regression bed: as
new inconsistency rules are added, none of these should flip to
"expected consistent, got inconsistent".

## Re-running the survey

When the Stage-1 rule set widens, the green/red partition will drift.
Re-run the survey to refresh both files:

```bash
./crates/harness/scripts/fetch-w3c-suites.sh
# Then build a selected.toml that names every w3c-owl2-rl id and run:
cargo run -p horndb-harness --bin harness --features real-engine -- \
    --engine owlrl run --selected /tmp/all.toml --allow-failing \
    | tee /tmp/survey.txt
```

See the "Maintenance" section in `harness/KNOWN-MANIFEST-BUGS.md` for
the full recipe.

## Acceptance

The selected IDs are listed under `[suites.owl2].include` (the
hand-rolled rule-coverage subset) and `[suites.owl2-w3c-rl].include`
(the W3C subset) in `harness/selected.toml`. Adding or removing
entries requires updating *this file* and the appropriate section
above.
