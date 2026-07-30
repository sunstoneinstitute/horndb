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
SPARQL10_BASE="https://w3c.github.io/rdf-tests/sparql/sparql10"

OWL2_DIR="$DATA/w3c-owl2-rl-tests"
SPARQL_DIR="$DATA/w3c-sparql11-tests"
SPARQL10_DIR="$DATA/w3c-sparql10-tests"
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
        echo "fetching rdf12-n-triples/${f}…"
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

# SPARQL 1.0 `graph/` + `dataset/` evaluation families (SPEC-28 S7, #266).
#
# These two families are NOT in the SPARQL 1.1 tarball fetched above — they
# only ever existed in the SPARQL 1.0 (DAWG) suite, so they come from the
# maintained `rdf-tests` mirror, file by file. The allowlist below is every
# file the two manifests reference, plus the data files the `dataset/`
# queries name in their own `FROM` / `FROM NAMED` clauses (the manifest does
# not list those).
#
# The per-case fixture dirs under
# `crates/harness/tests/fixtures/sparql11/selected_subset/{graph,dataset}-*`
# are a checked-in mirror of these files (so CI needs no network), derived by
# three mechanical transformations — the same ones a manifest-driven W3C
# runner applies:
#
#   1. relative IRIs resolve against the upstream file IRI. Data files are
#      parsed with that base; each `query.rq` gains one explicit
#      `BASE <upstream-query-iri>` line and is otherwise the upstream text
#      (the repo's pre-commit hooks additionally strip trailing whitespace
#      and normalise the final newline — neither changes the query).
#   2. the dataset becomes one `data.trig`: for `graph/`, the manifest's
#      `qt:data` files land in the default graph and `qt:graphData` files in
#      a named graph per file; for `dataset/`, every file named by the
#      query's `FROM`/`FROM NAMED` lands in a named graph. Graph names are
#      the files' upstream IRIs. Blank-node labels are made per-document
#      (`<file-stem>-bN`), which is the RDF merge SPARQL 1.1 §13.1 defines
#      the dataset by.
#   3. the `rs:ResultSet` Turtle result graph becomes `expected.srj`
#      (SPARQL Results JSON), the format `w3c_suite.rs` diffs against.
#
# This script therefore does NOT overwrite those fixtures. Which cases are
# graded is `harness/selected.toml`'s `[sparql_query]` section; the upstream
# cases left out are in `harness/KNOWN-MANIFEST-BUGS.md`.
SPARQL10_GRAPH_FILES=(
    manifest.ttl
    data-g1.ttl data-g2.ttl data-g3.ttl data-g3-dup.ttl data-g4.ttl
    data-optional.ttl data-variable-join.ttl
    graph-01.rq graph-01.ttl graph-02.rq graph-02.ttl graph-03.rq graph-03.ttl
    graph-04.rq graph-04.ttl graph-05.rq graph-05.ttl graph-06.rq graph-06.ttl
    graph-07.rq graph-07.ttl graph-08.rq graph-08.ttl graph-09.rq graph-09.ttl
    graph-10.rq graph-10.ttl graph-11.rq graph-11.ttl
    graph-empty.rq graph-empty.ttl
    graph-empty-exist.rq graph-empty-exist.ttl
    graph-empty-not-exist.rq graph-empty-not-exist.ttl
    graph-optional.rq graph-optional.ttl
    graph-variable-join.rq graph-variable-join.ttl
    graph-variable-scope.rq graph-variable-scope.ttl
)
SPARQL10_DATASET_FILES=(
    manifest.ttl
    data-g1.ttl data-g2.ttl data-g3.ttl data-g4.ttl
    data-g1-dup.ttl data-g2-dup.ttl data-g3-dup.ttl data-g4-dup.ttl
    dataset-01.rq dataset-01.ttl dataset-02.rq dataset-02.ttl
    dataset-03.rq dataset-03.ttl dataset-04.rq dataset-04.ttl
    dataset-05.rq dataset-05.ttl dataset-06.rq dataset-06.ttl
    dataset-07.rq dataset-07.ttl dataset-08.rq dataset-08.ttl
    dataset-09b.rq dataset-09.ttl dataset-10b.rq dataset-10.ttl
    dataset-11.rq dataset-11.ttl dataset-12b.rq dataset-12.ttl
)
fetch_sparql10() {
    local family="$1"; shift
    mkdir -p "$SPARQL10_DIR/$family"
    for f in "$@"; do
        if [[ ! -f "$SPARQL10_DIR/$family/$f" ]]; then
            echo "fetching sparql10/${family}/${f}…"
            curl -sSfL "$SPARQL10_BASE/$family/$f" -o "$SPARQL10_DIR/$family/$f"
        fi
    done
}
fetch_sparql10 graph "${SPARQL10_GRAPH_FILES[@]}"
fetch_sparql10 dataset "${SPARQL10_DATASET_FILES[@]}"

echo "done."
