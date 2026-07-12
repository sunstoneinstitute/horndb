# State of the art for the HornDB query optimizer (SPEC-23)

**One-line summary:** the cited prior-art reference behind SPEC-23's cardinality estimator
and cost-based join planner — what the state-of-the-art (SotA) techniques are, which to implement now, which
to design the seam for, and which to skip — so the [PLAN-23-03] (statistics/estimator) and
[PLAN-23-04] (cost-based JoinPlanning) plans can name real algorithms instead of stubs.

**When to read this:** before implementing the `Stats` seam, the cardinality estimator, or
the cost-based ordering stage; or when reviewing whether those plans picked the right
technique. It is a *survey*, not a contract — the design decisions live in
[`specs/SPEC-23-unified-ir.md`](../specs/SPEC-23-unified-ir.md); the task breakdowns live in
the PLAN-23-* files. This doc is the "why these algorithms."

**Provenance & confidence.** Synthesized from three engineering briefs commissioned
2026-07-06/07 (Oxigraph `sparopt`, DuckDB, ClickHouse — summarized in SPEC-23 §"Prior art")
and two SotA deep-dives (RDF cardinality estimation; hybrid execution with worst-case
optimal joins, WCOJ). The
deep-dives read paper abstracts and search summaries but **could not fetch full PDFs**
(egress policy blocked arxiv/vldb/acm). So: **authors, venue, and year below are
confirmed**; **exact formulas and constants are reconstructed from abstracts and must be
verified against the primary PDF before being written into executable code** (this is
SPEC-23 §8's "verify-before-cite" rule). Every such spot is flagged ⚠.

HornDB-specific framing: a SPARQL Basic Graph Pattern (BGP) is a subgraph-match problem —
each triple pattern is a hyperedge (an edge that can connect more than two nodes) over its
`{s,p,o}` variables, and every join is over the
one ternary triple relation. That makes the **graph-engine** line of work (Graphflow,
EmptyHeaded) and the **RDF-native** estimators (Characteristic Sets) the most directly
applicable prior art — more so than the general-SQL optimizers (DuckDB/ClickHouse) that
SPEC-23's first research round covered.

---

## Part A — Cardinality / selectivity estimation

The `Stats` seam (SPEC-23 §5.3) should be a **layered, cost-tiered trait** so every
technique below can be added without changing the seam. Ranked tiers:

### Tier 0 — per-predicate counts + NDV (build now)

`total_triples`, `predicate_count(p)`, and per-position distinct-value counts — NDV —
(`distinct_subjects(p)`, `distinct_objects(p)`). This is the DuckDB "almost no statistics"
baseline (base counts + NDV tracked with HyperLogLog, a compact distinct-count sketch) and
enough for heuristic ordering and independence-assumption join estimates. RDF is *better*
placed than SQL: the predicate is usually bound, so per-predicate counts and per-position
NDV come straight from the columnar partitions, and NDV can be maintained as HyperLogLog
(HLL) sketches over dictionary IDs. Storage already exposes
`triple_count()` and `top_predicates(n)`; the NDV half is the gap.

### Tier 1 — Characteristic Sets (the RDF-native SotA — implement early)

**Neumann & Moerkotte, "Characteristic Sets: Accurate Cardinality Estimation for RDF
Queries with Multiple Joins," ICDE 2011.** The single highest-value structure for a
star-heavy SPARQL workload.

- **Structure.** The *characteristic set* of a subject `s` is the set of predicates on its
  outgoing edges: `SC(s) = { p : (s,p,o) ∈ G }`. Real entities cluster into a small number
  of distinct predicate-sets — "implicit types" (a `Person` has `name/age/knows`, a
  `Product` has `price/label`) — so the count of *distinct* characteristic sets is orders of
  magnitude smaller than the subject count. For each distinct set `C` store `count(C)` (how
  many subjects have exactly that set) and, per predicate `p ∈ C`, `occurrences(C,p)` (total
  triples for that predicate over those subjects). The **multiplicity**
  `m(C,p) = occurrences(C,p) / count(C)` is the average number of objects per subject —
  it captures multi-valued predicates, which is where independence assumptions fail.
- **Star-join estimate** (shared subject `?s`, query predicates `P`, objects unbound) ⚠:
  `card ≈ Σ_{C : P ⊆ C} [ count(C) · Π_{p ∈ P} m(C,p) ]`. Summing over co-occurring
  predicate sets captures **predicate correlation for free** — the hard part of RDF
  estimation. A bound object scales its predicate's multiplicity down by that predicate's
  object selectivity (`≈ 1/NDV_object(p)`) instead of `m(C,p)`.
- **Memory.** Proportional to the number of *distinct* CS, not triples — tens of thousands
  in practice, but heterogeneous/schema-free graphs (Wikidata-like) can explode it. Bound
  it by keeping the top-K most frequent sets and folding the rare-set tail into a residual
  bucket estimated from marginal predicate stats. Worst-case memory is capped by K.
- **Maintenance — the sharp edge.** CS is **batch-computed** (scan triples grouped by
  subject) and **not naturally incremental**: one triple insert/delete can move a subject
  between characteristic sets, changing several counters, and a subject's CS is only known
  once all its triples are seen. For HornDB's DBSP/Z-set delta model this means either
  periodic recompute or an incremental scheme keyed on per-subject predicate-set deltas —
  design this alongside the seam, not after (SPEC-23 §8 #1).
- **Non-star extension — SumRDF.** Stefanoni, Motik, Kostylev, "Estimating the Cardinality
  of Conjunctive Queries over RDF Data Using Graph Summarisation," WWW 2018 (the RDFox
  research line — Motik is an RDFox author). Generalizes CS to a graph summary read under
  possible-world semantics (averaging over all data graphs the summary could stand for),
  giving a closed-form expectation for **arbitrary conjunctive
  queries**, not just stars — more accurate on path/complex shapes, heavier to build and
  evaluate. Design the seam so a SumRDF-style whole-graph summary can slot in behind the CS
  interface later.

**Call: implement CS early.** Make the seam's core query `count(C)` + per-predicate
`occurrences(C,p)`. It is the concrete replacement for the `1/16` selectivity stub.

### Tier 2 — pessimistic / degree-sequence bounds (design the seam now; light impl early)

A *guaranteed upper bound* on join output, never an under-estimate. This is the **strategic
fit for a WCOJ engine**: leapfrog triejoin already runs within the AGM bound (the proven
ceiling on join output size — first bullet below) at execution time, so an AGM-based
*optimizer* speaks the executor's native language. The line of work, from simplest to
strongest (and costliest):

- **AGM bound.** Atserias, Grohe, Marx, "Size Bounds and Query Plans for Relational Joins,"
  FOCS 2008 / SICOMP 2013. Tight worst-case output size = `min Π_e |R_e|^{x_e}` over
  fractional edge covers `x` (each variable covered with total weight ≥ 1). Cheap to compute
  per query (see Part B §"AGM as a cost signal").
- **Bound sketch.** Cai, Balazinska, Suciu, "Pessimistic Cardinality Estimation: Tighter
  Upper Bounds for Intermediate Join Cardinalities," SIGMOD 2019. Partitions each join
  attribute's domain by hashing into buckets and sums a per-bucket AGM-style bound using the
  **maximum degree** within each bucket — much tighter than one global AGM bound, still a
  true upper bound.
- **Degree-sequence bound → SafeBound → LpBound** (the modern frontier). Deeds, Suciu,
  Balazinska, Cai, "Degree Sequence Bound for Join Cardinality Estimation," ICDT 2023 (full
  degree sequences, not just max degree); **SafeBound**, SIGMOD 2023 (compresses degree
  sequences + predicate support — the "productionizable" reference impl); **LpBound**,
  PACMMOD 2025 — bound = optimum of a **linear program** over ℓ_p-norms of degree sequences
  plus Shannon inequalities; handles acyclic *and cyclic* queries, selections, group-by;
  reported estimation time a few ms, orders-of-magnitude tighter than traditional estimators
  on subgraph-matching benchmarks. ⚠ (exact LP formulation reconstructed from abstract.)
- **Theory ceiling — PANDA.** Abo Khamis, Ngo, Suciu, PODS 2017 (polymatroid/entropic bound
  with degree constraints). Carries a poly-log factor — **reference, not a planner
  algorithm.** Skip; it only tells you tighter bounds exist when you know degrees.

**Call: expose per-predicate, per-role degree info now** (at minimum `max_degree`;
ideally a compressed degree sequence / its ℓ_p-norms for the subject and object roles of
each predicate). Nearly free given HornDB already reasons about fractional edge covers, and
it gives a robust, non-underestimating estimator that pairs cleanly with leapfrog triejoin.
Bound sketch is the easy early win; SafeBound/LpBound are the design-for target. **Why
bounds matter:** Leis et al. (below) show estimation *error* — specifically catastrophic
under-estimates — is what wrecks plans; an upper bound removes the catastrophic-plan risk.

### Tier 3 — index-based sampling (seam hook now; fallback impl)

**Wander Join** — Li, Wu, Yi, Zhao, "Wander Join: Online Aggregation via Random Walks,"
SIGMOD 2016 (Best Paper). Random walks over the join graph turn each successful/failed walk
into an **unbiased estimate** via its inclusion probability — **no precomputed statistics**,
uses indexes at run time. HornDB's sorted permutation indexes make "walk = index probe"
cheap. The **G-CARE benchmark** (Park et al., SIGMOD 2020) found — surprisingly — that a
simple Wander-Join-style sampler is **consistently more accurate than the summary methods
(Characteristic Sets, SumRDF)** on subgraph matching, because the summaries lean on
independence/uniformity assumptions that break on real graphs. The cost: it runs at query
time, and its estimates have high variance when walks dead-end (selective or empty joins).

**Call: expose a `sample_join(pattern)` hook, implement as a targeted fallback** — the
empirically strongest accuracy backstop for non-star shapes and cold summaries, not the
default (per-query cost + variance).

### Tier 4 — learned estimators (leave the door open; build nothing)

MSCN (Kipf et al., CIDR 2019) and the RDF-specific LMKG exist, but the sober verdict (Wang
et al., "Are We Ready For Learned Cardinality Estimation?", PVLDB 2021) is: more accurate on
*static* data, but **cannot keep up with fast updates, costly to train/infer, brittle to
distribution shift, with unpredictable failures — not production-ready.** That profile is
exactly wrong for an incremental, continuously-materializing reasoner. A learned estimator is
"just another `Stats` impl returning a number," so a clean trait leaves the door open at zero
present cost — but **skip** building one.

### Return type

Estimators should return **either a point estimate or an `(estimate, upper_bound)` pair**,
so the planner can prefer bounds when avoiding catastrophic plans matters (Leis) and point
estimates when tightness matters.

---

## Part B — Join execution, hybrid planning, and variable ordering

**Target shape:** a **Graphflow-style cost-based optimizer** (i-cost + connected-subset
dynamic programming, DP),
wrapped in a **Freitag-style structural hybrid** (WCOJ only for cyclic cores), with the
**Free Join** IR as the north star. HornDB already has both a leapfrog triejoin and a
binary-hash executor — the executor mechanics exist; the *optimizer* is the gap.

### The hybrid decision — structural first (do now)

**Freitag, Bandle, Schmidt, Kemper, Neumann, "Adopting Worst-Case Optimal Joins in
Relational Database Systems," VLDB 2020 (Umbra/HyPer).** The closest match to HornDB's
"WCOJ-vs-hash inside a real optimizer" problem.

- **Hash-trie built at runtime, not a persistent index.** The classic objection to WCOJ —
  it needs sorted/indexed inputs — is answered by building the hash-trie **on demand during
  execution**. HornDB's runtime already does exactly this; no permanent per-order index
  needed.
- **WCOJ only for sub-plans.** Keep the standard binary-join plan; during optimization,
  detect the portions that would produce large intermediate results (the **cyclic cores**)
  and replace only those with a multi-way WCOJ. The rest stays binary.
- **When WCOJ wins:** cyclic queries (triangles, denser patterns) where binary joins
  materialize a large intermediate the final join then shrinks. Acyclic queries → binary
  wins.

**Call: replace HornDB's `≥4 patterns → WCOJ` threshold with a structural decision.** Build
the BGP's variable-connection graph; route **acyclic tree parts → binary hash join** and
**cyclic cores → leapfrog triejoin**. This is a strict correctness improvement — a 6-pattern
star should stay binary; a 3-pattern triangle should go WCOJ — and the current pattern-count
switch gets both wrong. Cheap to compute. This requires HornDB's `ExecutionPlan` to allow
**per-subplan** mode (multi-way WCOJ nodes embedded in an otherwise-binary tree), which it
does not today.

### The cost model — i-cost (do now, the most transplantable idea)

**Mhedhbi & Salihoglu, "Optimizing Subgraph Queries by Combining Binary and Worst-Case
Optimal Joins," VLDB 2019 (Graphflow); extended TODS 2021.** Solves *exactly* HornDB's
problem in a subgraph-match engine structurally identical to RDF BGP matching.

- **Unified additive cost = binary-join cost + intersection-cost ("i-cost").** i-cost charges
  a WCOJ multi-way-intersection step the total size of the adjacency lists that must be read
  and intersected to extend the match by one vertex; binary steps pay the usual build+probe.
  One additive function lets DP compare a WCOJ extension against a binary join **on equal
  footing** — the single most transplantable idea for HornDB's WCOJ-vs-hash decision. ⚠
  (exact i-cost constants reconstructed from abstract.)
- **Connected-subset DP.** DP over *connected* subsets of query vertices (the classic DPccp
  join-ordering algorithm, restricted to connected subqueries): for each connected subset
  keep the cheapest plan; extend by either a
  binary join or a WCOJ multi-way intersection; cost via the combined metric. Exponential in
  query-graph size → **greedy fallback** above a size cutoff (most SPARQL BGPs are small, so
  DP is usually fine).
- **Catalogue estimator.** Precompute statistics for small subgraph patterns (adjacency-list
  sizes, extension selectivities) and compose them. **HornDB's advantage over Graphflow:**
  RDF *predicates*. Key the catalogue on the predicate — per-predicate counts and
  subject/object degree distributions — which is a natural, compact catalogue that feeds
  i-cost directly and reuses the Tier-0/Tier-2 `Stats` surface.

**Call: adopt near-wholesale** — i-cost + binary-cost as the unified function, connected-
subset DP for small BGPs with a greedy fallback, catalogue keyed on predicate. On unified
memory, add a **materialization term** (trie-materialization vs hash-table build is
memory-bandwidth-bound, not the CPU-bound pairwise model DuckDB assumes — SPEC-23 §5.5).

### Variable ordering (do now: greedy; design for: GHD)

Any variable order is worst-case optimal (runs within the AGM bound — generic join / NPRR,
Ngo/Porat/Ré/Rudra, PODS 2012 / JACM 2018; leapfrog triejoin, Veldhuizen, ICDT 2014), so
**ordering is a constant-factor/cost problem, not a correctness one** — don't start from
"AGM-optimal ordering" theory; let the i-cost model drive it.

- **Do now — greedy elimination order.** Order variables **smallest-estimated-multiway-
  intersection first**, falling back to **min-degree** (fewest patterns mention it) when
  estimates are absent. Cheap, no LP. Keeps the current descending-degree heuristic as the
  tie-break seed.
- **Design for — GHD / fractional hypertree width.** For whole-query structure (not a single
  cyclic core), the optimal strategy is a **generalized hypertree decomposition**: a tree of
  bags, each bag solved by WCOJ, bags joined by binary joins; **fractional hypertree width**
  (min over GHDs of the max bag's fractional-edge-cover number) bounds runtime. **EmptyHeaded**
  (Aberger, Lamb, Tu, Nötzli, Olukotun, Ré, SIGMOD 2016 / TODS 2017) compiles the query to a
  min-width GHD as its logical plan, then generates code for the loops and SIMD
  set-intersections; within a
  bag generic join gives the attribute order, across bags the tree gives the join order.
  Finding min-fhw GHD is NP-hard but tractable by width-pruned search on the small
  hypergraphs of real queries.
- **Later — adaptive.** ADOPT (Wang et al., VLDB 2023) reorders attributes *at runtime* by
  regret minimization when the static estimate is wrong; Graphflow has a lighter adaptive
  variant. Advanced; gate on evidence that static estimates are unreliable in practice.

### AGM as a cheap cost signal (do now)

For a BGP, build the hypergraph (variables = nodes, triple patterns = hyperedges) and solve
`minimize Σ_e x_e·log|R_e|  s.t.  Σ_{e∋v} x_e ≥ 1 ∀v,  x_e ≥ 0`; the AGM bound is `exp(min)`.
Arity ≤ 3 and a handful of variables/constraints make this **microseconds per query** —
either a tiny LP or the closed form for common shapes (paths, stars, triangles, cliques). Use
it as an **upper-bound guard and tie-breaker** on candidate WCOJ cores: if a cyclic core's
AGM bound is small relative to the product of its inputs, WCOJ will avoid a large
intermediate → prefer WCOJ.

### The north star — Free Join (design for it; don't build yet)

**Wang, Willsey, Suciu, "Free Join: Unifying Worst-Case Optimal and Traditional Joins,"
SIGMOD 2023.** One framework subsuming *both* binary-hash and WCOJ plans, plus hybrids
strictly better than either.

- **Generalized Hash Trie (GHT):** a tree whose internal nodes are hash maps keyed on a
  *tuple* of attributes. Depth-1 keyed on the whole join key = an ordinary hash table (binary
  join); one attribute per level = the leapfrog trie (WCOJ); everything between is valid. This
  generalizes HornDB's hard binary/WCOJ split into **one structure with a granularity knob**.
- **Free-join plan:** per relation, choose *how finely to factor* its schema into attribute
  groups, and choose the global interleaving (variable order). WCOJ-vs-binary is not a switch
  — it's a per-relation granularity choice.
- **COLT (Column-Oriented Lazy Trie):** builds trie levels lazily (materialize a sub-trie
  only when probed) and column-oriented (intersections over contiguous arrays). This is what
  lets WCOJ **match or beat binary hash joins on acyclic queries**, where plain leapfrog
  triejoin historically loses on cache behaviour — HornDB's exact worry.
- **Planner:** convert a good binary-join plan into a free-join plan, then refine
  factorization where WCOJ helps. ⚠ (the exact refinement heuristic and batching scheme could
  not be extracted from the abstract — confirm against the PDF.)

**Call: adopt the conceptual model now** (granularity-per-relation, one unified structure) so
the Stage-2 planner IR isn't a binary switch; **defer the GHT/COLT executor rewrite** until
the cheaper wins (structural hybrid + i-cost DP) are measured and leapfrog's cache behaviour
is proven to be costing real time.

---

## What this means for SPEC-23

- **`Stats` seam (§5.3):** layer it — Tier 0 counts/NDV → Tier 1 Characteristic Sets → Tier 2
  per-predicate-per-role degree sequences/ℓ_p-norms → Tier 3 `sample_join` hook → Tier 4
  learned door. Return `(estimate, upper_bound)`. Plan CS incremental maintenance against the
  SPEC-06 delta model from day one. Detail in [PLAN-23-03].
- **Cardinality estimator (§5.4):** Characteristic Sets for stars, degree-sequence bounds for
  the upper bound, sampling fallback for non-star/cold. The DuckDB denominator model the plan
  currently names is the Tier-0 baseline, not the destination. Detail in [PLAN-23-03].
- **JoinPlanning (§5.5):** structural cyclic-core hybrid (Freitag) as the first-order routing;
  i-cost + binary-cost unified additive model with connected-subset DP and greedy fallback
  (Graphflow); greedy min-degree/smallest-intersection variable order; AGM LP as a cheap
  guard; Free Join/COLT + GHD as the design-for horizon. Detail in [PLAN-23-04]; later items
  (Free Join executor, LpBound, ADOPT) in [PLAN-23-05].
- **The one thing not to do:** treat WCOJ-vs-hash as a global pattern-count switch, or start
  ordering from AGM-optimal theory. Both are settled wrong by the prior art above.

---

## Sources

Authors/venue/year confirmed via search; full PDFs were **not** reachable this session
(egress policy) — verify exact formulas/constants before writing them into code (⚠ marks in
the body).

**RDF-native cardinality**
- Neumann & Moerkotte, "Characteristic Sets…", ICDE 2011 — https://dblp.org/rec/conf/icde/NeumannM11.html
- Stefanoni, Motik, Kostylev (SumRDF), WWW 2018 — https://arxiv.org/abs/1801.09619
- Park et al. (G-CARE benchmark), SIGMOD 2020 — https://dl.acm.org/doi/10.1145/3318464.3389702

**Pessimistic / bound estimation**
- Atserias, Grohe, Marx (AGM), FOCS 2008 / SICOMP 2013
- Cai, Balazinska, Suciu (bound sketch), SIGMOD 2019 — https://dl.acm.org/doi/10.1145/3299869.3319894
- Deeds, Suciu, Balazinska, Cai (degree-sequence bound), ICDT 2023 — https://arxiv.org/abs/2201.04166
- SafeBound, SIGMOD 2023 — https://arxiv.org/abs/2211.09864
- LpBound, PACMMOD 2025 — https://arxiv.org/abs/2502.05912 · code https://github.com/fdbresearch/LpBound
- Abo Khamis, Ngo, Suciu (PANDA), PODS 2017

**Sampling / learned**
- Li, Wu, Yi, Zhao (Wander Join), SIGMOD 2016 — https://dl.acm.org/doi/10.1145/2882903.2915235
- Kipf et al. (MSCN), CIDR 2019 — https://arxiv.org/abs/1809.00677
- Wang et al., "Are We Ready For Learned Cardinality Estimation?", PVLDB 2021 — https://www.vldb.org/pvldb/vol14/p1640-wang.pdf
- Leis et al., "How Good Are Query Optimizers, Really?", VLDB 2015 — https://www.vldb.org/pvldb/vol9/p204-leis.pdf

**WCOJ execution / hybrid planning / ordering**
- Wang, Willsey, Suciu (Free Join), SIGMOD 2023 — https://arxiv.org/abs/2301.10841
- Freitag, Bandle, Schmidt, Kemper, Neumann (adopting WCOJ), VLDB 2020 — https://www.vldb.org/pvldb/vol13/p1891-freitag.pdf
- Mhedhbi & Salihoglu (Graphflow), VLDB 2019 — https://arxiv.org/abs/1903.02076 · TODS 2021 https://dl.acm.org/doi/10.1145/3446980
- Aberger et al. (EmptyHeaded), SIGMOD 2016 / TODS 2017 — https://arxiv.org/abs/1503.02368
- Ngo, Porat, Ré, Rudra (generic join / NPRR), PODS 2012 / JACM 2018 — https://arxiv.org/abs/1310.3314 (survey)
- Veldhuizen (Leapfrog Triejoin), ICDT 2014 — https://arxiv.org/abs/1210.0481
- Wang et al. (ADOPT), VLDB 2023 — https://www.vldb.org/pvldb/vol16/p2805-wang.pdf

**General-SQL optimizers** (SPEC-23 §"Prior art"): Oxigraph `sparopt`; DuckDB
`src/optimizer/`; ClickHouse `src/Analyzer/` + `src/Processors/QueryPlan/Optimizations/`.
