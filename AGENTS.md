# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project in one line

HornDB is a hybrid forward/backward-chaining RDF reasoner targeting OWL 2 RL with a SPARQL 1.1 frontend, designed for unified-memory hardware (HBM / CXL). The canonical "why" lives in `docs/specs/SPEC-00-vision.md`; read it before making architectural changes.

## Authoritative documents

These files drive the project — keep them in mind when planning work:

- `docs/specs/SPEC-00..09-*.md` — subsystem contracts. Each ends with **Acceptance criteria** that gate the spec.
- `docs/plans/2026-05-24-SPEC-*.md` — the one-per-spec implementation plans the Stage-1 pass executed.
- `TASKS.md` — Stage-1 follow-ups. Ordered CRITICAL → HIGH → MEDIUM → LOW. When picking up a task, move it to its own commit and check it off in the same commit.
- `BENCHMARKS.md` — per-subsystem performance targets, vendor baselines, and current measured numbers. Update the relevant row whenever a bench moves; do not let it drift from `TASKS.md`.
- `harness/curation/owl2-rl-50.md` and `harness/selected.toml` — the conformance subset every spec is graded against.

The harness-first rule (from SPEC-00): a SPEC is not satisfied until its referenced subset in SPEC-01's harness is green. Implementation work may *grow* a subset but never bypass it.

## Workspace layout

Nine Rust crates under `crates/`, all `publish = false`, all on `edition = 2021`, pinned to Rust `1.88.0` via `rust-toolchain.toml`:

| Crate | SPEC | Role |
|---|---|---|
| `horndb-storage` | SPEC-02 | Tiered storage, dictionary encoding, columnar partitions. Foundation. |
| `horndb-wcoj` | SPEC-03 | Leapfrog Triejoin executor, trie iterators, planner. |
| `horndb-owlrl` | SPEC-04 | OWL 2 RL rules — **compiled** via `build.rs` from `rules.toml` (Soufflé-style codegen, no interpreter). |
| `horndb-closure` | SPEC-05 | GraphBLAS closure backend. **Links to SuiteSparse:GraphBLAS** via `build.rs` + `bindgen` + `pkg-config` (`links = "graphblas"`). |
| `horndb-incremental` | SPEC-06 | DBSP-style Z-set deltas, change feed, checkpointing. Insertion-only at Stage 1. |
| `horndb-sparql` | SPEC-07 | Parser (spargebra), algebra, planner, runtime, axum HTTP server (`server` feature, on by default). Pulls `oxrdf 0.3` / `sparesults 0.3` directly — workspace is otherwise on `oxrdf 0.2` (see RDF 1.2 migration in TASKS.md). |
| `horndb-ml` | SPEC-08 | ML/LLM boundary — candidate generation, audit, registry. Symbolic is source of truth. |
| `horndb-hardware-ext` | SPEC-09 | Empty placeholder; Stage-3 territory. |
| `horndb-harness` | SPEC-01 | Conformance + benchmark runner, ships the `harness` binary. Has its own `selected.toml`. |

Dependency order (for refactors): `storage` → `wcoj` → `{owlrl, closure}` → `incremental` → `sparql`; `harness` and `ml` sit on top.

## Build, test, lint

The pre-commit configuration is split intentionally — keep this split when adding hooks:

- **Pre-commit (fast):** `cargo fmt --all -- --check` only.
- **Pre-push (slow):** `cargo clippy --workspace --all-targets --exclude horndb-harness -- -D warnings` and `cargo build --workspace`.

`horndb-harness` is excluded from the pre-push clippy hook because `oxrocksdb-sys` (pulled transitively via `oxigraph`) takes minutes to compile from scratch; CI runs it on a cached runner. Do not remove the exclusion without first solving the cache story.

Day-to-day commands:

```bash
cargo fmt --all                                         # auto-format
cargo clippy --workspace --all-targets -- -D warnings    # what CI runs
cargo test --workspace                                   # all unit/integration tests
cargo test -p horndb-sparql --features server          # SPARQL HTTP server tests (required for full SPARQL pass)
cargo test -p <crate> <test_name>                        # single test
cargo test -p horndb-wcoj -- --ignored                 # run the WCOJ differential fuzzer (currently red — see TASKS.md)
cargo bench -p <crate> --bench <name>                    # criterion benches (e.g. `four_cycle`, `per_tuple`, `load_lubm`, `transitive`, `sameas`, `insert_throughput`)
```

CI mirrors the above plus a conformance run with the real engine; see `.github/workflows/ci.yml`. Nightly runs LDBC SPB-256 on a self-hosted runner (`.github/workflows/nightly.yml`).

## The harness binary

Built by `cargo build -p horndb-harness --bin harness [--release] [--features real-engine]`. Two engines:

- `--engine stub` — no real engine, for harness plumbing tests.
- `--engine owlrl` — the real engine. Requires `--features real-engine` at build time.

Typical local runs (also documented in `crates/harness/README.md`):

```bash
# Stage 0 / plumbing only
cargo run -p horndb-harness --bin harness -- --engine stub run --allow-failing

# Stage 1 real engine, full 50-case OWL 2 RL subset (fetches W3C suites first)
./crates/harness/scripts/fetch-w3c-suites.sh
cargo run -p horndb-harness --bin harness --features real-engine -- --engine owlrl run

# Trend report from prior runs (SQLite-backed)
cargo run -p horndb-harness --bin harness -- report --suite ldbc-spb-256 --metric editorial-qps
```

Harness state lives in `target/harness.sqlite`; CI publishes JUnit to `target/junit.xml`. Fetched corpora go under `crates/harness/data/` (gitignored).

There are currently two `selected.toml` files (`harness/selected.toml` at workspace root, `crates/harness/selected.toml` for the SPARQL fixtures). Consolidation is tracked in TASKS.md — pick one when touching either.

## Crate-specific gotchas

- **`horndb-closure`** has a `build.rs` that bindgen's against `wrapper.h` and `pkg-config`s `graphblas`. You need SuiteSparse:GraphBLAS installed locally to build this crate. The wrapper headers and integration notes live alongside `Cargo.toml`.
- **`horndb-owlrl`** generates Rust source from `rules.toml` in `build.rs` (the codegen pipeline is in `codegen/`). When editing rules, expect a slower first build and check both `INTEGRATION-NOTES.md` and the generated code under `target/`.
- **`horndb-wcoj`** has a known correctness bug on BGPs with repeated patterns (TASKS.md CRITICAL). The differential fuzzer in `tests/differential_fuzz.rs` is `#[ignore]`'d with a regression file checked into `tests/differential_fuzz.proptest-regressions`. The 4-cycle benchmark is also currently ~1.6× *slower* than the binary-hash reference — both gates block SPEC-03 acceptance.
- **`horndb-sparql`** intentionally depends on `oxrdf = "0.3"` directly while the rest of the workspace transitively uses `oxrdf 0.2` (pinned by `oxigraph 0.4`). The RDF 1.2 / triple-terms migration that aligns these is a Stage-2 priority in TASKS.md — do not "fix" the mismatch ad hoc.
- **`horndb-incremental`** is **insertion-only at Stage 1**. Retraction semantics are deferred (see `FUTURE-WORK.md` and SPEC-06).

## Workspace conventions

- Common deps go in the root `[workspace.dependencies]` and are referenced with `dep.workspace = true` from each crate. Add new shared deps there, not per-crate.
- Each subsystem crate has an `INTEGRATION-NOTES.md` (sometimes also `FUTURE-WORK.md` or `STAGE1-ACCEPTANCE.md`). Read these before changing the public API of a crate — they record decisions that aren't in the specs.
- Plans (`docs/plans/2026-05-24-*.md`) are historical implementation logs of the Stage-1 dispatch; treat them as commit-message-grade context, not as a source of truth for current behaviour.
- `.claude/worktrees/` is the local worktree pool — the multi-agent Stage-1 pass dispatched parallel subagents into worktrees there. Disk pressure during such runs is a known operational risk (TASKS.md LOW).
