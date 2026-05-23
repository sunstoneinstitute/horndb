# Building a Modern RDF Reasoner: State of the Art and a Feasibility Assessment for Sunstone Institute

## TL;DR

- **A small team can build a credible OWL 2 RL / Datalog reasoner that competes with GraphDB on cost-per-feature and on backward-chaining query latency, but matching RDFox's materialization throughput (6.1 M triples/sec, ~37 bytes/triple, 9.2 B triples on a single SPARC T5-8 with 128 cores and 4 TB RAM, per Nenov et al., ISWC 2015) is a 3–5 engineer-year undertaking and not the right MVP.** The competitive angle is hybrid execution (materialize the schema/transitive closure subset, backward-chain the rest) on modern unified-memory hardware (Grace-Hopper / MI300A / Apple Silicon) where RDFox's pure main-memory shared-everything model leaves performance on the table.
- **Adopt a tiered correctness/performance benchmark stack**: W3C OWL 2 Test Cases + SPARQL 1.1 Test Suite as the *correctness* gold standard, LDBC Semantic Publishing Benchmark (SPB) at SF3/SF5 as the *primary performance* benchmark (it is the only industrial benchmark designed for reasoning-enabled triple stores; co-designed with Ontotext and BBC), supplemented by LUBM/UOBM for OWL profile coverage and the ORE 2015 corpus (1,920 ontologies + 47 user submissions, Zenodo record 18578) for DL/EL stress tests.
- **The 3–5 highest-leverage bets**: (1) worst-case-optimal join (Leapfrog Triejoin) backward-chaining engine with magic sets; (2) GraphBLAS-based transitive closure for the materialized subset (subClassOf, subPropertyOf, sameAs equivalence classes); (3) DBSP/differential-dataflow-style incremental maintenance instead of RDFox's DRed/B/F counting; (4) HBM-resident hot tier with CXL/NVMe-backed cold tier and dictionary-encoded columnar storage; (5) Soufflé-style ahead-of-time compilation of the OWL 2 RL rule set to native parallel C++ (or Rust) rather than interpreting it.
- **ML/embedding augmentation is a force multiplier, not a replacement for symbolic reasoning.** The pattern that fits Sunstone's PROV-O/correctability constraints is *symbolic reasoner = source of truth and proof; ML = performance optimizer and candidate generator*. Expected wins: 10–100× on entity-resolution / owl:sameAs candidate generation, 2–10× on query planning, 2–5× reduction in materialization footprint via predicted-hot-set triage. Replacing exact reasoning with embeddings outright is not viable for a provenance-oriented system. LLMs belong at the LLM→SPARQL translation boundary, not inside the reasoning core.

## Key Findings

### 1. The performance bar is RDFox; the feature/cost bar is GraphDB

RDFox is a centralised, main-memory, parallel Datalog engine. Its public numbers from the ISWC 2015 paper (Nenov, Piro, Motik, Horrocks, Wu, Banerjee, *RDFox: A Highly-Scalable RDF Store*, LNCS 9367, pp. 3–20) remain the canonical benchmark to beat: **87× speedup on 128 cores, 9.2 B triples max, 36.9 bytes/triple, 1 M triples/sec import, 6.1 M triples/sec reasoning**. RDFox computes a full materialization (forward chaining) and answers SPARQL over the closure; it supports incremental addition/deletion via the FBF counting algorithm (Motik et al., AAAI 2015; later "Maintenance of Datalog Materialisations Revisited," *Artif. Intell.* 269, 2019, 76–136). It is *not* primarily a backward-chaining engine — Oxford Semantic's own FAQ explicitly notes "Materialisation … provides performance benefits on the order of 100–1000× [over backward chaining]." owl:sameAs is handled by a rewriting/representative algorithm that avoids the equivalence-class blow-up.

**Strategically important update**: Oxford Semantic Technologies was acquired by Samsung Electronics on 16 July 2024 (Samsung Global Newsroom: *"Samsung Electronics Co., Ltd. today announced that it has signed an agreement to acquire Oxford Semantic Technologies, a UK startup specializing in knowledge graph technology"*). RDFox is now a Samsung subsidiary product, not an independent UK vendor. This changes the licensing/strategic landscape: long-term roadmap, pricing, and EU data-sovereignty posture are now Samsung's call.

GraphDB (Ontotext / Graphwise, formerly OWLIM) is a file-backed Java SAIL on the RDF4J framework with the proprietary TRREE rule engine. It supports RDFS, OWL Horst, OWL 2 RL, OWL 2 QL, and custom rulesets. Ontotext's own documented numbers (graphdb.ontotext.com/documentation/11.2/benchmark.html) for LDBC SPB-256 show **RDFS-Plus optimized expansion ≈ 1:1.6** and **OWL 2 RL expansion ≈ 1:3.2** (515 M implicit statements). GraphDB scales to "tens of billions of statements on a single server" but is markedly slower per core than RDFox on materialization-heavy workloads. The 2023 LDBC-audited GraphDB SPB SF5 (~1 B edges) run achieved 158 queries/sec single-server with 24 read agents (Ontotext audit document, LDBC).

Other systems in the landscape:
- **Stardog, AllegroGraph, Virtuoso**: backward-chaining/query-rewriting OWL 2 RL fragments, file-backed. Better for transactional workloads than for full materialization.
- **Jena (TDB2 + rules)**: serviceable RDFS/OWL Lite, not competitive on scale.
- **Blazegraph**: archived; foundational influence on Amazon Neptune. Not a target.

### 2. Datalog engines applicable to RDF reasoning

- **Soufflé** (Oracle Labs, now community): compiles Datalog to parallel C++ via staged Futamura projections; semi-naïve evaluation, specialised tries/B-trees (PACT'19, PPoPP'19, VLDB'18 index selection). Production-grade for static analysis (Doop/DaCapo). Soufflé's `EQREL` data structure (PACT'19) is directly applicable to owl:sameAs.
- **Nemo** (TU Dresden, Markus Krötzsch's group): Rust-based in-memory engine, hierarchically-sorted column-based tables (descendant of VLog's design), supports existential rules (TGDs), stratified negation, aggregates, datatypes. The Ivliev, Gerlach, Meusel, Steinberg, Krötzsch KR 2024 paper (pp. 743–754) **won the Best Paper Award in the "KR in the Wild" track at KR 2024 in Hanoi** (per ICCL TU Dresden announcement) — direct evidence the underlying design is current state of the art.
- **VLog** (Urbani et al., predecessor of Nemo): vertical storage layout, knowledge-graph focused.
- **Vadalog** (Bellomarini, Sallinger, Gottlob — Oxford/Bank of Italy): Warded Datalog±, captures PTIME with existential quantification; supports OWL 2 QL; uses the Volcano iterator model. PVLDB 11(9):975–987, 2018.
- **DDlog** (VMware Research, Ryzhyk): differential-Datalog over differential-dataflow; the engineering backbone behind DBSP.
- **DBSP** (Budiu, McSherry, Ryzhyk, Tannen): general algebraic framework for incremental view maintenance over rich query languages including monotonic and non-monotonic recursion; PVLDB 2023 / *VLDB Journal* 2025. Directly relevant as a *better* incremental-maintenance substrate than DRed/counting.
- **LogicBlox / RelationalAI**: commercial; origin of Leapfrog Triejoin (Veldhuizen, ICDT'14).
- **DaRLing** (DeMaCS Calabria): freely-available Datalog rewriter targeting OWL 2 RL + SPARQL.
- **Inferray** (Subercaze et al., PVLDB 9(6), 2016): vertical-partitioning, sort-merge join, MSD-radix sort on 64-bit pairs, custom counting sort, dedicated transitive-closure storage. Reported 21.3 M triples/sec on a transitivity chain of 25,000 nodes — **142× faster than RDFox and 590× faster than OWLIM on a 2,500-node chain**, hardware-bandwidth-limited.

### 3. OWL DL / EL reasoners — the correctness substrate

- **ELK** (Kazakov, Krötzsch, Simančík, U. Ulm/Oxford): the reference parallel OWL 2 EL reasoner. The "Incredible ELK" paper (*Journal of Automated Reasoning* 2014) reports classification of the full SNOMED CT (~300,000 classes) "in as little as 5 seconds on a quad-core computer"; the ELK project page (korrekt.org) currently advertises "less than 4 seconds on a modern laptop." Apache 2.0. Used by SNOMED tooling and the OBO foundry.
- **Konclude** (Steigmiller, Liebig — derivo; Glimm — Ulm): OWL 2 DL, hybrid tableau + saturation, C++, multi-CPU. **Won 4 of 6 tracks at ORE 2015** (all 3 OWL DL tracks plus OWL EL Realisation). The reference for OWL 2 DL today.
- **HermiT** (Glimm, Horrocks, Motik, Stoilos), **Pellet/Openllet**, **FaCT++**: classical DL reasoners, slower but standards-complete.
- The ORE 2015 corpus (Matentzoglu & Parsia, Zenodo record 18578, 1,920 ontologies + 47 user submissions) and competition framework (github.com/ykazakov/ore-2015-competition-framework) are the canonical DL/EL stress test.

### 4. Bottlenecks — where time and money actually go

For OWL 2 RL / Datalog materialization at scale, in our reading of the literature and benchmarks:

1. **Recursive closure** (subClassOf, subPropertyOf, transitive properties, sameAs). Inferray's two-orders-of-magnitude lead over RDFox/OWLIM on pure transitive closure is largely about *layout* — vertical partitioning + sort-merge + dense-renumbered connected components.
2. **owl:sameAs equivalence classes**. Naïve handling causes quadratic blow-up; RDFox's representative-rewriting and Soufflé's EQREL are the published state-of-the-art.
3. **Multi-way joins on cyclic / star patterns**. Classical binary-join plans are sub-optimal; Leapfrog Triejoin (Veldhuizen, ICDT'14) is worst-case-optimal up to a log factor and the Hogan et al. (ISWC'19) "A Worst-Case Optimal Join Algorithm for SPARQL" paper demonstrates 1–2 orders of magnitude improvement on Jena.
4. **Dictionary encoding overhead**. Every URI/literal must be interned to a 64-bit ID; RDFox uses two-keys indexes per (S,P,O) triple list. The encoding hot path is itself a memory-bandwidth bottleneck.
5. **Memory bandwidth, not compute**. The Inferray paper's cache-miss/TLB-miss/page-fault analysis is decisive — once the data structure is right, modern Xeon/EPYC chips become the bottleneck on DDR5 bandwidth (~400 GB/s/socket), not on instruction throughput.
6. **Skew** (rdf:type, owl:Class). High-degree predicates dominate the inner loop and create severe load imbalance in naïve parallel evaluation.
7. **I/O when data exceeds RAM**. RDFox is explicitly main-memory and refuses to spill; GraphDB's file-based approach pays for this with much lower per-core throughput.

### 5. Tricks from databases / AI kernels / graph processing worth applying

- **Worst-case-optimal joins**: Leapfrog Triejoin (Veldhuizen 2014) and the Hogan et al. (Springer ISWC'19) SPARQL adaptation; recent **cuMatch** (SIGMOD 2025) demonstrates WCOJ on GPU with trie-based intermediate storage.
- **Vectorized execution**: DuckDB's STANDARD_VECTOR_SIZE=2048 model with SIMD over data chunks is directly transferable to triple-pattern matching (the per-tuple overhead in a Volcano-style iterator dominates at ~2 ns/tuple for predicates).
- **GraphBLAS**: SuiteSparse:GraphBLAS (Davis, *ACM TOMS* Alg. 1000, 2019; Alg. 1037, 2023) — semiring-based BFS/transitive-closure as iterated matrix-matrix or matrix-vector multiply on (∨,∧) or (min,+) semirings. Backs **RedisGraph/FalkorDB** in production. Davis reports MATLAB R2021a's `C=A*B` is up to 30× faster after the SuiteSparse:GraphBLAS rewrite. The Anti-Section Transitive Closure (Green, Du, Bader, IPDPS'21) shows GPU transitive closure at scale on the same algebra.
- **GPU subgraph matching**: **GSI** (Zeng et al., ICDE'20), **cuTS** (Xiang et al., SC'21, trie-based, multi-GPU), **STMatch** (Wei & Jiang, SC'22, stack-based DFS), **EGSM** (Cuckoo-trie filtering, SIGMOD'23). The STMatch paper (doi:10.1109/SC41404.2022.00058) reports "**up to 3385× speedups with an average of 694×**" over cuTS on RTX 3090. These are subgraph-isomorphism systems, not full SPARQL — but the kernels are directly reusable for triple-pattern joins.
- **Differential / incremental dataflow**: Naiad (SOSP'13), Differential Dataflow (CIDR'13), Shared Arrangements (PVLDB Vol 13, 2020), DBSP (PVLDB'23/VLDBJ'25). Materialize Inc.'s production deployment is the best evidence this is practical at scale.
- **Compressed storage**: HDT (Header-Dictionary-Triples), k²-trees, Roaring bitmaps, Elias-Fano. RDFCSA (compressed self-index for triples) competitive on binary joins.
- **Modern Datalog optimization**: semi-naïve evaluation, magic sets / demand transformation, subsumptive tabling, factorized representations (F-IVM, Olteanu et al.).

### 6. Hardware primitives that shift the design space

- **Unified memory at HBM bandwidths**. NVIDIA H200 (141 GB HBM3e, 4.8 TB/s), AMD MI300X (192 GB HBM3, 5.3 TB/s), B200 (180 GB HBM3e, 7.7 TB/s). MI300A is the more interesting design for graph workloads — CDNA3 + Zen4 + unified HBM. GH200/GB200 NVL72 give NVLink-C2C coherence between CPU and GPU. **Memory bandwidth is roughly 10× DDR5 per socket — this is exactly the bottleneck the RDF reasoning literature identifies.**
- **CXL 2.0/3.0** memory pooling and tiering. Recent papers (HybridTier/FreqTier, ASPLOS'25; "GPU Graph Processing on CXL-Based Microsecond-Latency External Memory," 2023) show CXL-attached DRAM tier works for graph analytics with frequency-based hot-page identification — important for the "doesn't fit in HBM" case.
- **GPUDirect Storage / BaM**: direct NVMe-to-GPU DMA at PCIe Gen5 bandwidths (~14 GB/s/drive); BaM (Big Accelerator Memory) shows GPU-initiated I/O at >10× CPU-driven baseline.
- **Tensor cores for sparse algebra**: cuSPARSE, 2:4 sparse tensor cores on Ampere/Hopper, FP8 on H100/B200. Useful for the Boolean/min-plus GEMM steps in GraphBLAS-style closure.
- **NPUs in workstations**: AMD XDNA, Apple Neural Engine, Intel NPU — programmable via IREE/MLIR but not currently used in production reasoners. Speculative for our use.
- **Roofline reality**: for pointer-chasing graph workloads on commodity x86, achieved bandwidth is typically 20–40% of peak; HBM with ~5 TB/s gives an order-of-magnitude headroom *if* the data structure is dense and predictable.

### 7. Existing GPU/modern-hardware reasoners — limited, but instructive

- **Cichlid** (Gu et al., IPDPS 2015): RDFS/OWL on Spark, *not* GPU despite the name. Distributed forward chaining; not directly comparable to RDFox.
- **Inferray** (Subercaze, Gravier, Chevalier, Laforest, PVLDB 9(6), 2016): cache-friendly SIMD-friendly *CPU* implementation; the methodological win is data layout (vertical partitioning + sort-merge), not parallel hardware.
- **TripleID-Q** (Chantrapornchai & Choksuchat, 2018): GPU SPARQL query processing on compact triple representation — research-grade, no reasoning.
- **GSI / cuTS / EGSM / STMatch / cuMatch**: GPU subgraph isomorphism systems; relevant kernels but not reasoners.
- **GraphBLAS reasoning**: no production OWL reasoner uses GraphBLAS as the substrate, but RedisGraph/FalkorDB and the LAGraph library prove the substrate is production-viable for graph queries. **This is the most under-exploited opportunity in the literature.**
- **No production reasoner uses TPUs or sparse tensor cores.** This is open territory but speculative.

### 8. Benchmarks — recommendation for Sunstone

| Layer | Suite | Why |
|---|---|---|
| **Correctness, normative** | W3C OWL 2 Test Cases (owl.semanticweb.org/page/Test_Cases) + OWL 2 Profiles spec normative tables (Tables 4–9, OWL 2 RL/RDF rules) | Only standards-defined conformance; required for any claim of "OWL 2 RL compliant." |
| **Correctness, SPARQL** | W3C SPARQL 1.1 Test Suite (w3c.github.io/rdf-tests/sparql/sparql11/) | Required for SPARQL 1.1 conformance; includes entailment regime tests. |
| **Correctness, DL stress** | ORE 2015 corpus (Zenodo record 18578, 1,920 ontologies + 47 user submissions) + framework (github.com/ykazakov/ore-2015-competition-framework) | The de facto reasoner-stress corpus; covers consistency, classification, realisation, OWL DL + EL. |
| **Performance, primary** | **LDBC Semantic Publishing Benchmark (SPB) v2.0** (ldbcouncil.org/benchmarks/spb/), SF3 + SF5 | Only industrial benchmark *designed* for reasoning-enabled triple stores; co-designed with BBC; uses OWL 2 RL workload; provides both read throughput and update throughput; recommended by Ontotext as their primary public benchmark. |
| **Performance, profile coverage** | LUBM (AllegroGraph's published LUBM-8000 run records exactly **1,105,993,401 triples** across 160,007 N-Triples files totalling 155 GB) + UOBM | Synthetic, well-understood; LUBM exercises basic OWL Lite reasoning, UOBM exercises OWL DL features. |
| **Performance, real-world** | SNOMED CT (EL, ~300 K classes), Gene Ontology, UniProt subset (1.5 B triples), Reactome, ChemBL | Real ontology shape and skew; UniProt is the standard "big bio" stress workload. |
| **Performance, RDF query** | WatDiv, BSBM, SP²Bench | SPARQL-only; useful for non-reasoning regressions. |

**Concrete recommendation**: Adopt the W3C OWL 2 Test Cases + SPARQL 1.1 Test Suite as the gold standard for correctness (any failing test is a release blocker), the ORE 2015 corpus as a continuous-integration stress harness, and LDBC SPB SF3/SF5 as the headline performance benchmark (because Ontotext publishes audited SF3/SF5 results — direct A/B comparability). LUBM-8000 (1.1B triples) and UOBM are useful secondary performance benchmarks for OWL-profile-specific regressions. WatDiv is useful for pure-SPARQL diversity testing.

### 9. Feasibility verdict

**Can a small/medium team beat RDFox on its own ground (pure main-memory single-server materialization)?** No, not within a reasonable budget. RDFox represents ~15 person-years of Oxford KR group work; the FBF/B/F counting algorithms and the sameAs-rewriting integration are *the* hard problems they spent that effort on. Re-deriving them is not a good use of resources.

**Can a small/medium team build a system that wins on (a) Sunstone's actual 60/40 backward/forward chaining mix and (b) cost-per-TB at multi-billion-triple scale?** Yes — there is genuine architectural headroom:

1. **RDFox's main-memory shared-everything design wastes HBM and CXL.** A reasoner that uses HBM (≈5 TB/s) for the hot working set (predicate-partitioned triples + sameAs equivalence classes), DDR5 for the warm tier, and CXL-attached or NVMe-backed cold storage should outperform RDFox on $/triple by an order of magnitude. The Inferray and Materialize-style differential-dataflow numbers suggest this is real.
2. **The 60% backward-chaining workload is exactly where RDFox is weakest.** RDFox is designed for materialize-then-query; their own FAQ admits backward chaining gives up the 100–1000× materialization speedup. A WCOJ-based backward-chaining engine with magic sets and demand transformation would land in the territory of Stardog/Virtuoso but with much better forward-chaining ground.
3. **DBSP/differential-dataflow is a strictly better incremental-maintenance substrate than DRed or counting.** McSherry, Ryzhyk, Tannen (PVLDB'23, VLDBJ'25) demonstrate this for general SQL+Datalog. Materialize Inc. ships it in production. Soufflé's PACT'19 EQREL handles owl:sameAs cleanly.
4. **GraphBLAS for closure subset.** subClassOf, subPropertyOf, owl:sameAs, transitive-property closure are *exactly* the cases where (∨,∧)/(min,+) semiring matrix-matrix multiply on SuiteSparse:GraphBLAS or LAGraph is competitive with hand-rolled rule evaluation.

**Architectural recommendation (MVP, 12–18 months, 3–4 engineers)**:

- Backend in Rust (memory safety + zero-cost abstractions + Apache Arrow interop). Nemo's Rust design is an existence proof.
- Storage: predicate-partitioned, dictionary-encoded, columnar; tries for index access (Leapfrog requirement); Roaring bitmaps for set operations; HDT-style compressed cold tier.
- Joins: Leapfrog Triejoin as the primary multi-way join, vectorized over Arrow chunks at DuckDB-sized (2048) batches; binary hash join fallback for ground patterns.
- Forward chaining: semi-naïve evaluation with delta tables; OWL 2 RL rules compiled to native Rust code (Soufflé-style ahead-of-time translation, no rule interpreter).
- Transitive closure / equivalence classes: SuiteSparse:GraphBLAS via the C ABI for the schema-level closure (subClassOf, subPropertyOf, sameAs).
- Backward chaining: SLG-resolution / magic-sets rewriter on top of the same WCOJ executor.
- Incremental maintenance: DBSP-style stream of differences (Z-set semantics) rather than DRed/counting.
- Hardware target: AMD MI300A or NVIDIA GH200 single-node for development; CXL-attached DRAM tier (Astera Labs Leo or equivalent) for production; NVMe Gen5 cold tier via io_uring.

**Highest-risk unknowns**:

1. SPARQL 1.1 *property paths* with reasoning interaction — RDFox has spent years on this; under-specified in the standard.
2. Custom datatypes / OWL 2 datatype reasoning — non-trivial in any engine.
3. Mixed workload (concurrent updates + queries) under MVCC with incremental materialization — the integration of DBSP with point queries is research-frontier.
4. Skew handling on rdf:type — every reasoner has bespoke code for this; no clean abstraction.

**Build-vs-buy economics**:

- RDFox commercial licensing prices are not publicly listed; Oxford Semantic Technologies (now Samsung) has traditionally operated under direct sales contracts. Any specific figure should be confirmed via direct quote from Oxford Semantic / Samsung.
- Graphwise (the rebranded Ontotext) likewise does not publish per-server list prices publicly. The AWS Marketplace listing for *GraphDB Enterprise Edition* states: "Pricing and entitlements for this product are managed through an external billing relationship between you and the vendor." G2 records only **GraphDB Free at $0**; no independently verified Enterprise list price exists in the public record. Treat any quoted figure as an industry-rumour datapoint, not a publishable price.
- A 3-engineer team for 18 months at fully-loaded €300K/year ≈ €1.35M total — this beats commercial licensing only if (a) Sunstone has multiple production deployments, (b) the product can be open-sourced and generate ecosystem value, or (c) the data sovereignty / hardware-target argument is binding.
- **European open-source data sovereignty consideration**: With Oxford Semantic now under Samsung ownership (16 July 2024 acquisition), RDFox is no longer a UK-independent product; it is now subject to Samsung corporate direction. GraphDB/Graphwise is Bulgarian (EU). An open-source EU-developed reasoner is a defensible strategic asset given EU Data Act and AI Act trajectories, but should not be the *primary* business justification.

## Details

### Detailed sourcing on each system

**RDFox** (Oxford Semantic Technologies, acquired by Samsung Electronics 16 July 2024):
- ISWC 2015 paper (LNCS 9367, pp. 3–20) — confirmed verbatim: "achieving speedups of up to 87 times, storage of up to 9.2 billion triples, memory usage as low as 36.9 bytes per triple, importation rates of up to 1 million triples per second, and reasoning rates of up to 6.1 million triples per second."
- AAAI 2014 (Motik, Nenov, Piro, Horrocks, Olteanu): parallel materialisation, triple-at-a-time semi-naïve variant.
- AAAI 2015 (Motik et al.): incremental maintenance (FBF — Forward/Backward/Forward).
- 2015 ISWC sister paper: handling owl:sameAs via rewriting.
- *Artif. Intell.* 269 (2019): "Maintenance of Datalog Materialisations Revisited" — current state of the art on incremental.
- Indexing: simple TwoKeysIndex per triple list; complex scheme partitions subject/object lists by predicate.

**GraphDB**:
- Documentation (graphdb.ontotext.com/documentation/11.3): "Manages tens of billions of RDF statements on a single server" (Ontotext, 2024); file-based indexes; RDFS, OWL 2 RL, OWL 2 QL.
- SPB benchmarks: SPB-256 with OWL 2 RL ruleset → ~515M implicit statements, 1:3.2 expansion ratio.
- 2023 LDBC-audited SF5 (~1B edges) run: 158 queries/sec, 24 read agents, single server.

**Soufflé**: Jordan, Scholz, Subotić (CAV'16); compiles via Futamura projection to parallel C++. EQREL (PACT'19) for parallel equivalence relations — directly applicable to owl:sameAs. Used in production for Doop (Java points-to), DDISASM, Gigahorse (smart contract analysis), VANDAL.

**Nemo** (Ivliev, Gerlach, Meusel, Steinberg, Krötzsch, KR 2024): Rust + WebAssembly + VSCode integration; hierarchically-sorted columnar tables (VLog-descendant); SPARQL-compatible RDF support; provides computation traces (proof trees). The 2024 KR paper won the Best Paper Award in the "KR in the Wild" track at KR 2024 in Hanoi, per ICCL TU Dresden's announcement.

**Vadalog** (Bellomarini, Sallinger, Gottlob, PVLDB 11(9):975–987, 2018): Warded Datalog±; PTIME data complexity; Volcano iterator model; commercial use at Bank of Italy and Oxford spinout. iWarded benchmark generator (Baldazzi et al.).

**Inferray** (Subercaze et al., PVLDB 9(6):468–479, 2016): RDFS, ρDF, RDFS-Plus. On a 25,000-node transitivity chain: 313M triples generated in 14.7 s (21.3M triples/sec). For 2,500-node chain: 142× faster than RDFox, 590× faster than OWLIM. Hardware: single Intel desktop, ≤16 GB RAM. Cache-miss-driven design.

**ELK** (Kazakov, Krötzsch, Simančík): concurrent classification for EL ontologies (ISWC 2011); "The Incredible ELK" paper (*J. Automated Reasoning* 2014) reports SNOMED CT classification "in as little as 5 seconds on a quad-core computer." The current project page (korrekt.org/page/ELK_Reasoner) advertises "less than 4 seconds on a modern laptop." Incremental reasoning since 0.4.0 (2013).

**Konclude**: Steigmiller, Liebig, Glimm, *J. Web Semantics* 27 (2014) 78–85; OWL 2 DL (SROIQV(D)) with nominal schemas; hybrid tableau + saturation; **won 4 of 6 tracks at ORE 2015** (ORE 2015 report, Parsia et al., *J. Automated Reasoning* 59:455–482, 2017). The ORE 2015 report verbatim: "Out of the six tracks, four were won by the new hybrid reasoner Konclude, and two (OWL EL Consistency and OWL EL Classification) were won by ELK."

**SuiteSparse:GraphBLAS** (Davis): Algorithm 1000 (*ACM TOMS* 2019) and Algorithm 1037 (*ACM TOMS* 2023). Backs RedisGraph/FalkorDB. Drives MATLAB R2021a's `*` operator (Davis: "C=A*B is now up to 30× faster"). RedisGraph 2.0 paper (Cailliau et al., IPDPSW'19) shows 6× latency improvement, 5× throughput on 6-hop queries vs RedisGraph 1.2 by moving to SuiteSparse:GraphBLAS 3.2.0 with OpenMP. Anti-Section Transitive Closure (Green, Du, Bader, IPDPS'21) shows GPU TC at scale on the same algebra.

**Leapfrog Triejoin**: Veldhuizen, ICDT'14 (originally arXiv:1210.0481, 2012). Worst-case-optimal up to a log factor per NPRR bound. Implemented in LogicBlox / RelationalAI. Hogan et al. (Springer LNCS 11778, ISWC'19): SPARQL adaptation, integrated into Apache Jena.

**Differential Dataflow / DBSP**: McSherry, Murray, Isaacs (CIDR 2013); Naiad (SOSP'13). DBSP: Budiu, McSherry, Ryzhyk, Tannen, PVLDB 2023 and *VLDB Journal* 2025. Materialize Inc. is the commercial vehicle. Frank McSherry blog and timely-dataflow GitHub are the practitioner references.

**WebPIE**: Urbani, Kotoulas, Maassen, van Harmelen, Bal (ESWC 2010 best paper; *J. Web Sem.* 2012). 100 billion triples LUBM closure on 64-machine Hadoop cluster (DAS-4 at VU Amsterdam). MapReduce-based — historical interest; not a competitive design today.

### Hardware reality check

- NVIDIA H100 SXM: 80 GB HBM3 @ 3.35 TB/s, 700 W.
- NVIDIA H200 SXM: 141 GB HBM3e @ 4.8 TB/s, 700 W.
- NVIDIA B200: 180 GB HBM3e @ 7.7 TB/s, 1000 W; FP4/FP6 support; NVLink 5 (1.8 TB/s/GPU).
- AMD MI300X: 192 GB HBM3 @ 5.3 TB/s. MI300A: integrated Zen4 + CDNA3, unified HBM — best architecture for graph workloads in 2024–25.
- MI350X: 288 GB HBM3e @ 8 TB/s, shipping 2025.
- DDR5 server: ~400 GB/s/socket — an order of magnitude less than HBM3.
- PCIe Gen5 NVMe: ~14 GB/s/drive — usable for cold tier via GPUDirect Storage.

### CXL outlook

Memory tiering systems (HybridTier ASPLOS'25, FreqTier) show CXL-attached DRAM is viable for graph analytics with frequency-based hot-page identification, but adds 100–200 ns latency. Graph workloads (Page Rank, XGBoost) show "over 90% and 50% of initially hot pages are no [longer hot after some time window]" — dynamic tiering policies matter. For RDF reasoning, the hot set is typically the schema TBox + indexed predicates; the cold set is the bulk ABox.

## Recommendations

**Stage 1 — Feasibility prototype (3 months, 1–2 engineers)**:
- Implement Leapfrog Triejoin over Arrow-encoded triples in Rust.
- Add OWL 2 RL rules via Soufflé-style code generation.
- Validate against LUBM-100 and a subset of W3C OWL 2 Test Cases.
- Benchmark vs RDFox and GraphDB on LDBC SPB-256.
- **Go/no-go threshold**: within 3× of RDFox on materialization throughput at SPB-256 → continue.

**Stage 2 — MVP (12 months, 3–4 engineers)**:
- Full OWL 2 RL semantics with normative-table verification.
- SuiteSparse:GraphBLAS closure backend (schema-level).
- DBSP-style incremental maintenance.
- SPARQL 1.1 with backward chaining and magic sets.
- Conformance: W3C OWL 2 Test Cases + SPARQL 1.1 Test Suite passing.
- **Go/no-go thresholds**: ORE 2015 OWL 2 RL fragment: 100% solved; SPB SF3: ≥50% of GraphDB Enterprise throughput; LUBM-8000 (1.1B triples) materialization: within 2× of RDFox.

**Stage 3 — Hardware specialization (12 months, +1–2 engineers)**:
- GPU backend (CUDA / ROCm) for GraphBLAS closure and WCOJ on subgraph patterns (cuMatch-style).
- CXL tiering policy with hot-page tracking.
- Multi-node distributed mode via DBSP timely-dataflow primitives.
- **Win condition**: outperform RDFox on $/billion-triples-materialized and outperform GraphDB on $/query-second on SPB SF5.

**Benchmarks to adopt immediately**:
- *Correctness gold standard*: W3C OWL 2 Test Cases (https://www.w3.org/TR/owl2-conformance/) + SPARQL 1.1 Test Suite (https://w3c.github.io/rdf-tests/sparql/sparql11/). Any failure is a release blocker.
- *Continuous DL stress*: ORE 2015 corpus (https://zenodo.org/records/18578).
- *Performance, primary*: LDBC SPB v2.0 at SF3 and SF5 (https://ldbcouncil.org/benchmarks/spb/).
- *Profile coverage*: LUBM-1000 / LUBM-8000 (1,105,993,401 triples / 155 GB N-Triples) + UOBM-DL.
- *Real-world stress*: SNOMED CT, UniProt subset.

## Caveats

- The numbers cited for RDFox (87×, 9.2B, 36.9 bytes/triple, 1M tps, 6.1M tps) are from the 2015 ISWC paper on a SPARC T5-8 with 128 cores and 4 TB RAM. Contemporary x86 hardware is faster but RDFox has not (publicly) re-published equivalent numbers — direct comparability requires running the current RDFox build yourself.
- GraphDB benchmark numbers (1:1.6 RDFS-Plus expansion, 1:3.2 OWL 2 RL expansion, 158 queries/sec SF5) are from Ontotext's own documentation and the LDBC-audited 2023 SF5 run; an audited number is more reliable than a vendor-published one.
- Inferray's claim of 142× speedup vs RDFox on transitivity chains is specific to *that* benchmark shape (pure transitivity); it does not generalize to OWL 2 RL. Inferray supports only RDFS / ρDF / RDFS-Plus — strictly less expressive than RDFox.
- The ORE 2015 competition is the *most recent* major reasoner competition. There is no published successor at the same scale; SemREC has run smaller editions but is not yet a like-for-like replacement.
- DBSP for non-monotonic Datalog is research-frontier — published 2023; production usage at Materialize is mostly SQL-shaped, not Datalog-shaped. Risk on this dependency is real.
- GPU subgraph-matching numbers (STMatch up to 3385× cuTS, avg 694×, on RTX 3090, Wei & Jiang SC'22) are kernel-level; integrating a GPU kernel into a full SPARQL execution engine has well-documented cost on host-device transfer and result materialization that the kernel papers do not always surface.
- No production OWL reasoner uses GPUs today. The published academic GPU systems (TripleID-Q, gStore-GPU) are research-grade. The proposed GraphBLAS backend is *novel composition* of known parts — execution risk is real.
- **Commercial licensing prices** for RDFox and GraphDB Enterprise are not publicly listed; both vendors operate under direct sales contracts. Any specific figure (€/$ per server per year) we quote should be treated as industry rumour until confirmed by a vendor quote. AWS Marketplace explicitly defers GraphDB Enterprise pricing to "an external billing relationship between you and the vendor." G2 lists only GraphDB Free at $0.
- The Oxford Semantic / Samsung acquisition (16 July 2024) materially changes the strategic landscape; long-term roadmap, pricing posture, and EU data-sovereignty positioning of RDFox are now Samsung's call. This *strengthens*, not weakens, the case for an EU/open-source alternative.
- All performance comparisons assume Linux + recent CPU/GPU drivers; SPARC-specific numbers in the RDFox 2015 paper are historically interesting but not the right baseline today.