---
status: draft
date: 2026-07-28
scope: "SPEC-28 — named-graph and RDF dataset semantics end to end: GRAPH in query, FROM/FROM NAMED dataset construction, named-graph SPARQL Update and graph management, the Graph Store Protocol HTTP surface, the graph-scoped access paths those need from storage, and idempotent quad-grain apply"
---

# SPEC-28 — Named-graph and dataset semantics

**One-line thesis:** HornDB's storage layer is already quad-aware, but
everything above it still behaves as if the store held one merged graph — and
the query translator *silently drops* `GRAPH` and `FROM`, so a named-graph
query returns default-graph rows with no error. This spec makes the whole stack
above storage honest about named graphs: first by refusing the queries it
cannot answer, then by answering them.

**Refines:** SPEC-07 (the SPARQL frontend contract — its F1–F9 and NF1–NF5 stay
in force; this spec fills in the named-graph half of F2, F5, and the Graph
Store Protocol line in its Scope) and SPEC-02 F7 (named graphs — this spec adds
the graph-scoped *access paths* F3/F4 do not describe). Consumes SPEC-25 S1
(per-tuple MVCC + retraction,
[#225](https://github.com/sunstoneinstitute/horndb/issues/225), done) and
coordinates with SPEC-25 S4 (named-graph snapshot export/import,
[#228](https://github.com/sunstoneinstitute/horndb/issues/228)) and SPEC-24 S4
(engine wiring, [#213](https://github.com/sunstoneinstitute/horndb/issues/213)).
**Epic:** [#189](https://github.com/sunstoneinstitute/horndb/issues/189)
(SPEC-07 surface completeness Stage 2), which already names GSP and named-graph
scoping; this spec is that epic's contract.
Supersedes [#54](https://github.com/sunstoneinstitute/horndb/issues/54) (GSP,
closed as deferred).

**Why now.** The Sunstone data platform treats the named graph as its core
modelling unit, not an edge case: one graph per dataset descriptor, per research
scope, per project — thousands of small graphs, not a handful of big ones. The
named graph is simultaneously its whole-graph `PUT` unit, its materialization
trigger unit, and its conflict-detection grain. HornDB cannot serve that platform
at all until named graphs work end to end.

## Problem — what exists today, and where it stops

> **Status note (2026-07-30).** This section describes the store as it was when
> the spec was written. Phases 1–3 have since landed
> ([#264](https://github.com/sunstoneinstitute/horndb/issues/264),
> [#265](https://github.com/sunstoneinstitute/horndb/issues/265),
> [#266](https://github.com/sunstoneinstitute/horndb/issues/266)): the two
> query bullets below are history — `GRAPH` and `FROM`/`FROM NAMED` are
> evaluated, within the limits recorded at the end of S3. The update, executor
> and GSP bullets still stand. `docs/architecture.md` carries the current state.

Storage is quad-aware. `Store::insert_quads` / `retract_quads` /
`intern_graph_uri` take a `GraphId`; `MemoryTier` keys partitions by graph
(`with_predicate(graph, predicate, …)`, `graphs()`, `predicates(graph)`); the
N-Quads loader routes each quad to its named graph. Above that line, nothing is.

- **Query silently discards `GRAPH`.** `translate.rs::translate_pattern`'s arm is
  `GraphPattern::Graph { name: _, inner } => translate_pattern(inner, cfg)`.
  There is no `Graph` variant in the `Algebra` enum
  (`crates/sparql/src/algebra/mod.rs::Algebra`). `GRAPH <g> { ?s ?p ?o }`
  therefore returns the *default graph's* rows, and `GRAPH ?g { … }` returns them
  with `?g` unbound. Both are wrong answers delivered with a 200.
- **Query silently discards the dataset clause.** All four arms of
  `translate.rs::translate_query_with` destructure `dataset: _`, so
  `FROM` and `FROM NAMED` are ignored the same way.
- **Update rejects named graphs** (`crates/sparql/src/update.rs`). Named-graph
  quads in `INSERT DATA` / `DELETE DATA`, a named `CLEAR`/`DROP`/`CREATE`, a
  `LOAD … INTO GRAPH <g>`, a named-graph template in `DELETE`/`INSERT … WHERE`,
  and any non-empty `USING`/`USING NAMED` are all errors — silently no-ops under
  `SILENT`. This is at least honest, unlike query.
- **The SPARQL executor writes only the default graph.** `HornBackend`
  (`crates/sparql/src/exec/horn.rs`) writes through the triple-grain store path,
  so every insert lands in the default graph; the one explicit `DEFAULT_GRAPH`
  use is `horn.rs::clear_all`'s retraction sweep. `HornBackend::len`'s doc
  comment states the assumption outright: "`HornBackend` never writes a named
  graph".
- **Two read paths are hard-wired to the default graph.**
  `store.rs::Store::scan_predicate_default_graph` and its
  `StoreSnapshot` copy have no graph parameter, and `StoreSnapshot::len` is
  default-graph-scoped — pinned by the test
  `store.rs::snapshot_len_is_default_graph_scoped`.
- **There is no Graph Store Protocol.** The axum router
  (`crates/sparql/src/server/mod.rs`) exposes `/query`, `/update`, `/metrics`
  and nothing else.

## Non-goals

- **What the reasoner reasons over, and where inferences land.** Cross-graph
  entailment scope, per-graph vs. dataset-wide materialization, and the graph a
  derived quad is written to belong to **SPEC-29** (drafted in parallel). This
  spec designs none of it. The one seam it must leave open is stated in S3: a
  graph-scoped read must be expressible over base quads alone or over base +
  derived, with SPEC-29 choosing which and naming the graph derived quads carry.
- **Branches, as-of reads, copy-on-write graph forking.** SPEC-25 S1 gives one
  commit clock and one lineage. The platform's branch model is its H2 horizon;
  no spec covers it yet.
- **Authentication and authorization.** Terminated upstream at `graph-server`
  (its auth design); HornDB is cluster-internal. SPEC-07 F7 already says this.
- **Dataset serialization formats on GSP.** GSP is a *graph* protocol; TriG and
  N-Quads bodies are 415, matching `graph-server` §5.
- **Federation (`SERVICE`)** and remote (`http(s):`) `LOAD` — SPEC-07 non-goal
  and #189 respectively, unchanged.

## Decisions

| # | Decision | Rationale |
|---|---|---|
| D1 | `GRAPH` and a non-empty `FROM`/`FROM NAMED` become **explicit errors** before any real support lands, shipped as their own first slice. | A wrong answer is worse than a refusal. Today's silent drop is undetectable from the client side; an error is a one-line fix and immediately correct. |
| D2 | When a query names no dataset, the default graph is the **union of all non-reserved graphs** (default), switchable to `strict` — the reserved sentinel graph alone. Graphs under the reserved `https://horndb.io/graph/` namespace (SPEC-27 F6, SPEC-29 D4) are never in the union; they enter the default dataset only when SPEC-29's `reasoning.default_dataset_includes_inferred` is set. | SPARQL 1.1 §13.2 leaves the no-`FROM` dataset implementation-defined. The consuming platform writes 100% of its data into named graphs, so `strict` makes every unqualified query return empty. Excluding reserved graphs keeps asserted and inferred data from blending by default (SPEC-29 D6). A deployment that mirrors another store, or is differentially compared against one, should set `strict` (see Risks). |
| D3 | `GRAPH ?g { … }` ranges over **named graphs only** and never binds `?g` to the default graph. | SPARQL 1.1 §13.3. The default graph's sentinel is not an IRI and has no lexical form to bind. Holds in both D2 modes: under `union` a sentinel-graph quad is visible to an unqualified BGP but not through `GRAPH ?g`. |
| D4 | A query that **does** name a dataset gets exact SPARQL 1.1 semantics. The D2 mode only decides the no-dataset case. | Conformance is not negotiable where the standard is definite. D2 exercises freedom only where the standard grants it. |
| D5 | The logical algebra gains `Algebra::Graph { name, inner }`; the planner **pushes the graph scope onto the scan node**, never applies it as a post-filter. | With thousands of small graphs, post-filtering an all-graphs scan costs O(store) to answer a question about one small graph — the platform's single hottest shape. |
| D6 | A variable-graph scan emits `?g` as an extra **scan column**, rather than lowering to a `Union` over `graphs()`. | Plan size must not grow with graph count. A thousand-arm union is unplannable and defeats every cardinality estimate. |
| D7 | Partitions stay keyed **per `(graph, predicate)`**. No graph column is added inside a partition. | This is already `MemoryTier`'s shape, and it makes a ground-graph scan a map probe followed by an unchanged SPEC-02 F3/F4 `(s_id, o_id)` scan. The WCOJ trie iterators are untouched. |
| D8 | The GSP surface mirrors `graph-server` §5 **minus branches**, and adds `?default`. `ETag`/`If-Match` conditional writes are **not** implemented. | HornDB has one lineage, so there is no branch path segment and no branch head to condition on. `graph-server` returns 400 for `?default` because its model has no default graph; HornDB has one (SPEC-02 F7), so it serves it. |
| D9 | Idempotence is enforced **at the store boundary**, not by callers. | The consuming change feed is at-least-once and replays on restart. One enforcement point is one place to be right; every caller getting it right is a standing bug source. |
| D10 | `WITH` / `USING` / `USING NAMED` are **in scope**, landing with the same dataset machinery as `FROM`. | They are the update-side spelling of the same construct; deferring them would leave a second silent-or-refusing surface for no saving. |
| D11 | A named graph **exists iff it holds at least one visible quad**. No empty-graph registry. | Matches the RDF dataset model and keeps storage simple. `CREATE <g>` on an absent graph succeeds as a no-op; `DROP` of an absent graph is an error unless `SILENT`. See Risks for the conformance exposure. |

## Requirements

### S1. Refuse, do not lie

The smallest shippable correctness fix, landing before any of S2–S5.

- **`GRAPH` is an error.** `translate_pattern`'s `GraphPattern::Graph` arm
  returns `SparqlError::UnsupportedAlgebra` naming the construct, replacing
  today's drop-the-wrapper translation. Both the ground and the variable form.
- **A non-empty dataset clause is an error.** The four `dataset: _` bindings in
  `translate_query_with` become real matches: a `QueryDataset` with any `default`
  or `named` entry errors; an absent or empty dataset stays a no-op. The same
  rule already exists on the update side for `USING`
  (`update.rs::validate_delete_insert`) — this makes query agree with it.
- **The HTTP mapping is a client error.** `/query` returns 400, not 500: the
  query is well-formed but unsupported by this server. The error text names
  `GRAPH` or `FROM`/`FROM NAMED` explicitly so a caller can act on it.
- **No behaviour change for graph-free queries.** Every existing selected
  conformance case stays green; only queries that were previously answered
  wrongly change, and they change from wrong to refused.

### S2. Graph-scoped access paths

Give the read surface a graph parameter, and give the whole-graph-scan hot path
a cost proportional to the graph.

- **Whole-graph scan.** `StoreSnapshot` grows `scan_graph(GraphId)` yielding
  every visible `(s, p, o)` in one graph, at a cost of O(quads in the graph +
  predicates in the graph) and never O(store). This is the GSP `GET` path and
  stage 3 of the whole-graph `PUT` diff (`graph-server` §6), so it is a hot path,
  not a convenience.
- **Graph-parameterized predicate scan.** `scan_predicate(graph, predicate)`
  replaces `scan_predicate_default_graph` on both `Store` and `StoreSnapshot`;
  the default-graph form is retired, not kept as an alias.
- **Whole-store counts.** `StoreSnapshot::len` / `triple_count` become
  whole-store, with an explicit `graph_len(GraphId)` alongside. The test
  `store.rs::snapshot_len_is_default_graph_scoped` inverts to assert the new
  contract. `graph_len` is what decides GSP 201-vs-204 and `DROP`'s existence
  check, so it must be O(1) or O(predicates in graph), not a scan.
- **Where the default-graph-scoped `len` contract goes.** The retired test
  assumed a live storage edge that does not exist: `horndb-incremental` has no
  `horndb-storage` dependency, and `incremental::snapshot::Snapshot` only
  mirrors `StoreSnapshot`'s shape, ahead of the SPEC-24 S6 swap
  ([#215](https://github.com/sunstoneinstitute/horndb/issues/215), not
  landed). Inverting `len` breaks no live circuit today. What this section
  still owes S6 is a target: the graph-scoped surface (`graph_len`,
  `iter_graph_term_ids`) must exist now and be documented as what the S6 swap
  wires onto, so that backing lands per-graph from the start — per-view under
  SPEC-29 D7 — instead of re-growing a whole-store assumption.
  [#213](https://github.com/sunstoneinstitute/horndb/issues/213) (S4, the
  engine-wiring consumer that depends on the S6 swap) carries a pointer
  comment to this bullet.
- **Graph enumeration is visibility-filtered.** `graphs()` returns exactly the
  graphs holding at least one quad visible at the pinned version (D11) — a
  graph all of whose quads are retracted disappears from it. This is what
  `GRAPH ?g` ranges over (D3) and what GSP `GET` 404s on.
- **SPEC-02 refinement.** F3/F4 describe per-predicate `(s_id, o_id)` columns
  with no graph column. The refinement this spec pins: the partition key is
  `(graph, predicate)`, graph is the **outer** key, and the six orderings of F4
  are per-partition and therefore already per-graph (D7). No SPEC-02 acceptance
  criterion changes.
- **The SPARQL executor stops hard-wiring the default graph.** `HornBackend`'s
  write path becomes quad-grain — it takes the graph from the quad being
  written, including `horn.rs::clear_all`'s `DEFAULT_GRAPH` retraction sweep —
  and its `live_keys` dedup set is keyed by *quad*, not triple: the same triple
  in two graphs is two rows.

### S3. Query — `GRAPH` and dataset construction

- **`Algebra::Graph { name: GraphTerm, inner: Box<Algebra> }`** joins the
  `Algebra` enum, where `GraphTerm` is a ground IRI or a variable. Translation
  stops discarding the wrapper (S1's error becomes a real node).
- **Ground form.** `GRAPH <g> { P }` evaluates `P` with every scan scoped to
  `g`'s `GraphId`. An unknown graph IRI yields zero rows, not an error. Any IRI
  is therefore a legal graph name, and any graph holding a quad is nameable —
  which, with D11, already satisfies SPEC-29's constraint 3 (derived graphs must
  be nameable). No further change is needed for it.
- **Variable form.** `GRAPH ?g { P }` evaluates `P` over each named graph and
  binds `?g` to that graph's IRI. Physically this is one scan carrying the graph
  id as an output column (D6), not a union of per-graph plans (D5/D6). `?g`
  never binds the default graph (D3).
- **Dataset construction.** `FROM <g>` builds the default graph as the RDF merge
  of the named `FROM` graphs; `FROM NAMED <g>` populates the set `GRAPH ?g`
  ranges over. A query with `FROM NAMED` but no `FROM` has an **empty** default
  graph (SPARQL 1.1 §13.2) — this is D4 territory and is not softened by D2.
- **The no-dataset case — default graph.** With no `FROM`/`FROM NAMED`, the
  default graph is the union of all **non-reserved** graphs, or only the sentinel
  graph, per the `default_graph` mode (D2). Graphs under
  `https://horndb.io/graph/` are excluded from the union in both modes. The mode
  is a SPEC-26 server setting (`[server.limits].default_graph = "union" |
  "strict"`, default `"union"`) and is per-query overridable through SPEC-26 S4's
  URL-parameter channel — which means `default_graph` grows that spec's
  enumerated overridable-key whitelist, a closed list there. `GRAPH ?g`'s range
  is unaffected by the mode (D3).
- **The no-dataset case — named graphs.** The same dataset's named-graph set is
  **all non-reserved graphs**. Reserved graphs are addressable only by explicit
  name (`FROM NAMED <g>`, ground `GRAPH <g>`) — naming one is the opt-in — or
  dataset-wide via SPEC-29's `reasoning.default_dataset_includes_inferred`, which
  adds them to both components. So `GRAPH ?g { ?s ?p ?o }` with no dataset clause
  does not enumerate reserved graphs unless that flag is set.
- **Property paths inherit the scope.** In `GRAPH <g> { ?x :p+ ?y }` the graph
  scope is applied to `Algebra::PathClosure`'s `edge` sub-plan **before** the
  closure is computed, never to the closure's output. Filtering afterwards
  admits paths that leave the graph and come back, which is a different (and
  wrong) answer. Under the union default graph a path legitimately traverses all
  graphs, because the union *is* the default graph.
- **Count and aggregate pushdowns are scope-aware or disabled.** The
  `count_bgp` / group-count shortcuts (`crates/sparql/src/exec/horn.rs`,
  `crates/sparql/src/plan/pushdown.rs`) return row counts without decoding
  terms; several bottom out in whole-store counters. Every such shortcut either
  takes the graph scope as a parameter or the planner declines to fire it. A
  pushdown that cannot express the scope must never fall back to a whole-store
  count — that is a silent wrong answer of exactly the kind S1 exists to
  eliminate. Cardinality *estimates* may stay coarse (a whole-store live count
  remains a valid upper bound under any scope), but they must be labelled
  estimates, never results.
- **The reasoning seam.** A graph-scoped scan must be expressible over base
  quads only or over base + derived quads. This spec provides the parameter and
  takes no position on its value for query reads; SPEC-29 sets it and defines the
  graph derived quads carry. One consumer is pinned here rather than deferred:
  S5's GSP read and `PUT` diff are always base-only (SPEC-29 D5), because a diff
  against derived quads deletes data the client never sent.

**What phase 3 shipped, and the two families it refuses.** Everything above is
implemented ([#266](https://github.com/sunstoneinstitute/horndb/issues/266))
except for two families of `GRAPH ?g` query, which are **refused** with an error
naming the construct rather than answered. Both follow from D5/D6: the graph
name is bound on the scan leaf, not joined on after the block is evaluated.

1. **A barrier between the `GRAPH ?g` wrapper and its scan leaves** — a
   sub-`SELECT`, `DISTINCT`, `GROUP BY`/aggregate, `LIMIT`/`OFFSET`, any
   property path, a nested `GRAPH`, or a `VALUES` that is not joined against a
   scoped arm. Each drops or merges the graph column, so rows would come back
   with `?g` unbound or mixed across graphs. The same constructs placed *above*
   the wrapper work. A quad-free arm is exempt where the other arm's graph
   column reaches every joined row — either side of a `Join`, or an
   `OPTIONAL`'s right arm — so `GRAPH ?g { ?s ?p ?o VALUES ?o { … } }` answers.
2. **`P` reading `?g` where leaf-binding diverges from SPARQL 1.1
   §18.2.2.2's post-join** — any expression (`FILTER`, a `BIND` expression, an
   `OPTIONAL` condition, `ORDER BY`), `BIND(… AS ?g)`, or any mention of `?g`
   in a `LeftJoin`'s right arm. Reading `?g` from a triple position, or from a
   `VALUES` column joined against a scoped arm, is allowed: there leaf-binding
   and the post-join agree.

Lifting either refusal means evaluating the whole block once per graph with `?g`
free and joining the graph name on afterwards — a design change against D5/D6,
not a bug fix. `harness/KNOWN-MANIFEST-BUGS.md` names the W3C cases each
refusal costs.

### S4. Update — named-graph writes and graph management

Replace today's error-unless-`SILENT` behaviour (`crates/sparql/src/update.rs`)
with real named-graph operations.

- **Quad data.** `INSERT DATA` / `DELETE DATA` accept `GRAPH <g> { … }` blocks
  and route each quad to its graph. `require_default_graph_name` and
  `require_default_graph` are retired.
- **Pattern updates.** Named-graph templates in `DELETE`/`INSERT … WHERE` are
  instantiated into their named graph.
- **Graph management.** `CREATE`, `CLEAR`, `DROP`, `LOAD … INTO GRAPH`, `ADD`,
  `MOVE`, `COPY` operate on real named graphs, with SPARQL 1.1 §3.2 existence
  semantics under D11: `CLEAR`/`DROP` of an absent graph is an error unless
  `SILENT`; `CREATE` of an absent graph succeeds; `CREATE` of an existing graph
  errors unless `SILENT`. `ADD`/`MOVE`/`COPY` between named graphs, and between
  a named graph and `DEFAULT`, all work; the same-graph identity case stays the
  zero-operation no-op it is today. `LOAD` remains `file:`-only (remote `LOAD`
  is #189, unchanged).
- **The reserved namespace is closed to writes.** Any write targeting a graph IRI
  under `https://horndb.io/graph/` — `CREATE`, `CLEAR`, `DROP`, `LOAD … INTO`,
  `ADD`/`MOVE`/`COPY` with it as destination, a `GRAPH` block in
  `INSERT DATA`/`DELETE DATA`, or a named-graph template — is an error naming the
  namespace. It is **not** suppressible by `SILENT`: it is a permission-shaped
  error, not an existence error, and `SILENT` only covers the latter. Reads of
  reserved graphs stay allowed.
- **`CLEAR`/`DROP` retract quads, they never unlink partitions.** Both go through
  the same store boundary as `DELETE DATA` (S6), so a delta consumer observes
  quad-grain retractions for every quad removed. A structural unlink of the
  graph's partitions would be faster and is forbidden: it bypasses the delta
  path, and a downstream view circuit would never withdraw the graph's derived
  triples.
- **`DROP ALL` is a data reset, not a system reset.** It drops every non-reserved
  graph and the default graph, quad by quad per the rule above. Reserved graphs
  are not dropped directly — they empty out as the view circuits withdraw the
  derived triples of the graphs that were dropped, and by D11 then cease to
  exist. `DROP ALL` is therefore not the rebuild-from-zero primitive: resetting
  a store's applied position, its view catalog, and its cursor relationship with
  an external feed is SPEC-30's
  ([#263](https://github.com/sunstoneinstitute/horndb/issues/263)), not this
  spec's.
- **One store batch per Update operation.** A multi-operation request
  (`DELETE DATA{…}; INSERT DATA{…}; DELETE DATA{…}`) applies each operation as
  its own store batch, in request order. Batches may be collapsed only if the
  collapse preserves per-quad last-writer order; otherwise a later operation's
  delete is silently undone by an earlier operation's insert. See S6 for the
  within-batch ordering rule, which does not extend across operations.
- **`SILENT` fidelity.** `update.rs` today notes that spargebra's desugaring
  drops `SILENT` on `ADD`/`MOVE`/`COPY`, which was observationally harmless
  while named graphs were unrepresentable. Once they are representable it is
  not: a non-silent `COPY`/`MOVE <missing> TO <g>` must error, and a silent one
  must not. "Must not error" is not the same as "no-op", though. spargebra
  desugars `COPY`/`MOVE` into a silent `DROP` of the destination graph followed
  by a copy from the source, so a silent `COPY`/`MOVE` from a missing source
  still empties `<g>` — it just does so without raising an error. `ADD`
  desugars with no destination `DROP`, so a silent `ADD` from a missing source
  is the one true no-op that leaves `<g>` untouched. The plan re-derives the
  flag (re-parse the verb, or take it from the operation before desugaring)
  rather than inheriting the current behaviour.
- **`WITH` / `USING` / `USING NAMED`** (D10). `WITH <g>` scopes the templates and
  the `WHERE` clause to `g`. `USING` / `USING NAMED` build the `WHERE` clause's
  dataset with the same machinery as `FROM` / `FROM NAMED` (S3); the blanket
  rejection in `update.rs::validate_delete_insert` goes.
- **Atomicity is preserved.** The existing preflight-then-apply structure —
  validate every operation before the first mutation, so a failing update never
  half-applies — stays. Its rejection set shrinks; its shape does not change.

### S5. Graph Store Protocol

The HTTP surface, aligned deliberately with `graph-server` §5 (D8) so a
HornDB-backed deployment is a drop-in for the shape the platform already
specifies. Its justification is standalone-product surface: GSP is what lets a
client load and replace whole graphs over HTTP without `graph-server` in front.
The data platform does not need it — it writes through `/update` and reads
through `/query` — so nothing on that integration path waits on this section.

| Route | Behaviour |
|---|---|
| `GET /graphs?graph=<iri>` \| `?default` | Serialize the graph. Content negotiation over `text/turtle`, `application/n-triples`. |
| `PUT /graphs?graph=<iri>` \| `?default` | Replace the graph wholesale with the payload. |
| `POST /graphs?graph=<iri>` \| `?default` | Merge (append) the payload into the graph. |
| `DELETE /graphs?graph=<iri>` \| `?default` | Remove every quad in the graph. |

- **Status codes** (matching `graph-server` §5 where the models agree):
  **201** the graph held no visible quads before the write; **204** replaced,
  merged, or deleted, and for an idempotent no-op; **400** parse error (with the
  parser's message in the body), unknown query parameters, or neither `graph`
  nor `default` given; **404** `GET`/`DELETE` of a graph with no visible quads;
  **413** payload over the configured cap; **415** unsupported media type,
  including the dataset formats TriG and N-Quads.
- **Whole-graph `PUT` is a replace, expressed as a diff.** The server reads the
  graph's currently visible **asserted (base)** quads — S2 `scan_graph` with the
  S3 reasoning seam pinned to base-only. Derived quads are never in the read set
  or the diff (SPEC-29 D5). It then computes `dels = base − payload` and
  `adds = payload − base` and commits both in one batch. An empty diff commits
  nothing and returns 204. This is `graph-server` §6 stage 3 minus the branch
  lock, and it is why S2's whole-graph scan is a hot path rather than a
  convenience.
- **Reserved graphs are read-only over GSP.** `GET` of a graph under
  `https://horndb.io/graph/` is allowed. `PUT`, `POST`, and `DELETE` of one
  return 400 with the namespace in the body, on the same terms as S4's
  closed-namespace rule.
- **`?default` is refused on a materialized store.** When the server is started
  with `serve --materialize`, `horn.rs::load_with_reasoning` dumps asserted base
  and inferred triples into the default graph indistinguishably, so a `PUT` diff
  of `?default` would compute deletions of triples the client never sent. GSP
  `PUT`/`POST`/`DELETE` of `?default` is therefore refused on such a store, with
  the reason named in the body. The restriction lifts when SPEC-29 P1 lands and
  inferences live in their own graphs.
- **Deliberate divergences from `graph-server` §5.** No `/branches/{b}` path
  segment and no `ETag` / `X-Sunstone-Txn` / `If-Match` conditional writes —
  HornDB has one lineage, so there is no branch head to condition on (D8).
  `?default` is served rather than 400 (D8). No `Sunstone-*` headers and no
  `?wait=materializations`: those are platform concerns owned by `graph-server`,
  which sits in front. A HornDB deployment that later grows branches revisits
  the conditional-write contract then, not now.
- **Blank nodes.** GSP payloads may contain blank nodes; they are scoped to the
  request, as the protocol requires. Request-scoped bnodes never equal stored
  ones, so the empty-diff no-op above does not hold for a bnode-bearing payload:
  re-`PUT`ting an identical body deletes and re-inserts every bnode-touching
  quad. The platform skolemizes upstream, so its traffic never exercises this,
  but conformance does.
- **Route placement.** The GSP routes join the existing axum router
  (`crates/sparql/src/server/mod.rs`) behind the same `server` feature as
  `/query` and `/update`.

### S6. Idempotent quad-grain apply

- **The requirement.** Inserting a quad that is already visible is a no-op.
  Retracting a quad that is not visible is a no-op. Neither is an error; both
  report an accurate affected-quad count. Replaying any prefix of a change feed
  converges to the same store state.
- **Where it is enforced (D9).** At the store boundary —
  `Store::insert_quads` / `retract_quads` — so every path above it (SPARQL
  Update, GSP, the N-Quads loader, a future change-feed consumer) inherits it.
  SPEC-25 S1 already specifies the retraction half ("retracting an absent tuple
  is a no-op with an observable count, not an error"); this spec makes the
  insert half explicit and extends both from triple grain to quad grain: the
  same `(s, p, o)` in two graphs is two independent quads with independent
  lifetimes.
- **Why it matters here.** The consuming feed is at-least-once and replays from
  its cursor on restart (`oxigraph-materializer` §6.3). The materializer's
  correctness argument is precisely that `INSERT DATA` of a present quad and
  `DELETE DATA` of an absent one are no-ops. A HornDB materializer needs the
  same property from HornDB, at quad grain, or restart corrupts the derived
  store.
- **Quad identity is lexical.** Two quads are the same quad iff they are RDF
  term-equal on the lexical form. The apply path performs no value normalization:
  `"01"^^xsd:integer` and `"1"^^xsd:integer` are distinct quads. Feed terms
  arrive already canonicalized upstream, and normalizing here would make a
  replayed `DELETE DATA` of the original lexical form miss the stored quad, so
  replay would stop converging.
- **Ordering within a batch.** Within one applied batch, deletions are applied
  before insertions, matching the materializer's `dels`-before-`adds` emission,
  so a retract+insert pair covering the same quad ends with it present. This rule
  is scoped to a single retract+insert pair and never spans operations: each
  Update operation is its own batch (S4), so a later operation's delete always
  wins over an earlier operation's insert.
- **Feed-level ordering and durability are SPEC-30's.** This spec owns the store
  batch. What a consumer may assume about applied-state durability, how an
  external cursor is reconciled at startup, and the ordering contract across a
  whole feed belong to SPEC-30
  ([#263](https://github.com/sunstoneinstitute/horndb/issues/263)).

### S7. Conformance

The harness-first rule applies: this spec is not satisfied until its referenced
subset is green. Suites are keys in `crates/harness/src/runner.rs`; the
selection lives in `harness/selected.toml`; corpora are fetched by
`crates/harness/scripts/fetch-w3c-suites.sh`.

- **New suite key `sparql11-gsp`** for the W3C SPARQL 1.1 Graph Store HTTP
  Protocol tests (upstream `http-rdf-update/`). These are protocol tests: each
  case is an HTTP request/response pair against a live server, so the runner
  needs a `TestKind` that boots the axum server (the `server` feature) and
  drives it, alongside the existing manifest-driven kinds. Gates S5.
- **The dataset and `GRAPH` families gate S3 through `[sparql_query]`.** The
  W3C `graph/` manifest (the `GRAPH`-pattern cases, ground and variable form)
  and `dataset/` manifest (`FROM` / `FROM NAMED` construction, including the
  empty-default-graph case D4 turns on).

  **Amendment (phase 3, #266).** This bullet originally said the two families
  are result-set tests that "fit the existing manifest-driven runner
  unchanged", under the `sparql11` suite key. Both halves were wrong.

  - The `sparql11` harness key does not run a query engine: its `Reasoner::ask`
    is a stub (`crates/owlrl/src/integration.rs`) that ignores the query, and
    the runner has no result-set `TestKind`. The repo's real query-evaluation
    gate is `harness/selected.toml`'s `[sparql_query]` section, run by
    `crates/sparql/tests/w3c_suite.rs` against both backends with a multiset
    result diff. The families land there, and are equally CI-gating (the
    `tests` job).
  - The families are in the W3C **SPARQL 1.0 (DAWG)** suite, not the SPARQL 1.1
    tarball. `crates/harness/scripts/fetch-w3c-suites.sh` grew a `sparql10`
    section with an explicit case allowlist to mirror them.

  The runner did need one change: a case directory may now carry `data.trig`
  (quads routed to their graphs) in place of `data.nt`. 24 of the 29 upstream
  cases are selected and green on both backends; the other 5 are listed with
  their gating reason in `harness/KNOWN-MANIFEST-BUGS.md`, which also records
  that no selected case grades the shipping `union` default-graph mode — D2's
  default is covered by `crates/sparql/tests/graph_query.rs` instead.
- **`sparql11` grows the update graph families.** The `add/`, `copy/`, `move/`,
  `clear/`, `drop/`, and graph-specific `delete-insert/` manifests. Gates S4.
- **`sparql11-syntax` needs no growth** — it already grades `GRAPH`, `WITH`, and
  `USING` syntax through `spargebra`, which parses all of them today. Parsing
  was never the gap.
- **Subset growth is concrete per phase, not aspirational.** Each phase's PR
  adds its named families to `harness/selected.toml` and lists the exact case
  IDs it turns on; a phase that grows behaviour without growing the selection is
  incomplete. Cases that cannot pass yet stay out of the selection with the
  gating reason recorded in `harness/KNOWN-MANIFEST-BUGS.md`, as the OWL 2 RL
  subset already does. The exact upstream case IDs are enumerated when
  `fetch-w3c-suites.sh` first pulls each manifest — this spec names the
  manifests, the phase plan names the IDs.
- **Differential check against the platform shape.** Beyond W3C: a test that
  replays a synthetic change feed (adds/dels at quad grain, with a mid-stream
  restart and a replayed batch) and asserts the store converges to the same
  state as a single clean application. Gates S6.

## Phasing

Each phase is independently shippable, tracked as a sub-issue of
[#261](https://github.com/sunstoneinstitute/horndb/issues/261) (this spec's
epic), and harness-gated: the SPEC-01 selected subset stays green throughout,
and each phase grows the subset per S7. Phases 1–4 have implementation plans
(`PLAN-28-01`..`04`); phase 5's is written when it is picked up.

1. **S1 — refuse, do not lie. Done.**
   *([#264](https://github.com/sunstoneinstitute/horndb/issues/264),
   `PLAN-28-01`)* Small and immediately
   correct. Turns a silent wrong answer into a 400. No storage work, no new
   algebra. Ship first, independently of everything below.
2. **S2 — graph-scoped access paths. Done.**
   *([#265](https://github.com/sunstoneinstitute/horndb/issues/265),
   `PLAN-28-02`)* `scan_graph`,
   `scan_predicate(graph, …)`, `graph_len`, visibility-filtered `graphs()`,
   whole-store `len`, and the `HornBackend` de-hardwiring. Pure plumbing with no
   user-visible behaviour change; prerequisite for phases 3–5.
3. **S3 — query. Done.**
   *([#266](https://github.com/sunstoneinstitute/horndb/issues/266),
   `PLAN-28-03`)* `Algebra::Graph`, ground and variable
   evaluation, dataset construction, the `default_graph` mode, path and pushdown
   scoping. Removes S1's query-side error, except for the two families of `GRAPH ?g` query
   S3 ends by naming, which stay refused. Grew `[sparql_query]` by the `graph/`
   and `dataset/` families (S7's amendment).
4. **S4 + S6 — update and idempotence.**
   *([#267](https://github.com/sunstoneinstitute/horndb/issues/267),
   `PLAN-28-04`)* Named-graph
   quads, the graph-management verbs, `WITH`/`USING`, `SILENT` fidelity, and the
   store-boundary idempotence contract. Grows `sparql11` by the update graph
   families; adds the change-feed replay test.
5. **S5 — Graph Store Protocol.**
   *([#268](https://github.com/sunstoneinstitute/horndb/issues/268))* The four routes, the
   status-code contract, the `PUT` read-diff-commit path. Adds the
   `sparql11-gsp` suite key and its runner support — a live-server harness kind,
   which is the expensive part of this phase (see Risks). Depends on phases 2
   and 4. On a reasoning-enabled store it additionally requires SPEC-29 D4's
   asserted-vs-derived separation, without which S5's `?default` restriction
   stands.

Phase 1 stands alone. Phases 3, 4, and 5 all depend on phase 2; 3 and 4 are
independent of each other; 5 depends on 4 for its write path.

Phase 5 is **independent of the data-platform integration path** and blocks
nothing on it: that platform writes through `/update` and reads through
`/query`, and keeps GSP in `graph-server` in front. S5 exists because it is what
makes HornDB usable as a standalone graph store, without `graph-server`.

## Acceptance criteria

1. **No silently-wrong named-graph answers (S1).** For a store holding data in
   both the default graph and a named graph, `GRAPH <g> { ?s ?p ?o }`,
   `GRAPH ?g { ?s ?p ?o }`, `SELECT … FROM <g> …`, and
   `SELECT … FROM NAMED <g> …` each return HTTP 400 naming the construct — never
   a 200 carrying default-graph rows. Verified before phase 3 lands and
   superseded by criterion 2 after it.
2. **`GRAPH` evaluates correctly (S3).** *Met by phase 3, with the two refused
   `GRAPH ?g` shapes S3 names.* The `[sparql_query]` selection (not `sparql11`
   — see S7's amendment) includes the W3C `graph/` and `dataset/` families and
   is green on both backends: 24 of 29 cases, the other 5 in
   `harness/KNOWN-MANIFEST-BUGS.md`. `GRAPH ?g` binds `?g` to each named graph
   and never to the default graph. A `FROM NAMED`-only query has an empty
   default graph.
3. **The default-graph mode is real and switchable (S3/D2).** *Met by phase 3,
   pinned by `crates/sparql/tests/graph_query.rs` — no W3C case grades it.*
   On a store whose
   data lives entirely in named graphs, an unqualified `SELECT ?s ?p ?o` returns
   every quad outside the reserved namespace under `default_graph = "union"` and
   zero rows under `"strict"`;
   the SPEC-26 per-query URL override flips it for one query without disturbing
   server config. Adding `FROM <g>` gives the same answer in both modes.
4. **Graph-scoped reads cost graph-scale (S2).** `scan_graph` on one small graph
   in a store holding ≥1000 graphs runs in time proportional to that graph's
   size — measured, with the number recorded in `docs/benchmarks.md`. No
   remaining call site of `scan_predicate_default_graph` exists, and
   `StoreSnapshot::len` is whole-store (the inverted
   `store.rs::snapshot_len_is_default_graph_scoped` test), with the incremental
   circuit moved onto `graph_len` in the same change (S2).
5. **Named-graph update works (S4).** The `sparql11` update graph families are
   in the selection and green. A round trip — `INSERT DATA { GRAPH <g> {…} }`,
   `COPY <g> TO <h>`, `DROP <g>`, `ASK { GRAPH <h> {…} }` — behaves per SPARQL
   1.1 §3, including `SILENT` on a missing source graph. A write targeting
   `https://horndb.io/graph/…` errors with and without `SILENT`.
6. **GSP is green (S5).** The `sparql11-gsp` suite key exists, its selection is
   non-empty and green, and the status-code contract in S5 is covered by tests
   including the 201-vs-204 first-write distinction, the empty-diff `PUT` no-op,
   the 400 on a write to a reserved graph IRI, and the `?default` refusal on a
   `--materialize` store.
7. **Replay converges (S6).** A synthetic at-least-once change feed — quad-grain
   adds and dels, a duplicated batch, a mid-stream restart from a stale cursor —
   produces a store byte-identical in quad content to a single clean
   application. Re-inserting a live quad and retracting an absent one each
   report an affected count of 0 and no error. `"01"^^xsd:integer` and
   `"1"^^xsd:integer` stay distinct quads through the whole path.
8. **Docs stay in sync (in-commit).** `docs/architecture.md` (including the
   stale `:319` claim that the store is default-graph-only), `TASKS.md`,
   `docs/specs/README.md`, and `docs/index.md` are updated in the commits that
   introduce the corresponding behaviour, per the root sync rules.

## Risks and open questions

- **The default-graph mode is a differential-test hazard.** The platform proxies
  `/sparql` to Oxigraph today, whose no-dataset default graph is the strict
  reading. A HornDB materializer running beside it under `union` will disagree
  with Oxigraph on every unqualified query. That is the *point* of D2 — union is
  the useful answer for a store whose data is all in named graphs — but any
  HornDB-vs-Oxigraph differential harness must set the mode explicitly, or it
  will report a stream of false failures. Decide the mode per comparison, and
  say which one a recorded number was taken under.
- **Empty named graphs and D11.** Declaring that a graph exists iff it holds a
  visible quad keeps storage simple, but W3C `CREATE`/`DROP` cases that
  distinguish an empty-but-existing graph from an absent one will fail. The
  fallback is a small explicit graph-existence set in the tier, independent of
  quad count. Settle this the moment the `clear/` and `drop/` manifests are
  fetched and graded — before building on D11, not after.
- **Thousands of small graphs versus per-partition overhead.** D7 makes the
  partition key `(graph, predicate)`. The platform expects thousands of graphs,
  each small, each with a handful of predicates — so the store holds a very
  large number of very small Arrow partitions, and the fixed per-partition
  overhead (six potential orderings, Roaring bitmaps, allocation headers) may
  dominate the actual data. SPEC-02 NF1's ≤50 B/triple budget was set against
  LUBM-shaped data in one graph and may not survive this shape. Measure on a
  synthetic thousand-graph corpus during phase 2 and, if it fails, decide then
  between a shared-partition layout with a graph column and a small-graph
  representation that skips the ordering machinery.
- **Variable-graph scans and the cardinality estimator.** D6 emits `?g` as a
  scan column, which the WCOJ estimator has never seen. `GRAPH ?g { ?s ?p ?o }`
  over a thousand graphs has a cardinality the current whole-store live-count
  bound describes badly. Expect estimator work in phase 3, and do not let a
  coarse estimate leak into a count *result* (S3). **Phase 3 outcome:** no
  estimator work was done — estimates stayed coarse (a whole-store upper bound
  is valid under any scope) and are structurally kept off the result path.
  Better estimates for many-graph plans remain open.
- **Pushdown regressions are silent by nature.** The count and group-count
  shortcuts exist to avoid materializing rows. If one of them ignores a graph
  scope it returns a plausible number, not an error — the exact failure mode S1
  was written to eliminate, reintroduced one layer down. Phase 3 needs a
  differential test that runs every pushdown-eligible shape with and without
  the pushdown enabled, inside a `GRAPH` scope, and compares. **Phase 3
  outcome:** that battery exists (`crates/sparql/src/plan/pushdown.rs`), and
  the pushdowns decline (`Ok(None)`) for any scope they were not explicitly
  taught, so an untaught scope falls back to the scan instead of counting the
  whole store.
- **GSP conformance needs a live server in the harness.** Every existing suite
  grades a parser or a reasoner in-process. The GSP tests are HTTP
  request/response pairs, so the runner grows a new kind that binds a port,
  boots the axum server, and tears it down. That is real harness work (and a CI
  ordering concern) that phase 5 must budget for rather than discover.
- **Conditional writes will come back.** D8 drops `If-Match` because HornDB has
  one lineage. The moment HornDB is asked to be the system of record rather than
  a materializer, lost-update protection matters and the contract has to be
  designed against whatever versioning exists then. Nothing in S5 should make
  that harder — keep the write path's commit step in one place.
