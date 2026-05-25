#!/usr/bin/env bash
# Fetch the W3C OWL 2 RL profile test cases and the SPARQL 1.1 test
# suite into crates/harness/data/, then materialise harness-format
# manifests via the in-tree extractor.
#
# OWL 2:   the canonical source is the file tree at
#          https://www.w3.org/2009/11/owl-test/.  We only need the
#          per-profile aggregate (`profile-RL.rdf`) — every Profile-RL
#          test case carries its premise/conclusion as embedded
#          RDF/XML strings inside that file (see SPEC-01 Stage-1
#          ingestion notes in harness/curation/owl2-rl-50.md).
# SPARQL:  the 2012 1.1 suite tarball is still served as-is.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"
DATA="$ROOT/crates/harness/data"
mkdir -p "$DATA"

OWL2_PROFILE_RL_URL="https://www.w3.org/2009/11/owl-test/profile-RL.rdf"
SPARQL_URL="https://www.w3.org/2009/sparql/docs/tests/sparql11-test-suite-20121023.tar.gz"

OWL2_DIR="$DATA/w3c-owl2-rl-tests"
SPARQL_DIR="$DATA/w3c-sparql11-tests"

if [[ ! -f "$OWL2_DIR/profile-RL.rdf" ]]; then
    echo "fetching OWL 2 RL profile test cases…"
    mkdir -p "$OWL2_DIR"
    curl -sSfL "$OWL2_PROFILE_RL_URL" -o "$OWL2_DIR/profile-RL.rdf"
fi

if [[ ! -d "$SPARQL_DIR" || -z "$(ls -A "$SPARQL_DIR" 2>/dev/null)" ]]; then
    echo "fetching SPARQL 1.1 test suite…"
    mkdir -p "$SPARQL_DIR"
    curl -sSfL "$SPARQL_URL" -o "$DATA/sparql11.tgz"
    tar -xzf "$DATA/sparql11.tgz" -C "$SPARQL_DIR"
fi

# Materialise the OWL 2 RL manifest into harness-friendly Turtle plus
# sibling .premise.ttl / .conclusion.ttl files.  Idempotent — the
# extractor skips cases whose sibling files already exist.
cargo run -p horndb-harness --bin harness -- \
    extract-owl2-rl \
    --source "$OWL2_DIR/profile-RL.rdf" \
    --out    "$OWL2_DIR"

# Convert the SPARQL suite's RDF/XML manifests to Turtle so the in-tree
# manifest parser can read them (Task 17 follow-up).
cargo run -p horndb-harness --bin harness -- \
    convert-manifests --root "$SPARQL_DIR"

echo "done."
