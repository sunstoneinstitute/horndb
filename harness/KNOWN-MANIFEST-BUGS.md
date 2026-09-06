> This file has four parts: the **OWL 2 RL** entailment cases the Stage-1
> reasoner does not cover (below), the **SPARQL query** cases the
> SPEC-07/SPEC-28 engine does not cover (middle), the root-cause triage of
> the **W3C SPARQL 1.1 evaluation suite** (`sparql11-eval`), and the
> **Graph Store Protocol suite** (`sparql11-gsp`, at the end). All
> follow the same rule — a W3C case that is not passing must be listed here
> with the specific missing capability that gates it, whether it is left out
> of `harness/selected.toml` or (for `sparql11-eval`) still selected and
> carried in that suite's `expected_failures` allowlist.

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

15 of the 115 synthesised entries fail today (down from 22 → 19 after the
#34 datatype-subsumption + `scm-eqc-rev` batch flipped 3 cases green —
`I5.8-006-pe`, `I5.8-011-pe`, `equivalentClass-003-pe` — then → 18 after
#40's `dt-diff` flipped `New-Feature-Keys-006-incons` green, then → 16 after
#160's value-space intersection narrowing flipped `I5.8-008-pe`/`I5.8-009-pe`
green, then → 15 after #160's hermetic `owl:imports` resolution flipped
`imports-011-pe` green; see the notes below). With both RL-reachable halves of
[#160](https://github.com/sunstoneinstitute/horndb/issues/160) landed, the
remaining 15 are all intentional Stage-1 non-goals. They fall into
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

Total: **15 cases**.

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

> **2026-07-07 — hermetic `owl:imports` resolution (issue #160).**
> `imports-011-pe` flips green and moves into `selected.toml`'s
> `[suites.owl2-w3c-rl]` block. The harness resolves a premise's
> `owl:imports <IRI>` against a checked-in catalog
> (`crates/harness/tests/fixtures/owl2-w3c-rl/imports-catalog.toml`) that maps
> each import IRI to a mirrored Turtle fixture, merging the imported ontology's
> triples (transitively) before the engine loads the premise — no network, so
> the suite stays offline/deterministic. See `crates/harness/src/rdf.rs`
> (`load_premise`/`expand_imports`). Adding a new imported case = drop the
> support ontology in the fixtures dir + one catalog line.

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

# Known-failing W3C SPARQL query cases

The SPARQL query-evaluation gate is `harness/selected.toml`'s
`[sparql_query]` section, run by `crates/sparql/tests/w3c_suite.rs` against
both backends (`MemStore` and `HornBackend`). SPEC-28 phase 3 (#266) added
the W3C SPARQL 1.0 `graph/` and `dataset/` families to it. Fixtures for
**every** case of both families — selected or not — are checked in under
`crates/harness/tests/fixtures/sparql11/selected_subset/`, mirrored from
`https://w3c.github.io/rdf-tests/sparql/sparql10/` (re-fetch allowlist and
mirror rules: `crates/harness/scripts/fetch-w3c-suites.sh`).

29 cases exist upstream (17 `graph/`, 12 `dataset/`); 24 are selected. The
5 below are not. When one is fixed, move it into `selected.toml` and delete
its entry here, in the same commit.

**What these 24 cases do not grade: the shipping `union` default-graph
mode.** The `graph/` family takes its dataset from the upstream manifest, so
those cases run in `strict`; every `dataset/` query carries its own `FROM` /
`FROM NAMED`, which fixes the dataset whatever the mode is. No W3C case here
exercises D2's *default*. That mode is covered by `crates/sparql/tests/
graph_query.rs` (`union_mode_unqualified_sees_all_non_reserved_deduped`,
`reserved_graph_excluded_from_union`, and the `GRAPH ?g` cases), not by
conformance — do not read the headline count as covering it.

## Blank nodes in the expected result (3 cases)

`w3c_suite.rs::assert_select_equal` diffs solutions as a multiset of
**literal** terms. It has no blank-node isomorphism matching, so a result
row binding a blank node can never match: the upstream result file's label
and whatever label the engine mints are different strings, and both are
equally correct RDF. Fixing this means matching expected against actual up
to a blank-node bijection, in the runner — not in the engine.

- `graph-11` — `{ ?s ?p ?o } UNION { GRAPH ?g { ?s ?p ?o } }` over
  `data-g3`/`data-g4`, whose subjects are blank nodes; 3 of the 8 expected
  rows bind one.
- `dataset-11` — same query and same 3 rows, with the dataset given by
  `FROM` / `FROM NAMED` instead of the manifest.
- `dataset-12b` — the four-`FROM`, four-`FROM NAMED` variant; 6 of 12
  expected rows bind a blank node.

A second, backend-level gate applies to the same three cases on the
`MemStore` leg: `MemStore` keeps every term as a bare lexical string
(`exec/mem.rs::term_to_lex`), so a blank node is indistinguishable from an
IRI on the way out and the JSON results report it as `"type": "uri"`.

## ~~An empty group inside a ground `GRAPH <g>` does not test the graph~~ — FIXED

`graph-exist` and `graph-not-exist` are now **green** and listed in
`selected.toml`. Both backends took a zero-pattern shortcut that emitted the
unit row before consulting the scope, so `ASK { GRAPH <g> {} }` — the
standard graph-existence probe — answered `true` for every IRI, at HTTP 200.
`graph-not-exist` failed on it; `graph-exist` passed for the wrong reason.
Both now go through `ScanScope::ground_graph`: an empty group matches only
when the scope is the default graph, or a ground `GRAPH <g>` whose `g`
survives the `FROM NAMED` filter and holds at least one quad. Direct pin:
`empty_group_probes_graph_existence` in `crates/sparql/tests/graph_query.rs`
(the W3C fixtures alone would not hold it — `graph-exist` passes either way).

## The graph variable is in scope inside the `GRAPH` block (2 cases)

SPARQL 1.1 §18.2.2.2 evaluates `GRAPH ?g { P }` as `Graph(?g, eval(P))`:
`P` is evaluated **first**, with `?g` free, and only then does `Graph`
bind `?g` to each graph name and drop rows where `P` already bound `?g` to
something else. HornDB carries the graph scope as a column on each scan
leaf inside the block (SPEC-28 D5/D6), so `?g` is bound *before* anything
above the leaf runs. For a `P` that merely mentions `?g` in a pattern the
two agree (the column joins by equality — that is `graph-variable-join`,
which is selected and green). They diverge when `P` tests or optionally
binds `?g`. Closing the gap means evaluating the whole block per graph, not
just its scan leaves — the machinery SPEC-28 phase 3 deliberately did not
build.

Both cases are **refused**, not answered — HornDB returns an "unsupported
algebra construct" error naming the construct and §18.2.2.2. They still fail
the manifest (a refusal is not the expected result set), but they fail
honestly, which is what SPEC-28 D1 requires.

- `graph-variable-scope` — `GRAPH ?g { FILTER (BOUND(?g)) }`. The filter must
  see `?g` unbound and reject, giving 0 rows. Leaf-binding puts the filter
  above a scan that already bound `?g`, which used to return one row per
  named graph (2). Now refused: *"a FILTER that references ?g inside
  GRAPH ?g"*.
- `graph-optional` — `GRAPH ?g { ?s ?p ?o OPTIONAL { ?s ?p ?g } }`. The `?g`
  inside the OPTIONAL is a free variable of the inner group, so the OPTIONAL
  matches on the object and `Graph` then keeps only the rows where that
  object *is* the graph name (1 row). Leaf-binding scopes the OPTIONAL's own
  scan instead, changing both what the OPTIONAL matches and which left rows
  survive; that used to return 4 rows. Now refused: *"an OPTIONAL that
  references ?g inside GRAPH ?g"*.

The refusal rule (`plan::lower::per_graph_var_divergence`) allows `?g` only
where the data supplies it and an inner join combines it — a triple-pattern
position or a `VALUES` column, joined upward through `Join`, `Union`, or an
`OPTIONAL`'s left arm. That is exactly the case where "the leaf keeps rows
whose `?g` equals this graph" *is* the post-join, and it is why
`graph-variable-join` stays selected and green. (For a `VALUES`-supplied `?g`
only the `Join` case reaches this rule: `plan::lower::per_graph_barrier` runs
first and refuses a `VALUES` arm of a `Union` or of an `OPTIONAL`'s left side,
because neither carries the graph column up.) Every other use — any expression
(`FILTER`, `BIND`, an `OPTIONAL` condition, `ORDER BY`), a `BIND` *to* `?g`,
or any mention inside an `OPTIONAL`'s right arm — refuses.

Lifting either refusal needs the graph variable joined **after** the block is
evaluated rather than bound on the scan leaf: evaluate `P` per graph with
`?g` free, then join `{?g → thatGraph}`. That is the per-graph block
evaluation SPEC-28 phase 3 deliberately did not build (D5/D6 chose the scan
column), so it is a design change, not a bug fix. That design is SPEC-28's S3
amendment (HDB-171, the `PerGraph` node); HDB-74 implements it and moves both
cases into `selected.toml`.

# Known-failing W3C SPARQL Update cases

The SPARQL Update-evaluation gate is `harness/selected.toml`'s `[sparql_update]`
section, run by `crates/sparql/tests/w3c_update_suite.rs` against both backends
(`MemStore` and `HornBackend`). SPEC-28 phase 4 (#267) added the W3C SPARQL 1.1
`add/`, `copy/`, `move/`, `clear/`, `drop/`, and `delete-insert/` families to
it. Each case is mirrored into
`crates/harness/tests/fixtures/sparql11/update_subset/<case>/` as `data.trig`
(initial state), `request.ru` (the update), and `expected.trig` (expected final
state); the runner loads the initial state, applies the update, and asserts
**quad-set equality** of the resulting store against the expected state (mirror
rules and the re-fetch allowlist: `crates/harness/scripts/fetch-w3c-suites.sh`).

Of the **36** `UpdateEvaluationTest` cases across those six families, **33** are
selected. The 3 left out are all in `clear/`, all for the same reason (D11). The
`delete-insert/` family additionally has 8 `NegativeSyntaxTest11` entries that
are *not* evaluation tests — they are graded by the `sparql11-syntax` suite kind
(spargebra accept/reject), not here, so they are out of scope for this runner
rather than "excluded".

Two cases from a seventh family, `delete/`, were mirrored later and are also
selected: `dawg-delete-with-02` and `dawg-delete-with-06` (fixture dirs
`delete-with-02` / `delete-with-06`, named after their `.ru`). They pin SPARQL
1.1 Update §3.1.2 — a bare `WITH <g>` sets only the *default* graph, so a ground
`GRAPH <other>` inside WHERE still reads `<other>`
([#281](https://github.com/sunstoneinstitute/horndb/issues/281)). The remaining
17 `delete/` cases are simply **not mirrored yet** — no known failure, nothing
excluded on grading grounds; mirror them when the family is next grown.

## Empty-but-existing named graphs under D11 (3 `clear/` cases)

SPEC-28 **D11**: a named graph exists iff it holds at least one visible quad —
there is no empty-graph registry, so clearing a graph to zero quads makes it
*cease to exist*. The runner's final-state check is quad-set equality (an
emptied graph contributes no quads and is indistinguishable from an absent one),
which is the same D11 view the engine itself takes.

The three `clear/` cases below have an expected final state that keeps a named
graph **empty but still existing**. Under D11 the engine instead drops the
emptied graph, so the two states differ in graph *existence* but **not** in
quads — quad-set equality would report them equal and pass them *for the wrong
reason* (a silent false-green: the very thing these cases are designed to
probe). They are therefore excluded rather than selected. Each was confirmed to
"pass" quad-set equality today, so the exclusion is about faithful grading, not
an engine failure:

- `clear-graph-01` — `CLEAR GRAPH :g1`; expected keeps `:g1` as an empty graph.
- `clear-named-01` — `CLEAR NAMED`; expected keeps `:g1` and `:g2` as empty
  graphs.
- `clear-all-01` — `CLEAR ALL`; expected keeps `:g1` and `:g2` as empty graphs.

`clear-default-01` (`CLEAR DEFAULT`) **is** selected: the default graph always
exists regardless of D11, so emptying it is graded faithfully. All four `drop/`
cases are selected — `DROP` removes graphs, which is exactly what D11 does, so
they grade faithfully.

**Count judged (SPEC-28 risk clause):** 3 of 36 evaluation cases, all one
edge (empty-graph existence in `clear/`), with 33 selected and green on both
backends. This is a handful of edge cases, **not** a material fraction — D11 is
not costing real conformance here — so no escalation to epic #261's
explicit-existence-set fallback is warranted. If a later family (or a re-fetch)
pushes the empty-graph-existence exclusions materially higher, revisit #261
before building further on D11.

To make any of these three gradable, the runner would need a graph-existence
set compared alongside the quad set, **and** the fixture format would need to
represent an empty-but-existing graph (a `GRAPH <g> {}` block parses to zero
quads and vanishes) — i.e. carry the expected graph set out of band. That is a
runner + fixture change, gated on the #261 decision, not a bug fix.

# Known-failing W3C SPARQL 1.1 *evaluation* cases (`sparql11-eval`, HDB-128)

`[suites.sparql11-eval]` in `harness/selected.toml` grades the **whole**
upstream SPARQL 1.1 evaluation manifest tree (`include = ["*"]`) — 547 cases,
read in place from the fetched corpus under `crates/harness/data/`. Nothing is
deselected: SPEC-00's harness-first rule forbids narrowing a suite to make a run
look better.

Measured on 2026-09-05 with `--engine owlrl`: **401 pass, 106 fail, 40 skip**.
The 40 skips are test types the harness does not grade at all
(`mf:ProtocolTest`, `mf:ServiceDescriptionTest`, `mf:CSVResultFormatTest`); they
report with the type IRI in the reason. Which task fixed what is in the git log
and in the per-root-cause tables below, not restated here — every branch that
moved these numbers used to conflict on this paragraph.

The 106 reds are listed one-by-one in `expected_failures` in
`harness/selected.toml`, grouped by the same root causes as below. That list is
an **allowlist, not an exclusion**: a listed case is still selected and still
executed; a failure becomes a Skip carrying its reason, and a listed case that
*passes* is reported as a **FAILURE** telling you to drop the line. So the list
cannot rot, and CI catches regressions in both directions.

## Engine gaps

| # | Root cause | Where |
|--:|---|---|
| 38 | **Entailment regimes** (RDF/RDFS/OWL-RL/OWL-Direct/RIF). The engine answers under simple entailment; `sd:entailmentRegime` on the manifest entry is not read. 28 of the 66 `entailment/` cases pass anyway — their answer does not need the regime. | `entailment/` |
| 25 | **Unimplemented builtins**: `BNODE`, `IRI`, `ENCODE_FOR_URI`, `MD5`, `SHA1/256/512`, `STRDT`, `STRLANG`, `UUID`, `STRUUID`, `RAND`, `NOW`, `TZ`, `TIMEZONE`, and the `xsd:` constructor call form. | `functions/`, `aggregates/agg-err-02` |
| 14 | **`EXISTS` / `NOT EXISTS` as a FILTER *expression*.** The pattern form used in `negation/` (i.e. `MINUS`) works (HDB-133); the expression form does not translate. Includes 4 `negation/` cases whose `MINUS` right-hand pattern itself contains a `FILTER NOT EXISTS`. | `exists/`, `negation/`, `subquery/subquery10` |
| 7 | **`SERVICE` (federated query).** No federation client — a SPEC-07 non-goal so far. | `service/` |
| 6 | **Sub-`SELECT` or property path nested inside `GRAPH ?g`** (SPEC-28 S3). | `subquery/`, `property-path/pp35` |
| 1 | **A comparison operator returns a value where §17.4 requires an expression error**, so `IF` takes the wrong branch and the variable stays bound instead of dropping out. | `functions/if02` |
| 1 | **Property-path evaluation:** `pp16` returns 13 of the 15 expected rows. | `property-path/pp16` |

## Harness gaps (grading, not the engine)

| # | Root cause | Where |
|--:|---|---|
| 9 | The runner grades `.srx` / `.srj` results only. **CONSTRUCT graph results** (`.ttl`, needing blank-node-isomorphic graph comparison) and **`.csv`/`.tsv`** serialisations are not graded yet; they report `result format not graded yet: …`. | `construct/`, `csv-tsv-res/`, `subquery/subquery12`, `subquery14` |
| 4 | **Blank-node labels are compared literally.** Grading these needs a bijection between the answer's and the expected result's blank nodes (SPARQL 1.1 result-set isomorphism). `plus-1`/`plus-2` differ *only* in a blank node's label (`_:b` vs `_:b0`); every other cell of every row matches. | `json-res/jsonres01`, `jsonres02`, `functions/plus-1`, `plus-2` |
| 1 | Upstream `.srx` head quirk: the expected header omits a projected variable that is unbound in every row, so the variable *sets* differ even though the rows match. | `aggregates/agg-empty-group` |

## ~~`INSERT { GRAPH :g2 … } WHERE { GRAPH :g1 … }` never reaches its `DROP`~~ — FIXED (HDB-137)

Four `basic-update/` cases (`insert-05a`, `insert-data-same-bnode`,
`insert-where-same-bnode`, `insert-where-same-bnode2`). The cross-graph copy
itself always worked; two other bugs stopped the requests:

1. The multi-operation atomicity preflight judged D11 graph existence for
   *every* operation against the pre-request store, so `DROP GRAPH :g2` was
   rejected before the earlier `INSERT` that creates `:g2` had run. Existence
   is now preflighted for the first operation only; later ones are checked at
   apply time, where the rollback journal already covers a failure.
2. A blank node in an `INSERT` template was scoped to the solution row but not
   to the operation, so `_:b` written by two operations of one request landed
   on the same node. It now also carries a per-operation tag.

---

# Known-failing W3C Graph Store Protocol cases (`sparql11-gsp`, HDB-165)

`[suites.sparql11-gsp]` grades the whole upstream `graph-store-protocol/`
manifest tree (`include = ["*"]`) — 13 cases, each an ordered sequence of HTTP
requests run against a live server the harness boots on a bound port
(`crates/harness/src/gsp.rs`).

Measured on 2026-09-06 with `--engine owlrl`: **7 pass, 0 fail, 6 skip**. The
6 skips are the `expected_failures` below.

Every one is a **deliberate divergence named by SPEC-28 S5**, not a gap to
close. The protocol leaves each of these optional, and S5 chose not to
implement them; do not "fix" the server to pass one of these cases without
first changing S5.

| # | Why it cannot pass | Cases |
|--:|---|---|
| 4 | **Direct graph identification** (`mf:DirectGraphIdentification`) — the graph is named by the request path, e.g. `PUT /gsp/person/1.ttl`. HornDB's `/graphs` route names the graph only with `?graph=<iri>` or `?default` (indirect identification), so these 404 on the first request. | `put_get_repeat_direct`, `put_delete_get_delete_direct`, `post_get_post_get_direct`, `head_existing_direct` |
| 1 | **`multipart/form-data` request body.** S5 accepts the two triples formats (`text/turtle`, `application/n-triples`) and answers 415 otherwise. The case's first POST/GET pair passes; it fails on the third request. | `post_get_post_get_indirect` |
| 1 | **`mf:POSTGraphCreation`** — POST to the bare endpoint, server mints a graph IRI and returns it in `Location`. S5 requires every request to name its target graph, so this is a 400. | `post_get_new_graph` |

## Note on the corpus

SPEC-28 names the upstream `http-rdf-update/` directory. That directory holds
no machine-readable tests: the 2012 tarball ships a prose draft (`tests.txt`),
and the maintained mirror's `manifest.ttl` marks every case `dawg:Deprecated`
with its request/response written out inside a Markdown `rdfs:comment`, saying
to use `../graph-store-protocol/` instead. That is the corpus this suite
fetches. It keeps the old `http-rdf-update/manifest#` case IRIs, which is why
the ids above still read that way.
