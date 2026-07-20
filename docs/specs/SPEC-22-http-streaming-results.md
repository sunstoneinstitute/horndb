---
status: implemented
date: 2026-07-06
scope: "Streaming SELECT results end-to-end to the HTTP layer — design"
---

# HTTP streaming results — design

**Date:** 2026-07-06
**Issue:** [#128](https://github.com/sunstoneinstitute/horndb/issues/128) remaining item 3 ("stream results out to the HTTP layer").
**Status:** design — implementation plan at `docs/plans/PLAN-22-01-http-streaming-results.md`.
**Predecessor:** `docs/specs/SPEC-19-streaming-runtime-pushdown.md`, whose Non-goals explicitly deferred exactly this increment.

## Problem

The #143 streaming runtime made the operator tree pull-based and batch-at-a-time
(`crates/sparql/src/exec/op/`), but the *boundary* still fully materializes:

- `Runtime::run` (`crates/sparql/src/exec/runtime.rs:32-40`) drains the whole
  operator tree into a `Vec<Bindings>` — decoding every `TermId` into a heap
  `Term` (`String`) up front — before returning.
- `api::execute_query` (`crates/sparql/src/api.rs:93-96`) collects that iterator
  into `QueryAnswer::Solutions { rows: Vec<Bindings> }`.
- The `/query` handler (`crates/sparql/src/server/query.rs:100-132`) then
  serializes the *entire* result set into one `String` and hands axum a
  single-shot body.

For a large SELECT result, peak memory is therefore ~3× the result set: slot
rows inside blocking operators (unavoidable for blocking plans), plus the fully
decoded `Vec<Bindings>`, plus the fully serialized body `String`. The last two
are pure boundary waste — the operator tree already yields ≤`batch_rows()`
(4096)-row chunks. This increment removes both: operator chunk → per-chunk
decode → per-chunk serialize → chunked HTTP frame.

## Consumers of the boundary (mapped, all verified 2026-07-06)

`Runtime::run` callers — **`run`'s signature does not change**, so none of
these churn:

| Caller | Site |
|---|---|
| `api::execute_query_with` (Select / Ask / Construct / Describe arms) | `src/api.rs:94,103,112,135` |
| `update::apply_delete_insert` (WHERE evaluation; must stay materialized — SPARQL 1.1 §3.1.3 pre-update snapshot) | `src/update.rs:582` |
| in-crate tests | `src/exec/runtime.rs` slot_differential (2063–2385), `src/plan/pushdown.rs` (656–875), `src/exec/op/chunk_tests.rs` (23, 34) |
| integration tests | `tests/exec_select.rs:43`, `tests/exec_ask.rs:28,41`, `tests/exec_construct.rs:28`, `tests/exec_property_paths.rs:50` |

`api::execute_query` / `QueryAnswer` consumers — **unchanged shape**:

| Caller | Notes |
|---|---|
| `src/server/query.rs::run` | restructured: SELECT takes the new streaming path, everything else keeps calling `execute_query` |
| `crates/python/src/graph.rs:206` | PyO3 binding; wants materialized results — untouched |
| `crates/sparql/examples/agg_profile.rs` | perf harness; untouched |
| `tests/api_end_to_end.rs` and friends | untouched |

The serializers (`src/results/{json,xml,csv,tsv}.rs`) are plain
`fn write_select_*(vars: &[String], rows: &[Bindings]) -> String` string
builders; they gain incremental header/chunk/footer forms and the existing
functions become thin wrappers (existing tests, including the sparesults
XML round-trip in `results/xml.rs` and `tests/results_json.rs`, stay green).

## The load-bearing constraint: nothing here is `Send`

Investigated, not assumed:

- `AppState<B>` holds `Arc<std::sync::RwLock<B>>` (`src/server/mod.rs:34-36`);
  the handler executes under `store.read().unwrap()`. A
  `std::sync::RwLockReadGuard` is **`!Send`** — it may not migrate across
  threads, so it cannot be held across an `.await` (the compiler rejects it in
  a work-stealing tokio runtime) and cannot be captured by a body stream that
  hyper polls from arbitrary worker threads.
- The operator tree is `Box<dyn Op + 'r>` with **no `Send` bound**
  (`src/exec/op/mod.rs:37-40,79`), and every streaming op holds
  `&'r Runtime<'a, E>` — i.e. it transitively borrows through the read guard.
  Adding `Send` bounds through `Op`/`Runtime`/`Executor` would be invasive and
  would still not make the guard sendable.

**Consequence:** execution, decode, and serialization must all stay on **one
thread** for the lifetime of the stream. The design therefore runs the whole
pipeline inside `tokio::task::spawn_blocking` (the closure captures the
`Arc<RwLock<B>>` clone — `Send` since `B: Send + Sync + 'static`, already
required by `AppState` — and takes the read guard *inside* the closure) and
ships already-serialized `Bytes` chunks to the async side over a **bounded
`tokio::sync::mpsc` channel**. Only `Bytes` crosses threads.

The response body is a small hand-rolled `http_body::Body` over the receiver
(`poll_recv` → `Frame::data`), in the style of the existing
`server/counting_body.rs` — **no new dependencies** (no `tokio-stream`, no
`http-body-util`, no `futures`); `axum 0.8`, `tokio`, `bytes`, `http-body`,
`pin-project-lite` are already `server`-feature deps.

Backpressure and cancellation fall out of the bounded channel: a slow client
blocks `blocking_send` (bounded buffering, ~8 chunks ≈ 8×4096 rows of
serialized text in flight); a disconnected client drops the receiver, which
makes `blocking_send` fail, which returns from the blocking closure and
**releases the read lock**.

### Accepted trade-off: the read lock is held while the client drains

Today the guard is scoped to execution only; serialization happens lock-free.
With streaming, the read guard lives until the last chunk is sent, so a slow
SELECT download blocks writers (`POST /update` takes the write lock) for its
duration. Concurrent *reads* are unaffected (shared read lock). This is
accepted: the alternative (decode+serialize everything under the lock, then
stream the buffered bytes) is exactly the materialization this increment
deletes. SPEC-02's MVCC snapshotting is the real fix; until then the bounded
channel caps how long a *dead* client can pin the lock (send blocks → recv
fails on disconnect), and a *slow-loris* reader can pin it — same class of
risk as any streaming endpoint over a lock, documented here deliberately.

## Design

### 1. Runtime seam: `run_stream`

`crates/sparql/src/exec/runtime.rs` gains a streaming handle; `run` is
reimplemented on top of it so its `Result<std::vec::IntoIter<Bindings>>`
signature — and every consumer in the table above — is untouched:

```rust
/// Streaming query handle: pulls one operator chunk at a time and decodes
/// it at the boundary. Borrows the `Runtime` (the operator tree holds
/// `&Runtime` internally), so keep the `Runtime` binding alive:
/// `let rt = Runtime::new(exec); let mut s = rt.run_stream(&plan)?;`
pub struct BindingsStream<'r, E: Executor + ?Sized> {
    exec: &'r E,
    op: Box<dyn Op + 'r>,
    buf: std::vec::IntoIter<Bindings>, // for the Iterator impl
}

impl<'r, E: Executor + ?Sized> BindingsStream<'r, E> {
    /// Decoded rows of the next operator chunk (≤ batch_rows() rows), or
    /// `None` at end of stream. Never `Some(empty)` (op invariant).
    pub fn next_chunk(&mut self) -> Result<Option<Vec<Bindings>>>;
}
impl Iterator for BindingsStream<...> { type Item = Result<Bindings>; ... }
```

`run_stream` applies `pushdown::rewrite` and `build` exactly as `run` does
today. The `Iterator` impl (row-at-a-time over an internal chunk buffer)
serves ASK and library callers; the HTTP path uses `next_chunk` so it
serializes chunk-at-a-time.

Bonus (cheap, in scope): the ASK arm of `execute_query_with` switches from
`run(...).next()` — which today still drains the *whole* result — to
`run_stream(...).next_chunk()?.is_some()`, restoring genuine early-exit for
streaming plans.

### 2. Incremental serializers

`src/results/mod.rs` gains:

```rust
/// Incremental SELECT serializer: header → zero or more row chunks → footer.
/// Concatenating header + chunks + footer over ANY chunking of the same rows
/// yields the same document as the one-shot `write_select_*` (which are now
/// implemented as wrappers over this trait).
pub trait SelectSerializer {
    fn header(&mut self, vars: &[String]) -> String;
    fn chunk(&mut self, vars: &[String], rows: &[Bindings]) -> String;
    fn footer(&mut self) -> String;
}
pub fn select_serializer(fmt: ResultFormat) -> Box<dyn SelectSerializer + Send>;
```

**All four formats stream** in this increment — each is trivially chunkable:

- **JSON**: header `{"head":{"vars":[…]},"results":{"bindings":[`, per-row
  objects with a `first_row_emitted` flag for comma placement across chunk
  boundaries, footer `]}}`.
- **XML**: header = prolog + `<sparql>` + `<head>` + `<results>` open; chunk =
  `<result>` blocks (stateless); footer closes `results`/`sparql`. XML is the
  crate's *default* Accept fallback (the LDBC SPB driver parses it with SAX —
  a streaming-friendly consumer), so excluding it would exclude the default.
- **CSV / TSV**: header row; chunks are self-contained lines; empty footer.

ASK bodies (`write_ask_json`/`write_ask_xml`) are one small document —
untouched.

### 3. HTTP layer

`src/api.rs` gains a planning-only entry point (no executor needed —
`planner::plan` takes only the algebra; `pushdown::rewrite` runs inside
`run_stream`; `count_bgp` is consulted at op-build time):

```rust
/// Parse → translate → plan a query for streaming execution, recording the
/// same Parse/Translate/Plan stage metrics as `execute_query`. Returns
/// `Some((projected_vars, plan))` for a plain SELECT; `None` for
/// ASK/CONSTRUCT/DESCRIBE/EXPLAIN (caller answers those via
/// `execute_query`, which re-parses — microseconds, and only on the
/// small-result forms).
pub fn plan_select(query: &str, cfg: &SparqlConfig)
    -> Result<Option<(Vec<String>, PhysicalPlan)>>;
```

`query_total{kind=select}` is bumped only on the `Some` path, so the
`execute_query` fallback keeps kind counts exact. The one distortion: a
non-SELECT query records one extra `Parse` stage observation (double parse).
Accepted and noted in `docs/metrics.md`.

`server/query.rs::run` becomes a router: `plan_select` → `Some` streams;
`None` falls through to today's materialized path verbatim. The streaming
path:

1. Async side: parse/translate/plan (`plan_select`), pick `ResultFormat`,
   create the bounded channel, `spawn_blocking`.
2. Blocking side: take the read guard, `Runtime::new(&*store)`,
   `run_stream(&plan)`, **pull and decode the first chunk before emitting
   anything** (see §4), then send `header+chunk₁` as the first `Bytes`
   message, then one message per subsequent chunk, then the footer. Record
   `stage_duration_seconds{stage=exec}` at first-chunk time and bump
   `query_errors{stage=exec}` on any exec/decode error.
3. Async side: `rx.recv().await` the first message. `Err` → clean HTTP 400
   (headers not yet sent). `Ok(bytes)` → `200` + `content-type` +
   `ChannelBody { first, rx }`.

`ChannelBody` (new `src/server/stream_body.rs`) implements
`http_body::Body<Data = Bytes, Error = SparqlError>`; `SparqlError` is
`std::error::Error + Send + Sync + 'static` (thiserror over `String`s +
`io::Error`), so it satisfies axum's `Into<BoxError>`. The existing
`CountingBody` middleware wraps it transparently, so response byte metrics
keep working.

### 4. Error-mid-stream semantics (the honest part)

Once the `200` + headers + partial body are on the wire, an exec/decode error
cannot become an HTTP 400/500. Neither SPARQL Results JSON nor XML has any
in-band way to express a trailing error. Per-phase behavior:

- **Before the first chunk** (parse/translate/plan errors on the async side;
  build/scan/first-chunk-decode errors on the blocking side): no bytes sent →
  clean **HTTP 400** with the error text, exactly like today. The
  first-chunk prebuffer exists precisely to widen this window to cover the
  most likely exec-time failures (scan/build/decode of chunk 1) at the cost
  of one chunk (≤4096 rows) of latency-to-headers.
- **After the first chunk** (all formats — JSON, XML, CSV, TSV): the body
  stream yields `Err`, which makes hyper **abort the response without the
  chunked-encoding terminator** (no `0\r\n\r\n`; on HTTP/2, RST_STREAM). Every
  HTTP/1.1+ client can detect the truncation at the protocol level; JSON/XML
  consumers additionally see a structurally unterminated document (no `]}}` /
  `</sparql>`). CSV/TSV consumers that ignore transfer-encoding errors could
  mistake a line-boundary truncation for a complete result — that is the
  trade-off, and it is inherent to streaming line-oriented formats; we choose
  **abort-and-truncate over silently swallowing the error** (emitting a
  well-formed-but-incomplete document would be worse: undetectable). The
  error is still observable server-side via `query_errors{stage=exec}`.

Mid-stream errors are rare by construction: MemStore plans materialize at
scan-build (errors surface pre-header), and for `HornBackend` the residual
mid-stream sources are per-chunk `decode_term` failures and per-chunk
`Filter`/`Extend` expression evaluation errors.

### 5. Query forms — scope

| Form | This increment | Why |
|---|---|---|
| SELECT | **streams** (all four formats) | the large-result case; the whole point |
| ASK | early-exit via `run_stream`, response stays one-shot | body is ~40 bytes; nothing to stream |
| CONSTRUCT / DESCRIBE | **deferred** (#TODO) | `construct_triples`/`describe_triples` (`exec/runtime.rs`) take `&[Bindings]` and DESCRIBE needs the full row set for seed expansion; N-Triples output *is* streamable in principle, but Stage-1 result sizes don't pay for the refactor |
| UPDATE | **out of scope, permanently** | §3.1.3 requires the WHERE solutions be computed over the pre-update graph before any mutation — materialization is semantics, not laziness (`update.rs:562-582`) |
| EXPLAIN | untouched | plan rendering, no result set |

### 6. Proving the memory win (and no perf regression)

- **Mechanism proof (CI-able):** a `--features server` integration test runs a
  >4096-row SELECT (5 000 MemStore triples — above `batch_rows()`, which is
  fixed at 4096 in non-`cfg(test)` builds; the `TEST_BATCH_ROWS` thread-local
  is unavailable to integration tests *and* wouldn't cross the
  `spawn_blocking` thread anyway) and asserts the body arrives in **≥2 data
  frames** whose concatenation parses as valid SPARQL JSON. Multiple frames ⇒
  the body was produced incrementally ⇒ the full serialized `String` never
  existed.
- **RSS measurement (hornbench only, per repo rule):** run the `serve` binary
  against a large synthetic graph, `curl` a full-scan
  `SELECT * WHERE { ?s ?p ?o }` (≥5 M rows), record the server's peak RSS
  (`/usr/bin/time -l` equivalent on the Linux host: `/usr/bin/time -v`,
  `Maximum resident set size`) on `main` vs this branch. Expected: the branch
  peak drops by roughly the decoded-`Bindings` + serialized-body share of the
  result set; the id-row materialization inside the scan seam remains (that
  was a #143 non-goal and still is — `scan_bgp_ids` returns a whole `Batch`
  of compact u64 slots).
- **No-regression:** SPB-256 `aggregation-qps` and `editorial-qps` on
  hornbench must hold (~30.8 / GraphDB ~153 baseline, docs/benchmarks.md).
  SPB results are small (aggregation answers are a handful of rows), so this
  increment targets **large SELECT memory**, not SPB throughput — say so when
  recording. The per-request cost added is one `spawn_blocking` dispatch and
  one bounded-channel hop; measured, not assumed, via the nightly/manual SPB
  run before merge.

### 7. Non-goals

- Streaming CONSTRUCT/DESCRIBE (deferred above, #TODO).
- Streaming the scan leaf / WCOJ row production (unchanged #143 non-goal).
- Probe-side streaming for `Join` (TASKS.md remaining item 1 — separate).
- MVCC / not holding the read lock across the response (SPEC-02).
- HTTP trailers as an error side-channel (axum supports them; no SPARQL
  client reads them — not worth the surface).
- Content-negotiation changes; `ResultFormat::from_accept` is untouched.

### 8. Testing strategy

- `slot_differential` proptests: untouched (they test below the boundary).
- New runtime unit tests: `run_stream` chunk-concatenation ≡ `run` output
  under a tiny `TEST_BATCH_ROWS` (multi-chunk forcing), including error
  propagation.
- New serializer tests: for each format, one-shot output ≡ header + chunks +
  footer under several chunk splits (1-row, 2-row, all-rows).
- Server (`--features server`): existing `tests/server_http.rs` suite is the
  HTTP contract gate and must stay green unmodified; new tests add (a) the
  ≥2-frame chunked-response assertion, (b) pre-header error → 400 via a test
  backend whose scan fails, (c) mid-stream error → 200 + body read error via
  a test backend that yields `Slot::Id` rows whose `decode_term` fails past
  row 4096.
- Final gate: `cargo nextest run -p horndb-sparql` and
  `cargo nextest run -p horndb-sparql --features server` green; clippy/fmt
  clean; bench numbers only from hornbench.

### 9. Risks

- **Lock-hold duration** (§ trade-off above) — documented, MVCC-bound.
- **First-chunk latency**: headers wait for chunk 1. For a blocking top
  operator (ORDER BY / GROUP BY) that is the whole query anyway; for
  streaming plans it is ≤4096 rows of work — negligible vs the 400-capture
  win.
- **Metrics drift**: `stage=exec` now means "to first chunk" for streamed
  SELECTs (full drain remains visible in `request_duration_seconds`); one
  extra `Parse` observation per non-SELECT query. Both noted in
  `docs/metrics.md` in the same commit that changes the emit sites.
- **spawn_blocking pool pressure**: each in-flight streamed SELECT occupies a
  blocking thread for its duration (tokio default cap 512). Same order as
  today's handler concurrency; acceptable at Stage 1.
