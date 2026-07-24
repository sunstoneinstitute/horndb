---
status: executed
date: 2026-07-24
scope: "SPEC-26 Phase 1b — wire horndb-config into the serve binary: --config + curated value flags, bind from config, [simd] resolved and injected into crates/simd (direct env reads removed), and startup-fatal validation. Depends on PLAN-26-01 (the horndb-config crate). This is the increment that makes a config file take effect end-to-end."
---

# serve wiring + `[simd]` injection — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a `config.toml` actually take effect. The `serve` binary resolves one
`ServerConfig` through `horndb-config` (PLAN-26-01), binds the socket from `[server].bind`,
and injects `[simd]` into `crates/simd` before the first dispatch. An explicit flag still
overrides env and file (precedence: file < env < argv). An invalid config fails startup with
a non-zero exit and a message naming the source. No watcher, no per-query overrides — those are
Phase 2/3. Tracking issue: [#250](https://github.com/sunstoneinstitute/horndb/issues/250).

**Depends on:** [#249](https://github.com/sunstoneinstitute/horndb/issues/249) (PLAN-26-01) — the
`horndb-config` crate, `load(&LoadInputs) -> Result<ServerConfig, ConfigError>`, and the typed model.

## Design (read this before any task)

**Where the wiring lives.** `crates/sparql/src/bin/serve.rs` is the only consumer that changes.
`horndb-sparql` gains a dependency on `horndb-config`; `crates/simd` does **not** — it stays a
leaf crate that receives plain resolved values through a new init entry point.

**Flag → `LoadInputs`.** `serve` keeps `clap`. The new `--config <path>` feeds
`LoadInputs.cli_config_path`. The curated value flags (`--bind`, `--simd-max-isa`,
`--simd-autotune`) are collected into the caller-supplied argv-override dict that `horndb-config`
merges as the highest layer, so an explicit flag wins over env and file exactly as S1 requires.
Flags left unset contribute nothing (they must not inject their `clap` default and clobber a
file value) — model each as `Option<T>` with no `default_value`, and only insert present ones
into the override dict. `--data` and `--materialize` stay plain `clap` fields (data corpora are
out of SPEC-26 scope; `--materialize` is a run-mode toggle, not config).

**`[simd]` injection.** `crates/simd/src/dispatch.rs` today reads `HORNDB_SIMD_MAX_ISA` and
`HORNDB_SIMD_AUTOTUNE` directly, lazily, via `OnceCell` (`configured_cap()`, `autotune()`).
Phase 1b:
- Add a `pub fn init(max_isa: Option<IsaCap>, autotune: bool)` (name/shape at implementer's
  discretion) that seeds those `OnceCell`s **before** any primitive resolves. Calling it twice,
  or after first dispatch, is a no-op-or-error per the `OnceCell` contract — document which.
- Remove the direct `std::env::var("HORNDB_SIMD_*")` reads. Absent an `init()` call (benches,
  unit tests, any non-`serve` embedder), `simd` falls back to its existing auto-detect defaults
  (no cap, autotune on) — the env is no longer a `simd`-level input.
- The env layer moves entirely to `horndb-config`: `[simd]` is reachable as
  `HORNDB_SIMD__MAX_ISA` / `HORNDB_SIMD__AUTOTUNE` (double-underscore nesting). The old
  single-underscore names are **dropped** (sanctioned 0.x break). `serve` resolves `[simd]` from
  the merged config and passes it to `simd::init()` before building the store/serving.

**Startup validation.** `load()` returning `ConfigError` is fatal: print the error (which names
file+key or the env var/flag) to stderr and exit non-zero. This is a `main()` early-return before
any bind or data load.

**Ordering in `main()`:** parse flags → `horndb_config::load()` (fatal on error) →
`simd::init()` from `[simd]` → load data → `TcpListener::bind(cfg.server.bind)` → serve.
`simd::init()` must precede the first store operation so calibration/ISA selection honors the cap.

## Tasks

### Task 1 — `--config` + curated value flags, `LoadInputs` assembly
- [ ] Add `horndb-config` as a dependency of `horndb-sparql` (workspace dep).
- [ ] Add `--config <path>` and `Option`-typed `--bind` / `--simd-max-isa` / `--simd-autotune`
      flags to the `serve` `Cli` (no `default_value` on the value flags).
- [ ] Build `LoadInputs` from the flags: `cli_config_path` from `--config`; an argv-override dict
      containing only the present value flags.
- [ ] Test: present flags land in the override dict; absent flags do not appear (no default
      injection).

### Task 2 — resolve config, bind from it, startup-fatal validation
- [ ] Call `horndb_config::load()` early in `main()`; on `Err`, print to stderr and exit non-zero.
- [ ] Bind `cfg.server.bind` instead of the hardcoded default; drop the old `--bind default_value`.
- [ ] Test (integration, `server` feature): with no flag/env the server binds the config-file
      value; `HORNDB_SERVER__BIND` overrides the file; `--bind` overrides both.
- [ ] Test: an unknown key / out-of-range value exits non-zero with a message naming the source.

### Task 3 — `[simd]` init entry point; remove direct env reads
- [ ] Add `simd::init(max_isa, autotune)` seeding the `OnceCell`s; document the twice/late-call
      contract.
- [ ] Remove the `std::env::var("HORNDB_SIMD_MAX_ISA"|"HORNDB_SIMD_AUTOTUNE")` reads in
      `dispatch.rs`; keep the auto-detect fallback when `init()` is not called.
- [ ] `serve` calls `simd::init()` from resolved `[simd]` before the first store op.
- [ ] Test: `init()` with a cap makes `configured_cap()` reflect it; without `init()`, defaults
      hold. Grep proves no `HORNDB_SIMD_` env read remains in `crates/simd`.
- [ ] Update `crates/simd/CLAUDE.md` / any doc that names the old env vars to the `__` form.

### Task 4 — docs sync (same commit as the behavior)
- [ ] `docs/architecture.md`: flip the SPEC-07 "Operator configuration system" row detail for
      Phase 1b from planned → implemented (leave 1a's line as delivered by PLAN-26-01).
- [ ] Confirm `docs/metrics.md` needs no change (Phase 1b adds no metric; reload/reject metrics
      are Phase 3).
- [ ] Verify the SPEC-01 selected subset and existing `sparql` server tests stay green
      (`cargo nextest run -p horndb-sparql --features server`).

## Acceptance criteria (mirror of #250)

1. With no flag/env, `serve` binds the config-file `bind`; `HORNDB_SERVER__BIND` overrides the
   file; `--bind` overrides both.
2. `[simd]` resolved from any layer reaches `crates/simd` init; `dispatch.rs` no longer reads env
   directly; `HORNDB_SIMD__MAX_ISA` / `HORNDB_SIMD__AUTOTUNE` work and the old names are gone.
3. An unknown key or out-of-range value fails startup with a non-zero exit naming the source.
4. Existing `sparql` server tests extend rather than regress; SPEC-01 selected subset green.

## Risks

- **Lazy `OnceCell` already initialized.** If any code path resolves a SIMD primitive before
  `serve` calls `simd::init()`, the cap seed is lost. Mitigation: call `init()` first thing after
  config resolution, before data load; add a debug assertion that `init()` observed an
  uninitialized cell.
- **Flag-default clobber.** A `clap` `default_value` on a curated flag would silently override a
  file value. Mitigation: `Option<T>` flags with no default; only present flags enter the dict
  (covered by Task 1's test).
