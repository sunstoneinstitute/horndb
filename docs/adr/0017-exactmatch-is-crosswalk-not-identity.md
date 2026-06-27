# ADR-0017: skos:exactMatch is a crosswalk edge, not OWL identity

**Status:** Accepted

**Date:** 2026-06-27

**Source:** Design decision taken while scoping first-class SSSOM mapping support (forthcoming SPEC-11). Grounded in a literature review of `owl:sameAs` vs SKOS mapping semantics.

## Context

HornDB is gaining first-class support for SSSOM (Simple Standard for Sharing Ontological Mappings) crosswalks. SSSOM mappings are carried by an open predicate vocabulary that *recommends* the SKOS mapping relations (`skos:exactMatch`, `skos:closeMatch`, `skos:broadMatch`, `skos:narrowMatch`, `skos:relatedMatch`) and the OWL/RDFS identity relations (`owl:sameAs`, `owl:equivalentClass`). The SSSOM **chain rules** (composition/transitivity rules — see forthcoming SPEC-11) will be compiled into `crates/owlrl/rules.toml` and fire in the same semi-naïve loop as the OWL 2 RL rules (ADR-0004).

Because those rules interleave with OWL-RL, one concrete question decides whether crosswalking pollutes entailment: **do we add a bridge rule `skos:exactMatch(a,b) → owl:sameAs(a,b)`?**

- A bridge rule would make `exactMatch` participate in full OWL identity, firing `eq-rep-*` and substituting every type/relation of one term onto the other across vocabularies — higher recall, but it asserts "curators say these line up" *means* "same individual."
- `skos:exactMatch` is also a **general SKOS predicate**, not SSSOM-private. It appears in our own concept schemes (the research-stack `rdf/scope/<N>/` output) and elsewhere, so a global bridge rule fires on **every** `exactMatch` in the hydrated graph, far beyond ingested SSSOM sets.

The literature is decisive. `owl:sameAs` is widely shown to be *too strong* and to misbehave through inference (Halpin, Hayes, McCusker, McGuinness & Thompson, *"When owl:sameAs Isn't the Same"*, ISWC 2010; *"The sameAs Problem"* survey, 2019). SKOS deliberately declined `owl:sameAs`: `skos:exactMatch` asserts interchangeability *for information-retrieval purposes* and explicitly does **not** imply that statements about one concept transfer to the other (SKOS Reference). Crucially, `exactMatch` being *transitive for chaining* is not the same as being *identity*: SSSOM's chain rules already compose `exactMatch` (and role-chain across it) **within the mapping layer**, so crosswalk recall does not depend on promoting it to `sameAs`.

## Decision

Treat the SKOS mapping predicates as **crosswalk edges, never OWL identity.**

- `skos:exactMatch` / `closeMatch` / `broadMatch` / `narrowMatch` / `relatedMatch` are composed by the SSSOM chain rules within the mapping layer and served by the compact crosswalk index. They are **not** bridged to `owl:sameAs`; no global `exactMatch → sameAs` rule exists, so `eq-rep-*` term substitution never fires on a mapping edge.
- `owl:sameAs` and `owl:equivalentClass` remain **genuine OWL identity** and continue to route to the GraphBLAS EQREL / union-find backend (ADR-0007) unchanged.
- **Escape hatch — per-set, at ingestion.** If a specific, high-trust mapping set genuinely warrants identity, its `exactMatch` may be promoted to `owl:sameAs` as a **load-time ingestion policy on that set**, never as a global reasoning rule. The decision of which sets are "identity-grade" is data/ingestion configuration, not entailment.

## Consequences

+ No entailment pollution: distinct-but-mapped concepts keep their own types, relations, and `disjointWith`/cardinality assertions; no cross-vocabulary contradictions leak in through a sledgehammer `sameAs` merge.
+ Crosswalk recall is preserved: the compiled SSSOM chain rules compose `exactMatch` chains and role-chain across them, and inferred mappings carry `mapping_justification = semapv:MappingChaining`, which lands on the existing rule proof-tree provenance (ADR-0013) for free.
+ Clean separation of concerns: **identity** = GraphBLAS EQREL (ADR-0007); **crosswalk** = mapping layer + compact index.
− Queries wanting cross-vocabulary term substitution must crosswalk **explicitly** (through the index / chain rules) rather than relying on OWL identity to do it implicitly. This is the intended trade — explicitness over a silent, hard-to-reverse entailment blowup.
− The per-set opt-in adds a small ingestion-policy surface (which mapping sets, if any, are promoted to identity-grade), to be specified in SPEC-11.

## Related

- Governing spec: forthcoming **SPEC-11** (SSSOM mappings + crosswalk index).
- ADR-0007 — `owl:sameAs` / schema closure routed to the GraphBLAS EQREL backend (the identity path this ADR deliberately keeps mappings *out* of).
- ADR-0004 — compiled OWL 2 RL rules; the SSSOM chain rules ride the same `rules.toml` → `build.rs` codegen.
- ADR-0013 — provenance / correctability; chain-derived mappings get proof-tree justification.
- ADR-0016 — embeddable reasoning view over an external SoR (where mappings are hydrated from).
