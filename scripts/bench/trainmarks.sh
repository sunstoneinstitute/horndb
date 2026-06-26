#!/usr/bin/env bash
#
# trainmarks.sh — run the DataTreehouse "trainmarks" RDF benchmark against
# HornDB's storage/WCOJ SPARQL backend (`HornBackend`).
#
# Upstream: https://github.com/DataTreehouse/trainmarks — a synthetic
# e-commerce graph (customers / orders / products) at three scales
# (~100K / ~1M / ~10M triples) with six SPARQL queries and Turtle/N-Triples
# I/O timing. No OWL reasoning. trainmarks is a public, permissively-licensed
# benchmark with NO DeWitt-style clause, so unlike the RDFox comparison these
# numbers MAY be committed and published.
#
# This script:
#   1. generates the datasets (vendored generate_data.py, fixed seed 42) if
#      they are not already present,
#   2. builds the `bench-trainmarks` driver (release),
#   3. runs each scale in its own process (bounded memory; per-op timeout),
#   4. writes a single results JSON in the upstream schema.
#
# Usage:
#   scripts/bench/trainmarks.sh                       # all three scales
#   scripts/bench/trainmarks.sh --scales medium,large # subset
#   scripts/bench/trainmarks.sh --timeout 300         # per-op timeout (s)
#
# Output: results JSON under target/trainmarks/results_horndb.json (gitignored
# scratch); copy it into the upstream report tree to render charts.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$REPO_ROOT"

SCALES="medium,large,xlarge"
TIMEOUT=600
WORK="${TRAINMARKS_DIR:-$REPO_ROOT/target/trainmarks}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --scales)  SCALES="$2"; shift 2 ;;
    --timeout) TIMEOUT="$2"; shift 2 ;;
    --work)    WORK="$2"; shift 2 ;;
    -h|--help) sed -n '2,30p' "$0"; exit 0 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

DATA="$WORK/data"
QUERIES="$WORK/queries"
OUT="$WORK/results_horndb.json"

mkdir -p "$WORK"
# Stage the vendored generator + queries into the work dir (generate_data.py
# writes to ./data and ./queries relative to its own cwd).
cp "$SCRIPT_DIR/trainmarks/generate_data.py" "$WORK/generate_data.py"
mkdir -p "$QUERIES"
cp "$SCRIPT_DIR/trainmarks/queries/"*.rq "$QUERIES/"

# 1. generate data (skip if all six files already exist)
need_gen=0
for s in medium large xlarge; do
  [[ -f "$DATA/$s.ttl" && -f "$DATA/$s.nt" ]] || need_gen=1
done
if [[ "$need_gen" == 1 ]]; then
  echo ">> generating trainmarks datasets (seed 42; ~1.7 GB total)" >&2
  ( cd "$WORK" && python3 generate_data.py )
else
  echo ">> datasets present under $DATA — skipping generation" >&2
fi

# 2. build the driver
echo ">> building bench-trainmarks (release)" >&2
cargo build --release -p horndb-bench-trainmarks >/dev/null 2>&1

DRIVER="$REPO_ROOT/target/release/bench-trainmarks"

# 3. run each scale fresh (one process per scale). Reset the results file once.
rm -f "$OUT"
IFS=',' read -ra SCALE_ARR <<< "$SCALES"
for scale in "${SCALE_ARR[@]}"; do
  echo ">> running scale: $scale" >&2
  "$DRIVER" \
    --data-dir "$DATA" \
    --queries-dir "$QUERIES" \
    --scale "$scale" \
    --out "$OUT" \
    --timeout-secs "$TIMEOUT"
done

echo ">> done. results: $OUT" >&2
