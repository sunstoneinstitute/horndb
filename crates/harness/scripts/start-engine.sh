#!/usr/bin/env bash
# Bring up a HornDB SPARQL endpoint, optionally materializing an OWL 2 RL
# closure first, ready for an LDBC SPB (or any SPARQL 1.1) client to query.
#
# Two stages, decoupled so the heavy reasoning step runs once:
#
#   1. (optional) Materialize: parse the raw corpus (Turtle ontologies +
#      reference datasets + N-Triples Creative Works) with the OWL 2 RL
#      engine and dump the full closure to a flat N-Triples file via
#      `horndb-bench materialize --dump-nt`.
#   2. Serve: load that flat file (no reasoning) into the in-memory store
#      and expose SPARQL 1.1 over HTTP via the `serve` binary.
#
#   The SPARQL query endpoint is  http://<BIND>/query   (NOT /sparql).
#   SPARQL Update is at           http://<BIND>/update .
#
# Usage:
#   # Materialize a corpus, then serve the closure (blocks in foreground):
#   CORPUS_DIRS="dir1 dir2" MATERIALIZE=1 ./start-engine.sh
#
#   # Serve an already-materialized / flat file directly (no reasoning):
#   DATA_FILES="closure.nt" ./start-engine.sh
#
# Environment knobs:
#   BIND           bind address (default 127.0.0.1:7878)
#   MATERIALIZE    1 to run the OWL 2 RL materialize step (default 0)
#   CORPUS_DIRS    space-separated dirs scanned for *.ttl / *.nt to feed
#                  the materialize step (when MATERIALIZE=1)
#   CORPUS_FILES   space-separated individual files for the materialize step
#   DATA_FILES     space-separated flat files served directly (when
#                  MATERIALIZE=0). Ignored if MATERIALIZE=1.
#   DUMP_NT        path for the materialized closure
#                  (default $ROOT/target/horndb-materialized.nt)
#   RELEASE        1 to build/run the binaries in --release (default 0)
#
# Model: sibling scripts bootstrap-rdfox-spb.sh / run-spb-256.sh.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"

BIND="${BIND:-127.0.0.1:7878}"
MATERIALIZE="${MATERIALIZE:-0}"
DUMP_NT="${DUMP_NT:-$ROOT/target/horndb-materialized.nt}"
RELEASE="${RELEASE:-0}"

CARGO_PROFILE_FLAG=()
TARGET_SUBDIR="debug"
if [[ "$RELEASE" == "1" ]]; then
    CARGO_PROFILE_FLAG=(--release)
    TARGET_SUBDIR="release"
fi

SERVE_BIN="$ROOT/target/$TARGET_SUBDIR/serve"
BENCH_BIN="$ROOT/target/$TARGET_SUBDIR/horndb-bench"

echo "start-engine: building binaries (profile=$TARGET_SUBDIR)..." >&2
cargo build "${CARGO_PROFILE_FLAG[@]}" \
    -p horndb-sparql --bin serve --features server >&2
if [[ "$MATERIALIZE" == "1" ]]; then
    cargo build "${CARGO_PROFILE_FLAG[@]}" -p horndb-bench-rdfox >&2
fi

# ---------------------------------------------------------------------------
# Stage 1 (optional): materialize the corpus to a flat N-Triples file.
# ---------------------------------------------------------------------------
if [[ "$MATERIALIZE" == "1" ]]; then
    # Collect input files: explicit CORPUS_FILES plus every *.ttl/*.nt
    # found (recursively) under each CORPUS_DIRS entry.
    declare -a INPUTS=()
    if [[ -n "${CORPUS_FILES:-}" ]]; then
        # shellcheck disable=SC2206
        INPUTS+=(${CORPUS_FILES})
    fi
    if [[ -n "${CORPUS_DIRS:-}" ]]; then
        for d in ${CORPUS_DIRS}; do
            while IFS= read -r -d '' f; do
                INPUTS+=("$f")
            done < <(find "$d" -type f \( -name '*.ttl' -o -name '*.nt' \) -print0)
        done
    fi
    if [[ ${#INPUTS[@]} -eq 0 ]]; then
        echo "start-engine: MATERIALIZE=1 but no inputs found via CORPUS_DIRS/CORPUS_FILES" >&2
        exit 2
    fi
    echo "start-engine: materializing ${#INPUTS[@]} input file(s) -> $DUMP_NT" >&2
    "$BENCH_BIN" materialize --dump-nt "$DUMP_NT" --data "${INPUTS[@]}" >&2
    DATA_FILES="$DUMP_NT"
fi

# ---------------------------------------------------------------------------
# Stage 2: serve the flat data over HTTP.
# ---------------------------------------------------------------------------
if [[ -z "${DATA_FILES:-}" ]]; then
    echo "start-engine: no DATA_FILES to serve (set DATA_FILES or MATERIALIZE=1 + CORPUS_*)" >&2
    exit 2
fi

echo "start-engine: serving on $BIND" >&2
echo "start-engine: SPARQL query endpoint -> http://$BIND/query" >&2
# shellcheck disable=SC2086
exec "$SERVE_BIN" --bind "$BIND" --data ${DATA_FILES}
