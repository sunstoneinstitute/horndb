#!/usr/bin/env bash
# Run LDBC SPB-256 against the local HornDB engine and record the
# numbers into the harness DB.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"

# Pre-conditions:
#   1. The engine is running and exposing a SPARQL 1.1 endpoint at
#      $HORNDB_ENDPOINT (default http://127.0.0.1:7878/sparql).
#   2. The LDBC SPB driver JAR is at $SPB_DRIVER_JAR.
#   3. The SF=0.256 scenario file is at $SPB_SCENARIO.
ENDPOINT="${HORNDB_ENDPOINT:-http://127.0.0.1:7878/sparql}"
JAR="${SPB_DRIVER_JAR:-$ROOT/crates/harness/data/ldbc-spb/spb-driver.jar}"
SCENARIO="${SPB_SCENARIO:-$ROOT/crates/harness/data/ldbc-spb/sf-0.256.properties}"

cargo run -p horndb-harness --bin harness --release --features real-engine -- \
    spb-run \
    --driver-jar "$JAR" \
    --scenario "$SCENARIO" \
    --endpoint "$ENDPOINT" \
    --duration 600 \
    --label "horndb"
