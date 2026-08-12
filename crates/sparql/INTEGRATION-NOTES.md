# SPEC-08 Integration Notes for `horndb-sparql`

These notes describe call sites that **SPEC-07's plan** is responsible
for implementing.

## F2 — PlanAdvisor at the SPARQL planner

Same contract as `wcoj/INTEGRATION-NOTES.md` — the SPARQL planner
constructs a `SubplanShape` from its algebra tree, calls
`registry.plan_advisor().advise(&shape)`, validates against its own
histograms, and falls back if implausible. NF2's 1 ms p99 budget
applies here too.

## F5 — Filtering by provenance in SPARQL

SPARQL queries should be able to filter on the provenance column
exposed by SPEC-02. SPEC-07's plan should:

1. Recognise the (engine-specific) predicate
   `<https://horndb.io/prov/source>` in `FILTER`
   clauses.
2. Map literal values `"symbolic"` and `"ml-derived"` onto the
   `MlProvenance` discriminants from SPEC-02's storage column.
3. Allow audit queries of the form:
   ```sparql
   SELECT ?s ?p ?o ?model WHERE {
     ?s ?p ?o .
     ?s <https://horndb.io/prov/source> "ml-derived" .
     ?s <https://horndb.io/prov/model>  ?model .
   }
   ```

## F3 — LLM → SPARQL endpoint (STAGE 2 — DEFERRED)

`POST /nl-query` is **not** part of Stage 0/1. When SPEC-07's plan
adds it, the implementation should:

1. Live in a new module (`crates/sparql/src/nl.rs`).
2. Take an injected `Arc<dyn LlmClient>` (trait to be defined in
   `horndb-ml` Stage 2) so the LLM provider is pluggable and the
   handler is testable without network.
3. Always return the generated SPARQL alongside the results (per
   SPEC-08 risks: "LLM SPARQL quality").
4. Defer cost reporting and training-data leakage controls to
   Stage 2+ per SPEC-08.

For Stage 0/1 the file remains absent — `horndb-ml` ships only
the boundary; the LLM client trait will land with the Stage 2 plan.

## GRAPH patterns and the query dataset (SPEC-28 phase 3, #266)

### How the graph scope travels

`translate.rs::translate_pattern` builds `Algebra::Graph { name, inner }`
from a `GRAPH` block, where `name` is a ground IRI or a variable.
`translate.rs::dataset_spec_from` turns `FROM`/`FROM NAMED` into a
`DatasetSpec` (all four `translate_query_with` arms — `SELECT`/`ASK`/
`CONSTRUCT`/`DESCRIBE`).

Lowering keeps **no** `Graph` node. `plan/lower.rs::lower_scoped` carries the
enclosing scope down and stamps it on every scan leaf (`BgpScan`,
`CountScan`, `GroupCountScan`, and transitively a `PathClosure`'s edge
sub-plan) — SPEC-28 D5: the scope is a scan parameter, never a post-filter,
because post-filtering a many-graph store to answer a one-graph question
costs O(store). Nested `GRAPH` follows SPARQL: innermost wins (ground form; a
nested `GRAPH` inside `GRAPH ?g` is refused — see below). There is no
fallback path — a scope that cannot be pushed is an error, not a silent
widening.

At execution the plan-level `GraphScope` plus the query's `DatasetSpec` and
mode make a `ScanScope` (`exec/scope.rs`), which resolves to a
`ResolvedScope` — the operator-level view, and the only place `PerGraph`
(`GRAPH ?g`) exists, because the scan operator loops the graphs itself and a
backend's single-scope read never sees it. `HornBackend` maps the rest onto
its own `SnapshotScope` and builds a **scoped** WCOJ snapshot. Only the two
no-dataset default-graph scopes (`union`, `strict`) are memoized. Every other
scope — a ground `GRAPH <g>`, a `FROM` list, each graph a `GRAPH ?g` visits —
is built and dropped per execution (`SnapshotScope::memoisable`, pinned by
`graph_scoped_snapshots_are_not_memoised`): caching them would let a client
walking `GRAPH <g1>`…`GRAPH <gN>` pin one six-ordering copy of the store per
graph named, evicted only by a write and reachable from an unauthenticated
`/query`. `MemStore` implements the same semantics over its own indexes: it
keeps the graph dimension *beside* the triple table (`graphs[i]` = the set
of graphs holding `triples[i]`), so its indexes and joins stay triple-keyed.

### Semantics

- **Ground `GRAPH <g>`** scans only `g`. An unknown graph IRI yields zero
  rows, never an error. `GRAPH <g> {}` — the standard existence probe —
  matches only if `g` holds a visible quad (SPEC-28 D11).
- **`GRAPH ?g`** binds the graph as an extra **scan output column**
  (`Slot::Id(TermId(g.0))`, since a `GraphId` *is* the interned `TermId`):
  one scan node looping over the graphs, so plan size is independent of graph
  count (D6). `?g` never binds the default graph (D3).
- **`FROM` / `FROM NAMED`** build the dataset per SPARQL 1.1 §13.2, including
  `FROM NAMED` with no `FROM` = **empty** default graph. `FROM` is a
  term-level set union of the named graphs; RDF-merge blank-node renaming is
  not implemented (the platform skolemizes upstream).
- **No dataset clause** → the default graph follows the `default_graph` mode:
  `union` (the default) is every non-reserved graph, deduped so a triple in
  two graphs is one row; `strict` is the default-graph sentinel alone. The
  named-graph set is every non-reserved graph in both modes.
- **Reserved graphs** (IRI prefix `https://horndb.io/graph/`,
  `exec::is_reserved_graph`) are outside the union and outside `GRAPH ?g`
  enumeration. Naming one explicitly — `FROM <g>`, `FROM NAMED <g>`, ground
  `GRAPH <g>` — is the opt-in.
- **Property paths** inherit the scope on their edge sub-plan, so the closure
  is computed over the scoped edge relation. A `g1 → g2 → g1` chain does not
  connect inside `GRAPH <g1>`; under the union default graph it does, because
  the union *is* the default graph.
- **Count and group-count pushdowns** are scope-aware or **decline**
  (`Ok(None)`, falling back to the scan). Decline-by-default is what makes
  adding a scope safe: a shortcut that cannot express the scope can never
  answer with a whole-store count. Cardinality *estimates* stay coarse (a
  whole-store count is a valid upper bound under any scope) and reach only the
  planner and `EXPLAIN`, never a `Batch`.

### The mode setting and its per-query override

`SparqlConfig.default_graph` (`lib.rs`, `DefaultGraphMode::{Union, Strict}`)
comes from `[server.limits].default_graph` — a typed `union | strict` enum in
`horndb-config`, so a bad value is rejected at startup naming the file and
key. `serve.rs` builds one `SparqlConfig` into `AppState.cfg`; the handlers
use it instead of the `SparqlConfig::default()` they hardcoded before. A
single query overrides it with the `default_graph` URL or form parameter on
all three protocol channels (GET, form-POST, direct POST); an unparseable
value is a 400 naming the key. Spelling: `default_graph`, the config-key
spelling — SPEC-26 S4 names every override after its field, and
`default-graph` sits one suffix from the SPARQL 1.1 Protocol's reserved
`default-graph-uri`, which phase 5's GSP needs on the same endpoint.

### Two families of `GRAPH ?g` query are refused, not answered

Both refusals are raised in `plan/lower.rs` as `UnsupportedAlgebra` (HTTP 400)
and name the offending construct. Both exist because the graph name is bound
on the scan **leaf**, not joined on after the block is evaluated.

1. **A barrier between the wrapper and its scan leaves**
   (`per_graph_barrier`): a sub-`SELECT`, `DISTINCT`, `GROUP BY`/aggregate,
   `LIMIT`/`OFFSET`, any property path, a nested `GRAPH`, or a `VALUES` that
   is not joined against a scoped arm. Each drops or merges the graph column,
   so rows would come back with `?g` unbound or mixed across graphs. The same
   constructs *above* the wrapper are fine. A quad-free arm is exempt where
   the other arm's graph column reaches every joined row: either side of a
   `Join`, or an `OPTIONAL`'s right arm — so
   `GRAPH ?g { ?s ?p ?o VALUES ?o { … } }` answers
   (`values_inside_graph_var_answers`).
2. **The block reads `?g` where leaf-binding diverges from SPARQL 1.1
   §18.2.2.2's post-join** (`per_graph_var_divergence`): any expression
   (`FILTER`, a `BIND` expression, an `OPTIONAL` condition, `ORDER BY`),
   `BIND(… AS ?g)`, or any mention of `?g` in a `LeftJoin`'s right arm.
   Allowed — because there the leaf's equality filter *is* the post-join — is
   `?g` in a `Bgp` triple position, or in a `VALUES` column joined against a
   scoped arm. (The divergence rule also permits a `VALUES`-supplied `?g`
   under `Union` or a `LeftJoin`'s left arm, but `per_graph_barrier` runs
   first and refuses those, so only the `Join` case reaches evaluation.)

Lifting either means evaluating the whole block once per graph with `?g` free
and joining the graph name on afterwards. That is a design change against
D5/D6, not a bug fix. `harness/KNOWN-MANIFEST-BUGS.md` names the W3C cases
each refusal costs.

### History

Before SPEC-28 phase 1 (#264), `translate_pattern` **discarded** the `GRAPH`
wrapper and the four `translate_query_with` arms bound `dataset: _`, so
`GRAPH <g> { P }` evaluated `P` against the default graph and returned HTTP
200 — a wrong answer a caller could not detect. Phase 1 turned that into a
400; phase 3 (this section) replaced the refusal with real evaluation, except
for the two families above. Storage had been quad-aware since SPEC-25 S1 (#225)
the whole time, so the Stage-1 premise that there was nothing to scope against
had already stopped being true.

### Conformance

W3C `graph/` and `dataset/` families, from the **SPARQL 1.0 (DAWG)** suite
(not the 1.1 tarball), mirrored per case under
`crates/harness/tests/fixtures/sparql11/selected_subset/` and run by
`crates/sparql/tests/w3c_suite.rs` on both backends via
`harness/selected.toml`'s `[sparql_query]` section. A case dir may carry
`data.trig` (quads routed to their graphs) instead of `data.nt`. 24 of 29
cases are selected and green; the other 5, and the note that no selected case
grades the shipping `union` mode, are in `harness/KNOWN-MANIFEST-BUGS.md`.
Direct pins live in `crates/sparql/tests/graph_query.rs`.

## HornBackend — storage/WCOJ/closure wiring (2026-06-11, #67)

`crates/sparql/src/exec/horn.rs` implements the `Executor` + `Store`
seam on top of `horndb-storage` and `horndb-wcoj`.

### Term identity and dictionary

All term identity lives in `horndb_storage::Dictionary` (kind-tagged
`TermId`s). This fixes the Stage-1 `MemStore` behaviour where terms
were stored as bare lexical strings and term kinds were recovered
heuristically from lexical shape (`classify_lexical` in `exec/mod.rs`).
Literals (leading `"`) were recovered correctly, but blank nodes were
stored as bare labels indistinguishable from IRIs and therefore surfaced
as `Term::Iri`. The dictionary's kind-tagged `TermId`s make recovery
exact for all three kinds. RDF term identity is preserved for typed
literals: only canonical-form `xsd:integer` literals (e.g. `"42"`)
take the inline-int `TermId` fast path, while non-canonical lexical
spellings (`"042"`, `"+42"`) keep distinct dictionary identities and
round-trip their exact lexical form. BGP matching is therefore
term-based (lexical form + datatype), as SPARQL semantics require.

### Native delete path (SPEC-25 S1)

`horndb-storage` gives every stored tuple a `[begin, end)` visibility
lifetime on the tier commit clock (SPEC-25 S1). `DELETE DATA` and
`CLEAR`/`DROP` retract through `Store::retract_triples` /
`Tier::retract_quad_batch`, which stamp the matching live row's `end`;
the row stays physically present as history until compaction. Every
store read (`scan_all_term_ids`, `triple_count`, …) is already
visibility-filtered, so `HornBackend` applies no overlay when building
the WCOJ snapshot. `HornBackend` keeps a `live_keys: HashSet<QuadKey>`
mirror of currently-live quads (`QuadKey { g, s, p, o }`, keyed by
*quad* since SPEC-28 S2 — the same triple in two graphs is two entries)
— not for visibility filtering, but to give `INSERT DATA` idempotency
and `DELETE DATA` no-op detection an O(1) check, avoiding storage's
O(partition-size) `StoreSnapshot::contains` on the bulk-load hot path.

### Lazily-rebuilt VecTripleSource snapshot

BGP execution requires all six sort orderings (SPO, SOP, PSO, POS,
OSP, OPS). `HornBackend` builds a `VecTripleSource` lazily on the first
query after any mutation and caches it behind a
`Mutex<HashMap<SnapshotScope, Arc<…>>>`. Since SPEC-28 phase 3 that map holds
**at most two** entries, ever: the `union` and the `strict` no-dataset default
graph. Every other scope — a ground `GRAPH <g>`, a `FROM` list, and each graph
a `GRAPH ?g` visits — is built and dropped per execution (see the GRAPH
patterns section above for why).
The snapshot holds all six orderings eagerly sorted; at ~144 bytes/triple
steady-state snapshot cost (construction briefly peaks ~168 B/triple
while the input vec is still alive) this is a documented Stage-1 cost.
The cache is cleared wholesale on every write (insert or delete).

A follow-up item exists to replace this with a direct `TripleSource`
over the columnar partitions, avoiding the full-copy rebuild.

### Batched-insert core (`insert_oxrdf_batch`)

Inserting triples one at a time via `Store::insert_triple` triggers a
per-predicate partition rebuild in `horndb-storage` on each call, giving
O(n²) cost for a bulk load. `insert_oxrdf_batch` addresses this with a
read-compute / write-commit split:

1. Phase 1 (read-only): intern all terms; drop any triple already live
   (an O(1) `live_keys` check) or repeated within the batch; collect the
   storage batch. Intern failures skip the triple (lenient for bulk
   loads — the single-triple `insert_oxrdf` propagates intern errors
   instead).
2. Phase 2 (write): call `store.insert_quads` once for the surviving
   entries, rebuilding each predicate partition at most once, then mark
   them live and invalidate the WCOJ snapshot only on success.

`load_lexical_triples` and `insert_algebra_triples_bulk` both delegate
to `insert_oxrdf_batch`. The `serve` binary uses it for the initial load.

Known Stage-1 limits of the update path: HTTP `INSERT DATA` / `DELETE
DATA` (`update.rs::apply_update`) still applies triples one at a time
through the `Store` trait, so a very large update body pays the
per-call partition-rebuild cost the bulk loaders avoid — batching
`apply_update` is a candidate follow-up under the SPEC-07 epic (#7).
Likewise, a store populated via `--materialize` is not re-reasoned on
subsequent updates; incremental maintenance of the closure is SPEC-06
territory.

### `reasoner` feature and `load_with_reasoning`

The `reasoner` feature (default-on) adds a `load_with_reasoning`
function that drives the `horndb_owlrl::integration::Engine` (RuleFiring
backend) over an `oxrdf::Dataset` and loads the full materialized closure
— asserted base plus all inferred triples — into the `HornBackend` in a
single `insert_oxrdf_batch` call. GraphBLAS is not required; only the
compiled-rule RuleFiring backend is used here. The `serve` binary exposes
this path via the `--materialize` flag.

### GRAPH patterns

`HornBackend`'s reads are graph-scoped: `wcoj_snapshot` takes a resolved
scope; only the two whole-store default-graph scopes are memoized — every
graph-scoped read builds and drops its own snapshot. Its **writes** are now
graph-scoped too (SPEC-28 phase 4, #267): every Update write form routes to
the graph it names, through `Store::apply_quads`/`clear_graph`. See "GRAPH
patterns and the query dataset" above for the read side and "Named-graph
Update" below for the write side.

### Non-recursive property paths (#49)

`translate.rs::translate_path` lowers the non-recursive path operators to
algebra at translation time, so the planner/runtime never see path nodes:

- `/` (Sequence) and `^` (Inverse) expand into triple patterns, as before.
- `|` (Alternative) and `?` (ZeroOrOne) lower to `Union`.
- `!` (NegatedPropertySet) lowers to a wildcard-predicate BGP wrapped in a
  `Filter` of `NOT IN {p1,…,pn}`. spargebra carries only forward predicates
  in `NegatedPropertySet`; an inverse member `!(^p)` parses as
  `Reverse(NegatedPropertySet([p]))` and is handled by the `Reverse` arm.

Two design points worth recording:

1. **Blank nodes in WHERE patterns are join variables.** spargebra flattens
   a path *sequence* `s p1/p2 o` into two patterns joined by a freshly minted
   blank node. A blank node in a query pattern is a non-distinguished variable
   (SPARQL 1.1 §4.1.4), so `match_term` now maps blank-node subject/object
   positions to deterministically named join variables instead of constants.
   This is what makes `Alternative`/`NegatedPropertySet` sub-paths compose
   across an algebra `Join`, and it also fixes a *latent* bug: plain `/`
   sequences were only ever joined correctly when both steps landed in a single
   BGP — across a `Join` boundary they silently produced no rows.

2. **Zero-length `?` is bounded.** `p?` is `Union(zero-length, single-step)`.
   The zero-length branch is lowered without enumerating the graph: both
   endpoints ground → equality test; one variable + one ground → bind the
   variable to the ground endpoint. Both endpoints being variables — whether
   two *distinct* ones (`?s p? ?o`) or the *same* one (`?x p? ?x`) — would have
   to range the variable over every node in the graph, so those cases are
   rejected with `UnsupportedPathOp` (returning the unit relation for `?x p? ?x`
   would wrongly emit an unbound `?x` row). They belong with the recursive
   `*`/`+` increment (#50) that routes through closure.

3. **Hidden path variables are query-globally unique and user-unspellable.**
   The intermediate variables minted during path/blank-node lowering (the
   `Sequence` join node, the `NegatedPropertySet` predicate slot, the
   blank-node existential) come from `hidden_var_name`. Two properties matter:
   uniqueness — the path-minted ones draw a process-global counter so two
   distinct path patterns in one query never reuse a hidden name and get
   spuriously joined (a per-pattern counter would, e.g. with two `!` sets) —
   and **un-spellability**: every hidden name carries the `?pp` prefix, and `?`
   cannot appear in a SPARQL `VARNAME`, so a user variable can never collide
   with (and thus never read or constrain) a hidden one. Because `?pp…` is not
   a valid `spargebra::Variable`, `translate_path` carries its endpoints as
   already-lowered `Term`s (not `TermPattern`s) and mints the `Sequence` join
   node as a `Term::Var` directly — routing it through `spargebra::Variable::new`
   would reject the name and fail otherwise-valid nested paths like `(p/q)?`.

4. **A single path expression is set-valued.** Several routes can connect the
   same `(start, end)` pair — distinct `|` branches, several unexcluded
   predicates of `!`, or the `?` zero-length/one-step overlap — and the lowering
   emits one witness per route (the witnesses differ only in the *hidden*
   columns). To match SPARQL's set semantics, `GraphPattern::Path` projects the
   result down to `visible_path_vars` and wraps it in `Distinct`. The
   projection drops only the **path-internal witnesses** (`?pp_seq_*`,
   `?pp_neg_*`); it deliberately **keeps blank-node-endpoint variables**
   (`?pp_bnode_*`), because a query blank node may co-refer with the *enclosing*
   graph pattern (`_:b :p ?o . _:b :q ?x`) and must survive to join outward —
   dropping it would Cartesian-explode the surrounding pattern. When both
   endpoints are ground the path is a pure existence test, collapsed to at most
   one solution via `Slice(0, 1)` — `Project { vars: [] }` can't express this
   because the runtime reads an empty projection as `SELECT *` and would keep
   the hidden columns.

Two Stage-1 approximations are documented in code: a zero-length `?` does not
node-membership-check a ground endpoint (so `?s p? <urn:absent>` self-matches an
absent term — see `zero_length_path`), and both-variable `?` endpoints are
rejected rather than enumerated. Both belong with the recursive `*`/`+`
increment (#50), which routes through closure and is the natural home for proper
node-set semantics. Kleene `*`/`+` themselves remain rejected
(`UnsupportedPathOp`).

## Named-graph Update (#52, SPEC-28 phase 4 / S4+S6, #267)

`update.rs` implements every Update form over real named graphs: quad data
(`INSERT DATA`/`DELETE DATA`), pattern updates (`INSERT`/`DELETE … WHERE`,
`WITH`/`USING`/`USING NAMED`), the graph-management verbs
`LOAD`/`CLEAR`/`DROP`/`CREATE` and (via spargebra desugaring) `ADD`/`MOVE`/
`COPY`, and multi-operation sequences. The parser classifies a single
data/pattern operation as before; everything else — a graph-management verb
or any `;`-joined sequence — becomes `ParsedUpdate::GraphManagement`, and the
executor walks the whole operation list in order.

**History.** Before phase 4 the execution store was default-graph only:
`HornBackend` wrote through a triple-grain path and rejected any write
naming a graph (error unless `SILENT`, else a no-op) even though
`horndb-storage` had been quad-aware since SPEC-25 S1 (#225) — the limit was
in this crate, not storage. Phase 4 closed that gap by making the
`exec::Store` write trait quad-shaped (`apply_quads`, `clear_graph`,
`graph_exists`, `graphs`, `scan_graph_quads`) and rewriting `update.rs` to
route every write by graph instead of refusing named ones. The write seam is
`Store::apply_quads` — **one atomic, idempotent, counted batch of
`(graph, s, p, o)` quads per Update operation** (SPEC-28 S6; see the store
boundary note in `crates/storage/INTEGRATION-NOTES.md`), never one call per
quad — so `INSERT DATA`/`DELETE DATA` with mixed `GRAPH <g> { … }` blocks and
a default-graph tail commit at one store version.

Routing by construct:

- **Quad data** — `INSERT DATA`/`DELETE DATA` group every quad (each
  carrying `GRAPH <g>` or the default graph) into one `apply_quads` call per
  operation.
- **Pattern updates** — each DELETE/INSERT template quad routes by its own
  `GraphNamePattern` (default / named / a WHERE-bound variable, via
  `resolve_graph_name`); the whole operation is still one `apply_quads` call
  (deletions before insertions, SPARQL 1.1 §3.1.3).
- **`WITH`/`USING`/`USING NAMED`** — the WHERE clause runs through the
  phase-3 query translate path (`translate_where`), so it understands
  `GRAPH`. `USING`/`USING NAMED` build the WHERE dataset via the phase-3
  `DatasetSpec` machinery (`dataset_spec_from`, made `pub(crate)` for this).
  spargebra 0.4.6 desugars a `WITH <g>` clause by injecting `<g>` into every
  default-graph DELETE/INSERT template quad and, absent an explicit `USING`,
  setting `using = Some(default:[g])` — it does **not** wrap the WHERE
  pattern in `GraphPattern::Graph`. Honouring `using` when building the WHERE
  dataset is therefore correct; wrapping the pattern too would double-scope.
  This finding is a doc comment on `apply_delete_insert`.
- **Graph management (D11: a graph exists iff it holds ≥1 visible quad, no
  registry)** — `CREATE <g>`: absent graph succeeds as a no-op, existing
  graph errors unless `SILENT`. `CLEAR`/`DROP <g>`: absent graph errors
  unless `SILENT`, present graph retracts every visible quad through
  `clear_graph` — **never a structural unlink**; there is no separate
  existence record to remove. `DROP ALL` sweeps the default graph and every
  **non-reserved** named graph via `graphs()`, quad by quad — never
  `clear_graph(AllGraphs)`, which would also wipe reserved graphs.
  `CLEAR`/`DROP NAMED` sweeps the non-reserved named graphs only.
- **`LOAD <source> [INTO GRAPH <g>]`** — triples formats (`.nt`/`.ttl`/
  default) route to the destination (default graph if no `INTO`); a plain
  `LOAD` of a dataset format (`.nq`/`.trig`) routes each quad to its own
  named graph; a dataset format combined with `INTO GRAPH` is a routing
  error (redirecting a quad source to one graph is undefined). Still
  `file:`-only fetch — the workspace carries no HTTP client, so a remote
  (`http(s):`) source is an error unless `SILENT` (→ E5, #189). The `file:`
  authority parsing (`file_iri_to_path`: `file:///abs`, `file://localhost/abs`,
  `file:/abs` are local, a non-empty non-`localhost` authority is rejected),
  percent-decoding, extension-based serialization pick (`.nt`/`.nq`/`.trig`,
  else Turtle, via `oxttl`), and verbatim (non-freshened) blank-node labels
  are unchanged from Stage-1.
- **`ADD`/`MOVE`/`COPY`** — spargebra rewrites these into `Drop` +
  `DeleteInsert` sequences per the W3C spec (the same-graph identity case,
  `… <g> TO <g>`, rewrites to zero operations, a valid no-op). The desugared
  ops now execute against real named graphs.
- **The reserved namespace is closed to writes.** A `https://horndb.io/graph/`
  prefix check (`is_reserved_graph`) covers every write form — data quads,
  templates, `CREATE`/`CLEAR`/`DROP`, `LOAD INTO`, `ADD`/`MOVE`/`COPY`
  destinations. It runs **before** any `SILENT`/existence logic and is
  **not suppressible by `SILENT`** — this is a permission-shaped refusal, not
  a missing-graph condition. Reads of reserved graphs stay allowed.

### `SILENT` fidelity for `ADD`/`MOVE`/`COPY`

spargebra drops the `SILENT` flag when it desugars `ADD`/`MOVE`/`COPY` into
`Drop`+`DeleteInsert` — its parser's `Add`/`Move`/`Copy` rules take `silent`
but discard it for `ADD`, and keep it only on `MOVE`/`COPY`'s source-`Drop`.
Since `SILENT` changes observable behaviour here (an absent source graph is a
no-op when silent, an error otherwise — SPARQL 1.1 §3.2.3/§3.2.5), `update.rs`
recovers the flag with a source-text pre-scan rather than accepting the loss:

- `recover_amc_hints` is a hand-rolled tokenizer (no regex) over the raw update
  text. It skips the three lexical contexts a bare keyword scan would trip on —
  `# …` comments, `<…>` IRIs, and `"…"`/`'…'` string literals (single- and
  triple-quoted) — and records one hint per `ADD`/`MOVE`/`COPY` occurrence, in
  source order. Each hint carries `(silent, source, is_identity)`: the recovered
  `SILENT` flag, the source operand (`DEFAULT` / `Named(<iri>)` / `Unknown` when
  the text alone can't resolve it, e.g. a prefixed name), and whether the op is
  the W3C identity case (`source == destination`, which spargebra desugars to
  zero ops).
- The hints drive the missing-source preflight: for each non-silent,
  non-identity hint, an absent source graph is an error. The source IRI comes
  from the hint's text-recovered operand when that resolved it (`Named`); a
  `DEFAULT` source always exists and is skipped. When the text could not resolve
  it (`Unknown`, e.g. a prefixed name `ex:g`), the check falls back to the
  desugared copy-op's source IRI, which the parser has already expanded —
  resolved structurally by `amc_copy_source`. Identity occurrences desugar to
  zero ops, so excluding them lines the remaining hints up 1:1 with the copy-ops
  by order: this is why an identity op (one verb token, zero desugared ops) no
  longer miscounts the alignment (the original bug) and a user's `SILENT` is
  always honoured. The sweep runs before any mutation, so e.g. a non-silent
  `COPY <absent> TO DEFAULT` — or `COPY ex:absent TO <dst>` — which desugars to a
  destructive `Drop` followed by a copy from a missing source, aborts before the
  `Drop` runs.

This tokenizer is a documented stopgap, not a permanent design choice: an
upstream issue is to be filed against the spargebra (oxigraph) tracker asking
for a structured `Add`/`Move`/`Copy` op, or a preserved `silent` flag on the
desugared ops, and linked from the doc comment on `recover_amc_hints`;
the whole tokenizer is deletable the day that ships.

**Atomicity.** A multi-operation update must not partially apply on failure
(SPARQL 1.1 §3.1.3). `apply_update_with` preflights the whole request against
the **pre-update** store first — a recovered-`SILENT` source-existence sweep
over the `recover_amc_hints` hints, then `validate_op` per operation
(reserved-namespace checks, D11 existence, `LOAD` routing/fetch, and the
WHERE-clause `translate_where`+`planner::plan` so an unsupported algebra
construct like `SERVICE`/`MINUS` is caught) — and only mutates once the whole
sequence is known-applyable. One store batch per operation, applied in
request order, never collapsed.

**Documented limitation.** The preflight reads D11 existence against the
pre-update store. That is exact for a single operation and for independent
operations, but a pathological multi-op request that flips one graph's
existence *between* operations (e.g. an earlier op creates or empties a
graph a later op then existence-checks) can, in principle, pass preflight
and still hit an existence error at apply time after an earlier op has
already mutated the store. Closing this needs store-level rollback, which is
out of scope here (→ `SPEC-30`); real graph management uses `SILENT` to
avoid the edge case, and no shipped test exercises it.

**Turtle/TriG base IRI.** `LOAD` passes the source IRI as the parser base, so a
document with relative IRIs (`<s> <p> <o> .`) resolves against its own IRI —
matching the storage Turtle loader. N-Triples/N-Quads need no base.

**Conformance.** `crates/sparql/tests/update_named_graph.rs` and
`update_graph_mgmt.rs` cover routing, D11, `SILENT` recovery, and the
reserved-namespace refusal on both backends. The W3C SPARQL 1.1 Update graph
families are wired through the harness's `[sparql_update]` selection
(`harness/selected.toml`): 33 of 36 fetched `UpdateEvaluationTest` cases are
selected and green on both backends; the other 3 (`clear-graph-01`,
`clear-named-01`, `clear-all-01`) test an empty-but-existing graph, which D11
cannot distinguish from an absent one, and are excluded with rationale in
`harness/KNOWN-MANIFEST-BUGS.md`.

**Deferred:** remote (`http(s):`) `LOAD` still waits on an HTTP client
decision (→ E5, #189); the Graph Store Protocol (direct REST access to named
graphs) is `SPEC-28` phase 5, separate and not started (#54, #268).

## EXPLAIN pragma (F9, #53)

The non-standard `EXPLAIN` pragma is recognised **before** spargebra sees the
text, because spargebra has no `EXPLAIN` keyword. `parser::parse_query`
strips a leading, whitespace-delimited, case-insensitive `EXPLAIN` (optionally
`EXPLAIN JSON`) token and wraps the inner parse as
`ParsedQuery::Explain { inner, json }`. The keyword must lead the request (it
precedes any `PREFIX`/`BASE` prologue) and needs a trailing whitespace boundary,
so a query starting with `?explainme` or an IRI is never mistaken for it; a bare
`EXPLAIN` with no following query surfaces as the inner parse error.

`api::execute_query_with` handles the `Explain` arm by translating + planning
the wrapped query and **not running it** (`plan_of` shares the translate→plan
path with the executing arms but stops before `Runtime::run`). Rendering lives
in `plan::explain`: an indented operator tree (`ExplainFormat::Text`) or a JSON
object tree (`ExplainFormat::Json`), returned as `QueryAnswer::Explanation`.

**Execution mode.** The header `mode:` line reports the entailment-regime
execution mode. Today the only mode is `Materialized` (the simple regime, or an
OWL-RL closure pre-written by SPEC-04/05); backward-chained mode (#55) is not yet
selectable, so the renderer prints `materialized` and labels backward-chaining
as not-yet-available. When #55 lands, `ExecutionMode` gains the backward variant
and the API picks it per query.

**Cardinality.** `Executor` gained `cardinality_estimate(&[TriplePattern]) ->
Option<usize>` (default `None`). `MemStore` returns the leading-pattern index
size (exact for a single pattern, an upper bound for a multi-pattern BGP);
`HornBackend` returns the live triple count as a sound upper bound (no
per-pattern statistic is exposed at the seam yet — SPEC-02's dictionary store
will carry index histograms). `plan::explain::estimate` combines child estimates
with textbook per-operator rules (join = product, union = sum, slice caps at
`length`, filter/distinct/project pass through). Numbers are estimates, surfaced
with a `~` prefix — there is no cost model (`plan::planner` is a 1:1 lowering).

**Deferred:** "chosen indexes" display (no index chooser exists; the plan is a
1:1 lowering) and the real materialized-vs-backward mode selection (with #55).
The `/query` handler serves the rendering as `text/plain` (text) or
`application/json` (JSON) by pragma — not by `Accept`, since EXPLAIN output is
not a SPARQL results document. Coverage: `tests/explain_pragma.rs`,
`tests/parser_basic.rs`, the `plan::explain` unit tests, and the
`/query` EXPLAIN server tests in `tests/server_http.rs`.

## HTTP streaming results (#128, 2026-07-06)

- `Runtime::run_stream` returns a `BindingsStream` (chunked decode at the
  boundary); `run` collects it — signature unchanged. `api::plan_select`
  is the planning-only SELECT entry the streaming handler uses.
- The `/query` handler streams plain SELECTs: exec+decode+serialize run in
  `spawn_blocking` (the store read guard and the `Op` tree are `!Send`),
  serialized `Bytes` cross to a `ChannelBody` over a bounded mpsc.
- Error contract: first chunk is pre-buffered → early errors are HTTP 400;
  mid-stream errors abort the chunked body (no terminator) — clients detect
  truncation at the protocol level. No format can express a trailing error.
- The read lock is now held until the client drains a streamed SELECT
  (writers wait; readers don't). Accepted until SPEC-02 MVCC. Corollary
  fixed in the same branch: `/update` takes its write lock inside
  `spawn_blocking` — blocking a runtime worker on `write()` while a slow
  reader drains could otherwise wedge the whole server.
- CONSTRUCT/DESCRIBE streaming deferred (#TODO); UPDATE must stay
  materialized (SPARQL 1.1 §3.1.3 pre-update snapshot semantics).

Review follow-ups (non-blocking, from the branch's code reviews):

- A panic (not `SparqlError`) in the blocking serializer closure drops `tx`
  without an `Err`, so the client sees a *cleanly terminated* truncated
  document (undetectable for CSV/TSV) and the panic is swallowed with the
  dropped `JoinHandle`. A drop-guard that sends `Err` on unwind would fix
  both.
- No ceiling on concurrent streamed SELECTs: each holds a blocking-pool
  thread (default cap 512) for the full drain; slow clients can exhaust
  the pool and queue new SELECTs indefinitely (no timeouts anywhere in
  Stage 1). SPEC-22 hardening list.
- `api::plan_select` duplicates `execute_query_with`'s Select-arm
  translate→plan sequence. Nothing diverges today (the pushdown rewrite
  lives inside `run_stream`/`build`), but if a rewrite step is ever added
  between translate and plan, extract a shared helper first.
- Measured on hornbench (2026-07-06, AMD Ryzen 7 7700, b94ba14 vs main):
  full-scan 5M-triple SELECT peak RSS 4.8 GiB vs 37.2 GiB (-87%, the
  query no longer adds to the load-time peak) and 4.9x faster drain --
  but LDBC SPB-256 aggregation-qps paid ~3% (35.4/34.9 vs main's
  36.2-36.4, GraphDB control flat): the per-query `spawn_blocking` +
  channel hop is measurable on small results. If that 3% matters, a
  size-based fast path (serialize inline when the first chunk is also
  the last) is the obvious lever. Implemented: single-chunk results now
  reply as a plain sized body (oneshot first-reply; the chunk-2 peek
  keeps the mid-stream abort contract). Measured after: 36.12 qps
  (b0a701b) vs main's 36.2-36.4 nightly cluster - recovered to noise.

Full rationale: `docs/specs/SPEC-22-http-streaming-results.md`.

## Count pushdown (#128: #144 first cut + 2026-07-06 extensions)

The pushdown pass (`plan/pushdown.rs::rewrite`) lowers count-only aggregation
shapes into scan-side count leaves so the runtime never materializes solution
rows for them:

- `COUNT(*)` / `COUNT(?bound-bgp-var)` over a bare BGP → `CountScan` +
  `Executor::count_bgp` (landed 2026-06-30).
- The same with an intervening `FILTER` that is a conjunction of
  `?v = <const>` / `sameTerm(?v, <const>)` equalities → the constants are
  substituted into the BGP first. Result-invariant because engine `Expr::Eq`
  is structural term equality over oxrdf-normalized forms, which coincides
  with the dictionary term identity BGP constants match by; if `Expr::Eq`
  ever gains numeric *value* semantics, the literal-constant case must be
  restricted to IRIs (pinned by `eq_filter_literal_term_identity_pin`).
- `GROUP BY` keys and/or multiple plain counts → `GroupCountScan` +
  `Executor::count_bgp_grouped`. `HornBackend` answers it by hashing the raw
  u64 WCOJ key columns (no `Row` build, no decode); other backends fall back
  to scan + hash-count on the key columns. Output rows sort by
  decoded-lexical key, byte-identical to `eval_group_native`'s order
  (observable under LIMIT).

`HornBackend::count_bgp_grouped` is the fourth instance of the `scan_bgp`
pattern-compilation block (`keep in sync` markers in `exec/horn.rs`); if a
fifth instance is ever needed, extract a shared `compile_patterns` helper
instead.

Deferred with reasons (mixed count+value aggregates, `COUNT(DISTINCT …)`,
non-equality filters, partial inlining, zero-aggregate `GROUP BY`):
`docs/specs/SPEC-21-count-pushdown-extensions.md`.

## SPEC-23 Phase 1 — logical IR + pass pipeline (#201)

`planner::plan` now runs `Algebra → LogicalPlan → run_passes → PhysicalPlan`
(`plan/{logical,types,pass,lower}.rs`). Decisions worth knowing before you
extend it:

- **Lowering is deliberately naive.** `lower_algebra` is a 1:1 image of the
  algebra; all transformation happens in registered passes so a plan change
  bisects to one `PassId`. Do not fold rewrites into the lowering.
  (`lower_physical` takes the plan by value and moves the field vectors —
  the pipeline's only deep copy is the one `lower_algebra` makes.)
- **`CoalesceBgp` has no syntax-reachable producer since the SPEC-28
  phase-1 refusal (#264).** spargebra merges adjacent triple patterns,
  and the Stage-1 `GRAPH` lowering — previously the only route from real
  syntax to `Algebra::Join(Bgp, Bgp)` — now errors instead. The pass is
  exercised by hand-built algebra
  (`disjoint_var_bgps_coalesce_and_stay_result_equivalent`,
  `join_of_bare_bgps_coalesces_to_one_flat_scan`) and kept for SPEC-28
  phase 3 (PLAN-28-03), whose `GRAPH` lowering re-creates the shape with
  per-scan graph scopes and adds the equal-scope merge guard. Every
  query keeps its pre-pipeline plan byte-for-byte (the golden battery).
- **Post-pass debug validation is differential, not absolute.** Legal SPARQL
  may reference variables its pattern never binds (`FILTER(?z = <iri>)` with
  unbound `?z` drops rows; `SELECT ?z` projects it unbound), so
  `pass::dangling_refs` (a multiset of `NodeKind:?var` tags covering
  Project lists and Filter / LeftJoin-ON / Extend / OrderBy /
  aggregate-input expressions) is compared against each pass's own input —
  a pass may not *increase* any tag's count, but parser-supplied dangling
  refs survive. The baseline rolls forward per pass so a regression always
  attributes to the single `PassId` that introduced it.
- **Pragma boundary:** `PRAGMA disable-pass=<id>` is stripped in both
  `api::execute_query_with` and `api::plan_select`. The latter matters
  because the HTTP `/query` handler routes every request through
  `plan_select` first — without stripping there, any pragma-carrying query
  (all forms, not just SELECT) would 400 as a raw spargebra parse error
  before the materialized fallback could see it. Stripping happens inside
  the `Stage::Parse` timed envelope, so malformed pragmas count in
  `query_errors{stage=parse}` like any other parse failure. Pragmas come
  before `EXPLAIN`: `PRAGMA ... EXPLAIN SELECT ...`.
- **`standard_passes()` allocates + asserts ordering on every `plan` call.**
  Cheap today (one pass), but if `stage_duration_seconds{stage=plan}` ever
  regresses, hoist it into a `OnceLock`.
- The `PhysicalPlan`-level `plan/pushdown.rs` rewrite (runs inside
  `Runtime::run_stream`) is untouched; porting it onto the pass registry is
  Phase-2 territory (`projection-pushdown` / `join-planning` `PassId`s are
  reserved for it).

## Heuristic rewrite passes (SPEC-23 Phase 2, #185, 2026-07-19)

- Four `LogicalPass`es registered after `CoalesceBgp` in
  `plan::pass::standard_passes`, source order: `Normalize` →
  `FilterPullup` → `FilterPushdown` → `ProjectionPushdown`. Each declares
  `must_follow` and is disable-able via `PlanCtx.disabled_passes`
  (`PRAGMA disable-pass=<kebab-name>`).
- `Normalize` reduces `Eq → SameTerm` only where the type lattice proves
  both operands the same non-literal kind (IRI/blank), plus structural
  constant folding of variable-free filter conjuncts through boolean
  connectives (`And`/`Or`/`Not`). It never touches `?v = <const-literal>`;
  the physical count-pushdown equality inlining (`plan::pushdown::eq_conjuncts`)
  now matches `SameTerm` alongside `Eq`.
- New `Expr::SameTerm` node: structural term equality, identical to `Eq`
  today; diverges only if `Eq` gains value-equality (numeric promotion).
- `FilterPullup` hoists conjuncts above inner `Join`s only, gated on the
  conjunct's vars being provably bound in its own arm (unbound-var filter
  is error→false; hoisting could flip it). Residuals stay on their arm.
- `FilterPushdown` sinks conjuncts to the deepest binding subtree; never
  into a `LeftJoin`'s optional arm; never past a `Project`'s scope
  boundary (projected-away vars stay hidden); conjuncts landing on the
  same sink merge into one `Filter` (keeps the count-pushdown one-layer
  pattern match).
- `ProjectionPushdown` is the logical mirror of `plan::pushdown::prune`;
  both run in Phase 2. `lower_count_group` peels a restricting `Project`
  (under `Group` and under its `Filter`) when it retains every var the
  count path reads, so the COUNT fast path composes with the narrowing.
  Retiring the physical `prune` is deferred to Phase 4.
- Guard: `tests/rewrite_invariance.rs` — full pipeline vs each pass singly
  disabled; a regression bisects to one `PassId`.

Full rationale: `docs/specs/SPEC-23-unified-ir.md` §5.2 (pass registry), §6
phase 2 (heuristic rewrite passes), §7 (acceptance criteria).
