#!/usr/bin/env bash
# Same SPB-256 driver, pointed at a local GraphDB Free instance, for
# the F10 differential comparison required at Stage 1.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"

ENDPOINT="${GRAPHDB_FREE_ENDPOINT:-http://127.0.0.1:7200/repositories/spb}"
JAR="${SPB_DRIVER_JAR:-$ROOT/crates/harness/data/ldbc-spb/spb-driver.jar}"
SCENARIO="${SPB_SCENARIO:-$ROOT/crates/harness/data/ldbc-spb/sf-0.256.properties}"

cargo run -p horndb-harness --bin harness --release -- \
    spb-run \
    --driver-jar "$JAR" \
    --scenario "$SCENARIO" \
    --endpoint "$ENDPOINT" \
    --duration 600 \
    --label "graphdb-free"
