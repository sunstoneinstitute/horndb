#!/usr/bin/env bash
# Same SPB-256 driver, pointed at a local RDFox instance, for the
# F10 differential comparison (SPEC-01). Sister to
# run-graphdb-free-spb-256.sh — the harness's spb-run subcommand is
# engine-agnostic, so the only thing that changes per-competitor is
# the endpoint URL and the --label.
#
# Publication note: RDFox commercial licenses typically forbid
# publishing comparative benchmark numbers ("DeWitt clause").
# Internal use under a benchmarking license is permitted; anything
# leaving the project (paper, blog, deck) needs legal review first.
# See SPEC-01 Risks and docs/benchmarks.md.
#
# Pre-conditions (must be satisfied before this script runs):
#
#   1. RDFox is installed and the binary is on $PATH (or pointed at
#      via $RDFOX_BIN). A valid license is in place — either at the
#      default path $HOME/.RDFox/RDFox.lic, or supplied inline via
#      the RDFOX_LICENSE_CONTENT env var (handy for CI).
#
#   2. An RDFox endpoint is running and exposes a datastore named
#      "$RDFOX_DATASTORE" (default: spb) configured for OWL 2 RL
#      reasoning. Minimal recipe to start one in another terminal:
#
#        RDFox -persistence file daemon \
#            init "endpoint.port=12110" \
#            "set datastore.type par-complex-nn" \
#            "create $RDFOX_DATASTORE" \
#            "active $RDFOX_DATASTORE" \
#            "import <spb-ontology.ttl>" \
#            "import <spb-rules.dlog>"
#
#      The SPB driver's load phase will then push the generated
#      dataset triples into the same store via SPARQL UPDATE.
#
#   3. The LDBC SPB driver JAR and the SF=0.256 scenario file are
#      present at the locations used by the sibling scripts.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"

RDFOX_HOST="${RDFOX_HOST:-127.0.0.1}"
RDFOX_PORT="${RDFOX_PORT:-12110}"
RDFOX_DATASTORE="${RDFOX_DATASTORE:-spb}"
ENDPOINT="${RDFOX_ENDPOINT:-http://${RDFOX_HOST}:${RDFOX_PORT}/datastores/${RDFOX_DATASTORE}/sparql}"

JAR="${SPB_DRIVER_JAR:-$ROOT/crates/harness/data/ldbc-spb/spb-driver.jar}"
SCENARIO="${SPB_SCENARIO:-$ROOT/crates/harness/data/ldbc-spb/sf-0.256.properties}"

cargo run -p horndb-harness --bin harness --release -- \
    spb-run \
    --driver-jar "$JAR" \
    --scenario "$SCENARIO" \
    --endpoint "$ENDPOINT" \
    --duration 600 \
    --label "rdfox"
