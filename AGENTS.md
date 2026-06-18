# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

It is the **always-loaded** tier: project identity, hard constraints, and pointers.
Task-specific detail lives one tier deeper, in nested `CLAUDE.md` files that load
only when you work in the relevant directory — see [Where deeper guidance
lives](#where-deeper-guidance-lives).

## Project in one line

HornDB is a hybrid forward/backward-chaining RDF reasoner targeting OWL 2 RL with a SPARQL 1.1 frontend, designed for unified-memory hardware (HBM / CXL). The canonical "why" lives in `docs/specs/SPEC-00-vision.md`; read it before making architectural changes.

## Authoritative documents

These files drive the project — keep them in mind when planning work:

- `docs/specs/SPEC-00..10-*.md` — subsystem contracts. Each ends with **Acceptance criteria** that gate the spec.
- `docs/plans/2026-05-24-SPEC-*.md` — the one-per-spec implementation plans the Stage-1 pass executed.
- `docs/architecture.md` — single-page architecture map synthesised from the SPECs and plans. Carries a **Status** field (implemented / specified / planned / deferred) for every subsystem and major feature. This is the "current state" view that sits between the SPECs (intent) and `TASKS.md` (outstanding work).
- `TASKS.md` — Stage-1 follow-ups. Ordered CRITICAL → HIGH → MEDIUM → LOW. When picking up a task, move it to its own commit and check it off in the same commit. You can push commits that only contain task claims/updates to origin without asking. Its header carries the task↔GitHub-issue mirroring procedure.
- `BENCHMARKS.md` — per-subsystem performance targets, vendor baselines, and current measured numbers. Update the relevant row whenever a bench moves; do not let it drift from `TASKS.md`.
- `harness/curation/owl2-rl-50.md` and `harness/selected.toml` — the conformance subset every spec is graded against.

The harness-first rule (from SPEC-00): a SPEC is not satisfied until its referenced subset in SPEC-01's harness is green. Implementation work may *grow* a subset but never bypass it.

### Where specs and plans live

All specs go in `docs/specs/`, all implementation plans in `docs/plans/`. There is exactly one home for each — **do not create a parallel tree**. When a superpowers skill (brainstorming, `writing-plans`, `writing-skills`, etc.) or any other tool defaults to writing under `docs/superpowers/` (or any other subdirectory), redirect its output to `docs/specs/` or `docs/plans/` instead. Naming:

- Subsystem contracts use the `SPEC-NN-<slug>.md` form and gate on **Acceptance criteria** (`docs/specs/`).
- Dated design specs and implementation plans use a `YYYY-MM-DD-<slug>.md` prefix (`docs/specs/` and `docs/plans/` respectively).

### Keep the docs in sync (do this in the same commit)

`docs/architecture.md`, `TASKS.md`, and the SPECs/plans are linked views of the same reality. When you edit one, update the others so they never drift:

- **Change `TASKS.md`** (check off, add, remove, re-scope) → update the matching **Status** field in `docs/architecture.md`. Checking off a task usually flips a row **planned** → **implemented**; adding one usually flips **specified** → **planned**. Mirror the change to the task's GitHub issue too — procedure in the `TASKS.md` header.
- **Change a SPEC or plan** such that the outstanding work changes → update `TASKS.md` (add or re-scope the tracking task), then reflect the new state in `docs/architecture.md`.

Source of truth: SPECs for *intent*, `TASKS.md` for *outstanding work*, `docs/architecture.md` for *current state*. When they disagree, **the code wins** — fix whichever is stale.

## Workspace layout

Nine Rust crates under `crates/`, all `publish = false`, all on `edition = 2021`, pinned to Rust `1.90.0` via `rust-toolchain.toml`:

| Crate | SPEC | Role |
|---|---|---|
| `horndb-storage` | SPEC-02 | Tiered storage, dictionary encoding, columnar partitions. Foundation. |
| `horndb-wcoj` | SPEC-03 | Leapfrog Triejoin executor, trie iterators, planner. |
| `horndb-owlrl` | SPEC-04 | OWL 2 RL rules — **compiled** via `build.rs` from `rules.toml` (Soufflé-style codegen, no interpreter). |
| `horndb-closure` | SPEC-05 | GraphBLAS closure backend. **Links to SuiteSparse:GraphBLAS** via `build.rs` + `bindgen` + `pkg-config` (`links = "graphblas"`). |
| `horndb-incremental` | SPEC-06 | DBSP-style Z-set deltas, change feed, checkpointing. |
| `horndb-sparql` | SPEC-07 | Parser (spargebra), algebra, planner, runtime, axum HTTP server (`server` feature, on by default). |
| `horndb-ml` | SPEC-08 | ML/LLM boundary — candidate generation, audit, registry. Symbolic is source of truth. |
| `horndb-hardware-ext` | SPEC-09 | Empty placeholder; Stage-3 territory. |
| `horndb-harness` | SPEC-01 | Conformance + benchmark runner, ships the `harness` binary. Loads `harness/selected.toml` at the workspace root. |

Dependency order (for refactors): `storage` → `wcoj` → `{owlrl, closure}` → `incremental` → `sparql`; `harness` and `ml` sit on top.

Per-crate build quirks, feature flags, and gotchas live in each crate's own
`CLAUDE.md` and `INTEGRATION-NOTES.md` — they load when you work in that crate.

## Build, test, lint

The pre-commit configuration is split intentionally — keep this split when adding hooks:

- **Pre-commit (fast):** `cargo fmt --all -- --check` only.
- **Pre-push (slow):** `cargo clippy --workspace --all-targets -- -D warnings` and `cargo build --workspace`.

First pre-push after a fresh checkout (or `cargo clean`) takes several minutes: the
harness pulls in `oxrocksdb-sys` (a ~700 MB artifact) transitively via `oxigraph`.
Subsequent pushes reuse the cache. Vendored GraphBLAS is already shared across
worktrees automatically; to also share the rocksdb build, point `CARGO_TARGET_DIR`
at a shared path.

Day-to-day commands:

```bash
cargo fmt --all                                          # auto-format
cargo clippy --workspace --all-targets -- -D warnings    # what CI runs
cargo nextest run --workspace                            # all unit/integration tests (parallel across binaries)
cargo nextest run -p horndb-sparql --features server     # SPARQL HTTP server tests (required for full SPARQL pass)
cargo nextest run -p <crate> <test_name>                 # single test
cargo bench -p <crate> --bench <name>                    # criterion benches (e.g. four_cycle, per_tuple, load_lubm, transitive, sameas, insert_throughput)
```

**Test runner — use `cargo nextest`.** The workspace builds ~90 separate
integration-test binaries; cargo's built-in runner executes them one binary at a
time, which dominates `cargo test --workspace` wall-clock. `cargo nextest`
schedules every test across all binaries in one concurrent pool — same tests,
no source changes, materially faster (locally ~40% on a quiet machine; more
under contention / in CI). Config lives in `.config/nextest.toml`. Install a
rustc-1.90-compatible version (the workspace is pinned to 1.90.0; nextest
>= 0.9.79 needs a newer rustc to *build* — a prebuilt binary has no such limit):

```bash
cargo install cargo-nextest --version '0.9.78' --locked   # build-from-source path
# or fetch a prebuilt binary (no rustc constraint), e.g. cargo-binstall cargo-nextest
```

`cargo test --workspace` still works and is the only way to run **doctests**
(nextest does not run them; the workspace currently has zero runnable doctests).
CI runs `cargo nextest run --profile ci` plus a separate `cargo test --doc`.

**Run benchmarks on the `hornbench` server, never the laptop.** Any `cargo bench`
run that produces numbers for `BENCHMARKS.md` must execute on the dedicated
benchmark host so results stay comparable over time (and to spare laptop battery
and thermals). Procedure: `ssh hornbench`; the repo is at `~/src/horndb`; `git
fetch`/`pull` and check out the commit under test (or `rsync` over any
not-yet-committed files), then run the bench there and record the numbers (note
the env) back in `BENCHMARKS.md`. Local `cargo bench` is fine only for a quick
smoke-check you are *not* going to record.

**macOS dev tip:** the workspace builds ~90 separate test binaries, and each freshly-linked one triggers a Gatekeeper (`syspolicyd`) + XProtect scan on first run — which can pin those daemons near 100% CPU during `cargo test`/`build`. Add your terminal to System Settings → Privacy & Security → **Developer Tools** (or run `sudo spctl developer-mode enable-terminal` once) to exempt its child processes from Gatekeeper assessment. This and `cargo nextest` (above) are complementary: the exemption removes the per-binary scan, nextest removes the serial-per-binary run.

CI (`.github/workflows/ci.yml`) mirrors the above plus a conformance run with the real engine; nightly runs LDBC SPB-256 on a self-hosted runner. **Pin every GitHub Action to a full 40-char commit SHA, never a floating tag** — full hygiene rules and the dependabot flow are in `.github/CLAUDE.md`.

## Workspace conventions

- Common deps go in the root `[workspace.dependencies]` and are referenced with `dep.workspace = true` from each crate. Add new shared deps there, not per-crate.
- Each subsystem crate has an `INTEGRATION-NOTES.md` (sometimes also `FUTURE-WORK.md` or `STAGE1-ACCEPTANCE.md`). Read these before changing the public API of a crate — they record decisions that aren't in the specs.
- Plans (`docs/plans/2026-05-24-*.md`) are historical implementation logs of the Stage-1 dispatch; treat them as commit-message-grade context, not as a source of truth for current behaviour.
- `.claude/worktrees/` is the local worktree pool — the multi-agent Stage-1 pass dispatched parallel subagents into worktrees there. Disk pressure during such runs is a known operational risk (TASKS.md LOW).

## Where deeper guidance lives

These nested `CLAUDE.md`/`AGENTS.md` files load on-demand when you work in their directory:

- `crates/harness/` — running the `harness` binary, engines, suite keys, the RDF 1.2 N-Triples suite.
- `crates/owlrl/` — the `rules.toml` → Rust codegen pipeline (canonical contributor guide). See also the `add-owlrl-rule` skill.
- `crates/closure/` — SuiteSparse:GraphBLAS linkage and the shared vendored build.
- `crates/wcoj/` — SPEC-03 status, the differential fuzzer, the 4-cycle bench.
- `crates/sparql/` — RDF 1.2 / `SparqlConfig::rdf12()`, feature flags, server tests.
- `crates/incremental/` — insertion vs retraction status.
- `.github/` — Action SHA-pinning, dependabot, CI/nightly layout.
- `docs/` — keeping `docs/index.md` current.
