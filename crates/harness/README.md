# horndb-harness

SPEC-01 conformance and benchmarking harness. See
`specs/SPEC-01-conformance-benchmarks.md` and
`plans/PLAN-01-01-conformance-harness.md`.

## Local invocation

Stage-0 (stub-only, no real engine yet):

```bash
cargo run -p horndb-harness --bin harness -- \
    --engine stub \
    run \
    --allow-failing
```

Stage-1 (real engine, full 50-case OWL 2 RL subset):

```bash
./crates/harness/scripts/fetch-w3c-suites.sh
cargo run -p horndb-harness --bin harness --features real-engine -- \
    --engine owlrl \
    run
```

ORE 2015 ten-ontology subset:

```bash
./crates/harness/scripts/fetch-ore2015-subset.sh
cargo run -p horndb-harness --bin harness --features real-engine -- \
    ore-run --selected harness/ore2015-selected.toml
```

LDBC SPB-256 (requires Java + the SPB driver JAR):

```bash
./crates/harness/scripts/run-spb-256.sh
./crates/harness/scripts/run-graphdb-free-spb-256.sh
cargo run -p horndb-harness --bin harness -- \
    report --suite ldbc-spb-256 --metric editorial-qps
```

## CI

- `.github/workflows/ci.yml` — per-PR correctness run (selected subset, real engine).
- `.github/workflows/nightly.yml` — SPB-256 horndb vs GraphDB Free.
