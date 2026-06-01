# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project in one line

HornDB is a hybrid forward/backward-chaining RDF reasoner targeting OWL 2 RL with a SPARQL 1.1 frontend, designed for unified-memory hardware (HBM / CXL). The canonical "why" lives in `docs/specs/SPEC-00-vision.md`; read it before making architectural changes.

## Authoritative documents

These files drive the project — keep them in mind when planning work:

- `docs/specs/SPEC-00..10-*.md` — subsystem contracts. Each ends with **Acceptance criteria** that gate the spec.
- `docs/plans/2026-05-24-SPEC-*.md` — the one-per-spec implementation plans the Stage-1 pass executed.
- `docs/architecture.md` — single-page architecture map synthesised from the SPECs and plans. Carries a **Status** field (implemented / specified / planned / deferred) for every subsystem and major feature. This is the "current state" view that sits between the SPECs (intent) and `TASKS.md` (outstanding work).
- `TASKS.md` — Stage-1 follow-ups. Ordered CRITICAL → HIGH → MEDIUM → LOW. When picking up a task, move it to its own commit and check it off in the same commit.
- `BENCHMARKS.md` — per-subsystem performance targets, vendor baselines, and current measured numbers. Update the relevant row whenever a bench moves; do not let it drift from `TASKS.md`.
- `harness/curation/owl2-rl-50.md` and `harness/selected.toml` — the conformance subset every spec is graded against.

The harness-first rule (from SPEC-00): a SPEC is not satisfied until its referenced subset in SPEC-01's harness is green. Implementation work may *grow* a subset but never bypass it.

### Keep the docs in sync (do this in the same commit)

These three documents are linked views of the same reality. When you edit one, update the others so they never drift:

- **When you change `TASKS.md`** (check off, add, remove, or re-scope a task), update the matching **Status** field in `docs/architecture.md` in the same commit. Checking off a task usually flips a row from **planned** → **implemented**; adding a task usually flips **specified** → **planned**.
- **When you change a SPEC or plan** (`docs/specs/` or `docs/plans/`) such that the outstanding work changes, update `TASKS.md` in the same commit (add or re-scope the tracking task), then reflect the new state in `docs/architecture.md`.

Source of truth: SPECs for *intent*, `TASKS.md` for *outstanding work*, `docs/architecture.md` for *current state*. When they disagree, the code wins — fix whichever is stale.

### Keep GitHub issues in sync with `TASKS.md`

Every open task in `TASKS.md` mirrors one GitHub issue (`sunstoneinstitute/horndb`), carrying the `([#N](…))` link in both its index line and its body heading. The issue is labelled to match the task: one `priority:` label (`critical`/`high`/`medium`/`low`) and one `category:` label (`correctness`/`performance`/`completeness`/`conformance`/`tooling`/`operational`/`maintainability`). Keep the two in lockstep, in the same change:

- **Add a task** → open an issue with the matching `priority:` + `category:` labels, then put its `([#N](url))` link on both the index line and the body heading. Use `gh issue create --title … --label "priority: …" --label "category: …" --body-file …`.
- **Complete a task** (`[ ]` → `[x]`) → `gh issue close N`. Keep the link in `TASKS.md` for traceability.
- **Retitle / re-prioritise / re-categorise a task** → `gh issue edit N` to update the title and swap the `priority:`/`category:` labels so they still match.
- **Remove a task** → `gh issue close N` (comment why) and drop its `TASKS.md` lines.

The `priority:`/`category:` label set is the GitHub mirror of the **Priority**/**Category** taxonomy defined at the top of `TASKS.md`; if you add a new category or priority there, create the matching label (`gh label create`) first.

## Workspace layout

Nine Rust crates under `crates/`, all `publish = false`, all on `edition = 2021`, pinned to Rust `1.88.0` via `rust-toolchain.toml`:

| Crate | SPEC | Role |
|---|---|---|
| `horndb-storage` | SPEC-02 | Tiered storage, dictionary encoding, columnar partitions. Foundation. |
| `horndb-wcoj` | SPEC-03 | Leapfrog Triejoin executor, trie iterators, planner. |
| `horndb-owlrl` | SPEC-04 | OWL 2 RL rules — **compiled** via `build.rs` from `rules.toml` (Soufflé-style codegen, no interpreter). |
| `horndb-closure` | SPEC-05 | GraphBLAS closure backend. **Links to SuiteSparse:GraphBLAS** via `build.rs` + `bindgen` + `pkg-config` (`links = "graphblas"`). |
| `horndb-incremental` | SPEC-06 | DBSP-style Z-set deltas, change feed, checkpointing. Insertion-only at Stage 1. |
| `horndb-sparql` | SPEC-07 | Parser (spargebra), algebra, planner, runtime, axum HTTP server (`server` feature, on by default). Tracks workspace `oxrdf 0.3` / `oxrdfio 0.2` / `sparesults 0.3` with `rdf-12` features on across the workspace (PR2 of the RDF 1.2 migration). Triple-term patterns are gated at runtime by `SparqlConfig::rdf12` — default `false` keeps SPARQL 1.1 callers on 1.1 semantics; `SparqlConfig::rdf12()` accepts `<<( s p o )>>` patterns. |
| `horndb-ml` | SPEC-08 | ML/LLM boundary — candidate generation, audit, registry. Symbolic is source of truth. |
| `horndb-hardware-ext` | SPEC-09 | Empty placeholder; Stage-3 territory. |
| `horndb-harness` | SPEC-01 | Conformance + benchmark runner, ships the `harness` binary. Loads `harness/selected.toml` at the workspace root. |

Dependency order (for refactors): `storage` → `wcoj` → `{owlrl, closure}` → `incremental` → `sparql`; `harness` and `ml` sit on top.

## Build, test, lint

The pre-commit configuration is split intentionally — keep this split when adding hooks:

- **Pre-commit (fast):** `cargo fmt --all -- --check` only.
- **Pre-push (slow):** `cargo clippy --workspace --all-targets -- -D warnings` and `cargo build --workspace`.

The pre-push clippy hook covers the full workspace including `horndb-harness`. The harness pulls in `oxrocksdb-sys` transitively via `oxigraph`, which compiles a ~700 MB artifact — expect the first pre-push after a fresh checkout (or a `cargo clean`) to take several minutes. Subsequent pushes reuse the cached build. If you run multiple worktrees in parallel, the vendored GraphBLAS is already
shared automatically (built once per `(target, version)` into
`crates/closure/vendor/.shared-build/`, flock-guarded — see
`crates/closure/INTEGRATION-NOTES.md`). The remaining large per-worktree
artifact is rocksdb (pulled in transitively by `horndb-harness`); point
`CARGO_TARGET_DIR` at a shared path if you want it compiled only once across
worktrees too.

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

### GitHub Actions hygiene

Pin every GitHub Action to a **full 40-char commit SHA**, never a tag:
`uses: owner/action@<sha> # vX.Y.Z`. The trailing `# vX.Y.Z` comment is required — it is what a human reads and what Dependabot rewrites on a bump. Floating tags (`@v4`, `@main`) are a supply-chain risk (the tag can be repointed at malicious code) and must not appear in `.github/workflows/`. When adding or upgrading an action, resolve the SHA first:

```bash
gh api repos/<owner>/<repo>/commits/<tag> --jq .sha   # SHA to pin
```

`.github/dependabot.yml` keeps these pinned SHAs (and their version comments) and the Cargo workspace dependencies up to date on a weekly schedule — GitHub Actions updates grouped under a `ci:` prefix, Cargo minor/patch updates under `chore:`. Review and merge those PRs like any other; do not hand-bump pins outside that flow unless patching an urgent CVE.

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

The canonical selection file is `harness/selected.toml` at the workspace root. It carries both the manifest-driven `[suites.*]` entries the harness binary loads and the path-based `[sparql_query]` section consumed by `crates/sparql/tests/w3c_suite.rs`.

Suite keys the runner recognises today (`crates/harness/src/runner.rs`): `owl2`, `owl2-w3c-rl`, `sparql11`, `rdf12-n-triples`. The last one runs the W3C RDF 1.2 N-Triples *syntax* tests (4 positive `<<( s p o )>>` cases + 6 bad-syntax negatives); it uses `TestKind::SyntaxPositive` / `SyntaxNegative` and invokes `oxttl::NTriplesParser` directly with no reasoner involvement. Fixtures live under `crates/harness/tests/fixtures/rdf12-n-triples/`, re-fetchable via `crates/harness/scripts/fetch-w3c-suites.sh`. Upstream URL: `https://w3c.github.io/rdf-tests/rdf/rdf12/rdf-n-triples/syntax/` — note the `syntax/` segment; the top-level `rdf-n-triples/manifest.ttl` only `mf:include`s the syntax sub-manifest alongside `c14n/` and the RDF 1.1 N-Triples suite.

## Crate-specific gotchas

- **`horndb-closure`** has a `build.rs` that bindgen's against `wrapper.h` and `pkg-config`s `graphblas`. You need SuiteSparse:GraphBLAS installed locally to build this crate. The wrapper headers and integration notes live alongside `Cargo.toml`.
  The vendored build is compiled once per `(target, version)` into a
  flock-guarded `crates/closure/vendor/.shared-build/<target>/<version>/` shared
  across worktrees (details in `INTEGRATION-NOTES.md`).
- **`horndb-owlrl`** generates Rust source from `rules.toml` in `build.rs` (the codegen pipeline is in `codegen/`). When editing rules, expect a slower first build and check both `INTEGRATION-NOTES.md` and the generated code under `target/`.
- **`horndb-wcoj`** has a known correctness bug on BGPs with repeated patterns (TASKS.md CRITICAL). The differential fuzzer in `tests/differential_fuzz.rs` is `#[ignore]`'d with a regression file checked into `tests/differential_fuzz.proptest-regressions`. The 4-cycle benchmark is also currently ~1.6× *slower* than the binary-hash reference — both gates block SPEC-03 acceptance.
- **`horndb-sparql`** tracks the unified workspace versions (`oxrdf 0.3.x`, `oxrdfio 0.2.x`, `sparesults 0.3.x`) with `rdf-12` (and `sparesults/sparql-12`) features on workspace-wide after PR2 of the RDF 1.2 migration. The crate additionally enables `spargebra/sep-0006` (for `GraphPattern::Lateral`) and `spargebra/sparql-12` (for `TermPattern::Triple`). Triple-term patterns are accepted only when callers pass `SparqlConfig::rdf12()` — the default config rejects them so SPARQL 1.1 callers keep their semantics; see `crates/sparql/src/lib.rs::SparqlConfig` and `translate_query_with` / `execute_query_with`. Note: enabling `oxrdf/rdf-12` workspace-wide forces `oxigraph/rdf-12` too (sparopt/spareval need their own `sparql-12` arms gated on, and Cargo only unifies features upward).
- **`horndb-incremental`** is **insertion-only at Stage 1**. Retraction semantics are deferred (see `FUTURE-WORK.md` and SPEC-06).

## Workspace conventions

- Common deps go in the root `[workspace.dependencies]` and are referenced with `dep.workspace = true` from each crate. Add new shared deps there, not per-crate.
- Each subsystem crate has an `INTEGRATION-NOTES.md` (sometimes also `FUTURE-WORK.md` or `STAGE1-ACCEPTANCE.md`). Read these before changing the public API of a crate — they record decisions that aren't in the specs.
- Plans (`docs/plans/2026-05-24-*.md`) are historical implementation logs of the Stage-1 dispatch; treat them as commit-message-grade context, not as a source of truth for current behaviour.
- `.claude/worktrees/` is the local worktree pool — the multi-agent Stage-1 pass dispatched parallel subagents into worktrees there. Disk pressure during such runs is a known operational risk (TASKS.md LOW).
