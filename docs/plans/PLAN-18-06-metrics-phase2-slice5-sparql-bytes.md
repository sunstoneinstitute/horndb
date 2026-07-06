---
status: executed
date: 2026-06-29
scope: "Metrics Phase 2 — Slice 5 (SPARQL request/response bytes)"
---

# Metrics Phase 2 — Slice 5 (SPARQL request/response bytes) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development. Checkbox steps.

**Goal:** Add the SPARQL HTTP request/response **byte** counters that Slice 1 deferred — via a body-counting `http_body::Body` wrapper (not a header guess), so the count is exact and robust to future streaming responses.

**Architecture:** Add two `Family<EndpointLabel, Counter>` series to `SparqlMetrics`. In the existing `record_request` middleware (`crates/sparql/src/server/mod.rs`), when the path maps to a known `Endpoint`, wrap the **request** body and the **response** body in a `CountingBody` that tallies data-frame bytes and `inc_by`s the appropriate counter once, when the body reaches end-of-stream. Request bytes are tallied when the handler's `String` extractor drains the body; response bytes when hyper (or a test's `to_bytes`) drains the response. No separate tower `Layer` is needed — reuse the existing middleware and its endpoint classification.

**Tech Stack:** Rust 1.90, prometheus-client 0.25, axum 0.8, tower 0.5, http-body 1.0, bytes 1, `horndb-metrics`, `horndb-sparql` (`server` feature).

**Reference spec:** `docs/specs/SPEC-18-metrics.md` §7.1 (deferral note), §7.2 (the fan-out item). Epic: #148. Note commit `d2cace9` removed a permanently-zero `response_bytes` series in Slice 1; this slice adds it back properly.

**Branch:** `feat/metrics-phase2-sparql-bytes`, stacked on `feat/metrics-phase2-wcoj`.

---

## Metric inventory (names omit `horndb_`)

| Metric | Type | Source |
|---|---|---|
| `sparql_request_bytes_total{endpoint}` | Counter (Family) | data bytes of the request body, per endpoint |
| `sparql_response_bytes_total{endpoint}` | Counter (Family) | data bytes of the response body, per endpoint |

`endpoint` reuses the existing `Endpoint`/`EndpointLabel` (query/update/metrics) — bounded.

---

## File Structure

- `crates/metrics/src/sparql.rs` — add `request_bytes` + `response_bytes` families to `SparqlMetrics` (struct, `register`, returned `Self`); remove the deferral comment.
- `crates/sparql/src/server/counting_body.rs` — **new**: `CountingBody<B>` + a `Direction` enum + the observe helper.
- `crates/sparql/src/server/mod.rs` — wrap request + response bodies in `record_request`; add `mod counting_body;`.
- `crates/sparql/Cargo.toml` — add deps the wrapper needs if not already present (`http-body`, `bytes`; possibly `pin-project-lite`) under the `server` feature.
- Tests: extend `crates/sparql/tests/server_http.rs` (`--features server`).

---

## Task 1: Add the byte counter families to `SparqlMetrics`

**Files:** `crates/metrics/src/sparql.rs`; test inline or in the existing metrics test.

- [ ] **Step 1:** Add to `SparqlMetrics`: `pub request_bytes: Family<EndpointLabel, Counter>,` and `pub response_bytes: Family<EndpointLabel, Counter>,`. In `register`, construct `Family::<EndpointLabel, Counter>::default()` for each and `reg.register("sparql_request_bytes", "SPARQL request body bytes", request_bytes.clone());` / `reg.register("sparql_response_bytes", "SPARQL response body bytes", response_bytes.clone());`. Add both to the returned `Self`. Remove the now-obsolete "Response-byte accounting is deferred…" comment.
- [ ] **Step 2:** Extend the metrics-crate test (or add one) to observe `request_bytes`/`response_bytes` for an endpoint and assert `horndb_sparql_request_bytes_total`, `horndb_sparql_response_bytes_total`, and `endpoint="query"` appear in `encode_metrics()`.
- [ ] **Step 3:** `cargo nextest run -p horndb-metrics` PASS; clippy clean.
- [ ] **Step 4:** Commit `feat(metrics): add sparql request/response byte counters`.

## Task 2: `CountingBody` + wire into the middleware

**Files:** `crates/sparql/Cargo.toml`, `crates/sparql/src/server/counting_body.rs` (new), `crates/sparql/src/server/mod.rs`; test `crates/sparql/tests/server_http.rs`.

> READ `crates/sparql/src/server/mod.rs` `record_request` first. It computes `endpoint: Option<Endpoint>`, `method`, runs `next.run(req)`, then emits the request counter/latency. You will wrap `req`'s body before `next.run` and `resp`'s body after.

- [ ] **Step 1: Deps.** In `crates/sparql/Cargo.toml`, ensure the `server` feature can use `http-body` (1.0), `bytes` (1), and a pinning helper. Add to `[dependencies]` (optional, gated into `server`) whatever is not already there: `http-body = "1"`, `bytes = "1"`, `pin-project-lite = "0.2"`. Prefer workspace deps — if these are already in the root `[workspace.dependencies]`, use `.workspace = true`; otherwise add them there. Add the corresponding `dep:` entries to the `server` feature list.

- [ ] **Step 2: Failing test.** In `crates/sparql/tests/server_http.rs`, add a test that POSTs a known-length query/update body and fully drains the response via `axum::body::to_bytes`, then asserts `horndb_metrics::encode_metrics()` shows `horndb_sparql_request_bytes_total{endpoint="..."}` with a value `>= the request body length` and `horndb_sparql_response_bytes_total` with `>= 1` (parse the labelled counter value). The body must be fully drained for the response counter to fire (end-of-stream). Run with `cargo nextest run -p horndb-sparql --features server` — confirm FAIL.
  > Parse a labelled counter line like `horndb_sparql_request_bytes_total{endpoint="update"} 42` — match the metric name + the `endpoint="..."` substring, take the trailing number.

- [ ] **Step 3: Implement `CountingBody`.** Create `crates/sparql/src/server/counting_body.rs`:
  ```rust
  use std::pin::Pin;
  use std::task::{Context, Poll};
  use bytes::Bytes;
  use http_body::{Body, Frame, SizeHint};
  use horndb_metrics::labels::{Endpoint, EndpointLabel};

  #[derive(Clone, Copy)]
  pub enum Direction { Request, Response }

  pin_project_lite::pin_project! {
      pub struct CountingBody<B> {
          #[pin] inner: B,
          bytes: u64,
          endpoint: Endpoint,
          dir: Direction,
          done: bool,
      }
  }

  impl<B> CountingBody<B> {
      pub fn new(inner: B, endpoint: Endpoint, dir: Direction) -> Self {
          Self { inner, bytes: 0, endpoint, dir, done: false }
      }
  }

  fn observe(endpoint: &Endpoint, dir: Direction, bytes: u64) {
      let m = horndb_metrics::metrics();
      let fam = match dir {
          Direction::Request => &m.sparql.request_bytes,
          Direction::Response => &m.sparql.response_bytes,
      };
      fam.get_or_create(&EndpointLabel { endpoint: endpoint.clone() }).inc_by(bytes);
  }

  impl<B> Body for CountingBody<B>
  where
      B: Body<Data = Bytes>,
  {
      type Data = Bytes;
      type Error = B::Error;

      fn poll_frame(
          self: Pin<&mut Self>,
          cx: &mut Context<'_>,
      ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
          let this = self.project();
          match this.inner.poll_frame(cx) {
              Poll::Ready(Some(Ok(frame))) => {
                  if let Some(d) = frame.data_ref() {
                      *this.bytes += d.len() as u64;
                  }
                  Poll::Ready(Some(Ok(frame)))
              }
              Poll::Ready(None) => {
                  if !*this.done {
                      *this.done = true;
                      observe(this.endpoint, *this.dir, *this.bytes);
                  }
                  Poll::Ready(None)
              }
              other => other,
          }
      }

      fn size_hint(&self) -> SizeHint { self.inner.size_hint() }
      fn is_end_stream(&self) -> bool { self.inner.is_end_stream() }
  }
  ```
  > VERIFY the http-body 1.0 API: `poll_frame`, `Frame::data_ref()`, `SizeHint`. If `Frame` has `into_data`/`is_data` instead, adapt. The `Counter::inc_by(u64)` signature must match (it does for the default u64 Counter). If `pin-project-lite` is unwanted, an alternative is requiring `B: Unpin` and using `Pin::new(&mut self.inner)` — but only if axum's body type is `Unpin`; prefer pin-project-lite for safety.

- [ ] **Step 4: Wire into `record_request`** (`crates/sparql/src/server/mod.rs`): add `mod counting_body;` and `use counting_body::{CountingBody, Direction};`. When `endpoint` is `Some(ep)`:
  - Before `next.run`: replace the request body with a counted one:
    ```rust
    let req = {
        let (parts, body) = req.into_parts();
        let counted = CountingBody::new(body, ep.clone(), Direction::Request);
        axum::http::Request::from_parts(parts, axum::body::Body::new(counted))
    };
    ```
  - After `next.run` returns `resp`: wrap the response body:
    ```rust
    let resp = {
        let (parts, body) = resp.into_parts();
        let counted = CountingBody::new(body, ep.clone(), Direction::Response);
        axum::response::Response::from_parts(parts, axum::body::Body::new(counted))
    };
    ```
  Keep the existing latency/requests emission. When `endpoint` is `None`, do not wrap (pass through). Ensure the existing `endpoint` variable usage still compiles (it is consumed by `RequestLabels` later — clone as needed).
  > `axum::body::Body::new` requires the wrapped body's `Error: Into<BoxError>` — axum's `Body` error satisfies this. Confirm it compiles.

- [ ] **Step 5: Verify.** `cargo nextest run -p horndb-sparql --features server` — new test passes, all existing server tests still pass. `cargo clippy -p horndb-sparql --all-targets --features server -- -D warnings` clean. Also confirm a plain `cargo build -p horndb-sparql` (default features include `server`) builds.
- [ ] **Step 6: Commit** `feat(metrics): count sparql request/response body bytes via CountingBody layer`.

## Task 3: Docs sync + verification

**Files:** `docs/architecture.md` (§15), `TASKS.md`, `docs/index.md`.

- [ ] **Step 1:** architecture.md §15 — the byte counters move to implemented; the fan-out is now COMPLETE (no remaining Phase-2 fan-out items; only OTel traces/logs remain, deferred to a later phase). Update the Status line and the planned/remaining wording accordingly (Phase 2 fully landed).
- [ ] **Step 2:** TASKS.md — mark the SPARQL request/response byte item done; note Phase-2 fan-out complete. If the whole metrics epic's fan-out is now done, reflect that (keep the OTel "later phase" note). No GitHub issue edits.
- [ ] **Step 3:** docs/index.md — add a pointer to this plan; update the remaining-fan-out parenthetical to "complete (OTel deferred)". Commit the plan file.
- [ ] **Step 4:** `cargo fmt --all`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo nextest run -p horndb-metrics -p horndb-sparql --features server`. Clean/PASS. Commit any Cargo.lock change.
- [ ] **Step 5:** Commit `docs(metrics): record Phase-2 sparql-bytes slice; fan-out complete (#148)`.

---

## Self-Review checklist
- §7.2 final item: request/response byte counters via a body-counting wrapper ✓ (exact, robust to streaming — not a header guess).
- `endpoint` label bounded (query/update/metrics). ✓
- Counted exactly once per body (on end-of-stream `None`, guarded by `done`). Request counts when the handler drains the body; response when hyper/test drains it. ✓
- Unknown paths not wrapped. ✓
- No behavior change to responses (same status/body content; the wrapper is transparent). Existing server tests green. ✓
- The Slice-1 deferral comment removed; `response_bytes` re-added properly (vs the zero-series removed in `d2cace9`).

## Execution handoff
subagent-driven-development; stacked PR against `feat/metrics-phase2-wcoj`; do not merge; tick the #148 sparql-bytes box (and note the fan-out is complete) when green.
