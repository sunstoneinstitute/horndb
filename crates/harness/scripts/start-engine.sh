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
#   BIND           bind address (default 127.0.0.1:3840)
#   MATERIALIZE    1 to run the OWL 2 RL materialize step (default 0)
#   CORPUS_DIRS    space-separated dirs scanned for *.ttl / *.nt to feed
#                  the materialize step (when MATERIALIZE=1)
#   CORPUS_FILES   space-separated individual files for the materialize step
#   DATA_FILES     space-separated flat files served directly (when
#                  MATERIALIZE=0). Ignored if MATERIALIZE=1.
#   DUMP_NT        path for the materialized closure
#                  (default $ROOT/target/horndb-materialized.nt)
#   RELEASE        1 to build/run the binaries in --release (default 0)
#   MEMORY_MAX     hard memory ceiling for the server process, e.g. "90G"
#                  (default: unset = no ceiling). See "Memory ceiling" below.
#
# Model: sibling scripts bootstrap-rdfox-spb.sh / run-spb-256.sh.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"

BIND="${BIND:-127.0.0.1:3840}"
MATERIALIZE="${MATERIALIZE:-0}"
DUMP_NT="${DUMP_NT:-$ROOT/target/horndb-materialized.nt}"
RELEASE="${RELEASE:-0}"

CARGO_PROFILE_FLAG=()
TARGET_SUBDIR="debug"
if [[ "$RELEASE" == "1" ]]; then
    CARGO_PROFILE_FLAG=(--release)
    TARGET_SUBDIR="release"
fi

# Honor CARGO_TARGET_DIR (the bench runner points it at host-local disk);
# fall back to the repo-local target/ when it is unset.
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
SERVE_BIN="$TARGET_DIR/$TARGET_SUBDIR/serve"
BENCH_BIN="$TARGET_DIR/$TARGET_SUBDIR/horndb-bench"

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

# ---------------------------------------------------------------------------
# Memory ceiling (optional).
#
# SPEC-31 bounds the executor's row buffers per query; it does not bound the
# store-side index and snapshot memory a query can trigger (HDB-229, HDB-230,
# HDB-231). On a large corpus that means nothing stops the server from
# consuming the whole host — which is what happened in HDB-167, where an SPB
# run at SF=0.256 exhausted hornbench's 124 GiB and took the machine off the
# network, needing a manual restart.
#
# MEMORY_MAX puts the server in a transient cgroup with a hard ceiling, so a
# runaway query kills the *server* (visible as a failed benchmark leg) instead
# of the host. It is a host guard, not per-query accounting: the whole process
# shares one budget.
#
# Prefers the caller's own user manager (no privilege). Falls back to a system
# scope via passwordless sudo, then to running uncapped with a warning.
# ---------------------------------------------------------------------------
if [[ -n "${MEMORY_MAX:-}" ]]; then
    export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
    export DBUS_SESSION_BUS_ADDRESS="${DBUS_SESSION_BUS_ADDRESS:-unix:path=$XDG_RUNTIME_DIR/bus}"
    SCOPE=(--scope --quiet --collect -p "MemoryMax=$MEMORY_MAX" -p MemorySwapMax=0)
    if systemd-run --user "${SCOPE[@]}" -- /bin/true >/dev/null 2>&1; then
        echo "start-engine: memory ceiling $MEMORY_MAX (user scope)" >&2
        # shellcheck disable=SC2086
        exec systemd-run --user "${SCOPE[@]}" -- \
            "$SERVE_BIN" --bind "$BIND" --data ${DATA_FILES}
    elif sudo -n systemd-run "${SCOPE[@]}" -- /bin/true >/dev/null 2>&1; then
        echo "start-engine: memory ceiling $MEMORY_MAX (system scope, via sudo)" >&2
        # shellcheck disable=SC2086
        exec sudo -n --preserve-env systemd-run "${SCOPE[@]}" -- \
            "$SERVE_BIN" --bind "$BIND" --data ${DATA_FILES}
    fi
    echo "start-engine: MEMORY_MAX=$MEMORY_MAX requested but no usable systemd-run;" \
         "serving UNCAPPED" >&2
fi

# shellcheck disable=SC2086
exec "$SERVE_BIN" --bind "$BIND" --data ${DATA_FILES}
