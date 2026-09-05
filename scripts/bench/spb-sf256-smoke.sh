#!/usr/bin/env bash
# HDB-37 stage 5: first editorial-qps numbers on the SF=0.256 corpus.
#
# Runs the nightly's own scenario (editorial agents on) against HornDB and
# GraphDB Free in turn, exactly as nightly.yml does, but with a short run
# period so it fits a bench dispatch. Results go to a scratch trend DB — the
# nightly's cumulative series must not gain off-schedule points.
#
# Knobs: DURATION (default 300), LEGS (default "H G").
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."
OUT="$PWD/bench-out"; mkdir -p "$OUT"
SUM="$OUT/SUMMARY.md"

DIST="${SPB_ASSETS:-/home/bench/src/horndb/crates/harness/data/ldbc-spb/dist}"
WORK="${SPB_WORK:-/home/bench/horndb-bench/spb-sf256}"
DATASET="${DATASET:-$WORK/spb-sf256.nt}"
BIND="${HORNDB_BIND:-127.0.0.1:3842}"
DURATION="${DURATION:-300}"
LEGS="${LEGS:-H G}"
VER="${GRAPHDB_VERSION:-10.8.14}"
GDB_BASE="${GRAPHDB_HOME_BASE:-/home/bench/graphdb}"
HEAP="${GRAPHDB_HEAP:-32g}"
export HARNESS_DB="$OUT/sf256-smoke.sqlite"
export SPB_DURATION_SECONDS="$DURATION"
export SPB_DRIVER_JAR="$DIST/semantic_publishing_benchmark-basic-standard.jar"
export SPB_SCENARIO="$DIST/spb-nightly.properties"

{ echo "# HDB-37 stage 5 — first SF=0.256 editorial + aggregation numbers"
  echo; echo "Host \`$(hostname)\` · $(date -Is) · commit \`$(git rev-parse --short HEAD)\`"
  echo; echo "dataset \`$DATASET\` · run period ${DURATION}s"; } > "$SUM"
sec() { { echo; echo "## $*"; echo '```'; } >> "$SUM"; }
end() { echo '```' >> "$SUM"; }
note() { echo "$*" >> "$SUM"; }

sec "scenario staged into the asset tree"
cp crates/harness/scenarios/spb-nightly.properties "$SPB_SCENARIO"
grep -E '^(aggregationAgents|editorialAgents|datasetSize|creativeWorksPath|queryTimeoutSeconds|warmupPeriodSeconds)=' "$SPB_SCENARIO" >> "$SUM"
ls -la "$DIST/generated-sf256/dataset.info" "$DIST/generated-sf256/query1SubstParameters.txt" >> "$SUM" 2>&1
end

cargo build -p horndb-harness --bin harness --release --features real-engine >"$OUT/build.log" 2>&1 \
  || { note "harness build FAILED"; tail -20 "$OUT/build.log" >> "$SUM"; exit 1; }

if [[ " $LEGS " == *" H "* ]]; then
  sec "HornDB"
  t0=$(date +%s)
  DATA_FILES="$DATASET" RELEASE=1 BIND="$BIND" \
    ./crates/harness/scripts/start-engine.sh > "$OUT/horndb-engine.log" 2>&1 &
  EPID=$!; ready=0
  for i in $(seq 1 3600); do
    curl -fsS "http://$BIND/readyz" >/dev/null 2>&1 && { ready=1; break; }
    kill -0 $EPID 2>/dev/null || break; sleep 2
  done
  note "ready=$ready after $(( $(date +%s) - t0 ))s"
  if [ "$ready" = 1 ]; then
    HORNDB_ENDPOINT="http://$BIND/query" HORNDB_UPDATE_ENDPOINT="http://$BIND/update" \
      ./crates/harness/scripts/run-spb-256.sh > "$OUT/spb-horndb.log" 2>&1
    note "--- spb-run exit=$? ---"
    grep -E 'queries_per_sec|ops_per_sec|editorial|aggregation' "$OUT/spb-horndb.log" | tail -20 >> "$SUM"
    tail -15 "$OUT/spb-horndb.log" >> "$SUM"
  else
    tail -30 "$OUT/horndb-engine.log" >> "$SUM"
  fi
  kill $EPID 2>/dev/null; wait $EPID 2>/dev/null; sleep 10
  end
fi

if [[ " $LEGS " == *" G "* ]]; then
  sec "GraphDB Free"
  GRAPHDB_HEAP="$HEAP" GRAPHDB_HOME_BASE="$GDB_BASE" GRAPHDB_VERSION="$VER" \
    ./crates/harness/scripts/start-graphdb-free.sh >> "$SUM" 2>&1
  if [ $? -eq 0 ] && ./crates/harness/scripts/wait-for-sparql.sh "http://127.0.0.1:7200/repositories/spb" 300 >> "$SUM" 2>&1; then
    ./crates/harness/scripts/run-graphdb-free-spb-256.sh > "$OUT/spb-graphdb.log" 2>&1
    note "--- spb-run exit=$? ---"
    grep -E 'queries_per_sec|ops_per_sec|editorial|aggregation' "$OUT/spb-graphdb.log" | tail -20 >> "$SUM"
    tail -15 "$OUT/spb-graphdb.log" >> "$SUM"
  else
    note "GraphDB did not become ready — leg skipped"
  fi
  pkill -f 'graphdb-[0-9]' 2>/dev/null || true
  end
fi

{ echo; echo "## Recorded metrics"; echo; echo '```'; } >> "$SUM"
sqlite3 "$HARNESS_DB" "SELECT dataset, metric_name, metric_value FROM metrics
  WHERE suite='ldbc-spb-256' AND metric_name IN
    ('editorial-qps','aggregation-qps','duration-s','editorial-total-ops',
     'aggregation-total-queries','aggregation-errors',
     'editorial-insert-count','editorial-update-count','editorial-delete-count',
     'editorial-insert-avg-ms','editorial-update-avg-ms','editorial-delete-avg-ms')
  ORDER BY dataset, metric_name;" >> "$SUM" 2>&1
echo '```' >> "$SUM"
