#!/usr/bin/env bash
#
# get_lubm.sh N DIR — make sure DIR holds a LUBM-N corpus (tbox.nt + abox.nt).
#
# Tried in order:
#   1. the files are already there (persistent bench dir survives checkouts)
#   2. stage-lubm.sh — parallel, and fetches its own converter; needs a JDK
#   3. gen_lubm.sh — serial, and needs a converter already on the host
#   4. the pre-generated tarball published as a `bench-corpora` release asset
#
# Step 4 exists because regenerating LUBM every bench run wastes wall clock,
# whatever tooling the host has. Only lubm-1 and lubm-10 are published as
# release assets today (they were produced by this repo's own gen_lubm.sh —
# UBA1.7, seed 0, index 0), so any larger scale relies on step 2.
#
# Exits non-zero if all four fail, so callers can pick their own fallback.
set -uo pipefail

N="${1:?usage: get_lubm.sh N DIR}"
DIR="${2:?usage: get_lubm.sh N DIR}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BASE="${LUBM_CORPUS_BASE:-https://github.com/sunstoneinstitute/horndb/releases/download/bench-corpora}"

if [ -s "$DIR/tbox.nt" ] && [ -s "$DIR/abox.nt" ]; then
  echo ">> LUBM-$N: reusing $DIR" >&2
  exit 0
fi

mkdir -p "$DIR" || exit 1
# stage-lubm.sh first: it parallelises both phases and brings its own
# RDF/XML -> N-Triples converter, so it works on a host that has a JDK but no
# converter (hornbench) and it is the only path that finishes at LUBM-8000
# scale. gen_lubm.sh stays as the fallback for a host where fetching Jena is
# blocked but a converter is already installed.
if "$SCRIPT_DIR/stage-lubm.sh" "$N" "$DIR"; then
  exit 0
fi
if "$SCRIPT_DIR/gen_lubm.sh" --universities "$N" --out "$DIR"; then
  exit 0
fi

echo ">> LUBM-$N: generator unavailable, fetching $BASE/lubm-$N.tar.gz" >&2
curl -fsSL "$BASE/lubm-$N.tar.gz" | tar xz -C "$DIR" || {
  echo ">> LUBM-$N: download failed" >&2
  exit 1
}
[ -s "$DIR/tbox.nt" ] && [ -s "$DIR/abox.nt" ]
