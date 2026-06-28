#!/usr/bin/env bash
# Per-run bring-up of the GraphDB Free A/B reference engine for the nightly
# LDBC SPB-256 leg. Mirrors HornDB's start-engine.sh: ensure the
# branch-pinned GraphDB version is present, start it detached, wait until the
# read-only `spb` repo answers, then return. The workflow tears it down after
# the leg (stop step: `pkill -f graphdb`).
#
# Why per-run start/stop and not a standing service: at SF=0.256 an idle
# GraphDB would hold tens of GB of page cache and compete with HornDB for RAM
# and OS file cache during the HornDB measurement leg. Running each engine
# only for its own leg keeps the A/B legs isolated. (Supersedes the earlier
# "wrap in a systemd unit" plan — no standing service / systemctl required.)
#
# The version is pinned by the caller (GRAPHDB_VERSION, set in the nightly
# workflow `env:`) so the binary tracks the git branch under test, not
# whatever happens to be installed on the runner. If that version is not yet
# unpacked under $GRAPHDB_HOME_BASE it is downloaded (cached across runs). The
# heavy one-time dataset load stays in bootstrap-graphdb-free-spb.sh; starting
# here only re-opens the already-persisted `spb` repo.
#
# Environment knobs:
#   GRAPHDB_VERSION    GraphDB dist version to run (default 10.8.14)
#   GRAPHDB_HOME_BASE  base dir holding the dist + home (default $HOME/graphdb)
#   GRAPHDB_PORT       listen port (default 7200)
#   GRAPHDB_REPO       repository id to wait on (default spb)
#   GRAPHDB_HEAP       JVM max heap (default 8g; bump for the 256M scale-up)
set -euo pipefail

VER="${GRAPHDB_VERSION:-10.8.14}"
GDB_BASE="${GRAPHDB_HOME_BASE:-$HOME/graphdb}"
PORT="${GRAPHDB_PORT:-7200}"
REPO="${GRAPHDB_REPO:-spb}"
HEAP="${GRAPHDB_HEAP:-8g}"
DIST_URL="https://maven.ontotext.com/repository/owlim-releases/com/ontotext/graphdb/graphdb/${VER}/graphdb-${VER}-dist.zip"

repo_ready() {
    curl -fsS --max-time 5 \
        -H 'Accept: application/sparql-results+json' \
        -G --data-urlencode 'query=ASK{}' \
        "http://localhost:${PORT}/repositories/${REPO}" >/dev/null 2>&1
}

# Idempotent: a correctly-serving GraphDB left up from a previous run is fine.
if repo_ready; then
    echo "start-graphdb-free: repo '${REPO}' already up on :${PORT}" >&2
    exit 0
fi

# Ensure the branch-pinned version is provisioned (download cached across runs).
mkdir -p "$GDB_BASE"
if [[ ! -x "${GDB_BASE}/graphdb-${VER}/bin/graphdb" ]]; then
    echo "start-graphdb-free: provisioning GraphDB ${VER}…" >&2
    curl -fsSL --retry 3 -o "${GDB_BASE}/graphdb-${VER}-dist.zip" "$DIST_URL"
    unzip -q -o "${GDB_BASE}/graphdb-${VER}-dist.zip" -d "$GDB_BASE"
fi

# Clear any GraphDB server from a different version/run before starting the
# pinned one. Match the versioned dist path (`graphdb-<N>...`), NOT a bare
# `graphdb`: this script's own path contains the substring "graphdb", so on
# Linux (procps) `pkill -f graphdb` matches the running script's command line
# and SIGTERMs itself — the bash exits 143 ("Terminated") before GraphDB ever
# starts, silently skipping the A/B leg. (BSD pkill on macOS spares the caller,
# which is why this never reproduced locally.) The server's JVM always carries
# `-Dgraphdb.dist=.../graphdb-<version>` on its command line, so the digit after
# `graphdb-` matches the server but never `start-graphdb-free.sh`.
pkill -f 'graphdb-[0-9]' 2>/dev/null || true
sleep 2

export GDB_JAVA_OPTS="${GDB_JAVA_OPTS:--Xmx${HEAP}} -Dgraphdb.home=${GDB_BASE}/home${VER%%.*}"
mkdir -p "${GDB_BASE}/home${VER%%.*}"
echo "start-graphdb-free: starting GraphDB ${VER} (detached) on :${PORT} (heap ${HEAP})…" >&2
nohup "${GDB_BASE}/graphdb-${VER}/bin/graphdb" -d -p "$PORT" > /tmp/graphdb-engine.log 2>&1

for i in $(seq 1 180); do
    if repo_ready; then
        echo "start-graphdb-free: repo '${REPO}' ready after ${i}s" >&2
        exit 0
    fi
    sleep 1
done
echo "::error::GraphDB repo '${REPO}' did not become ready in 180s" >&2
cat /tmp/graphdb-engine.log >&2 2>/dev/null || true
exit 1
