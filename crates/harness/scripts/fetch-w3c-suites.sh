#!/usr/bin/env bash
# Fetch the W3C OWL 2 Test Cases and SPARQL 1.1 Test Suite into
# crates/harness/data/, then convert their RDF/XML manifests to Turtle
# so the in-tree manifest parser (src/manifest.rs) can read them.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"
DATA="$ROOT/crates/harness/data"
mkdir -p "$DATA"

OWL2_URL="https://www.w3.org/2009/11/owl-test/testOntology-20091022.zip"
SPARQL_URL="https://www.w3.org/2009/sparql/docs/tests/sparql11-test-suite-20121023.tar.gz"

if [[ ! -d "$DATA/w3c-owl2-tests" ]]; then
    echo "fetching OWL 2 test cases…"
    curl -sSfL "$OWL2_URL" -o "$DATA/owl2.zip"
    mkdir -p "$DATA/w3c-owl2-tests"
    (cd "$DATA/w3c-owl2-tests" && unzip -q "$DATA/owl2.zip")
fi

if [[ ! -d "$DATA/w3c-sparql11-tests" ]]; then
    echo "fetching SPARQL 1.1 test suite…"
    curl -sSfL "$SPARQL_URL" -o "$DATA/sparql11.tgz"
    mkdir -p "$DATA/w3c-sparql11-tests"
    tar -xzf "$DATA/sparql11.tgz" -C "$DATA/w3c-sparql11-tests"
fi

# Convert each .rdf manifest into .ttl using the harness CLI helper.
# (The convert subcommand is added in Task 17.)
cargo run -p reasoner-harness --bin harness -- \
    convert-manifests --root "$DATA"

echo "done."
