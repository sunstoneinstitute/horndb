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

## GRAPH patterns (Stage 1, #66)

`GRAPH <iri> { P }` and `GRAPH ?g { P }` lower transparently to `P`.
The Stage-1 executor holds a single merged graph (corpora are loaded
from flat triple dumps), so there is no named-graph store to scope
against; a graph-name variable remains unbound in results. This makes
the SPB named-graph queries (Q10/Q12) translate and run. Correct
named-graph scoping (zero solutions for absent graphs, `?g` binding
per named graph) is deferred to the named-graph epic (#7).

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

### Tombstone deletes over insertion-only storage

`horndb-storage` is insertion-only at Stage 1. `DELETE DATA` is
implemented by a `tombstones: HashSet<(u64, u64, u64)>` overlay in
`HornBackend`. The overlay is applied when building the WCOJ snapshot:
tombstoned triples are filtered out before the sorted `VecTripleSource`
is constructed. A `stored_keys` mirror of every physically written key
gives O(1) membership tests without re-scanning the storage columns.

### Lazily-rebuilt VecTripleSource snapshot

BGP execution requires all six sort orderings (SPO, SOP, PSO, POS,
OSP, OPS). `HornBackend` builds a `VecTripleSource` lazily on the first
query after any mutation and caches it behind a `Mutex<Option<Arc<…>>>`.
The snapshot holds all six orderings eagerly sorted; at ~144 bytes/triple
steady-state snapshot cost (construction briefly peaks ~168 B/triple
while the input vec is still alive) this is a documented Stage-1 cost.
The snapshot is invalidated (set to `None`) on every write (insert or delete).

A follow-up item exists to replace this with a direct `TripleSource`
over the columnar partitions, avoiding the full-copy rebuild.

### Batched-insert core (`insert_oxrdf_batch`)

Inserting triples one at a time via `Store::insert_triple` triggers a
per-predicate partition rebuild in `horndb-storage` on each call, giving
O(n²) cost for a bulk load. `insert_oxrdf_batch` addresses this with a
read-compute / write-commit split:

1. Phase 1 (read-only): intern all terms; classify each triple as
   new-to-storage or tombstone-resurrection; collect the storage batch.
   Intern failures skip the triple (lenient for bulk loads — the
   single-triple `insert_oxrdf` propagates intern errors instead).
2. Phase 2 (write): call `store.insert_triples` once for the whole
   batch, rebuilding each predicate partition at most once.
3. Phase 3: invalidate the WCOJ snapshot once iff any triple became
   newly live.

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

Named-graph patterns remain unscoped (unchanged Stage-1 behaviour).
See the GRAPH patterns section above.

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

## Graph-management Update verbs (#52)

`update.rs` implements `LOAD`/`CLEAR`/`DROP`/`CREATE` and (via spargebra
desugaring) `ADD`/`MOVE`/`COPY`, plus multi-operation update sequences. The
parser classifies a single data/pattern operation as before; everything else —
a graph-management verb or any `;`-joined sequence — becomes
`ParsedUpdate::GraphManagement`, and the executor walks the whole operation
list in order.

The execution store is **default-graph only** (one merged graph; see "GRAPH
patterns" above). The graph-management verbs therefore map onto that single
graph, with a uniform `SILENT` convention: an operation that would touch a
named graph the Stage-1 store cannot represent is an **error** when not silent
and a **no-op** when `SILENT`. Concretely:

- **`INSERT DATA`/`DELETE DATA`** with a `GRAPH <g> { … }` block are rejected
  (`require_default_graph_name`): the apply loop ignores `q.graph_name`, so a
  named block would silently mutate the default graph. (Multi-op support newly
  routes such quads through this path, so the check guards both single- and
  multi-op data updates.)
- **`CLEAR`/`DROP DEFAULT`/`ALL`** clear the store via the new
  `Store::clear_all` seam method. `MemStore::clear_all` resets its vector and
  indexes; `HornBackend::clear_all` tombstones every physically-written key
  (storage is insertion-only, so it mirrors the `delete_triple` tombstone path)
  and zeroes the live count. Re-inserting a cleared triple resurrects it via
  the existing tombstone-clearing insert path.
- **`CLEAR`/`DROP GRAPH <iri>` / `NAMED`** address a graph that does not exist:
  error unless `SILENT`.
- **`CREATE GRAPH <iri>`** cannot create a named graph: error unless `SILENT`.
- **`LOAD <source> [INTO GRAPH <g>]`** fetches and parses `source`, merging its
  triples into the default graph. Only `file:` sources are fetched — the
  workspace carries no HTTP client, so remote (`http(s):`) sources are an error
  unless `SILENT`. The `file:` authority is parsed (`file_iri_to_path`):
  `file:///abs`, `file://localhost/abs`, and `file:/abs` are local; a non-empty
  non-`localhost` authority is rejected. The path is percent-decoded before
  reading (so `file:///tmp/a%20b.nt` opens `/tmp/a b.nt`). The serialization is picked from
  the path extension (`.nt`/`.nq`/`.trig`, else Turtle) and parsed with `oxttl`
  (the same parser family `serve.rs` uses); all graph names in a quad source
  merge into the default graph. A named `INTO GRAPH` destination is an error
  unless `SILENT`. **Blank-node labels are carried through verbatim** — the same
  Stage-1 approximation the bulk loaders use: labels are not freshened per
  loaded document, so re-loading an identical blank-node triple dedups.
  Per-document blank-node scoping belongs with the SPEC-02 dictionary store.
- **`ADD`/`MOVE`/`COPY`** are not distinct spargebra variants; the parser
  rewrites them per the W3C spec into `Drop` + a `DeleteInsert` whose insert
  target / WHERE is a `GRAPH` pattern. Named-graph operands are therefore
  rejected by the existing `apply_delete_insert` named-graph guards. The
  same-graph identity case (`… <g> TO <g>`) is rewritten to **zero
  operations**, a valid no-op (`parse_update` admits an empty op list for this
  reason). One spargebra-imposed limitation: the rewrite **drops the `SILENT`
  flag**, so a named-operand `ADD`/`MOVE`/`COPY` errors even with `SILENT`
  rather than swallowing the error. This is observationally identical to the
  prescribed no-op for a default-graph-only store (no data can move either
  way); preserving `SILENT` here would require re-parsing the verb and is
  out of scope while named graphs are unrepresentable.

**Atomicity.** A multi-operation update must not partially apply on failure
(SPARQL 1.1 §3.1.3). This matters here because spargebra desugars
`COPY`/`MOVE <named> TO DEFAULT` into a destructive `Drop{DEFAULT}` *followed
by* a `DeleteInsert` reading the unrepresentable named graph — applying op-by-op
would clear the default graph and only then reject. `apply_update_with` runs a
`validate_op` preflight over the whole sequence first: it mirrors every apply-
time rejection (structural named-graph/triple-term checks, a non-silent `LOAD`
fetch+parse, and the WHERE-clause `translate_where`+`planner::plan` so an
unsupported algebra construct like `SERVICE`/`MINUS` is caught), and only mutates
once the whole sequence is known-applyable.

**Turtle/TriG base IRI.** `LOAD` passes the source IRI as the parser base, so a
document with relative IRIs (`<s> <p> <o> .`) resolves against its own IRI —
matching the storage Turtle loader. N-Triples/N-Quads need no base.

**Deferred** (documented, out of scope here): true named-graph scoping and a
quad-aware `Store` seam belong with the Graph Store Protocol increment (#54);
remote `LOAD` waits on an HTTP client decision; and the W3C SPARQL 1.1 Update
conformance suite is wired by the harness epic (#10). Coverage for this
increment lives in `tests/update_graph_mgmt.rs` (both backends) and the
`/update` server tests in `tests/server_http.rs`.

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
- **Post-pass debug validation is differential, not absolute.** Legal SPARQL
  may reference variables its pattern never binds (`FILTER(?z = <iri>)` with
  unbound `?z` drops rows; `SELECT ?z` projects it unbound), so
  `pass::dangling_refs` is compared before/after each pass — a pass may not
  *introduce* new dangling refs, but parser-supplied ones survive.
- **Pragma boundary:** `PRAGMA disable-pass=<id>` is stripped in
  `api::execute_query_with` only. The streaming SELECT path (`plan_select`,
  used by the HTTP `/query` streaming handler) does not accept pragmas yet —
  a small, self-contained follow-up if pragma-driven bisection is ever
  needed over HTTP streaming.
- **`standard_passes()` allocates + asserts ordering on every `plan` call.**
  Cheap today (one pass), but if `stage_duration_seconds{stage=plan}` ever
  regresses, hoist it into a `OnceLock`.
- The `PhysicalPlan`-level `plan/pushdown.rs` rewrite (runs inside
  `Runtime::run_stream`) is untouched; porting it onto the pass registry is
  Phase-2 territory (`projection-pushdown` / `join-planning` `PassId`s are
  reserved for it).
