# `horndb-ml` — integration notes (SPEC-08)

Decisions that are not in the spec but constrain the public API. Read
before changing the surface.

## Boundary discipline

The symbolic engine is the source of truth (SPEC-00 / SPEC-08). This
crate only *proposes* (candidate generation, re-verified symbolically)
and *advises* (planner, hot-set, NL→SPARQL translation). Nothing here is
on the correctness path; with `MlConfig.enabled = false` every accessor
returns the `Disabled*` no-op and the engine is bit-identical to a non-ML
build (NF1).

## HTTP boundary (F3 `/nl-query`, F6 `/ml-audit`) — `server` feature

The HTTP endpoints live **inside this crate**, behind the off-by-default
`server` feature (axum + tokio + serde). Rationale:

- It keeps the in-process traits (`CandidateGenerator`, `PlanAdvisor`,
  `HotSetAdvisor`, `Translator`) dependency-light — a consumer that only
  wants the traits does not pull in axum/tokio.
- It avoids a dependency cycle / heavy coupling between `horndb-ml` and
  `horndb-sparql`. The SPARQL stack is reached through the
  `SparqlExecutor` trait, injected at the call site, so `horndb-ml` does
  **not** depend on `horndb-sparql`. The `serve` binary (SPEC-07) is the
  natural place to wire a real executor and `Router::merge` the ML router.

### NL → SPARQL translation (`nlquery.rs`)

- The LLM call is **never** bundled. It lives behind the `Translator`
  trait. Production supplies an impl that calls the chosen provider's API.
- Tests use `MockTranslator` (deterministic, offline) and a fake
  `SparqlExecutor`, so `cargo test -p horndb-ml --features server` is
  fully hermetic — no network, no live model. This is a hard requirement:
  do not introduce a test that calls a real LLM.
- `generated_sparql` is **always** returned, even on execution failure or
  `dry_run`, so a human can audit/correct what the model produced
  (SPEC-08 F3, "LLM SPARQL quality" risk). Execution failures return
  HTTP 200 with `execution_error` set and `results: null` — the
  translation succeeded; only the run failed.
- Status codes: disabled/no translator → 503 (fail closed, never guess);
  upstream model error → 502; empty translation → 422; empty question →
  400.

### Cost reporting

`CostReport` (prompt/completion tokens + estimated USD) is surfaced in the
`/nl-query` response. The engine does **not** assume a price — the
`Translator` computes `estimated_usd` from its own price card. This
addresses the SPEC-08 "Cost transparency" risk.

### Training-data leakage controls (`config::LlmPrivacy`)

`MlConfig.llm_privacy` governs whether the raw question text is retained:

- Default is `no_retention()` — the question is never persisted/echoed.
- `retain_questions()` keeps the literal text (operator accepts the
  upstream-logging risk).
- `redact_in_logs` records that *a* query happened (and its length/cost)
  without the literal content — useful where audit volume matters but PII
  must not be stored.

`LlmPrivacy::loggable_text` is the single chokepoint the endpoint calls
before writing any telemetry, so the policy cannot be bypassed by a
forgetful call site. This addresses the SPEC-08 "Training-data leakage"
and "Privacy / GDPR" risks.

### CI

`cargo test --workspace` runs default features (server off), so the
server tests are run by a dedicated CI step:
`cargo test -p horndb-ml --features server` plus a matching server-feature
clippy step (mirrors the `horndb-sparql --features server` pattern).

## Deferred (still open under epic #8)

- **Real FAISS-backed `CandidateGenerator`** + the synthetic
  entity-resolution acceptance (≥10× speedup over brute force, ≥99%
  false-positive rejection by symbolic re-verification). Native FAISS
  linkage is heavy (like the GraphBLAS vendoring) and separable from the
  HTTP boundary; it is its own increment.
- Wiring the ML router into the `serve` binary with a real
  `SparqlExecutor` over SPEC-07, and a production `Translator` against a
  live provider. The seam (`MlAppState`, `build_router`, the two traits)
  is ready for that wiring.
