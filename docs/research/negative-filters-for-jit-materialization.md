# Negative filters for just-in-time materialization

**One-line summary:** a cited survey of techniques that answer "this derived triple
cannot exist" cheaply and with certainty, so the hybrid reasoner (SPEC-04, SPEC-05,
SPEC-07 backward mode) can materialize fewer triples at query time. Ordered from
cheapest to most expensive to maintain under updates.

**When to read this:** before scoping work that reduces what the JIT materializer
produces, or before adding any approximate index (Bloom filter or similar) to the
reasoning path. It is a *survey*, not a contract. Design decisions belong in a SPEC.

**Provenance & confidence.** Written 2026-09-02 from the author's knowledge of the
literature. Authors, venues, and years are confirmed. Where a result is summarized in
one sentence, check the paper before relying on a detail.

## The problem

HornDB materializes either at ingest or just in time at query time. Both are brute
force: a query usually needs a small fraction of the triples a rule closure produces,
and the rest is wasted memory and work.

What we want is a filter with **no false negatives**: if the filter says "not
derivable", that is certain, and the backward chainer can drop the branch. False
positives are fine, they only mean we do the real work.

The key observation from the literature: a membership filter over *derived triples*
(a Bloom filter on the closure) is the most expensive point on the spectrum and the
least informative. Every other technique below puts the filter on the *derivation
structure* (the schema, the hierarchy, the reachability relation) instead of on the
triple set. None of them stores a derived triple.

## The spectrum

### O1. Schema-level over-approximation

Zero per-fact update cost. The filter depends only on the rules and the schema, which
change rarely.

- **Type inference for Datalog.** Schäfer and de Moor, "Type inference for Datalog
  with complex type hierarchies", POPL 2010. Computes, for each head argument of each
  rule, a superset of the classes and predicates that can ever appear there. If a
  query pattern does not intersect that superset, the backward-chaining branch is
  provably empty.
- **QueryPIE.** Urbani, Piro, van Harmelen, Bal, "Hybrid reasoning on OWL RL",
  Semantic Web Journal 2014, and "QueryPIE: backward reasoning for OWL Horst over very
  large knowledge bases", ISWC 2011. Materializes only the schema closure at load and
  uses it to prune backward-chaining branches at query time. This is the closest
  published design to HornDB's hybrid.

### O2. Interval labelling of hierarchies

- **Semantic index.** Rodriguez-Muro and Calvanese, "High performance query answering
  over DL-Lite ontologies", KR 2012. Implemented in Ontop. Classes and properties get
  numeric ids such that every subclass falls in a contiguous range. A query for
  `?x rdf:type C` including all `rdfs:subClassOf` inference becomes a range scan over
  stored types. No derived type triple exists. Labels are recomputed on schema
  change, which is rare.

#### Applying the semantic index in HornDB

"Semantic" here means ontology-aware, nothing to do with embeddings. A label is a
second integer per class or property, chosen by a post-order walk of the hierarchy
so that a subtree is a contiguous range. Multiple inheritance breaks single
contiguity, so each class owns a *list* of ranges and a query becomes a few
`BETWEEN` tests instead of one.

**Keep labels separate from dictionary ids.** The dictionary id is assigned in
arrival order and appears in every column of every triple. The label is only
meaningful as a sort key in the two columns that carry a hierarchy: the object of
`rdf:type` and the predicate column. A relabel then touches those partitions only.
Schema triples such as `C rdfs:subClassOf D` keep their dictionary ids and never
move.

**Additive schema changes.** Hand out labels with a stride so that a new leaf class
takes a label from the gap and nothing else changes. When a gap runs out, or an
existing class gains a new parent, append the class's ranges to the range lists of
the new ancestors instead of renumbering. Relabel only as an amortized compaction
when range lists grow long enough to hurt scans. This is the order-maintenance
problem of Dietz and Sleator (STOC 1987) and Bender et al. (ESA 2002), and the
insertable tree labels of ORDPATH (O'Neil et al., SIGMOD 2004). Contiguity is per
subtree of the merged DAG, so vocabularies extending each other are not a problem.

**Three ways to store the label in the type partition.** Every option keeps the
term id as the identity of the class everywhere else in the store, and none stores
a derived type triple.

| Option | Rows hold | Per-row cost | Scan cost | Relabel cost |
|---|---|---|---|---|
| 1. Both columns | `(x, class_id, label)`, sorted by `(label, x)` | 16 to 32 bits raw, a few bits after run-length encoding since the column is sorted | None, class ids read directly | Re-sort partition and rewrite label column |
| 2. Label only | `(x, label)`, sorted by `(label, x)` | None | One dependent array lookup per output row to recover the term id, and the column cannot be joined by term id without translating first | Re-sort partition |
| 3. Term id only, sorted by label | `(x, class_id)`, physically sorted by `label(class_id)` | None | One array lookup per block to map block min and max term ids into label space | Re-sort partition and rebuild the sparse block index |

The relabel cost is shared by all three, since the sort key changes in each. Under
additive-only schema changes with gaps and range lists it is rare and can run as a
background rewrite. The lookup table in options 2 and 3 is one entry per class and
stays in L1 or L2 for the whole scan.

Ranking: option 3 has no per-row cost and a negligible per-block cost. Option 1
pays a small compressed column to avoid even that. Option 2 pays the per-row lookup,
loses term-id joins on the type column, and gains nothing over option 3, so it is
dominated. The predicate column takes option 3 by default, because predicates are
few and that column is usually already grouped by predicate.

### O3. Reachability labellings for transitive rules

This is the correct form of the Bloom-filter instinct. Transitive properties,
`owl:sameAs` chains, and deep hierarchies all produce a transitive closure. The
reachability literature has exactly the structure we want: a per-node label that
proves non-reachability with certainty, and falls back to a real search for the
positive case. Memory is O(k·n) for n nodes and k labels, not O(closure size).

- **GRAIL.** Yildirim, Chaoji, Zaki, "GRAIL: scalable reachability index for large
  graphs", VLDB 2010. k random depth-first traversals give k interval labels per
  node. If any interval of v is not contained in the matching interval of u, u does
  not reach v. Containment in all k triggers a real search.
- **DAGGER.** Yildirim, Chaoji, Zaki, "DAGGER: a scalable index for reachability
  queries in large dynamic graphs", 2013. The dynamic-graph version of GRAIL.
- **IP labelling.** Wei, Yu, Lu, Jin, "Reachability querying: an independent
  permutation labeling approach", VLDB 2014. Min-hash style labels. Negative answers
  are certain, positives fall through to search.
- **BFL.** Su, Zhu, Wei, Yu, "Reachability querying: can it be even faster?", IEEE
  TKDE 2017. Bloom filters as node labels.
- **Lower bound on dynamic exact reachability.** Henzinger, Krinninger, Nanongkai,
  Saranurak, "Unifying and strengthening hardness for dynamic problems via the online
  matrix-vector multiplication conjecture", STOC 2015. Fully dynamic exact
  reachability cannot beat the naive approach under the OMv conjecture. This is why
  randomized labels with lazy rebuild are the practical choice, not an exact
  incrementally maintained index.

### O4. Structural summaries with a completeness guarantee

- Čebirić, Goasdoué, Manolescu, "Query-oriented summarization of RDF graphs", PVLDB
  2015. The quotient summary is complete for basic graph patterns: no match in the
  summary implies no match in the graph. The summary is therefore a negative filter
  for whole patterns, not single triples.
- Goasdoué, Guzewicz, Manolescu, "Incremental structural summarization of RDF
  graphs", EDBT 2019. Incremental maintenance of the summary. The same line of work
  proves when saturating the summary equals summarizing the saturation, so the
  summary can stand in for the materialized graph.

### O5. Abstraction refinement

- Glimm, Kazakov, Liebig, Tran, Vialard, "Abstraction refinement for ontology
  materialization", ISWC 2014. Materialize over one abstract individual per type
  signature, then refine until the concrete closure is reached. The abstract closure
  is often orders of magnitude smaller than the concrete one and answers "could x
  ever get type C" before touching real individuals.

### O6. Lower and upper bound programs

- **PAGOdA.** Zhou, Cuenca Grau, Nenov, Kaminski, Horrocks, "PAGOdA: pay-as-you-go
  ontology query answering using a Datalog reasoner", JAIR 2015. Two Datalog programs
  bracket the answer set. Anything outside the upper bound is a certain negative.
  Only the gap between the bounds goes to the expensive reasoner. Built for OWL DL,
  but the bracketing trick applies within OWL RL to choosing a cheap
  over-approximating rule subset.

### O7. Membership filters on derived facts

The naive reading of the Bloom-filter instinct, listed for completeness.

- Counting Bloom filters and cuckoo filters (Fan, Andersen, Kaminsky, Mitzenmacher,
  "Cuckoo filter: practically better than Bloom", CoNEXT 2014) support deletion, so
  the filter itself can be maintained.
- The real cost is knowing *which* derived facts died on retraction. That is the
  DRed / backward-forward / FBF problem (Motik, Nenov, Piro, Horrocks, "Incremental
  update of Datalog materialisation: the backward/forward algorithm", AAAI 2015, and
  "Maintenance of Datalog materialisations revisited", Artificial Intelligence 2019)
  that `horndb-incremental` already carries. A filter at this level adds memory and
  update cost without removing the maintenance problem.

## Where this points for HornDB

OWL 2 RL materialization is dominated by three shapes of derivation. Each has a
technique above that never stores a derived triple:

| Derivation shape | Technique |
|---|---|
| Transitive properties, `owl:sameAs` chains, deep hierarchies | O3 reachability labels (GRAIL or IP labelling), with DAGGER for updates |
| Type propagation through `rdfs:subClassOf` / `rdfs:subPropertyOf` | O2 semantic index |
| Rule-branch pruning in the backward chainer | O1 type inference over `rules.toml`, QueryPIE-style schema closure |

O4 and O5 are candidates once the three above are in place. O7 is not recommended.
