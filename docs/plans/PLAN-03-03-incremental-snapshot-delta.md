---
status: executed
date: 2026-08-20
scope: "HDB-82 — incremental delta maintenance for the cached WCOJ snapshot, so a small SPARQL Update costs O(delta) merge work instead of a full six-ordering rebuild of the whole store"
---

# Incremental snapshot delta — retire the rebuild-on-every-mutation cost

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task.

**Goal:** A small `INSERT`/`DELETE` stops paying for a whole-store re-index.
Today any mutation clears the cached `VecTripleSource`, and the next query
rebuilds all six sorted orderings from scratch.

## Why (measured, not assumed)

trainmarks q6 (`DELETE`/`INSERT … WHERE`) at the `large` scale (1,001,000
triples), Apple M4, release build, this branch's base commit:

| phase | time | share |
|---|---|---|
| q6 total (best of 3) | 0.3494 s | 100% |
| snapshot rebuild — store scan | 0.019 s | 5% |
| snapshot rebuild — **six sort passes** | **0.308 s** | **88%** |
| everything else (WHERE eval + apply) | ~0.022 s | 6% |

The rebuild is **94%** of q6, and the sorts are 16× the scan. The WHERE binds
only `?product a :Product` — 2,000 rows — so the cost tracks store size, not
delta size. Against the published upstream run
(<https://datatreehouse.github.io/trainmarks/>) we are worst-in-class at every
scale: 11.52 s at xlarge vs maplib 0.020 s (~575×), while every other engine
stays roughly flat across scales.

The driver runs q6 cold + 3 warm, and every run mutates, so every run pays a
full rebuild.

## Approach

Keep the cached snapshot and **merge the delta into it** instead of dropping
it. For a delta of `k` rows against a base of `n`, each ordering becomes one
O(n + k) linear merge rather than an O(n log n) comparison sort — and the store
rescan disappears entirely.

This is option 1 ("incremental index maintenance") from the HDB-82 brief. Option
2 (delta overlay merged at read time) was rejected: it changes the WCOJ hot read
path and risks regressing q1–q5, which scale acceptably today. Option 3
(narrowed invalidation) does not help — a delta disturbs every ordering.

**Fall back to the existing full rebuild whenever the fast path is not provably
correct or not profitable.** Correctness never depends on the fast path being
taken; a fallback is always a legal outcome.

## Global Constraints

- **Correctness over speed, always.** Any case the delta path cannot prove
  correct must fall back to `invalidate()`. A wrong row is infinitely worse than
  a slow query.
- **The sorted+deduped invariant is load-bearing.** After `apply_delta`, every
  ordering must be sorted ascending and free of duplicate rows — exactly the
  state `from_triples` leaves. WCOJ leapfrog correctness depends on it.
- **All three columns of an ordering always have equal length.** `TripleColumns`
  debug-asserts this in `view()`.
- **Set semantics.** The same triple present in two graphs is ONE row of a union
  snapshot (SPEC-28 S3). A delete from one graph must not remove the union row
  while another in-scope graph still holds the triple.
- **No new public dependency on `unsafe`.** Safe Rust only.
- Rust 1.90.0, `edition = 2021`. `cargo fmt --all` clean and
  `cargo clippy --workspace --all-targets -- -D warnings` clean.
- Run tests with `cargo nextest run`, not `cargo test`.
- Do not edit `TASKS.md` on this branch (it is lock-serialized on `main` — see
  the feature-branch exception in the root `CLAUDE.md`).

---

## Task 1 — `VecTripleSource::apply_delta`

**File:** `crates/wcoj/src/source/vec_source.rs`

Add one public method to `VecTripleSource`:

```rust
/// Apply a delta to every ordering in place, preserving the sorted+deduped
/// invariant. `dels` are removed if present, `adds` inserted if absent;
/// both are treated as sets. Cost is O(n + k log k) per ordering, against
/// `from_triples`'s O(n log n) — the point of the method.
pub fn apply_delta(&mut self, dels: &[Triple], adds: &[Triple])
```

Semantics, exactly:

- A `del` not present is a no-op (not an error).
- An `add` already present is a no-op — no duplicate row.
- A triple appearing in BOTH `dels` and `adds` ends up **present** (delete
  applies before insert, matching SPARQL 1.1 §3.1.3 and the `apply_quads`
  batch contract).
- Duplicates within `dels`, or within `adds`, are tolerated.
- Empty `dels` and empty `adds` → no work, no change.

Per ordering, the algorithm is one pass:

1. Project `dels` and `adds` through `Triple::by_ordering(ord)`, sort each with
   `sort_unstable()`, `dedup()`.
2. Remove from `adds_sorted` anything the base already contains, and remove from
   `dels_sorted` anything the base does not contain — or handle both inline in
   the merge; either is fine as long as the output invariant holds.
3. Walk the base columns and the two sorted delta lists together, writing the
   merged result into three fresh `Vec<TermId>` sized
   `base.len() + adds_sorted.len()`. Skip base rows matching a `del` (unless the
   same row is also in `adds`). Emit `add` rows at their sorted position, never
   emitting a row equal to the previous emitted row.
4. Replace the ordering's `levels`.

Building fresh column vectors and swapping them in is acceptable and preferred
over in-place shifting — it is simpler to prove correct and still O(n + k).

**`total`:** `from_triples` currently sets `total = triples.len()` BEFORE the
dedup, so it over-counts under a multi-graph union (a documented quirk — see the
`union_triples` doc comment in `crates/sparql/src/exec/horn.rs`). Do not change
`from_triples`. After `apply_delta`, set `total` to the post-merge row count of
`Ordering::Spo` (the deduped truth). Document this on the method in one
sentence: after a delta, `total` is exact rather than an over-count. Note it is
only read by `total_triples()`.

### Tests (same file's `mod tests`, or a new integration test in `crates/wcoj/tests/`)

The load-bearing test is **differential against the existing builder** — for a
given base and delta, `apply_delta` must produce byte-identical columns to
`from_triples(expected_set)`:

- `apply_delta_matches_full_rebuild` — a randomized differential test. Use a
  fixed-seed deterministic PRNG (a small hand-rolled LCG/xorshift is fine; do
  NOT add a `rand` dependency). Over ~200 iterations: build a random base of
  0–200 triples from a small term-id domain (so collisions and duplicates
  actually occur), pick random `dels` (biased so ~half are present in the base)
  and random `adds` (biased so ~half already exist), apply the delta, and assert
  every one of the six orderings' three columns equals those of
  `VecTripleSource::from_triples` over the expected set computed independently
  with a `HashSet`. Assert `total` matches the expected set's size.
- `apply_delta_empty_is_noop` — empty/empty leaves all six orderings unchanged.
- `apply_delta_delete_absent_and_insert_present_are_noops`.
- `apply_delta_same_triple_deleted_and_added_stays_present`.
- `apply_delta_to_empty_base_matches_from_triples`.
- `apply_delta_removing_everything_leaves_all_orderings_empty` — and the source
  is still usable (no panic on iteration; `total_triples() == 0`).

Assert on all six orderings, not just SPO — an ordering-specific bug is exactly
what this test exists to catch.

**Do not touch `crates/sparql/` in this task.**

---

## Task 2 — Wire the delta into `HornBackend`

**File:** `crates/sparql/src/exec/horn.rs`

Replace the unconditional `self.invalidate()` in `apply_quads` (the site guarded
by `if report.retracted > 0 || report.inserted > 0`) with a delta-aware update
that keeps cached snapshots alive when it can prove the result correct.

Add a private method roughly:

```rust
/// Push a committed quad delta into every memoised snapshot, falling back to
/// a full `invalidate()` for any scope the delta cannot be applied to safely
/// or profitably.
fn apply_delta_to_snapshots(
    &mut self,
    del_rows: &[(GraphId, OxTerm, OxTerm, OxTerm)],
    add_rows: &[(GraphId, OxTerm, OxTerm, OxTerm)],
)
```

Call it in place of `invalidate()` at the `apply_quads` site only. **Leave the
other three `invalidate()` call sites alone** — `insert_oxrdf_in_graph`,
`insert_oxrdf_batch` (bulk load, where a full rebuild is the cheaper option),
and `clear_graph` (mass delete). Scope discipline matters more than squeezing
those.

### Fall back to `invalidate()` — the whole cache — when ANY of these holds

1. **The delta is large relative to the base.** If
   `dels.len() + adds.len() > base_rows / 2` for a snapshot, a full rebuild is
   competitive and simpler; bail. Pick the threshold as a named `const` with a
   one-line comment, not a bare literal.
2. **`Arc::get_mut` fails** — another reader still holds the snapshot. Correct
   and rare; just invalidate.
3. **A scope whose membership the delta could change.** For `DefaultUnion`, an
   add whose `GraphId` is not already in the snapshot's graph set changes which
   graphs the union covers. Bail.
4. **Multi-graph union ambiguity.** For `DefaultUnion` / `FromUnion` covering
   more than one non-reserved graph, a delete from one graph must not remove the
   union row while another graph still holds the triple. Bail unless the scope
   resolves to exactly one graph. (The overwhelmingly common shape — trainmarks,
   LUBM, every default-graph-only workload — is a single graph, so the fast path
   still fires where it matters.)

Only `DefaultUnion` and `DefaultStrict` are memoisable (`SnapshotScope::
memoisable`), so those are the only two cases to handle. `OneGraph` and
`FromUnion` are never cached — do not add caching for them here.

### Correctness detail that WILL bite — read this

`stats_cache` holds `(Arc<VecTripleSource>, Arc<SnapshotStats>)` and validates
itself by `Arc::ptr_eq` against the current snapshot. Its doc comment says "any
write rebuilds the snapshot into a fresh `Arc`, so a stale entry never passes
the identity check" — **mutating through `Arc::get_mut` keeps the same pointer
and breaks that assumption.** You MUST clear `stats_cache` unconditionally on
every delta, and update that doc comment to say why the invariant now holds by
explicit clearing rather than by pointer identity. Missing this ships stale
cardinality estimates to `EXPLAIN`.

Convert `(GraphId, OxTerm, OxTerm, OxTerm)` rows to `WTriple` for the delta via
the same `TermId` path the surrounding code already uses (see `lookup_key` /
`intern_key` and `graph_triples`); a del row whose terms are not in the
dictionary cannot be present in any snapshot, so it can be dropped from the
delta.

### Tests (`crates/sparql/tests/`, or `horn.rs`'s `mod tests` where the existing
neighbours live)

Behaviour must be identical to a full rebuild. The strongest test is
equivalence:

- `update_then_query_matches_fresh_backend` — build a backend, run a pattern
  update, and assert every query result equals that of a second backend built
  directly from the post-update triple set. Cover: `INSERT DATA`,
  `DELETE DATA`, `DELETE`/`INSERT … WHERE`, and a delete of a triple that is
  absent.
- A test that a mutation is visible to the very next query (the delta actually
  lands — a no-op `apply_delta` would pass a weaker test).
- A test covering the multi-graph union fallback: the same triple in two named
  graphs, delete it from one, and assert the union default graph still returns
  it.
- A test that `EXPLAIN`'s cardinality estimate is not stale after a mutation
  (guards the `stats_cache` hazard above).
- Assert the snapshot cache is actually retained across a small update — use the
  existing test-only cache-size window (`graph_scoped_snapshots_are_not_memoised`
  uses it) so the fast path is proven to fire and does not silently rot into
  always-fallback.

The whole existing suite must stay green:
`cargo nextest run -p horndb-sparql --features server` and
`cargo nextest run -p horndb-wcoj`.

---

## Task 3 — Measure and document

1. Re-run trainmarks at `medium` and `large` locally and record before/after q6.
   The generator and queries are vendored at `scripts/bench/trainmarks/`; the
   driver is `crates/bench-trainmarks`. Data generation for one scale can be
   driven by importing `generate_data.py` and calling `generate_triples` +
   `write_turtle` / `write_ntriples` directly (the `__main__` block writes all
   three scales, ~1.7 GB, which is not needed here).
2. Confirm no regression on q1–q5 or the I/O rows.
3. Update `docs/architecture.md`'s trainmarks row with the new behaviour (the
   snapshot is now delta-maintained; a small update no longer re-indexes the
   store). Keep it to the house style — short and precise, no debugging diary.
4. Note in `crates/wcoj/INTEGRATION-NOTES.md` (and the `VecTripleSource` module
   doc, which currently says all six orderings are "materialised eagerly"
   and rebuilt after any mutation) that the source now supports in-place delta
   maintenance.

**Do not** record numbers in `docs/benchmarks.md` from this laptop — that file
takes hornbench numbers only (root `CLAUDE.md`). State the local before/after in
the task report instead, and flag that a hornbench run is the follow-up.
