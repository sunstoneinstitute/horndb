# ADR-0022: Which SPARQL entailment regimes HornDB claims

**Status:** Proposed. This record asks Stig (CTO) for one decision: which entailment regimes HornDB claims, out of the options below. Which regimes a SPARQL endpoint advertises is an externally-visible product claim, so nothing here is accepted or implemented. In particular, no `expected_failures` line has been moved, `harness/KNOWN-MANIFEST-BUGS.md` marks nothing as deselected, and SPEC-07 acceptance criterion 2 is unchanged.

**Date:** 2026-09-19

**Source:** Written for Worklode task HDB-138 (follow-up from HDB-128, which wired up the full W3C SPARQL 1.1 evaluation suite). Bears on SPEC-07 acceptance criterion 2; constrained by ADR-0001 (OWL 2 RL, not OWL 2 DL).

## Context

### What SPEC-07 currently promises

SPEC-07 acceptance criterion 2, verbatim:

> 2. 100% pass on W3C SPARQL 1.1 Entailment Regimes test suite, OWL 2 RL/RDF regime.
>
>    Criteria 1 and 2 are graded by the `sparql11-eval` suite in
>    `harness/selected.toml` — the whole upstream query + update evaluation
>    manifest tree (547 cases, `include = ["*"]`), run by CI's conformance job.
>    Both criteria are met when that suite's `expected_failures` allowlist is
>    empty. […] the 38 `entailment/` reds are what gates criterion 2 specifically.

Three separate facts below make that criterion unreachable as written. It is not a matter of effort.

### What the engine actually does today

The task this ADR answers says "the engine only offers simple entailment" and that "RDFS/OWL 2 RL regimes map onto the existing materialization". Both were checked against the code. The first is right. The second is half right, and the half that is wrong is the point of this ADR.

**Query-time entailment does not exist.** `crates/sparql/src/regime/` defines an `EntailmentRegime` trait with two implementations, `SimpleRegime` and `MaterializedOwlRlRegime`. The trait has exactly one method, `name(&self) -> &'static str`. Nothing outside the module and its own test file references either type: `SparqlConfig` has no regime field, `api::execute_query_with` never consults one, and `exec::horn::HornBackend` does not know regimes exist. The module's own header says so — "the regime is essentially a *marker* — the runtime does not rewrite queries based on it". `EXPLAIN`'s `ExecutionMode` enum (`crates/sparql/src/plan/explain.rs`) has a single variant, `Materialized`. Capability is not occurrence: the trait is scaffolding with no caller.

Two things follow that should be fixed regardless of which option is chosen:

- `docs/architecture.md:337` records "Entailment regimes: OWL 2 RL/RDF + simple | **implemented** | `regime/owl_rl.rs`, `regime/simple.rs`". By the root `CLAUDE.md` rule that the code wins, that row is stale — nothing reads those types. Correcting it is deliberately left out of this branch, because "which regimes are implemented" is the claim this ADR asks about.
- `crates/sparql/src/regime/owl_rl.rs:20` returns `"http://www.w3.org/ns/entailment/OWL-RL"`, with a comment calling it the "W3C SPARQL 1.1 Entailment Regimes registry IRI". **There is no such registry IRI.** The registry is `ent:RDF`, `ent:RDFS`, `ent:D`, `ent:OWL-RDF-Based`, `ent:OWL-Direct`, `ent:RIF`. OWL 2 RL is not a regime; it is a *profile*, named with `sd:supportedEntailmentProfile http://www.w3.org/ns/owl-profile/RL` alongside the `ent:OWL-RDF-Based` regime. An endpoint advertising `ent:OWL-RL` in its service description is advertising an IRI no client recognises.

**Load-time OWL 2 RL materialization does exist, and is real.** `serve --materialize` (`crates/sparql/src/bin/serve.rs`) parses every input file into one `oxrdf::Dataset` and runs `load_with_reasoning` — the full compiled OWL 2 RL closure from `crates/owlrl` (65 rules in `rules.toml`) — before loading the result into the served store. It requires the `reasoner` cargo feature and refuses `.nq`/`.trig` inputs, because it collapses everything into one default graph. SPEC-29 views (`crates/sparql/src/reasoning/`) are the per-graph successor: a declared spine plus one data graph, closed into a reserved inferred graph, configured server-wide through `[reasoning]` and explicitly "never a per-query override".

So HornDB can put an OWL 2 RL closure in front of a query. What it cannot do today is decide *per query* which entailment relation the answer is computed under, and neither `--materialize` nor SPEC-29 views are reachable from the conformance path at all: `crates/harness/src/sparql_eval.rs` builds a bare `HornBackend` and calls `execute_query_with` directly. The `--engine owlrl` flag threads `horndb_owlrl::Engine` into the OWL-2-style suites only; `sparql11-eval` never touches it.

### The 66 `entailment/` cases, by regime

`sd:entailmentRegime` in this manifest is an **RDF list of alternatives**: a case is satisfied by answering under *any one* of the regimes it lists. `rdfs01`, for instance, carries `sd:entailmentRegime ( ent:RDFS ent:D ent:OWL-RDF-Based )`.

Counting each case once per regime it lists, across all 66 cases:

| Regime IRI | Cases listing it |
|---|---:|
| `ent:OWL-Direct` | 47 |
| `ent:D` | 36 |
| `ent:OWL-RDF-Based` | 36 |
| `ent:RDFS` | 35 |
| `ent:RDF` | 21 |
| `ent:OWL-RL` | **0** |
| `ent:RIF` | **0** |

Grouping instead by the *weakest* regime each case admits — which is what decides reachability for us — and splitting by current status:

| Group | Cases | Pass today | In `expected_failures` |
|---|---:|---:|---:|
| Lists at least one of `ent:RDF` / `ent:RDFS` / `ent:D` | 39 | 25 | 14 |
| Lists only `ent:OWL-Direct` + `ent:OWL-RDF-Based` | 6 | 2 | 4 |
| Lists only `ent:OWL-Direct` | 21 | 1 | 20 |
| **Total** | **66** | **28** | **38** |

Three consequences, in order of how much they matter.

**1. There is no OWL 2 RL case in the suite, and no RIF case.** Confirmed by grep over the whole fetched corpus: neither `ent:OWL-RL` nor `ent:RIF` appears anywhere. The comment in `harness/selected.toml:129` and row 38 of `harness/KNOWN-MANIFEST-BUGS.md` both say the blocked regimes are "RDF/RDFS/OWL-RL/OWL-Direct/RIF". That list is wrong on two of its five entries. OWL 2 RL appears only as `pr:RL` inside an `sd:EntailmentProfile` list on some cases — and even that is a manifest typo for the real predicate `sd:supportedEntailmentProfile`, so no conforming tool reads it either.

**2. SPEC-07 AC2 is unreachable under ADR-0001.** 24 of the 38 reds admit no regime weaker than OWL-Direct or OWL-RDF-Based. Those are OWL 2 DL and OWL 2 Full respectively — the exact expressivity ADR-0001 declares a non-goal ("OWL 2 DL completeness […] is a separate, harder problem class — not winnable at this budget"). The ceiling for any RDFS/RL-based strategy is therefore **42 of 66**, never 66. AC2 asks for 100% on a suite that is, by case count, mostly an OWL 2 DL test set.

**3. The "28 pass" number measures nothing about entailment.** Every one of the 28 is answered from the asserted triples alone; not one exercises an inference step. Broken out:

- **8** are BIND-semantics tests that happen to live in this directory (`bind01`–`bind08`). Their data is four `:sN :p N` triples plus `:p a owl:DatatypeProperty`; the answer is identical under every regime.
- **3** expect an **empty** answer and get one because nothing is inferred: `d-ent-01` (`?L a xsd:integer`), `rdfs13` (`?L a rdfs:Literal`), `owlds01`. These test a regime *restriction* — condition C2 of the spec, which keeps literals and out-of-vocabulary terms out of answers. Simple entailment satisfies them by having nothing to restrict.
- **2** test that a regime must **not** over-match: `lang` and `plainLit` both ask for `?x foaf:name "name"@en` over data holding both `"name"` and `"name"@en`, and expect only the second.
- **15** are plain asserted-triple lookups: `rdf02`, `rdf03`, `rdf04`, `rdfs08`, `rdfs12`, `owlds02`, `parent2`, `paper-sparqldl-Q5`, `sparqldl-01`, `sparqldl-04`–`sparqldl-09`.

Reporting "28 of 66 entailment cases pass" alongside a statement that HornDB does entailment would be misleading. The honest reading is 28 cases whose answers do not depend on any regime, and 0 cases of demonstrated entailment-regime support.

### What an OWL 2 RL closure would and would not fix

This is static analysis of each fixture against the 65 rules in `crates/owlrl/rules.toml`, not a measured run — no engine was executed for this ADR, and the plan below starts by measuring it. Of the 14 reds that admit RDF/RDFS/D:

**11 should flip green** under the existing RL closure: `rdfs01`, `rdfs02` (`prp-spo1`), `rdfs03` (`prp-spo1` + `prp-dom`), `rdfs04` (`cax-sco`), `rdfs05` (`scm-cls` + `cax-sco`), `rdfs06` (`prp-dom`), `rdfs07` (`prp-rng`), `rdfs09` (`scm-sco` + `cax-sco`), `rdfs10` (`scm-spo` + `prp-spo1`), `sparqldl-02`, `sparqldl-03` (`scm-cls`).

**3 would stay red**, and each shows a different way OWL 2 RL is not RDFS:

- `rdf01` asks `ex:b ?x rdf:Property` and expects `rdf:type` — i.e. it needs `ex:b rdf:type rdf:Property`, RDF entailment's rule `rdf1` ("everything used as a predicate is an `rdf:Property`"). OWL 2 RL has no such rule and `rules.toml` has none. **RL under-entails RDF.**
- `rdfs11` needs `ex:p rdfs:subPropertyOf ex:p` for a property that is never declared. RDFS makes `rdfs:subPropertyOf` reflexive on every property; RL's `scm-op` fires only on `?p rdf:type owl:ObjectProperty`, and `rdfs11.ttl` declares only `ex:b`. **RL under-entails RDFS.**
- `paper-sparqldl-Q1-rdfs` asks `?c rdfs:subClassOf ex:Student` under `( ent:RDFS ent:D )` and expects exactly `{GraduateAssistant, Student}`. Under the RL closure, `scm-cls-nothing` (`?c a owl:Class → owl:Nothing rdfs:subClassOf ?c`) adds a third row. **RL over-entails RDFS.**

That last case is the decisive one, because the suite contains its twin. `paper-sparqldl-Q1` is the *same query over the same data* under `( ent:OWL-Direct ent:OWL-RDF-Based )`, and its expected answer is `{owl:Nothing, GraduateAssistant, Student}` — precisely the three rows the RL closure produces. So `paper-sparqldl-Q1` would flip green for exactly the reason `paper-sparqldl-Q1-rdfs` would stay red.

**One materialized store cannot serve both.** Two cases, one query, one dataset, two different correct answers, distinguished only by the regime the client asked for. Any design that materializes once and answers everything from that store is provably wrong for one of them. Realistic yield from wiring the RL closure into the conformance path: about 12 cases (11 above plus `paper-sparqldl-Q1`), taking the suite from 28/66 to roughly 40/66 against a hard ceiling of 42.

### Is a regime served by materializing first honestly that regime?

Yes, under a condition HornDB does not currently meet.

The spec is explicit that the implementation technique is free: for RDF and RDFS it notes "Materialization is a common implementation technique" while "implementations are not required to implement these rules […] techniques based on query rewriting are equally possible". The regime is defined by the *answer set*, not by when the work happens. A sound and complete finite closure, queried afterwards by simple entailment, is the same regime.

The condition has two halves, and each is a real cost:

- **Soundness and completeness for that regime's entailment relation.** The OWL 2 RL rule set is not the RDFS rule set, in both directions, as the three cases above show. Serving RDFS from an RL closure is not "RDFS by another route"; it is a different answer set. Claiming RDFS therefore requires an RDFS-exact rule profile — a second rule set to build, grade and maintain — not a relabelling of the one we have.
- **The regime's answer restrictions (C1, C2).** The spec constrains answers to Skolemized blank nodes and to a finite vocabulary, which is why `rdfs13` and `d-ent-01` expect empty answers rather than every literal in the graph. Today those pass because HornDB infers nothing. Put a real closure behind them and the restrictions have to be implemented on purpose, or cases that pass today start failing — and `expected_failures` is bidirectional, so that regression is loud.

There is also a scope mismatch worth naming. `--materialize` closes one merged default graph and refuses named-graph inputs; SPEC-29 views are server-scoped and "never a per-query override". A SPARQL entailment regime is a property of the *query*, chosen per request against the dataset that request scopes. Nothing in HornDB currently selects an entailment relation at that granularity. That is the gap, and it is the same gap on the harness side.

### RIF and D-entailment

**RIF is not out of scope so much as absent.** No case in the suite carries `ent:RIF`. Supporting it would mean implementing RIF Core entailment — parsing RIF rule documents, importing them as a `rif:usedWith` profile, and entailing under the combined RDF+rules semantics. That is a second rule language beside OWL 2 RL, with its own conformance suite, and it buys zero cases here. The deselection is justified by "nothing to grade and a whole rule language to build", not merely by "out of scope".

**D-entailment is already partly here and should not be dismissed.** 36 of 66 cases list `ent:D`, and the one case that lists *only* `ent:D` — `d-ent-01` — passes today. D-entailment is RDF entailment plus datatype-aware literal equality over a recognised datatype map, which is close to what `crates/owlrl/src/datatype_literals.rs` already does with `dt-eq`/`dt-diff`/`dt-not-type`. But `ent:D` never appears without `ent:RDFS` beside it except in that single case, so claiming D alone moves the suite by nothing. The honest statement is that D is a cheap add-on to an RDFS claim, not a claim worth making on its own.

### How the harness counts, and what a deselection would do to the headline

Confirmed in `crates/harness/src/`:

- Outcome is a flat `Status::{Passed, Failed, Skipped}` (`outcome.rs:9`). A case listed in `expected_failures` is **still selected and still executed**; `apply_expected_failure` (`runner.rs:131`) turns a `Failed` into a `Skipped` carrying `known failure: <why>`, and turns a `Passed` into a **`Failed`** reading "listed in expected_failures but passed — drop it from harness/selected.toml". The allowlist cannot rot in either direction.
- The headline (`bin/harness.rs:231`) is one flat `passed=/failed=/skipped=` across every manifest-driven suite. `expected_failures` cases land in `skipped`, never in `passed`. Current measured `sparql11-eval` figures, from `harness/KNOWN-MANIFEST-BUGS.md` (2026-09-19, `--engine owlrl`): **444 pass, 63 fail, 40 skip**. (HDB-138's body quotes 372/135 from the HDB-128 era; that is stale.)
- A case absent from `include` produces **no `Outcome` at all** (`runner.rs:100`) — invisible in every counter and in the JUnit report. That is the dangerous shape, and SPEC-00's harness-first rule exists to forbid it.

So the repo already has the mechanism that keeps a deselection honest, and two precedents for using it:

- **`sparql11-gsp`.** Six cases stay in `expected_failures` under the heading "Every one is a **deliberate divergence named by SPEC-28 S5**, not a gap to close […] do not 'fix' the server to pass one of these cases without first changing S5." They count as skips, never as passes, and the reason table says why each is permanent.
- **HDB-77's `[sparql_default_graph]`.** The HornDB-specific fixtures sit in their own top-level section with the comment "These fixtures are written for HornDB and must never be counted as W3C conformance", graded by a separate test binary so they can never reach the W3C headline. `docs/architecture.md:124` repeats the claim.

**Proposed reporting rule, whichever option is chosen: a regime HornDB does not claim stays in `include` and stays in `expected_failures`.** It is never removed from `include`. What changes is only the prose in `harness/KNOWN-MANIFEST-BUGS.md`: the entailment block splits into *gaps* (cases a claimed regime should eventually make green — those are work) and *declined* (cases whose every listed regime HornDB does not claim — those are permanent, in the `sparql11-gsp` style, with the ADR number as the authority). The headline denominator never shrinks, `skipped` stays honest, and any declined case that starts passing still reports as a failure. Nothing needs to change in `crates/harness/`.

The one thing that must change either way is how the number is *described*. "443/547 on the W3C SPARQL 1.1 evaluation suite" is fine. "N of 66 entailment cases pass" is not, while every passing case is answered from asserted triples.

## Options considered

**A. Claim `simple` only.** Advertise `ent:Simple`; document OWL 2 RL materialization as a data-preparation feature of `serve --materialize` and SPEC-29 views, not as a query-time regime. All 38 reds are declined, permanently, with the case-level justification above. SPEC-07 AC2 is rewritten to grade what is actually gradeable. Cost: HornDB never advertises an entailment regime, which some procurement checklists ask for, and AC2 loses its headline ambition. Work: documentation only, plus the harness reading `sd:entailmentRegime` so the decline is *checked* rather than assumed.

**B. Claim `simple` + `ent:RDFS` (+ `ent:D`).** Build an RDFS-exact rule profile — not the RL one — plus the C1/C2 answer restrictions, and dispatch per case from `sd:entailmentRegime`. Reaches at most 42/66; the remaining 24 are declined on ADR-0001. Cost: a second rule set beside OWL 2 RL, with its own soundness argument, maintained forever, for at most 14 conformance cases and an `sd:` advertisement. HornDB's product does not otherwise need RDFS-exact semantics — customers want the RL closure.

**C. Claim `simple` + `ent:RDFS` + OWL 2 RL as `ent:OWL-RDF-Based` restricted to `pr:RL`.** B plus advertising the RL closure with the correct registry IRIs (`sd:entailmentRegime ent:OWL-RDF-Based`, `sd:supportedEntailmentProfile pr:RL`), and answering the `ent:OWL-RDF-Based` cases from the RL closure. Adds `paper-sparqldl-Q1` and possibly some of `paper-sparqldl-Q4` / `sparqldl-10` / `sparqldl-13`. Cost: B's cost, plus a claim to `ent:OWL-RDF-Based` that is *unsound in general* — OWL-RDF-Based is OWL 2 Full, the RL rules are a sound-but-incomplete approximation of it, and the four remaining cases in that group are exactly where the approximation is visible. Advertising a regime we answer incompletely is the one thing on this list that is not honest.

**D. Do nothing and leave SPEC-07 AC2 as written.** Cost: a permanently unreachable acceptance criterion in an accepted spec, and 38 `expected_failures` lines with a root-cause comment that misnames two of the five regimes it lists. Rejected — not because the work is required, but because the criterion says something false.

**Recommendation: A**, with the harness change from B's plan done anyway.

The argument is arithmetic. 47 of the 66 cases list `ent:OWL-Direct`, 21 of them exclusively; ADR-0001 declines OWL 2 DL as a founding decision. The best case for the whole RDFS programme is 14 cases, realistically 11, bought with a second rule engine that no customer has asked for and that must stay correct beside the OWL 2 RL one forever. HornDB's entailment claim is already graded, and graded well, by the suite built for it: 100 of 115 W3C OWL 2 RL cases green, with the 15 reds triaged as Stage-1 non-goals. That is the claim worth making.

The honest case against A, stated plainly:

- It concedes an acceptance criterion in an accepted spec rather than meeting it. Someone has to be comfortable saying SPEC-07 AC2 was written against a suite nobody had counted.
- `sd:entailmentRegime ent:Simple` in a service description reads as "no reasoning" to a client that does not read the rest of the documentation, which undersells a product whose whole point is reasoning. Option C's IRIs are what a procurement checklist looks for.
- 11 or 12 of those cases are cheap-ish once the harness dispatches per case, and turning down free conformance points is a real cost — especially the `rdfs01`–`rdfs10` family, which is the part of RDFS that OWL 2 RL genuinely does subsume.
- If a customer later demands an RDFS-advertising endpoint, A's decision has to be reopened, and the rule-profile work is the same work deferred.

A partial hedge, if that last point weighs: take A now, and file B's rule-profile work as a *spike* rather than dropping it — the measurement task below tells us the real number for about a day of work, and the decision can be revisited with that number in hand instead of this ADR's static analysis.

## Decision (proposed)

Deferred to the CTO. This ADR records the options; it changes nothing. On a decision, the plan below is filed as Worklode tasks.

## Plan once a decision exists

Task 1 is worth doing under **every** option, including D, because it replaces this ADR's static analysis with measurement and fixes two statements that are wrong today.

**1. Measure, and fix the two wrong statements. (Every option.)**
Run `sparql11-eval` with a locally hacked `sparql_eval.rs` that materializes each entailment case's data through `horndb_owlrl::Engine` before querying, and record which of the 66 flip. Throw the hack away; the deliverable is the number. Then correct, in one commit: the regime list in the `harness/selected.toml:129` comment and in row 38 of `harness/KNOWN-MANIFEST-BUGS.md` (there is no OWL-RL or RIF case in the suite), the stale 372/135 figures wherever they survive, and `crates/sparql/src/regime/owl_rl.rs:20`'s non-existent `ent:OWL-RL` IRI. Also settle `docs/architecture.md:337`, which currently calls the unwired regime markers "implemented". Size: half a day plus one conformance run.

**2. Parse `sd:entailmentRegime`. (Every option except D.)**
`crates/harness/src/manifest.rs::project_entry`, the `Suite::Sparql11Eval` branch, reads only `qt:query`, `qt:data`, `qt:graphData` and `mf:result` off the action node. Add the `sd:` namespace, read the regime list (it is an RDF list, and may be a single IRI), and carry it on `TestKind::SparqlQueryEval` in `crates/harness/src/testcase.rs`. Default to `ent:Simple` when absent, which is what every non-`entailment/` case wants. Tests: `crates/harness/tests/manifest_parse.rs` gets a fixture with a list-valued regime and one with none. Size: small, self-contained, no behaviour change yet.

**3. Grade against the claimed set. (Every option except D.)**
In `crates/harness/src/sparql_eval.rs::run_query_eval`, compare the case's regime list against HornDB's claimed set. If they intersect, run under the claimed regime. If they do not, the case is **declined**: report `Skipped` with reason `regime not claimed: <IRIs> (ADR-0022)`. The claimed set is a constant in the harness, not a `selected.toml` key — it is one project-wide fact, and `SuiteEntry` has no per-case metadata to hang it on. Under option A this makes the decline *checked* rather than asserted, which is the whole point of doing it. Size: small.

**4. Re-sort `KNOWN-MANIFEST-BUGS.md` and the allowlist. (Every option except D.)**
Split the 38 into *gaps* and *declined* as described above; keep every one in `include` and in `expected_failures`; write the reason table in the `sparql11-gsp` style, citing this ADR. Under option A all 38 are declined. Size: documentation.

**5. Amend SPEC-07 AC2. (Every option, including D — only the wording differs.)**
Name the claimed regimes and the gradeable target. Under A that is roughly: 100% on the evaluation suite under simple entailment, with the `entailment/` directory's cases declined and counted as skips, and OWL 2 RL conformance graded by `owl2-w3c-rl` instead. Under B or C it is the reachable count for the claimed set, stated as a number, not as 100%. This edit belongs to whoever owns the decision. Size: small, but it is the gate.

**Options B and C only, in order:**

**6. An RDFS-exact rule profile.** A selectable closure profile beside the RL one. `crates/owlrl` compiles one rule set from `rules.toml`; this needs a second, plus a way to pick. The three divergences named above (`rdf1`, `subPropertyOf` reflexivity on undeclared properties, no `scm-cls-nothing`) are the acceptance tests, together with the RDFS half of the W3C RDF entailment suite if we fetch it. This is the expensive task and the one that decides whether B is worth doing. Size: multi-week; specify it before scheduling it.

**7. Answer restrictions C1 and C2.** Skolemized blank nodes and a finite answer vocabulary, applied to results from a materialized store. `rdfs13`, `d-ent-01` and `owlds01` pass today by inferring nothing; with a closure behind them they become real tests of this. Size: medium, and required before any regime claim is sound.

**8. Per-query regime selection in `horndb-sparql`.** Give `SparqlConfig` a regime and make `api::execute_query_with` honour it, so the `EntailmentRegime` trait stops being dead scaffolding. Decide what "materialize per query" means when `--materialize` closes one merged graph and SPEC-29 views are server-scoped; most likely each claimed regime maps to a named view and the query scopes to it. Size: large — this is where B and C stop being conformance work and become architecture.

**9. Service-description advertisement.** Emit `sd:entailmentRegime` and `sd:supportedEntailmentProfile` with the correct registry IRIs on the endpoint's service description. Only meaningful once 6–8 are real. Size: small, last.

## Consequences

- `+` The suite's real shape is on the record: `entailment/` is mostly an OWL 2 DL test set, 24 of its 38 reds are unreachable under ADR-0001, and the 28 "passes" demonstrate no entailment at all.
- `+` The reporting rule keeps the harness-first discipline intact — nothing leaves `include`, nothing inflates `passed` — and needs no code change in `crates/harness/`.
- `+` Task 1 is worth doing under every option, so the branch that files these tasks is not blocked on the decision.
- `−` Under A, SPEC-07 AC2 is conceded rather than met, and HornDB advertises `ent:Simple` on an endpoint whose selling point is reasoning.
- `−` Under B or C, HornDB maintains two rule sets forever for at most 14 conformance cases.
- `−` Under C specifically, `ent:OWL-RDF-Based` is claimed on a sound-but-incomplete approximation. Of the options this is the only one whose claim is not defensible on its own terms.
- `−` Whatever is chosen, the `ent:OWL-RL` IRI in `regime/owl_rl.rs` and the "implemented" row in `docs/architecture.md` are wrong today and stay wrong until task 1 lands.

## Related

- Constrained by: ADR-0001 (OWL 2 RL, not OWL 2 DL) — the 24 OWL-Direct/OWL-RDF-Based-only reds are unreachable by that decision.
- Bears on: SPEC-07 acceptance criterion 2 (`HDB-SPEC-7`); SPEC-01's harness-first rule; SPEC-04 (`crates/owlrl/rules.toml`); SPEC-29 (named-graph reasoning views).
- Worklode: HDB-138 (this design task), HDB-128 (wired up the full evaluation suite), HDB-77 (the HornDB-specific-vs-W3C separation this ADR's reporting rule follows).
- Code the design touches: `crates/harness/src/{manifest,testcase,sparql_eval,runner}.rs`, `crates/sparql/src/regime/`, `crates/sparql/src/bin/serve.rs`, `crates/sparql/src/reasoning/`, `crates/owlrl/rules.toml`.
- Upstream: [SPARQL 1.1 Entailment Regimes](https://www.w3.org/TR/sparql11-entailment/) (regime registry, conditions C1/C2, and the note that materialization is a permitted technique).
