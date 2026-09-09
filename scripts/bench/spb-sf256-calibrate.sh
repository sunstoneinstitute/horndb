#!/usr/bin/env bash
# HDB-37 stage 1: validate the SF=0.256 pipeline end-to-end at 1/32 scale and
# measure the rates the full run has to be budgeted from.
#
# The SPB generator is NOT standalone: `generateCreativeWorks` first runs a
# system query against `endpointURL` (ReferenceDataAnalyzer.analyzeEntities), so
# an engine holding the ontologies + reference datasets must be up before it.
# That is why this brings HornDB `serve` up first.
#
# Phases, each timed and each independently reported:
#   A  inventory of the corpus inputs
#   B  serve the ontologies + reference datasets
#   C  generate Creative Works at $CAL_N triples
#   D  materialize the OWL 2 RL closure over ontologies + generated CWs
#
# Knobs: CAL_N (default 8000000).
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."
OUT="$PWD/bench-out"; mkdir -p "$OUT"
SUM="$OUT/SUMMARY.md"

DIST="${SPB_ASSETS:-/home/bench/src/horndb/crates/harness/data/ldbc-spb/dist}"
WORK="${SPB_WORK:-/home/bench/horndb-bench/spb-sf256}"
CAL_N="${CAL_N:-8000000}"
BIND="${HORNDB_BIND:-127.0.0.1:3842}"
GEN="$WORK/generated-cal"
CLOSURE="$WORK/closure-cal.nt"
JAR="$DIST/semantic_publishing_benchmark-basic-standard.jar"
mkdir -p "$WORK"

{ echo "# HDB-37 stage 1 — SF=0.256 pipeline calibration (CAL_N=$CAL_N)"
  echo; echo "Host \`$(hostname)\` · $(date -Is) · commit \`$(git rev-parse --short HEAD)\`"; } > "$SUM"
sec() { { echo; echo "## $*"; echo '```'; } >> "$SUM"; }
end() { echo '```' >> "$SUM"; }

# --- A: inventory -----------------------------------------------------------
sec "A · corpus inventory"
{
  echo "--- previous prep's generation scenario (gen.properties) ---"
  cat "$DIST/gen.properties" 2>&1
  echo "--- aggregation-horndb.properties ---"
  cat "$DIST/aggregation-horndb.properties" 2>&1
  echo "--- shape of an already-generated CW file ---"
  head -2 "$DIST/generated/generatedCreativeWorks-000001.nt" 2>&1
  cat "$DIST/generated/dataset.info" 2>&1
  echo "--- ontologies (ldbc/ excluded, per fetch-ldbc-spb.sh) ---"
  find "$DIST/data/ontologies" -name '*.ttl' -not -path '*/ldbc/*' -printf '%10s  %p\n' 2>&1 | sort -rn
  echo "--- reference datasets ---"
  du -sh "$DIST/data/datasets" 2>&1
  find "$DIST/data/datasets" -type f -printf '%10s  %p\n' 2>&1 | sort -rn | head -20
  echo "--- disk before ---"
  df -h / | tail -1
} >> "$SUM" 2>&1
end

mapfile -t ONTO < <(find "$DIST/data/ontologies" -name '*.ttl' -not -path '*/ldbc/*' | sort)

# --- B: serve ontologies + reference datasets -------------------------------
sec "B · HornDB serve (ontologies + reference datasets)"
cargo build --release -p horndb-sparql --bin serve --features server >> "$SUM" 2>&1 || { echo "serve build FAILED" >> "$SUM"; end; exit 1; }
cargo build --release -p horndb-bench-rdfox >> "$SUM" 2>&1 || { echo "horndb-bench build FAILED" >> "$SUM"; end; exit 1; }

mapfile -t REFDATA < <(find "$DIST/data/datasets" -type f \( -name '*.ttl' -o -name '*.nt' -o -name '*.n3' \) | sort)
echo "serving ${#ONTO[@]} ontology + ${#REFDATA[@]} reference-dataset files" >> "$SUM"
t0=$(date +%s)
./target/release/serve --bind "$BIND" --data "${ONTO[@]}" "${REFDATA[@]}" > "$OUT/serve-cal.log" 2>&1 &
SERVE_PID=$!
ready=0
for i in $(seq 1 1800); do
  curl -fsS "http://$BIND/readyz" >/dev/null 2>&1 && { ready=1; break; }
  kill -0 $SERVE_PID 2>/dev/null || break
  sleep 1
done
t1=$(date +%s)
echo "serve ready=$ready after $((t1-t0))s" >> "$SUM"
tail -20 "$OUT/serve-cal.log" >> "$SUM" 2>&1
if [ "$ready" != 1 ]; then echo "ABORT: serve never became ready" >> "$SUM"; end; kill $SERVE_PID 2>/dev/null; exit 1; fi
curl -sS -G --data-urlencode 'query=SELECT (COUNT(*) AS ?n) WHERE {?s ?p ?o}' \
     -H 'Accept: text/csv' "http://$BIND/query" >> "$SUM" 2>&1
grep VmHWM /proc/$SERVE_PID/status >> "$SUM" 2>&1
end

# --- C: generate Creative Works ---------------------------------------------
sec "C · generate Creative Works (datasetSize=$CAL_N)"
rm -rf "$GEN"; mkdir -p "$GEN"
SCEN="$DIST/spb-gen-cal.properties"
SRC="$DIST/gen.properties"; [ -f "$SRC" ] || SRC="$DIST/spb-nightly.properties"
sed -e "s|^datasetSize=.*|datasetSize=$CAL_N|" \
    -e "s|^creativeWorksPath=.*|creativeWorksPath=$GEN|" \
    -e "s|^endpointURL=.*|endpointURL=http://$BIND/query|" \
    -e "s|^endpointUpdateURL=.*|endpointUpdateURL=http://$BIND/update|" \
    -e "s|^dataGeneratorWorkers=.*|dataGeneratorWorkers=4|" \
    -e "s|^loadOntologies=.*|loadOntologies=false|" \
    -e "s|^loadReferenceDatasets=.*|loadReferenceDatasets=false|" \
    -e "s|^generateCreativeWorks=.*|generateCreativeWorks=true|" \
    -e "s|^loadCreativeWorks=.*|loadCreativeWorks=false|" \
    -e "s|^generateQuerySubstitutionParameters=.*|generateQuerySubstitutionParameters=false|" \
    -e "s|^validateQueryResults=.*|validateQueryResults=false|" \
    -e "s|^warmUp=.*|warmUp=false|" \
    -e "s|^runBenchmark=.*|runBenchmark=false|" \
    -e "s|^checkConformance=.*|checkConformance=false|" \
    "$SRC" > "$SCEN"
echo "--- scenario (from $(basename "$SRC")) ---" >> "$SUM"; grep -v '^#' "$SCEN" | grep . >> "$SUM"
t0=$(date +%s)
( cd "$DIST" && timeout 5400 java -jar "$JAR" "$SCEN" ) > "$OUT/gen-cal.log" 2>&1
GEN_RC=$?; t1=$(date +%s); GEN_S=$((t1-t0))
tail -25 "$OUT/gen-cal.log" >> "$SUM"
GEN_TRIPLES=$(cat "$GEN"/generatedCreativeWorks-*.nt 2>/dev/null | wc -l)
GEN_BYTES=$(du -sb "$GEN" 2>/dev/null | cut -f1)
{ echo "--- exit=$GEN_RC wall=${GEN_S}s ---"
  echo "generated triples: $GEN_TRIPLES   bytes: $GEN_BYTES"
  ls "$GEN" | head -5; echo "files: $(ls "$GEN"/generatedCreativeWorks-*.nt 2>/dev/null | wc -l)"
  head -1 "$GEN"/generatedCreativeWorks-000001.nt 2>&1
  cat "$GEN/dataset.info" 2>&1; } >> "$SUM"
end

kill $SERVE_PID 2>/dev/null; wait $SERVE_PID 2>/dev/null; sleep 3

# --- D: materialize the closure ---------------------------------------------
sec "D · materialize OWL 2 RL closure (ontologies + generated CWs)"
if [ "$GEN_TRIPLES" -lt 1000 ]; then
  echo "SKIPPED: generation produced $GEN_TRIPLES triples" >> "$SUM"; end; exit 1
fi
mapfile -t CWS < <(ls "$GEN"/generatedCreativeWorks-*.nt)
t0=$(date +%s)
/usr/bin/time -v ./target/release/horndb-bench materialize \
  --dump-nt "$CLOSURE" --data "${ONTO[@]}" "${CWS[@]}" > "$OUT/mat-cal.log" 2>"$OUT/mat-cal.time"
MAT_RC=$?; t1=$(date +%s); MAT_S=$((t1-t0))
cat "$OUT/mat-cal.log" >> "$SUM"
grep -E 'Maximum resident|Elapsed \(wall' "$OUT/mat-cal.time" >> "$SUM" 2>&1
PEAK_KB=$(awk '/Maximum resident set size/{print $NF}' "$OUT/mat-cal.time")
CLO_TRIPLES=$(wc -l < "$CLOSURE" 2>/dev/null || echo 0)
CLO_BYTES=$(stat -c %s "$CLOSURE" 2>/dev/null || echo 0)
{ echo "--- exit=$MAT_RC wall=${MAT_S}s ---"
  echo "closure triples: $CLO_TRIPLES  bytes: $CLO_BYTES"; } >> "$SUM"
end

# --- extrapolation ----------------------------------------------------------
TARGET=256000000
{
  echo
  echo "## Extrapolation to SF=0.256 (datasetSize=$TARGET)"
  echo
  echo "| quantity | calibration (N=$CAL_N) | ×$(awk "BEGIN{printf \"%.1f\", $TARGET/$CAL_N}") to target |"
  echo "|---|---|---|"
  awk -v n="$CAL_N" -v t="$TARGET" -v gt="$GEN_TRIPLES" -v gb="$GEN_BYTES" -v gs="$GEN_S" \
      -v ct="$CLO_TRIPLES" -v cb="$CLO_BYTES" -v ms="$MAT_S" -v pk="$PEAK_KB" 'BEGIN{
    f = t/n;
    printf "| generated triples | %d | %.0f |\n", gt, gt*f;
    printf "| generated bytes | %.2f GiB | %.1f GiB |\n", gb/1073741824, gb*f/1073741824;
    printf "| generation wall-clock | %d s | %.0f s (%.1f h) |\n", gs, gs*f, gs*f/3600;
    printf "| closure triples | %d | %.0f |\n", ct, ct*f;
    printf "| closure bytes (.nt) | %.2f GiB | %.1f GiB |\n", cb/1073741824, cb*f/1073741824;
    printf "| materialize wall-clock | %d s | %.0f s (%.1f h) |\n", ms, ms*f, ms*f/3600;
    printf "| materialize peak RSS | %.2f GiB | %.1f GiB |\n", pk/1048576, pk*f/1048576;
    if (gt>0) printf "| closure expansion | %.2fx | — |\n", ct/gt;
    if (ct>0) printf "| serving estimate @170 B/triple | %.2f GiB | %.1f GiB |\n", ct*170/1073741824, ct*f*170/1073741824;
  }'
  echo
  echo "Host has 124 GiB RAM / no swap. Anything in the right column above ~100 GiB does not fit."
  echo
  echo '```'; free -h; df -h /; echo '```'
} >> "$SUM"
