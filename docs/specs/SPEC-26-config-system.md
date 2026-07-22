---
status: draft
date: 2026-07-22
scope: "Operator configuration system — layered TOML config files (base + config.d), live watch/reload, and a two-tier server-vs-query settings model with per-query overrides (URL params + a SETTINGS clause); wires bind, query timeout, result-row cap, and rdf12 to real config. Per-session memory accounting is delegated to a companion spec."
---

# SPEC-26 — Configuration system

**One-line thesis:** HornDB has no config surface today — the `serve` binary
takes a handful of `clap` flags with hardcoded defaults, and nothing else is
tunable. This spec gives operators a real, layered config: a base TOML file plus
a `config.d/` drop-in directory, merged by a documented precedence, watched and
re-applied live where safe; and it gives query authors a way to override a
bounded set of settings per query, with server config supplying the defaults.

**Refines:** the SPEC-07 SPARQL frontend server surface (the `serve` binary and
the axum layer in `crates/sparql/src/server/`). It does not change SPARQL query
semantics; it changes how the server and individual queries are *configured*.
Cross-cuts SPEC-17 (metrics) — the config layer emits its own reload metrics.

**Companion:** per-session memory accounting is a separate, larger subsystem
(allocation tracking threaded through the join and storage layers). This spec
defines the `max_session_memory` *knob* — parsed, stored, and surfaced — but its
enforcement is delegated to a companion spec written when that work is picked up.
Stateful cross-request sessions (a `SET VARIABLE` that persists across HTTP
requests) are likewise designed-for but deferred (see Non-goals and Risks).

## Problem — what exists today, and where it stops

The only configuration surface the running server has is command-line flags on
the `serve` binary (`crates/sparql/src/bin/serve.rs`):

- `--data <paths...>` — RDF files/dirs to load at startup (required).
- `--bind <addr>` — **hardcoded default** `127.0.0.1:3840`.
- `--materialize` — run OWL 2 RL forward-chaining before serving.

Where it stops:

- **No config files, no env config.** Nothing is read from a file or an
  environment variable. An operator cannot set anything without editing the
  command line. The only product-facing runtime env vars anywhere are the two
  SIMD knobs (`HORNDB_SIMD_MAX_ISA`, `HORNDB_SIMD_AUTOTUNE`); there is no
  general mechanism.
- **No live reconfiguration.** Every knob is fixed at process start.
- **No resource limits.** There is no query timeout, no result-row cap, no
  memory budget, and no per-query or per-session settings of any kind. A
  `wcoj::CancelToken` exists (`crates/wcoj/src/cancel.rs`, polled ~every 2048
  rows) but nothing drives it on a deadline.
- **`SparqlConfig` is plumbed but unused.** `SparqlConfig` (`crates/sparql/src/lib.rs`)
  carries a single `rdf12` flag and the library API can vary it per request, but
  the HTTP layer always passes `SparqlConfig::default()` — the per-request path
  is dead code from the server's point of view.

## Non-goals

- **Per-session memory accounting and enforcement.** This spec parses and stores
  `max_session_memory` but does not enforce it. Real accounting (allocation
  tracking across `wcoj`/`storage`, per-query attribution, over-budget abort) is
  the companion spec's job. Until then the knob is accepted and surfaced with a
  metric, and documented as not-yet-enforced.
- **Stateful cross-request sessions.** A `SET VARIABLE k=v` that persists across
  multiple HTTP requests needs a session identity (a `session_id`), a
  server-side session registry, and TTL/eviction. The settings *model* here is
  shaped to admit it later, but SPEC-26 ships only server-scoped config and
  query-scoped overrides — no persistent session state.
- **Reconfiguring the data set live.** Changing `--data` / loaded corpora at
  runtime (hot add/drop of graphs) is not in scope; a changed data path is
  restart-only (see S3).
- **A config RPC / admin API.** Config changes come from files (operator edits)
  and per-query overrides (query authors). There is no HTTP endpoint to mutate
  server config at runtime in this spec.
- **Secret management.** No integration with secret stores/vaults. Config values
  are plaintext TOML; secret handling is a later concern if a value ever needs
  it.

## Requirements

### S1. Layered config-file resolution and merge

Assemble one effective `ServerConfig` from built-in defaults plus operator files.

- **Format: TOML.** Config files are TOML (`config.toml`). The workspace already
  depends on `toml` + `serde`; no new parser is added.
- **Main-file location precedence, highest wins:** `--config <path>` (CLI flag) >
  `HORNDB_CONFIG` (env var) > `/etc/horndb/config.toml` (built-in default path).
  - A missing file at the **default** path is not an error — the server runs on
    built-in defaults. A missing file at an **explicitly requested** path
    (`--config` or `HORNDB_CONFIG`) is a fatal startup error.
- **Drop-in directory.** The base file may set `config_dir` (default
  `/etc/horndb/config.d/`). Every `*.toml` file directly in that directory is a
  fragment. A missing/empty `config_dir` is not an error.
- **Merge order, lowest → highest precedence:**
  1. built-in defaults (compiled in),
  2. the base `config.toml`,
  3. `config.d/*.toml` fragments in **lexical filename order** (so `99-*`
     overrides `00-*`).
- **Merge semantics:** deep-merge of TOML tables. For a table, keys union and
  recurse. For a scalar or an array, the higher-precedence value **replaces** the
  lower (arrays do not concatenate — a `99-*` fragment fully replaces an array,
  which is the predictable operator-override behavior).
- **Validation.** The merged tree is deserialized into the typed `ServerConfig`.
  Unknown keys and type/range errors are rejected with a message naming the file
  and key. At startup a rejection is fatal (non-zero exit); on reload it is
  handled per S3 (keep-and-log).

### S2. Config model — two tiers

Separate server-scoped config from the bounded set a query may override.

- **`ServerConfig`** — the whole merged tree, the server-scoped tier. Sections:
  - `[server]` — network identity: `bind` (default `127.0.0.1:3840`). Additional
    server-only fields (e.g. `config_dir`) live here. **Restart-only** (S3).
  - `[server.limits]` — the **defaults** for every query-overridable setting:
    `query_timeout` (duration, default `30s`), `max_result_rows` (integer,
    default `1_000_000`), `rdf12` (bool, default `false`), `max_session_memory`
    (byte size, default unset/unlimited; parsed and stored, enforcement
    delegated). **Hot-reloadable.**
  - `[logging]` — `level` (default `info`). **Hot-reloadable.**
  - `[reload]` — `debounce` (duration, default `250ms`). **Hot-reloadable.**
- **`QuerySettings`** — the query-scoped tier: the overridable subset
  (`query_timeout`, `max_result_rows`, `rdf12`, `max_session_memory`). It is
  constructed per query by layering overrides (S4) on top of the current
  `[server.limits]` defaults.
- **Durations and byte sizes** parse from human strings (`"30s"`, `"2GiB"`) via
  small typed newtypes with `serde` deserializers, reused for file values, URL
  params, and the `SETTINGS` clause so the three channels accept identical
  syntax.

### S3. Live watch and reload

Re-apply config when files change, without dropping the running server.

- **Watcher.** A `notify`-based watcher observes the base file and `config_dir`.
  Events are debounced by `[reload].debounce`.
- **Reload cycle.** On a settled change: re-resolve (S1) → re-merge → validate →
  on success, atomically publish the new `ServerConfig` via an `ArcSwap`; request
  handlers read a cheap snapshot per request. On validation failure, **keep the
  current config**, log the error (file + key), and increment the rejected-reload
  metric. A bad edit never takes the server down or leaves it half-applied.
- **Hot vs restart-only.** `[server.limits]`, `[logging]`, `[reload]` take effect
  on the next request/operation after a successful reload. `[server].bind` and
  the `--data` corpora are **restart-only**: a changed restart-only key is stored
  in the new `ServerConfig` (so a later restart uses it) but a log line states it
  "requires restart to take effect" — the server never silently claims a
  restart-only change went live.
- **Generation.** Each successfully applied config carries a monotonically
  increasing generation number, exposed as a metric (S6) for operator confidence.

### S4. Query-scoped overrides

Let a query override the bounded `QuerySettings` subset, defaulting from
`[server.limits]`.

- **Two channels, both per-query:**
  1. **URL query parameters** on `/query`, e.g.
     `?query_timeout=30s&max_result_rows=1000`.
  2. **A `SETTINGS` clause** appended to the SPARQL text, ClickHouse-style:
     `SELECT ... WHERE { ... } SETTINGS query_timeout = 10s, max_result_rows = 500`.
     The clause is stripped from the tail of the request string before the query
     reaches `spargebra`, and parsed by a small `key = value {, key = value}`
     grammar in `horndb-config`.
- **Precedence, highest wins:** `SETTINGS` clause > URL query param >
  `[server.limits]` default. Rationale: the clause is embedded by the query
  author and is the most specific statement of intent.
- **Unknown or out-of-range override.** An unknown setting key or an unparseable
  value is a per-query client error (HTTP 400) naming the offending key — it does
  not affect server config or other queries.
- **Only the whitelisted subset is overridable.** Server-only keys (`bind`,
  `config_dir`, `logging`, `reload`) are never settable per query.

### S5. Enforcement wired in this spec

Make the settings real for everything except memory.

- **`query_timeout`.** The server layer spawns a timer bound to the query's
  `wcoj::CancelToken`; at the deadline it cancels, and the in-flight query ends
  with a typed "query timeout exceeded" error. `wcoj` gains no config dependency —
  the timer lives in `crates/sparql`.
- **`max_result_rows`.** A counter in the result stream; on overflow the stream
  ends with a typed "result row limit exceeded" error rather than silently
  truncating.
- **`rdf12`.** Flips the already-plumbed `SparqlConfig.rdf12` per query — this
  finally makes the existing per-request path live from the HTTP layer.
- **`max_session_memory`.** Parsed, stored on `QuerySettings`, and surfaced, but
  enforcement is a no-op stub with a metric. The companion memory-accounting spec
  turns the stub into real over-budget aborts. Documented at the API as
  accepted-but-not-yet-enforced.

### S6. Crate, CLI, and observability

- **Crate.** A new foundation crate **`horndb-config`** owns file resolution, the
  `config.d` merge, the watcher, the `ArcSwap` live handle, the typed
  `ServerConfig`/`QuerySettings` structs, and the duration/byte-size newtypes and
  the `SETTINGS`-clause parser. New workspace dependencies: `notify` and
  `arc-swap` (both added to `[workspace.dependencies]`). `horndb-sparql` depends
  on it; the companion memory spec and any future consumer depend on it later.
- **CLI integration.** `serve` gains `--config <path>`. A server-scoped setting
  given explicitly on the CLI (e.g. `--bind`) overrides the merged file value —
  CLI flag > merged config files — so existing invocations keep working.
- **Metrics** (added to `crates/metrics/` with the matching `docs/metrics.md`
  rows in the **same commit**, per the root sync rule):
  - `config_reload_total{result="applied|rejected"}` — counter.
  - `config_active_generation` — gauge (the applied generation from S3).
  - `config_last_reload_unixtime` — gauge.

## Phasing

Each phase is independently shippable and harness-gated (the SPEC-01 selected
subset stays green throughout; existing `sparql` server tests extend rather than
regress). Implementation plans (`PLAN-26-MM-*.md`) are written when each
increment is picked up; tracking issues are filed then (use `#TODO` until filed).

1. **Static layered config (S1, S2, S6-crate/CLI/startup-validation).** The
   `horndb-config` crate, file resolution + `config.d` merge, the typed model,
   `serve --config`, and startup validation. `bind` now comes from config. No
   watcher yet, no query overrides yet. *(tracking: `#TODO`)*
2. **Query-scoped overrides + enforcement (S4, S5, S6-metrics for rejects).**
   URL params, the `SETTINGS` clause, precedence, and real enforcement of
   `query_timeout` / `max_result_rows` / `rdf12`; `max_session_memory` stub.
   *(tracking: `#TODO`)*
3. **Live watch and reload (S3, remaining S6 metrics).** The `notify` watcher,
   debounce, `ArcSwap` publish, keep-and-log on bad reload, generation metric,
   hot-vs-restart-only handling. *(tracking: `#TODO`)*

Phase 1 stands alone and delivers immediate value (operators get a config file).
Phase 2 depends on Phase 1's model. Phase 3 depends on Phase 1's resolution/merge
and is orthogonal to Phase 2.

## Acceptance criteria

1. **Resolution precedence holds (S1).** A test proves `--config` > `HORNDB_CONFIG`
   > `/etc/horndb/config.toml`; a missing default path runs on defaults, a missing
   explicit path is a fatal error.
2. **Merge precedence holds (S1).** A `config.d/99-*.toml` fragment overrides a
   `00-*.toml` fragment and the base file; tables deep-merge, scalars and arrays
   replace — all verified against fixture directories.
3. **Startup validation is honest (S1/S2).** An unknown key or out-of-range value
   fails startup with a non-zero exit and a message naming the file and key.
4. **`bind` comes from config, CLI still wins (S6).** With no flag, the server
   binds the config value; with `--bind`, the flag overrides the file.
5. **Query overrides work and are ordered (S4).** A test proves
   `SETTINGS query_timeout=…` > `?query_timeout=…` > `[server.limits]` default; an
   unknown/invalid override yields HTTP 400 naming the key without disturbing
   server config.
6. **Enforcement is real (S5).** `SETTINGS query_timeout=…` cancels a long-running
   query via the `CancelToken`; `max_result_rows` ends an over-cap stream with a
   typed error; `rdf12` per query flips RDF 1.2 acceptance. `max_session_memory`
   is accepted and surfaced but documented as not-yet-enforced.
7. **Live reload is safe (S3).** Editing a hot key (`[server.limits]`,
   `[logging]`) takes effect within the debounce window and bumps
   `config_active_generation` and `config_reload_total{result="applied"}`; editing
   a file into an invalid state keeps the previous config live and bumps
   `config_reload_total{result="rejected"}`. A changed `bind` logs
   "requires restart" and does not rebind.
8. **Docs stay in sync (in-commit).** `docs/metrics.md`, `docs/architecture.md`,
   `docs/specs/README.md`, and `docs/index.md` are updated in the commits that
   introduce the corresponding behavior, per the root sync rules.

## Risks and open questions

- **`SETTINGS`-clause parsing vs. the SPARQL grammar.** Stripping a trailing
  `SETTINGS …` clause off the query string before `spargebra` sees it must not
  misfire on a legitimate query that contains the token `SETTINGS` (e.g. as an
  IRI local name or a string literal). Mitigation: only recognize the clause as a
  suffix after the outermost query form, and bench it against the SPARQL test
  corpus; if ambiguity remains, gate the clause behind the URL-param channel and
  reconsider. This is the highest-risk grammar decision in the spec.
- **Watcher portability and editor atomic-save patterns.** `notify` semantics
  differ across platforms and editors (rename-into-place vs. truncate-in-place vs.
  multiple events per save). The debounce plus full re-resolve-and-validate on any
  event is chosen precisely so the reload is idempotent and insensitive to event
  shape, but the watcher must re-establish itself if the watched file is replaced
  by rename. Verify on Linux (the deploy target) and macOS (dev).
- **Duration/byte-size syntax scope.** The human-string parsers (`"30s"`,
  `"2GiB"`) must be pinned to one unambiguous grammar (binary vs decimal byte
  units, allowed duration suffixes) and shared verbatim across files, URL params,
  and `SETTINGS`, or the three channels will drift. Settle the grammar in the
  Phase-1 plan.
- **Deferred memory knob honesty.** Accepting `max_session_memory` while not
  enforcing it risks operators believing they are protected. Mitigation: the
  accepted-but-not-enforced state is documented at the API and (optionally) a
  one-time log line notes it; the companion spec removes the gap.
- **Deferred session tier shape.** Deferring stateful `SET VARIABLE` is only cheap
  if the `QuerySettings` model does not have to change to admit it later. Keep the
  override-layering design (defaults → session → query) explicit in the code even
  though the session layer is absent, so adding it is a new layer, not a rewrite.
- **Restart-only reload UX.** Storing a changed restart-only value (so a later
  restart honors it) while not applying it live is the least-surprising behavior,
  but an operator watching only the generation metric could think `bind` changed.
  The "requires restart" log line is the mitigation; consider a dedicated metric
  label if this proves confusing in practice.
