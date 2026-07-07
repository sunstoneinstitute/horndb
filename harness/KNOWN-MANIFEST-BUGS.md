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

## Summary (2026-06-01 survey, post `task-34-dt-datatype-rules`)

16 of the 115 synthesised entries fail today (down from 22 → 19 after the
#34 datatype-subsumption + `scm-eqc-rev` batch flipped 3 cases green —
`I5.8-006-pe`, `I5.8-011-pe`, `equivalentClass-003-pe` — then → 18 after
#40's `dt-diff` flipped `New-Feature-Keys-006-incons` green, then → 16 after
#160's value-space intersection narrowing flipped `I5.8-008-pe`/`I5.8-009-pe`
green; see the notes below). The RL-reachable remainder is tracked in
[#160](https://github.com/sunstoneinstitute/horndb/issues/160). They fall into
the following buckets, grouped by the missing capability — not by a
single rule name — because the residue is mostly tests that need
*combinations* of features (datatype value-space intersection,
fresh-bnode generation, literal-collision inconsistency, ...) the
Stage-1 engine intentionally defers:

| Missing capability | Cases blocked |
|---|---|
| Fresh-bnode generation of `owl:complementOf` partner classes (`DisjointClasses-001/003-pe`, `ObjectQCR-002-pe`) | 3 |
| `differentFrom`/`AllDifferent` entailment from disjoint properties (`DisjointObjectProperties-001/002-pe`, `DisjointDataProperties-002-pe`) — not an OWL 2 RL rule; `prp-pdw`/`prp-adp` only derive `owl:Nothing` on a *shared* `(u, w)` pair | 3 |
| Annotation-property / `equivalentClass` substitution (`equivalentClass-008-Direct-pe`, `I4.6-003/005-Direct-pe`, `I5.26-010-pe`) | 4 |
| `prp-fp`/`prp-ifp` propagation into `differentFrom` (`fp/ifp-differentFrom-pe`) and `differentFrom` symmetry (`differentFrom-001-pe`) | 3 |
| Self-chain → `owl:TransitiveProperty` meta-rule (`chain2trans1-pe`) — not in W3C OWL 2 RL | 1 |
| `cls-uni`/`cls-int` requiring engine to *generate* fresh blank-node list classes (`I5.5-005-pe`) | 1 |
| `owl:imports` external resolution (`imports-011-pe`) | 1 |

Total: **16 cases**.

> **2026-06-18 — literal-value datatype rules implemented (`dt-eq`/`dt-diff`/`dt-not-type`, issue #40).**
> `New-Feature-Keys-006-incons` flips green and moves into `selected.toml`'s
> `[suites.owl2-w3c-rl]` block: a functional property with two distinct string
> values now collapses via `prp-fp` to `owl:sameAs`, `dt-diff` derives the two
> literals are `owl:differentFrom`, and the compiled `eq-diff1` closes it to
> `owl:Nothing` (inconsistency). See `crates/owlrl/src/datatype_literals.rs` and
> the load-time `inject_datatype_literal_axioms` pass in `integration.rs`.

> **2026-06-16 — unqualified max-cardinality implemented (`cls-maxc1`/`cls-maxc2`, issue #35).**
> No W3C case in the synthesised `owl2-w3c-rl` suite is gated on *unqualified*
> max-cardinality (the only cardinality case, `New-Feature-ObjectQCR-002`, is
> *qualified* — `owl:maxQualifiedCardinality` + `owl:onClass`). So this batch
> adds no `selected.toml` entry; the rules are covered by unit + integration
> tests in `crates/owlrl`. The total above is unchanged. (Update: the qualified
> `cls-maxqc1..4` rules later landed in #36 — see the next note — but
> `ObjectQCR-002-pe` stays red on fresh-bnode `owl:complementOf` generation,
> not on the cardinality rules.)

> **2026-06-16 — qualified max-cardinality implemented (`cls-maxqc1`–`cls-maxqc4`, issue #36).**
> Covered by unit + integration tests in `crates/owlrl`. No `selected.toml`
> entry was added: the only qualified-cardinality W3C case,
> `New-Feature-ObjectQCR-002-pe`, is blocked on fresh-bnode
> `owl:complementOf` generation (a TGD), not on the cardinality rules — its
> conclusion asserts `Stewie a [owl:complementOf Woman]`, which `cls-maxqc1..4`
> cannot emit (they only produce `owl:sameAs`/`owl:Nothing`). It has therefore
> been reclassified into the fresh-bnode `owl:complementOf` bucket above. The
> total above is unchanged at 19.

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

**`task-34-dt-datatype-rules`** — added `dt-type1` (every XSD literal
inhabits its own datatype) plus the `dt-type2` XSD subsumption lattice
(`byte ⊑ short ⊑ int ⊑ long ⊑ integer ⊑ decimal`, and the
`unsignedX`/`nonNegativeInteger`/... arms), injected at load time, and
the `scm-eqc-rev` rule (class analogue of `scm-eqp-rev`: two-way
`rdfs:subClassOf` ⇒ `owl:equivalentClass`). Flipped:

- `#WebOnt-I5.8-006-pe` (`dt-type2` lattice: `xsd:byte` range ⊑ wider
  `xsd:short`)
- `#WebOnt-I5.8-011-pe` (`dt-type2` lattice over the unsigned arm)
- `#WebOnt-equivalentClass-003-pe` (`scm-eqc-rev` — pure two-way
  `rdfs:subClassOf` between `Car`/`Automobile`; no datatype involved)

## Cases, grouped by missing capability

### ~~Datatype value-space intersection (`I5.8-008/009-pe`)~~ — RESOLVED (2026-07-07, `#160`)

`dt-type1` and the `dt-type2` XSD subsumption lattice implemented the
*subsumption* cases `I5.8-006-pe` and `I5.8-011-pe` (`task-34-dt-datatype-rules`).
The two `WebOnt-I5.8-*-pe` cases below are **not** subsumption — they require
value-space *intersection* narrowing, genuine interval reasoning the lattice
alone cannot express:

- `#WebOnt-I5.8-008-pe` — `short ∩ unsignedInt = [0, 32767] ⊆ unsignedShort`.
- `#WebOnt-I5.8-009-pe` — `nonNegativeInteger ∩ nonPositiveInteger =
  {0} ⊆ short`.

Both are now **green** and listed in `selected.toml`. A load-time pass
(`crates/owlrl/src/datatype_ranges.rs`, wired from `integration.rs`) models each
XSD numeric-tower datatype's value space as an integer interval, intersects the
value spaces of a property's ≥2 declared `rdfs:range` datatypes, and asserts
`rdfs:range T` for every datatype `T` whose value space is a **superset** of that
intersection (supersets only ⇒ no false `dt-not-type` inconsistency). Opaque
datatypes (`xsd:string`/`boolean`/`dateTime`/user IRIs) disqualify a property.
`scm-rng1` then propagates the derived narrower range through the fixpoint.

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
- `#New-Feature-ObjectQCR-002-pe` — conclusion asserts
  `Stewie a [owl:complementOf Woman]`, a contrapositive derivation
  requiring a fresh complement class (TGD). `cls-maxqc1..4` are now
  implemented but only emit `owl:sameAs`/`owl:Nothing`, so this case
  stays red on the fresh-bnode gap, not on missing cardinality rules.

### `differentFrom`/`AllDifferent` from disjoint properties

`prp-pdw` (pairwise, `owl:propertyDisjointWith`) and `prp-adp` (list,
`owl:AllDisjointProperties`) are both implemented; the W3C `*-incons` and
`*-cons` cases for explicit property disjointness pass. The `-pe` variants
below are *not* reachable by OWL 2 RL property-disjointness rules: both
`prp-pdw` and `prp-adp` only derive an inconsistency (`owl:Nothing`) when a
*single* individual pair `(u, w)` is related by two disjoint properties
(`u pi w ∧ u pj w`). These cases instead assert `Peter owl:differentFrom
Lois` / an `owl:AllDifferent` list over the *objects* of disjoint-property
assertions on a shared subject (`Stewie hasFather Peter ∧ Stewie hasMother
Lois ⇒ Peter ≠ Lois`). That is an OWL 2 DL entailment, with no
corresponding OWL 2 RL rule — Stage-2/DL territory.

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

### ~~`prp-key` + literal disequality~~ — RESOLVED (2026-06-18, `dt-diff`)

`#New-Feature-Keys-006-incons` is now **green** and listed in
`selected.toml`. `hasName` is a functional property, so `prp-fp`
collapses its two values to `"Peter" owl:sameAs "Kichwa-Tembo"`; the
new `dt-diff` rule (distinct string values ⇒ `owl:differentFrom`)
then lets the compiled `eq-diff1` derive `owl:Nothing`. Implemented in
`crates/owlrl/src/datatype_literals.rs` + `inject_datatype_literal_axioms`
(issue #40).

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
