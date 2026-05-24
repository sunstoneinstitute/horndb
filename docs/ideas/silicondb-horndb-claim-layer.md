# SiliconDB × HornDB claim layer

## Goal

Use SiliconDB as a probabilistic candidate generator and HornDB as the provenance-preserving verifier.

- **SiliconDB** proposes, ranks, and prioritizes claims.
- **HornDB** proves, disproves, or leaves claims unresolved.
- Only **HornDB-certified claims** are promoted into the asserted reasoning graph.

This lets us combine noisy large-scale extraction with strict symbolic reasoning.

## Core architecture

Use a dataset with named graphs:

- `:ontology` — schema / ontology / rules
- `:asserted` — HornDB-certified facts
- `:claims` — SiliconDB candidate claims
- `:evidence` — source spans, extraction artifacts, provenance
- `:belief` — probability / calibration / attention metadata
- `:verdicts` — HornDB proofs and disproofs
- `:history` — optional event log

The key rule is simple:

> A claim is not an asserted fact until HornDB promotes it.

## Vocabulary sketch

### Classes

```turtle
ex:Claim a owl:Class .
ex:ClaimBundle a owl:Class .
ex:Evidence a owl:Class .
ex:BeliefState a owl:Class .
ex:Verification a owl:Class .
ex:Proof a owl:Class .
ex:Disproof a owl:Class .
ex:AttentionSignal a owl:Class .
ex:Cohort a owl:Class .
```

### Claim statuses

```turtle
ex:Candidate a ex:ClaimStatus .
ex:UnderReview a ex:ClaimStatus .
ex:Proven a ex:ClaimStatus .
ex:Disproven a ex:ClaimStatus .
ex:Unknown a ex:ClaimStatus .
ex:NeedsMoreEvidence a ex:ClaimStatus .
```

## Claim representation

### Named-node claim

This is the canonical representation for indexing and workflow.

```turtle
:claim123 a ex:Claim ;
  ex:subject :GeneX ;
  ex:predicate ex:associatedWith ;
  ex:object :DiseaseY ;
  ex:claimType ex:BinaryRelation ;
  ex:status ex:UnderReview ;
  prov:wasDerivedFrom :evidenceSpan42 .
```

### RDF-star variant

Useful for statement-level annotation, but not required as the canonical storage model.

```turtle
<< :GeneX ex:associatedWith :DiseaseY >>
    a ex:Claim ;
    ex:status ex:UnderReview ;
    prov:wasDerivedFrom :evidenceSpan42 .
```

## Belief state

Belief is separate from claim identity.

A practical model is:

- `ex:probability`
- optional Beta parameters `ex:alpha`, `ex:beta`
- calibration metadata
- evidence weight / count

```turtle
:belief123 a ex:BeliefState ;
  ex:aboutClaim :claim123 ;
  ex:probability "0.83"^^xsd:decimal ;
  ex:alpha "17.0"^^xsd:decimal ;
  ex:beta "3.5"^^xsd:decimal ;
  ex:calibrationModel :calib-v4 ;
  ex:updatedAt "2026-05-24T12:34:56Z"^^xsd:dateTime .
```

## Attention / energy

Borrow SiliconDB’s idea of an attention layer, but treat it as prioritization metadata, not truth semantics.

```turtle
:attention123 a ex:AttentionSignal ;
  ex:aboutClaim :claim123 ;
  ex:energy "0.91"^^xsd:decimal ;
  ex:novelty "0.77"^^xsd:decimal ;
  ex:contradictionPressure "0.68"^^xsd:decimal ;
  ex:peerDeviation "0.84"^^xsd:decimal ;
  ex:downstreamImpact "0.93"^^xsd:decimal ;
  ex:rank "12"^^xsd:integer .
```

## Evidence / provenance

Evidence nodes capture the source data and extraction lineage.

```turtle
:evidenceSpan42 a ex:Evidence ;
  prov:wasDerivedFrom :paper42 ;
  ex:sourceOffset "18422"^^xsd:integer ;
  ex:sourceLength "316"^^xsd:integer ;
  ex:extractor :llm-extractor-v3 ;
  ex:extractionConfidence "0.79"^^xsd:decimal ;
  dct:created "2026-05-24T12:00:00Z"^^xsd:dateTime .
```

## Verification / verdicts

HornDB writes the result of proof attempts back into RDF.

### Verification record

```turtle
:verif789 a ex:Verification ;
  ex:aboutClaim :claim123 ;
  ex:status ex:Proven ;
  ex:verifiedBy :horndb-run-2026-05-24-001 ;
  prov:generatedAtTime "2026-05-24T12:40:00Z"^^xsd:dateTime .
```

### Proof object

```turtle
:proof789 a ex:Proof ;
  ex:forClaim :claim123 ;
  ex:usesRule :rule_prp_trp ;
  prov:wasDerivedFrom :fact17, :fact88, :fact91 ;
  ex:proofHash "sha256:..." .
```

### Disproof object

```turtle
:disproof456 a ex:Disproof ;
  ex:forClaim :claim123 ;
  ex:reason ex:ContradictionWithHigherConfidenceClaim ;
  prov:wasDerivedFrom :fact22, :fact23 ;
  ex:counterexampleGraph :counterexample-graph-1 .
```

## Lifecycle

A claim can move through:

```text
candidate → under_review → proven
candidate → under_review → disproven
candidate → under_review → unknown
candidate → needs_more_evidence
```

When HornDB proves a claim, promote the underlying triple into `:asserted`:

```turtle
GRAPH :asserted {
  :GeneX ex:associatedWith :DiseaseY .
}
```

## Threshold policy

SiliconDB can gate claims for verification with explicit policy metadata.

```turtle
:policy1 a ex:PromotionPolicy ;
  ex:minProbability "0.85"^^xsd:decimal ;
  ex:minEnergy "0.60"^^xsd:decimal ;
  ex:minEvidenceCount "3"^^xsd:integer ;
  ex:requireProvenanceDepth "2"^^xsd:integer .
```

Then a claim can note the policy under which it was selected:

```turtle
:claim123 ex:evaluatedUnder :policy1 .
```

## Numeric claims

Numeric facts should store the value and the uncertainty separately.

```turtle
:claim456 a ex:NumericClaim ;
  ex:subject :CompanyA ;
  ex:predicate ex:revenue ;
  ex:numericValue "142.7"^^xsd:decimal ;
  ex:unit :USD_Million ;
  ex:intervalLower "138.0"^^xsd:decimal ;
  ex:intervalUpper "149.2"^^xsd:decimal ;
  ex:probability "0.76"^^xsd:decimal ;
  prov:wasDerivedFrom :report2024#table3 .
```

## Peer cohort comparison

Borrow SiliconDB’s peer-relative scoring for triage.

```turtle
:server-17 ex:memberOfCohort :linux-db-servers ;
           ex:peerDeviationScore "0.91"^^xsd:decimal .
```

This is not truth by itself, but it is excellent prioritization metadata.

## How the systems interact

### SiliconDB owns

- candidate generation
- belief updates
- attention / energy
- peer comparison
- retrieval shortcuts
- prioritization

### HornDB owns

- ontology and entailment
- proofs and disproofs
- certified assertions
- provenance-preserving explanation
- deterministic rule-based reasoning

## Minimal viable vocabulary

If we want the smallest useful starting point, this is enough:

- `ex:Claim`
- `ex:Evidence`
- `ex:BeliefState`
- `ex:Verification`
- `ex:Proof`
- `ex:Disproof`
- `ex:probability`
- `ex:energy`
- `ex:status`
- `ex:subject`
- `ex:predicate`
- `ex:object`
- `prov:wasDerivedFrom`

## Recommended storage choice

Use **named graphs as the canonical store**, and optionally expose RDF-star for convenience.

Reason:

- named graphs are easy to partition and audit
- they fit provenance naturally
- they work well with existing triple-store tooling
- RDF-star is nice for statement annotations, but should remain optional

## Practical workflow

1. SiliconDB ingests raw text / extracted claims.
2. It assigns probabilities and energy scores.
3. High-value claims are sent to HornDB.
4. HornDB tries to prove or disprove them.
5. HornDB writes verdicts and proof trees.
6. SiliconDB consumes verdicts to recalibrate ranking and future candidate generation.

## Bottom line

This is a clean division of labor:

- **SiliconDB** = scout / rank / prioritize
- **HornDB** = verify / explain / certify

That preserves provenance and correctness while still taking advantage of probabilistic triage.
