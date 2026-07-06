# SPEC-11 — SSSOM Mappings and Crosswalk Index

## Purpose

Define first-class support for **ontology crosswalks** expressed as [SSSOM](https://mapping-commons.github.io/sssom/) mappings: how mapping facts are represented in the store, how their chain-rule closure is materialized, and how query-time crosswalking is served from a compact, SIMD-friendly index that exploits HornDB's sequential integer `TermId`s.

The motivating use case (Sunstone) is that **crosswalking between vocabularies is part of nearly every query** — translating a term in one ontology to its counterpart in another. "Head-on support" therefore means materializing mappings in a flattened, space-efficient form so that crosswalking is cheap by construction, not a per-query burden.

This SPEC governs the *reasoning and serving* of mappings. It does **not** define an SSSOM ingestion/ETL path: per ADR-0016 and the data-platform ADR-0002, the system-of-record owns SSSOM storage and emits mapping facts to HornDB through the changefeed like any other named graph. HornDB is the reasoning view.

## Scope

In scope:
- Representation of a mapping in the store (§F2): an n-ary `sssom:Mapping` node (canonical/asserted) plus, for positive mappings, the materialized base triple `subject predicate object`.
- The mapping-predicate vocabulary (`skos:exactMatch`/`closeMatch`/`broadMatch`/`narrowMatch`/`relatedMatch`, `owl:equivalentClass`/`equivalentProperty`/`sameAs`, the `semapv:*` cross-species predicates) added to `crates/owlrl/src/vocab.rs`.
- The SSSOM **chaining rules** — transitivity (T1), role-chains over exact/equivalent matches (RCE1/RCE2), inverse (RI1–5), generalisation (RG1–2) — compiled into `crates/owlrl/rules.toml` and fired in the existing semi-naïve loop (ADR-0004), with the transitive subset delegated to the GraphBLAS closure backend (SPEC-05).
- **Negative mappings** (`predicate_modifier = Not`): monotone negative-chaining (positive ∘ negative ⟹ negative) modelled without negation-as-failure.
- The **compact crosswalk index** (§F5): rung-2 baseline (Elias-Fano subjects + Frame-of-Reference bit-packed objects), with PGM/learned-index as the aggressive target.
- The **crosswalk spine** (§F6): designated high-traffic mapping sets held always-resident; identity-strength mappings ride the existing spine.
- Confidence propagation along chains (§F7) and chain provenance (§F8).
- A **harness-only** SSSOM/TSV loader (§F9) for benchmarking and standalone runs.

Out of scope:
- **SSSOM/TSV parsing as a production path.** The SoR owns it (ADR-0016, data-platform ADR-0002). The harness loader (§F9) exists only for benching/standalone.
- **Storage substrate and CDC transport** — data-platform ADR-0002 (Postgres quad store, outbox → logical replication). HornDB consumes the changefeed; it does not define it.
- **A global `exactMatch → owl:sameAs` identity bridge** — forbidden by ADR-0017. `exactMatch` is a crosswalk edge, never OWL identity; only a per-set, load-time opt-in may promote a specific trusted set.
- **RDF 1.2 per-edge annotation** — deferred (ADR-0014, ADR-0002 Decision 10). Mappings use n-ary nodes in plain RDF 1.1.
- **The OWL role-chain rules RCE-N1–4** (`equivalentClass ∘ subClassOf`, etc.) — the SSSOM spec defers these to a reasoner, and HornDB's OWL 2 RL `cax-*`/`scm-*` rules (SPEC-04) already entail them. Not re-implemented here.
- **Probabilistic mapping reconciliation** (boomer-style) and the full SeMRA confidence-aggregation / `reviewer_agreement` model — Stage 2. SSSOM chaining is, by the spec's own words, "structural, not proper reasoning."
- **Namespace-clustered ID assignment** (rung-3 affine compression) — a separate, optional decision tied to canonical-IRI minting; see Risks.

## Background: the SSSOM data model (normative summary)

A **mapping** has four required slots — `subject_id`, `predicate_id`, `object_id`, `mapping_justification` (the last from the `semapv:` vocabulary) — plus optional provenance (`confidence`, `subject_source`, `object_source`, `predicate_modifier`, `mapping_cardinality`, `author_id`, `mapping_date`, `derived_from`, …). A **mapping set** groups mappings and carries set-level "propagatable" slots that default down onto each row. The predicate vocabulary is **open** ("implementations MUST accept any arbitrary predicate"); SKOS/OWL predicates are recommended. `mapping_cardinality` is a set-scoped **derived** aggregate, not a per-row fact. A negated mapping (`predicate_modifier = Not`) asserts the triple is **false**.

## Functional requirements

**F1. Mapping-predicate vocabulary.** Add the recommended SSSOM predicates to `crates/owlrl/src/vocab.rs` (one `///`-doc'd field each, per the owlrl codegen contract): the five SKOS mapping relations, `owl:equivalentClass`/`equivalentProperty`/`sameAs`, and the `semapv:crossSpecies{Exact,Narrow,Broad}Match` predicates. The open-vocabulary requirement is honoured: unknown predicates are stored and crosswalked as opaque edges, but only the known set participates in the compiled chain rules.

**F2. Mapping representation.** Each mapping is materialized as:
- An n-ary `sssom:Mapping` node (the canonical, asserted form — same shape as a claim node, and as SSSOM's OWL reification) carrying `subject`, `predicate`, `object`, `mapping_justification`, `confidence`, `predicate_modifier`, and a `derived_from` link for inferred mappings. This is the provenance/correctability unit (ADR-0013) and need not be resident in the hot index.
- For a **positive** mapping, the base triple `subject predicate object`, which is what the crosswalk index encodes and what queries hit.
- For a **negative** mapping, **no** positive base triple — see F4.

Set-level propagatable slots (sources, default justification/confidence, `curie_map`) are hoisted to the mapping-set header (named-graph level), not repeated per row.

**F3. Chaining rules.** Compile the SSSOM chaining rules into `rules.toml`:
- **T1 (transitivity):** `A -[p]-> B -[p]-> C ⟹ A -[p]-> C` for `p ∈ {exactMatch, broadMatch, narrowMatch, equivalentClass, equivalentProperty, subClassOf, subPropertyOf, sameAs}`. Each instantiation has a **constant** leading predicate, so it compiles directly; the transitive-closure instances are marked `delegate = "closure"` (GraphBLAS, like `scm-sco`/`eq-trans`).
- **RCE1/RCE2 (role chains over exact/equivalent):** `A -[exactMatch|equivalentClass]-> B -[p]-> C ⟹ A -[p]-> C`, and the `-[p]-> B -[exactMatch]->` mirror.
- **RI1–5 (inverse):** `narrowMatch ↔ broadMatch`, and the `semapv:crossSpecies*` inverses. Tagged `semapv:MappingInversion`.
- **RG1–2 (generalisation):** `equivalentClass ⟹ exactMatch`, `subClassOf ⟹ broadMatch` (deliberate weakening when mixing OWL- and SKOS-strength mappings).

Inferred mappings are tagged `mapping_justification = semapv:MappingChaining` (or `MappingInversion`), and their `derived_from` is populated from the rule's proof premises (F8). The OWL role-chain rules RCE-N1–4 are **not** added — the OWL-RL engine entails them.

**F4. Negative mappings (quadruple semantics, monotone).** Per `inference.md`, inference operates on mapping **quadruples** `(s, p, o, modifier)`, not triples: from `A exactMatch B` and `B exactMatch[Not] C` one must derive `A exactMatch[Not] C`, not `A exactMatch C`. Model a negated mapping as a **positive fact over a distinct negated predicate** (e.g. an internal `notExactMatch` partition), so negative-chaining is ordinary **monotone** Datalog — `exactMatch(A,B) ∧ notExactMatch(B,C) ⟹ notExactMatch(A,C)` — with **no negation-as-failure**, preserving SPEC-04's negation-free/stratified guarantee. Negated mappings are **excluded from the positive crosswalk index** and served as exclusions by the query layer.

**F5. Compact crosswalk index.** A per-(predicate, source-namespace, target-namespace) structure over `TermId` pairs:
- **Rung-2 baseline:** Elias-Fano-encoded subject column (monotone) + Frame-of-Reference bit-packed object column, bidirectional (a second object-major ordering), ~10 bytes/pair. Supports point lookup, range scan, and **batch (vectorized) translation** of a column of IDs — the hot crosswalk path.
- **Rung-4 target:** a PGM / learned-index piecewise-linear model over the correspondence, degrading gracefully on arbitrary crosswalks (~3–4 B/pair) with no dictionary changes.
- Metadata columns (confidence, justification) are kept out of the hot index (F7/F8).
The index is rebuilt/overlaid per hydration view; branch overlays compose with the SPEC-06 Z-set delta machinery.

**F6. Crosswalk spine.** Identity-strength mappings (`owl:sameAs`/`equivalentClass`) route to the existing GraphBLAS EQREL identity spine (ADR-0007) and are always hydrated as part of the spine (ADR-0002 Decision 2). Crosswalk-strength mappings (`exactMatch` etc.) for **designated** high-traffic sets are promoted into an always-resident crosswalk spine alongside it, so common crosswalks are present in every query without per-query hydration; non-designated sets stay per-set, hydrated on demand. The compact index (F5) is what makes the resident footprint affordable.

**F7. Confidence.** Default confidence is **1.0** when unspecified. Per-row confidence is quantized (u8) and kept in a side column; uniform per-set confidence is hoisted to the header (0 B/pair). Confidence combines along a chain by **product** by default (independent-probability semantics), configurable; the aggregation model references SeMRA (Hoyt et al. 2025). For the GraphBLAS-delegated transitive rules this is a custom semiring over the confidence weight.

**F8. Provenance.** Every inferred mapping records `Provenance{rule_id, premises}` via the existing compiled-rule machinery (SPEC-04 F4); the premise set **is** the SSSOM `derived_from` set, and the proof tree bottoms out at asserted mappings. This satisfies ADR-0013 and SSSOM's `semapv:MappingChaining`/`MappingInversion` justification requirement without new machinery.

**F9. Harness loader (bench/standalone only).** A minimal SSSOM/TSV reader in `crates/harness`: parse the commented-YAML header (→ set metadata + `curie_map`), expand CURIEs to IRIs, split `|`-delimited multivalue cells, and emit mapping quadruples. Not a production surface; production mappings arrive as RDF via the changefeed.

## Non-functional requirements

**NF1. Crosswalk throughput.** Batch translation of a column of `TermId`s through a resident crosswalk index achieves ≥ (target TBD, benched on hornbench) IDs/sec, dominated by the vectorized lower-bound + add, not by allocation.

**NF2. Index size.** ≤ ~10 bytes/pair bidirectional on a real mapping set (rung-2), beating the 16 B (unidirectional) / 32 B (bidirectional) naive two-column baseline. Recorded in `docs/benchmarks.md`.

**NF3. Chain-rule materialization.** Full chaining closure over a ~1.16M-mapping corpus (OxO2's reference: 1,160,020 asserted → +49,536 inferred in ~17 min / ~380 MB on a laptop) completes **≥ one order of magnitude faster** on the hornbench reference host — target ≤ ~1 min, ideally seconds — exploiting compiled rules + GraphBLAS closure.

**NF4. Resident footprint.** Crosswalk-spine memory is bounded by `(promoted mappings) × ~10 B/pair`; e.g. 10 M resident mappings ≤ ~100 MB.

## Dependencies

- SPEC-02 (storage, dictionary/`TermId`, columnar partitions) — the index builds on the dictionary and partition layout.
- SPEC-04 (rule engine) — the chain rules ride `rules.toml` + `build.rs`; provenance via F4.
- SPEC-05 (GraphBLAS closure) — transitive chaining + identity (ADR-0007); confidence semiring.
- SPEC-06 (incremental) — branch overlay of the index via Z-set deltas; changefeed hydration.
- SPEC-07 (SPARQL frontend) — planner dispatch of crosswalk patterns to the index; optional query-rewrite.
- ADR-0016 (reasoning view over external SoR), ADR-0017 (`exactMatch` is crosswalk, not identity).

## Acceptance criteria

1. A curated SSSOM conformance subset (the mappings analogue of `harness/curation/owl2-rl-50.md`) is added to `harness/selected.toml` and is green. It includes a real mapping set (e.g. a Biomappings/Mondo slice) loaded via §F9.
2. **Chaining conformance:** T1, RCE1/RCE2, RI1–5, RG1–2 produce the correct inferred mappings on the subset, each tagged with the correct `semapv:*` justification.
3. **Negative chaining:** the `inference.md` xanthene example (positive ∘ `Not` ⟹ `Not`) is derived correctly, and no positive `exactMatch` is derived across a negative link.
4. **Identity isolation (ADR-0017):** a differential test confirms `skos:exactMatch` never yields `owl:sameAs` entailment (no `eq-rep-*` firing on mapping edges), while `owl:sameAs`/`equivalentClass` mappings do reach identity.
5. **Index correctness:** crosswalk-index lookup is bit-identical to a base-triple scan over the same mappings (differential test), in both directions.
6. **Index size + throughput** measured on hornbench and recorded in `docs/benchmarks.md`: ≤ ~10 B/pair (NF2); throughput (NF1); full-closure time vs the OxO2 baseline (NF3).
7. **Crosswalk-in-every-query:** a designated spine set is resident and crosswalkable without per-query hydration.
8. **Provenance:** every inferred mapping returns a proof tree / `derived_from` chain bottoming out at asserted mappings (ADR-0013).

## Risks and open questions

- **Confidence chain-combination semantics.** Default is product; SSSOM's `confidence-model.md` references SeMRA (Hoyt et al. 2025) but does not mandate a single chaining formula. Validate the default (product vs noisy-or vs min) against SeMRA before committing; expose it as configuration.
- **Spine promotion mechanism.** How a set is designated always-resident — explicit config, a mapping-set property, or a size/traffic threshold — is unresolved. Start with explicit config.
- **Rung-4 (PGM) timing.** The learned-index encoder is the real aggressive win; sequence it after the rung-2 baseline proves correctness and the bench harness exists.
- **Namespace-clustered IDs (rung-3 affine ~3 B/pair) are *not* a free deferral.** They require a sparse/clustered ID space (vs the dense `Vec<Term>` reverse map), are fragile under partial/lazy hydration (ADR-0016), and need a canonical SoR-sourced numbering — and affine alignment is mathematically achievable for at most one crosswalk per namespace. Pursue only for **Sunstone-minted** namespaces against a primary external ontology, decided at the canonical-IRI-minting layer (the concept-canonicalization follow-up to ADR-0002), under its own ADR. Not on the critical path.
- **Harness corpus selection.** Which real SSSOM set(s) to vendor for conformance + bench (size, licence, representativeness). Biomappings and a Mondo slice are candidates.
- **Open-vocabulary crosswalking.** Mappings under non-recommended predicates are stored and crosswalked but do not chain; confirm this is the desired default vs warning at ingestion.
