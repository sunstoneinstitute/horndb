# reasoner-harness

SPEC-01 conformance and benchmarking harness. See
`specs/SPEC-01-conformance-benchmarks.md` and
`plans/2026-05-24-SPEC-01-conformance-harness.md`.

## Quick start

Run the currently-selected subset against the in-tree stub engine:

```bash
cargo run -p reasoner-harness --bin harness -- \
    --engine stub \
    run \
    --junit target/junit.xml \
    --allow-failing
```

`--allow-failing` is needed locally because `harness/selected.toml`
intentionally includes one test the stub cannot pass; this is how we
prove the harness flags red on real failure.

## Query the trend DB

```bash
cargo run -p reasoner-harness --bin harness -- \
    report --suite owl2 --metric pass-rate
```

## CI

`.github/workflows/ci.yml` runs the same `harness run` *without*
`--allow-failing`, so any newly-broken case in the selected subset
blocks the PR.
