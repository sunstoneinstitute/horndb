#!/usr/bin/env bash
# Same SPB-256 driver, pointed at a local GraphDB Free instance, for
# the F10 differential comparison required at Stage 1.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"

# GraphDB (RDF4J protocol) splits query and update: the repository URL
# is the query endpoint, and `<repo>/statements` is the update endpoint.
ENDPOINT="${GRAPHDB_FREE_ENDPOINT:-http://127.0.0.1:7200/repositories/spb}"
UPDATE_ENDPOINT="${GRAPHDB_FREE_UPDATE_ENDPOINT:-${ENDPOINT}/statements}"
JAR="${SPB_DRIVER_JAR:-$ROOT/crates/harness/data/ldbc-spb/spb-driver.jar}"
SCENARIO="${SPB_SCENARIO:-$ROOT/crates/harness/data/ldbc-spb/sf-0.256.properties}"
DURATION="${SPB_DURATION_SECONDS:-600}"

cargo run -p horndb-harness --bin harness --release -- \
    spb-run \
    --driver-jar "$JAR" \
    --scenario "$SCENARIO" \
    --endpoint "$ENDPOINT" \
    --endpoint-update "$UPDATE_ENDPOINT" \
    --duration "$DURATION" \
    --label "graphdb-free"
