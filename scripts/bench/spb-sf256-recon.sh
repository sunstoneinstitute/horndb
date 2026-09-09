#!/usr/bin/env bash
# HDB-37 stage 0: read-only recon of the SPB tree on hornbench, plus a 1M-triple
# generator smoke run so the SF=0.256 generation command is validated (and its
# throughput measured) before the expensive stage 1.
#
# Everything lands in bench-out/SUMMARY.md.
set -uo pipefail
mkdir -p bench-out
OUT=bench-out/SUMMARY.md
SPB_SRC=/home/bench/src/horndb/crates/harness/data/ldbc-spb
DIST="${SPB_ASSETS:-$SPB_SRC/dist}"
PERSIST=/home/bench/horndb-bench

sec()  { { echo; echo "## $*"; echo '```'; } >> "$OUT"; }
end()  { echo '```' >> "$OUT"; }
run()  { echo "\$ $*" >> "$OUT"; "$@" >> "$OUT" 2>&1; }

{ echo "# HDB-37 stage 0 — SPB SF=0.256 recon"; echo; echo "Host \`$(hostname)\` · $(date -Is)"; } > "$OUT"

sec "Host"
run free -h
run df -h / /home
end

sec "SPB dist tree"
run ls -la "$DIST"
run du -sh "$DIST"
end

sec "SPB source tree (top 2 levels)"
run bash -c "find $SPB_SRC -maxdepth 2 -not -path '*/.git/*' | sort | head -60"
end

sec "Scenario .properties files present"
run bash -c "find $SPB_SRC -name '*.properties' -not -path '*/.git/*' -printf '%10s  %p\n' | sort -k2 | head -40"
end

sec "definitions.properties (dist)"
run bash -c "cat $DIST/definitions.properties 2>&1 | head -60"
end

sec "An upstream example scenario (whole file)"
run bash -c "f=\$(find $SPB_SRC -name 'test.properties' -o -name 'sf*.properties' -o -name '*standard*.properties' 2>/dev/null | grep -v '/dist/' | head -1); echo \"file: \$f\"; cat \"\$f\" 2>&1"
end

sec "How the generator reads datasetSize / creativeWorks format"
run bash -c "grep -rn 'DATASET_SIZE\|datasetSize\|GENERATE_CREATIVE_WORKS_FORMAT\|generateCreativeWorksFormat' $SPB_SRC/src 2>/dev/null | head -30"
run bash -c "grep -rn 'n-quads\|n-triples\|nquads\|ntriples' $SPB_SRC/src --include=*.java 2>/dev/null | head -20"
end

sec "Current dataset actually served"
run bash -c "ls -la $DIST/*.nt 2>&1"
run bash -c "for f in $DIST/*.nt; do echo -n \"\$f: \"; wc -l < \"\$f\"; done 2>&1"
run bash -c "head -2 $DIST/spb-256.nt 2>&1"
end

sec "Leftover generated Creative Works from the earlier prep"
run bash -c "ls -la $DIST/generated 2>&1 | head -20"
run bash -c "du -sh $DIST/generated $DIST/data 2>&1"
run bash -c "find $DIST/data -maxdepth 2 -type d 2>&1 | head -30"
end

sec "GraphDB"
run bash -c "ls -la /home/bench/graphdb 2>&1"
run bash -c "du -sh /home/bench/graphdb/home*/data 2>&1"
run bash -c "cat /home/bench/graphdb/graphdb-*/conf/graphdb.properties 2>&1 | grep -v '^#' | grep . | head -20"
run bash -c "grep -rn 'Xmx\|GDB_JAVA_OPTS\|GDB_HEAP' /home/bench/graphdb/graphdb-*/bin/graphdb 2>/dev/null | head -20"
run bash -c "echo GDB_JAVA_OPTS=\${GDB_JAVA_OPTS:-<unset>}"
end

sec "Oxigraph stores"
run bash -c "du -sh /home/bench/oxigraph/* 2>&1 | head"
end

sec "Toolchain"
run bash -c "java -version 2>&1 | head -3; ant -version 2>&1; nproc"
end

# ---------------------------------------------------------------------------
# Generator smoke: 1M triples, so stage 1 has a validated command and a rate.
# ---------------------------------------------------------------------------
SMOKE_N=${SMOKE_N:-1000000}
JAR="$DIST/semantic_publishing_benchmark-basic-standard.jar"
GEN_DIR="$PERSIST/spb-smoke/generated"
SCEN="$DIST/spb-gen-smoke.properties"

sec "Generator smoke — datasetSize=$SMOKE_N"
if [ ! -f "$JAR" ]; then
  echo "driver jar missing at $JAR — smoke skipped" >> "$OUT"
else
  rm -rf "$GEN_DIR"; mkdir -p "$GEN_DIR"
  sed -e "s|^datasetSize=.*|datasetSize=$SMOKE_N|" \
      -e "s|^generateCreativeWorks=.*|generateCreativeWorks=true|" \
      -e "s|^creativeWorksPath=.*|creativeWorksPath=$GEN_DIR|" \
      -e "s|^warmUp=.*|warmUp=false|" \
      -e "s|^runBenchmark=.*|runBenchmark=false|" \
      -e "s|^generateCreativeWorksFormat=.*|generateCreativeWorksFormat=n-triples|" \
      "$DIST/spb-nightly.properties" > "$SCEN" 2>/dev/null \
    || cp "$DIST/spb-nightly.properties" "$SCEN"
  echo "--- scenario ---" >> "$OUT"; grep -v '^#' "$SCEN" | grep . >> "$OUT"
  echo "--- run ---" >> "$OUT"
  t0=$(date +%s)
  ( cd "$DIST" && timeout 900 java -jar "$JAR" "$SCEN" ) >> "$OUT" 2>&1
  rc=$?
  t1=$(date +%s)
  echo "--- exit=$rc  wall=$((t1-t0))s ---" >> "$OUT"
  ls -la "$GEN_DIR" 2>&1 | head -10 >> "$OUT"
  du -sh "$GEN_DIR" 2>&1 >> "$OUT"
  echo -n "generated triples: " >> "$OUT"
  cat "$GEN_DIR"/* 2>/dev/null | wc -l >> "$OUT"
fi
end
