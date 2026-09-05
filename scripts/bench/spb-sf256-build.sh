#!/usr/bin/env bash
# HDB-37 stage 3: build the true SF=0.256 SPB closure on hornbench.
#
# Why chunked: stage 2 measured the OWL 2 RL materialize peak at ~3.8 KB per
# asserted triple (28 GiB for 8 M), so one process over 256 M would need ~900 GiB
# on a 124 GiB host. Serving the result is cheap by comparison — 136 B per
# closure triple, so the 533 M-triple closure fits in ~72 GiB.
#
# Creative Works are closed in slices. That is sound here because the SPB
# ontologies are the only shared premises: CW subjects are disjoint per slice and
# the reference datasets are not part of the closure input, so no OWL 2 RL rule
# joins two CWs. Phase V checks exactly that against the stage-2 whole-set count
# before the full run starts, and aborts if it does not hold.
#
# Slices repeat the (small) ontology closure. Those repeats are dropped by the
# store on load — RDF is a set — so the concatenated file is deliberately not
# deduplicated; the served triple count is reported alongside the line count.
#
# Phases: V verify chunking · G generate · M materialize slices · S serve check.
# Knobs: TARGET_N (default 256000000), CHUNK_FILES (default 178 ~ 8 M triples).
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."
OUT="$PWD/bench-out"; mkdir -p "$OUT"
SUM="$OUT/SUMMARY.md"

DIST="${SPB_ASSETS:-/home/bench/src/horndb/crates/harness/data/ldbc-spb/dist}"
WORK="${SPB_WORK:-/home/bench/horndb-bench/spb-sf256}"
TARGET_N="${TARGET_N:-256000000}"
CHUNK_FILES="${CHUNK_FILES:-178}"
BIND="${HORNDB_BIND:-127.0.0.1:3842}"
JAR="$DIST/semantic_publishing_benchmark-basic-standard.jar"
GEN="$WORK/generated-sf256"
CHUNKDIR="$WORK/chunks"
FINAL="$WORK/spb-sf256.nt"
mkdir -p "$WORK"

{ echo "# HDB-37 stage 3 — build the SF=0.256 closure (datasetSize=$TARGET_N)"
  echo; echo "Host \`$(hostname)\` · $(date -Is) · commit \`$(git rev-parse --short HEAD)\`"; } > "$SUM"
sec() { { echo; echo "## $*"; echo '```'; } >> "$SUM"; }
end() { echo '```' >> "$SUM"; }
note() { echo "$*" >> "$SUM"; }

mapfile -t ONTO < <(find "$DIST/data/ontologies" -name '*.ttl' -not -path '*/ldbc/*' | sort)

sec "build"
cargo build --release -p horndb-sparql --bin serve --features server >"$OUT/build.log" 2>&1 || { note "BUILD FAILED"; tail -20 "$OUT/build.log" >> "$SUM"; end; exit 1; }
cargo build --release -p horndb-bench-rdfox >>"$OUT/build.log" 2>&1 || { note "BUILD FAILED"; tail -20 "$OUT/build.log" >> "$SUM"; end; exit 1; }
note ok; end

# --- V: is slicing sound? ---------------------------------------------------
# Stage 2 closed the whole 8 M calibration set in one process: 16,654,450 lines.
# Close the same set as two halves; the only legitimate difference is the
# ontology closure appearing twice.
sec "V · slice-soundness check on the 8 M calibration set"
CAL="$WORK/generated-cal"
mapfile -t CALF < <(ls "$CAL"/generatedCreativeWorks-*.nt 2>/dev/null)
if [ "${#CALF[@]}" -lt 4 ]; then
  note "SKIPPED — calibration Creative Works missing under $CAL"; end
else
  half=$(( ${#CALF[@]} / 2 ))
  ./target/release/horndb-bench materialize --dump-nt "$WORK/v-a.nt" --data "${ONTO[@]}" "${CALF[@]:0:$half}" >>"$OUT/verify.log" 2>&1
  ./target/release/horndb-bench materialize --dump-nt "$WORK/v-b.nt" --data "${ONTO[@]}" "${CALF[@]:$half}"    >>"$OUT/verify.log" 2>&1
  ./target/release/horndb-bench materialize --dump-nt "$WORK/v-o.nt" --data "${ONTO[@]}"                       >>"$OUT/verify.log" 2>&1
  A=$(wc -l < "$WORK/v-a.nt"); B=$(wc -l < "$WORK/v-b.nt"); O=$(wc -l < "$WORK/v-o.nt")
  WHOLE=16654450
  note "half A: $A   half B: $B   ontology-only: $O"
  note "A + B - ontology = $((A + B - O))   whole-set (stage 2): $WHOLE   delta: $((A + B - O - WHOLE))"
  rm -f "$WORK/v-a.nt" "$WORK/v-b.nt"
  if [ "$((A + B - O))" -ne "$WHOLE" ]; then
    note "ABORT: slicing changes the closure — a rule joins across Creative Works."; end; exit 1
  fi
  note "slicing is sound."
  end
fi

# --- G: generate ------------------------------------------------------------
sec "G · generate Creative Works (datasetSize=$TARGET_N)"
mapfile -t REFDATA < <(find "$DIST/data/datasets" -type f \( -name '*.ttl' -o -name '*.nt' \) | sort)
t0=$(date +%s)
./target/release/serve --bind "$BIND" --data "${ONTO[@]}" "${REFDATA[@]}" > "$OUT/serve-gen.log" 2>&1 &
SPID=$!; ready=0
for i in $(seq 1 1800); do
  curl -fsS "http://$BIND/readyz" >/dev/null 2>&1 && { ready=1; break; }
  kill -0 $SPID 2>/dev/null || break; sleep 1
done
note "reference endpoint ready=$ready after $(( $(date +%s) - t0 ))s"
if [ "$ready" != 1 ]; then note "ABORT: generation endpoint never came up"; tail -20 "$OUT/serve-gen.log" >> "$SUM"; end; exit 1; fi

rm -rf "$GEN"; mkdir -p "$GEN"
SCEN="$DIST/spb-gen-sf256.properties"
sed -e "s|^datasetSize=.*|datasetSize=$TARGET_N|" \
    -e "s|^creativeWorksPath=.*|creativeWorksPath=$GEN|" \
    -e "s|^endpointURL=.*|endpointURL=http://$BIND/query|" \
    -e "s|^endpointUpdateURL=.*|endpointUpdateURL=http://$BIND/update|" \
    -e "s|^dataGeneratorWorkers=.*|dataGeneratorWorkers=8|" \
    "$DIST/gen.properties" > "$SCEN"
t0=$(date +%s)
( cd "$DIST" && timeout 7200 java -Xmx8g -jar "$JAR" "$SCEN" ) > "$OUT/gen.log" 2>&1
GEN_RC=$?; GEN_S=$(( $(date +%s) - t0 ))
kill $SPID 2>/dev/null; wait $SPID 2>/dev/null; sleep 3
GEN_TRIPLES=$(cat "$GEN"/generatedCreativeWorks-*.nt 2>/dev/null | wc -l)
GEN_FILES=$(ls "$GEN"/generatedCreativeWorks-*.nt 2>/dev/null | wc -l)
tail -6 "$OUT/gen.log" >> "$SUM"
note "--- exit=$GEN_RC wall=${GEN_S}s ---"
note "generated: $GEN_TRIPLES triples in $GEN_FILES files, $(du -sh "$GEN" | cut -f1)"
note "$(cat "$GEN/dataset.info" 2>/dev/null | head -2 | tr '\n' ' ')"
end
[ "$GEN_TRIPLES" -lt 1000000 ] && { note "ABORT: generation produced too little"; exit 1; }

# --- M: materialize in slices ----------------------------------------------
sec "M · materialize the closure in slices of $CHUNK_FILES files"
rm -rf "$CHUNKDIR"; mkdir -p "$CHUNKDIR"; : > "$FINAL"
mapfile -t ALL < <(ls "$GEN"/generatedCreativeWorks-*.nt)
NCHUNK=$(( (${#ALL[@]} + CHUNK_FILES - 1) / CHUNK_FILES ))
note "slices: $NCHUNK"
MT0=$(date +%s); MAXPEAK=0; FAILED=0
for ((c = 0; c < NCHUNK; c++)); do
  slice=("${ALL[@]:$((c * CHUNK_FILES)):$CHUNK_FILES}")
  outf="$CHUNKDIR/chunk-$c.nt"
  ct0=$(date +%s)
  ./target/release/horndb-bench materialize --dump-nt "$outf" --data "${ONTO[@]}" "${slice[@]}" >> "$OUT/materialize.log" 2>&1 &
  mpid=$!; hwm=0
  while kill -0 $mpid 2>/dev/null; do
    cur=$(awk '/VmHWM/{print $2}' /proc/$mpid/status 2>/dev/null)
    [ -n "${cur:-}" ] && [ "$cur" -gt "$hwm" ] && hwm=$cur
    sleep 2
  done
  wait $mpid; rc=$?
  [ "$hwm" -gt "$MAXPEAK" ] && MAXPEAK=$hwm
  if [ $rc -ne 0 ] || [ ! -s "$outf" ]; then note "slice $c FAILED (rc=$rc)"; FAILED=1; break; fi
  cat "$outf" >> "$FINAL"; rm -f "$outf"
  [ $((c % 4)) -eq 0 ] && note "slice $c/$NCHUNK  $(( $(date +%s) - ct0 ))s  peak $((hwm/1024))MiB  total $(du -h "$FINAL" | cut -f1)  free $(awk '/MemAvailable/{printf "%.0fGi", $2/1048576}' /proc/meminfo)"
done
MAT_S=$(( $(date +%s) - MT0 ))
CLO_LINES=$(wc -l < "$FINAL")
note "--- slices done: wall=${MAT_S}s max peak RSS=$((MAXPEAK/1024))MiB failed=$FAILED ---"
note "closure file: $CLO_LINES lines, $(du -h "$FINAL" | cut -f1)"
rmdir "$CHUNKDIR" 2>/dev/null
df -h / | tail -1 >> "$SUM"
end
[ "$FAILED" = 1 ] && { note "ABORT: a slice failed; $FINAL is incomplete"; exit 1; }

# --- S: serve the result ----------------------------------------------------
sec "S · HornDB serves the SF=0.256 closure"
t0=$(date +%s)
./target/release/serve --bind "$BIND" --data "$FINAL" > "$OUT/serve-final.log" 2>&1 &
SPID=$!; ready=0; SHWM=0
for i in $(seq 1 5400); do
  cur=$(awk '/VmHWM/{print $2}' /proc/$SPID/status 2>/dev/null)
  [ -n "${cur:-}" ] && [ "$cur" -gt "$SHWM" ] && SHWM=$cur
  curl -fsS "http://$BIND/readyz" >/dev/null 2>&1 && { ready=1; break; }
  kill -0 $SPID 2>/dev/null || break; sleep 1
done
LOAD_S=$(( $(date +%s) - t0 ))
note "ready=$ready after ${LOAD_S}s   peak RSS $((SHWM/1024)) MiB"
if [ "$ready" = 1 ]; then
  note "distinct triples in store:"
  curl -sS -G --data-urlencode 'query=SELECT (COUNT(*) AS ?n) WHERE {?s ?p ?o}' \
       -H 'Accept: text/csv' "http://$BIND/query" >> "$SUM" 2>&1
  note "sample aggregation-shaped query:"
  ( time curl -sS -G --data-urlencode 'query=SELECT ?t (COUNT(*) AS ?n) WHERE {?s a ?t} GROUP BY ?t ORDER BY DESC(?n) LIMIT 5' \
       -H 'Accept: text/csv' "http://$BIND/query" ) >> "$SUM" 2>&1
fi
tail -5 "$OUT/serve-final.log" >> "$SUM"
kill $SPID 2>/dev/null; wait $SPID 2>/dev/null
end

{ echo; echo "## Result"; echo
  echo "| item | value |"; echo "|---|---|"
  echo "| generated (asserted CW) triples | $GEN_TRIPLES |"
  echo "| closure file | \`$FINAL\` |"
  echo "| closure lines | $CLO_LINES |"
  echo "| closure size | $(du -h "$FINAL" | cut -f1) |"
  echo "| generation wall-clock | ${GEN_S}s |"
  echo "| materialize wall-clock (${NCHUNK} slices) | ${MAT_S}s |"
  echo "| materialize max peak RSS | $((MAXPEAK/1024)) MiB |"
  echo "| serve load wall-clock | ${LOAD_S}s |"
  echo "| serve peak RSS | $((SHWM/1024)) MiB |"
  echo; echo '```'; free -h; df -h / | tail -1; echo '```'; } >> "$SUM"
