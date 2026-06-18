# maplib vs HornDB

This page is a related-systems comparison: where [DataTreehouse's maplib](https://github.com/DataTreehouse/maplib)
overlaps with HornDB, where the two diverge, and — the part worth keeping — how
maplib's SPARQL execution model contrasts with the HornDB WCOJ executor
(SPEC-03) and GraphBLAS closure backend (SPEC-05). Read this before scoping
query-execution or closure work where "how does the other Polars-native RDF
engine do it" is a useful reference point.

The findings below were read from the maplib source tree (the
`lib/triplestore` and `lib/query_processing` crates) as of June 2026; specific
files are cited inline. maplib evolves, so re-verify against the repo before
treating any code-level detail as current.

## One-line framing

maplib is a **knowledge-graph construction** tool: turn tabular/industrial data
into RDF fast (via OTTR templates) and query it immediately. HornDB is a
**reasoner**: derive what an RDF graph entails under OWL 2 RL and maintain that
closure incrementally. Same substrate — Rust, columnar, in-memory, SPARQL —
opposite ends of the pipeline. maplib gets data *into* a graph; HornDB derives
what the graph *entails*.

## Shared DNA

| Dimension | HornDB | maplib |
|---|---|---|
| Core language | Rust | Rust (Python-fronted) |
| Data model | RDF triples | RDF triples |
| Query language | SPARQL 1.1 frontend (spargebra-based) | SPARQL (SELECT / CONSTRUCT / INSERT), spargebra parser |
| Columnar substrate | Dictionary-encoded columnar partitions | Polars / Apache Arrow DataFrames |
| Memory bias | Unified-memory hardware (HBM / CXL) | In-memory (~100M triples / 32 GB) |
| Rule-based materialization | OWL 2 RL (compiled rules + GraphBLAS closure) | SPARQL CONSTRUCT recursion + closed-source Datalog; SHACL Rules on roadmap |

Both parse SPARQL with Oxigraph's `spargebra` and return columnar results
(maplib hands Polars DataFrames back to Python zero-copy over Arrow IPC).

## Where they diverge

- **Job.** maplib's reason to exist is OTTR (Reasonable Ontology Templates) —
  mapping tabular data to RDF interactively. HornDB has no ETL/mapping story; its
  spine is reasoning.
- **Reasoning depth.** HornDB targets standards-track OWL 2 RL entailment
  (SPEC-04 compiled rules, SPEC-05 GraphBLAS closure, SPEC-06 incremental Z-set
  deltas). maplib's reasoning is SPARQL CONSTRUCT with recursion plus a Datalog
  engine that is *not* open source; SHACL Rules are roadmap.
- **Incremental maintenance.** HornDB's SPEC-06 delta / change-feed /
  checkpointing machinery has no maplib counterpart; maplib re-materializes.
- **Hardware thesis.** HornDB's unified-memory targeting and GraphBLAS
  linear-algebra closure backend is a bet maplib does not make. maplib is candid
  about being a 32 GB-class in-memory tool with disk-based storage planned.
- **Ecosystem.** DataTreehouse's scale-out/virtualization ambitions live partly
  in sibling tools (`chrontext` for SPARQL over time-series DBs), not in maplib
  alone.

The two are more complementary than competitive: maplib's OTTR ingestion is the
front-end HornDB lacks; HornDB's OWL 2 RL materialization is the reasoning maplib
gates behind closed-source Datalog. The awkward overlap is the middle — both
have a columnar, in-memory SPARQL engine.

## maplib's SPARQL-on-Polars execution model

This is the substantive part, and the most useful reference for SPEC-03/SPEC-05.

### Storage: vertically partitioned, datatype-split

The triplestore (`lib/triplestore/src/lib.rs`) keys storage as
**named graph → predicate IRI → `(subject-type, object-type)` datatype pair →
`Triples`** (`HashMap<NamedNode, HashMap<NamedNode, HashMap<(BaseRDFNodeType,
BaseRDFNodeType), Triples>>>`). The `Triples` value is Polars-backed and
optionally carries a subject/object index (`subject_object_index`). So it is a
predicate-oriented column store (classic vertical partitioning, one logical
table per property) with a further partition level by the *pair* of subject and
object RDF node types. A triple pattern with a bound predicate is therefore a
direct map selection — no predicate index lookup — and typed-term filters prune
whole partitions before any work happens.

### Everything is a Polars LazyFrame

The entire pattern-evaluation module is `lib/triplestore/src/sparql/lazy_graph_patterns/`.
maplib builds a Polars lazy logical plan and delegates predicate/projection
pushdown and physical execution to Polars' optimizer and vectorized,
multithreaded engine. The design bet is: do not write a join engine — lower
SPARQL algebra to a Polars plan and let a mature columnar engine run it.

### BGP ordering: greedy variable-connectedness, no statistics

`lib/triplestore/src/sparql/lazy_graph_patterns/triples_ordering.rs` orders the
triple patterns in a BGP with a **greedy variable-connectedness heuristic**. The
`strictly_before` comparator prioritizes, in order:

1. patterns connected to already-visited variables (avoid cross joins) — the
   in-code metaphor is "quantity is cost to include, so less is better";
2. patterns whose terms have known bindings;
3. patterns reusing already-visited variables;
4. concrete (named-node) predicates over variable predicates.

It then stops and defers the rest: "we rely on Polars to do the rest in the
query optimizer," with an in-code `//Todo find the least costly among the two`
marking the absent cost model. Ordering decisions use **no cardinality
statistics**: candidate join trees are compared by their `n_cross_joins` count
(`lazy_graph_patterns/join.rs`), and the per-pattern comparator is purely
structural. The one quantitative element is a coarse row-growth *guardrail* in
`join_workaround` — `factor = 1.5 - 0.1 * on.len()` clamped to `[1.1, 1.5]`,
applied to `max(left_height, right_height)` to estimate output height — but it
feeds only a `max_rows` abort (`MaxRowsReached`), not the ordering. The planner
picks a connected, cross-join-avoiding order and trusts Polars.

### Join execution: pairwise Polars inner/left join

`lib/query_processing/src/graph_patterns/join.rs` joins two solution mappings
with a plain Polars `LazyFrame::join`:

- `JoinArgs.how` is `Inner` (BGP join) or `Left` (OPTIONAL);
- join keys (`on`) are the **shared variables**, intersected by datatype
  compatibility (the datatype partitioning surfaces here);
- `nulls_equal: true` — this is how maplib encodes SPARQL's unbound-variable
  join semantics;
- `maintain_order: None` — Polars may reorder freely.

UNION is a vertical concat, FILTER a Polars predicate expression, and
projection/DISTINCT/ORDER BY are native Polars operations.

### Property paths: sparse-matrix fixpoint

`lib/triplestore/src/sparql/lazy_graph_patterns/path.rs` does **not** evaluate
transitive paths (`*` / `+`) with DataFrame joins. It converts the relation to a
CSR sparse adjacency matrix (the `sprs` crate) and iterates by repeated squaring
to a fixpoint, boolean-izing each step with `.map(|x| (x > &0) as u32)`
(reachability, not path counts), until the entry sum stops changing. `zero_or_more`
first adds the identity (`&rel_mat + &eye`); `one_or_more` accumulates
(`&new_rels + &rel_mat`). This is linear-algebra closure — boolean-semiring
matrix multiplication to a fixpoint.

## Contrast with HornDB's executor and closure

| | maplib | HornDB |
|---|---|---|
| Join strategy | Binary / pairwise natural joins | Leapfrog Triejoin — worst-case-optimal multiway (SPEC-03) |
| Execution substrate | Delegates to Polars relational operators | Own trie-iterator executor + planner |
| Planner | Greedy cross-join-avoidance, no cardinality statistics (only a coarse row-growth guardrail for a `max_rows` abort) | Variable-ordering plan; cardinality-aware (`EXPLAIN` reports estimates) |
| Intermediate results | Materializes each pairwise join | Avoids them; intersects sorted iterators in lockstep |
| Transitive closure | Sparse-matrix squaring fixpoint (property paths only) | GraphBLAS semiring closure, first-class (SPEC-05) |

Two observations worth carrying into SPEC-03/SPEC-05 work:

1. **The join contrast is unconditional on cyclic queries.** maplib's planner has
   no cardinality model, so it has no mechanism to defend against
   intermediate-result blowup — it avoids cross products and trusts Polars. On
   cyclic queries (the canonical triangle / 4-cycle case — see the `four_cycle`
   bench), a pairwise-join plan can produce intermediates asymptotically larger
   than the output regardless of ordering, while a worst-case-optimal multiway
   join stays within the AGM output bound. maplib is not in that game; HornDB's
   leapfrog triejoin wins there by construction. Conversely, maplib's target
   workload is star/tree-shaped KG queries, where binary hash joins are near
   optimal and Polars' vectorized throughput is hard to beat — so a fair
   head-to-head must declare its join topology.

2. **maplib independently arrived at linear-algebra closure.** The one place it
   refuses to delegate to row-joins — transitive property paths — it reaches for
   sparse-matrix fixpoint, which is conceptually the same instinct as HornDB's
   GraphBLAS closure backend. The difference is scope: maplib bolts a hand-rolled
   CSR squaring loop onto property paths; HornDB makes a general semiring closure
   engine drive OWL 2 RL. The convergence is not coincidence — it is the same
   physics pushing both toward linear algebra where row-at-a-time joins fall over.

## Ideas worth borrowing

- **Datatype-partitioned per-predicate storage** is a clean way to get
  typed-literal pruning and predicate pushdown nearly for free.
- **Delegating scalar/projection/aggregation to a mature columnar kernel** is
  sound even if HornDB keeps its own multiway-join core for the parts that need
  worst-case optimality.

## What to read next

- [`specs/SPEC-03-query-engine.md`](specs/SPEC-03-query-engine.md) — the leapfrog
  triejoin executor and planner.
- [`specs/SPEC-05-closure-backend.md`](specs/SPEC-05-closure-backend.md) — the
  GraphBLAS closure backend.
- [`specs/2026-06-05-provenance-symbolic-reasoning-landscape.md`](specs/2026-06-05-provenance-symbolic-reasoning-landscape.md)
  — the broader competitive landscape for provenance + symbolic reasoning.
- [`rdflib.md`](rdflib.md) — the other related-systems comparison (Python-side).

## Sources

- [DataTreehouse/maplib (GitHub)](https://github.com/DataTreehouse/maplib)
- [maplib: Interactive, Literal RDF Model Mapping for Industry (paper)](https://www.researchgate.net/publication/370190078_maplib_Interactive_literal_RDF_model_mapping_for_industry)
- [maplib API docs](https://datatreehouse.github.io/maplib/maplib.html)
- Source files cited inline, verified against a clone of `main` (June 2026): `lib/triplestore/src/lib.rs` (storage struct), `lib/triplestore/src/sparql/lazy_graph_patterns/{triples_ordering,join,path}.rs`, `lib/query_processing/src/graph_patterns/join.rs` (the `join_workaround` execution path).
