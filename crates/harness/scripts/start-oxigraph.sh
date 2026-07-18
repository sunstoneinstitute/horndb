#!/usr/bin/env bash
# Per-run bring-up of the Oxigraph A/B reference engine for the nightly LDBC
# SPB-256 leg. Mirrors start-graphdb-free.sh: ensure the branch-pinned Oxigraph
# binary is present, serve the already-loaded persisted store detached, wait
# until the query endpoint answers, then return. The workflow tears it down
# after the leg (stop step: `pkill -f 'oxigraph serve'`).
#
# Why per-run start/stop and not a standing service: at SF=0.256 an idle
# Oxigraph would hold GBs of page cache and compete with HornDB for RAM and OS
# file cache during the HornDB measurement leg. Running each engine only for
# its own leg keeps the A/B legs isolated (same rationale as the GraphDB leg).
#
# The heavy one-time dataset load lives in bootstrap-oxigraph-spb.sh; starting
# here only re-opens the already-persisted RocksDB store. If that store is
# missing, this exits non-zero so the workflow (continue-on-error) skips the
# A/B leg rather than serving an empty store.
#
# Environment knobs:
#   OXIGRAPH_VERSION    release to run (default 0.5.9); must match the pin in
#                       the nightly workflow env and in bootstrap-oxigraph-spb.sh
#   OXIGRAPH_HOME_BASE  base dir holding the binary + store (default $HOME/oxigraph)
#   OXIGRAPH_STORE      persisted RocksDB store to serve (default $OXIGRAPH_HOME_BASE/spb-store;
#                       the optimized leg points this at spb-store-optimized)
#   OXIGRAPH_BIND       listen address (default 127.0.0.1:7878)
set -euo pipefail

VER="${OXIGRAPH_VERSION:-0.5.9}"
OX_BASE="${OXIGRAPH_HOME_BASE:-$HOME/oxigraph}"
STORE="${OXIGRAPH_STORE:-$OX_BASE/spb-store}"
BIND="${OXIGRAPH_BIND:-127.0.0.1:7878}"
BIN="${OXIGRAPH_BIN:-$OX_BASE/oxigraph-${VER}}"
ASSET_URL="https://github.com/oxigraph/oxigraph/releases/download/v${VER}/oxigraph_v${VER}_x86_64_linux_gnu"

# Clear any pidfile from a previous run first: on the persistent bench runner
# /tmp survives across runs, so a stale pidfile whose PID has since been
# recycled could make the workflow's stop step SIGTERM an unrelated process.
# Every path below either writes a fresh pidfile (we started the server) or
# leaves none (already serving / store missing → nothing for stop to kill).
rm -f /tmp/oxigraph-engine.pid

endpoint_ready() {
    curl -fsS --max-time 5 -H 'Accept: application/sparql-results+json' \
        -G --data-urlencode 'query=ASK{}' "http://${BIND}/query" >/dev/null 2>&1
}

# Idempotent: an Oxigraph left serving from a previous run is fine.
if endpoint_ready; then
    echo "start-oxigraph: already serving on ${BIND}" >&2
    exit 0
fi

# Ensure the branch-pinned binary is provisioned (download cached across runs).
mkdir -p "$OX_BASE"
if [[ ! -x "$BIN" ]]; then
    echo "start-oxigraph: provisioning Oxigraph ${VER}…" >&2
    # Download to a temp path then rename, so an interrupted curl can't leave a
    # partial (or non-executable) binary at the cached path.
    curl -fsSL --retry 3 -o "${BIN}.tmp" "$ASSET_URL"
    chmod +x "${BIN}.tmp"
    mv "${BIN}.tmp" "$BIN"
fi

# The store must have been loaded once by bootstrap-oxigraph-spb.sh. Serving a
# missing/empty store would silently produce a zero-triple A/B; fail instead so
# the workflow skips the leg.
if [[ ! -d "$STORE" ]]; then
    echo "::error::Oxigraph store missing at $STORE — run bootstrap-oxigraph-spb.sh on the runner first" >&2
    exit 1
fi

echo "start-oxigraph: serving $STORE on ${BIND}…" >&2
# `serve` takes an exclusive RocksDB lock; the workflow stop step releases it.
nohup "$BIN" serve --location "$STORE" --bind "$BIND" >/tmp/oxigraph-engine.log 2>&1 &
echo $! > /tmp/oxigraph-engine.pid

for ((i = 1; i <= 180; i++)); do
    if endpoint_ready; then
        echo "start-oxigraph: ready after ${i}s on ${BIND}" >&2
        exit 0
    fi
    sleep 1
done

echo "::error::Oxigraph did not become ready in 180s" >&2
cat /tmp/oxigraph-engine.log >&2 || true
exit 1
