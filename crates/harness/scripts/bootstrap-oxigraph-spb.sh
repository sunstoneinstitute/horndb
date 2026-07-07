#!/usr/bin/env bash
# Provision Oxigraph as a nightly LDBC SPB-256 A/B reference engine.
#
# Why Oxigraph: it is a Rust SPARQL 1.1 store over RocksDB with no reasoner,
# so — like HornDB and GraphDB Free in this nightly — it serves the flat
# materialized closure `spb-256.nt` as-is (no inference). That makes it the
# closest architectural peer to HornDB in the A/B (Rust + columnar/LSM store,
# no reasoning), and its MIT/Apache-2.0 license carries no RDFox-style
# publication ("DeWitt") restriction, so its numbers can be published freely.
#
# This is the one-time (heavy) provisioning step, sibling to
# bootstrap-graphdb-free-spb.sh: it downloads the pinned Oxigraph release
# binary (cached across runs) and bulk-loads the closure into a persisted
# RocksDB store directory. It is idempotent — re-running wipes and reloads the
# store. The per-run start-oxigraph.sh only re-opens the already-loaded store.
#
# The nightly does NOT depend on Oxigraph staying up afterwards:
# start-oxigraph.sh serves the same persisted store per run and the workflow
# stops it after the A/B leg, so the engine never competes with HornDB for RAM
# / OS page cache. Keep OXIGRAPH_VERSION in step with the nightly workflow's
# pin so bootstrap and the per-run start agree on the binary.
#
# Usage:
#   DATASET=/path/to/spb-256.nt ./bootstrap-oxigraph-spb.sh
set -euo pipefail

VER="${OXIGRAPH_VERSION:-0.5.9}"
OX_BASE="${OXIGRAPH_HOME_BASE:-$HOME/oxigraph}"
# Persisted RocksDB store the per-run server re-opens. Outside the ephemeral
# Actions checkout so it survives `git clean -ffdx` between nightly runs.
STORE="${OXIGRAPH_STORE:-$OX_BASE/spb-store}"
# Flat N-Triples closure both engines serve. Same file HornDB's
# start-engine.sh loads, so the A/B compares identical corpora.
DATASET="${DATASET:?set DATASET to the flat .nt closure to load}"

BIN="${OXIGRAPH_BIN:-$OX_BASE/oxigraph-${VER}}"
# Release asset is a bare binary (no archive): oxigraph_v<VER>_x86_64_linux_gnu.
ASSET_URL="https://github.com/oxigraph/oxigraph/releases/download/v${VER}/oxigraph_v${VER}_x86_64_linux_gnu"

test -f "$DATASET" || { echo "error: DATASET not found: $DATASET" >&2; exit 2; }
mkdir -p "$OX_BASE"

# Ensure the branch-pinned binary is present (download cached across runs).
if [[ ! -x "$BIN" ]]; then
    echo "bootstrap-oxigraph: provisioning Oxigraph ${VER}…" >&2
    # Download to a temp path then rename, so an interrupted curl can't leave a
    # partial (or non-executable) binary at the cached path.
    curl -fsSL --retry 3 -o "${BIN}.tmp" "$ASSET_URL"
    chmod +x "${BIN}.tmp"
    mv "${BIN}.tmp" "$BIN"
fi

# Fresh load: Oxigraph's bulk loader appends, so start from an empty store to
# keep the reload idempotent (re-running must not duplicate triples).
rm -rf "$STORE"
mkdir -p "$STORE"

echo "bootstrap-oxigraph: loading $DATASET into $STORE…" >&2
"$BIN" load --location "$STORE" --file "$DATASET"
echo "bootstrap-oxigraph: load complete." >&2
