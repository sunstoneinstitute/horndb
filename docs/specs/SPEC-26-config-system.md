---
status: draft
date: 2026-07-22
scope: "Operator configuration system — layered config (built-in defaults < base config.toml < config.d/*.toml < env vars < argv), live watch/reload, and a two-tier server-vs-query settings model with per-query overrides via URL query parameters; wires bind, the SIMD knobs, query timeout, result-row cap, and rdf12 to real config. Per-query memory accounting is delegated to a companion spec."
---

# SPEC-26 — Configuration system

**One-line thesis:** HornDB has no config surface today — the `serve` binary
takes a handful of `clap` flags with hardcoded defaults, and nothing else is
tunable. This spec gives operators a real, layered config: a base TOML file plus
a `config.d/` drop-in directory (with environment variables and command-line
flags as higher-precedence overrides), merged by a documented precedence, watched
and re-applied live where safe; and it gives query authors a way to override a
bounded set of settings per query via URL query parameters, with server config
supplying the defaults.

**Refines:** the SPEC-07 SPARQL frontend server surface (the `serve` binary and
the axum layer in `crates/sparql/src/server/`). It does not change SPARQL query
semantics; it changes how the server and individual queries are *configured*.
Cross-cuts SPEC-17 (metrics) — the config layer emits its own reload metrics.

**Companion:** per-query memory accounting is a separate, larger subsystem
(allocation tracking threaded through the join and storage layers). This spec
defines the `max_query_memory` *knob* — parsed, stored, and surfaced — but its
enforcement is delegated to a companion spec written when that work is picked up.
SPARQL over HTTP is session-less, so this spec has **no session tier**:
configuration is either server-scoped (files, env vars, argv) or query-scoped
(URL query parameters). The code keeps the override layering extensible so a
session tier could be added later without a rewrite (see Risks), but no session
state ships here.

## Problem — what exists today, and where it stops

The only configuration surface the running server has is command-line flags on
the `serve` binary (`crates/sparql/src/bin/serve.rs`):

- `--data <paths...>` — RDF files/dirs to load at startup (required).
- `--bind <addr>` — **hardcoded default** `127.0.0.1:3840`.
- `--materialize` — run OWL 2 RL forward-chaining before serving.

Where it stops:

- **No config files, no general env config.** Nothing is read from a config
  file, and the only product-facing runtime env vars anywhere are the two SIMD
  knobs (`HORNDB_SIMD_MAX_ISA`, `HORNDB_SIMD_AUTOTUNE`, read directly in
  `crates/simd/src/dispatch.rs`). There is no general mechanism, and those two
  vars are unreachable from a config file. SPEC-26 absorbs them into a `[simd]`
  config section (S2), keeping the two env-var names as the env layer (S1).
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

- **Per-query memory accounting and enforcement.** This spec parses and stores
  `max_query_memory` but does not enforce it. Real accounting (allocation
  tracking across `wcoj`/`storage`, per-query attribution, over-budget abort) is
  the companion spec's job. Until then the knob is accepted and surfaced with a
  metric, and documented as not-yet-enforced.
- **A session tier / stateful cross-request sessions.** SPARQL over HTTP is
  session-less. A `SET VARIABLE k=v` that persists across multiple HTTP requests
  would need a session identity (a `session_id`), a server-side session registry,
  and TTL/eviction — none of which ships here. SPEC-26 has exactly two tiers:
  server-scoped config and query-scoped URL-parameter overrides. The override
  layering is kept extensible in code so a session tier could be slotted in later
  (see Risks), but no session state exists in this spec.
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

### S1. Layered config resolution and merge

Assemble one effective `ServerConfig` by layering built-in defaults, command-line
flags, environment variables, and operator files.

- **File format: TOML.** Config files are TOML (`config.toml`). The workspace
  already depends on `toml` + `serde`.
- **Config-value precedence, lowest → highest (higher wins):**
  1. built-in defaults (compiled in),
  2. the base `config.toml`,
  3. `config.d/*.toml` fragments in **lexical filename order** (so `99-*`
     overrides `00-*`),
  4. **environment variables** — e.g. `HORNDB_SERVER__BIND`,
     `HORNDB_SIMD_MAX_ISA`,
  5. **command-line flags** (argv) — e.g. `--bind`, `--simd-max-isa`,
     `--simd-autotune`.

  So an env var overrides any config file, and an explicit command-line flag
  overrides everything: **config file < env < argv** (CLI wins). This is the
  conventional precedence — an operator can always force a value with a flag, and
  env vars are the standard container/bootstrap override that sits above the
  on-disk files.
- **This is separate from the config-file *location* precedence** (which file to
  read): `--config <path>` > `HORNDB_CONFIG` env var >
  `/etc/horndb/config.toml` (default path).
  - A missing file at the **default** path is not an error — the server runs on
    the lower layers. A missing file at an **explicitly requested** path
    (`--config` or `HORNDB_CONFIG`) is a fatal startup error.
- **Environment-variable mapping.** Every server setting is reachable from an env
  var under the `HORNDB_` prefix with `__` (double underscore) as the nesting
  separator — e.g. `[server].bind` is `HORNDB_SERVER__BIND`,
  `[server.limits].query_timeout` is `HORNDB_SERVER__LIMITS__QUERY_TIMEOUT`. The
  two pre-existing SIMD vars (`HORNDB_SIMD_MAX_ISA`, `HORNDB_SIMD_AUTOTUNE`) are
  kept as documented aliases for `[simd]` so current usage keeps working.
- **Command-line flags** cover a curated subset of common knobs (`--bind`,
  `--simd-max-isa`, `--simd-autotune`, plus `--config` for the file location); the
  full surface is reachable via env vars and files, not via a flag per nested
  field.
- **Drop-in directory.** The base file may set `config_dir` (default
  `/etc/horndb/config.d/`). Every `*.toml` file directly in that directory is a
  fragment. A missing/empty `config_dir` is not an error.
- **Merge semantics.** Layers deep-merge: for a table, keys union and recurse; for
  a scalar or an array, the higher-precedence layer **replaces** the lower (arrays
  do not concatenate — a `99-*` fragment fully replaces an array, the predictable
  operator-override behavior).
- **Validation.** The merged tree is deserialized into the typed `ServerConfig`.
  Unknown keys and type/range errors are rejected with a message naming the source
  (file + key, or the env var/flag). At startup a rejection is fatal (non-zero
  exit); on reload it is handled per S3 (keep-and-log).

### S2. Config model — two tiers

Separate server-scoped config from the bounded set a query may override.

- **`ServerConfig`** — the whole merged tree, the server-scoped tier. Sections:
  - `[server]` — network identity: `bind` (default `127.0.0.1:3840`). Additional
    server-only fields (e.g. `config_dir`) live here. **Restart-only** (S3).
  - `[server.limits]` — the **defaults** for every query-overridable setting:
    `query_timeout` (duration, default `30s`), `max_result_rows` (integer,
    default `1_000_000`), `rdf12` (bool, default `false`), `max_query_memory`
    (byte size, default unset/unlimited; parsed and stored, enforcement
    delegated). **Hot-reloadable.**
  - `[simd]` — `max_isa` (string, e.g. `"scalar"`; default: auto-detect) and
    `autotune` (bool, default `true`), absorbing the current `HORNDB_SIMD_MAX_ISA`
    / `HORNDB_SIMD_AUTOTUNE` env vars (S1). **Restart-only** — ISA selection and
    calibration happen once at startup (`crates/simd/src/dispatch.rs`).
  - `[logging]` — `level` (default `info`). **Hot-reloadable.**
  - `[reload]` — `debounce` (duration, default `250ms`). **Hot-reloadable.**
- **`QuerySettings`** — the query-scoped tier: the overridable subset
  (`query_timeout`, `max_result_rows`, `rdf12`, `max_query_memory`). It is
  constructed per query by layering overrides (S4) on top of the current
  `[server.limits]` defaults.
- **Durations and byte sizes** parse from human strings (`"30s"`, `"2GiB"`) via
  small typed newtypes with `serde` deserializers, reused for file values and URL
  params so both channels accept identical syntax.

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

- **One channel: URL query parameters** on `/query`, e.g.
  `?query_timeout=30s&max_result_rows=1000`. (A ClickHouse-style in-query
  `SETTINGS` clause was considered and dropped — parsing it off the SPARQL text
  before `spargebra` is too risky; see Risks. URL params are unambiguous and need
  no grammar change.)
- **Precedence, highest wins:** URL query param > `[server.limits]` default.
- **Unknown or out-of-range override.** An unknown setting key or an unparseable
  value is a per-query client error (HTTP 400) naming the offending key — it does
  not affect server config or other queries.
- **Only the whitelisted subset is overridable.** Server-only keys (`bind`,
  `config_dir`, `simd`, `logging`, `reload`) are never settable per query.

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
- **`max_query_memory`.** Parsed, stored on `QuerySettings`, and surfaced, but
  enforcement is a no-op stub with a metric. The companion memory-accounting spec
  turns the stub into real over-budget aborts. Documented at the API as
  accepted-but-not-yet-enforced.

### S6. Crate, CLI, and observability

- **Crate.** A new foundation crate **`horndb-config`** owns layer resolution
  (defaults/argv/env/files), the `config.d` merge, the watcher, the `ArcSwap`
  live handle, the typed `ServerConfig`/`QuerySettings` structs, and the
  duration/byte-size newtypes. A layered-config library is used for the
  argv/env/file providers rather than hand-rolling the merge — **`figment`** is
  the leading candidate (file + env + serde providers, deterministic layer
  order), settled with the alternatives (`config` crate) in the Phase-1 plan.
  New workspace dependencies: the layering library, `notify`, and `arc-swap` (all
  added to `[workspace.dependencies]`). `horndb-sparql` depends on it; the
  companion memory spec and any future consumer depend on it later.
- **CLI integration.** `serve` gains `--config <path>` (and the curated value
  flags, S1). Per the S1 value precedence, an explicit command-line flag
  **overrides** the config file and env vars (config file < env < argv), so
  existing flag-based invocations keep forcing their values.
- **SIMD wiring.** `[simd]` values (from any layer) are resolved by
  `horndb-config` and passed into the `crates/simd` init path; `simd` stays a
  low-level leaf crate and does not depend on `horndb-config`. The legacy env-var
  read sites become one of the resolved layers rather than a separate mechanism.
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

1. **Static layered config (S1, S2, S6-crate/CLI/SIMD/startup-validation).** The
   `horndb-config` crate, the defaults/argv/env/file layering + `config.d` merge,
   the typed model, `serve --config` and the curated value flags, the `[simd]`
   section wired into `crates/simd` init, and startup validation. `bind` and the
   SIMD knobs now come from config. No watcher yet, no query overrides yet.
   *(tracking: `#TODO`)*
2. **Query-scoped overrides + enforcement (S4, S5, S6-metrics for rejects).**
   URL-parameter overrides and real enforcement of `query_timeout` /
   `max_result_rows` / `rdf12`; `max_query_memory` stub. *(tracking: `#TODO`)*
3. **Live watch and reload (S3, remaining S6 metrics).** The `notify` watcher,
   debounce, `ArcSwap` publish, keep-and-log on bad reload, generation metric,
   hot-vs-restart-only handling. *(tracking: `#TODO`)*

Phase 1 stands alone and delivers immediate value (operators get a config file).
Phase 2 depends on Phase 1's model. Phase 3 depends on Phase 1's resolution/merge
and is orthogonal to Phase 2.

## Acceptance criteria

1. **File-location precedence holds (S1).** A test proves `--config` >
   `HORNDB_CONFIG` > `/etc/horndb/config.toml` for *which file* is read; a missing
   default path runs on the lower layers, a missing explicit path is a fatal error.
2. **Value precedence and merge hold (S1).** A test proves the value order
   built-in < base file < `config.d/99-*` < env < argv (with `99-*` overriding
   `00-*`): an env var overrides the file and a command-line flag overrides both,
   tables deep-merge, scalars/arrays replace — verified against fixtures.
3. **Startup validation is honest (S1/S2).** An unknown key or out-of-range value
   fails startup with a non-zero exit and a message naming the source (file+key or
   env var/flag).
4. **`bind` and `[simd]` come from config, CLI/env win over the file (S1/S6).**
   With no flag or env var, the server binds the config-file value; setting the
   env var overrides the file, and `--bind` overrides both (config file < env <
   argv).
5. **Query overrides work and are ordered (S4).** A test proves
   `?query_timeout=…` > `[server.limits]` default; an unknown/invalid URL
   parameter yields HTTP 400 naming the key without disturbing server config.
6. **Enforcement is real (S5).** A `?query_timeout=…` override cancels a
   long-running query via the `CancelToken`; `max_result_rows` ends an over-cap
   stream with a typed error; `rdf12` per query flips RDF 1.2 acceptance.
   `max_query_memory` is accepted and surfaced but documented as not-yet-enforced.
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

- **In-query `SETTINGS` clause — dropped (why).** A ClickHouse-style
  `... SETTINGS k=v` clause was considered and rejected: recognizing and stripping
  it off the query string before `spargebra` sees it risks misfiring on a
  legitimate query that contains the token `SETTINGS` (as an IRI local name or a
  string literal), and it is a grammar change with no clean fallback. URL query
  parameters give the same per-query override with zero grammar risk, so they are
  the only per-query channel (S4). Revisit only if a concrete need for in-query
  settings appears.
- **Watcher portability and editor atomic-save patterns.** `notify` semantics
  differ across platforms and editors (rename-into-place vs. truncate-in-place vs.
  multiple events per save). The debounce plus full re-resolve-and-validate on any
  event is chosen precisely so the reload is idempotent and insensitive to event
  shape, but the watcher must re-establish itself if the watched file is replaced
  by rename. Verify on Linux (the deploy target) and macOS (dev).
- **Duration/byte-size syntax scope.** The human-string parsers (`"30s"`,
  `"2GiB"`) must be pinned to one unambiguous grammar (binary vs decimal byte
  units, allowed duration suffixes) and shared verbatim across config files and
  URL params, or the two channels will drift. Settle the grammar in the Phase-1
  plan.
- **Deferred memory knob honesty.** Accepting `max_query_memory` while not
  enforcing it risks operators believing they are protected. Mitigation: the
  accepted-but-not-enforced state is documented at the API and (optionally) a
  one-time log line notes it; the companion spec removes the gap.
- **Deferred session tier shape.** SPARQL over HTTP is session-less, so no session
  tier ships now — but keep the override-layering design (server defaults → [future
  session] → query) explicit in the code so that if a session tier is ever added
  it slots in as a new layer, not a rewrite of `QuerySettings` resolution.
- **Restart-only reload UX.** Storing a changed restart-only value (so a later
  restart honors it) while not applying it live is the least-surprising behavior,
  but an operator watching only the generation metric could think `bind` changed.
  The "requires restart" log line is the mitigation; consider a dedicated metric
  label if this proves confusing in practice.
