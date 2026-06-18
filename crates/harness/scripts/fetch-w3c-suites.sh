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
RDF12_NT_BASE="https://w3c.github.io/rdf-tests/rdf/rdf12/rdf-n-triples/syntax"

OWL2_DIR="$DATA/w3c-owl2-rl-tests"
SPARQL_DIR="$DATA/w3c-sparql11-tests"
RDF12_NT_DIR="$DATA/rdf12-n-triples"
RDF12_NT_FIXTURES="$ROOT/crates/harness/tests/fixtures/rdf12-n-triples"

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

# W3C RDF 1.2 N-Triples syntax suite. Unlike the OWL 2 RL aggregate
# (which embeds premise/conclusion ontologies as RDF/XML literals), the
# RDF 1.2 N-Triples tests ship as plain `.nt` files referenced by an
# already-Turtle manifest, so no extraction / DOCTYPE rewriting is
# needed. We mirror them into both `data/` (canonical fetch landing
# pad, gitignored) and `tests/fixtures/rdf12-n-triples/` (checked in
# so CI can run the suite without network access). The 10 files below
# are the IDs selected in `harness/selected.toml` — extend this list
# when expanding the selection.
RDF12_NT_FILES=(
    manifest.ttl
    ntriples12-syntax-01.nt
    ntriples12-syntax-02.nt
    ntriples12-syntax-03.nt
    ntriples12-nested-1.nt
    ntriples12-bad-syntax-01.nt
    ntriples12-bad-syntax-05.nt
    ntriples12-bad-syntax-06.nt
    ntriples12-bad-syntax-07.nt
    ntriples12-bad-syntax-08.nt
    ntriples12-bad-syntax-10.nt
)
mkdir -p "$RDF12_NT_DIR" "$RDF12_NT_FIXTURES"
for f in "${RDF12_NT_FILES[@]}"; do
    if [[ ! -f "$RDF12_NT_DIR/$f" ]]; then
        echo "fetching rdf12-n-triples/$f…"
        curl -sSfL "$RDF12_NT_BASE/$f" -o "$RDF12_NT_DIR/$f"
    fi
    # Mirror into the checked-in tests/fixtures path so CI (which does
    # not invoke this script) sees the same bytes. cp -n keeps an
    # already-staged fixture untouched if a fix-up has been hand-edited.
    cp -n "$RDF12_NT_DIR/$f" "$RDF12_NT_FIXTURES/$f" 2>/dev/null || true
done

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

# SPARQL 1.1 *syntax* suite (issue #110). The `[suites.sparql11-syntax]`
# selection in harness/selected.toml runs a curated, checked-in subset of the
# upstream syntax sub-suites — they are graded by `spargebra` accept/reject,
# need no data/results, and run in sub-milliseconds, so they ride the per-PR
# correctness tier without a network fetch. The upstream cases this subset is
# drawn from land here after the tarball extract above:
#   $SPARQL_DIR/syntax-query/         (PositiveSyntaxTest11 / NegativeSyntaxTest11)
#   $SPARQL_DIR/syntax-update-1/      (PositiveUpdateSyntaxTest11 / Negative…)
#   $SPARQL_DIR/syntax-update-2/
# The checked-in fixtures under crates/harness/tests/fixtures/sparql11-syntax/
# are intentionally hand-curated (stable IDs, no large corpus) rather than a
# byte-copy of any single upstream file, so this script does NOT overwrite
# them. To grow the selection, add cases to that directory + selected.toml;
# the manifest reader (mf:*SyntaxTest11) and runner already understand them.
if [[ -d "$SPARQL_DIR/syntax-query" ]]; then
    echo "upstream SPARQL syntax sub-suites present under $SPARQL_DIR (see sparql11-syntax notes above)."
fi

echo "done."
