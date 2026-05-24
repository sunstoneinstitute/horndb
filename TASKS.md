# Follow-up Tasks

Items deferred from the Stage-1 implementation pass (2026-05-24). Ordered
by priority within each category. Correctness gaps come first because they
make features unsafe to use; performance gaps next because they affect what
the system is usable for; everything else last.

When a task is picked up, move it to its own commit / PR and check it off
here in the same commit.

## CRITICAL — Correctness gaps

- [ ] **SPEC-03 WCOJ over-produces on BGPs with repeated patterns.**
  - The differential fuzzer in `crates/wcoj/tests/differential_fuzz.rs`
    (currently `#[ignore]`'d) finds inputs where the WCOJ executor returns
    more bindings than the binary-hash reference. The minimal failing input
    is saved in `crates/wcoj/tests/differential_fuzz.proptest-regressions`
    (e.g. two copies of `(?a, p, ?b)` plus `(0, p, ?b)`).
  - Diagnosis from the implementation agent: the `carry_at` refresh path in
    `crates/wcoj/src/executor/wcoj.rs` does not handle two iters at the
    same depth with identical patterns — the leapfrog's seek-past-match
    advancement on one iter is not reflected when the sibling iter is
    refreshed.
  - Acceptance criterion #3 in `specs/SPEC-03-query-engine.md` cannot be
    closed until this passes. Remove the `#[ignore]` and the regression
    file when fixed.

## HIGH — Lint cleanup (CI gate)

- [ ] **Workspace-wide `cargo clippy -- -D warnings` is red.**
  - The `.pre-commit-config.yaml` `pre-push` hook will block pushes once
    these are fixed, but at the moment a fresh clone fails clippy. Known
    categories (from a sweep on `cde4b99`):
    - `uninlined_format_args` (multiple crates)
    - `manual_range_inclusive` in `horndb-wcoj`
    - `into_iter` / `next` confused with the `Iterator` trait methods in
      `horndb-wcoj` trie types — needs `#[allow]` with rationale or
      renaming
    - `map_or` can be simplified in `horndb-owlrl`
    - explicit lifetimes that could be elided in `horndb-wcoj`
    - `loop variable used to index` rewrite in `horndb-wcoj` joins
  - The `horndb-harness` crate also has its own clippy gaps that are
    excluded from the pre-push hook because of rocksdb compile time;
    address those in a separate pass with CI cache priming.

## HIGH — Performance gaps

- [ ] **SPEC-03 WCOJ is 1.6× *slower* than binary-hash on the 4-cycle bench.**
  - Acceptance criterion #2 in `specs/SPEC-03-query-engine.md` requires
    ≥10× speedup; measured ~12.5s vs ~7.6s on a 10⁶-edge synthetic graph.
  - Root causes diagnosed by the implementation agent:
    1. `VecTripleSource::seek` is `partition_point` over the full physical
       level range — should index per-level into a sorted view to get O(log
       k) seek where k is the local subtree size.
    2. `Box<dyn TrieIterator>` virtual dispatch dominates the inner loop —
       consider an enum dispatch or static generics.
    3. `refresh(depth)` does `up + open_level` on every re-entry — should
       cache the cursor state and rewind in place.
  - Address after the correctness gap above (so the fuzzer can validate the
    rewrite).

## HIGH — RDF 1.2 (triple terms) support

- [ ] **Migrate workspace to oxrdf 0.3 with the `rdf-12` feature, deliver
  end-to-end RDF 1.2 triple-term support.**
  - We deliberately track the W3C **RDF 1.2** standard rather than the
    community **RDF-star** extension it superseded — RDF 1.2 has cleaner
    semantics and a cleaner SPARQL surface for the same underlying
    triple-term graph model.
  - Today the workspace is mixed: `horndb-sparql` already pulls
    `oxrdf 0.3` directly, while `horndb-storage` and the harness ride
    `oxrdf 0.2` transitively (oxigraph 0.4 pins it). Stage-1 storage and
    SPARQL dispatch surface RDF 1.2 triple terms as `unreachable!`
    because the Stage-1 N-Triples / SPARQL 1.1 loaders cannot produce
    them; this task lifts that to real support.
  - Concrete work:
    1. Bump workspace `oxrdf` to `0.3.x` + `oxrdfio = "0.3"`, enable the
       `rdf-12` feature; resolve `oxigraph` upgrade (or replace it with
       narrower deps in the harness — see Operational gaps below).
    2. Extend `TermKind` (`crates/storage/src/term.rs`) and the dictionary
       encoding to admit a `TripleTerm` kind; replace the catch-all
       `unreachable!` in `kind_of` with real handling.
    3. Extend the N-Triples/Turtle/N-Quads loaders to accept RDF 1.2
       triple-term syntax (currently the loaders use 1.1-only grammar).
    4. Extend SPEC-07 SPARQL algebra translation to admit `TriplePattern`
       subject/object as triple terms (gate behind a config flag during
       rollout so default behaviour stays SPARQL 1.1).
    5. Add a W3C RDF 1.2 conformance subset to the harness's
       `selected.toml` once the W3C test suite ships fixtures.
  - Replaces the previous "RDF-star — deferred indefinitely" entries in
    SPEC-00-vision and SPEC-07-sparql-frontend.

## MEDIUM — Stage-2 scope explicitly deferred per plans

Items that were marked Future Work in the per-spec plans. Pull from this
list when the corresponding Stage-1 slice is settled.

- [ ] **SPEC-02 storage**: HDT cold tier (F9), CXL/NVMe tiering, MVCC with
  per-tuple visibility, all-6 trie orderings for hot predicates, snapshot
  HDT export, persistent dictionary (Marisa-trie / FST).
- [ ] **SPEC-04 rules**: full `dt-*` datatype rules, `cls-int*`/`cls-uni*`
  list-walking rules, `rdf:type` skew parallelism (F5), production proof
  recording (F4 — Stage-1 ships a stub `Provenance` enum), user-defined
  rules via runtime Datalog frontend.
- [ ] **SPEC-05 closure**: incremental closure updates (F6 — needs the
  SPEC-06 fix below for closure deltas), GPU backend (SPEC-09 territory),
  LAGraph adoption for higher-level primitives.
- [ ] **SPEC-06 incremental**: closure-operator deltas (F5), correct
  retraction semantics (F6 — Stage-1 supports insertion only), MVCC for
  in-flight reads, distributed timely-dataflow (SPEC-09 territory).
- [ ] **SPEC-07 SPARQL**: `DESCRIBE` query form, full `Update` vocabulary
  (`LOAD`/`CLEAR`/`DROP`), backward-chained entailment mode, Kleene-star
  property paths (`*` and `+`), Graph Store Protocol, `EXPLAIN` pragma,
  full streaming result serialization (currently buffers).
- [ ] **SPEC-08 ML**: F3 LLM → SPARQL endpoint (HTTP), real FAISS-backed
  `CandidateGenerator`, HTTP audit endpoint, cost reporting, training-data
  leakage controls.
- [ ] **SPEC-01 harness**: replace the hand-picked 50-case W3C OWL 2 RL
  subset with the full W3C OWL 2 + SPARQL 1.1 suites, full ORE 2015
  corpus (1,920 ontologies), LDBC SPB SF3 + SF5 audited-style runs, LUBM
  + UOBM profile coverage, RDFox A/B (license review required for
  publication — see SPEC-01 risks).

## LOW — Operational

- [ ] **Disk pressure during multi-agent runs.** `oxrocksdb-sys` (pulled
  in transitively by the harness via `oxigraph`) compiles a ~700 MB
  artifact. With multiple worktrees in parallel, the project consumed all
  free space on `/` (~16 GB free → ~100 MB) during the 2026-05-24
  implementation pass and surfaced as misleading "1Password failed to fill
  whole buffer" git-signing errors. Options: set `CARGO_TARGET_DIR` to a
  shared location across worktrees, prune the harness's rocksdb dep
  (replace oxigraph with a narrower SPARQL-only dependency), or document
  a minimum-15-GB-free precondition.
- [ ] **1Password SSH agent reliability.** During the same run the agent
  intermittently returned "no identities" / "communication with agent
  failed" even when the desktop app was unlocked. Two implementation
  subagents hit this and one bypassed signing (which violated the global
  rule); the right fix is to either keep the app foregrounded during long
  agent sessions or pre-cache an unencrypted signing key for CI.
- [ ] **Consolidate `selected.toml` files.** SPEC-01 ships
  `harness/selected.toml` at the workspace root; SPEC-07 added a parallel
  `crates/harness/selected.toml` for its 5 W3C SPARQL fixtures. Pick one
  canonical location and fold the other in.
- [x] **Plans/specs cross-reference cleanup.** `specs/README.md` doesn't
  yet point at `plans/`; add a "Plans" column to the SPEC table so the
  per-spec plan files are discoverable from the spec.

## ARCHIVE — Done (for reference)

- [x] 9 specs written (SPEC-00..09)
- [x] 9 plans written (one per spec; SPEC-09 is roadmap-only)
- [x] 7 implementation subagents dispatched in parallel under worktree
      isolation; all 7 landed signed commits into main
- [x] SPEC-09 is roadmap-only by design (Stage 3, gated on Stage 2 green)
