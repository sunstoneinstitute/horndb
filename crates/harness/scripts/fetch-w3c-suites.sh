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

# How to invoke the in-tree extractor/converter below. CI already has an
# optimized harness binary, so it sets HARNESS_BIN to that path rather than
# paying for a second, debug build of the workspace.
if [[ -n "${HARNESS_BIN:-}" ]]; then
    harness() { "$HARNESS_BIN" "$@"; }
else
    harness() { cargo run -p horndb-harness --bin harness -- "$@"; }
fi

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"
DATA="$ROOT/crates/harness/data"
mkdir -p "$DATA"

OWL2_PROFILE_RL_URL="https://www.w3.org/2009/11/owl-test/profile-RL.rdf"
SPARQL_URL="https://www.w3.org/2009/sparql/docs/tests/sparql11-test-suite-20121023.tar.gz"
RDF12_NT_BASE="https://w3c.github.io/rdf-tests/rdf/rdf12/rdf-n-triples/syntax"
SPARQL10_BASE="https://w3c.github.io/rdf-tests/sparql/sparql10"
GSP_BASE="https://w3c.github.io/rdf-tests/sparql/sparql11/graph-store-protocol"

OWL2_DIR="$DATA/w3c-owl2-rl-tests"
SPARQL_DIR="$DATA/w3c-sparql11-tests"
SPARQL10_DIR="$DATA/w3c-sparql10-tests"
GSP_DIR="$DATA/w3c-gsp-tests"
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
harness extract-owl2-rl \
    --source "$OWL2_DIR/profile-RL.rdf" \
    --out    "$OWL2_DIR"

# Convert the SPARQL suite's RDF/XML manifests to Turtle so the in-tree
# manifest parser can read them (Task 17 follow-up).
harness convert-manifests --root "$SPARQL_DIR"

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

# SPARQL 1.1 *evaluation* suite (`[suites.sparql11-eval]`, HDB-128). Unlike
# every other suite here it is NOT mirrored into tests/fixtures: the harness
# reads the extracted tarball directly, via
#   $SPARQL_DIR/sparql11-test-suite/manifest-all.ttl
# whose `mf:include` list the manifest reader follows. So running this script is
# a precondition for `harness run` — without it the suite's manifest is missing
# and the run errors out. CI's conformance job runs this script (with
# HARNESS_BIN set) for exactly that reason.

# SPARQL 1.1 Graph Store Protocol suite (`[suites.sparql11-gsp]`, SPEC-28 S5).
#
# NOT in the 2012 tarball fetched above. That tarball's `http-rdf-update/`
# directory — which SPEC-28 names — holds only a prose draft (`tests.txt`) and,
# in the maintained mirror, a manifest whose every case is marked
# `dawg:Deprecated` with its request/response spelled out in a Markdown
# `rdfs:comment`. Nothing there is machine-readable. The replacement upstream
# points at is `graph-store-protocol/`, whose cases use the W3C HTTP-in-RDF
# vocabulary (`ht:Request` / `ht:Response`) and still carry the old
# `http-rdf-update/manifest#` case IRIs. That is what we fetch.
#
# Like `sparql11-eval`, this corpus is read straight from `data/` rather than
# mirrored into `tests/fixtures/` — hence `fetched = true` in selected.toml.
mkdir -p "$GSP_DIR"
for f in manifest.ttl manifest-direct.ttl manifest-indirect.ttl; do
    if [[ ! -f "$GSP_DIR/$f" ]]; then
        echo "fetching graph-store-protocol/${f}…"
        curl -sSfL "$GSP_BASE/$f" -o "$GSP_DIR/$f"
    fi
done

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
#
# Note for the next fetch: upstream `dataset-09b.rq` and `dataset-10b.rq` are
# byte-identical apart from one newline (both are
# `FROM <data-g3-dup.ttl> FROM NAMED <data-g3.ttl>`), so the two mirrored
# cases are the same test twice. The manifest lists them as distinct cases,
# so this is either an upstream quirk or a `10b` that was meant to swap the
# two files. Re-check when this list is next re-fetched; if upstream has
# diverged, `dataset-10b`'s fixture needs regenerating.
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

# SPARQL 1.1 UPDATE evaluation families (SPEC-28 phase 4, #267).
#
# These come from the SPARQL 1.1 tarball already extracted above (they are NOT
# a separate fetch): the `add/`, `copy/`, `move/`, `clear/`, `drop/`,
# `delete-insert/`, and `delete/` sub-suites under
#   $SPARQL_DIR/sparql11-test-suite/{add,copy,move,clear,drop,delete-insert,delete}/
# Of `delete/`, only `delete-with-02` and `delete-with-06` are mirrored (the two
# WITH-vs-GRAPH cases, issue #281); the rest of that family is not mirrored yet.
# Each manifest entry is a `ut:UpdateEvaluationTest`: an `mf:action` with a
# `ut:request` (the `.ru`), a `ut:data` (default-graph state), and zero or more
# `ut:graphData [ ut:graph <file> ; rdfs:label <IRI> ]` (named-graph state), and
# an `mf:result` of the same shape for the expected final state.
#
# The per-case fixture dirs under
# `crates/harness/tests/fixtures/sparql11/update_subset/<case>/` are a checked-in
# mirror (so CI needs no network), derived by the same mechanical
# transformations a manifest-driven update-eval runner applies:
#
#   1. relative IRIs resolve against BASE http://example.org/ — the namespace
#      every data file uses — so the `<>` in clear-default/drop-default becomes
#      <http://example.org/>. Graph names are the manifest `rdfs:label` IRIs and
#      match the `:gN` the `.ru` requests resolve to.
#   2. `ut:data` + `ut:graphData` collapse into one `data.trig` (initial) and one
#      `expected.trig` (final): default-graph triples at top level, each named
#      graph's triples inside a `GRAPH <label> { … }` block. An empty component
#      (`empty.ttl`, or a `ut:graphData` that is emptied) contributes no quads,
#      so it is never emitted as an empty GRAPH block (D11: no empty graphs).
#   3. `ut:request` is copied verbatim as `request.ru`.
#
# The runner is `crates/sparql/tests/w3c_update_suite.rs`; the graded set is
# `harness/selected.toml`'s `[sparql_update]` section. The upstream cases left
# out — the three `clear/` cases that keep an empty-but-existing named graph
# (D11), plus the `delete-insert/` NegativeSyntaxTest11 cases (syntax-graded,
# not eval) — are in `harness/KNOWN-MANIFEST-BUGS.md`. As with the graph/dataset
# fixtures, this script does NOT overwrite the checked-in mirror; regenerate by
# re-running the mechanical transforms above against a fresh tarball extract.
if [[ -d "$SPARQL_DIR/sparql11-test-suite/add" ]]; then
    echo "upstream SPARQL 1.1 update families present under $SPARQL_DIR/sparql11-test-suite (see update_subset notes above)."
fi

echo "done."
