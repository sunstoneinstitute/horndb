#!/usr/bin/env bash
#
# get_lubm.sh N DIR — make sure DIR holds a LUBM-N corpus (tbox.nt + abox.nt).
#
# Tried in order:
#   1. the files are already there (persistent bench dir survives checkouts)
#   2. gen_lubm.sh — needs a JDK for the UBA generator
#   3. the pre-generated tarball published as a `bench-corpora` release asset
#
# Step 3 exists because regenerating LUBM from scratch every bench run wastes
# wall clock, whatever tooling the host has (hornbench now has OpenJDK 21).
# Only lubm-1 and lubm-10 are published as release assets today, so any
# larger scale relies on step 2 succeeding on the host. The assets were
# produced by this repo's own gen_lubm.sh (UBA1.7, seed 0, index 0).
#
# Exits non-zero if all three fail, so callers can pick their own fallback.
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
if "$SCRIPT_DIR/gen_lubm.sh" --universities "$N" --out "$DIR"; then
  exit 0
fi

echo ">> LUBM-$N: generator unavailable, fetching $BASE/lubm-$N.tar.gz" >&2
curl -fsSL "$BASE/lubm-$N.tar.gz" | tar xz -C "$DIR" || {
  echo ">> LUBM-$N: download failed" >&2
  exit 1
}
[ -s "$DIR/tbox.nt" ] && [ -s "$DIR/abox.nt" ]
