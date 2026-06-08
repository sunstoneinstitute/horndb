# SPEC-07 — Wire BGP evaluation onto `horndb-wcoj` (issue #67)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended)
> or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`)
> syntax for tracking. **Do not start coding before this plan is reviewed.**

**Goal.** Replace the SPARQL frontend's standalone index-nested-loop BGP executor (`MemStore`) with a
backend that evaluates basic graph patterns on the `horndb-wcoj` worst-case-optimal join engine. This
closes the architectural gap called out in [#67](https://github.com/sunstoneinstitute/horndb/issues/67):
HornDB ships a WCOJ engine (SPEC-03, the differentiator) and the query path does not use it. The
measured symptom is SPB aggregation queries at 0.5–7 s each (~0.9 q/s aggregate); the target is
~1–10 ms per query.

**Non-goal.** This plan does **not** fix RDF term-kind/datatype fidelity, add `GRAPH`/named graphs,
string `FILTER` functions, or property paths — those are #66. It does **not** change anything in the
`Runtime` layer (LeftJoin/Union/Group/OrderBy/Filter/Distinct/Slice) or the planner/algebra above the
BGP seam. It swaps exactly one trait implementation pair (`Executor` + `Store`) and the server's
backing-store type.

---

## 1. The seam (verified against live code)

The query path is `server/query.rs` → `api::execute_query_with` → `Runtime::new(exec).run(&plan)` →
`Executor::scan_bgp`. The pluggable seam is two traits in `crates/sparql/src/exec/mod.rs`:

```rust
pub trait Executor {
    fn scan_bgp(&self, patterns: &[TriplePattern])
        -> Result<Box<dyn Iterator<Item = Bindings> + '_>>;        // exec/mod.rs:65
}
pub trait Store {
    fn insert_triple(&mut self, subject: Term, predicate: Term, object: Term);   // exec/mod.rs:78
    fn delete_triple(&mut self, subject: &Term, predicate: &Term, object: &Term);
}
pub trait FullBackend: Executor + Store {}   // blanket impl, exec/mod.rs:83
```

Everything above BGP already materialises into `Vec<Bindings>` in `Runtime::eval`
(`exec/runtime.rs:26-146`), so `scan_bgp` returning an eagerly-materialised iterator costs nothing —
**we do not need streaming** and can sidestep all self-borrow lifetime puzzles by collecting inside
`scan_bgp`.

`api::execute_query_with<E: Executor>` and `execute_update<S: Store>` are already generic
(`api.rs:35,86`), so the new backend drops in without touching the API layer. Only `AppState`
(`server/mod.rs:19`, concrete `Arc<RwLock<MemStore>>`) and the `serve` binary
(`bin/serve.rs:44-61`) name `MemStore` concretely and must change.

### Types the seam speaks (lexical, `crates/sparql/src/algebra/mod.rs`)

```rust
pub enum Term { Var(Var), Iri(String), BlankNode(String), Literal(String), Triple(Box<TriplePattern>) }  // :33
pub struct Var(Arc<str>);                                          // :15
pub struct TriplePattern { subject: Term, predicate: Term, object: Term }  // :48
pub struct Bindings { inner: BTreeMap<String, Term> }              // exec/mod.rs:18
```

`Literal(String)` is the **N-Triples lexical form** (`"hello"`, `"3"^^<…>`, `"x"@en`). Term kind is
recovered from the leading `"` by `classify_lexical` (`exec/mod.rs:109`). **MemStore joins on exact
lexical-string equality** — it stores `Vec<(String,String,String)>` and indexes by string
(`exec/mem.rs:22`). This fact is the keystone of the design below.

### What `horndb-wcoj` exposes (verified, `crates/wcoj/src/`)

```rust
pub type TermId = u64;                                             // ids.rs:3
pub struct Triple { s: TermId, p: TermId, o: TermId }             // ids.rs:6
pub enum Term { Bound(TermId), Var(Var) }   pub struct Var(pub u8); // pattern.rs:5,9
pub struct TriplePattern { s: Term, p: Term, o: Term }            // pattern.rs:30
pub struct Bgp { patterns: Vec<TriplePattern> }  // .variables() -> Vec<Var>   // pattern.rs:159
pub struct VecTripleSource; impl VecTripleSource { fn from_triples(Vec<Triple>) -> Self }  // source/vec_source.rs:12
pub struct Planner { wcoj_cutover: usize }  // Default = 4         // planner.rs:12
pub enum Executor<'src,S: TripleSource> { … }                     // executor/mod.rs:23
//   Executor::for_bgp(&src, &bgp, &Planner, CancelToken) -> Self  // and is an Iterator<Item=Result<RecordBatch>>
```

A result row is an Arrow `RecordBatch`: one `UInt64` column per variable in `plan.var_order`, columns
named `v{n}`, value extracted via `col.as_any().downcast_ref::<UInt64Array>().unwrap().value(r)`
(pattern from `crates/wcoj/tests/wcoj_smoke.rs`). The top-level `Executor` enum runs the **planner's
choice**: `wcoj_cutover = 4` ⇒ BGPs with ≤3 patterns use the binary-hash path, ≥4 use leapfrog WCOJ.
`VecTripleSource::from_triples` builds **all six orderings eagerly**; `wcoj` has **no dependency on
`horndb-closure`/GraphBLAS** (`wcoj/Cargo.toml` is `arrow + anyhow + thiserror` only — confirmed).

### Stale handoff assumptions, now corrected by the code

- WCOJ over-production on repeated patterns: **fixed** (`TASKS.md:31` `[x]`). Not a risk.
- 4-cycle ≥10× acceptance gate: **fixed** (`TASKS.md:34` `[x]`, #1 closed).
- `differential_fuzz.rs` no longer carries `#[ignore]` on its proptest body (CLAUDE.md's "currently
  red" note is stale). The WCOJ join is routable as-is.

---

## 2. Key design decision: a **lexical-local** dictionary, not `storage::Dictionary`

The handoff floats reusing `crates/storage/src/dictionary.rs` (keyed on `oxrdf::Term`, kind/datatype
preserving). **This plan recommends against it for #67**, and instead builds a tiny lexical
`String ↔ u64` dictionary inside the new backend. Rationale:

1. **Behavioural equivalence is the safety net.** MemStore joins on exact lexical-string equality. A
   `HashMap<String,u64>` + `Vec<String>` dictionary assigns one id per distinct lexical string, so the
   WCOJ backend joins on *exactly the same equivalence* as MemStore — same answers, just a faster join
   engine. That gives us a clean differential oracle (Task 6) and **zero semantic regression**.
2. **`storage::Dictionary` keys on `oxrdf::Term`,** so using it forces a lexical-string ↔ `oxrdf::Term`
   parse at every intern/lookup boundary (parsing N-Triples literal syntax to recover datatype/lang).
   That is real work, it pulls a new cross-crate dependency into `horndb-sparql`, and — critically — it
   would *change* join semantics (canonicalising `"1"^^xsd:integer` vs `"01"^^…`), which is a
   correctness change masquerading as a wiring change. Term-kind/datatype fidelity is an explicit
   **#66** concern, not #67.
3. **"Avoid perfect over done."** The measured win in #67 is the join engine, not the dictionary. Ship
   the join engine; leave fidelity to the typed-term work that will swap this dictionary for
   `storage::Dictionary` wholesale once the whole exec layer carries typed terms.

> Decision to confirm at review: lexical-local dictionary now, `storage::Dictionary` later. If the
> reviewer wants fidelity in this PR, that is a larger change and should be re-scoped onto #66.

---

## 3. Backend shape

New module `crates/sparql/src/exec/wcoj.rs` (no new workspace deps beyond `horndb-wcoj`, already a
clean downward edge `wcoj → … → sparql`):

```rust
pub struct WcojBackend {
    dict: LexDict,                 // String <-> u64, kind-agnostic (lexical equality)
    triples: Vec<wcoj::Triple>,    // canonical id-encoded triples (the write log)
    seen: HashSet<wcoj::Triple>,   // dedup on insert, mirrors MemStore.seen
    source: OnceCell<VecTripleSource>,   // derived; built lazily, invalidated on write
}
```

- **`LexDict`**: `intern(&str) -> u64` (insert-on-miss) and `get(&str) -> Option<u64>` (lookup-only,
  for query constants) and `resolve(u64) -> &str`. Ids start at 1 (0 reserved/unused). Pure, ~40 lines.
- **`source`**: rebuilt from `triples` on first read after a write. `VecTripleSource::from_triples` is
  O(n log n) × 6 orderings; acceptable because SPB aggregation is read-only and MemStore already
  rebuilds wholesale on delete (`exec/mem.rs:185`). Note rebuild cost as a follow-up (Task 8).

### `scan_bgp` algorithm

1. **Empty BGP** (`patterns.is_empty()`): return one empty `Bindings` (join identity). Match MemStore.
2. **Var numbering**: walk patterns, assign each distinct `Var` name a `wcoj::Var(u8)` (error/clamp if
   >256 distinct vars — realistically ≤ a handful). Keep `Vec<(u8, String)>` inverse map.
3. **Translate** each `algebra::TriplePattern` → `wcoj::TriplePattern`:
   - `Term::Var(v)` → `wcoj::Term::Var(num[v])`.
   - constant (`Iri`/`Literal`/`BlankNode`) → `dict.get(lexical)`; **if `None`, the constant is absent
     from the data ⇒ the whole BGP yields zero rows — short-circuit to an empty iterator.** (Use
     `lex_of` on the algebra term to get its lexical string; `Term::Triple` is rejected here — RDF 1.2
     triple-term *data* patterns are out of scope, return `UnsupportedAlgebra` as today.)
4. **Build & run**: `Bgp::new(patterns)`, `Planner::default()`,
   `Executor::for_bgp(self.source(), &bgp, &planner, CancelToken::new())`. Collect all
   `Result<RecordBatch>`.
5. **Decode**: `plan.var_order` is `bgp.variables()` (column order). For each batch row, for each
   column `i`, read the `u64`, `dict.resolve` it to the lexical string, `classify_lexical` it back to a
   `Term`, and `bindings.set(var_name(var_order[i]), term)`. Push into a `Vec<Bindings>`.
6. Return `Ok(Box::new(rows.into_iter()))`.

> The planner picks binary-hash for ≤3 patterns and WCOJ for ≥4 automatically — SPB's 2-pattern joins
> ride the binary-hash path, cyclic shapes ride WCOJ. We do not second-guess `wcoj_cutover`.

### `Store` (write seam)

- `insert_triple(s,p,o)`: `dict.intern` each lexical form → `wcoj::Triple`; if not in `seen`, push to
  `triples`, insert into `seen`, and invalidate `source` (`OnceCell::take`).
- `delete_triple(&s,&p,&o)`: resolve via `dict.get`; if all three present, remove the matching
  `wcoj::Triple` from `triples`/`seen` and invalidate `source`. (Dictionary entries are not GC'd —
  matches MemStore, which never shrinks its term space.)
- `lex_of(&algebra::Term) -> Option<String>`: reuse/mirror the existing lexical projection used by
  `MemStore` (`bound_lex`/`lex_of_bound`, `exec/mod.rs:88`) so insert and query agree on the string
  form byte-for-byte. **This must be identical to MemStore's** or the differential test (Task 6) will
  (correctly) fail.

---

## 4. Tasks

### Task 1 — `LexDict` + module scaffold
- [ ] Add `horndb-wcoj = { workspace = true }` to `crates/sparql/Cargo.toml` (add to
      `[workspace.dependencies]` first if absent). Confirm it does **not** transitively pull GraphBLAS.
- [ ] Create `crates/sparql/src/exec/wcoj.rs`; `mod wcoj;` in `exec/mod.rs`.
- [ ] Implement `LexDict` (`intern`/`get`/`resolve`, ids from 1) with unit tests for round-trip and
      lookup-miss.

### Task 2 — translation helpers
- [ ] `lex_of(&algebra::Term) -> Option<String>` matching MemStore's lexical projection exactly; unit
      test that, for IRI/literal/bnode constants, it equals what `MemStore` stores.
- [ ] Var-numbering pass: `&[TriplePattern] -> (Vec<wcoj::TriplePattern-with-vars>, inverse map)`,
      with the constant-absent short-circuit signalled distinctly from a translation error.

### Task 3 — `Executor::scan_bgp`
- [ ] Lazy `source()` builder over `OnceCell<VecTripleSource>` from `self.triples`.
- [ ] Implement `scan_bgp` per §3 (empty-BGP identity; absent-constant ⇒ empty; build/run/decode).
- [ ] Decode `RecordBatch` columns → `Bindings` via `dict.resolve` + `classify_lexical`.

### Task 4 — `Store` writes
- [ ] `insert_triple`/`delete_triple` with `seen`-dedup and `source` invalidation.
- [ ] Constructor(s): `WcojBackend::default()` and a bulk loader
      `from_lex_triples(impl Iterator<Item=(String,String,String)>)` for the serve binary's load path.

### Task 5 — server + serve binary wiring
- [ ] Change `AppState.store` (`server/mod.rs:19`) to `Arc<RwLock<WcojBackend>>`. Update `query.rs`,
      `update.rs` (they only call `execute_query_with`/`execute_update`, which are generic — should be a
      type-name change only).
- [ ] `bin/serve.rs:44-61`: construct `WcojBackend` and feed it the same lexical triples it currently
      feeds `MemStore` (the `lex_triple` path at `:133` is unchanged).
- [ ] `cargo build -p horndb-sparql --bin serve --features server --release` is green.

### Task 6 — differential correctness test (the gate)
- [ ] `crates/sparql/tests/wcoj_vs_mem_differential.rs`: proptest generating a small random triple set
      and a random BGP (constants drawn from the data + a few absent ones; 0–4 patterns; shared vars to
      force joins, including a **repeated-pattern** case). Assert
      `WcojBackend.scan_bgp(bgp)` and `MemStore.scan_bgp(bgp)` produce the **same multiset** of
      `Bindings`. 256 cases. This is the acceptance gate for the wiring.
- [ ] Port/duplicate the existing `MemStore` unit tests (`exec/mem.rs` `#[cfg(test)]`) to also run
      against `WcojBackend` so both backends share one behavioural spec.

### Task 7 — benchmark validation (the point of the issue)
- [ ] Reproduce the SPB aggregation A/B per handoff §8 (serve + driver in one shell command; driver in
      foreground; no bare `wait`; `kill $SERVEPID`). Record before/after `aggregation-qps` in the
      harness DB and per-query latencies.
- [ ] Update `BENCHMARKS.md` SPARQL row. **RDFox numbers stay out of committed files (DeWitt clause).**

### Task 8 — docs + issue bookkeeping (same commit as the code)
- [ ] `docs/architecture.md` §9: flip the SPEC-07 "runtime executes against `MemStore`" row to
      "BGP eval on `horndb-wcoj`".
- [ ] `TASKS.md`: check off / re-scope the #67 line; mirror to GitHub (`gh issue close 67` on merge,
      keep the link). Note residual follow-ups (below) as their own tasks/issues if kept.

---

## 5. Open questions resolved (vs handoff §5)

- **Dictionary ownership & kind fidelity** → lexical-local dictionary; fidelity explicitly deferred to
  #66 (§2). Behavioural equivalence to MemStore is the design invariant.
- **Trie orderings** → all six, built eagerly by `VecTripleSource::from_triples`, lazily on first read
  after a write. No on-demand machinery needed for #67.
- **Join/variable ordering** → delegated to `wcoj::Planner` + the `Executor` enum (binary-hash ≤3
  patterns, WCOJ ≥4). No custom cardinality work in #67; revisit only if Task 7 shows a bad SPB plan.
- **Writes** → rebuild `VecTripleSource` on mutation (read-heavy workload; matches MemStore's
  rebuild-on-delete). Incremental source is a follow-up.
- **MemStore fate** → **kept**, as the differential oracle and for its existing unit tests. Not removed.
- **Object-only-bound pattern** → handled natively by the `Osp`/`Ops` orderings; the differential test
  covers it.

## 6. Risks

- **Lexical projection drift.** If `lex_of` (insert/query) and MemStore's projection disagree by even a
  byte, joins silently miss. Mitigated by Task 2's equality test against MemStore + Task 6 differential.
- **`Var(u8)` ceiling.** BGPs with >256 distinct variables overflow `wcoj::Var`. Realistically
  impossible for hand-written SPARQL; clamp/error explicitly with a typed error rather than panicking.
- **Rebuild-on-write latency** for `/update`-heavy workloads. Acceptable for #67 (read benchmark);
  flagged as follow-up, not a blocker.
- **Plan quality on real SPB shapes.** `UniformEstimator` is crude; if Task 7 shows a pathological plan,
  the fix is a better cardinality estimate (separate task), not a change to this wiring.

## 7. Follow-ups (out of scope; capture as TASKS/issues if pursued)

- Typed-term exec layer + swap `LexDict` → `storage::Dictionary` for datatype/lang fidelity (#66).
- Incremental `TripleSource` (avoid full rebuild on insert).
- `CompressedTripleSource` (`crates/wcoj/src/source/compressed.rs`) for memory footprint once the full
  18M-triple corpus is in play.
- Streaming `scan_bgp`/`Runtime` (currently everything materialises).

## 8. Acceptance

1. `cargo test -p horndb-sparql --features server` green, including the new differential test.
2. `cargo clippy --workspace --all-targets -- -D warnings` and `cargo build --workspace` green.
3. The `serve` binary answers SPARQL over the WCOJ backend (Task 5).
4. Task 7 shows SPB aggregation per-query latency dropping from ~0.5–7 s toward ~1–10 ms and aggregate
   q/s rising by orders of magnitude (exact target per `BENCHMARKS.md`).
