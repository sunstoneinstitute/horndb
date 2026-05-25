# Known-failing W3C OWL 2 RL cases (Stage-1 engine)

Per SPEC-01's "Risks and open questions" section: some upstream W3C
test cases reference rules the Stage-1 `horndb-owlrl` engine does not
yet implement. This file lists each excluded case with the missing
rule(s) that gate it, so the selection discipline (F11) stays honest
about *why* a W3C case isn't in `harness/selected.toml`.

The cases live in `crates/harness/tests/fixtures/owl2-w3c-rl/`
(synthesised from `https://www.w3.org/2009/11/owl-test/profile-RL.rdf`
by `harness extract-owl2-rl`) and are deliberately *not* listed in
`selected.toml`'s `[suites.owl2-w3c-rl]` block. When a missing rule
lands, the corresponding entries move from this file into
`selected.toml` in the same commit.

See `crates/owlrl/rules.toml` for which rules are implemented, and
`docs/specs/SPEC-04-rules.md` § "Stage-1 scope" for what is intentionally
deferred. The OWL 2 RL rule names follow the W3C
[Profiles document](https://www.w3.org/TR/owl2-profiles/#Reasoning_in_OWL_2_RL_and_RDF_Graphs_using_Rules).

## Summary (2026-05-25 survey)

31 of the 115 synthesised entries fail today. They fall into the
following buckets, ordered by how many cases each missing rule blocks:

| Missing rule (W3C OWL 2 RL) | Cases blocked |
|---|---|
| `prp-spo2` (property chains) | 4 |
| `cax-adc` / cls-expression-aware `cax-dw` (`owl:AllDisjointClasses`, description-logic 10x) | 4 |
| `eq-diff1..3` (`owl:differentFrom` non-identity) | 3 |
| `prp-pdw` / `prp-adp` (disjoint properties via class/chain interactions) | 4 |
| `prp-key` (`owl:hasKey`) | 2 |
| `prp-rfp` (`owl:ReflexiveProperty`) | 1 |
| `cls-maxqc1..4` (qualified cardinality) | 1 |
| `owl:imports` external resolution | 1 |
| `cls-int1` / `cls-uni` / `cls-hv1` interactions | 8 |
| `prp-fp` + `eq-diff1` interaction | 3 |

Total: **31 cases**.

The seven inconsistency-marker rules `cax-dw`, `prp-irp`, `prp-asyp`,
`prp-pdw`, `prp-npa1`, `prp-npa2`, `eq-diff1` were added on
2026-05-25 (branch `feat/owlrl-inconsistency-rules`) and flipped these
six cases from red to green:

- `#DisjointClasses-002-incons` (was under `cax-dw`)
- `#New-Feature-AsymmetricProperty-001-incons` (was under `prp-asyp`)
- `#New-Feature-IrreflexiveProperty-001-incons` (was under `prp-irp`)
- `#New-Feature-NegativeDataPropertyAssertion-001-incons` (was under `prp-npa1/2`)
- `#New-Feature-NegativeObjectPropertyAssertion-001-incons` (was under `prp-npa1/2`)
- `#New-Feature-DisjointDataProperties-001-incons` (was under `prp-pdw`)

The remaining residue under `cax-adc` and `prp-pdw / prp-adp` needs
class-expression / property-chain rules beyond the Stage-1 scope of
this commit.

## Cases, grouped by missing rule

### Property chain (`prp-spo2`)

The Stage-1 engine implements `prp-spo1` (single sub-property step) but
not the OWL 2 RL property-chain rule `prp-spo2`.

- `#chain2trans1-pe` — chain `(p, p) ⇒ p` synthesises transitivity.
- `#New-Feature-ObjectPropertyChain-001-pe`
- `#New-Feature-ObjectPropertyChain-BJP-003-pe`
- `#WebOnt-equivalentProperty-003-pe` — chain composed with property equivalence.

### `cax-adc` / cls-expression-aware `cax-dw` (disjoint classes)

`cax-dw` itself (an individual claimed in two `owl:disjointWith`
classes) is now implemented; see `crates/owlrl/rules.toml` and was
added on 2026-05-25. The remaining failures here either need the
`owl:AllDisjointClasses` n-ary list rule (`cax-adc`) or interact with
class-expression machinery the engine does not yet have.

- `#DisjointClasses-001-pe` — expects `owl:complementOf` entailment.
- `#DisjointClasses-003-pe` — `owl:AllDisjointClasses` ternary.
- `#WebOnt-description-logic-101-incons` — needs DL-style class
  expression unfolding before `cax-dw` can fire.
- `#WebOnt-description-logic-103-incons`

### `eq-diff*` (`owl:differentFrom`)

`eq-diff1` is now implemented (2026-05-25). The cases below still
fail because they require `prp-fp` / `prp-ifp` (functional /
inverse-functional sameAs collapse) which is being added in a sibling
branch, or list-based `eq-diff2`/`eq-diff3` which are out of scope.

- `#WebOnt-differentFrom-001-pe`
- `#owl2-rl-rules-fp-differentFrom-pe` — needs `prp-fp` + `eq-diff1`.
- `#owl2-rl-rules-ifp-differentFrom-pe` — needs `prp-ifp` + `eq-diff1`.

### `prp-pdw` / `prp-adp` (disjoint properties)

`prp-pdw` is now implemented (2026-05-25); the inconsistency case for
`owl:DisjointDataProperties` flipped to green. The remaining cases
need either the n-ary `prp-adp` rule, class-expression rules, or
property-chain composition.

- `#New-Feature-DisjointDataProperties-002-pe`
- `#New-Feature-DisjointObjectProperties-001-pe`
- `#New-Feature-DisjointObjectProperties-002-pe`
- `#WebOnt-description-logic-104-incons` — property-disjointness via chain.

### `prp-key` (`owl:hasKey`)

- `#New-Feature-Keys-003-pe`
- `#New-Feature-Keys-006-incons`

### `prp-rfp` (`owl:ReflexiveProperty`)

- `#New-Feature-ReflexiveProperty-001-pe`

### Object qualified cardinality (`cls-maxqc1..4`)

- `#New-Feature-ObjectQCR-002-pe`

### `owl:imports` external resolution

- `#WebOnt-imports-011-pe` — premise references an imported ontology
  that the Stage-1 loader does not fetch.

### Class-expression rule interactions (`cls-int*` / `cls-uni*` / `cls-hv*`)

These exercise rules the engine implements individually but in
combinations that need additional class-expression machinery:

- `#WebOnt-I4.6-003-pe`
- `#WebOnt-I4.6-005-Direct-pe`
- `#WebOnt-I5.26-010-pe`
- `#WebOnt-I5.5-005-pe` — equivalentClass over `owl:unionOf`.
- `#WebOnt-I5.8-006-pe` — `owl:intersectionOf` member entailment.
- `#WebOnt-I5.8-008-pe`
- `#WebOnt-I5.8-009-pe`
- `#WebOnt-I5.8-011-pe`
- `#WebOnt-equivalentClass-003-pe` — equivalentClass over `owl:hasValue`.
- `#WebOnt-equivalentClass-008-Direct-pe` — equivalentClass + intersectionOf.

### `prp-fp` + sameAs propagation

- `#WebOnt-sameAs-001-pe` — `owl:FunctionalProperty` collapses two
  objects to `sameAs`.

## Maintenance

When the Stage-1 rule set widens, re-run the survey to refresh the
green/red partition:

```bash
./crates/harness/scripts/fetch-w3c-suites.sh
# Build a selected.toml that names every w3c-owl2-rl id (the
# extractor's manifest is the canonical id list):
grep -oE '<#[A-Za-z0-9._-]+>' crates/harness/tests/fixtures/owl2-w3c-rl/manifest.ttl \
    | grep -v '#manifest' | sed 's/<#/    "#/' | sed 's/>$/",/' > /tmp/all_ids.txt
# (Wrap with a version + [suites.owl2-w3c-rl] block — see
#  harness/selected.toml for the template.)
cargo run -p horndb-harness --bin harness --features real-engine -- \
    --engine owlrl run --selected /tmp/all.toml --allow-failing \
    | tee /tmp/survey.txt
```

Then move each newly-passing id from the lists above into
`harness/selected.toml`'s `[suites.owl2-w3c-rl]` `include` block and
delete it from this file. Both files must move in the same commit.
