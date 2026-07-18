#!/usr/bin/env bash
# Same SPB-256 driver, pointed at a local Oxigraph instance — used by both
# Oxigraph A/B legs of the nightly (as-loaded and optimized store). Sister to
# run-graphdb-free-spb-256.sh — the harness's spb-run subcommand is
# engine-agnostic, so the only thing that changes per-competitor is the
# endpoint URL and the --label.
#
# Oxigraph exposes SPARQL 1.1 Query at /query and Update at /update on its bind
# address (default 127.0.0.1:7878), so both endpoints share one port.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"

BIND="${OXIGRAPH_BIND:-127.0.0.1:7878}"
# Trend label (the `dataset` column). The optimized-store leg overrides this
# with `oxigraph-optimized` so the two configurations chart as separate series.
LABEL="${OXIGRAPH_LABEL:-oxigraph}"
ENDPOINT="${OXIGRAPH_ENDPOINT:-http://${BIND}/query}"
UPDATE_ENDPOINT="${OXIGRAPH_UPDATE_ENDPOINT:-http://${BIND}/update}"
JAR="${SPB_DRIVER_JAR:-$ROOT/crates/harness/data/ldbc-spb/spb-driver.jar}"
SCENARIO="${SPB_SCENARIO:-$ROOT/crates/harness/data/ldbc-spb/sf-0.256.properties}"
DURATION="${SPB_DURATION_SECONDS:-600}"

# `spb-run` only talks HTTP to the standing Oxigraph, so the real engine isn't
# needed here — but building with `--features real-engine` keeps the harness
# Cargo fingerprint identical to the HornDB and GraphDB legs, so all three
# reuse one cached build instead of recompiling the harness.
cargo run -p horndb-harness --bin harness --release --features real-engine -- \
    spb-run \
    --driver-jar "$JAR" \
    --scenario "$SCENARIO" \
    --endpoint "$ENDPOINT" \
    --endpoint-update "$UPDATE_ENDPOINT" \
    --duration "$DURATION" \
    --label "$LABEL"
