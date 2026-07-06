---
status: research-note
date: 2026-06-05
scope: "Competitive landscape: provenance proof + symbolic reasoning"
---

# Competitive landscape: provenance proof + symbolic reasoning

**Date:** 2026-06-05
**Status:** Research note — competitive landscape, informs SPEC-04 (rule engine) and SPEC-08 (ML boundary)
**Question:** Which products/systems combine an *audit-grade / verifiable provenance proof* with *symbolic
reasoning* (rule-based, Datalog, OWL/RDF inference, theorem proving), the way HornDB intends to emit verifiable
justifications for inferred triples?

## TL;DR

The two capabilities HornDB unifies are usually offered separately:

- **Symbolic reasoning** — rule-based inference (RDFS/OWL/Datalog), forward- or backward-chaining, deriving *new* facts.
- **Provenance proof** — a per-inference artifact saying *why* a derived fact holds, ranging from a human-readable
  explanation up to a cryptographically verifiable certificate.

Many systems do both, **but almost all of them mean "explanation"** — a proof tree or justification set for human
or debugging use — **not "cryptographically verifiable proof."** The audit-grade-crypto + recursive-reasoning
combination HornDB targets is essentially a research frontier; **no shipping product occupies it.**

The single most important finding, cross-checked across the whole survey:

> **No system — research or product — provides a cryptographic / ZK proof of a *recursive OWL/Datalog inference
> fixpoint*.** ZK work proves non-recursive *query evaluation*; recursive-reasoning systems (Soufflé, RDFox,
> Scallop) produce *replayable* proofs but not *cryptographic* ones. That intersection is open — and is HornDB's
> defensible niche.

## Tier 1 — Symbolic reasoning + a real derivation proof (closest functional matches)

This is the band HornDB lives in: an inference engine plus a per-triple proof/justification artifact.

| Product | Reasoning | Proof form | Verifiable? |
|---|---|---|---|
| **EYE / cwm** | N3, forward + backward chaining | **N3 proof graph** using the W3C SWAP `reason` vocabulary (`r:Proof`, `r:Inference`) | **Yes — designed for independent machine validation.** Closest in *spirit* to HornDB. |
| **Soufflé** | Datalog, semi-naïve, compiled to C++ | **Proof tree** via `-t explain` / `explain`; lazily reconstructed, default depth-4 with `subproof` for the remainder | Replayable/inspectable, not signed |
| **RDFox** (now Samsung) | Datalog materialization, incremental, stratified negation/aggregation | **Proof tree** with `shortest` / `to-explicit` / `exhaustive` modes | Replayable, not signed |
| **Stardog** | Query-rewriting (backward) over RDFS/QL/RL/EL + SWRL | **Proof tree** = minimal set of asserted statements (a justification set) via `reasoning explain` | Human/debug explanation |
| **Ontotext GraphDB** | Forward-chaining RDFS / OWL-Horst / RL / QL materialization | **Proof plugin** (`proof:explain`): which rule fired + which premises matched; access-control-aware | Open-source plugin, explanation-grade |
| **Nemo / VLog / Rulewerk** | Datalog + existential-rule chase | **Derivation tree** via `--trace`, exportable as **JSON / GraphML** (Evonne visualizer) | Machine-readable, replayable |
| **Apache Jena** | RDFS/OWL + rule reasoner | `getDerivation()` → `RuleDerivation` (rule + matched triples); must enable trace | Debug-grade |
| **Cycorp Cyc** | Argumentation over a large rule/fact KB | **Justification chains** (ground facts + rules + methods), NL drill-down, Query Justification API | Human-auditable |
| **Protégé / OWL Explanation** (Pellet, HermiT, ELK) | OWL 2 DL/EL entailment | **Justifications** — minimal axiom subsets; *laconic*/*precise* refinements | Explanation-grade |

The recurring split: **structural proof objects** — proof/derivation trees (EYE, Soufflé, RDFox, GraphDB, Nemo,
Jena) — vs. **justification sets** — a minimal axiom subset with no rule-firing trace (Stardog, OWL/Protégé). EYE
is the only one explicitly engineered so a *separate* checker can validate the proof — the lineage of
Berners-Lee's "proof layer" of the Semantic Web stack that HornDB is effectively reviving.

## Tier 2 — Algebraic-provenance / neuro-symbolic line (research, formally rigorous)

Here "provenance" is a **semiring annotation / polynomial** rather than a tree. The same algebra
(Green–Karvounarakis–Tannen, *Provenance Semirings*, PODS 2007) specializes to bag semantics, probabilities,
why-provenance, and gradients.

- **Scallop** (`scallop-lang`, PLDI 2023) — Datalog with recursion/negation/aggregation, **built directly on
  provenance semirings**; tracks top-k proofs per fact and makes them differentiable (~45k lines of Rust). The
  cleanest demonstration that the *same* machinery that yields proofs also yields learning — directly relevant to
  HornDB's ML boundary (SPEC-08).
- **ProvSQL** (PostgreSQL extension, VLDB 2018) — rewrites SQL to track a **provenance circuit** evaluable in any
  semiring; also computes probabilities and Shapley values.
- **DeepProbLog / ProbLog** — SLD-resolution proofs compiled to weighted Boolean formulas; neural predicates plug
  into the same proof search.
- **SPARQLprov** — how-provenance polynomials (`N[X]`) for SPARQL via engine-agnostic query rewriting.
- **Semiring provenance for Description Logics / OBDA** (Bourgaux et al.; Calvanese et al. IJCAI 2019) — lifts the
  same annotation algebra onto OWL-style reasoning.
- **xclingo** — derivation trees for ASP answer sets.

Verifiability here is the weaker "commutes with semiring homomorphisms" sense, not cryptographic.

## Tier 3 — Cryptographically verifiable proof + reasoning (the actual frontier)

"Proof" here means a **crypto certificate** — and it's almost entirely research.

- **zkSPARQL / `sparql-noir`** (zksparql.org) — compiles SPARQL into **zero-knowledge circuits (Noir)** over
  signed Verifiable Credentials. Proves correct query *evaluation*.
- **ESWC 2025 selective-disclosure paper** — proves SPARQL result *soundness* via ZK over Merkle-committed signed
  RDF; ~3 orders of magnitude faster than executing-in-circuit.
- **ZKSQL** (VLDB 2023) — ZK-verifiable SQL query evaluation (SQL analogue).
- **VeriDKG** (VLDB 2024) — authenticated data structure (RGB-Trie) + accumulator, blockchain-maintained, for
  verifiable SPARQL over decentralized KGs.
- **OriginTrail DKG** — *shipped product*: RDF "Knowledge Assets" with blockchain-anchored cryptographic digests;
  markets "neuro-symbolic reasoning." Provenance is asset-integrity, not inference-proof.
- **Secure Network Provenance (SNP/SNooPy, SOSP 2011)** — the one genuinely **tamper-evident, signed**
  Datalog-provenance system, for adversarial settings.
- **Datalog proof-carrying authorization** — Binder, SecPAL, Soutei: authorization decisions derived by logic from
  **signed credentials** (`says` operator).
- **Enabling standards** — RDF Dataset Canonicalization **RDFC-1.0** (W3C Rec, May 2024; ≈ URDNA2015) makes signing
  RDF graphs possible; **PROV-O** (OWL2 provenance ontology); **Verifiable Credentials 2.0** (W3C Rec, May 2025) +
  Data Integrity (EdDSA/ECDSA); **IPLD/IPFS** Merkle-DAG content addressing for RDF.

## Where this leaves HornDB

1. **Direct functional peers** (reasoning + inference proof): **EYE, RDFox, Stardog, GraphDB, Soufflé, Nemo, Cyc.**
   Of these, **EYE is the philosophical twin** — independently verifiable N3 proofs from a hybrid forward/backward
   chainer — but EYE is not a SPARQL-fronted, HBM/CXL-targeted store.
2. **No commercial product markets a cryptographically / independently verifiable proof object for derived
   triples.** Stardog/RDFox/GraphDB/Cyc proofs are explanation/audit-grade (replayable), not signed certificates.
   "Neuro-symbolic"/"explainable" marketing (AllegroGraph 8, metaphacts metis, Stardog Voicebox) is LLM-grounding +
   citation, not formal entailment proof — though Voicebox's "Safety RAG" adds a verifiable *execution* gate.
3. **The defensible niche** is the combination nobody ships: **SPARQL 1.1 frontend + OWL 2 RL recursive reasoning +
   a proof artifact that is both replayable and (optionally) cryptographically anchored.** The tractable near-term
   path the research points to is a **Merkle-committed / content-addressed triple store** (RDFC-1.0 canonicalization
   → hash → sign) carrying **proof trees in the EYE/Soufflé style**, with ZK-over-the-rule-engine as the
   long-horizon frontier. **Scallop** is the reference for unifying that same provenance machinery with the ML
   boundary (SPEC-08).

## Confidence notes

- **High confidence:** the reasoning + proof-tree/justification capabilities of EYE, cwm, Soufflé, RDFox, Stardog,
  GraphDB, Nemo, Jena, Cyc, OWL/Protégé; semiring-provenance theory and Scallop/ProvSQL/ProbLog; the W3C standards
  (RDFC-1.0, PROV-O, VC 2.0) and the ZK-SPARQL research cluster.
- **Medium confidence:** several vendor docs (Oxford Semantic, Stardog, GraphDB) returned HTTP 403 to direct fetch
  and were captured via search snippets of the canonical pages. Vadalog's internal "stop-provenance" detail is
  paraphrased from the VLDB paper.
- **Notable absence (high confidence it is a real gap, not just unfound):** no cryptographic proof of a recursive
  OWL/Datalog fixpoint in either products or literature.

## Sources

### Key papers (most relevant to HornDB's verifiable-justification goal)

- **Cuong et al. / ESWC 2025 — "Proving Soundness of SPARQL Query Results Using Selective Disclosure of RDF
  Datasets and Zero-Knowledge Proofs."** *The Semantic Web — ESWC 2025*, Springer LNCS, chapter DOI
  `10.1007/978-3-032-25156-5_16`. Proves SPARQL result *soundness* by proving properties of the queried, signed,
  Merkle-committed RDF dataset rather than executing the query in-circuit — reported ~3 orders of magnitude faster
  than the execute-in-circuit approach; circuit encoding covers a SPARQL 1.1 fragment (BGP, Join, Filter, OPTIONAL,
  UNION, bounded property paths, EXISTS, NOT EXISTS, MINUS). The single closest published result to "verifiable
  proof of SPARQL answers." — https://link.springer.com/chapter/10.1007/978-3-032-25156-5_16
- **Green, Karvounarakis & Tannen — "Provenance Semirings."** PODS 2007. The unifying framework: bag semantics,
  probabilistic/incomplete DBs, and why/how-provenance as one semiring-parameterised algorithm; free semiring =
  provenance polynomials `N[X]`. — https://web.cs.ucdavis.edu/~green/papers/pods07.pdf ·
  https://dl.acm.org/doi/10.1145/3034786.3056125 · Datalog/power-series extension: https://www.cis.upenn.edu/~val/15MayPODS.pdf
- **Li, Huang, Naik et al. — "Scallop: A Language for Neurosymbolic Programming."** PLDI / PACMPL 2023.
  Datalog + provenance semirings + differentiable top-k proofs. —
  https://dl.acm.org/doi/10.1145/3591280 · https://arxiv.org/abs/2304.04812 ·
  https://www.cis.upenn.edu/~mhnaik/papers/pldi23.pdf · https://github.com/scallop-lang/scallop ·
  precursor (probabilistic→differentiable): https://www.cis.upenn.edu/~mhnaik/papers/aiplans21.pdf
- **Zhao, Subotić, Scholz — "Debugging Large-scale Datalog: A Scalable Provenance Evaluation Strategy."** ACM
  TOPLAS 2020 (Soufflé proof trees). — https://dl.acm.org/doi/10.1145/3379446 · https://souffle-lang.github.io/provenance
- **Berners-Lee / Verborgh et al. — EYE reasoner & the Semantic Web "proof layer."** Independently validatable N3
  proofs via the SWAP `reason` vocabulary. — https://eyereasoner.github.io/eye/ · https://josd.github.io/Papers/EYE.pdf

### Foundations & academic provenance / neuro-symbolic

- ProbLog (theory + tabled-proof inference) — https://arxiv.org/pdf/1304.6810 · https://arxiv.org/pdf/1202.3719
- DeepProbLog (neural predicates) — https://arxiv.org/abs/1805.10872 · https://www.sciencedirect.com/science/article/pii/S0004370221000552
- Why-provenance for Datalog (proof-tree classes; complexity) — https://arxiv.org/pdf/2303.12773
- Incremental why-provenance via SAT (AAAI) — https://ojs.aaai.org/index.php/AAAI/article/view/28914/29739
- Revisiting Semiring Provenance for Datalog (KR 2022) — https://proceedings.kr.org/2022/10/kr2022-0010-bourgaux-et-al.pdf
- Semiring provenance for lightweight Description Logics — https://arxiv.org/abs/2310.16472 · ELHr (IJCAI 2020): https://www.ijcai.org/Proceedings/2020/0258.pdf
- Provenance for Ontology-Based Data Access (IJCAI 2019) — https://www.ijcai.org/proceedings/2019/0224.pdf
- Provenance for SPARQL: SPARQLprov + how-provenance polynomials — https://vldb.org/pvldb/vol14/p3389-galarraga.pdf · https://arxiv.org/pdf/1209.0378
- xclingo — explainable ASP (ICLP 2020) — https://arxiv.org/abs/2009.10242 · https://github.com/bramucas/xclingo2
- Reason maintenance — JTMS (Doyle 1979) / ATMS (de Kleer 1986) — https://en.wikipedia.org/wiki/Reason_maintenance · https://dekleer.org/Publications/Problem%20Solving%20with%20the%20ATMS.pdf
- OWL justifications: laconic & precise (Horridge) — http://owl.cs.manchester.ac.uk/research/explanation/ · https://link.springer.com/chapter/10.1007/978-3-540-88564-1_21 · https://cdn.bcs.org/bcs-org-media/2146/dd-2012-matthew-horridge.pdf
- Neuro-symbolic verifiable reasoning framing — VeriCoT — https://arxiv.org/pdf/2511.04662 · Proof-of-Thought — https://arxiv.org/pdf/2409.17270

### RDF/OWL reasoners & proofs

- EYE reasoner — https://eyereasoner.github.io/eye/ · https://josd.github.io/Papers/EYE.pdf · Euler/SWAP: https://eulersharp.sourceforge.net/2006/02swap/
- cwm (`--why`) — https://www.w3.org/2000/10/swap/doc/cwm.html
- RDFox reasoning / explain — https://docs.oxfordsemantic.tech/reasoning.html · https://docs.oxfordsemantic.tech/rdfox-shell.html · scalable reasoning paper: https://www.cs.ox.ac.uk/people/boris.motik/pubs/npmhwb15RDFox-scalable.pdf
- Stardog reasoning explain — https://docs.stardog.com/inference-engine/advanced-reasoning-features · https://docs.stardog.com/stardog-cli-reference/reasoning/reasoning-explain
- Ontotext GraphDB Proof plugin & Provenance plugin — https://graphdb.ontotext.com/documentation/11.3/inference.html · https://graphdb.ontotext.com/documentation/6.6/standard/provenance-plugin.html
- Apache Jena derivation / `RuleDerivation` — https://jena.apache.org/documentation/inference/ · https://jena.apache.org/documentation/javadoc/jena/org/apache/jena/reasoner/rulesys/RuleDerivation.html
- Pellet / ELK reasoners — https://github.com/stardog-union/pellet · https://github.com/liveontologies/elk-reasoner
- Nemo trace / VLog / Rulewerk — https://github.com/knowsys/nemo · https://proceedings.kr.org/2024/70/kr2024-0070-ivliev-et-al.pdf · https://iccl.inf.tu-dresden.de/web/VLog/en · Evonne proof viz: https://imld.de/cnt/uploads/2024-XLoKR-EvonNemo.pdf

### Engines / commercial

- Samsung acquires Oxford Semantic (RDFox) — https://news.samsung.com/global/samsung-electronics-announces-acquisition-of-oxford-semantic-technologies-uk-based-knowledge-graph-startup
- Stardog explainable AI / Voicebox Safety RAG — https://www.stardog.com/blog/explainable-ai-in-stardog/ · https://www.stardog.com/blog/safety-rag-improving-ai-safety-by-extending-ais-data-reach/
- AllegroGraph 8 neuro-symbolic — https://allegrograph.com/press_room/franz-unveils-allegrograph-8-0-the-first-neuro-symbolic-ai-platform-merging-knowledge-graphs-generative-ai-and-vector-storage/ · reasoner: https://franz.com/agraph/support/documentation/reasoner-tutorial.html
- Cambridge Semantics AnzoGraph inferences — https://docs.cambridgesemantics.com/anzograph/v3.1/userdoc/inferences.htm
- Cyc justification / argumentation — https://cyc.com/wp-content/uploads/2021/04/Cyc-Technology-Overview.pdf · http://dev.cyc.com/api/samples/core/query/justification/ · Cyc-vs-LLM (2023): https://arxiv.org/pdf/2308.04445
- TerminusDB (content-addressed lineage) — https://github.com/terminusdb/terminusdb · https://terminusdb.org/docs/terminusdb-explanation/
- metaphacts metis / eccenca + xpSHACL — https://metaphacts.com/introducing-metis · https://eccenca.com/products/enterprise-knowledge-graph-platform-corporate-memory · https://arxiv.org/pdf/2507.08432
- ProvSQL (PostgreSQL provenance circuits) — https://provsql.org/ · https://inria.hal.science/hal-01851538/document
- LogicBlox / Vadalog / DLV / Datomic / DDlog — https://www.cs.ox.ac.uk/dan.olteanu/papers/logicblox-sigmod15.pdf · https://developer.logicblox.com/2010/03/querying-data-provenance/ · https://www.vldb.org/pvldb/vol11/p975-bellomarini.pdf · https://arxiv.org/pdf/cs/0003036 · https://docs.datomic.com/transactions/model.html · https://github.com/vmware-archive/differential-datalog

### Cryptographic / verifiable

- zkSPARQL — https://zksparql.org/ · https://github.com/jeswr/zkSPARQL-bench/
- ESWC 2025 selective disclosure (see Key papers) — https://link.springer.com/chapter/10.1007/978-3-032-25156-5_16
- ZKSQL — verifiable SQL via ZK (VLDB 2023) — https://www.vldb.org/pvldb/vol16/p1804-li.pdf
- VeriDKG — verifiable SPARQL over decentralized KGs (VLDB 2024) — https://www.vldb.org/pvldb/vol17/p912-zhou.pdf · https://dl.acm.org/doi/10.14778/3636218.3636242
- vChain — verifiable queries over blockchain (authenticated data structures) — https://arxiv.org/pdf/1812.02386
- OriginTrail DKG — https://docs.origintrail.io/dkg-knowledge-hub/learn-more/readme/decentralized-knowledge-graph-dkg
- Secure Network Provenance / SNooPy (SOSP 2011) — tamper-evident signed Datalog provenance — https://haeberlen.cis.upenn.edu/papers/snp-tr2.pdf · ExSPAN: https://netdb.cis.upenn.edu/papers/netProvenance.pdf
- Content-addressed RDF — IPLD / IPFS Merkle DAG — https://github.com/ipld/specs/blob/main/IPLD.md · https://docs.ipfs.tech/concepts/merkle-dag/
- RDF Dataset Canonicalization (RDFC-1.0, W3C Rec 2024) — https://www.w3.org/news/2024/rdf-dataset-canonicalization-is-a-w3c-recommendation/ · https://www.w3.org/TR/rdf-canon/
- PROV-O (W3C Rec) — https://www.w3.org/TR/prov-o/
- Verifiable Credentials 2.0 family (W3C Rec 2025) — https://www.w3.org/news/2025/the-verifiable-credentials-2-0-family-of-specifications-is-now-a-w3c-recommendation/ · https://www.w3.org/TR/vc-data-model-2.0/
- Datalog proof-carrying authorization — Binder — https://www.cs.umd.edu/sites/default/files/scholarly_papers/VKolovski_1.pdf · SecPAL: https://courses.cs.vt.edu/cs5204/fall08-kafura/Papers/Security/SecPal-Reference.pdf · Soutei: https://link.springer.com/chapter/10.1007/11737414_10
