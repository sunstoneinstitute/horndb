#!/usr/bin/env bash
#
# Baseline x86-64 vs `-C target-cpu=<level>`: does letting rustc emit newer
# instructions (AVX2, BMI2, POPCNT, ...) move HornDB's hot paths?
#
# Why this exists: every release build today — the nightly included — targets
# baseline x86-64 (SSE2 only). The repo sets no `target-cpu`, and
# `actions-rust-lang/setup-rust-toolchain` exports `RUSTFLAGS="-D warnings"`,
# which overrides any `build.rustflags` a runner-side config might set. The
# hand-written kernels in `horndb-simd` are unaffected (they pick AVX2/AVX-512
# at runtime); everything the compiler vectorises or schedules on its own is.
#
# Driven on hornbench by `.github/workflows/bench.yml`:
#
#   gh workflow run bench.yml --ref <branch> -f script=scripts/bench/target-cpu-ab.sh
#
# Each variant builds into its own target dir with its own RUSTFLAGS, so the
# binaries cannot mix. Knobs (via the workflow's `env` input):
#
#   VARIANTS  target-cpu levels to compare (default "baseline x86-64-v3 x86-64-v4";
#             "baseline" means no -C target-cpu at all)
#   LEGS      "load spb" (default both)
#   REPS      store_load repetitions per variant (default 3, interleaved across
#             variants so host drift spreads over all of them)
#   THREADS   store_load parse threads (default 8 = the shipped `auto` on hornbench)
#
# Legs:
#   load  `store_load` (a real `Store` bulk load plus first read) on trainmarks
#         xlarge, N-Triples and Turtle. Median wall of REPS.
#   spb   LDBC SPB with nightly.yml's bring-up (same dataset, scenario and
#         memory ceiling), one run per variant. Writes to a scratch HARNESS_DB,
#         never the nightly's.
#
# Output: bench-out/<leg>-<variant>*.log and bench-out/SUMMARY.md.
#
# Deliberately NOT `set -e`: one failed variant must not cost the others.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

OUT="$REPO_ROOT/bench-out"
mkdir -p "$OUT"
SUMMARY="$OUT/SUMMARY.md"

VARIANTS="${VARIANTS:-baseline x86-64-v3 x86-64-v4}"
LEGS="${LEGS:-load spb}"
REPS="${REPS:-3}"
THREADS="${THREADS:-8}"

PERSIST="${BENCH_PERSIST:-/home/bench/horndb-bench}"
mkdir -p "$PERSIST" 2>/dev/null || PERSIST="$REPO_ROOT/target/bench-persist"
mkdir -p "$PERSIST"

# Scratch trend DB: run-spb-256.sh labels its rows `horndb`, which must not land
# in the nightly's cumulative series.
export HARNESS_DB="$OUT/target-cpu-harness.sqlite"
SPB_ASSETS="${SPB_ASSETS:-/home/bench/src/horndb/crates/harness/data/ldbc-spb/dist}"
HORNDB_BIND="${HORNDB_BIND:-127.0.0.1:3841}"
# Match nightly.yml: its dataset (the true-scale corpus since HDB-37, not the
# older spb-256.nt stand-in) and its server memory ceiling.
SPB_DATASET="${SPB_DATASET:-$SPB_ASSETS/spb-sf128.nt}"
MEMORY_MAX="${MEMORY_MAX:-90G}"

note() { echo "$*" >&2; }
want() { case " $LEGS " in *" $1 "*) return 0 ;; esac; return 1; }

# Per-variant build environment. The target dir sits on the runner's persistent
# disk (the checkout is wiped between runs); sccache keys on RUSTFLAGS, so
# variants never share objects and a re-run reuses its own.
variant_env() {
  local v="$1"
  export CARGO_TARGET_DIR="$PERSIST/target-cpu-ab/$v"
  if [ "$v" = baseline ]; then
    export RUSTFLAGS=""
  else
    export RUSTFLAGS="-C target-cpu=$v"
  fi
}

median() { sort -g | awk '{a[NR]=$1} END{if(NR) print a[int((NR+1)/2)]}'; }

{
  echo "# target-cpu A/B"
  echo
  echo "- commit: \`$(git rev-parse --short HEAD)\`"
  echo "- date (UTC): $(date -u +%Y-%m-%d)"
  echo "- host: \`$(hostname)\` — $(nproc) cores, $(uname -sr)"
  echo "- cpu: $(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2- | sed 's/^ //')"
  echo "- variants: \`$VARIANTS\`; legs: \`$LEGS\`; store_load reps: $REPS, threads: $THREADS"
  echo
} > "$SUMMARY"

# Skip any variant the host cannot execute (e.g. x86-64-v4 without AVX-512):
# the binary would die with SIGILL, not measure anything.
supported() {
  local flags; flags=" $(grep -m1 '^flags' /proc/cpuinfo | cut -d: -f2) "
  local need=()
  case "$1" in
    baseline) return 0 ;;
    x86-64-v2) need=(sse4_2 popcnt ssse3) ;;
    x86-64-v3) need=(sse4_2 popcnt avx2 bmi2 fma movbe) ;;
    x86-64-v4) need=(sse4_2 popcnt avx2 bmi2 fma avx512f avx512bw avx512dq avx512vl) ;;
    native) return 0 ;;
    *) return 0 ;;  # a named CPU (znver4, ...) — trust the caller
  esac
  local f
  for f in "${need[@]}"; do
    case "$flags" in *" $f "*) ;; *) note ">> $1 needs $f, host lacks it"; return 1 ;; esac
  done
}

RUN_VARIANTS=()
for v in $VARIANTS; do
  if supported "$v"; then RUN_VARIANTS+=("$v"); else echo "- **skipped \`$v\`**: host CPU lacks it" >> "$SUMMARY"; fi
done

# --------------------------------------------------------------------------
# Builds, up front: a build failure shows as one summary line, and build time
# does not sit between the timed reps.
# --------------------------------------------------------------------------
declare -A BUILT
# The SPB driver (`harness`) is the measuring tool, not the thing measured, so
# it is built once at baseline and shared by every variant. It also cannot be
# built with target-cpu=x86-64-v3/v4: oxrocksdb-sys (via oxigraph) turns on
# RocksDB's AVX2 CRC path whenever avx2 is on, and that path needs PCLMUL,
# which neither level includes -- the C++ build fails.
HARNESS_OK=0
if want spb; then
  variant_env baseline
  note "== build harness (baseline, shared by all variants)"
  if cargo build --release -p horndb-harness --bin harness --features real-engine > "$OUT/build-harness.log" 2>&1; then
    HARNESS_OK=1
  else
    echo "- **harness build failed** (see build-harness.log); spb leg skipped" >> "$SUMMARY"
    tail -40 "$OUT/build-harness.log" >&2
  fi
fi
for v in "${RUN_VARIANTS[@]}"; do
  variant_env "$v"
  note "== build $v (RUSTFLAGS='$RUSTFLAGS', target $CARGO_TARGET_DIR)"
  ok=1
  if want load; then
    cargo build --release -p horndb-bench-trainmarks --bin store_load > "$OUT/build-$v.log" 2>&1 || ok=0
  fi
  if want spb && [ "$ok" = 1 ]; then
    cargo build --release -p horndb-sparql --bin serve --features server >> "$OUT/build-$v.log" 2>&1 || ok=0
  fi
  if [ "$ok" = 1 ]; then
    BUILT[$v]=1
  else
    echo "- **build failed for \`$v\`** (see build-$v.log)" >> "$SUMMARY"
    # Also into the job log: the artifact is not always reachable, the log is.
    note ">> build failed for $v; last lines of build-$v.log:"
    grep -E '^(error|warning)|^\s+-->' "$OUT/build-$v.log" | head -40 >&2
    tail -40 "$OUT/build-$v.log" >&2
  fi
done
unset RUSTFLAGS CARGO_TARGET_DIR

# --------------------------------------------------------------------------
# load: store_load on trainmarks xlarge, reps interleaved across variants.
# --------------------------------------------------------------------------
leg_load() {
  local work="$PERSIST/trainmarks"
  local data="$work/data"
  if [ ! -f "$data/xlarge.nt" ] || [ ! -f "$data/xlarge.ttl" ]; then
    note ">> generating trainmarks datasets (seed 42; ~1.7 GB total)"
    mkdir -p "$work"
    cp scripts/bench/trainmarks/generate_data.py "$work/generate_data.py"
    ( cd "$work" && python3 generate_data.py ) || return 1
  fi

  local fmt rep v bin log
  for fmt in nt ttl; do
    for rep in $(seq 1 "$REPS"); do
      for v in "${RUN_VARIANTS[@]}"; do
        [ -n "${BUILT[$v]:-}" ] || continue
        bin="$PERSIST/target-cpu-ab/$v/release/store_load"
        log="$OUT/load-$v-$fmt.log"
        note "== load $fmt rep $rep variant $v"
        echo "----- rep $rep -----" >> "$log"
        "$bin" --file "$data/xlarge.$fmt" --threads "$THREADS" >> "$log" 2>&1 \
          || echo "FAILED rep $rep" >> "$log"
      done
    done
  done

  {
    echo "## load — store_load, trainmarks xlarge, $THREADS threads (median of $REPS)"
    echo
    echo "| corpus | variant | wall (s) | vs baseline | peak RSS (MiB) |"
    echo "|---|---|---|---|---|"
  } >> "$SUMMARY"
  for fmt in nt ttl; do
    local base=""
    for v in "${RUN_VARIANTS[@]}"; do
      log="$OUT/load-$v-$fmt.log"
      [ -f "$log" ] || continue
      local wall rss delta="—"
      wall=$(grep -o '\[load\] wall [0-9.]*' "$log" | awk '{print $3}' | median)
      rss=$(grep -o 'peak RSS [0-9]*' "$log" | awk '{print $3}' | median)
      [ -n "$wall" ] || { echo "| xlarge.$fmt | $v | FAILED | | |" >> "$SUMMARY"; continue; }
      if [ "$v" = baseline ]; then
        base="$wall"
      elif [ -n "$base" ]; then
        delta=$(awk -v b="$base" -v w="$wall" 'BEGIN{printf "%+.1f%%", 100*(w-b)/b}')
      fi
      echo "| xlarge.$fmt | $v | $wall | $delta | $rss |" >> "$SUMMARY"
    done
  done
  echo >> "$SUMMARY"
}

# --------------------------------------------------------------------------
# spb: SPB per variant, same bring-up as nightly.yml.
# --------------------------------------------------------------------------
wait_for_ready() {
  local url="$1" timeout="$2" t=0
  while [ "$t" -lt "$timeout" ]; do
    [ "$(curl -s -o /dev/null -w '%{http_code}' "$url")" = 200 ] && return 0
    sleep 5; t=$((t + 5))
  done
  return 1
}

leg_spb() {
  [ "$HARNESS_OK" = 1 ] || return 1
  local dataset="$SPB_DATASET"
  local jar="$SPB_ASSETS/semantic_publishing_benchmark-basic-standard.jar"
  if [ ! -f "$dataset" ] || [ ! -f "$jar" ]; then
    note ">> SPB assets missing under $SPB_ASSETS — skipping"
    echo "- spb: **skipped**, assets missing under \`$SPB_ASSETS\`" >> "$SUMMARY"
    return 1
  fi
  cp crates/harness/scenarios/spb-nightly.properties "$SPB_ASSETS/spb-nightly.properties"

  local v log pid
  for v in "${RUN_VARIANTS[@]}"; do
    [ -n "${BUILT[$v]:-}" ] || continue
    variant_env "$v"
    log="$OUT/spb-$v.log"
    note "== spb variant $v"
    DATA_FILES="$dataset" RELEASE=1 BIND="$HORNDB_BIND" MEMORY_MAX="$MEMORY_MAX" \
      ./crates/harness/scripts/start-engine.sh > "$OUT/spb-engine-$v.log" 2>&1 &
    pid=$!
    # /readyz, not /query: serve answers /query while still loading.
    if ./crates/harness/scripts/wait-for-sparql.sh "http://$HORNDB_BIND/query" 600 \
       && wait_for_ready "http://$HORNDB_BIND/readyz" 2400; then
      # The driver runs from the shared baseline build (see HARNESS_OK):
      # run-spb-256.sh does `cargo run`, which honours these two variables.
      CARGO_TARGET_DIR="$PERSIST/target-cpu-ab/baseline" RUSTFLAGS="" \
      SPB_DRIVER_JAR="$jar" \
      SPB_SCENARIO="$SPB_ASSETS/spb-nightly.properties" \
      HORNDB_ENDPOINT="http://$HORNDB_BIND/query" \
      HORNDB_UPDATE_ENDPOINT="http://$HORNDB_BIND/update" \
        ./crates/harness/scripts/run-spb-256.sh > "$log" 2>&1
    else
      echo ">> engine did not become ready ($v)" >> "$log"
      tail -50 "$OUT/spb-engine-$v.log" >> "$log"
    fi
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    sleep 5
  done
  unset RUSTFLAGS CARGO_TARGET_DIR

  {
    echo "## spb — LDBC SPB, \`$(basename "$dataset")\`, nightly scenario (one run per variant)"
    echo
    echo "| variant | editorial-ops/s | aggregation-qps |"
    echo "|---|---|---|"
  } >> "$SUMMARY"
  for v in "${RUN_VARIANTS[@]}"; do
    log="$OUT/spb-$v.log"
    [ -f "$log" ] || continue
    local ed agg
    ed=$(grep -o 'editorial_ops_per_sec=[0-9.]*' "$log" | tail -1 | cut -d= -f2)
    agg=$(grep -o 'aggregation_queries_per_sec=[0-9.]*' "$log" | tail -1 | cut -d= -f2)
    echo "| $v | ${ed:-FAILED} | ${agg:-FAILED} |" >> "$SUMMARY"
  done
  echo >> "$SUMMARY"
}

want load && { leg_load || echo "- load leg **failed**" >> "$SUMMARY"; }
want spb && { leg_spb || true; }

cat "$SUMMARY"
