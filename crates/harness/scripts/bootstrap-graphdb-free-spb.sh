#!/usr/bin/env bash
# Provision GraphDB as the nightly LDBC SPB-256 A/B reference engine.
#
# Why 10.8.x and not 11.x: GraphDB 11+ hard-requires a license even for
# the "free" tier ("No license was set" on any load/query). GraphDB 10.x
# still ships the genuine no-license free tier (limited concurrency, no
# cluster) — enough for a read-only aggregation A/B. 10.8.14 is what the
# nightly was validated against.
#
# The dist zip is pulled from Ontotext's public Maven repo, which serves
# it without the registration form the website download requires.
#
# This is the one-time (heavy) provisioning step: it downloads the pinned
# GraphDB version, (re)creates the `spb` repo, and loads the dataset. It is
# idempotent — re-running re-creates the repo and reloads the dataset.
#
# The nightly does NOT depend on GraphDB staying up afterwards:
# start-graphdb-free.sh brings the same pinned version up per run (downloading
# it if the runner lacks it) and the workflow stops it after the A/B leg, so
# the engine never competes with HornDB for RAM / page cache. No standing
# service / systemd unit is required. Keep GRAPHDB_VERSION in step with the
# nightly workflow's pin so bootstrap and the per-run start agree.
#
# Usage:
#   DATASET=/path/to/spb-256.nt ./bootstrap-graphdb-free-spb.sh
set -euo pipefail

VER="${GRAPHDB_VERSION:-10.8.14}"
GDB_BASE="${GRAPHDB_HOME_BASE:-$HOME/graphdb}"
PORT="${GRAPHDB_PORT:-7200}"
REPO="${GRAPHDB_REPO:-spb}"
# Flat N-Triples closure both engines serve. Same file HornDB's
# start-engine.sh loads, so the A/B compares identical corpora.
DATASET="${DATASET:?set DATASET to the flat .nt closure to load}"
RULESET="${GRAPHDB_RULESET:-empty}"   # empty = no inference; serve the closure as-is
DIST_URL="https://maven.ontotext.com/repository/owlim-releases/com/ontotext/graphdb/graphdb/${VER}/graphdb-${VER}-dist.zip"

test -f "$DATASET" || { echo "error: DATASET not found: $DATASET" >&2; exit 2; }
mkdir -p "$GDB_BASE"
cd "$GDB_BASE"

if [[ ! -d "graphdb-${VER}" ]]; then
    echo "downloading GraphDB ${VER}…"
    curl -fsSL --retry 3 -o "graphdb-${VER}-dist.zip" "$DIST_URL"
    unzip -q "graphdb-${VER}-dist.zip"
fi

echo "starting GraphDB ${VER} (detached) on :${PORT}…"
pkill -f 'graphdb' 2>/dev/null || true
sleep 2
export GDB_JAVA_OPTS="${GDB_JAVA_OPTS:--Xmx8g} -Dgraphdb.home=${GDB_BASE}/home${VER%%.*}"
mkdir -p "${GDB_BASE}/home${VER%%.*}"
nohup "./graphdb-${VER}/bin/graphdb" -d -p "$PORT" > /tmp/graphdb.log 2>&1

for i in $(seq 1 120); do
    if curl -fsS --max-time 5 "http://localhost:${PORT}/rest/repositories" >/dev/null 2>&1; then
        echo "GraphDB up after ${i}s"; break
    fi
    sleep 1
done

cfg="$(mktemp --suffix=.ttl)"
cat > "$cfg" <<TTL
@prefix rep: <http://www.openrdf.org/config/repository#> .
@prefix sr: <http://www.openrdf.org/config/repository/sail#> .
@prefix sail: <http://www.openrdf.org/config/sail#> .
@prefix graphdb: <http://www.ontotext.com/config/graphdb#> .

[] a rep:Repository ;
   rep:repositoryID "${REPO}" ;
   rep:repositoryImpl [
     rep:repositoryType "graphdb:SailRepository" ;
     sr:sailImpl [
       sail:sailType "graphdb:Sail" ;
       graphdb:ruleset "${RULESET}" ;
       graphdb:base-URL "http://www.bbc.co.uk/"
     ]
   ] .
TTL

echo "creating repo '${REPO}' (ruleset=${RULESET})…"
curl -sS -o /dev/null -w "create HTTP %{http_code}\n" \
    -X POST "http://localhost:${PORT}/rest/repositories" -F "config=@${cfg}"
rm -f "$cfg"

echo "clearing + loading ${DATASET} ($(wc -l < "$DATASET") triples)…"
curl -sS -X DELETE "http://localhost:${PORT}/repositories/${REPO}/statements" -w "clear HTTP %{http_code}\n" || true
curl -sS -X POST -H "Content-Type: application/n-triples" -T "$DATASET" \
    "http://localhost:${PORT}/repositories/${REPO}/statements" -w "load HTTP %{http_code}\n"

echo -n "loaded triple count: "
curl -sS -G --data-urlencode 'query=SELECT (COUNT(*) AS ?n) WHERE {?s ?p ?o}' \
    -H 'Accept: text/csv' "http://localhost:${PORT}/repositories/${REPO}"

cat <<EOF

done. GraphDB ${VER} serving repo '${REPO}' at
  query:  http://localhost:${PORT}/repositories/${REPO}
  update: http://localhost:${PORT}/repositories/${REPO}/statements
The nightly A/B leg (run-graphdb-free-spb-256.sh) targets these by default.
EOF
