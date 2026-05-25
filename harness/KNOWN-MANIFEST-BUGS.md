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

## Summary (2026-05-25 survey, post `feat/owlrl-cls-com`)

22 of the 115 synthesised entries fail today. They fall into the
following buckets, grouped by the missing capability — not by a single
rule name — because the residue is mostly tests that need *combinations*
of features (datatype subsumption, fresh-bnode generation,
literal-collision inconsistency, ...) the Stage-1 engine intentionally
defers:

| Missing capability | Cases blocked |
|---|---|
| Datatype subsumption (`dt-type1..2`, XSD numeric tower `byte ⊑ short ⊑ int ⊑ ...`) | 5 |
| Fresh-bnode generation of `owl:complementOf` partner classes (`DisjointClasses-001/003-pe`) | 2 |
| `prp-pdw`/`prp-adp` over class- or chain-derived property assertions (`DisjointObjectProperties-001/002-pe`, `DisjointDataProperties-002-pe`) | 3 |
| Annotation-property / `equivalentClass` substitution (`equivalentClass-008-Direct-pe`, `I4.6-003/005-Direct-pe`, `I5.26-010-pe`) | 4 |
| `prp-fp`/`prp-ifp` propagation into `differentFrom` (`fp/ifp-differentFrom-pe`) and `differentFrom` symmetry (`differentFrom-001-pe`) | 3 |
| `prp-key` + functional-property literal disequality (`Keys-006-incons`, needs `dt-not-type`) | 1 |
| Self-chain → `owl:TransitiveProperty` meta-rule (`chain2trans1-pe`) — not in W3C OWL 2 RL | 1 |
| `cls-uni`/`cls-int` requiring engine to *generate* fresh blank-node list classes (`I5.5-005-pe`) | 1 |
| `cls-maxqc1..4` (qualified cardinality, `ObjectQCR-002-pe`) | 1 |
| `owl:imports` external resolution (`imports-011-pe`) | 1 |

Total: **22 cases**.

Three Stage-1 rule batches landed on 2026-05-25 and together flipped 11
cases from red to green:

**`feat/owlrl-inconsistency-rules`** — added `cax-dw`, `prp-irp`,
`prp-asyp`, `prp-pdw`, `prp-npa1`, `prp-npa2`, `eq-diff1`. Flipped:

- `#DisjointClasses-002-incons` (was under `cax-dw`)
- `#New-Feature-AsymmetricProperty-001-incons` (was under `prp-asyp`)
- `#New-Feature-IrreflexiveProperty-001-incons` (was under `prp-irp`)
- `#New-Feature-NegativeDataPropertyAssertion-001-incons` (was under `prp-npa1/2`)
- `#New-Feature-NegativeObjectPropertyAssertion-001-incons` (was under `prp-npa1/2`)
- `#New-Feature-DisjointDataProperties-001-incons` (was under `prp-pdw`)

**`feat/owlrl-sameas-rules`** — added `prp-fp`, `prp-ifp`, `prp-rfp`,
`eq-rep-s`, `eq-rep-p`, `eq-rep-o`. Flipped:

- `#WebOnt-sameAs-001-pe` (was under `prp-fp` + sameAs)

**`feat/owlrl-list-rules`** — added the list-walking rules `prp-spo2`,
`prp-key`, `cls-int1`, `cls-uni`, `cax-adc`, `eq-diff2`/`eq-diff3`, plus
load-time auto-`owl:Thing` inference for `owl:NamedIndividual`s. Flipped:

- `#New-Feature-ObjectPropertyChain-001-pe` (`prp-spo2` two-step chain)
- `#New-Feature-ObjectPropertyChain-BJP-003-pe` (`prp-spo2` two-step chain)
- `#New-Feature-Keys-003-pe` (`prp-key` single-key sameAs derivation)
- `#New-Feature-ReflexiveProperty-001-pe` (load-time auto-Thing + `prp-rfp`)

The `cls-int1`/`cls-uni`/`cax-adc`/`eq-diff2`/`eq-diff3` rules also
landed and have isolated unit-test coverage in `list_rules.rs`, but no
*W3C* test in the synthesised suite is gated by exactly those rules
without also requiring complementOf / datatype subsumption / annotation
substitution / fresh-bnode emission. So the unit tests are the green
gate for those rules in this batch; the W3C wins come from `prp-spo2`,
`prp-key`, and auto-Thing.

**`feat/owlrl-cls-com`** — added `cls-com` (compiled), `scm-int`
(list_rules.rs), and `scm-eqp-rev` (compiled). Flipped:

- `#WebOnt-description-logic-101-incons` (`scm-int` decomposes
  `Unsatisfiable ≡ c ⊓ d`, `cls-com` then fires on `c ⊑ ¬d`)
- `#WebOnt-description-logic-103-incons` (same chain across e3/f)
- `#WebOnt-description-logic-104-incons` (pure `cls-com` over a
  `c ⊑ [complementOf d]` subClassOf chain — no intersection needed)
- `#WebOnt-equivalentProperty-003-pe` (`scm-eqp-rev` derives
  `equivalentProperty` from two-way `subPropertyOf`)

## Cases, grouped by missing capability

### Datatype subsumption (`dt-type1..2`, XSD numeric tower)

The Stage-1 engine has *no* datatype-aware rules. The `WebOnt-I5.8-*`
tests assert that an `rdfs:range` declaration of `xsd:byte` (or
`xsd:short`, ...) entails the same property having a wider XSD range
like `xsd:short`. That requires the engine to know the XSD numeric
hierarchy — Stage-2 work (SPEC-04 risk § "Datatype reasoning").

- `#WebOnt-I5.8-006-pe`
- `#WebOnt-I5.8-008-pe`
- `#WebOnt-I5.8-009-pe`
- `#WebOnt-I5.8-011-pe`
- `#WebOnt-equivalentClass-003-pe` — equivalentClass over a datatype
  expression involving `xsd:byte`.

### Fresh-bnode generation of `owl:complementOf` partner classes

`cls-com` (2026-05-25, `feat/owlrl-cls-com`) closes the
`description-logic-1xx-incons` series, but the two `DisjointClasses-*-pe`
cases below remain red because their *conclusion* graphs assert that the
target individual belongs to a *generated* anonymous class with an
`owl:complementOf` partner. OWL 2 RL does not include existential
fresh-bnode generation (TGDs are explicitly disclaimed in SPEC-04), so
these need Stage-2 work.

- `#DisjointClasses-001-pe` — conclusion is `Stewie a _:X` with
  `_:X owl:complementOf Girl`.
- `#DisjointClasses-003-pe` — same shape over an `AllDisjointClasses`
  premise.

### `prp-pdw`/`prp-adp` over derived property assertions

`prp-pdw` is implemented (2026-05-25); the W3C `*-incons` cases for
explicit data-property disjointness pass. The `-pe` variants below
require the engine to first *derive* the offending pair (via a chain
or class-expression rule), then trigger the disjointness check.

- `#New-Feature-DisjointDataProperties-002-pe`
- `#New-Feature-DisjointObjectProperties-001-pe`
- `#New-Feature-DisjointObjectProperties-002-pe`

### Annotation-property / `equivalentClass` substitution

These tests assert that an annotation triple on an `owl:equivalentClass`
or `owl:sameAs` partner is reflected onto the other partner.
OWL 2 RL does not provide a rule that substitutes annotation
predicates across class equivalence; Stage-2 work.

- `#WebOnt-I4.6-003-pe` — sameAs ⇒ equivalentClass for classes.
- `#WebOnt-I4.6-005-Direct-pe`
- `#WebOnt-I5.26-010-pe`
- `#WebOnt-equivalentClass-008-Direct-pe` — equivalentClass +
  annotation-property substitution.

### `prp-fp`/`prp-ifp` interaction with `differentFrom`

`prp-fp` and `prp-ifp` are implemented (`feat/owlrl-sameas-rules`)
and emit `owl:sameAs` correctly. The W3C cases below require chaining
through to `differentFrom` symmetry / `owl:Nothing` derivation, which
needs additional rules beyond the Stage-1 scope.

- `#WebOnt-differentFrom-001-pe` — needs `differentFrom` symmetry.
- `#owl2-rl-rules-fp-differentFrom-pe`
- `#owl2-rl-rules-ifp-differentFrom-pe`

### `prp-key` + literal disequality (`dt-not-type`)

`prp-key` is implemented in `list_rules.rs` (2026-05-25). The one
remaining `-incons` case requires the engine to know that the
literals `"Peter"` and `"Kichwa-Tembo"` cannot be `owl:sameAs`
(i.e. `dt-not-type` literal-tower disequality) — Stage-2 work.

- `#New-Feature-Keys-006-incons`

### Self-chain → `owl:TransitiveProperty` meta-rule

OWL 2 RL's `prp-spo2` derives chain conclusions on instances but does
not derive `?p rdf:type owl:TransitiveProperty` from a `(p, p)`
self-chain. The `chain2trans1-pe` test expects this meta-derivation,
which is not part of the W3C profile.

- `#chain2trans1-pe`

### `cls-uni`/`cls-int` with fresh-bnode generation

`cls-uni` and `cls-int1` are implemented (`list_rules.rs`) and emit
type-membership conclusions. The W3C case below conversely requires
the engine to *generate* a new blank-node `owl:unionOf` class
expression — out of OWL 2 RL scope (existential generation is the
`tuple-generating-dependency` extension explicitly disclaimed in
SPEC-04).

- `#WebOnt-I5.5-005-pe` — equivalentClass derivation over a
  generated `owl:unionOf`.

### Object qualified cardinality (`cls-maxqc1..4`)

- `#New-Feature-ObjectQCR-002-pe`

### `owl:imports` external resolution

- `#WebOnt-imports-011-pe` — premise references an imported ontology
  that the Stage-1 loader does not fetch.

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
