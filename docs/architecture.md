# HornDB Architecture

This document is the single-page map of HornDB's architecture: what each
subsystem is, how the pieces fit together, and — for every part — what
state it is actually in. It is synthesised from the authoritative SPECs
(`docs/specs/SPEC-00..10-*.md`) and their Stage-1 implementation plans
(`docs/plans/2026-05-24-*.md`).

For the canonical "why" read `docs/specs/SPEC-00-vision.md` first; for the
ground-truth gap list read `TASKS.md`. This document sits between them: the
SPECs say what *should* exist, `TASKS.md` tracks the work to close the gaps,
and the **Status** fields here say what exists *today*.

## How to read this document

Every architectural part carries a **Status** field with one of five values:

| Status | Meaning |
|---|---|
| **implemented** | Code exists and is exercised by tests and/or the conformance harness at Stage-1 level. |
| **specified** | A SPEC (and usually a plan) describes it, but there is no code yet. |
| **planned** | A concrete follow-up exists in `TASKS.md` to build or finish it. |
| **to-spec** | Committed for the current investment round: a `needs-decomposition` epic issue exists and it is queued to be spec'd, but no SPEC is written yet. |
| **deferred** | Intentionally out of scope for now — a later roadmap stage, or indefinitely. |

A part can move only forward: to-spec → specified → planned → implemented. Once an
epic's SPEC is written it leaves **to-spec** and its parts become **specified**.
"deferred" is orthogonal — it marks a scope decision, not a progress point; work is
pulled out of **deferred** into **to-spec** when we commit to specifying it (see
[Stage-2 investment epics](#stage-2-investment-epics)).

> **Maintenance:** the Status fields here and the checkboxes in `TASKS.md`
> are two views of the same reality and must be kept in sync. See
> [Keeping this document honest](#keeping-this-document-honest) and the rule
> in the root `CLAUDE.md`.

---

## 1. Vision and the differentiating bets

**Source:** `docs/specs/SPEC-00-vision.md` · **Status: implemented (Stage 1)**

HornDB is a hybrid forward/backward-chaining RDF reasoner targeting **OWL 2 RL**
semantics with a **SPARQL 1.1** frontend, built in Rust for unified-memory
hardware (HBM / CXL). The symbolic reasoner is the source of truth; ML is a
force multiplier, never the reasoner.

Six bets define the project. Their current state:

| # | Bet | Status | Notes |
|---|---|---|---|
| 1 | Hybrid execution (materialize the closure subset, backward-chain the rest with magic sets) | **partially implemented** | Forward materialization (SPEC-04) and GraphBLAS closure (SPEC-05) ship. Magic-sets / backward-chaining (SPEC-03 F4/F5, SPEC-07 backward mode) is now **planned** under the unified-IR epic E1 ([#185](https://github.com/sunstoneinstitute/horndb/issues/185), `SPEC-23` approved; leaf task [#207](https://github.com/sunstoneinstitute/horndb/issues/207) in `TASKS.md`). |
| 2 | Unified-memory hardware as a first-class target (HBM/DDR5/CXL/NVMe) | **specified / deferred** | Tier API scaffolding exists in SPEC-02; GPU/CXL/NVMe specialization is SPEC-09, Stage 3. |
| 3 | DBSP-style incremental maintenance (Z-set deltas) | **partially implemented** | Insertion Z-set machinery ships (SPEC-06); **rule-path retraction is delta-incremental** — a two-phase overdelete / re-derive fixpoint driven by per-rule weight traces (`SPEC-24` S1, [#210](https://github.com/sunstoneinstitute/horndb/issues/210); supersedes the recompute-and-diff of [#45](https://github.com/sunstoneinstitute/horndb/issues/45), which survives as a config-gated fallback/oracle) — and **closure-path retraction** withdraws `ClosureInferred` rows whose base support is retracted ([#5](https://github.com/sunstoneinstitute/horndb/issues/5)); closure deletion is now **delta-incremental / output-sensitive** — support-counting decremental with a retained recompute fallback — plus exact warm-store retraction via `seed_base_edges` (`SPEC-24` S2, [#211](https://github.com/sunstoneinstitute/horndb/issues/211), `PLAN-24-02`); the change feed publishes per-tick nets to bounded subscribers (`SPEC-24` S3, [#212](https://github.com/sunstoneinstitute/horndb/issues/212)); and the circuit is **wired behind the SPARQL write funnel** — one assert/retract batch plus one `tick()` per Update operation, the engine consuming its own feed, derived rows in a reserved graph the default union reads (`SPEC-24` S4, [#213](https://github.com/sunstoneinstitute/horndb/issues/213), `crates/sparql/src/exec/circuit.rs`; rule registration stays the E4 seam). The rest of `SPEC-24` (epic E2, [#186](https://github.com/sunstoneinstitute/horndb/issues/186)) stays **planned**: phase tasks [#214](https://github.com/sunstoneinstitute/horndb/issues/214)–[#217](https://github.com/sunstoneinstitute/horndb/issues/217) in `TASKS.md`. |
| 4 | GraphBLAS for the closure subset | **implemented** | SuiteSparse:GraphBLAS backend ships (SPEC-05). |
| 5 | Soufflé-style ahead-of-time rule compilation (no interpreter) | **implemented** | `build.rs` codegen from `rules.toml` (SPEC-04). |
| 6 | Provenance / correctability as a hard requirement | **partially implemented** | Stage-1 ships per-triple `Provenance` and proof trees (SPEC-04 F4: `MemStore::proof_tree` / `Engine::proof`); production proof *persistence* (compressed side-table) is **planned**. |

**Non-goals (explicit, unchanged):** beating RDFox on pure single-node
materialization throughput; OWL 2 DL completeness; a rule-interpretation
engine; neural reasoning as source of truth; being a property-graph database.

---

## 2. Subsystem layering

Nine Rust crates under `crates/`, all `publish = false`, `edition = 2021`,
pinned to Rust `1.90.0`. Dependency / build order:

```
                          ┌──────────────┐   ┌──────────┐
                          │ harness (01) │   │  ml (08) │
                          └──────┬───────┘   └────┬─────┘
                                 │ grades         │ opt-in, advises
                                 ▼                ▼
        ┌──────────────────────────────────────────────────┐
        │                  sparql (07)                       │  public surface
        └───────────────────────┬────────────────────────────┘
                                 ▼
                        ┌─────────────────┐
                        │ incremental (06)│  Z-set deltas (insert-only)
                        └────────┬────────┘
                  ┌──────────────┴──────────────┐
                  ▼                              ▼
          ┌──────────────┐              ┌────────────────┐
          │  owlrl (04)  │  routes ───▶ │  closure (05)  │
          └──────┬───────┘  closure     └───────┬────────┘
                 ▼                               │
          ┌──────────────┐                       │
          │  wcoj (03)   │  join substrate       │
          └──────┬───────┘                       │
                 ▼                               ▼
        ┌──────────────────────────────────────────────────┐
        │                  storage (02)                      │  foundation
        └────────────────────────────────────────────────────┘

        hardware-ext (09): empty placeholder, Stage 3.
        python / rdflib API (10): partial — crates/python core surface; off-workspace.
```

Layering rule (SPEC-00): **the harness (SPEC-01) comes first** — the test
bench exists before the engine it grades. A SPEC is not satisfied until its
referenced subset in the harness is green; work may *grow* a subset but never
bypass it.

---

## 3. SPEC-01 — Conformance & benchmarking harness

**Crate:** `horndb-harness` · **Spec:** `SPEC-01` · **Overall status: implemented (Stage 1)**

The bench every other spec is graded against. Ships the `harness` binary with
two engines: `--engine stub` (plumbing) and `--engine owlrl` (real, needs
`--features real-engine`).

| Component | Status | Notes |
|---|---|---|
| W3C OWL 2 RL test-case runner (manifest parse, classify pass/fail/skip) | **implemented** | `runner.rs`, `manifest.rs`, `testcase.rs`. Suite keys: `owl2`, `owl2-w3c-rl`. Premises resolve `owl:imports` hermetically via a per-directory catalog (`rdf.rs` `load_premise`/`expand_imports`) — no network. |
| SPARQL 1.1 test runner | **implemented** | Suite key `sparql11`; path-based `[sparql_query]` consumed by `crates/sparql/tests/w3c_suite.rs` (it also covers the `default_graph`-mode dimension the upstream manifests do not express). The full upstream evaluation suite is the separate `sparql11-eval` key — row below. |
| W3C RDF 1.2 N-Triples *syntax* suite | **implemented** | Suite key `rdf12-n-triples`; 4 positive + 6 negative cases via `oxttl::NTriplesParser`, no reasoner. |
| W3C SPARQL 1.1 *syntax* suite (query + update) | **implemented** | Suite key `sparql11-syntax` ([#110](https://github.com/sunstoneinstitute/horndb/issues/110), epic #10); `mf:*SyntaxTest11` types graded by `spargebra` (same parser as SPEC-07) via `TestKind::SparqlSyntax{Positive,Negative}`. 10 positive + 5 negative (5 update-form) curated checked-in cases under `tests/fixtures/sparql11-syntax/`; relative IRIs resolve against the action-file IRI; sub-ms, no network, no reasoner. |
| W3C OWL 2 RL test-suite ingestion pipeline | **implemented** | `owl2_rl_extract.rs` + `harness extract-owl2-rl`; 115 W3C cases → 100 green in `[suites.owl2-w3c-rl]`, 15 reds tracked in `harness/KNOWN-MANIFEST-BUGS.md`. [#160](https://github.com/sunstoneinstitute/horndb/issues/160)'s RL-reachable remainder is now fully closed — datatype value-space intersection **and** hermetic `owl:imports` resolution both landed; the 15 residual reds are intentional Stage-1 non-goals (OWL 2 DL entailments / fresh-bnode TGD generation). |
| Versioned selection manifest (`harness/selected.toml`) | **implemented** | Single canonical file at workspace root (manifest `[suites.*]` + `[sparql_query]`). |
| Result DB (SQLite) + trend reports (`harness report`) | **implemented** | `db.rs`, `report.rs`; state in `target/harness.sqlite`, JUnit at `target/junit.xml`. |
| Stub-engine smoke target | **implemented** | `stub.rs` (F12). |
| LUBM materialization RDFox A/B (`scripts/bench/compare-rdfox.sh --lubm N`) | **implemented (N=1)** | Identical TBox+ABox and rule set through both engines; closure-count parity gate + HornDB wall-clock cap. Parity is exact (delta 0, [#59](https://github.com/sunstoneinstitute/horndb/issues/59)). The 3× *timing* gate is still open and is **not** closure-bound — the gap is the SPEC-04 F5 `rdf:type`-partition scan ([#133](https://github.com/sunstoneinstitute/horndb/issues/133)). RDFox numbers internal-only (DeWitt). Status and numbers: `docs/benchmarks.md`. |
| LDBC SPB nightly throughput A/B (`.github/workflows/nightly.yml`) | **implemented (feasible scale)** | Per-run HornDB bring-up via `crates/harness/scripts/start-engine.sh` (serving the prepared flat closure, no reasoning); `harness spb-run` drives the SPB aggregation mix and records the full driver report to the trend DB. A/B references are **GraphDB Free 10.8.14** and **Oxigraph 0.5.9** (the latter a Rust/RocksDB SPARQL store with no reasoner — the closest architectural peer, serving the same flat closure; run as two legs, `oxigraph` as-loaded and `oxigraph-optimized` from an `oxigraph optimize`d store copy), each brought up per run so no engine competes for RAM during another's measurement; each leg skips gracefully if that engine fails to start. The Oxigraph legs need a one-time persisted-store build on the runner (`bootstrap-oxigraph-spb.sh`, builds both stores) before they record. The trend DB keeps a 90-day rolling window (`harness prune`). Runs at *feasible scale* (512 k-triple SPB closure, aggregation-only); true SF=0.256 + editorial agents is a TASKS.md follow-up. Numbers: `docs/benchmarks.md`. |
| W3C SPARQL 1.1 *evaluation* suite (query + update) | **implemented** | Suite key `sparql11-eval` (HDB-128). Grades the **whole** upstream manifest tree — `mf:QueryEvaluationTest` + `mf:UpdateEvaluationTest`, 547 cases, `include = ["*"]` — by executing the real SPEC-07 engine and comparing against the case's `.srx`/`.srj` result (`src/sparql_eval.rs`, `TestKind::SparqlQueryEval`/`SparqlUpdateEval`). Read in place from the corpus fetched by `scripts/fetch-w3c-suites.sh` into the gitignored `crates/harness/data/`, following the manifest's `mf:include` list — not mirrored into fixtures. Measured 2026-09-05: **401 pass / 106 fail / 40 ungraded test types**. Nothing is deselected (SPEC-00 harness-first): the 106 reds sit in the suite's `expected_failures` allowlist, which turns a listed failure into a Skip **and** a listed pass into a failure, so CI catches drift in both directions. Root-cause triage in `harness/KNOWN-MANIFEST-BUGS.md`; nightly charts `--metric passed`. Because the corpus is not checked in, the entry carries `fetched = true`: a missing manifest reports the suite Skipped (naming the fetch script) instead of aborting the whole run, so jobs that never fetch still grade everything else, while the jobs that *do* fetch pass `harness run --require-corpus` to keep a missing corpus a hard error. |
| W3C SPARQL 1.1 Graph Store Protocol suite | **implemented** | Suite key `sparql11-gsp` (HDB-165, SPEC-28 S5). The one suite that is not file-graded: each `mf:GraphStoreProtocolTest` is an ordered sequence of HTTP requests, so `src/gsp.rs` (`TestKind::GraphStoreProtocol`) boots the real axum router — `build_router` over a `HornBackend`, the same storage path `serve` uses — on `127.0.0.1:0` and drives it over a plain socket, one server per case so state carries between requests but not between cases. The manifest reader learned the W3C HTTP-in-RDF vocabulary (`ht:Request`/`ht:Response`, `cnt:chars`, `hts:` status IRIs). Response bodies are compared graph-isomorphically (parse + blank-node canonicalize), and the manifest's `/gsp` path prefix is rewritten to HornDB's `/graphs`. Corpus: `graph-store-protocol/` from the `rdf-tests` mirror — **not** the `http-rdf-update/` directory SPEC-28 names, which holds only a deprecated prose draft; the replacement keeps the same `http-rdf-update/manifest#` case IRIs. Fetched by `scripts/fetch-w3c-suites.sh`, so the entry carries `fetched = true` like `sparql11-eval`. Measured 2026-09-06 with `--engine owlrl`: **7 pass / 0 fail / 6 skip** of 13 cases, `include = ["*"]`. The 6 are SPEC-28 S5's deliberate divergences (4 direct graph identification, 1 `multipart/form-data` body, 1 POST-creates-a-graph), listed in `expected_failures` and explained in `harness/KNOWN-MANIFEST-BUGS.md`. |
| Full W3C OWL 2 *evaluation* suite, ORE 2015, LDBC SPB SF3/SF5, LUBM-100/UOBM, broader RDFox A/B | **deferred** | SPEC-01 harness epic ([#10](https://github.com/sunstoneinstitute/horndb/issues/10)) **closed** after the Stage-1 core surface landed (OWL 2 RL ingestion + `owl2-w3c-rl`, RDF 1.2 N-Triples syntax, LUBM RDFox A/B at N=1, and the SPARQL 1.1 *syntax* suite `sparql11-syntax`, [#110](https://github.com/sunstoneinstitute/horndb/issues/110)). The SPARQL 1.1 *evaluation* suite has since landed as `sparql11-eval` (row above, HDB-128). **Stage-2 deferred** (heavy external corpora needing large downloads / self-hosted runners): the full ORE 2015 corpus, LDBC SPB SF3/SF5 audited runs, LUBM-100/1000/8000 + UOBM at scale, and broader/published RDFox A/B (DeWitt license review). Scaffolding exists (`ore.rs`, `ldbc_spb.rs`). |

---

## 4. SPEC-02 — Storage & dictionary encoding

**Crate:** `horndb-storage` · **Spec:** `SPEC-02` · **Overall status: implemented (Stage-1 slice)**

Predicate-partitioned, columnar, dictionary-encoded triple store. The
foundation every other crate reads/writes through.

| Component | Status | Notes |
|---|---|---|
| Dictionary (URIs/blank nodes/literals → stable 64-bit ID, reverse lookup) | **implemented** | `dictionary.rs`, lock-free reads via `DashMap`. The forward map is keyed on a compact byte encoding of the term, not on the `oxrdf::Term`: a typed literal's key carries a small dense id for its datatype IRI (a language-tagged literal's, for its tag) instead of the text, from two side tables private to the dictionary (HDB-95). `TermId` assignment is unaffected. Reclaims dead terms on `Store::compact()` (HDB-121): a mark phase over the rows that survive row compaction — bounded by the tier's own `min_pinned`, no second liveness scheme — frees the lexical bytes and the forward-map key of every term no stored row mentions. **Ids are not re-used** (a thread that has interned but not yet installed rows holds an id no version can see), so index space stays monotonic under `MAX_DICT_INDEX`; `dictionary_terms` vs `dictionary_terms_live` shows the gap. The sweep needs writers quiesced — see `crates/storage/INTEGRATION-NOTES.md`. **The key is not a persistence format** — the side-table ids are assigned in first-seen order, so the same corpus reimported in another order produces different key bytes; SPEC-25 S2's mapped base must build on `snapshot::term_codec`. |
| Term taxonomy in high bits (`TermKind`, inline small literals) | **implemented** | `term.rs`. Includes `TripleTerm = 6` (RDF 1.2). |
| Predicate-partitioned columnar `(s_id, o_id)` storage | **implemented** | `partition.rs`, `store.rs`. |
| In-memory tiering scaffolding | **implemented** | `tier.rs`, `memory_tier.rs` — single warm tier in Stage 1. |
| N-Triples bulk loader (incl. RDF 1.2 `<<( s p o )>>` objects) | **implemented** | `loader/`; fixture `tests/fixtures/triple_term.nt`. |
| Six index orderings on demand (for hot predicates) | **implemented** | `ordering.rs`, `partition.rs` — object-major layout eager for hot predicates, lazy (`OnceLock`) for cold; `Store::scan_predicate_ordered` / `top_predicates`. [#16](https://github.com/sunstoneinstitute/horndb/issues/16) (SPEC-02 F4 + acceptance #6). A partition holds **two** physical layouts, not six: within a partition the predicate is constant, so the six trie orderings collapse onto subject-major and object-major. The threshold is reachable without a code change since HDB-88 — `HORNDB_HOT_THRESHOLD=<n>`, or `off` to make every partition lazy (`horndb_storage::hot_threshold` / `set_hot_threshold`; `MemoryTier::with_hot_threshold` still overrides per tier). Default unchanged at 1,000,000, now as a measured choice — `docs/benchmarks.md`, "Cutting the `apply_quad_batch` hash tables". Note that **no crate above `horndb-storage` calls `scan_predicate_ordered` / `ordered_predicate` today**, so on every shipped path the eager build is load-side cost with no reader; the eager option is kept for SPEC-25 S5 tiering and the SPEC-02 F4 acceptance test. |
| HDT-derived snapshot export/import (SPEC-02 F9) | **implemented** | `snapshot/` — export to a compact, front-coded + gap-coded format and re-import; round-trip is label-preserving (acceptance #5). Measured 5.440 B/triple on synthetic LUBM-shaped data (NF1 ≤6). Named-graph coverage **implemented** — `SPEC-25` S4 ([#228](https://github.com/sunstoneinstitute/horndb/issues/228) in `TASKS.md`): export/import cover the default graph plus every named graph and round-trip to exact quad-set equality. The same encoding is applied per graph, behind a format-version bump: a store with no named-graph data still writes v1 (the Stage-1 layout), named-graph data writes v2, and a Stage-1 reader rejects v2 through the existing `unsupported snapshot version` path (one-way compatibility). This is the checkpoint format S3 recovery starts from. Not rdfhdt wire-compatible. |
| HDT cold tier + tiering seam | **planned** | `SPEC-25` S5 ([#229](https://github.com/sunstoneinstitute/horndb/issues/229) in `TASKS.md`): read-only cold `Tier` over the snapshot encoding, partition-granularity demotion/promotion with the SPEC-08 `HotSetAdvisor` bias, real `storage_tier_bytes_estimated` values (the [#148](https://github.com/sunstoneinstitute/horndb/issues/148) deferral). CXL/NVMe placement stays SPEC-09 (Stage 3). |
| Copy-on-write snapshot isolation (concurrent-read / single-writer) | **implemented** | SPEC-02 #19: `Store::snapshot()` / `StoreSnapshot` pin a stable, internally-consistent read transaction over an immutable versioned `TierSnapshot`; `MemoryTier::insert_quad_batch` is copy-on-write (clone the top-level graph map, rebuild only affected graphs, bump version, atomically swap) so concurrent writers never disturb a pinned snapshot. Since HDB-84 an `insert_quad_batch` write appends its rows to the touched partitions as one extra sorted **run** rather than rebuilding them; the runs merge into the columns readers need once, on the first read, so repeated small batches cost the rows they carry, not the rows already stored. Since HDB-122 that merge runs **outside** the partition's `runs` mutex (clone the run list, merge, swap the collapsed list back under the lock), so the reader paying for it no longer stalls concurrent writers to the same partition; `horndb_storage_partition_merge_seconds` and `horndb_storage_partition_merges_total{trigger}` make the cost and its trigger attributable. Since HDB-102 `apply_quad_batch` takes the same append-run path, chosen **per predicate**: a predicate the batch only adds to gets an appended run and carries nothing, while a predicate the batch deletes from is still rebuilt row by row (a delete end-stamps a row inside an immutable `Arc`-shared run, so it cannot be written in place without rewriting history under pinned readers). That covers every add-only batch — so `Store::insert_quads`, `INSERT DATA` and every non-deleting SPARQL write, which HDB-91 had measured at `copy_forward` + `build` = 94% of an append: a 1,002,000-triple append into a 9,995,000-triple store in 16 calls went **15.83s → 1.45s**, and chunked now costs what one call costs (`docs/benchmarks.md`, "`apply_quad_batch` takes the append-run path"). `ApplyReport::inserted` stays exact on both paths: the append path tests each added pair against the partition's unmerged runs (`PredicatePartition::mark_live`, one galloping search per run), which answers the same question as the merged view without forcing the merge. Before that, HDB-88 made the rebuild itself cheaper without changing its shape — the add side of a batch is grouped into a sorted `Vec` instead of a `HashSet`, `still_visible` likewise, and the builder skips its sort when the rows already arrive sorted: tier work on a 10M bulk insert −51%, `copy_forward` on an append −72%, the 16-call 1M append −34% (`docs/benchmarks.md`, "Cutting the `apply_quad_batch` hash tables"). The append-only dictionary keeps pinned term ids meaningful. HDT export reads one pinned snapshot, so a checkpoint under concurrent writes is internally consistent (NF5). `memory_tier.rs`, `store.rs`. |
| MVCC with per-tuple visibility + delete path | **implemented** | `SPEC-25` S1 ([#225](https://github.com/sunstoneinstitute/horndb/issues/225) in `TASKS.md`, `PLAN-25-01`): begin/end visibility stamps on the tier commit clock, `retract_quad_batch`, compaction, and the SPEC-24 S6 snapshot contract ([#215](https://github.com/sunstoneinstitute/horndb/issues/215)). Substrate: stamp columns on the existing copy-on-write tier (not delete-bitmap sidecars or in-place append — that comparison is deferred to hornbench, [#242](https://github.com/sunstoneinstitute/horndb/issues/242)). Native retraction retired the `horndb-sparql` `DELETE DATA` tombstone overlay. Sits above the copy-on-write snapshot isolation row. |
| Graph-scoped storage access paths | **implemented** ([#265](https://github.com/sunstoneinstitute/horndb/issues/265), `PLAN-28-02`, `SPEC-28` phase 2) | `store.rs`: `StoreSnapshot::scan_graph(GraphId)` (every visible triple in one graph, O(quads in the graph + predicates in the graph), never O(store)) and its id-level twin `iter_graph_term_ids(GraphId)` (key-ordered); `graph_len(GraphId)` (O(predicates in the graph), backed by a cached per-partition live-row count); `graph_uri(GraphId)` (decode a graph id back to its IRI); `graphs()` now visibility-filtered (a graph exists iff it holds >=1 visible quad) and sorted by `GraphId`. `scan_predicate(graph, predicate)` replaces `scan_predicate_default_graph` on both `Store` and `StoreSnapshot` (deleted, no alias). `len()`/`is_empty()` flip from default-graph-scoped to whole-store. This is storage-only plumbing: `HornBackend`'s write funnel is now quad-grain internally (`clear_all` sweeps every graph; the `live_keys` mirror this phase introduced was removed again in HDB-89 — storage decides membership), but the public `exec::Store` trait and all SPARQL-visible behaviour were unchanged by *this* phase. Phase 3 ([#266](https://github.com/sunstoneinstitute/horndb/issues/266)) is what made these paths user-visible: `GRAPH`/`FROM` queries now evaluate against them (SPEC-07 section below). Phase 4 ([#267](https://github.com/sunstoneinstitute/horndb/issues/267), `PLAN-28-04`) built named-graph Update on the same plumbing — the store is **not** default-graph-only (see the two Update rows in the SPEC-07 section below). Phase 5 (GSP) reads these same paths: `scan_graph_quads` is the base-only read set the `/graphs` `PUT` diff is computed against ([#268](https://github.com/sunstoneinstitute/horndb/issues/268)). |
| Persistent on-disk dictionary (FST base) | **implemented** (probe costs pending hornbench) | `SPEC-25` S2 ([#226](https://github.com/sunstoneinstitute/horndb/issues/226)), `PLAN-25-02`, HDB-57: `Dictionary::flush(path)` / `Dictionary::open(path)` / `Store::with_dictionary` (`dict_base.rs`). One memory-mapped file: a `u64` offset table over a `snapshot::term_codec` arena for id → term (one indirection; a zero-length slot is a GC tombstone, so a reclaimed index reloads as reclaimed and is never re-issued) and an `fst::Map` for term → id — the HDB-93 choice (`benchmarks.md`, "Which structure backs the mapped dictionary base"). The in-memory overlay is unchanged and numbers from `base_len + 1`, so ids are byte-identical with or without a base (`tests/dictionary_persist.rs` reloads a corpus into a reopened store and gets the same ids and no new ones). Reopen is mmap + header validation. Deferred: the running process keeps its overlay after a flush (the merged file serves the next open); the HDB-93 repeat cache. Bench: `benches/dict_persist.rs`, `audit-pass.sh` leg `dict_persist` — pending, hornbench was offline. |
| Write-ahead log + crash recovery | **implemented** (append/replay costs pending hornbench) | `SPEC-25` S3 ([#227](https://github.com/sunstoneinstitute/horndb/issues/227)), `PLAN-25-03`, HDB-58: `Store::open(dir)` / `open_with(dir, SyncPolicy)` replays `dir/wal.<gen>` over the `dir/dict.<gen>` base named by `dir/MANIFEST`; every insert / retract / apply batch is one CRC-32C-framed record (its dictionary appends, dels, adds, commit version, blank-node tag) appended and fsynced (`EveryBatch` default, `Every(Duration)` lazy) before the tier write; `Store::checkpoint()` flushes the dictionary, dumps the visible rows as `Checkpoint` records into the next generation and switches `MANIFEST` by rename (the atomic commit point), then unlinks the old files. Recovery reproduces term ids, quads, commit version and per-row stamps for every batch after the checkpoint; rows the checkpoint carried restart at its version. Torn tail records are dropped and truncated; a bad checksum mid-log is `StorageError::Wal`. Not wired to `serve` (call site recorded in `crates/storage/INTEGRATION-NOTES.md` for HDB-51); upgrades NF5 for WAL-backed stores. SPEC-24 S5 ([#214](https://github.com/sunstoneinstitute/horndb/issues/214), HDB-52) layers the DeltaLog contract on this format: two more record kinds (`Input`, `TickCommit`) share the framing and the fsync policy, replay hands them to the circuit instead of the tier (`Store::log_input` / `log_tick_commit` / `take_recovered_inputs`), and a checkpoint's generation roll truncates them. |
| Turtle / N-Quads bulk-import paths (SPEC-02 F8) | **implemented** | `loader/turtle.rs`, `loader/nquads.rs` (streaming, via `oxttl`); N-Quads routes each quad to the graph named by its fourth term (F7), default-graph triples to the reserved sentinel. Shared `LoadStats`/`subject_to_term`/the tier batch size (`load_batch_triples`, `HORNDB_LOAD_BATCH_TRIPLES`) hoisted to `loader/mod.rs`; N-Triples path unchanged. Fixtures `tests/fixtures/{tiny.ttl, with_literals.ttl, named_graphs.nq}`. [#18](https://github.com/sunstoneinstitute/horndb/issues/18). |
| Parallel chunked parsing in the bulk loaders | **implemented, on by default** | `loader/parallel.rs` plus `load_*_slice` beside each streaming loader: `oxttl`'s `split_slice_for_parallel_parsing` cuts the document into N chunks, one thread each, while the calling thread **allocates term ids** in document order — so a parallel load produces the same store as a serial one, term ids included (`tests/parallel_loader.rs`). Since HDB-106 the parse threads also **probe** each term against the dictionary (`Dictionary::get`, read-only, allocates nothing) and send the answers with the row; the consumer interns only what the probes missed. Ids are unchanged because the dictionary is append-only — a probe that hits names the id a later `intern` would return, and a probe that misses falls through to `intern` on the consumer, in order. At the shipped 8-thread / 8M-triple-buffer default the probe resolves 41.8% of intern calls, taking the `intern` phase 3.00s -> 2.14s and a trainmarks xlarge Turtle load 5.58s -> 5.08s at flat peak RSS. **The win depends on the parse threads having idle capacity**, so the probe is **gated off below 4 chunks** (`parallel::should_probe`, `MIN_PROBE_CHUNKS`): ungated it measured -9.0% / -7.9% (Turtle / N-Triples) at 8 threads and -4.9% / -5.2% at 4, but a **4-5% loss at 2**, where the parse is already close to the critical path and the probe adds to it roughly one-for-one — and `auto` = `min(available_parallelism(), 8)` puts a 2-core VM or container on that path by default. Below the gate the loaders take the pre-HDB-106 path exactly. Gated on the actual chunk count, not the thread setting, since `oxttl` may return fewer chunks than asked. Sweep, gate confirmation and reasoning in `docs/benchmarks.md`. `intern` is also a **counted phase** on this path now (one clock pair per 8,192-row batch in `QuadSink`), with an optional `HORNDB_INTERN_PHASES=1` sub-phase split; before HDB-106 it could only be read off as a residue. N-Triples/N-Quads split unconditionally; Turtle needs `HORNDB_PARALLEL_TURTLE=1` and must pass `turtle_split_is_safe`, because `oxttl` propagates leading prefixes into chunks but not the base. **`HORNDB_LOAD_THREADS` defaults to `auto`** = `available_parallelism()` capped at 8; `=1` is serial, an explicit `=<n>` is uncapped, and a malformed value falls back to 1. The cap is 8 because HDB-96's sweep flattens there — Turtle 12.926s → 5.581s, N-Triples 9.672s → 4.903s, with the 16th thread worth +1.5% / −1.1% — and because by 8 threads `parse` is 14% / 3.6% of the load, the rest being interning and the tier, both serial by construction. Costs peak RSS +78% Turtle / +57% N-Triples (the 8M-triple parse buffer). The `load_*_file` entry points reach this path by reading the whole document into memory, bounded by `HORNDB_LOAD_MAX_SLICE_BYTES` (default 2 GiB) above which they fall back to streaming. Sweep, phase split and the memory accounting in `docs/benchmarks.md`. |
| HDT bulk-import path | **planned** | Tracked under SPEC-02 completeness ([#3](https://github.com/sunstoneinstitute/horndb/issues/3)); add when a consumer needs HDT ingest (export side ships, row above). |

> **Note:** SPEC-03's 4-cycle ≥10× performance gate was first hypothesised to
> be blocked here — that closing it needed a compressed columnar warm tier
> (SPEC-02 F1), not more executor tuning. [#15](https://github.com/sunstoneinstitute/horndb/issues/15)
> tested that with a compressed columnar `TripleSource` inside `horndb-wcoj`
> (7.5× smaller, WCOJ 0.73× → 1.11×) — directionally right but **not** ≥10×.
> The gate was finally closed in [#1](https://github.com/sunstoneinstitute/horndb/issues/1)
> by fixing the *graph shape*: the old uniform low-degree synthetic graph never
> forces the intermediate-result blow-up WCOJ needs. The canonical win case is
> a *skewed* graph (high-out-degree hubs + a thin closure), where a binary join
> must materialise a huge 3-path relation while WCOJ never does. See §5.

---

## 5. SPEC-03 — WCOJ query engine

**Crate:** `horndb-wcoj` · **Spec:** `SPEC-03` · **Overall status: implemented (Stage-1 slice)**

The join substrate all triple-pattern matching flows through. Leapfrog
Triejoin with a binary-hash fallback.

| Component | Status | Notes |
|---|---|---|
| Triple-pattern executor (variable bindings out) | **implemented** | `executor/wcoj.rs`. |
| Leapfrog Triejoin on n-way patterns | **implemented** | `trie/leapfrog.rs`, `trie/source_iter.rs`. |
| Binary hash-join fallback | **implemented** | `executor/binary_hash.rs`. |
| Generic-over-source executor (GAT, no `Box<dyn>` in hot path) | **implemented** | Removed vtable dispatch and per-prime allocations during the WCOJ perf pass. |
| Cardinality estimation | **implemented** | `cardinality.rs`, `estimator.rs`, `stats.rs`. `StatsEstimator` over `SnapshotStats`; reached from SPARQL through `HornBackend::cardinality_estimate`, which today serves `EXPLAIN` only. Summaries are cached per snapshot scope and tagged with the store commit version (`HornBackend::stats_cache`), and a small write **merges its quad delta into them** (`SnapshotStats::apply_delta`, HDB-123) instead of dropping them, so a write-then-read feed does not pay an `O(store)` rebuild per batch. Exact under the merge: totals, per-predicate counts, distinct predicates, both NDVs. Approximate: `max_degree` is only ever raised (a valid upper bound, loose after deletes), and the characteristic-set index is left stale until a full rebuild, which `STATS_DRIFT_DIVISOR` forces once merged rows pass 1/10 of the graph. Rebuilds are counted and timed by `horndb_sparql_stats_rebuild_total` / `_seconds`. |
| Cost-based plan choice (estimator drives the plan) | **implemented** (HDB-46, SPEC-23 phase 4, `PLAN-23-04`) | `Planner::choose(bgp, &dyn Stats) -> JoinSpec` (`crates/wcoj/src/planner.rs`) is cost-based: GYO ear removal finds the BGP's cyclic core (never split by a hash join), `CostModel` (`cost.rs`) prices WCOJ nodes by i-cost (rows read per variable extension) and hash joins by build+probe on one additive scale, and a DP over connected pattern subsets (≤ 5 non-ground patterns, one cyclic core per connected component; greedy build-up over cores and single patterns past that) picks the cheapest `JoinSpec` — a tree of `Scan` / `HashJoin` / multi-way `Wcoj` nodes. Only sub-nodes of a hash tree pay a materialisation term, and a hybrid tree must beat whole-BGP WCOJ by `HYBRID_MARGIN` (2×) or the planner emits one streaming WCOJ node. Variable order inside a WCOJ node is connected degree-first (next variable must touch the bound prefix; ties by estimated rows) with a shortlist sweep over the most selective first variables, so on the HDB-108 q3 shape `?customer` (selective `:country :Norway`) binds before `?order` (`tests/planner_choice.rs::q3_shape_binds_selective_customer_before_order`; same-process A/B in `tests/plan_ab.rs`). The AGM bound caps the cardinality estimate only. Uninformed stats (`ZeroStats`: the direct-store path, or a copied snapshot whose summary is still being built on the `horndb-stats` background thread) and single-pattern BGPs skip the search: one WCOJ node in degree order. `HORNDB_WCOJ_CUTOVER=<n>` restores the retired fixed cutover for bisection. Stats reach the planner at execution time in `HornBackend` (the cached per-scope `SnapshotStats`, built off the query path on first use); the SPARQL `PassId::JoinPlanning` stays unregistered — algebra-level (non-BGP) join ordering and EXPLAIN display of the `JoinSpec` are follow-ups. Whole-BGP WCOJ plans stream from the leapfrog executor; hybrid plans run on the hash-join tree evaluator (`executor/binary_hash.rs`), which materialises each node. hornbench numbers for the q3 target (`docs/benchmarks.md`) are pending the runner coming back online. |
| Cancellation (≤100 ms) | **implemented** | `cancel.rs`. |
| Correctness vs binary-join (differential fuzzer) | **implemented** | Repeated-pattern over-production bug fixed; fuzzer cases 16 → 256, `#[ignore]` removed. |
| 4-cycle ≥10× WCOJ-over-binary-join gate (acceptance #2) | **implemented** | Met in [#1](https://github.com/sunstoneinstitute/horndb/issues/1) by re-pointing `benches/four_cycle.rs` at the *canonical* WCOJ win case — a skewed ~10⁶-edge graph (`SyntheticGraph::skewed_four_cycle`: high-out-degree hubs + a thin, dedicated closure) instead of the old uniform low-degree graph, which never forces the intermediate-result blow-up WCOJ exists to avoid. Correctness pinned by `tests/skewed_four_cycle.rs` against an independent brute-force count. Measured ~34×; numbers in `docs/benchmarks.md`. |
| Magic-sets / demand transformation (F4) | **planned** | Unified-IR epic E1 ([#185](https://github.com/sunstoneinstitute/horndb/issues/185), `SPEC-23` approved) — leaf issue [#207](https://github.com/sunstoneinstitute/horndb/issues/207). `wcoj/src/lib.rs` still stubs it. |
| SLG-resolution tabling (F5) | **planned** | E1 leaf issue [#207](https://github.com/sunstoneinstitute/horndb/issues/207). Blocks SPEC-07 backward-chained mode. |
| GPU WCOJ kernels | **deferred** | SPEC-09, Stage 3. |

---

## 6. SPEC-04 — OWL 2 RL rule engine

**Crate:** `horndb-owlrl` · **Spec:** `SPEC-04` · **Overall status: implemented (Stage-1 slice)**

Forward-chaining engine. The OWL 2 RL/RDF rule set is **compiled** to native
Rust at build time from `rules.toml` (Soufflé-style) — no interpreter.

| Component | Status | Notes |
|---|---|---|
| Codegen pipeline (`build.rs` from `rules.toml`, `codegen/`) | **implemented** | Emits `fire_<id>` functions; see `INTEGRATION-NOTES.md`. |
| Semi-naïve evaluation with delta tables | **implemented** | `delta.rs`, `engine.rs`, `backend.rs`. Compiled rules fire genuinely delta-driven since HDB-40 ([#134](https://github.com/sunstoneinstitute/horndb/issues/134)): one variant per body atom reads the previous round's applied triples, the rest the full store (`MaterializeOpts::firing`; `Naive` is the oracle in `tests/semi_naive_differential.rs`). |
| `Engine` satisfying the harness `Reasoner` trait | **implemented** | `integration.rs` (oxrdf dictionary over `MemStore`); closure backend is injectable via `Engine::with_backend(BackendChoice)` — default `RuleFiring`, optional GraphBLAS (`graphblas-backend` feature, [#61](https://github.com/sunstoneinstitute/horndb/issues/61)). Adapter in `harness/src/owlrl_engine.rs`. |
| Reset and rematerialize (F7) | **implemented** | Full re-materialization per `load`. |
| `owl:sameAs` routed to SPEC-05 EQREL (F6) | **implemented** | Rule engine does not re-derive `eq-sym`/`eq-trans`. |
| Subset of rules (`eq-rep-*`, common `prp-*`/`cls-*`/`cax-*`/`scm-*`, incl. `scm-eqc-rev`) | **implemented** | 98 W3C OWL 2 RL cases green. `scm-eqc-rev` derives `owl:equivalentClass` from two-way `rdfs:subClassOf`. Datatype value-space intersection narrowing of `rdfs:range` (`datatype_ranges.rs`) flips `I5.8-008/009-pe`. |
| `Provenance` side-table (F4) | **implemented** | `provenance.rs` — `struct Provenance { rule_id, premises }` recorded per derived triple; the basis of the proof tree (next row). |
| Proof recording (F4: `(rule_id, premises)` per derived triple → recursive proof tree) | **implemented** | Compiled + `list_rules.rs` rules record real body premises; `MemStore::proof_tree` / `Engine::proof` return a full proof tree bottoming out at asserted triples (`provenance.rs`, `integration.rs`; `tests/proof_tree.rs` covers NF4 depth + latency). Closure-backend nodes record empty premises by design; restriction-rule schema declarations are an elided side condition (instance premises still recorded). Production *persistence* (compressed side-table, on-demand re-derivation) remains Stage 2. |
| Datatype subsumption (`dt-type1` + `dt-type2` XSD lattice) | **implemented** | Load-time injection of `byte ⊑ short ⊑ int ⊑ ... ⊑ decimal` (and unsigned/non-negative arms); flips `I5.8-006-pe`/`I5.8-011-pe` green. |
| Max-cardinality (unqualified `cls-maxc1`/`cls-maxc2`, qualified `cls-maxqc1`–`cls-maxqc4`) | **implemented** | Hand-written in `list_rules.rs`; restriction literals (`owl:maxCardinality "0"`/`"1"`, and qualified `owl:maxQualifiedCardinality` + `owl:onClass`) classified at load time in `integration.rs`. `cls-maxc1`/`cls-maxqc1`/`cls-maxqc2` → `owl:Nothing` (inconsistency), `cls-maxc2`/`cls-maxqc3`/`cls-maxqc4` → `owl:sameAs`. The qualified rules ([#36](https://github.com/sunstoneinstitute/horndb/issues/36)) are covered by unit + integration tests; no `selected.toml` entry, because the only W3C qualified-cardinality case (`ObjectQCR-002-pe`) is blocked on fresh-bnode `owl:complementOf` generation, not on these rules. |
| Disjoint properties (`prp-pdw` pairwise, `prp-adp` list `owl:AllDisjointProperties`) | **implemented** | `prp-pdw` compiled from `rules.toml`; `prp-adp` ([#37](https://github.com/sunstoneinstitute/horndb/issues/37)) hand-written in `list_rules.rs` (list-walking analogue), both head `?u rdf:type owl:Nothing` on a shared `(u, w)` pair. Covered by unit + engine tests; the W3C `DisjointObjectProperties-*-cons` / `DisjointDataProperties-*-cons` cases in the selection exercise the no-false-fire path. The `*-pe` variants stay red on a DL `differentFrom`/`AllDifferent` entailment with no OWL 2 RL rule (`harness/KNOWN-MANIFEST-BUGS.md`). |
| Literal-value datatype rules (`dt-eq`/`dt-diff`/`dt-not-type`) | **implemented** | Load-time `inject_datatype_literal_axioms` (`integration.rs`) classifies each instance literal's value via `crates/owlrl/src/datatype_literals.rs` over the Stage-1 datatype set (XSD integer tower, `xsd:string`/`boolean`, plain/lang literals), bucketed by canonical value so the pass is O(k) in distinct literals (HDB-147): value-equal ⇒ `owl:sameAs` within a bucket (`dt-eq`, cross-lexical `1`≡`+1`≡`01` and cross-datatype `1`^^byte≡`1`^^integer), out-of-value-space lexical form ⇒ `owl:Nothing` (`dt-not-type`). `dt-diff` is **not** materialised pairwise: its only consumer is `eq-diff1`, so the post-fixpoint `inject_literal_differences` pass asserts `owl:differentFrom` only for the comparable, value-distinct literal pairs that the closure made `owl:sameAs`, then re-runs the fixpoint so `eq-diff1` reports the clash. Flips `#New-Feature-Keys-006-incons` green (issue #40). Disjoint value spaces (string vs integer) are never cross-compared; non-XSD/unhandled datatypes stay opaque (Stage-1 soundness). |
| Datatype value-space intersection (`I5.8-008/009-pe`) | **implemented** | Post-materialization pass `crates/owlrl/src/datatype_ranges.rs` (`derive_range_intersections`, wired in `integration.rs`): models each XSD numeric-tower datatype's value space as an integer interval, intersects the value spaces of a property's ≥2 *independent* (subset-incomparable) `rdfs:range` datatypes, and asserts `rdfs:range T` for every `T` whose value space is a superset of that intersection (supersets only ⇒ sound). Runs after the fixpoint so it composes with `scm-rng1`/`scm-rng2`-inferred ranges. Flips `I5.8-008/009-pe` green (issue #160). |
| `rdf:type` skew parallelism (F5) | **partially implemented (list-rule path, compiled-rule object index and semi-naïve firing landed; cross-rule parallelism of compiled rules open)** | The `rdf:type`-driven hand-written list rules (`cls-int1`, `cls-uni`, `cax-adc`, `prp-key`) partition their per-subject filtering by class id and parallelise it across rayon above `PAR_TYPE_THRESHOLD` (`crates/owlrl/src/list_rules.rs`), selected by `MaterializeOpts::parallel` (`ParallelStrategy::Auto` default; `Serial` is the oracle). Identical closure proven by `tests/rdf_type_skew_differential.rs` (3 large-extent fixtures + proptest); `benches/rdf_type_skew.rs` + `docs/benchmarks.md` record the win ([#39](https://github.com/sunstoneinstitute/horndb/issues/39)). Both causes the 2026-06-27 profile named for the **compiled** (`cax-sco`-style) rules — an un-indexed full `rdf:type`-partition scan, and naïve (non-delta) re-firing — are now fixed (fixes #1 and #2 below), and neither moved end-to-end reason time: on the taxonomy corpus `compiled_rules_ms` is flat (212.0 → 208.3 ms) inside an unchanged `reason_ms`, and on LUBM-1 compiled-rule firing turns out to be only ~4 % of `reason_ms`. The remaining cost is therefore neither of those two, and not a parallelism gap either — it was the apply/intern path over a closure that the all-pairs `dt-eq`/`dt-diff` literal axiom injection blew up quadratically — fixed by HDB-147 (O(k) bucketed `dt-eq`, `dt-diff` derived lazily); the LUBM-1 attribution is pending a hornbench re-run (`docs/benchmarks.md`). Fix #1 — a within-partition object index on `MemStore` (`obj_index`: predicate → object → subjects; no `FireFn`/trait change) — **landed** ([#133](https://github.com/sunstoneinstitute/horndb/issues/133), 2026-07-07): `probe(None, p, Some(o))` is now O(\|extent\|), cutting `compiled_rules_ms` ~17% on the LUBM-shaped A/B (hornbench; closure bit-identical) — see `docs/benchmarks.md`. Fix #2 — genuine delta-driven semi-naïve firing (`FireFn` now takes the previous round's delta; the codegen emits one delta-bound variant per body atom) — **landed** (HDB-40, [#134](https://github.com/sunstoneinstitute/horndb/issues/134)); closure and round count proven identical to the naïve oracle (`tests/semi_naive_differential.rs`), numbers in `docs/benchmarks.md`. Both specified in `docs/specs/SPEC-15-owlrl-type-index-seminaive.md`. |
| `eq-rep-p` predicate-position skew fix + always-relevant rule marking | **implemented** | Always-relevant marking via `wildcard_predicate`; semantics-preserving class-canonical path in `crates/owlrl/src/eq_rep_p_opt.rs` (union-find over `owl:sameAs`), default `EqRepPStrategy::Optimized`. Differential proptest `tests/eq_rep_p_differential.rs` proves identical closure to the naïve oracle. `TASKS.md` #2. Downstream F5 partition-by-class-id (row above) now implemented for the list-rule path. |
| Inconsistency surfaced at serve time (`owl:Nothing` marker) | **implemented** | `Engine::inconsistent_individuals` (`integration.rs`) decodes the `owl:Nothing` witnesses behind `Engine::is_consistent`; `load_with_reasoning` returns them in `ReasonStats` (capped at 20) and `serve --materialize` applies `[reasoning].on_inconsistency` (`warn` default / `reject-startup`, which exits non-zero without reporting ready / `serve-with-flag`), always publishing the `horndb_reasoning_inconsistent` gauge. `serve-with-flag` stamps `x-horndb-inconsistent: true` on every HTTP response. Covered by `crates/sparql/tests/serve_inconsistency.rs`. Witnesses are logged rather than exposed through the SPEC-27 provenance view (HDB-66, not landed); re-checking after each incremental round is HDB-51. |
| User-defined rules (runtime Datalog frontend) | **deferred** | Stage 2 extension. |

---

## 7. SPEC-05 — GraphBLAS closure backend

**Crate:** `horndb-closure` · **Spec:** `SPEC-05` · **Overall status: implemented (Stage-1 slice)**

Handles the *closure subset* — transitive properties, `rdfs:subClassOf`,
`rdfs:subPropertyOf`, `owl:sameAs` — as semiring matrix algebra on
SuiteSparse:GraphBLAS. SPEC-04 routes those axioms here.

| Component | Status | Notes |
|---|---|---|
| SuiteSparse:GraphBLAS C-ABI integration (`build.rs` + bindgen, `links = "graphblas"`) | **implemented** | `ffi.rs`, `grb.rs`, `bindings.rs`. |
| Transitive closure via iterated `GrB_mxm` (`LOR_LAND_BOOL`) | **implemented** | `closure/transitive.rs`. |
| `rdfs:subClassOf` / `rdfs:subPropertyOf` schema closure | **implemented** | `closure/schema.rs`. |
| `owl:sameAs` equivalence classes (union-find / EQREL) | **implemented** | `sameas.rs`. |
| Dense renumbering cache (`dictionary_id ↔ dense_index`) | **implemented** | `dense_id.rs`. |
| Materialization writeback to storage (no rule re-fire) | **implemented** | `sink.rs`. |
| Wiring the GraphBLAS closure into the owlrl `Engine` (production replacement for `RuleFiringBackend`) | **implemented** | `crates/owlrl/src/graphblas_backend.rs` (`GraphBlasBackend`, `graphblas-backend` feature) computes `scm-sco`/`scm-spo`/`eq-sym`/`eq-trans`/`prp-trp` via strict `transitive_closure` over a dense `BoolMatrix`; injected via `Engine::with_backend(BackendChoice::GraphBlas)` — operator-selectable at the server via `[reasoning].backend = "graphblas"` (see §15). Differential parity with `RuleFiringBackend` in `crates/owlrl/tests/closure_backend_differential.rs`. Profiling ([#61](https://github.com/sunstoneinstitute/horndb/issues/61), `docs/benchmarks.md`) shows the swap is a decisive win only when closure dominates; the LUBM-shaped materialize cost is compiled-rule/`rdf:type`-scan bound ([#133](https://github.com/sunstoneinstitute/horndb/issues/133)), not closure-bound. |
| Vendored GraphBLAS source subset (static, OpenMP, checked-in bindings) | **implemented** | A trimmed `v10.3.0` source subset checked into `crates/closure/vendor/GraphBLAS` (~43 MB, 3875 files) — no git submodule; `vendored`+`openmp` default Cargo features (`regen-bindings` optional), statically linked (verified via `otool -L`), checked-in `src/bindings.rs`. `vendor/refresh-graphblas.sh` re-derives the subset from an upstream tag using `vendor/graphblas-keep.txt`; provenance in `vendor/GraphBLAS.vendor.md` (ADR-0019). Supersedes the `[x]` "CI: install GraphBLAS on runners". |
| Shared, flock-guarded GraphBLAS build across worktrees | **implemented** | `build.rs` compiles the vendored GraphBLAS once per `(target, version)` into `crates/closure/vendor/.shared-build/<target>/<version>/` (anchored at the main worktree, gitignored), reused across git worktrees; concurrent builders serialise on an `fs4` advisory flock with the builder pid written in for diagnostics; CI caches the dir keyed on the git tree hash of `crates/closure/vendor/GraphBLAS`. Details in `crates/closure/INTEGRATION-NOTES.md`. Narrows the disk-pressure concern ([#13](https://github.com/sunstoneinstitute/horndb/issues/13), closed not-planned) to rocksdb. |
| Incremental closure updates (F6) — insertion + retraction | **implemented** | `closure/incremental.rs` (`IncrementalTransitiveClosure`) + `sink.rs` (`IncrementalClosureBackend`): a single-edge insert updates only the affected slice (backward-reach(s) × forward-reach(o)) and writes only the delta to the sink. **Deletion/retraction** (`delete_edge`/`delete_edges`/`delete_transitive_edges`) retains the asserted base edges alongside the closed set; retracting a base edge recomputes base-reachability over the affected source region and withdraws only the closure pairs no longer derivable over the post-delete base (invariant `closed == transitive_closure(base)`). Differential proptests vs GraphBLAS full closure (`tests/incremental.rs` insertion, `tests/incremental_retraction.rs` random insert/delete sequences). SPEC-06 owns the +/- sign; the SPEC-05 layer is sink-insertion-only and returns the withdrawn edges. Closure-path retraction delivered under [#5](https://github.com/sunstoneinstitute/horndb/issues/5) (insertion path [#42](https://github.com/sunstoneinstitute/horndb/issues/42)). |
| Valued closure / custom semirings (Sunstone annotated reasoning) — Fork A | **implemented** | Readiness metrics ([#11](https://github.com/sunstoneinstitute/horndb/issues/11)): `grb::ValuedMatrix` (FP64 `(max,×)` carrier, built-in + user-defined-op multiply) and `metrics::valued_transitive_closure` (N/nnz/density/iterations-to-fixpoint/per-iter frontier work/MxM share). **Fork A delivered** ([#12](https://github.com/sunstoneinstitute/horndb/issues/12)): `crosswalk::CrosswalkGraph` — build a weighted concept/entity adjacency from RDF 1.2 triple-term–annotated confidences (dictionary IDs → dense F7 renumbering) and resolve best-confidence crosswalk/propagation mappings in one built-in `(max,×)` closure instead of a SPARQL property-path crawl (`tests/crosswalk.rs`, `benches/crosswalk.rs` on a GTIO/SKOS-shaped graph). **Measured on `hornbench` (`docs/benchmarks.md`):** valued penalty a modest constant vs boolean; generic-kernel penalty for a scalar FP64 op ~1.0× → built-in semirings suffice for a scalar carrier and **PreJIT buys ≈0**. **Fork B (structured carrier / custom semiring) and PreJIT deferred** (SPEC-05 valued-reasoning addendum) until a use case needs a structured `(confidence, match-type, provenance)` carrier. |
| LAGraph adoption; GPU GraphBLAS backend | **deferred** | Stage 2 (LAGraph) / SPEC-09 Stage 3 (GPU). |

---

## 8. SPEC-06 — DBSP incremental maintenance

**Crate:** `horndb-incremental` · **Spec:** `SPEC-06` · **Overall status: implemented; rule-path and closure-path retraction (F6) landed**

Maintains the materialized closure under updates using DBSP / Z-set
semantics. Insertion is fully incremental. **Rule-path retraction is now
delta-incremental too** (`SPEC-24` S1,
[#210](https://github.com/sunstoneinstitute/horndb/issues/210)): a tick with
retractions runs a two-phase overdelete / re-derive (DRed-style) fixpoint
driven by per-row per-rule one-step weight traces, with an incremental
distinct at the fixpoint boundary; the old recompute-and-diff
([#45](https://github.com/sunstoneinstitute/horndb/issues/45)) survives only
as a config-gated fallback and differential-test oracle.
**Closure-path retraction**
([#5](https://github.com/sunstoneinstitute/horndb/issues/5))
withdraws `ClosureInferred` rows whose base support is retracted, via the
deletion half of SPEC-05's incremental closure; the fully delta-incremental
*closure* path is now **implemented** — output-sensitive support-counting
decremental deletion (cost proportional to the closure delta plus the inspected
frontier, with the recompute path retained as a per-instance fallback and
differential oracle) plus exact warm-store seeded retraction via
`seed_base_edges` (`SPEC-24` S2,
[#211](https://github.com/sunstoneinstitute/horndb/issues/211), `PLAN-24-02`).
The change feed now reconciles to per-tick nets and bounds its subscribers
(`SPEC-24` S3, [#212](https://github.com/sunstoneinstitute/horndb/issues/212)).
The circuit is wired behind the SPARQL write funnel: every Update operation
is one `assert`/`retract` batch plus one `tick()`, the engine consumes its own
change feed, and derived rows land in a reserved graph the default union
reads (`SPEC-24` S4,
[#213](https://github.com/sunstoneinstitute/horndb/issues/213);
`crates/sparql/src/exec/circuit.rs`). Rule registration stays a seam for E4.
The rest of the Stage-2 completeness work — the bilinear-join runtime (S7)
and the intra-tick joint fixpoint (S8) — remains **planned** under `SPEC-24`
(epic [#186](https://github.com/sunstoneinstitute/horndb/issues/186)),
decomposed into `TASKS.md` phase tasks
[#216](https://github.com/sunstoneinstitute/horndb/issues/216)–[#217](https://github.com/sunstoneinstitute/horndb/issues/217).
MVCC backing of snapshots (S6) is **implemented** — see the F7 row below —
and so is the durable input log (S5) — see the checkpoint row.

| Component | Status | Notes |
|---|---|---|
| Z-set storage (`(triple, ±1)` multiplicity) | **implemented** | `zset.rs`. |
| Linear rule operator (single-pattern bodies) | **implemented** | `operator.rs`. |
| Bilinear rule operator (two-pattern bodies) | **implemented** | `operator.rs`, `circuit.rs`. |
| Change feed (`(triple, mult, time, derivation_kind)`) | **implemented (net-delta + bounded)** | `change_feed.rs`, `circuit.rs`. **`SPEC-24` S3** ([#212](https://github.com/sunstoneinstitute/horndb/issues/212)): derived emissions accumulate in a tick-local Z-set keyed by `(triple, kind)` (`Circuit::pending_derived`, fed by the single `emit_derived` funnel) and only non-zero nets publish at tick end, in key order; `TickReport::derived_merged` counts those net records. A same-tick closure withdraw + re-add (the replacement-path case) therefore never reaches a subscriber — `tests/closure_retraction.rs::mixed_tick_replacement_path_final_state_correct` asserts its absence. Asserted records keep per-record publish semantics, in the user's order. Subscribers: `subscribe()` is unbounded (explicit opt-out), `subscribe_bounded(capacity, LagPolicy)` bounds the buffer — `LagPolicy::DisconnectSlow` (default) drops the lagging subscriber and counts it on `incremental_change_feed_dropped_subscribers`, `LagPolicy::Block` backpressures the tick. `Circuit::{subscribe_bounded, subscriber_count}` expose this to engine consumers (S4). Tests: `tests/change_feed.rs` (bounded-buffer drop, fast subscriber unaffected, `Block` under a capacity-1 channel delivering 1000 records with no gaps or duplicates, circuit-level bounded subscriber). |
| Checkpoint merge (collapse ±1 pairs) | **implemented** | `checkpoint.rs`, `delta_log.rs`. `Circuit::tick` advances the asserted base by `Checkpoint::merge` over exactly the batch it drained, so `Zset::add`'s zero-row pruning is the F8 collapse. |
| Durable input log + checkpoint scheduling (F8) | **implemented (contract; format is SPEC-02 Stage 2)** | `SPEC-24` S5 ([#214](https://github.com/sunstoneinstitute/horndb/issues/214), ADR-0018, HDB-52). One physical log, typed records: `Circuit::attach_input_log(store)` makes every `assert_triple` / `retract_triple` an `Input` record in the store's SPEC-25 S3 write-ahead log — durable on append under the store's `SyncPolicy` — and every tick writes a `TickCommit` marker for the range it drained, always fsynced, so a completed tick is durable under any policy. `Circuit::recover()` replays what `Store::open` found: each batch a marker closed replays as its own tick (tick grouping is part of the state — `[+a, -a]` in one tick derives nothing, split across two it derives then withdraws), and the un-ticked tail comes back pending. Attaching the log arms the F8 cadence (`CheckpointPolicy`, default 1 minute or 100K deltas, whichever first, overridable with `set_checkpoint_policy`): the scheduler runs at the end of every tick and, when due, `Circuit::checkpoint()` drains any pending input through a tick, persists the attached store and truncates the log by rolling its generation. Tests: `crates/incremental/tests/wal_recovery.rs` — `kill_and_replay_reproduces_pre_crash_zset` (SPEC-24 acceptance 5), checkpoint truncation, cadence firing. **Residual:** the SPEC-24 S4 engine wiring does not attach an input log yet — its base changes are already logged by the storage write path — so this seam is exercised by a directly driven circuit until ADR-0018's one-tick-one-storage-batch invariant lands on the engine write path (HDB-151). Restoring the base from a checkpoint rather than from the log is SPEC-30 P2 (rebuild-from-zero, HDB-157). |
| Retraction semantics (F6) | **implemented (delta-incremental)** | `SPEC-24` S1 ([#210](https://github.com/sunstoneinstitute/horndb/issues/210), `PLAN-24-01`): `Circuit::tick()` runs one unified incremental fixpoint; a tick with retractions runs a two-phase overdelete / re-derive (DRed-style) pass driven by per-row per-rule one-step weight traces (`rule_weights`) with an incremental distinct, publishing net rule events. Order-independent and correct for arbitrary `(triple, ±k)`. The Stage-1 recompute-and-diff ([#45](https://github.com/sunstoneinstitute/horndb/issues/45)) survives as a config-gated fallback (`Circuit::new_with_recompute_fallback()`) and as the differential-test oracle. Tests: `tests/retraction.rs` (acceptance #3 — insert 10K / retract 10K bit-identical), `tests/acceptance_differential.rs` (multiplicity equality over interleaved insert+retract, pinned against the oracle), `tests/incremental_rule_retraction.rs`. Bench: `benches/retraction_throughput.rs` (incremental vs fallback A/B). Closure-path retraction landed earlier ([#5](https://github.com/sunstoneinstitute/horndb/issues/5)); the delta-incremental *closure* path is now **implemented** — output-sensitive support-counting deletion with a retained recompute fallback, plus exact warm-store seeded retraction (`seed_base_edges`) (`SPEC-24` S2, [#211](https://github.com/sunstoneinstitute/horndb/issues/211), `PLAN-24-02`). |
| Closure-operator deltas (F5) | **implemented (insertion + retraction)** | `closure_plan.rs` (`ClosureRule` / `TransitiveClosureRule`) + `circuit.rs` (`add_closure_plan`, closure pass): wraps SPEC-05's `IncrementalClosureBackend` ([#42](https://github.com/sunstoneinstitute/horndb/issues/42)), folds the asserted insertion delta into the retained per-predicate closure, emits only newly inferred triples tagged `ClosureInferred`. Differential proptest vs full recompute (`tests/closure_deltas_differential.rs`) ([#44](https://github.com/sunstoneinstitute/horndb/issues/44)). **Closure-path retraction** ([#5](https://github.com/sunstoneinstitute/horndb/issues/5)): `ClosureRule::apply_retract_delta` consumes the negative-only delta and `Circuit::tick` runs it before the rule recompute on retraction ticks, withdrawing a `ClosureInferred` row whose base support is gone (publishing a negative `ClosureInferred`) while preserving rows still rule-owned or otherwise supported (`tests/closure_retraction.rs`, updated `tests/retraction_closure.rs`). |
| MVCC for in-flight reads (F7) | **implemented (storage-backed)** | `SPEC-24` S6 ([#215](https://github.com/sunstoneinstitute/horndb/issues/215), ADR-0018). `Circuit::attach_store(store, graphs)` binds the circuit's reader view to the store the SPEC-24 S4 wiring writes — the default graph (asserted rows) plus the derived-mirror graph — and `Circuit::snapshot()` returns `Some(Snapshot)` pinning that store's current commit version. Acquire is O(1): an `Arc` clone plus a tier pin-count bump, with **no presence set materialized in the circuit** (the old lazily-cached `(asserted ∪ derived)` Z-set and its O(n) first-acquire rebuild are gone). `contains()` is a per-tuple visibility check (`PredicatePartition::contains_at`, O(log rows)); `iter()` merges the view's graphs in storage key order (predicate ascending, then subject/object) and dedupes, materializing one predicate at a time. **One clock:** `Snapshot::logical_time()` *is* the storage commit version, so "snapshot at t" means the same thing in both layers with no mapping (ADR-0018); the circuit's per-record `LogicalTime` survives only as a change-feed/diagnostic counter. A circuit with no store attached returns `None` — the in-memory-only shape used by unit tests and benches has no reader view. Tests: `crates/incremental/tests/snapshot.rs` (cross-tick pinning, retraction invisible to an earlier pin, overlapping independence, derived-row pinning, key order, `logical_time()` == store commit version, concurrent reader/writer stability). **Residual:** an Update still commits its base batch and its derived mirror as *two* storage versions (`crates/sparql/src/exec/horn.rs`), so a snapshot taken between them sees base rows without their consequences; ADR-0018's "one tick, one storage batch" invariant is not yet enforced on the engine write path. |
| Distributed timely-dataflow | **deferred** | SPEC-09, Stage 3. |

---

## 9. SPEC-07 — SPARQL 1.1 frontend

**Crate:** `horndb-sparql` · **Spec:** `SPEC-07` · **Overall status: implemented (epic #7 closed)** — the SPARQL 1.1 query/update surface is delivered (SELECT/ASK/CONSTRUCT/DESCRIBE, full expression + aggregation surface, all property-path operators incl. recursive, pattern + graph-management Update on real storage, EXPLAIN). Remaining sub-features are Stage-2 and tracked as **deferred** rows below (backward-chaining, streaming, Turtle CONSTRUCT/DESCRIBE output); the Graph Store Protocol landed with SPEC-28 phase 5. Full W3C conformance (acceptance #1/#2) gates on the harness epic ([#10](https://github.com/sunstoneinstitute/horndb/issues/10)), not on more frontend features.

The public query surface. Parser → algebra → planner → runtime, with an axum
HTTP server (`server` feature, on by default).

| Component | Status | Notes |
|---|---|---|
| Parser (spargebra) → AST | **implemented** | `parser.rs`. |
| Algebra translation (BGP, Join, LeftJoin, Filter, Project, Distinct, Slice, OrderBy, Union, Extend, Values) | **implemented** | `algebra/translate.rs`. All 14 runtime operator impls run native on id-carrying slot rows (`Slot`/`Row`/`Batch`) after Slice 2 of [#128](https://github.com/sunstoneinstitute/horndb/issues/128). `Join`/`LeftJoin` (`OPTIONAL`) are hash joins that now **stream their probe (left) side** ([#128](https://github.com/sunstoneinstitute/horndb/issues/128), `docs/specs/SPEC-20-join-probe-streaming.md`): the build (right) side is drained once into a `JoinState` index, the probe side is pulled chunk-by-chunk (`exec/runtime.rs` `probe_join_chunk`/`probe_left_join_chunk`), replacing the earlier drain-both `compute_join`/`compute_left_join` — ~linear in the common case (was a quadratic nested loop pre-#116/#141). Join keys are selected from the build side's actually-*bound* columns (`bound_join_vars`, replacing the schema-intersection `batch_join_vars`) so an all-unbound shared variable no longer degrades the probe toward O(\|l\|·\|r\|). The `JoinState` index (and every hot runtime hash table — GROUP BY and DISTINCT sets) uses `rustc-hash` (FxHash), matching owlrl/closure. `row_join_key` **canonicalizes** each key column to its dictionary id (`row_join_key`): `Slot::Id` keys on its raw id with no decode, `Slot::Term` is encoded back to its id via `Executor::encode_term` when the dictionary holds it (else keyed lexically), so an `Id` row and a `Term` row for the same value share a bucket without the old decode-both-sides-to-`String` key ([#128](https://github.com/sunstoneinstitute/horndb/issues/128) lever 3). `merge_rows_with` applies the slot compatibility rule (per-join column-index lookups hoisted to a once-per-join `build_merge_plan`); a required `Op::may_emit_term` static provenance claim + per-column `force_term_columns` preserve the stream-wide no-Id∧Term-mix invariant that `normalize_columns` (still used by `Union`, which drains both children) relied on for whole-batch joins. |
| Aggregation / `GROUP BY` (`COUNT`/`SUM`/`MIN`/`MAX`/`AVG`/`SAMPLE`/`GROUP_CONCAT`, `DISTINCT` modifiers) | **implemented** | `algebra/translate.rs` + `exec/runtime.rs::eval_group_native`. Unblocks the LDBC SPB aggregation mix (incl. the driver's `COUNT` warm-up query). #66. **Perf ([#128](https://github.com/sunstoneinstitute/horndb/issues/128) — Slice 1 + Slice 2 landed):** `eval_group_native` keys groups on raw-id `KeyPart`s (no per-row `TermId → String` decode); `COUNT(*)` is `members.len()` (zero column access); value aggregates decode the union of all aggregates' referenced columns once per group via `decode_subset`; the per-group key-slot row is moved, not cloned (#167). `DISTINCT` dedup hashes on `Vec<KeyPart>`. The group map, the DISTINCT operator's seen-set, and the per-aggregate `COUNT(DISTINCT …)`/`dedup_terms` sets all use `rustc-hash` (FxHash) rather than std's SipHash ([#128](https://github.com/sunstoneinstitute/horndb/issues/128) lever 3). Probe-side join streaming + the bound-key probe fix **landed** (2026-07-06, `docs/plans/PLAN-20-01-join-probe-streaming.md`). All #128 follow-on increments have since landed: count-pushdown extensions — equality-filter inlining, grouped COUNT (`Executor::count_bgp_grouped` + `GroupCountScan`), multi-count (`docs/specs/SPEC-21-count-pushdown-extensions.md`, `implemented`); HTTP result streaming (`docs/specs/SPEC-22-http-streaming-results.md`, `implemented`). **HDB-100 fast paths for the shapes SPEC-21 pushdown declines** (mixed COUNT+value aggregates, `COUNT(DISTINCT …)`): `eval_group_native` still runs the general path for these (SPEC-21's decision not to push them down stands — it is about *where* the count happens, not about how slow the fallback is allowed to be), but the fallback itself is no longer a per-row string round trip. `Executor::decode_numeric(TermId)` reads the dictionary's stored `oxrdf::Literal` value in place under one lock (no `Term` clone/`to_string`/unescape/reparse); `Executor::decode_terms` batches `decode_term` through one lock. Per aggregate, when its inner expression is a bare scan-column variable, `eval_group_native` folds `COUNT`/`COUNT(DISTINCT)`/`SUM`/`AVG`/`MIN`/`MAX` off raw slots with this seam instead of decoding the group into a `Bindings` — `COUNT` needs no decode at all, `SUM`/`AVG` fold via `decode_numeric`, `MIN`/`MAX` stay numeric-only (recovering the winning row's *original* term) unless a non-numeric value forces the lexical fallback that matches the general path exactly; aggregates without a fast path (`GROUP_CONCAT`, `SAMPLE`, a computed expression, a `DISTINCT` `SUM`/`AVG`/`MIN`/`MAX`) keep the general decode, coexisting in the same query. A single-column `GROUP BY` over an id-keyed column groups on a raw `u64`/`Option<u64>` instead of a `Vec<KeyPart>` per row (`Option` covers a column mixing bound ids with `Unbound`, e.g. grouping on an `OPTIONAL` variable); the group map gets a capped reserve rather than one sized from `Executor::cardinality_estimate` — that estimates the underlying BGP's row count, not the distinct group count, and for exactly this shape (many rows folding into few groups) would over-allocate by orders of magnitude. trainmarks xlarge q2/q4 (mixed-aggregate/`COUNT DISTINCT` shapes, ineligible for SPEC-21 pushdown): **−40%/−47%** (`docs/benchmarks.md`). Deferred with reasons (permanent non-goals, not open work): pushing these shapes into a count-only seam (`GroupCountScan`) itself, non-equality filters. SPB-256 `aggregation-qps` progression and current gap vs GraphDB Free: `docs/benchmarks.md`. |
| `FILTER`/`BIND` expression coverage | **implemented (Stage-1 surface)** | Comparisons (incl. `<=`/`>=`), `IN`/`NOT IN`, boolean connectives, arithmetic, `IF`, `COALESCE`, and 30 builtins (string/regex/numeric/type-check/datetime accessors) — `algebra/mod.rs::Func`, `exec/runtime.rs::eval_func`. Comparison and ordering still use the best-effort f64 lexical model; **arithmetic, the rounding builtins and the numeric aggregates do not** (HDB-131). They run on `exec/numeric.rs::Numeric`, one value type per XSD numeric datatype (integer / decimal / float / double) implementing the SPARQL 1.1 §17.4.1 operator mapping: operands promote up that lattice, `integer / integer` yields `xsd:decimal`, `xsd:decimal` is exact fixed point rather than f64 (`SUM` of decimals gives `11.1`, not `11.100000000000001`), `CEIL`/`FLOOR`/`ROUND`/`ABS` return the argument's own type, results render in XSD canonical lexical form (`2.0E-1`), and a non-numeric operand is a type error that leaves the variable unbound instead of being silently coerced or skipped. Backed by `oxsdatatypes`, already in the tree as oxrdf's dependency. The inline-integer aggregate fast path is unchanged — an inline-int `TermId` still folds arithmetically with no dictionary lock. `EXISTS`, non-deterministic builtins (`RAND`/`NOW`/`UUID`/…), hashing, `STRLANG`/`STRDT`, and custom functions still return `UnsupportedAlgebra`. #66. |
| `GRAPH` named-graph patterns + dataset clause (`FROM`/`FROM NAMED`) | **implemented, except two families of `GRAPH ?g` query (SPEC-28 phase 3)** | Earlier statuses — "implemented (Stage-1 merged-graph)", "broken — returns wrong answers", "refused (explicit 400)" — are all retired. `translate.rs` builds `Algebra::Graph`; `plan/lower.rs` pushes the scope onto every scan leaf under it (`GraphScope`, innermost `GRAPH` wins), so a ground `GRAPH <g>` scans only `g` and an unknown graph IRI gives zero rows, never an error. `GRAPH ?g` binds the graph as a **scan output column** (SPEC-28 D6 — one scan node, plan size independent of graph count) and never binds the default graph. `FROM`/`FROM NAMED` build a `DatasetSpec`, including `FROM NAMED` with no `FROM` = empty default graph. With no dataset clause the default graph follows `[server.limits].default_graph`: `union` (default) = every non-reserved graph, deduped; `strict` = the default-graph sentinel only. Graphs under `https://horndb.io/graph/` are reserved: out of the union and out of `GRAPH ?g` enumeration unless named explicitly. Property paths inherit the scope *before* the closure runs. Count/group-count pushdowns are scope-aware or decline (`Ok(None)`), so no shortcut can answer a scoped query with a whole-store count; estimates stay coarse and never reach a result. **Still refused** (`SparqlError::UnsupportedAlgebra` → HTTP 400, never a wrong answer): (1) a barrier between the `GRAPH ?g` wrapper and its scan leaves — a sub-`SELECT`, `DISTINCT`, `GROUP BY`, `LIMIT`, a property path, a nested `GRAPH`, or a `VALUES` that is not joined against a scoped arm — which would drop or merge the graph column; (2) a block that **reads** `?g` where leaf-binding diverges from SPARQL 1.1 §18.2.2.2's post-join — any expression (`FILTER`, `BIND`, an `OPTIONAL` condition, `ORDER BY`), `BIND(… AS ?g)`, or `?g` in an `OPTIONAL`'s right arm. What works: `?g` in a triple position; and a quad-free arm joined against a scoped one, so `GRAPH ?g { ?s ?p ?o VALUES ?o { … } }` answers. Lifting either needs per-graph evaluation of the whole block, a design change against D5/D6 — **specified** by SPEC-28's S3 amendment (HDB-171): one `PerGraph` plan node evaluates the block once per graph with `?g` free and joins the graph name on afterwards, plan size still independent of graph count; implementation is HDB-74 (**planned**), which lifts both families at once and turns W3C `graph-variable-scope`, `graph-optional` and HDB-135's six `sparql11-eval` cases green. Conformance: W3C `graph/` + `dataset/` families, 24 of 29 cases selected and green on both backends (`harness/selected.toml` `[sparql_query]`); the other 5 in `harness/KNOWN-MANIFEST-BUGS.md`. See `crates/sparql/INTEGRATION-NOTES.md`. ([#266](https://github.com/sunstoneinstitute/horndb/issues/266); [#261](https://github.com/sunstoneinstitute/horndb/issues/261) parent epic; was #66/#7.) |
| `MINUS` | **implemented** | `translate.rs` lowers `MINUS` to `Algebra::Minus`, an anti-join (SPARQL 1.1 §18.5) run by `MinusOp` / `Runtime::probe_minus_chunk`, reusing the streaming hash-join machinery (`JoinState`) rather than a parallel path. Output schema is `left`'s alone. A left row is dropped only when some right row is both compatible (agrees on every shared bound variable) **and** shares at least one actually-bound variable with it; the disjoint-variable case (no shared columns at all) is a fast path that keeps every left row unchanged — this is deliberately *not* the same as `FILTER NOT EXISTS`, which has no domain-intersection requirement. Fixes 3 of the `negation/` suite's 7 cases (HDB-133); the other 4 remain blocked on the separate `FILTER NOT EXISTS`-as-expression gap (`EXISTS` row in `FILTER`/`BIND` expression coverage, above). Part of the SPEC-07 umbrella (#7). |
| Planner + runtime executor | **implemented** | `plan/`, `exec/`. BGPs route to `exec/horn.rs::HornBackend`, which executes on `horndb-storage` (kind-tagged dictionary `TermId`s — fixes the Stage-1 lexical type-erasure/IRI-coercion) via the `horndb-wcoj` Leapfrog Triejoin (binary-hash for ≤3 patterns; WCOJ via `Planner::default()` for ≥4). `MemStore` (`exec/mem.rs`) is retained as the in-process test double. `DELETE DATA` is handled by a tombstone overlay over the insertion-only storage layer. `load_with_reasoning` (`reasoner` feature, default-on) runs the `horndb-owlrl` Engine (RuleFiring backend) and loads the full materialized closure directly into the backend, replacing the earlier dump-to-flat-file round trip. The `serve` binary accepts `--materialize` to trigger this path. **HDB-117:** the closure crosses that boundary as **ids, not strings** — `Engine::materialized_triple_ids` + `Engine::dictionary_entries` feed `HornBackend::load_id_closure`, which interns once per engine dictionary entry and remaps the closure ids (HDB-87's "intern once", applied to the reasoning path) instead of decoding, re-parsing and re-interning three strings per closure triple. `Engine::materialized_triples` stays for tests, `bench-rdfox` and dump tools. (#67) **Perf ([#128](https://github.com/sunstoneinstitute/horndb/issues/128) — Slice 1 + Slice 2 landed):** `scan_bgp_ids` (`exec/horn.rs`) feeds the runtime id-carrying slot rows (`Slot`/`Row`/`Batch`) straight from the WCOJ `UInt64Array` columns — the dictionary is no longer defeated at this seam. `Runtime::run` decodes once at the boundary via `decode_term`. **All 13 operators are now native on slot rows** (Slice 2 ported the last six — LeftJoin, Union, OrderBy, Extend, Values, PathClosure); the `from_bindings`/`to_bindings` decode-adapter (`eval_rows`) and the `cfg(test)` `eval_legacy` differential oracle are removed — one slot runtime. Value-needing operators (`FILTER`/`ORDER BY`/`BIND`/aggregates) decode only their referenced columns on demand via `referenced_vars` + `decode_subset`; `ORDER BY`/`MIN`/`MAX`/relational comparisons always resolve a value (ids are insertion-ordered, not value-ordered), though `ORDER BY` no longer always builds a `Term` for it — see the HDB-101 note below. The string `scan_bgp` is retained only as the default for non-`HornBackend` executors (DESCRIBE still adapts through it). **#143 Streaming pull-based runtime IMPLEMENTED** (2026-06-30): the runtime is now a pull-based, batch-at-a-time operator tree (`crates/sparql/src/exec/op/`); every Op is native; legacy materializing `eval` deleted; chunk-boundary invariance tested. **#144 Column pruning IMPLEMENTED** (`plan/pushdown.rs`). **#144 COUNT-over-BGP aggregate pushdown IMPLEMENTED** (`Executor::count_bgp` + `CountScan` + `CountScanOp`). **Join probe-side streaming + bound-key join-variable selection LANDED** (2026-07-06, `docs/plans/PLAN-20-01-join-probe-streaming.md`): `JoinOp`/`LeftJoinOp` drain only their build side and stream the probe side chunk-by-chunk; join keys come from `bound_join_vars`. **Count-pushdown extensions IMPLEMENTED** (`docs/plans/PLAN-21-01-count-pushdown-extensions.md`, executed): equality-filter inlining, grouped COUNT via `Executor::count_bgp_grouped` + `GroupCountScan`, and multi-count lowering. **HTTP result streaming IMPLEMENTED** (`docs/plans/PLAN-22-01-http-streaming-results.md`, executed): `Runtime::run_stream` + `ChannelBody` chunked bodies for all four SELECT formats. **HDB-101 `ORDER BY`: sort keys resolved once per row, and `LIMIT` fused as top-k.** The comparator used to resolve each row's value inside itself — re-evaluating the key expression and re-parsing its lexical form on both sides of every comparison — so an n-row sort paid O(n log n) decodes for O(n) work. `compute_order_by` now resolves each key once per row into a typed `SortCol` (`exec/runtime.rs`); a bare batch-column key reads the slots through HDB-100's `decode_numeric`/`decode_terms` seam and never builds a `Bindings`. `Runtime::build_top_k` additionally fuses a bounded `LIMIT` into the sort (`OrderByOp::top_k`, a max-heap of `offset + limit` rows) for `Slice(OrderBy(..))` and `Slice(Project(OrderBy(..)))` — the shape SPARQL algebra §18.2.5 produces — and refuses every other shape, since anything else between the sort and the slice drops rows after it. Row order is unchanged: the heap orders on `(key, input position)`, matching the stable sort's first `n` rows including ties, and it is skipped entirely when a key column's comparison is not a strict total order (mixed numeric/non-numeric, or a NaN). q3 before/after and its phase split: `docs/benchmarks.md`. SPB-256 numbers: `docs/benchmarks.md`. |
| Query triple source (what a BGP reads) | **implemented, opt-in — not yet the default** (HDB-120) | With `HORNDB_DIRECT_SOURCE=1`, single-graph queries read `horndb-storage`'s columnar partitions **directly** — `exec/store_source.rs::StoreTripleSource` over `horndb_wcoj::source::merged::MergedIter` — instead of building a `VecTripleSource` copy of the scope. Within one predicate partition the predicate is constant, so the stored `(s, o)` / `(o, s)` columns are already in every ordering's component order and only the depth of the constant term moves (`Pso|Pos` → 0, `Spo|Ops` → 1, `Sop|Osp` → 2); `MergedIter` merges the per-predicate blocks into a global trie ordering on the fly. Columns arrive as `Arc` clones of the stored Arrow buffers, so an insert-only scope copies nothing, and the SPEC-25 S1 copy-on-write snapshot already gives a stable view (`StoreSnapshot::tier_arc`), so no extra locking. **Fallbacks that still build the copy:** a genuine multi-graph union (a `FROM` list, or a `DefaultUnion` over more than one non-reserved graph) — `MergedIter` needs distinct leaf keys, which one graph's predicates give and a union does not — and `EXPLAIN`, whose `cardinality_estimate` reads `SnapshotStats::from_source` off `VecTripleSource::sorted_columns`. Known costs, both measured on `hornbench` before this is called a win: each level operation is a linear pass over the live leaves rather than one binary search over a flat column, and `OrderedTripleIter::active_run` (the k==2 SIMD-intersect fast path) is not implemented for `MergedIter`. **Why it is opt-in:** correct but not yet faster. `crates/sparql/tests/direct_source_parity.rs` runs one query battery against two backends differing only in `set_direct_source`, including after inserts and retractions, and they agree; and the **hornbench A/B has now run** (HDB-144, `docs/benchmarks.md`): warm reads are **1.16-6.14x slower** on trainmarks xlarge, LDBC SPB-256 `aggregation-qps` is **4.13x slower** (56.60 -> 13.71 qps), and the load path is unaffected. The footprint saving is **27.0%** of process RSS (2,982 -> 2,176 MiB on trainmarks xlarge, re-measured at HDB-158; the 8.2% first reported at HDB-144 was diluted by the driver's own whole-corpus parse buffer, which dominated RSS in both legs). **Neither gate is met, so the default stays off.** The gap is the merged cursor's inner loop, not source construction — `HornBackend::direct_cache` already keeps one built source per (tier version, graph), since `PredicatePartition::ordered_at` materializes the visible subset per predicate whenever the partition holds retractions. The follow-up is a single-live-leaf specialization plus `active_run` (HDB-145); re-run the A/B after that fix before revisiting the default. |
| SELECT / CONSTRUCT / ASK | **implemented** | Result formats in `results/`. |
| Entailment regimes: OWL 2 RL/RDF + simple | **implemented** | `regime/owl_rl.rs`, `regime/simple.rs` (materialized mode). |
| SPARQL Update `INSERT/DELETE DATA` | **implemented** | `update.rs`: every quad in the request — inside a `GRAPH <g> { … }` block or the bare default-graph form — is grouped into **one** `apply_quads` call per operation (implemented, SPEC-28 phase 4, [#267](https://github.com/sunstoneinstitute/horndb/issues/267), `PLAN-28-04`). `apply_quads` (SPEC-28 S6) commits deletions-then-insertions at one store version and is idempotent at quad grain — inserting an already-live quad or retracting an absent one is a counted no-op, never an error or a version bump — with lexical (not value) literal identity, the property an at-least-once change feed needs to replay safely. |
| Pattern-based Update (`INSERT`/`DELETE … WHERE`, `DELETE WHERE`, `WITH/DELETE/INSERT … WHERE`) | **implemented** | `update.rs::apply_delete_insert`: evaluates the WHERE pattern via `translate_where` → planner → runtime, collects all solutions over the pre-update graph, then applies deletions-before-insertions (SPARQL 1.1 §3.1.3) through the `Store` seam. Ground-template safety drops triples with unbound slots; a template blank node is scoped to both the solution row **and** the operation (a per-operation tag from `Store::next_bnode_doc_tag`), so `_:b` written by two operations of one request is two distinct nodes (SPARQL 1.1 §4.1.4; W3C `insert-where-same-bnode`, HDB-137). **Named-graph templates and `WITH`/`USING`/`USING NAMED` work** (implemented, SPEC-28 phase 4, [#267](https://github.com/sunstoneinstitute/horndb/issues/267), `PLAN-28-04`): each DELETE/INSERT template quad routes to its own graph (default / named / a WHERE-bound variable, via `resolve_graph_name`); `WITH`/`USING` scope the WHERE clause through the phase-3 `DatasetSpec` machinery (`dataset_spec_from`); a bare `WITH <g>` sets the *default* graph only, so a ground `GRAPH <other>` inside WHERE still reads `<other>` (SPARQL 1.1 Update §3.1.2 — spargebra encodes WITH-only as `named: None`, which `apply_delete_insert` keeps unrestricted instead of collapsing to the query-side "`FROM` without `FROM NAMED` ⇒ no named graphs" rule; W3C `delete-with-02`/`-06`, [#281](https://github.com/sunstoneinstitute/horndb/issues/281)). A multi-op sequence applies **one store batch per operation**, in request order, never collapsed. ([#51](https://github.com/sunstoneinstitute/horndb/issues/51)) |
| Embedded HTTP server (`/query`, `/update`) | **implemented** | `server/` (axum), behind `server` feature. One `RwLock` over the backend, but **`/query` holds it only long enough to pin a read view** (HDB-119, `exec::Pinnable::pin_read`): execution and streaming run lock-free, so a slow client cannot block `/update` and a queued writer cannot block new readers. Writes still take the write lock, on the blocking pool. |
| `/healthz`, `/readyz`, graceful shutdown, request ids (HDB-124) | **implemented** | `server/health.rs` (`GET /healthz` always `200`; `GET /readyz` reflects `AppState.ready`, an `Arc<AtomicBool>` flipped once at the end of the startup load in `bin/serve.rs::run_load` — `503` for the whole, potentially multi-minute, cold-load window). `bin/serve.rs::main` binds the listener and starts `axum::serve` **before** that load (spawned onto a `spawn_blocking` thread), which is what lets the probes answer while data is still loading. SIGTERM/SIGINT stop new connections (`with_graceful_shutdown`) and give in-flight requests up to `[server].shutdown_drain` (new SPEC-26 `[server]` key, default `30s`, `--shutdown-drain` / `HORNDB_SERVER__SHUTDOWN_DRAIN`) to finish, enforced by racing the server task against a `tokio::time::timeout`; past the deadline the process force-exits. Every response carries `x-request-id` (`server/request_id.rs`: passthrough or `<pid>-<seq>` generated, no new crate) and the same id lands in the `record_request` middleware's `eprintln!` access-log line. Tests: `server/health.rs` unit tests (ready-flag → status code), `tests/serve_config_wiring.rs::healthz_readyz_and_sigterm_drain` (real subprocess: readyz flips, `x-request-id` on the wire, SIGTERM drains and exits 0). |
| Admission control on `/query` + request-body cap (HDB-118) | **implemented** | `server/mod.rs::Limits`: a `tokio::sync::Semaphore` bounds concurrent query execution to `[server.limits].max_concurrent_queries` (default: host core count); a request that waits longer than `[server.limits].queue_timeout` (default 5 s) is shed with HTTP 503 + `Retry-After`. The permit is **held for the whole streamed response**, not just plan+first-chunk — the blocking task owns a blocking-pool thread, a pinned read view (HDB-119 — no longer the store read guard) and the operator tree until the client finishes draining, so releasing earlier would cap nothing. `axum::extract::DefaultBodyLimit` caps the `/query` and `/update` request body at `[server.limits].max_request_body` (default 4 MiB); `/metrics` is registered after the layer and keeps axum's default, and `LOAD` reads files rather than request bodies so bulk ingest is unaffected. Observability: `horndb_sparql_queries_in_flight` (gauge) and `horndb_sparql_queries_rejected_total` (counter) — see `docs/metrics.md`. Before this, every request went straight to `spawn_blocking` (default cap 512 threads) with no queue and no body limit. |
| trainmarks (DataTreehouse) end-to-end benchmark | **implemented** | All six trainmarks queries complete on `HornBackend` at all three scales (100K/1M/10M), no timeouts — `hornbench` baseline 2026-07-06 in `docs/benchmarks.md`. Native driver `crates/bench-trainmarks` + `scripts/bench/trainmarks.sh`. The original q4 `OPTIONAL` cliff (~231s@1M / TIMEOUT@10M) was removed by the hash `LeftJoin` ([#116](https://github.com/sunstoneinstitute/horndb/issues/116), [#128](https://github.com/sunstoneinstitute/horndb/issues/128) Slice 2): q4 now 0.334s@1M / 6.80s@10M. q6 (`DELETE`/`INSERT … WHERE`) no longer re-indexes the store on every write (HDB-82, `PLAN-03-03`): `HornBackend` merges a small quad delta into its memoised `VecTripleSource` in place instead of dropping the sorted orderings, falling back to the full rebuild when the merge is not provably correct or not profitable. HDB-97 then made the orderings themselves lazy — `VecTripleSource` materialises only the `Pso` anchor (the order storage already scans in) and derives the other five on first use, so a snapshot indexes what queries read rather than all six: `hornbench` xlarge q6 cold 5.04s → 1.86s, q6 warm 1.31s → 0.52s, at the cost of +0.6s on q3 cold, the suite's only `Pos` reader. HDB-98 then made that derive cheaper: `Pso` and `Pos` are both predicate-major, so `Pos` is built by sorting each predicate block on its own (`TripleColumns::derive_blockwise`, O(n log(n/b)) for b blocks) rather than globally sorting all n rows — `hornbench` xlarge q3 cold 1.665s → 1.500s, its derive component 484ms → 363ms. Orderings that do not share the anchor's level-0 axis (`Spo`, `Sop`, `Osp`, `Ops`) still take a global sort. Local M4 before/after, warm q6: 0.0151s → 0.0032s @98K, 0.194s → 0.034s @1M, with q1–q5 and the I/O rows unchanged. A `hornbench` run at 10M is the outstanding follow-up — `docs/benchmarks.md` still carries the pre-HDB-82 baseline. **HDB-158**: the driver now flushes its parse batch every 65,536 triples (`loader::load_batch_triples`, the granularity `Store::load_*_file` inserts at) instead of materialising the whole corpus into one `Vec` of owned `oxrdf` terms. On trainmarks xlarge that halves the process footprint (5,833 -> 2,982 MiB) and makes `read_turtle` 7.4% faster, and it is what makes the `serving footprint` row a serving-side number rather than a load high-water mark. The `--reserve-triples` flag went with it. |
| RDF 1.2 triple-term patterns `<<( s p o )>>` | **implemented (gated)** | Accepted only when the caller's `SparqlConfig.rdf12` is `true`: library callers pass `SparqlConfig::rdf12()` directly; over HTTP it comes from `[server.limits].rdf12` (`PLAN-28-03` Task 2) and can be flipped per request with `?rdf12=true|false` (SPEC-26 S5): `serve.rs` puts the live config handle in `AppState.config` and each handler derives that request's `SparqlConfig` from the `[server.limits]` it snapshots, replacing the previous hardcoded `SparqlConfig::default()`. Default (`rdf12 = false`) keeps SPARQL 1.1 callers on 1.1 semantics. `translate_query_with` / `execute_query_with`. |
| `DESCRIBE` query form | **implemented (partial)** | Forward one-level Concise Bounded Description: `translate.rs` lowers the describe pattern like SELECT, `exec/runtime.rs::describe_triples` emits each resource's outgoing triples. Recursive/symmetric blank-node CBD and typed-literal/Turtle serialisation deferred (Stage-1 `MemStore` erases term types on scan; tracked in [#57]). `TASKS.md` #48. |
| Non-recursive property paths (`/`, `^`, `\|`, `?`, `!`) | **implemented** | `translate.rs::translate_path` lowers them at translation time: `/`(Seq) and `^`(Inverse) expand into triple patterns; `\|`(Alternative) and `?`(ZeroOrOne) lower to `Union` (zero-length `?` binds endpoints without enumerating the graph — two distinct unbound endpoints are rejected as out of Stage-1 scope); `!`(NegatedPropertySet) lowers to a wildcard-predicate BGP under a `NOT IN` filter. A WHERE-pattern blank node (incl. the one spargebra mints when it flattens a sequence) is now treated as a non-distinguished join variable (`match_term`), which also fixes latent `/`-sequence joins across algebra boundaries. Covered by `tests/exec_property_paths.rs` and conformance fixtures `path-{alt,neg,opt}-001` (both backends). ([#49](https://github.com/sunstoneinstitute/horndb/issues/49)) |
| Kleene-star property paths (`*`, `+`) | **implemented** | `translate.rs::translate_closure_path` lowers `+`/`*` to the `Algebra::PathClosure` node (the inner one-step path is expanded over the hidden endpoint vars `?pp_src`/`?pp_dst`, so `(p\|q)+`, `^p+`, `(p/q)+` all work); `runtime.rs::eval_path_closure` materialises the edge relation, takes its transitive closure by BFS to a fixpoint (cycle-safe), and for `*` adds the reflexive pairs over the touched node set, then binds/filters against the query endpoints. Covered by `tests/exec_property_paths.rs` and conformance fixtures `path-{plus,star}-001` (both backends; `path-star-001` is the acceptance-#7 `subClassOf*` shape). **Deferred:** routing a materialised single-predicate closure through the SPEC-05 GraphBLAS backend + selectivity-based planner choice (F3 fast path — correctness ships now, acceleration later); strict full-graph node-set semantics for `*`'s zero-length match over nodes untouched by the path. ([#50](https://github.com/sunstoneinstitute/horndb/issues/50)) |
| Graph-management Update (`LOAD`/`CLEAR`/`DROP`/`CREATE`/`ADD`/`MOVE`/`COPY`) + multi-op updates | **implemented** (SPEC-28 phase 4, [#267](https://github.com/sunstoneinstitute/horndb/issues/267), `PLAN-28-04`) | `update.rs`: parser admits graph-management verbs and multi-operation sequences (`parser::ParsedUpdate::GraphManagement`); the executor walks the op list, one `apply_quads` store batch per operation. Verbs follow SPARQL 1.1 §3.2 / SPEC-28 **D11** existence semantics: `CREATE` on an absent graph is a no-op, on an existing graph an error unless `SILENT`; `CLEAR`/`DROP <g>` on an absent graph is an error unless `SILENT`, on a present graph retracts every quad through `clear_graph` (quad-by-quad — **no structural unlink**; a graph exists exactly when it holds ≥1 quad — **D11 settled permanently by HDB-80**: no empty-graph registry, `CLEAR` and `DROP` have the same effect, and the three W3C `clear/` cases that test empty-graph existence are excluded for good, see SPEC-28 S4's settlement note). `DROP ALL` clears the default graph plus every non-reserved named graph (via `graphs()`); `CLEAR`/`DROP NAMED` sweeps the non-reserved named graphs only — the reserved namespace (below) is never swept. `LOAD <src> [INTO GRAPH …]` routes by format: triples formats (`.nt`/`.ttl`/default) go to the destination (default graph if no `INTO`); dataset formats (`.nq`/`.trig`) under plain `LOAD` route each quad to its own named graph; a dataset format combined with `INTO GRAPH` is a routing error. **The `serve` binary's `--data` startup loader shares this same parser call site** (`update::parse_rdf_bytes`, HDB-112): `collect_data_files` now also collects `.nq`/`.trig`, and each one loads its quads into the named graphs it carries, not just the default graph — closing the gap where a dataset-format catalog (one named graph per dataset) could not be loaded at server start at all, only via `LOAD` over `/update` after boot. `--materialize` (OWL 2 RL closure at startup) does not yet support `.nq`/`.trig` inputs and refuses them up front rather than silently collapsing their named graphs into the default graph. Still `file:`-only — remote `http(s):` LOAD stays deferred (→ E5 [#189](https://github.com/sunstoneinstitute/horndb/issues/189)). `ADD`/`MOVE`/`COPY` execute their spargebra-desugared `Drop`+`DeleteInsert` ops between named graphs, with **`SILENT` fidelity recovered**: spargebra 0.4.6 drops the flag when it desugars these verbs, so `update.rs::recover_amc_hints` re-derives it from the update's raw source text (a hand-rolled tokenizer, not a parser — see `crates/sparql/INTEGRATION-NOTES.md`). It resolves every operand to an absolute IRI against the update's own `PREFIX`/`BASE` prologue (prefixed names via the prefix map, base-relative `<…>` via `oxiri` RFC 3986, seeded by `spargebra::Update::base_iri`), so `(silent, source, is_identity)` are all text-determined for every operand form. The missing-source preflight then reads only those hints — never the desugared ops — so it cannot collide with a user-written `{?s ?p ?o}` `DeleteInsert`, and a prefixed identity op (`ex:g TO ex:g`, zero desugared ops) is recognised and excluded like any other. An identity op alongside a `SILENT` copy no longer false-errors, and a prefixed or base-relative absent source is neither false-rejected nor silently wiped. A source IRI that needs a `\uXXXX`/`PN_LOCAL_ESC` escape can't be reproduced by the raw scan, so a non-silent `ADD`/`MOVE`/`COPY` with such a source **fails closed** (errors before any mutation) rather than resolve a wrong graph — a documented known-limitation; full unescape-parity resolution is future work. The reserved `https://horndb.io/graph/` namespace is closed to every write form (data quads, templates, `CREATE`/`CLEAR`/`DROP`, `LOAD INTO`, `ADD`/`MOVE`/`COPY` destinations) — checked before any `SILENT`/existence logic and **not suppressible by `SILENT`**; reads stay allowed. Preflight (`validate_op`) mirrors every apply-time error against the pre-update store, so a bad multi-op sequence aborts before any op runs. **Multi-op requests are all-or-nothing (SPARQL 1.1 §3.1.3):** preflight reads against the *pre-update* store, so it can only judge D11 graph existence for the **first** operation — every later one runs against a store the earlier ones may have changed, and checking it up front rejected legal requests such as `INSERT { GRAPH <g> … } WHERE { … } ; DROP GRAPH <g>` (HDB-137; W3C `basic-update/insert-05a` and its three variants). Two error families therefore fire at apply time — a later op's D11 existence, and a reserved-graph write through a template graph *variable* (known only once a WHERE row binds it). `update.rs::Journal` covers both: for a request with more than one op it records, just before each write batch, whether every quad that batch is about to touch was visible before this request first touched it (`Store::quad_exists`, a point read on both backends; a `CLEAR`/`DROP` sweep records the graph's quads without a point read), and any failure restores exactly that state in one `apply_quads` batch. Ops still apply in request order against the live store, so a later WHERE clause reads the earlier ops' writes (read-your-own-writes); a working copy would avoid the recording reads, but `Executor::scan_bgp` evaluates a whole BGP join at once, so a pending delta cannot be layered over its results. A single-op request is already one atomic batch and journals nothing. Tests: `tests/update_graph_mgmt.rs` (including `multi_op_failure_rolls_back_earlier_ops_{mem,horn}`), `tests/update_named_graph.rs` (both backends), `tests/server_http.rs`. ([#52](https://github.com/sunstoneinstitute/horndb/issues/52)) |
| Backward-chained entailment mode (F4 second mode) | **planned** | Unified-IR epic E1 leaf issue [#207](https://github.com/sunstoneinstitute/horndb/issues/207) (`SPEC-23` approved); depends on SPEC-03 magic-sets/tabling landing there. |
| `EXPLAIN` pragma (F9) | **implemented** | `parser.rs` recognises a leading non-standard `EXPLAIN` / `EXPLAIN JSON` pragma (case-insensitive, whitespace-delimited so `?explainme` is not mistaken for it), strips it, and wraps the inner query as `ParsedQuery::Explain`. `api::execute_query` translates + plans the wrapped query **without executing it** and renders via `plan::explain` (`QueryAnswer::Explanation`): an indented operator tree (or JSON object tree) carrying a header `mode:` line (the entailment-regime execution mode — `materialized` today, labelled "backward-chaining not yet available" pending [#55](https://github.com/sunstoneinstitute/horndb/issues/55)) and per-node `~N rows` cardinality estimates. Estimates come from `Executor::cardinality_estimate` (new trait method, default `None`; `MemStore` returns the leading-pattern index size, `HornBackend` now returns the **stats-backed point estimate** from the SPEC-23 phase-3 `StatsEstimator` over a recompute-from-snapshot `SnapshotStats`, replacing the earlier live-triple-count upper bound) combined by textbook per-operator rules. The `/query` handler serves the rendering as `text/plain` or `application/json` by pragma. Satisfies acceptance #5 (EXPLAIN on `subClassOf+` shows mode + cardinality). Covered by `tests/explain_pragma.rs`, `tests/parser_basic.rs`, `tests/server_http.rs`, and `plan::explain` unit tests. **Deferred:** "chosen indexes" display (no cost-based index chooser yet — the plan is a 1:1 lowering) and the real materialized-vs-backward mode selection (with [#55](https://github.com/sunstoneinstitute/horndb/issues/55)). ([#53](https://github.com/sunstoneinstitute/horndb/issues/53)) |
| Graph Store Protocol | **implemented** ([#268](https://github.com/sunstoneinstitute/horndb/issues/268)) | `SPEC-28` phase 5, `crates/sparql/src/server/graph_store.rs`. Direct REST access to *named* graphs, behind the same `server` feature as `/query` and `/update`. One route, `/graphs`, selecting a graph with `?graph=<iri>` or `?default`: `GET` serializes it (content negotiation over `text/turtle`, the default, and `application/n-triples`), `PUT` replaces it, `POST` merges into it, `DELETE` empties it. Status codes: **201** the graph held no visible quads before the write, **204** replaced/merged/deleted and for an idempotent no-op, **400** parse error (the parser's own message in the body), unknown query parameter, or neither `graph` nor `default`, **404** `GET`/`DELETE` of a graph with no visible quads, **413** payload over `[server.limits].max_request_body` (the same `DefaultBodyLimit` layer as `/query`+`/update`), **415** unsupported media type *including* the dataset formats TriG and N-Quads, which carry a graph slot the protocol has no room for. **`PUT` is a replace expressed as a diff over base quads only:** the read set is `Store::scan_graph_quads` — asserted quads straight out of storage, no reasoning seam — so `dels = base − payload` and `adds = payload − base` commit in one `apply_quads` batch and a derived quad is never deleted (SPEC-29 D5); an empty diff commits nothing and returns 204. Reserved `https://horndb.io/graph/` graphs are read-only over GSP: `GET` allowed, the three write verbs 400 through Update's own closed-namespace check (`update.rs::reserved_iri_write_check`). `?default` writes are refused on a `--materialize` store (a one-way process-global set by `serve`, like the inconsistency flag) because `load_with_reasoning` puts asserted and inferred triples into the default graph indistinguishably; `GET ?default` stays fine. Blank nodes are request-scoped, so re-`PUT`ting an identical bnode-bearing body deletes and re-inserts every bnode-touching quad — the empty-diff no-op cannot apply there. Deliberate divergences from `graph-server` §5 (SPEC-28 D8): no `/branches/{b}` segment, no `ETag`/`X-Sunstone-Txn`/`If-Match` conditional writes, no `Sunstone-*` headers, no `?wait=materializations`; `?default` is served, not 400. `GET` takes an admission permit like `/query`. Tests: `crates/sparql/tests/graph_store_protocol.rs` and `graph_store_materialized.rs` (own binary — the materialize flag is process-global). **SPEC-28 acceptance criterion 6 is closed:** the `sparql11-gsp` harness suite (HDB-165) grades these routes against the W3C Graph Store Protocol cases on a live server — see the SPEC-01 section. ([#54](https://github.com/sunstoneinstitute/horndb/issues/54), closed — superseded by `SPEC-28`) |
| Named-graph query + update scoping (`GRAPH`, `FROM`/`FROM NAMED`) | **implemented** ([#267](https://github.com/sunstoneinstitute/horndb/issues/267), `PLAN-28-04`) | `SPEC-28` phases 1–4, all delivered. Phase 1 ([#264](https://github.com/sunstoneinstitute/horndb/issues/264)) refused `GRAPH` and the dataset clause instead of answering them wrongly; phase 3 replaced that refusal with real evaluation, so only the two families of `GRAPH ?g` query in the `GRAPH` row above still 400. Phase 2 ([#265](https://github.com/sunstoneinstitute/horndb/issues/265)) supplied the storage plumbing phase 3 reads through (`graph_len`, graph-scoped scan/iteration — see the SPEC-02 section above). Phase 3 ([#266](https://github.com/sunstoneinstitute/horndb/issues/266), `PLAN-28-03`) delivered `Algebra::Graph`, scan-scope lowering, `GRAPH ?g` as a scan column, `DatasetSpec`, the `union`/`strict` default-graph mode with its `[server.limits].default_graph` setting and per-query `default_graph` URL/form override, scope-aware pushdowns, path scoping, and the W3C `graph/` + `dataset/` families in `[sparql_query]`. `MemStore` grew a graph dimension in the same phase — a per-triple set of holding graphs beside the triple table, so every index stays triple-keyed. **Phase 4 ([#267](https://github.com/sunstoneinstitute/horndb/issues/267), `PLAN-28-04`) delivered named-graph Update** on that same plumbing: quad-data and pattern-update routing per graph, `WITH`/`USING`/`USING NAMED`, D11 graph-management existence semantics, recovered `SILENT` fidelity, the reserved namespace closed to writes, and a store-boundary idempotent quad-grain apply (S6, `apply_quads`) so an at-least-once change feed replays safely — see the two Update rows above. `update.rs` no longer rejects named-graph targets. **Phase 5 (Graph Store Protocol) delivered the four `/graphs` routes and their status-code contract** (row above) plus its conformance half, the `sparql11-gsp` suite key and the live-server harness kind (HDB-165), closing SPEC-28 acceptance criterion 6. |
| Named-graph reasoning scope | **implemented (P1)** ([#269](https://github.com/sunstoneinstitute/horndb/issues/269), `PLAN-29-01`; P2–P4 specified) | `SPEC-29`. What OWL 2 RL reasons over in a many-graph store, and where derived triples land. P1 ships the view model and catalog (D1/D2), spine-closes-once factoring (D3), per-view inferred graphs under the reserved `https://horndb.io/graph/` namespace (D4), the source-graph read invariant (D5), the `default_dataset_includes_inferred` flag (D6), quad-shaped input deltas with per-view routing (D7), and the `[reasoning]` config section (D9) — `crates/sparql/src/reasoning/`, behind the `reasoner` feature. `reasoning.enabled = false` (the default) is a no-op path: no engine runs and no reserved graph appears. Derived triples never enter the source graph, so a whole-graph `PUT` diff cannot delete inferences the client never sent. Still open: incremental spine fan-out (P2), provenance attribution (P3), virtual views (P4), and migrating `serve --materialize` onto views. |
| Change-feed materializer (apply, cursor, recovery) | **implemented (P1)** ([#270](https://github.com/sunstoneinstitute/horndb/issues/270), `PLAN-30-01`; P2–P4 specified) | `SPEC-30`. The durability contract HornDB offers a consumer that applies an external `{adds, dels}` feed and holds its own cursor: applied-state guarantees across restart, startup reconciliation against that cursor, and rebuild-from-zero. Exists because SPEC-02 NF5 accepts losing updates between checkpoints (WAL is `SPEC-25` S3, [#227](https://github.com/sunstoneinstitute/horndb/issues/227), planned), so a naive apply-then-advance consumer can diverge from HornDB permanently and silently. P1 ships the applied-position slot itself (§S1–S3, S5, S6): `crates/sparql/src/feed.rs` records feed id, generation, an opaque position token, and wall-clock time as quads in the reserved `https://horndb.io/graph/feed` graph, advanced by one trailing `apply_quads` call issued only after every operation in an `/update` request has committed — so a mid-request failure or crash always leaves the slot at or behind the data, never ahead (D5), and a feed-id mismatch against a non-empty slot refuses with HTTP 409 before any mutation (D6). `X-HornDB-Feed-Id`/`X-HornDB-Feed-Position` request headers opt an `/update` call in; no headers means no slot touch. `horndb_feed_*` metrics (S6) cover advances, quad counts by op, apply latency, and the still-always-zero generation/rebuild/recovery-gap gauges P2–P4 will give real values. Tests: `tests/feed_slot.rs` (property test `position_never_overstates` fuzzes fault-injected multi-op sequences against the D5 invariant), `tests/server_http.rs`. **Out of scope for P1** (specified, not started): rebuild-from-zero (P2), checkpoint integration (P3), WAL integration (P4). |
| Streaming result serialization (F6) | **implemented** | Both the per-node buffering (#143 streaming runtime) and the whole-body buffering are gone. `Runtime::run_stream` yields a `BindingsStream`; `server/query.rs::stream_select` pulls it chunk-by-chunk through per-format incremental serializers into a chunked `ChannelBody`, with a sized-body fast path when a SELECT fits one chunk. All four SELECT formats (JSON/XML/CSV/TSV) stream. A failure after the headers commit — a `SparqlError` or a **panic** in the serializer (`AbortBodyOnPanic`, HDB-115) — aborts the body instead of terminating a truncated document cleanly. Design: `docs/specs/SPEC-22-http-streaming-results.md` + `docs/plans/PLAN-22-01-http-streaming-results.md`. ([#56](https://github.com/sunstoneinstitute/horndb/issues/56), closed; delivered under [#128](https://github.com/sunstoneinstitute/horndb/issues/128)) **HDB-119 retired SPEC-22's accepted trade-off** (the store read lock held for the whole drain): `stream_select` pins a read view first and streams with no lock held, and the query answers from that one pinned commit version — an `/update` committed mid-stream is invisible to it. `HornBackend`'s view is O(1) (shared storage handle + one tier pin, on SPEC-25 S1 MVCC); `MemStore`, the test backend, deep-copies. Pinned by `server_http.rs::update_completes_while_a_select_is_still_streaming`. |
| SPARQL 1.1 Federation (`SERVICE`) | **deferred** | Indefinitely — out of scope, not just unimplemented. `translate.rs` rejects it with HTTP 400, body `unsupported algebra construct: Service`. |
| Operator configuration system | **implemented** (see §15) | `SPEC-26` (approved) — layered config (built-in defaults < base `config.toml` < `config.d/*.toml` drop-ins < env < argv), live watch/reload (`notify` + `ArcSwap`), and a two-tier server-vs-query settings model with per-query URL-param overrides; wires `bind`, the `[simd]` knobs, `query_timeout`, `max_result_rows`, `rdf12`, and (HDB-118) `max_concurrent_queries` / `queue_timeout` / `max_request_body` to real config (`max_query_memory` parsed but enforcement delegated to a companion spec). New foundation crate `horndb-config`. Four leaf tasks: Phase 1a `horndb-config` crate ([#249](https://github.com/sunstoneinstitute/horndb/issues/249), `PLAN-26-01`), Phase 1b serve wiring + `[simd]` injection ([#250](https://github.com/sunstoneinstitute/horndb/issues/250), `PLAN-26-02`), Phase 2 query overrides + enforcement ([#251](https://github.com/sunstoneinstitute/horndb/issues/251)), Phase 3 live watch/reload ([#252](https://github.com/sunstoneinstitute/horndb/issues/252)) — all four have landed. |

---

## 10. SPEC-08 — ML / LLM integration boundary

**Crate:** `horndb-ml` · **Spec:** `SPEC-08` · **Overall status: implemented (interfaces + HTTP boundary, opt-in)**

The boundary where ML sits. Symbolic reasoning is the source of truth; ML
proposes and advises. Disabling all ML must be bit-identical for correctness
(NF1). The whole crate is opt-in via configuration. The HTTP boundary
(`POST /nl-query`, `GET /ml-audit`) ships behind the off-by-default `server`
feature; the LLM is never bundled (reached via the `Translator` trait, mock-tested
hermetically). The one remaining piece is the real FAISS-backed candidate
generator (Stage-2, native-linkage heavy) — see `TASKS.md` / SPEC-08.

| Component | Status | Notes |
|---|---|---|
| `CandidateGenerator` trait (propose `sameAs` etc.) | **implemented** | `candidate.rs` — interface + reference scaffolding. |
| `PlanAdvisor` trait (cost/join-order hints) | **implemented** | `planner.rs`. |
| `HotSetAdvisor` trait (tier-placement hints) | **implemented** | `hotset.rs`. |
| Provenance for ML-derived facts (F5) | **implemented** | `provenance.rs`. |
| Model registry + config (`ml.enabled`) | **implemented** | `registry.rs`, `config.rs`. |
| LLM → SPARQL HTTP endpoint (`POST /nl-query`, F3) | **implemented** | `server/nlquery.rs` (`server` feature). `Translator`/`SparqlExecutor` traits in `nlquery.rs`; LLM never bundled, mock-tested (hermetic). Generated SPARQL always returned for audit. |
| HTTP audit endpoint (`GET /ml-audit`, F6) | **implemented** | `server/audit.rs` wraps the in-process `MlAuditLog`; paginated, `since`-filtered. |
| Cost reporting (token counts + est. USD) | **implemented** | `CostReport` surfaced in the `/nl-query` response. |
| Training-data leakage controls | **implemented** | `config::LlmPrivacy` — no-retention default, redaction option; single `loggable_text` chokepoint. |
| Real FAISS-backed `CandidateGenerator` | **planned** | Open increment under `TASKS.md` MEDIUM · *Completeness* — "SPEC-08 ML" (#8). Native FAISS linkage; separable from the HTTP boundary. |

---

## 11. SPEC-09 — Hardware specialization (Stage 3)

**Crate:** `horndb-hardware-ext` (empty placeholder) · **Spec:** `SPEC-09` · **Overall status: specified / deferred**

Roadmap, not an implementation contract. Stage 1 and Stage 2 must not depend
on it; Stage 3 begins only after Stage 2 acceptance passes.

| Component | Status |
|---|---|
| GPU/APU GraphBLAS closure backend | **deferred** (Stage 3) |
| GPU WCOJ kernels (cuMatch-style) | **deferred** (Stage 3) |
| CXL 2.0/3.0 warm-tier extension | **deferred** (Stage 3) |
| NVMe cold tier via GPUDirect Storage / BaM | **deferred** (Stage 3) |
| Multi-node distributed DBSP | **deferred** (Stage 3) |
| TPU / NPU / FPGA / custom silicon | **deferred** (indefinitely) |

---

## 12. SPEC-10 — rdflib-compatible Python API

**Crate:** `crates/python` (`horndb-python`) · **Spec:** `SPEC-10` ·
**Overall status: partially implemented**

A Python compatibility layer (PyO3/maturin) exposing rdflib-shaped term
classes, a `Graph` facade, core operations, parse/serialize, and SPARQL
passthrough to the Rust engine. The first increment ships the core
graph-centric surface; `docs/rdflib.md` compares common rdflib workflows with
the HornDB surface. Tracked as a MEDIUM *Completeness* epic in `TASKS.md`
(#9), split into shippable increments.

The binding crate is **excluded from the Cargo workspace** so the hermetic
`cargo build/clippy/test --workspace` never needs a Python interpreter; it is
built with maturin and exercised by a dedicated `python-rdflib-compat` CI job.

| Component | Status | Notes |
|---|---|---|
| rdflib-shaped terms (`URIRef`, `BNode`, `Literal`, `Variable`, `Namespace`) | **implemented** | SPEC-10 F1; differential-tested vs upstream rdflib. |
| `Graph` facade (add/remove/set/triples/subjects/objects/value/len/contains/iter) | **implemented** | F2. |
| `Dataset` / `ConjunctiveGraph` named-graph facades | **planned** | F3; not yet built in the Python binding. Unrelated to the store, which gained named-graph query (SPEC-28 phase 3) and update (phase 4, [#267](https://github.com/sunstoneinstitute/horndb/issues/267)) — this row tracks the still-unwritten rdflib-shaped `Dataset`/`ConjunctiveGraph` Python classes. |
| `parse` / `serialize` (Turtle, N-Triples) | **implemented** | F4; TriG/N-Quads/RDF-XML/JSON-LD deferred. |
| `query` / `update` passthrough to SPEC-07 | **implemented** | F5; SELECT/ASK/CONSTRUCT + INSERT/DELETE DATA. |
| Namespace binding (`bind`, `namespaces`, `Namespace`) | **implemented** | F6. |
| `rdflib-compat` differential subset | **implemented** | Acceptance #1/#2/#6; `crates/python/tests/`, `harness/curation/rdflib-compat.md`. |
| Multi-version CPython wheel matrix (macOS + Linux) | **planned** | Acceptance #7; one Linux CI job today. |

> The tracking epic (#9) is split into per-increment sub-issues as
> implementation lands; the first increment delivered the core surface above.

---

## 13. SPEC-11 — SSSOM mappings & crosswalk index

**Crate:** `crates/owlrl` (chain rules) + `crates/storage` (crosswalk index) ·
**Spec:** `SPEC-11` · **Overall status: partial / in progress**

First-class support for [SSSOM](https://mapping-commons.github.io/sssom/)
ontology crosswalks: mappings arrive as RDF from the external SoR (ADR-0016,
data-platform ADR-0002 — HornDB does **not** parse SSSOM/TSV in production),
their chain-rule closure is materialized by the compiled rule engine, and
query-time crosswalking is served from a compact, SIMD-friendly index over
sequential `TermId`s. `skos:exactMatch` is a crosswalk edge, **not** OWL
identity (ADR-0017). Tracked as a HIGH *Completeness* task in `TASKS.md`.

| Component | Status | Notes |
|---|---|---|
| Mapping-predicate vocabulary in `vocab.rs` (SKOS/OWL/semapv) | **implemented** | SPEC-11 F1. |
| Mapping representation (n-ary `sssom:Mapping` node + positive base triple; negated = n-ary only) | **partial** | F2; n-ary node builder exists, full materialization-on-inference is follow-up. RDF 1.2 deferred (ADR-0014, ADR-0002 D10). |
| SSSOM chaining rules in `rules.toml` (T1 / RCE1-2 / RI1-5 / RG1-2; transitive → closure) | **implemented** | F3; rides SPEC-04 codegen + SPEC-05 closure. RCE-N OWL rules already entailed by `cax-*`/`scm-*`. |
| Negative-mapping chaining (monotone, `Not` as distinct predicate) | **implemented** | F4; preserves SPEC-04 negation-free stratification. |
| Compact crosswalk index (rung-2 EF+FOR baseline → rung-4 PGM) | **planned** | F5; ~10 B/pair bidi target (NF2). |
| Crosswalk spine (designated sets always-resident; identity rides ADR-0007 spine) | **planned** | F6. |
| Confidence propagation along chains (product default; SeMRA) | **implemented** | F7. |
| Chain provenance (`derived_from` = proof premises) | **implemented** | F8; reuses SPEC-04 F4. |
| Harness SSSOM/TSV loader (bench/standalone only) | **implemented** | F9; not a production surface. |

> `skos:exactMatch` is deliberately kept out of OWL identity (ADR-0017) — the
> chain rules give crosswalk recall without `eq-rep-*` entailment pollution.

---

## 14. SPEC-12 — SIMD acceleration layer

**Crate:** new `horndb-simd` (zero-dep leaf) + consumers `crates/wcoj`,
`crates/storage`, `crates/owlrl` · **Spec:** `SPEC-12` · **Overall status: partially implemented** (primitives crate + WCOJ F1 seek/intersect consumer + storage F2 decode/scan consumer landed; F3 delta-apply still specified, gated on [#133](https://github.com/sunstoneinstitute/horndb/issues/133))

A single, shared, runtime-dispatched SIMD layer for the data-parallel hot loops:
`std::arch` intrinsics on stable Rust with cached-function-pointer dispatch
(AVX-512/AVX2 on the EPYC Zen4 reference host, NEON on Apple-Silicon dev Macs,
**scalar fallback always present as the correctness oracle**). Serves the SPEC-03
NF1 (`per_tuple`) hot path and the SIMD-friendly half of SPEC-02 NF2 (STREAM
`rdf:type` scan). (SIMD alone did **not** close NF1 — see the `per_tuple` row and
#237/#239.)
Every kernel is differential-proven bit-identical to its scalar oracle. Tracked as a
HIGH *Performance* task in `TASKS.md`.

| Component | Status | Notes |
|---|---|---|
| `horndb-simd` primitives crate + scalar oracle + per-kernel differential proptests | **implemented** | F4+F5; new zero-dep leaf *below* `storage` (`simd → storage → wcoj → …`). Sole home for hand-written intrinsics. Ships six runtime-dispatched primitives (`lower_bound`, `intersect`, `merge`, `dedup`, `filter`/`filter_range`, `gather`) over `&[u64]`/`&[u32]`, each differential-proven bit-identical to the scalar oracle on every ISA path the host runs (`crates/simd/tests/differential.rs`), plus the `with_forced_isa` F5 override. AVX2/AVX-512 kernels for x86_64, NEON for aarch64; `merge` and `filter_range`'s AVX2 arm keep scalar-equivalent bodies until a bench earns intrinsics. Per-host kernel choice is the dispatch row's selection ladder; kernel bench numbers in `docs/benchmarks.md`. |
| WCOJ seek + leapfrog intersect SIMD | **implemented** | F1; highest payoff. Seek: `VecTripleSource` stores each ordering **column-major** (three `Vec<TermId>`, one per trie level), so `VecIter` seeks the stored column directly through `horndb_simd::lower_bound` at **every** depth — no transient column to build (#239 removed the old row-major layout's per-level copy and with it the O(range) per-`open_level` rebuild that made deeper levels stay scalar). `PackedColumn::lower_bound` (compressed source) bisects to the owning block, decodes it, and SIMD-finishes (`source/vec_source.rs`, `source/packed_column.rs`). Intersect: both the standalone `LeapfrogJoin` (`trie/leapfrog.rs`) **and the production executor's inlined leapfrog** (`executor/wcoj.rs::BatchIter`) gain a k==2 fast path over `active_run` contiguous views via `horndb_simd::intersect` — when both contributing iters at a depth expose a run ≥ `SIMD_INTERSECT_MIN_RUN` (64), the whole pairwise intersection is precomputed once and drained, replacing per-candidate round-robin seeks. The SIMD *seek* path is likewise live in `BatchIter`. To honour the leapfrog's distinct-key contract, `active_run` returns a **deduplicated** cached copy at depths 0 and 1 (the stored column repeats a key once per child row, so a subject with many objects would otherwise emit each key many times); the leaf column is already strictly increasing under a fixed prefix and is returned as a zero-copy slice. Output bit-identical to scalar — gated by the WCOJ differential fuzzer (narrow + a wide `N_WIDE > 64` variant that arms the intersect), the leapfrog BTreeSet oracle, and `tests/batchiter_simd.rs` (incl. the duplicate-subject hazard). **#237** then attacked the non-intersect half of NF1: the leapfrog descent gallops child-run/cursor boundaries instead of bisecting the wide parent range (`run_end`/`seek_gallop`), and an armed leaf is **bulk-materialized** into the Arrow batch (`push_run_chunk`) instead of drained per value — marginal cost 14.4 → **8.5 ns/tuple** (hornbench). The residual to ≤5 ns was the row→column input copy (~46% of the marginal profile); **[#239](https://github.com/sunstoneinstitute/horndb/issues/239)** removed it by making `VecTripleSource` columnar, reaching **2.74 ns/tuple** — **NF1 met** (`docs/benchmarks.md`). |
| Dictionary decode + `rdf:type` partition scan SIMD | **implemented (hornbench numbers recorded; scan meets SPEC-02 #4, decode misses NF4)** | F2; jointly satisfies SPEC-02 acceptance #4. `horndb-storage` consumes `horndb-simd`: bulk inline-int decode (`Dictionary::decode_inline_ints`/`lookup_inline_int_batch`/`lookup_batch`, the mask+cast unpack core) and a vectorised `rdf:type` partition scan (`PredicatePartition::subjects_with_object` via the new `horndb_simd::filter_indices_eq` scan+index-compact primitive composed with `gather`). New primitive is differential-proven equal to scalar on every host ISA path (`crates/simd/tests/differential.rs`); storage paths covered by `crates/storage` unit tests. **hornbench measured (2026-07-07, Ryzen 7 7700, node-0-pinned):** `partition_scan` **34.5 GB/s = ~104% of STREAM-Triad** → SPEC-02 acceptance #4 **met (GREEN)**; `dict_decode` AVX2 vs scalar **~1.01×** → NF4 ≥4× **not met (RED)**, the decode loop is load/store-bound so SIMD is not the lever (`docs/benchmarks.md`). |
| Delta-apply merge/dedup/sort SIMD | **specified (gated on [#133](https://github.com/sunstoneinstitute/horndb/issues/133))** | F3; needs hash-delta → sorted-run change first. The `cax-sco` partition-filter scan is **out of scope** — superseded by #133's object index + semi-naïve firing. |
| Runtime ISA dispatch (cached fn-ptr, `is_*_feature_detected!`, no nightly) | **implemented** | NF5; cached `OnceLock` fn-ptr per primitive, scalar-forced build green on stable 1.90. F5 `with_forced_isa` makes dispatch test-forceable so the differential suite exercises every host ISA path. **Kernel selection** resolves each primitive through the ladder `forced → ISA cap → known-CPU table → representative-input calibration → static widest` (reworked 2026-07-01 after an LDBC SPB-256 A/B proved the previously-calibrated SIMD kernels net-harmful vs scalar on both measured hosts — a kernel microbench win does not imply a workload win). The ISA cap and auto-tune toggle are seeded via `horndb_simd::configure` from `[simd]` config (`HORNDB_SIMD__MAX_ISA` / `HORNDB_SIMD__AUTOTUNE`; `crates/simd` reads no env directly). The known-CPU table (`cpu.rs`, CPUID-keyed, SPB-derived) pins scalar on both measured hosts; representative-input calibration (auto-tune) is the fallback for unlisted CPUs. Selected ISA + selection tier exported as the `horndb_simd_kernel_isa{kernel,isa,source}` gauge. Full ladder + knobs: `docs/architecture/simd.md`; measurements: `docs/benchmarks.md`. |

> SIMD accelerates loops that are already *algorithmically right*. It is **not** a
> substitute for the missing indexes/semi-naïve firing that dominate the SPEC-04
> materialize path — that is [#133](https://github.com/sunstoneinstitute/horndb/issues/133)
> (see §6), explicitly out of SPEC-12's scope.

---

## 15. SPEC-26 — Operator configuration system

**Crate:** new `horndb-config` (dependency-light leaf) · **Spec:** `SPEC-26` · **Overall status: implemented** (Phases 1a/1b library + `serve` wiring, Phase 2 per-query overrides and enforcement, and Phase 3 live watch/reload have all landed; `max_query_memory` enforcement stays deferred to the companion memory spec)

A single typed `ServerConfig` loaded by layering, lowest precedence to highest:
built-in defaults, a base `config.toml`, `config.d/*.toml` drop-in fragments
(pooled across every configured directory and applied in file-name order),
environment variables (`HORNDB_` prefix, `__` nesting), and caller-supplied
command-line overrides. Two small newtypes, `ByteSize` and `HumanDuration`,
parse human-readable strings like `"2GiB"` and `"30s"` so config values stay
both typed and readable. `figment` (an internal implementation detail, not part
of the public API) does the layering; every model struct rejects unknown keys
so a typo in a config file fails loudly instead of being silently ignored.

| Component | Status | Notes |
|---|---|---|
| Layered load (`horndb-config`: defaults < base < config.d < env < argv), typed model, validation | **implemented** | `crates/config/`, SPEC-26 S1/S2 (PLAN-26-01). Library only. |
| `serve` wiring (`--config`, value flags, `[simd]` injection, startup-fatal validation) | **implemented** | SPEC-26 S6 (PLAN-26-02, [#250](https://github.com/sunstoneinstitute/horndb/issues/250)). |
| `[server.limits]` admission control (`max_concurrent_queries`, `queue_timeout`, `max_request_body`) | **implemented** | HDB-118. Server-scope, so deliberately **not** in `QuerySettings` (no per-query override). `serve.rs` rejects `max_concurrent_queries = 0` at startup — `usize` gives serde no lower bound to reject it. Enforcement lives in `crates/sparql/src/server/` (see the SPEC-07 table above). |
| `[reasoning].backend` — operator-selectable closure backend for `serve --materialize` | **implemented** | `rule-firing` (default) or `graphblas`, mapped in `crates/sparql/src/bin/serve.rs` onto `horndb_owlrl::BackendChoice` and threaded through `load_with_reasoning`. `graphblas` needs `horndb-sparql`'s non-default `graphblas` feature (it links SuiteSparse:GraphBLAS); selecting it on a build without the feature is startup-fatal naming the feature, and the release image builds with it. Reported as the `horndb_reasoning_backend{backend}` info gauge and a startup log line. The serve-level LUBM-1 / SKOS A/B on `hornbench` is outstanding; the backend-level profiling behind it is the [#61](https://github.com/sunstoneinstitute/horndb/issues/61) row in `docs/benchmarks.md`. |
| Per-query URL/form overrides + enforcement (`query_timeout`, `max_result_rows`, `rdf12`) | **implemented** | SPEC-26 S4/S5 ([#251](https://github.com/sunstoneinstitute/horndb/issues/251)). Each `/query` request layers the whitelisted URL (and, for a form POST, body) parameters over the `[server.limits]` defaults snapshotted from `AppState.config` — into one `QuerySettings`; an unknown key or unparseable value is a 400 naming the key and touches nothing else. Enforcement: a server-layer timer trips the query's `wcoj::CancelToken` (published to executors thread-locally, `exec::cancel`; `horndb-wcoj` gains no config dependency) and the query ends as `SparqlError::QueryTimeout` (504); a row counter in the result stream ends an over-cap stream with `SparqlError::ResultRowLimit` — a 400 before the headers commit, an aborted body after — never a silent truncation; `rdf12` flips per query, making the already-plumbed per-request `SparqlConfig` path live from HTTP. The `default_graph` key threaded early by SPEC-28 S3 ([#266](https://github.com/sunstoneinstitute/horndb/issues/266)) folded into this whitelist. |
| `max_query_memory` enforcement | **deferred** | SPEC-26 S5 non-goal: the knob is parsed, carried on `QuerySettings` and accepted per query, but bounds nothing. Real per-query memory accounting is the companion memory spec. |
| Live watch/reload | **implemented** | SPEC-26 S3 + the remaining S6 metrics ([#252](https://github.com/sunstoneinstitute/horndb/issues/252)). `horndb_config::ConfigHandle` is the live `ServerConfig` behind an `ArcSwap`; `AppState.config` holds it and each `/query` request takes a snapshot, so a hot key edited on disk is live for the next request. `horndb_config::watch` arms a `notify` watcher over the base file's **parent directory** plus every `config_dirs` entry (a directory watch, so an editor's rename-into-place save — which replaces the inode — never orphans it), debounces by `[reload].debounce`, then re-runs the whole layered load. Reload is therefore insensitive to event shape. A config that fails validation is dropped: the previous one stays live and `config_reload_total{result="rejected"}` goes up. A cycle that resolves to the config already live publishes nothing. Restart-only keys (`restart_only_changes`: `[server].bind`, `.config_dirs`, `.shutdown_drain`, the three `[server.limits]` admission keys, `[simd]`, `[reasoning]`) are stored for a later restart and logged "requires restart to take effect"; the watcher never re-applies `[simd]`. Metrics: `config_reload_total{result}`, `config_active_generation`, `config_last_reload_unixtime`. Tests: `crates/config/tests/watch.rs` (real files, incl. two successive rename-into-place saves and a `config.d` drop-in), `crates/sparql/tests/serve_config_wiring.rs::live_reload_applies_hot_keys_keeps_bad_edits_out_and_flags_restart_only` (real `serve` subprocess). macOS is unverified. |

---

## 16. Cross-cutting concerns

### Query optimization vs. reasoning-strategy selection
**Status: partially implemented — Phases 1–3 are implemented. Phase 1
(optimizer framework scaffolding, [#201](https://github.com/sunstoneinstitute/horndb/issues/201)):
`crates/sparql/src/plan/{logical,types,pass,lower}.rs` ship the logical IR
(flat n-ary `Bgp`), the binding/type lattice, and the typed/ordered/
individually-disable-able pass registry, with `planner::plan` routed through
the pipeline behind golden-plan tests and a `PRAGMA disable-pass=<id>` query
pragma for bisection. Phase 2 (heuristic rewrite passes, [#202](https://github.com/sunstoneinstitute/horndb/issues/202)):
`crates/sparql/src/plan/passes/` registers `Normalize` (constant folding +
lattice-gated `Eq`→`SameTerm`), `FilterPullup`, `FilterPushdown`
(`LeftJoin`-asymmetry- and `Project`-scope-aware), and `ProjectionPushdown`
after `CoalesceBgp`, guarded by the slot-differential battery
(`crates/sparql/tests/rewrite_invariance.rs`: full pipeline vs each pass
singly disabled). Phase 3 (layered `Stats` seam + Characteristic-Sets
cardinality estimator, [#203](https://github.com/sunstoneinstitute/horndb/issues/203)):
`crates/wcoj/src/stats.rs` adds the read-only, cost-tiered `Stats` trait
(Tier-0 counts + per-position NDV, Tier-1 Characteristic-Sets index with
top-K + residual bucket, Tier-2 per-role max-degree, Tier-3 `sample_join`
hook — inert by default), computed **recompute-from-snapshot**
(`SnapshotStats::from_source` scans the pinned `VecTripleSource`); and
`crates/wcoj/src/estimator.rs` the `StatsEstimator` — a per-pattern base +
denominator join model (transitive-equality-class denominators + PK/FK cap),
a Characteristic-Sets star estimator, and a sound degree-based upper bound, so
estimates are an `(estimate, upper_bound)` pair. It is wired into `EXPLAIN` on
the `HornBackend` path, with `UniformEstimator` demoted to the zero-stats
fallback. The accuracy gate (`estimator.rs` `mod accuracy_gate`) proves the
estimator strictly better than `UniformEstimator`, Characteristic Sets beating
the Tier-0 denominator model on star shapes, and `upper_bound` never below the
measured count (SPEC-23 acceptance #3). The SPEC-23 §8 statistics-ownership
question is resolved **provisionally** as recompute-from-snapshot, with the
cached summary maintained incrementally from the committed quad delta on the
`HornBackend` write path (HDB-123) and rebuilt when drift passes the bound;
full incremental maintenance under SPEC-06 deltas is still open (see
`PLAN-23-03`). Phase 4
([#204](https://github.com/sunstoneinstitute/horndb/issues/204), HDB-46) is
implemented: `horndb-wcoj`'s planner is cost-based (cyclic-core routing, i-cost
DP over connected subsets, greedy WCOJ variable order) and the fixed
`wcoj_cutover == 4` is retired to an env-var bisection aid — see the
"Cost-based plan choice" row above. Later phases
([#205](https://github.com/sunstoneinstitute/horndb/issues/205)–[#207](https://github.com/sunstoneinstitute/horndb/issues/207))
stay planned in `TASKS.md`; reasoning-strategy selection stays out of the optimizer until phase 6.** Two concerns that are easy to
conflate live in different places:

- **The query optimizer** (the SPEC-23 framework over the SPEC-03 WCOJ and
  SPEC-07 SPARQL planners) is a *join/variable-ordering cost engine*: logical
  IR, pass registry, `Stats` seam, cost-based ordering — all over a graph that
  has **already been reasoned**. It does not choose reasoning strategies.
- **Reasoning-strategy selection** — a compiled OWL-RL rule vs. the GraphBLAS
  closure resolver, expanding an SSSOM crosswalk, resolving a SKOS
  `broader`/`narrower` hierarchy — is **not a query-time decision today**. It
  happens *upstream of the optimizer*, at materialization / rule-compile time,
  so it is **absent from SPEC-23 by design** and is not in the physical plan:
  - OWL-RL/closure routing is fixed in `crates/owlrl/rules.toml`
    (`delegate = "closure"` → GraphBLAS, SPEC-05; else compiled rule firing,
    SPEC-04) and the closure is **materialized ahead of the query**.
  - SSSOM crosswalking is materialized as base triples plus a resident
    crosswalk index/spine (SPEC-11) precisely so it is "cheap by construction,
    not a per-query burden"; queries hit the already-closed graph.
- **When it *would* enter the optimizer:** only once hybrid backward-chaining
  lands (magic sets / demand transformation, SPEC-03 F4/F5, now **planned** — E1
  leaf issue [#207](https://github.com/sunstoneinstitute/horndb/issues/207)), when
  a query can answer without full materialization and a genuine "materialize
  vs. rewrite vs. delegate-to-resolver" choice appears. That choice is
  **logical, not physical**: a rewrite pass in the SPEC-23 pass registry
  (before `JoinPlanning`), fed by a *reasoning/materialization catalog* seam
  **parallel to `Stats`** (what is already closed + resolver cost), and realized
  physically via the existing `PathClosure` node (`Algebra::l`) or a
  closure-scan operator. The prior art SPEC-23 surveys (Oxigraph `sparopt`,
  DuckDB, ClickHouse) are all **non-reasoning** engines, so none of them informs
  this layer — it is HornDB-specific.
- **Now specified and decomposed.** This is the flagship of the Stage-2 push: the
  **single unified query+reasoning IR** epic E1 ([#185](https://github.com/sunstoneinstitute/horndb/issues/185),
  `SPEC-23` approved 2026-07-18, decomposed into leaf issues
  [#201](https://github.com/sunstoneinstitute/horndb/issues/201)–[#207](https://github.com/sunstoneinstitute/horndb/issues/207)).
  See [Stage-2 investment epics](#stage-2-investment-epics).

### Provenance / correctability
**Status: partially implemented.** Stage-1 ships per-triple `Provenance`
(`owlrl/src/provenance.rs`) and an ML-derived-fact provenance hook
(`ml/src/provenance.rs`). Proof trees (SPEC-04 F4) and proof retrieval
(NF4) are **implemented**: `MemStore::proof_tree` / `Engine::proof` build
a recursive proof bottoming out at asserted triples, within the NF4 100 ms
budget (`owlrl/tests/proof_tree.rs`). Production *persistence* of proofs
(compressed side-table, on-demand re-derivation) is **planned**
(`TASKS.md` SPEC-04 rules).

**No user-facing surface yet — proofs are reachable only from Rust.**
`load_with_reasoning` (`sparql/src/exec/horn.rs`) drops the `owlrl::Engine`
after dumping the closure into storage, so a running `serve --materialize`
holds no derivation data; there is no HTTP, SPARQL, or Python surface.
Exposure is **specified** in `specs/SPEC-27-provenance-as-a-queryable-view.md`
(virtual `hprov:` RDF view, queried with ordinary SPARQL) — draft, no plan yet
([#260](https://github.com/sunstoneinstitute/horndb/issues/260)).

### RDF 1.2 (triple terms)
**Status: implemented end-to-end (Stage-1 surface).** We track W3C **RDF 1.2**,
not the community RDF-star extension. `TermKind::TripleTerm` in storage, the
N-Triples loader, gated SPARQL triple-term patterns, and the
`rdf12-n-triples` harness suite all ship. Turtle/TriG/N-Quads/semantics suites
remain **deferred** (`TASKS.md`, RDF 1.2 entries — both `[x]`). The OWL 2 RL
Stage-1 engine and W3C-manifest paths explicitly bail on triple-term inputs.

### Performance gates (docs/benchmarks.md)
**Status: partially implemented.** Per-subsystem targets and measured numbers
live in `docs/benchmarks.md`. SPEC-03's 4-cycle ≥10× gate is **met**
([#1](https://github.com/sunstoneinstitute/horndb/issues/1)). SPEC-03 NF1
(`per_tuple` ≤5 ns/tuple — SPEC-03 is the source of truth; the ≤2.5 ns SIMD-epic
figure is superseded) is **met**: #237 (galloping descent + bulk leaf
materialization) took the marginal cost 14.4 → 8.5 ns/tuple on hornbench, and
[#239](https://github.com/sunstoneinstitute/horndb/issues/239) (columnar
`VecTripleSource`, removing the row→column input copy) took it to **2.74
ns/tuple**. The SPEC-02 NF2
STREAM `rdf:type` scan is owned by **SPEC-12** (§14, the SIMD layer). Keep
`docs/benchmarks.md` rows in sync with the `TASKS.md` performance entries.

### Observability / metrics
**Status: implemented (Phase-1 Slice 1 + Phase-2 fan-out complete: owlrl + incremental + ml + wcoj + sparql-bytes slices); OTel traces/logs deferred to a later phase.** Metrics use
`prometheus-client` (typed `#[derive(EncodeLabelSet)]` labels, no strings) in a
foundational `horndb-metrics` crate that owns a process-global `OnceLock`
registry and the only `prometheus-client` dependency. Hot-path updates are
direct atomic ops on cached handles; quantities that are expensive to compute
(triple/dictionary/tier sizes) are pulled at scrape time via a `Collector`, not
maintained continuously. Slice 1 ships the SPARQL HTTP layer (request
count/latency/status + per-stage parse/translate/plan/exec timing +
query-kind counters), the closure backend (`ClosureMetrics` → histograms), and
storage sizes, exposed at `GET /metrics` (OpenMetrics text, behind the `server`
feature). OTel interop is achieved off-box by a collector scraping `/metrics`;
no in-process OTLP push. **Phase-2 Slice 1 (owlrl):** `OwlrlMetrics` subsystem
— per-rule fire counts (`horndb_owlrl_rule_fires_total{rule}`), per-rule +
per-phase latency histograms, `owlrl_triples_inferred_total`,
`owlrl_rounds_total`, dirty-predicate prune counters; closure `input_nnz`
observed alongside `output_nnz`; `storage_tier_bytes_estimated` now carries the
`tier` label (`MemTier` enum wired, `tier="unknown"` until full HBM/CXL
accounting lands). **Phase-2 Slice 2 (incremental):** `IncrementalMetrics`
subsystem — `horndb_incremental_tick_duration_seconds` histogram (per-tick
latency), `horndb_incremental_asserted_merged_total` /
`horndb_incremental_derived_merged_total` counters (merge work per tick),
`horndb_incremental_closure_withdraw_total` /
`horndb_incremental_closure_promote_total` counters (retraction/promotion),
`horndb_incremental_fixpoint_rounds` histogram (convergence depth); and
`horndb_incremental_change_feed_subscribers` gauge (maintained at subscribe +
publish-reap) plus `horndb_incremental_change_feed_dropped_subscribers_total`
(subscribers dropped for lag under `LagPolicy::DisconnectSlow`, `SPEC-24` S3). **Phase-2 Slice 3 (ml):** `MlMetrics` subsystem (behind the
`server` feature of `horndb-ml`) — `horndb_ml_nl_query_total{result}` counter
(`result` ∈ `ok`/`error`); `horndb_ml_prompt_tokens_total`,
`horndb_ml_completion_tokens_total`, `horndb_ml_estimated_usd_total` counters
(from `CostJson`); `horndb_ml_translate_duration_seconds`,
`horndb_ml_execute_duration_seconds`, `horndb_ml_audit_query_duration_seconds`
histograms; `horndb-metrics` is an optional dep of `horndb-ml` gated on the
`server` feature. **Phase-2 Slice 4 (wcoj):** `WcojMetrics` subsystem — three
unlabelled histograms (`horndb_wcoj_seeks_per_query`,
`horndb_wcoj_iterations_per_query`, `horndb_wcoj_peak_iterators`) observed
exactly once per query in `impl Drop for BatchIter`; the inner loop only
increments plain `u64` struct fields (NO per-seek atomic/timing — strict §5.3
compliance). Whole-query granularity only. **Phase-2 Slice 5 (sparql-bytes):**
`horndb_sparql_request_bytes_total{endpoint}` and
`horndb_sparql_response_bytes_total{endpoint}` counters added to `SparqlMetrics`;
implemented via a `CountingBody` `http_body::Body` wrapper wired into the existing
`record_request` middleware — tallies data-frame bytes and observes once on
end-of-stream (exact, robust to streaming; not a `Content-Length` guess). Replaces
the permanently-zero series removed in Slice 1 (commit `d2cace9`). **Phase-2
fan-out is now complete** — no remaining Phase-2 fan-out items. Issue
[#148](https://github.com/sunstoneinstitute/horndb/issues/148) is closed. **Deferred:**
real HBM/CXL tier byte accounting, tracked under **EPIC E3** storage tiering
([#187](https://github.com/sunstoneinstitute/horndb/issues/187)); OTel traces and logs,
tracked under **EPIC E8** ([#192](https://github.com/sunstoneinstitute/horndb/issues/192)).
Design: `docs/specs/SPEC-17-metrics.md`.

### Build & CI split
**Status: implemented.** Pre-commit runs `cargo fmt --check` only; pre-push
runs workspace `clippy -D warnings` + `cargo build`. CI mirrors this plus a
real-engine conformance run, split into three parallel build jobs — `lint`
(fmt + clippy), `tests` (nextest + doctests), and `conformance` (harness under
the cheap-to-compile `conformance` cargo profile + real-engine run) — so no
compile pipeline waits behind another. Docs-only PRs skip the build via the
gate job; the cargo cache is saved only from `main` (see `.github/AGENTS.md`).
The closure crate needs SuiteSparse:GraphBLAS locally (built from vendored
sources — §7).

### Memory allocator (snmalloc)
**Status: implemented** (HDB-86 E1). The four shipped binaries —
`bench-trainmarks`, `harness`, `serve` (SPARQL HTTP), `bench-rdfox` — set
`#[global_allocator]` to snmalloc. A library cannot set one, so each binary
declares it itself, behind a **default-on `snmalloc` cargo feature** that is
both the revert switch and the way to re-run the A/B; CI builds both paths via
`clippy --all-targets`. Rationale: a bulk load frees ~30M oxrdf terms on the
main thread that were allocated on parse threads, and glibc `malloc` takes the
owning arena's lock for every such cross-thread free. Measured on hornbench at
trainmarks xlarge: **−10.6% on the `parse` phase, −6.3% end-to-end**; mimalloc
lost the same A/B and is not carried. Numbers in `docs/benchmarks.md`.
Caveat: E1 measured only the bulk-load path, while `serve` is what the LDBC
SPB-256 nightly measures — the nightly is the gate on the query path.

### Integration-test runner (cargo nextest)
**Status: implemented.** The workspace builds ~90 separate `crates/*/tests/*.rs`
binaries; cargo's built-in runner executes them serially per binary, which
dominated `cargo test --workspace` wall-clock. The standard runner is now
`cargo nextest run`, which schedules all tests across the binaries in one
concurrent pool — same test set, no source changes (locally ~40% faster on a
quiet machine; more under contention / in CI). Config: `.config/nextest.toml`
(`default` + `ci` profiles). nextest does not run doctests, so CI keeps a
separate `cargo test --workspace --doc` step (zero runnable doctests today).
Chosen over consolidating test files into fewer targets, which would touch test
source and risk dropping coverage for a smaller, riskier win ([#108](https://github.com/sunstoneinstitute/horndb/issues/108)).

---

## 17. Roadmap stages

| Stage | Scope | Status |
|---|---|---|
| **Stage 0** — Harness bootstrap | SPEC-01 minimal slice + CI gating | **implemented** |
| **Stage 1** — Feasibility prototype | SPEC-02/03/04 slices + SPEC-05/06/07 slices, ≥50-case W3C OWL 2 RL subset green | **implemented** (with open gaps tracked in `TASKS.md`) |
| **Stage 2** — MVP | Full SPEC-02..07, full W3C OWL 2 RL + SPARQL 1.1 entailment suites, ORE 2015, LDBC SPB SF3, RDF 1.2 priority | **planned / specified** |
| **Stage 3** — Hardware specialization | SPEC-09: GPU/CXL/NVMe/multi-node | **deferred** |

### Stage-2 investment epics

The Stage-2 push (opened 2026-07-07) pulls the previously-**deferred** work into
**to-spec**: each cluster below has a `needs-decomposition` epic issue and is
queued to be specified, then decomposed into leaf issues via the `to-issues`
skill. The flagship is the **single unified query+reasoning IR** (E1) — see §16
"Query optimization vs. reasoning-strategy selection" for why it comes first.
Individual subsystem rows above keep their fine-grained deferred sub-notes; those
are now rolled up under the named epic here.

| Epic | Scope | Priority | Issue |
|---|---|---|---|
| **E1** Unified query + reasoning IR (single IR; `SPEC-23`) — **spec approved + decomposed 2026-07-18**, the first epic to leave `to-spec` | optimizer framework (ships first) + reasoning-as-rewrite & reasoning/materialization catalog seam, magic-sets/backward-chaining (SPEC-03 F4/F5), SPARQL backward mode (SPEC-07), cost-based WCOJ planner, property-path→GraphBLAS choice | **critical** | [#185](https://github.com/sunstoneinstitute/horndb/issues/185) → leaf issues [#201](https://github.com/sunstoneinstitute/horndb/issues/201)–[#207](https://github.com/sunstoneinstitute/horndb/issues/207) |
| **E2** SPEC-06 incremental maintenance completeness (`SPEC-24`) | fully delta-incremental retraction, MVCC backing, feed reconciliation, engine wiring, WAL/backpressure/cost-model — **S1 rule retraction delivered** ([#210](https://github.com/sunstoneinstitute/horndb/issues/210), `PLAN-24-01`); **S2 output-sensitive closure deletion + exact seed delivered** ([#211](https://github.com/sunstoneinstitute/horndb/issues/211), `PLAN-24-02`); **S3 change-feed net-delta + bounded backpressure delivered** ([#212](https://github.com/sunstoneinstitute/horndb/issues/212)); **S4 engine wiring delivered** ([#213](https://github.com/sunstoneinstitute/horndb/issues/213)); **S6 storage-MVCC snapshot backing delivered** ([#215](https://github.com/sunstoneinstitute/horndb/issues/215), ADR-0018); remaining phase tasks [#214](https://github.com/sunstoneinstitute/horndb/issues/214), [#216](https://github.com/sunstoneinstitute/horndb/issues/216), [#217](https://github.com/sunstoneinstitute/horndb/issues/217) in `TASKS.md` | **high** | [#186](https://github.com/sunstoneinstitute/horndb/issues/186) |
| **E3** SPEC-02 storage Stage-2 (`SPEC-25`) | per-tuple MVCC + delete path, persistent dictionary, WAL + crash recovery, named-graph snapshots, cold tier + tiering seam, deferred acceptance benches — **`SPEC-25` approved + decomposed 2026-07-19**; phase sub-issues [#225](https://github.com/sunstoneinstitute/horndb/issues/225)–[#230](https://github.com/sunstoneinstitute/horndb/issues/230) | **high** | [#187](https://github.com/sunstoneinstitute/horndb/issues/187) |
| **E4** SPEC-04 rule completeness Stage-2 | proof persistence, datatype value-space + full `dt-*`, list/QCR rules, user-defined rules, owlrl Z-set wiring | **medium** | [#188](https://github.com/sunstoneinstitute/horndb/issues/188) |
| **E5** SPEC-07 SPARQL surface completeness Stage-2 | Remote LOAD, XML results, recursive DESCRIBE, streaming CONSTRUCT/DESCRIBE. **Re-scoped 2026-07-28:** the named-graph half (GSP + named-graph scoping) is carved out into `SPEC-28` ([#261](https://github.com/sunstoneinstitute/horndb/issues/261)), at **critical** | **medium** | [#189](https://github.com/sunstoneinstitute/horndb/issues/189) |
| **E6** SPEC-08 ML integration Stage-2 | FAISS candidate generator, NL→SPARQL endpoint | **medium** | [#190](https://github.com/sunstoneinstitute/horndb/issues/190) |
| **E7** RDF 1.2 Stage-2 | Turtle/TriG/N-Quads/JSON-LD serialize + semantics suites, per-edge mapping annotation | **medium** | [#191](https://github.com/sunstoneinstitute/horndb/issues/191) |
| **E8** SPEC-17 observability Stage-2 | OpenTelemetry traces & logs | **low** | [#192](https://github.com/sunstoneinstitute/horndb/issues/192) |

Already-open Stage-2 tracking (not re-filed): SIMD remainder → [#132](https://github.com/sunstoneinstitute/horndb/issues/132); SSSOM mappings core → [#130](https://github.com/sunstoneinstitute/horndb/issues/130); OWL 2 RL conformance gap → [#160](https://github.com/sunstoneinstitute/horndb/issues/160); LDBC SPB scale → [#125](https://github.com/sunstoneinstitute/horndb/issues/125); Python graph-scoping → [#119](https://github.com/sunstoneinstitute/horndb/issues/119).

**Stays deferred (Stage-3 / indefinite), no epic:** all of SPEC-09 (GPU closure/WCOJ, CXL, NVMe, multi-node distributed DBSP, custom silicon); `SERVICE` federation, GeoSPARQL, full-text search; LAGraph adoption and valued-closure Fork B / PreJIT (revisit on a real use case); Windows Python support.

---

## Keeping this document honest

The Status fields above mirror the checkbox state in `TASKS.md`. They drift
apart the moment one is edited without the other. Two rules (also recorded in
the root `CLAUDE.md`):

1. **When you change `TASKS.md`** (check off, add, remove, or re-scope a task),
   update the matching **Status** field here in the same commit — e.g.
   checking off "SPEC-07 DESCRIBE" flips that row from **planned** to
   **implemented**.
2. **When you change a SPEC or plan** (`docs/specs/` or `docs/plans/`) such
   that the work-to-do changes, update `TASKS.md` in the same commit — add or
   re-scope the tracking task — and then reflect it here.

Source of truth for *intent* is the SPECs; for *outstanding work* it is
`TASKS.md`; for *current state* it is this document. When they disagree, the
code wins — fix whichever is stale.
