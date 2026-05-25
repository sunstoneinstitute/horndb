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

## Aspirational ≥50-case W3C OWL 2 RL subset

The original target was the W3C OWL 2 test suite filtered to the
`OWL 2 RL` profile (≥50 cases). That requires:

1. **Fix `scripts/fetch-w3c-suites.sh`.** The hard-coded URL
   `https://www.w3.org/2009/11/owl-test/testOntology-20091022.zip`
   is 404. The canonical source is the file tree at
   `https://www.w3.org/2009/11/owl-test/`, in particular
   `profile-RL.rdf` (~254 KB, 4919 lines) which carries every
   Profile-RL-tagged test case.
2. **Handle RDF/XML DOCTYPE quoting.** `profile-RL.rdf` uses
   single-quoted `<!ENTITY>` declarations in its DOCTYPE; oxrdfio
   currently rejects these. Either pre-process the file (replace
   single with double quotes) or upstream an oxrdfio fix.
3. **Extract the embedded ontologies.** Each `test:TestCase` carries
   `test:rdfXmlPremiseOntology` and `test:rdfXmlConclusionOntology` as
   escaped RDF/XML *strings*, not file references. The harness's
   existing `manifest.rs` parser expects `mf:action` / `mf:result`
   file URIs. So either:
   - extend the parser with a `W3CEmbedded` test variant that
     re-parses the embedded ontology on every case, or
   - one-shot extract every embedded ontology into a sibling
     `.premise.ttl` / `.conclusion.ttl` and emit a synthesized
     `manifest.ttl` that the existing parser already understands.
   The second is simpler and matches the rest of the harness.
4. **Curate which W3C cases the Stage-1 engine can pass.** The Stage-1
   engine implements 33 rules (rules.toml) but is missing `cax-dw`,
   `prp-irp`, `prp-pdw`, datatype rules, and full bnode-existential
   matching. Manifests touching these will fail. The curation pass
   picks the largest subset that's green against today's engine and
   keeps a `KNOWN-MANIFEST-BUGS.md` (or similar) for failures
   attributable to upstream rather than to us.

Once those four steps are done, `harness/selected.toml` gains a
second OWL2 suite entry (e.g. `[suites.owl2-w3c-rl]`) pointing at the
generated manifest, and this document gets a "W3C reality" section
listing the IDs and their rule coverage.

## Selection process (when W3C is wired)

1. After running `scripts/fetch-w3c-suites.sh`, list every test case
   in `crates/harness/data/w3c-owl2-tests/` with profile `OWL 2 RL`
   (filter by `<#profile>`).
2. For each Stage-1 rule above, pick the smallest positive-entailment
   test that exercises the rule in isolation.
3. Where the OWL 2 RL profile has known-broken upstream manifests
   (see `harness/KNOWN-MANIFEST-BUGS.md`), pick the next-smallest.
4. Round to 50 total by adding consistency / inconsistency tests until
   the count is met.

## Acceptance

The selected IDs are listed under `[suites.owl2].include` in
`harness/selected.toml`. Adding or removing entries requires updating
*this file* and the appropriate section above.
