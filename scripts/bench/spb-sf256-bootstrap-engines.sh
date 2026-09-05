#!/usr/bin/env bash
# HDB-37 stage 4: make the SF=0.256 closure servable by every nightly leg.
#
# Three independent legs, each reported on its own so one failure does not cost
# the others:
#   P  query substitution parameters — the driver picks its aggregation-query
#      constants by querying a loaded store, and the ones in the asset tree were
#      generated against the old 200 k dataset, so they must be rebuilt.
#   G  GraphDB Free — bulk-loaded offline (`preload`/`importrdf`). The old
#      bootstrap POSTs the file over HTTP, which is hours at this scale.
#   O  Oxigraph — both persisted stores, via the existing bootstrap script.
#
# Knobs: DATASET, LEGS (default "P G O"), GRAPHDB_HEAP.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."
OUT="$PWD/bench-out"; mkdir -p "$OUT"
SUM="$OUT/SUMMARY.md"

DIST="${SPB_ASSETS:-/home/bench/src/horndb/crates/harness/data/ldbc-spb/dist}"
WORK="${SPB_WORK:-/home/bench/horndb-bench/spb-sf256}"
DATASET="${DATASET:-$WORK/spb-sf256.nt}"
GEN="${GEN_DIR:-$WORK/generated-sf256}"
BIND="${HORNDB_BIND:-127.0.0.1:3842}"
JAR="$DIST/semantic_publishing_benchmark-basic-standard.jar"
LEGS="${LEGS:-P G O}"
VER="${GRAPHDB_VERSION:-10.8.14}"
GDB_BASE="${GRAPHDB_HOME_BASE:-/home/bench/graphdb}"
GDB_HOME="$GDB_BASE/home${VER%%.*}"
HEAP="${GRAPHDB_HEAP:-32g}"
PORT="${GRAPHDB_PORT:-7200}"
REPO="${GRAPHDB_REPO:-spb}"

{ echo "# HDB-37 stage 4 — load the SF=0.256 closure into every A/B engine"
  echo; echo "Host \`$(hostname)\` · $(date -Is) · commit \`$(git rev-parse --short HEAD)\`"
  echo; echo "DATASET=\`$DATASET\` LEGS=\`$LEGS\`"; } > "$SUM"
sec() { { echo; echo "## $*"; echo '```'; } >> "$SUM"; }
end() { echo '```' >> "$SUM"; }
note() { echo "$*" >> "$SUM"; }

sec "preconditions"
if [ ! -s "$DATASET" ]; then note "ABORT: no dataset at $DATASET"; end; exit 1; fi
note "dataset: $(du -h "$DATASET" | cut -f1), $(wc -l < "$DATASET") lines"
free -h >> "$SUM"; df -h / | tail -1 >> "$SUM"
end

# --- P: query substitution parameters ---------------------------------------
if [[ " $LEGS " == *" P "* ]]; then
  sec "P · regenerate query substitution parameters"
  cargo build --release -p horndb-sparql --bin serve --features server >"$OUT/build.log" 2>&1 \
    || { note "BUILD FAILED"; tail -20 "$OUT/build.log" >> "$SUM"; end; exit 1; }
  t0=$(date +%s)
  ./target/release/serve --bind "$BIND" --data "$DATASET" > "$OUT/serve-subst.log" 2>&1 &
  SPID=$!; ready=0
  for i in $(seq 1 5400); do
    curl -fsS "http://$BIND/readyz" >/dev/null 2>&1 && { ready=1; break; }
    kill -0 $SPID 2>/dev/null || break; sleep 2
  done
  note "HornDB ready=$ready after $(( $(date +%s) - t0 ))s"
  if [ "$ready" = 1 ]; then
    SCEN="$DIST/spb-subst.properties"
    sed -e "s|^creativeWorksPath=.*|creativeWorksPath=$GEN|" \
        -e "s|^endpointURL=.*|endpointURL=http://$BIND/query|" \
        -e "s|^endpointUpdateURL=.*|endpointUpdateURL=http://$BIND/update|" \
        -e "s|^generateCreativeWorks=.*|generateCreativeWorks=false|" \
        -e "s|^generateQuerySubstitutionParameters=.*|generateQuerySubstitutionParameters=true|" \
        -e "s|^querySubstitutionParameters=.*|querySubstitutionParameters=20000|" \
        "$DIST/gen.properties" > "$SCEN"
    t0=$(date +%s)
    ( cd "$DIST" && timeout 5400 java -Xmx8g -jar "$JAR" "$SCEN" ) > "$OUT/subst.log" 2>&1
    note "--- exit=$? wall=$(( $(date +%s) - t0 ))s ---"
    tail -8 "$OUT/subst.log" >> "$SUM"
    ls -la "$GEN"/query*SubstParameters.txt 2>&1 | head -15 >> "$SUM"
  else
    tail -20 "$OUT/serve-subst.log" >> "$SUM"
  fi
  kill $SPID 2>/dev/null; wait $SPID 2>/dev/null; sleep 5
  end
fi

# --- G: GraphDB Free --------------------------------------------------------
if [[ " $LEGS " == *" G "* ]]; then
  sec "G · GraphDB Free bulk load (heap $HEAP)"
  pkill -f 'graphdb-[0-9]' 2>/dev/null || true; sleep 5
  mkdir -p "$GDB_HOME"
  cfg="$WORK/graphdb-spb-repo.ttl"
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
       graphdb:ruleset "empty" ;
       graphdb:base-URL "http://www.bbc.co.uk/"
     ]
   ] .
TTL
  note "bulk loaders available:"; ls "$GDB_BASE/graphdb-$VER/bin" >> "$SUM" 2>&1
  export GDB_JAVA_OPTS="-Xmx$HEAP -Dgraphdb.home=$GDB_HOME"
  LOADED=0
  for loader in preload importrdf; do
    bin="$GDB_BASE/graphdb-$VER/bin/$loader"
    [ -x "$bin" ] || continue
    note "--- $loader --help ---"; "$bin" --help >> "$SUM" 2>&1
    note "--- running $loader ---"
    t0=$(date +%s)
    if [ "$loader" = preload ]; then
      "$bin" -f -c "$cfg" "$DATASET" > "$OUT/graphdb-load.log" 2>&1
    else
      "$bin" load -f -c "$cfg" -m parallel "$DATASET" > "$OUT/graphdb-load.log" 2>&1
    fi
    rc=$?; note "--- $loader exit=$rc wall=$(( $(date +%s) - t0 ))s ---"
    tail -20 "$OUT/graphdb-load.log" >> "$SUM"
    [ $rc -eq 0 ] && { LOADED=1; break; }
  done
  if [ "$LOADED" = 1 ]; then
    note "starting GraphDB to verify…"
    nohup "$GDB_BASE/graphdb-$VER/bin/graphdb" -d -p "$PORT" > /tmp/graphdb-verify.log 2>&1
    up=0
    for i in $(seq 1 600); do
      curl -fsS --max-time 5 -G --data-urlencode 'query=ASK{}' \
        -H 'Accept: application/sparql-results+json' \
        "http://localhost:$PORT/repositories/$REPO" >/dev/null 2>&1 && { up=1; break; }
      sleep 2
    done
    note "repo '$REPO' up=$up"
    if [ "$up" = 1 ]; then
      note "triple count:"
      curl -sS --max-time 900 -G --data-urlencode 'query=SELECT (COUNT(*) AS ?n) WHERE {?s ?p ?o}' \
        -H 'Accept: text/csv' "http://localhost:$PORT/repositories/$REPO" >> "$SUM" 2>&1
    fi
    pkill -f 'graphdb-[0-9]' 2>/dev/null || true; sleep 5
  else
    note "GraphDB bulk load FAILED — the A/B leg cannot serve this dataset"
  fi
  du -sh "$GDB_HOME/data" >> "$SUM" 2>&1
  end
fi

# --- O: Oxigraph ------------------------------------------------------------
if [[ " $LEGS " == *" O "* ]]; then
  sec "O · Oxigraph persisted stores"
  t0=$(date +%s)
  DATASET="$DATASET" ./crates/harness/scripts/bootstrap-oxigraph-spb.sh > "$OUT/oxigraph.log" 2>&1
  note "--- exit=$? wall=$(( $(date +%s) - t0 ))s ---"
  tail -10 "$OUT/oxigraph.log" >> "$SUM"
  du -sh /home/bench/oxigraph/spb-store /home/bench/oxigraph/spb-store-optimized >> "$SUM" 2>&1
  end
fi

{ echo; echo '```'; free -h; df -h / | tail -1; echo '```'; } >> "$SUM"
