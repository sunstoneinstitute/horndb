---
status: executed
date: 2026-06-18
scope: "SPEC-07 graph-management SPARQL Update (#52)"
---

# Plan: SPEC-07 graph-management SPARQL Update (#52)

Epic #7 increment. Implement the graph-management Update verbs
`LOAD` / `CLEAR` / `DROP` / `CREATE` / `ADD` / `MOVE` / `COPY`, which are today
rejected as `UnsupportedForm` in `crates/sparql/src/update.rs`.

## Constraints discovered

- The SPARQL execution store is **default-graph-only** (single merged graph);
  there is no named-graph scoping in the `Store`/`Executor` seam
  (`crates/sparql/src/exec/mod.rs`). The storage tier *can* hold quads, but the
  SPARQL `Store` trait exposes only `insert_triple`/`delete_triple` over the
  default graph. Widening the seam to true named-graph scoping is a separate,
  larger effort (the named-graph epic / GSP increment #54). This increment
  documents and implements the default-graph-only Stage-1 semantics.
- The resolved parser is **spargebra 0.3.5**. It desugars `ADD`/`MOVE`/`COPY`
  into sequences of `Drop` + a `DeleteInsert` whose insert target / WHERE is a
  `GRAPH` pattern (`copy_graph`). So:
  - `ADD from TO to`  → `[DeleteInsert(copy from→to)]`
  - `COPY from TO to` → `[Drop{silent:true,to}, DeleteInsert(copy from→to)]`
  - `MOVE from TO to` → `[Drop{silent:true,to}, DeleteInsert(copy from→to), Drop{silent,from}]`
  Multi-operation updates are therefore **required** (the parser currently flags
  `operations.len() > 1` as `UnsupportedForm`).
- `Load { silent, source: NamedNode, destination: GraphName }`,
  `Clear { silent, graph: GraphTarget }`, `Create { silent, graph: NamedNode }`,
  `Drop { silent, graph: GraphTarget }`. `GraphTarget` ∈ {NamedNode, DefaultGraph,
  NamedGraphs, AllGraphs}.
- No HTTP client dependency in the workspace. `oxttl`/`oxrdfio` parse RDF.

## Default-graph-only semantics (Stage-1)

The single merged default graph is the only graph that exists. We map the verbs
onto it conservatively and **honour `SILENT`** uniformly: a `SILENT` op that
would touch an unrepresentable named graph succeeds as a no-op; the same op
non-silent is an error (so callers get a clear signal rather than silent data
loss). Specifically:

- `CLEAR DEFAULT` / `CLEAR ALL` / `DROP DEFAULT` / `DROP ALL` → clear the store.
- `CLEAR GRAPH <iri>` / `CLEAR NAMED` → no named graphs exist; **no-op success**
  (clearing an absent graph removes nothing; SPARQL says CLEAR of an empty/absent
  graph is not an error unless `SILENT` is absent *and* the graph does not exist —
  CLEAR's error is only for non-existent graphs, same rule as DROP). We treat a
  named/`NAMED` target as "graph does not exist": non-silent → error, silent →
  no-op. `DEFAULT`/`ALL` always clear.
- `DROP GRAPH <iri>` / `DROP NAMED` → graph does not exist: non-silent → error,
  silent → no-op. `DEFAULT`/`ALL` → clear the store.
- `CREATE GRAPH <iri>` → cannot represent a named graph: non-silent → error,
  silent → no-op. (Per SPARQL, CREATE of an existing graph errors unless SILENT;
  here no named graph can be created, so the unrepresentable-target rule applies.)
- `LOAD <source> [INTO GRAPH <g>]` → fetch + parse + insert into the default
  graph. `INTO GRAPH <named>` cannot target a named graph: non-silent → error,
  silent → no-op (skip load). `source` fetch: `file:` IRIs are read from disk and
  parsed (format by extension: `.nt`/`.nq`/`.trig`, default Turtle) via `oxttl`
  (the parser family already used by `serve.rs` — no new dependency); remote
  `http(s):` IRIs are **not** fetched (no HTTP client dependency) — non-silent →
  error, silent → no-op.
- `ADD/MOVE/COPY` (post-desugar): a `DeleteInsert` whose insert target is a
  `GRAPH <named>` or whose WHERE reads a named `GRAPH` is rejected by the existing
  named-graph guards in `apply_delete_insert`. The same-graph identity case
  (`… <g> TO <g>`, including `DEFAULT TO DEFAULT`) is rewritten by spargebra to
  **zero operations** per the W3C identity-case rewrite, so it is a valid no-op
  (`parse_update` admits an empty op list for this reason). Any named-graph
  operand is rejected (non-silent) — silent must still succeed as a no-op.

## Tasks

### Task 1 — `Store::clear_all` seam + impls (TDD)
- Add `fn clear_all(&mut self)` to the `Store` trait (`exec/mod.rs`), default-
  graph clear of all triples.
- `MemStore::clear_all` → reset all fields.
- `HornBackend::clear_all` → tombstone all currently-live keys (insertion-only
  storage; mirror the existing tombstone-delete path) and reset live count +
  invalidate snapshot. Verify count → 0 and a subsequent query returns nothing.
- Unit tests for both backends.

### Task 2 — Parser: admit graph-management + multi-op forms (TDD)
- Extend `ParsedUpdate` with a `GraphManagement { inner: Update }` variant (or
  reuse a general `Operations` variant) that carries the full op sequence.
- `parse_update`: classify an update whose ops are all in
  {InsertData, DeleteData, DeleteInsert, Load, Clear, Create, Drop} (any count)
  as executable; keep `UnsupportedForm` only for genuinely-unhandled shapes
  (none remain in 0.3.5, but keep the arm for forward-compat / a future variant).
  Preserve the existing single-op `InsertData`/`DeleteData`/`DeleteInsert`
  classification so current call sites are unchanged.
- Tests: each verb parses to the executable variant; `ADD/MOVE/COPY` desugar
  visible (op counts).

### Task 3 — Executor: implement the verbs (TDD)
- In `apply_update_with`, iterate the op sequence and handle `Load`, `Clear`,
  `Create`, `Drop` with the semantics above; `InsertData`/`DeleteData`/
  `DeleteInsert` arms stay as-is (the `DeleteInsert` named-graph guards already
  reject named operands, giving correct `ADD/MOVE/COPY` behaviour). Honour
  `SILENT` on every verb.
- LOAD: a small `fetch_and_parse(source) -> Result<Vec<(Term,Term,Term)>>` over
  `file:` IRIs using oxrdfio; route triples through `store.insert_triple`.
- Tests (MemStore + HornBackend where it makes sense):
  - CLEAR/DROP DEFAULT/ALL empties a seeded store.
  - CLEAR/DROP of a named graph: silent ok / non-silent errors.
  - CREATE named: silent ok / non-silent errors.
  - LOAD file:.nt into default graph inserts triples; LOAD INTO named errors
    (silent ok); LOAD http(s) errors (silent ok); LOAD missing file errors
    (silent ok).
  - ADD/COPY/MOVE DEFAULT TO DEFAULT behave (identity / clear+identity / clear src);
    named operand errors (silent ok).
  - Multi-op update applies in order.

### Task 4 — HTTP server + integration coverage
- A server test (`--features server`) exercising at least `CLEAR DEFAULT` and a
  `LOAD <file:...>` through `/update`.

### Task 5 — Docs sync (rides the PR; no TASKS.md on branch)
- `docs/architecture.md`: flip the "Full Update vocabulary (`LOAD`/`CLEAR`/
  `DROP`)" row from **planned** → **implemented**, note default-graph-only
  semantics + the file-only LOAD boundary; add ADD/MOVE/COPY desugaring note.
- `crates/sparql/INTEGRATION-NOTES.md`: document the graph-management semantics
  and the SILENT/no-op convention, the file-only LOAD boundary, and that true
  named-graph scoping stays deferred to #54.
- Update `docs/index.md` if a new plan doc is added (this file).

## Out of scope (deferred, documented)
- True named-graph scoping / a quad-aware SPARQL `Store` seam (→ GSP #54).
- Remote (`http(s):`) LOAD fetching (no HTTP client dep).
- W3C SPARQL 1.1 Update conformance suite wiring (→ harness #10).
