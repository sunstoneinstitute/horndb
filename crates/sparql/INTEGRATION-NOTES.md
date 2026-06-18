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
