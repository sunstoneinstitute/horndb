#!/usr/bin/env bash
# Run LDBC SPB-256 against the local HornDB engine and record the
# numbers into the harness DB.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"

# Pre-conditions:
#   1. The engine is running and exposing SPARQL 1.1 over HTTP. The
#      `serve` binary (see start-engine.sh) splits query and update:
#      query at  $HORNDB_ENDPOINT        (default .../query)
#      update at $HORNDB_UPDATE_ENDPOINT (default .../update)
#      — NOT a single /sparql path.
#   2. The LDBC SPB driver JAR is at $SPB_DRIVER_JAR.
#   3. The scenario file is at $SPB_SCENARIO. Its relative paths
#      (ontologies, queries, substitution params) resolve against the
#      scenario file's own directory, so point it at the prepared SPB
#      assets tree (the Ant `dist/`), not a bare copy.
ENDPOINT="${HORNDB_ENDPOINT:-http://127.0.0.1:3840/query}"
UPDATE_ENDPOINT="${HORNDB_UPDATE_ENDPOINT:-http://127.0.0.1:3840/update}"
JAR="${SPB_DRIVER_JAR:-$ROOT/crates/harness/data/ldbc-spb/spb-driver.jar}"
SCENARIO="${SPB_SCENARIO:-$ROOT/crates/harness/data/ldbc-spb/sf-0.256.properties}"
DURATION="${SPB_DURATION_SECONDS:-600}"

cargo run -p horndb-harness --bin harness --release --features real-engine -- \
    spb-run \
    --driver-jar "$JAR" \
    --scenario "$SCENARIO" \
    --endpoint "$ENDPOINT" \
    --endpoint-update "$UPDATE_ENDPOINT" \
    --duration "$DURATION" \
    --label "horndb"
