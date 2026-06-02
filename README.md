# HornDB

A hybrid forward/backward-chaining RDF reasoner targeting **OWL 2 RL** semantics with a **SPARQL 1.1** frontend, designed for modern unified-memory hardware (HBM-equipped GPUs/APUs, CXL-attached DRAM tiers).

Apache-2.0, EU-developed, open from the start. Built by [Sunstone Institute](https://sunstoneinstitute.ai).

> Status: **Stage 1 (feasibility prototype) in progress.** The workspace builds and the SPEC-01 conformance harness runs the Stage-1 OWL 2 RL subset against the real engine in CI. See [`TASKS.md`](TASKS.md) for the live punch list, including known correctness and performance gaps.

## Why this exists

The reasoner space today forces a choice between:

- **Pure-materialization commercial engines** (RDFox, GraphDB) that give up 100–1000× on backward chaining and are not open;
- **Open-source toolkits** (Apache Jena, Eclipse RDF4J) that are flexible but slower on the same materialization workload.

HornDB makes a different set of bets — they're stated in full in [`docs/specs/SPEC-00-vision.md`](docs/specs/SPEC-00-vision.md), but in short:

1. **Hybrid execution**, not pure materialization. Materialize the schema/transitive-closure subset (subClassOf, subPropertyOf, sameAs, transitive properties); backward-chain the rest with magic sets.
2. **Unified-memory hardware as a first-class target.** HBM for the hot working set, DDR5 for warm, CXL/NVMe for cold.
3. **DBSP-style incremental maintenance.** Z-set differences instead of DRed/FBF counting.
4. **GraphBLAS for the closure subset.** Schema-level transitive closure as semiring matrix multiply on SuiteSparse:GraphBLAS.
5. **Soufflé-style ahead-of-time rule compilation.** OWL 2 RL rules compiled to native Rust — no rule interpreter in the hot path.
6. **Provenance as a hard requirement.** Every inferred triple traces back to its premises.

Non-goals: OWL 2 DL completeness, property-graph compatibility, beating RDFox on pure single-node main-memory materialization, embedding-based "neural" reasoning as the source of truth.

## Architecture at a glance

Nine Rust crates under `crates/`, one per implementation SPEC (SPEC-01..09). SPEC-00 is the vision and SPEC-10 (an rdflib-compatible Python API) is planned with no crate yet. For the current state of every subsystem — what is implemented, specified, planned, or deferred — see [`docs/architecture.md`](docs/architecture.md).

```
storage  ─┬─ wcoj  ─┬─ owlrl  ─┐
          │         │          ├─ sparql  ─── harness
          │         └─ closure ─┤
          └─────────── incremental ─┘
                                    └─ ml (boundary)
```

| Crate | SPEC | What it does |
|---|---|---|
| `horndb-storage` | [SPEC-02](docs/specs/SPEC-02-storage.md) | Tiered storage, dictionary encoding, columnar partitions |
| `horndb-wcoj` | [SPEC-03](docs/specs/SPEC-03-query-engine.md) | Worst-case optimal joins (Leapfrog Triejoin) |
| `horndb-owlrl` | [SPEC-04](docs/specs/SPEC-04-rule-engine.md) | OWL 2 RL rule engine (rules compiled at `build.rs` time) |
| `horndb-closure` | [SPEC-05](docs/specs/SPEC-05-closure-backend.md) | GraphBLAS closure backend (SuiteSparse:GraphBLAS C ABI) |
| `horndb-incremental` | [SPEC-06](docs/specs/SPEC-06-incremental-maintenance.md) | DBSP-style Z-set deltas |
| `horndb-sparql` | [SPEC-07](docs/specs/SPEC-07-sparql-frontend.md) | SPARQL 1.1 frontend + axum HTTP server |
| `horndb-ml` | [SPEC-08](docs/specs/SPEC-08-ml-integration.md) | ML/LLM boundary — symbolic source of truth, ML as optimizer |
| `horndb-hardware-ext` | [SPEC-09](docs/specs/SPEC-09-hardware-specialization.md) | Stage-3 placeholder (GPU/CXL/multi-node) |
| `horndb-harness` | [SPEC-01](docs/specs/SPEC-01-conformance-benchmarks.md) | Conformance + benchmark runner; ships the `harness` binary |

The harness comes **first by design**: every SPEC's acceptance criteria reference a concrete subset of SPEC-01's test corpus, and a SPEC is not satisfied until its subset is green.

## Getting started

### Prerequisites

- Rust **1.88.0** (pinned via `rust-toolchain.toml` — `rustup` will install it automatically).
- **SuiteSparse:GraphBLAS** for `horndb-closure`. By default (`vendored` + `openmp` Cargo features, both on) the crate **builds the vendored submodule from source** — pinned to **v10.3.0** under `crates/closure/vendor/GraphBLAS` — so you need the submodule checked out (`git submodule update --init --recursive`) plus **CMake**, a **C compiler**, and **OpenMP** at build time. `pkg-config` is also required (the `build.rs` probe uses it to emit link flags). To link a system GraphBLAS instead, disable default features and provide one to `pkg-config`; the probe gate is `>=8.0`.
- Optional: `pre-commit` (`pip install pre-commit && pre-commit install`) for the fmt / clippy / build hooks. Pre-commit runs `cargo fmt --all -- --check` only; pre-push runs `cargo clippy --workspace --all-targets -- -D warnings` and `cargo build --workspace` (the full workspace, including `horndb-harness`).
- For the LDBC SPB-256 nightly: Java + the SPB driver JAR.
- For the W3C conformance suite and ORE 2015 ontologies: see the fetch scripts under `crates/harness/scripts/`.

### Build and test

```bash
cargo build --workspace
cargo test  --workspace
cargo test -p horndb-sparql --features server     # SPARQL HTTP tests
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

### Run the conformance harness

Stage-0 plumbing check (no real engine):

```bash
cargo run -p horndb-harness --bin harness -- --engine stub run --allow-failing
```

Stage-1, real engine, full 50-case OWL 2 RL subset:

```bash
./crates/harness/scripts/fetch-w3c-suites.sh
cargo run -p horndb-harness --bin harness --features real-engine -- \
    --engine owlrl run
```

ORE 2015 ten-ontology subset — *scaffolding only at Stage 1.* The selection
(`harness/ore2015-selected.toml`) and fetch script ship, but the harness has no
`ore-run` subcommand yet; wiring the corpus into a real-engine run is Stage-2
work (tracked in `TASKS.md`). To fetch the corpus today:

```bash
./crates/harness/scripts/fetch-ore2015-subset.sh
```

LDBC SPB-256 (requires Java + the SPB driver JAR):

```bash
./crates/harness/scripts/run-spb-256.sh
./crates/harness/scripts/run-graphdb-free-spb-256.sh
cargo run -p horndb-harness --bin harness -- \
    report --suite ldbc-spb-256 --metric editorial-qps
```

Harness state is persisted to `target/harness.sqlite`. Fetched corpora go under `crates/harness/data/` (gitignored).

## CI

- `.github/workflows/ci.yml` — per-PR: fmt, clippy, full workspace tests, SPARQL server tests, and a real-engine conformance run with JUnit publishing.
- `.github/workflows/nightly.yml` — self-hosted runner: LDBC SPB-256 against both HornDB and GraphDB Free.

## Roadmap

| Stage | Scope | Gate |
|---|---|---|
| **Stage 0** | Harness bootstrap | Selected suite plumbing green; deliberate failure is correctly flagged red. |
| **Stage 1** (in progress) | Storage + WCOJ + OWL 2 RL minimal slice | ≥50 W3C OWL 2 RL cases green; within 3× of RDFox materialization on LUBM-100; benchmarked vs RDFox/GraphDB on LDBC SPB-256. |
| **Stage 2** | Full SPEC-02..07 + RDF 1.2 triple terms | Full W3C OWL 2 RL + SPARQL 1.1 + Entailment Regimes green; ORE 2015 OWL 2 RL fragment 100% solved; LDBC SPB SF3 ≥50% of GraphDB Enterprise. |
| **Stage 3** | Hardware specialization | GPU backend for GraphBLAS + WCOJ; CXL tiering; multi-node via DBSP timely-dataflow. Conformance bar from Stage 2 does not drop. |

## Repository map

```
docs/specs/               # SPEC-00..10 — the contracts
docs/plans/               # Per-spec implementation plans (historical)
docs/architecture.md      # Single-page current-state map (Status per subsystem)
TASKS.md                  # Live follow-up list (CRITICAL → LOW)
crates/                   # The nine workspace crates
harness/                  # Workspace-level harness assets (selected.toml, curation/)
.github/workflows/        # CI (per-PR) and nightly (SPB-256)
initial-research.md       # Feasibility study and competitive landscape
```

## Performance

Targets, baselines, current measurements, and reproduction commands live in [`BENCHMARKS.md`](BENCHMARKS.md). Live performance gaps are tracked in [`TASKS.md`](TASKS.md) alongside correctness gaps.

## The name

**Horn** as in **Horn clauses** — implications of the form *B₁ ∧ … ∧ Bₙ → H* with at most one positive conclusion. OWL 2 RL is precisely the fragment of OWL 2 whose entailment rules can be written as Horn rules and evaluated bottom-up to a fixpoint, which is what this engine does; the W3C OWL 2 RL/RDF rules document is, quite literally, a set of Horn rules. So the name picks out the engine's logical core, not just the data it stores.

**DB** because the user-facing shape is a database — load triples, run SPARQL, get answers (with provenance) — rather than a standalone rule engine or a library. It puts HornDB in the same naming neighbourhood as FaunaDB, EdgeDB, SurrealDB.

The name was picked after sweeping roughly forty alternatives (Norse mythology, Norwegian cognition vocabulary, Greek philosophy, owl imagery) for namespace collisions across crates.io, PyPI, npm, GitHub, and the relevant TLDs. *HornDB* was the cleanest that also said something true about the engine.

## License

Apache-2.0 — see [`LICENSE`](LICENSE).
