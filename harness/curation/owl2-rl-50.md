# OWL 2 RL Stage-1 Selection (50 cases)

This document records exactly *which* 50 W3C OWL 2 test cases the
Stage-1 selected subset names, and *why* each was picked.

Coverage target: the "most-used rules" from the OWL 2 RL/RDF rule
table (cax-sco, cax-eqc1, cax-eqc2, prp-spo1, prp-spo2, prp-dom,
prp-rng, prp-trp, prp-symp, prp-eqp1, prp-eqp2, scm-sco, scm-spo,
scm-cls, scm-eqc1, scm-eqp1, cls-thing, cls-nothing1, cls-int1,
cls-uni, eq-sym, eq-trans). Each rule must have at least one positive
entailment fixture and, where applicable, one negative entailment
fixture in the selection.

## Selection process

1. After running `scripts/fetch-w3c-suites.sh`, list every test case
   in `crates/harness/data/w3c-owl2-tests/` with profile
   `OWL 2 RL` (filter by `<#profile>`).
2. For each rule above, pick the smallest positive-entailment test
   that exercises the rule in isolation.
3. Where the OWL 2 RL profile has known-broken upstream manifests
   (see `harness/KNOWN-MANIFEST-BUGS.md`), pick the next-smallest.
4. Round to 50 total by adding consistency / inconsistency tests until
   the count is met.

## Acceptance

The selected 50 IDs are listed under `[suites.owl2].include` in
`harness/selected.toml`. Adding or removing entries requires updating
this file *and* the methodology section above.

## Status

The actual 50 IDs are baked in by the Stage-1 implementer after
running the fetch script and the `harness list-cases` discovery
subcommand. Until the upstream archives are fetched in this
environment, `harness/selected.toml` retains the Stage-0 in-tree
fixtures so the local smoke test still proves the harness wiring.
