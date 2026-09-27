#!/usr/bin/env bash
#
# Build-variant A/B on hornbench: the same benchmarks run against several builds
# of HornDB that differ in compiler flags (`-C target-cpu=<level>`), in source
# commit, or both.
#
# Two questions it was written for:
#
# * target-cpu. Every release build today — the nightly included — targets
#   baseline x86-64 (SSE2 only). The repo sets no `target-cpu`, and
#   `actions-rust-lang/setup-rust-toolchain` exports `RUSTFLAGS="-D warnings"`,
#   which overrides any `build.rustflags` a runner-side config might set. The
#   hand-written kernels in `horndb-simd` pick AVX2/AVX-512 at runtime either
#   way; everything the compiler vectorises or schedules on its own does not.
# * a source change, measured against its parent: build the parent commit as
#   one variant and HEAD as another, same flags, same session.
#
# Driven by `.github/workflows/bench.yml`:
#
#   gh workflow run bench.yml --ref <branch> -f script=scripts/bench/target-cpu-ab.sh \
#     -f env="VARIANTS=before=baseline@<full-sha> baseline"
#
# Knobs (via the workflow's `env` input, one KEY=VALUE per line):
#
#   VARIANTS  space-separated variant specs (default "baseline x86-64-v3 x86-64-v4").
#             A spec is `[label=]<cpu>[@<ref>]`:
#               <cpu>   `baseline` (no -C target-cpu) or a target-cpu value
#                       (x86-64-v3, x86-64-v4, znver4, native, ...)
#               <ref>   a commit to build instead of HEAD. Give the FULL sha or a
#                       branch name: the Actions checkout is shallow, so the
#                       commit is fetched by name, and GitHub will not resolve an
#                       abbreviated sha over the wire.
#               label   what the summary calls it (default: the spec itself)
#             Deltas in the summary are against the FIRST variant listed.
#   LEGS      "load spb" (default both)
#   REPS      store_load repetitions per variant (default 3). Reps are
#             interleaved across variants so host drift spreads over all of them.
#   THREADS   store_load parse-thread counts, space-separated (default "8", the
#             shipped `auto` on hornbench). "1 8" adds the one-thread load, where
#             every intern call runs on the consumer.
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
# Not `set -e`: one failed variant must not cost the others. But any failure —
# a variant that does not build, a leg that yields no number — makes the script
# exit non-zero at the end, so a partial result never shows up as a green run.

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
WORK="$PERSIST/target-cpu-ab"
mkdir -p "$WORK"

# Scratch trend DB: run-spb-256.sh labels its rows `horndb`, which must not land
# in the nightly's cumulative series.
export HARNESS_DB="$OUT/target-cpu-harness.sqlite"
SPB_ASSETS="${SPB_ASSETS:-/home/bench/src/horndb/crates/harness/data/ldbc-spb/dist}"
HORNDB_BIND="${HORNDB_BIND:-127.0.0.1:3841}"
# Match nightly.yml: its dataset (the true-scale corpus since HDB-37, not the
# older spb-256.nt stand-in) and its server memory ceiling.
SPB_DATASET="${SPB_DATASET:-$SPB_ASSETS/spb-sf128.nt}"
MEMORY_MAX="${MEMORY_MAX:-90G}"

FAILED=0
note() { echo "$*" >&2; }
fail() { FAILED=1; echo "- **$***" >> "$SUMMARY"; note ">> FAIL: $*"; }
want() { case " $LEGS " in *" $1 "*) return 0 ;; esac; return 1; }
median() { sort -g | awk '{a[NR]=$1} END{if(NR) print a[int((NR+1)/2)]}'; }

{
  echo "# build-variant A/B"
  echo
  echo "- commit (HEAD): \`$(git rev-parse --short HEAD)\`"
  echo "- date (UTC): $(date -u +%Y-%m-%d)"
  echo "- host: \`$(hostname)\` — $(nproc) cores, $(uname -sr)"
  echo "- cpu: $(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2- | sed 's/^ //')"
  echo "- variants: \`$VARIANTS\`; legs: \`$LEGS\`; store_load reps: $REPS, threads: \`$THREADS\`"
  echo
} > "$SUMMARY"

# ---------------------------------------------------------------------------
# Variant parsing. Per variant: LABEL, CPU, SRC (source tree), TDIR (target dir).
# ---------------------------------------------------------------------------
declare -A CPU SRC TDIR
ORDER=()

# Skip any cpu level the host cannot execute (e.g. x86-64-v4 without AVX-512):
# the binary would die with SIGILL, not measure anything.
supported() {
  local flags; flags=" $(grep -m1 '^flags' /proc/cpuinfo | cut -d: -f2) "
  local need=()
  case "$1" in
    x86-64-v2) need=(sse4_2 popcnt ssse3) ;;
    x86-64-v3) need=(sse4_2 popcnt avx2 bmi2 fma movbe) ;;
    x86-64-v4) need=(sse4_2 popcnt avx2 bmi2 fma avx512f avx512bw avx512dq avx512vl) ;;
    *) return 0 ;;  # baseline, native, or a named CPU — trust the caller
  esac
  local f
  for f in "${need[@]}"; do
    case "$flags" in *" $f "*) ;; *) note ">> $1 needs $f, host lacks it"; return 1 ;; esac
  done
}

# Source tree for a ref: HEAD is the checkout itself; anything else is exported
# once (`git archive`, so no worktree registration survives the checkout wipe)
# into the persistent work dir, keyed by full sha.
source_for() {
  local ref="$1" sha dir
  if [ -z "$ref" ]; then echo "$REPO_ROOT"; return 0; fi
  if ! sha=$(git rev-parse --verify -q "$ref^{commit}"); then
    git fetch -q --no-tags --depth=1 origin "$ref" >&2 || return 1
    sha=$(git rev-parse --verify -q FETCH_HEAD^{commit}) || return 1
  fi
  dir="$WORK/src/$sha"
  if [ ! -f "$dir/.complete" ]; then
    rm -rf "$dir"; mkdir -p "$dir"
    git archive "$sha" | tar -x -C "$dir" || return 1
    touch "$dir/.complete"
  fi
  echo "$dir"
}

for spec in $VARIANTS; do
  label="$spec"; body="$spec"
  case "$spec" in *=*) label="${spec%%=*}"; body="${spec#*=}" ;; esac
  cpu="${body%%@*}"; ref=""
  case "$body" in *@*) ref="${body#*@}" ;; esac
  if ! supported "$cpu"; then
    echo "- skipped \`$label\`: host CPU lacks \`$cpu\`" >> "$SUMMARY"
    continue
  fi
  if ! src=$(source_for "$ref"); then
    fail "could not fetch ref \`$ref\` for \`$label\` (use a full sha or a branch name)"
    continue
  fi
  CPU[$label]="$cpu"
  SRC[$label]="$src"
  # Target dir keyed by what decides the binary: source tree + flags. sccache
  # keys on RUSTFLAGS too, so variants never share objects.
  TDIR[$label]="$WORK/target/$(printf '%s' "$src|$cpu" | sha1sum | cut -c1-12)-$cpu"
  ORDER+=("$label")
  echo "- \`$label\`: target-cpu \`$cpu\`, source $( [ -n "$ref" ] && echo "\`$ref\`" || echo HEAD)" >> "$SUMMARY"
done
echo >> "$SUMMARY"

rustflags_for() { [ "$1" = baseline ] && echo "" || echo "-C target-cpu=$1"; }

# ---------------------------------------------------------------------------
# Builds, up front: a build failure shows as one summary line, and build time
# does not sit between the timed reps.
# ---------------------------------------------------------------------------
declare -A BUILT
# The SPB driver (`harness`) is the measuring tool, not the thing measured, so
# it is built once, at baseline from HEAD, and shared by every variant. It also
# cannot be built with target-cpu=x86-64-v3/v4: oxrocksdb-sys (via oxigraph)
# turns on RocksDB's AVX2 CRC path whenever avx2 is on, and that path needs
# PCLMUL, which neither level includes -- the C++ build fails.
HARNESS_TDIR="$WORK/harness"
HARNESS_OK=0
if want spb; then
  note "== build harness (baseline, HEAD, shared by all variants)"
  if CARGO_TARGET_DIR="$HARNESS_TDIR" RUSTFLAGS="" \
     cargo build --release -p horndb-harness --bin harness --features real-engine \
       > "$OUT/build-harness.log" 2>&1; then
    HARNESS_OK=1
  else
    fail "harness build failed (see build-harness.log); spb leg skipped"
    tail -40 "$OUT/build-harness.log" >&2
  fi
fi

for v in "${ORDER[@]}"; do
  flags=$(rustflags_for "${CPU[$v]}")
  blog="$OUT/build-$v.log"
  note "== build $v (RUSTFLAGS='$flags', src ${SRC[$v]}, target ${TDIR[$v]})"
  ok=1
  if want load; then
    ( cd "${SRC[$v]}" && CARGO_TARGET_DIR="${TDIR[$v]}" RUSTFLAGS="$flags" \
        cargo build --release -p horndb-bench-trainmarks --bin store_load ) > "$blog" 2>&1 || ok=0
  fi
  if want spb && [ "$ok" = 1 ]; then
    ( cd "${SRC[$v]}" && CARGO_TARGET_DIR="${TDIR[$v]}" RUSTFLAGS="$flags" \
        cargo build --release -p horndb-sparql --bin serve --features server ) >> "$blog" 2>&1 || ok=0
  fi
  if [ "$ok" = 1 ]; then
    BUILT[$v]=1
  else
    fail "build failed for \`$v\` (see build-$v.log)"
    # Also into the job log: the artifact is not always reachable, the log is.
    grep -E '^(error|warning)|^\s+-->' "$blog" | head -40 >&2
    tail -40 "$blog" >&2
  fi
done

# ---------------------------------------------------------------------------
# load: store_load on trainmarks xlarge, reps interleaved across variants.
# ---------------------------------------------------------------------------
leg_load() {
  local work="$PERSIST/trainmarks"
  local data="$work/data"
  if [ ! -f "$data/xlarge.nt" ] || [ ! -f "$data/xlarge.ttl" ]; then
    note ">> generating trainmarks datasets (seed 42; ~1.7 GB total)"
    mkdir -p "$work"
    cp scripts/bench/trainmarks/generate_data.py "$work/generate_data.py"
    ( cd "$work" && python3 generate_data.py ) || { fail "trainmarks data generation failed"; return 1; }
  fi

  local fmt th rep v log
  for th in $THREADS; do
    for fmt in nt ttl; do
      for rep in $(seq 1 "$REPS"); do
        for v in "${ORDER[@]}"; do
          [ -n "${BUILT[$v]:-}" ] || continue
          log="$OUT/load-$v-$fmt-t$th.log"
          note "== load $fmt threads $th rep $rep variant $v"
          echo "----- rep $rep -----" >> "$log"
          "${TDIR[$v]}/release/store_load" --file "$data/xlarge.$fmt" --threads "$th" >> "$log" 2>&1 \
            || echo "FAILED rep $rep" >> "$log"
        done
      done
    done
  done

  local first="${ORDER[0]:-}"
  {
    echo "## load — store_load, trainmarks xlarge (median of $REPS; delta vs \`$first\`)"
    echo
    echo "| corpus | threads | variant | wall (s) | Δ | reps (s) | peak RSS (MiB) |"
    echo "|---|---|---|---|---|---|---|"
  } >> "$SUMMARY"
  for th in $THREADS; do
    for fmt in nt ttl; do
      local base=""
      for v in "${ORDER[@]}"; do
        log="$OUT/load-$v-$fmt-t$th.log"
        [ -f "$log" ] || continue
        local wall rss reps delta="—"
        wall=$(grep -o '\[load\] wall [0-9.]*' "$log" | awk '{print $3}' | median)
        reps=$(grep -o '\[load\] wall [0-9.]*' "$log" | awk '{print $3}' | paste -sd' ')
        rss=$(grep -o 'peak RSS [0-9]*' "$log" | awk '{print $3}' | median)
        if [ -z "$wall" ] || grep -q '^FAILED' "$log"; then
          fail "load xlarge.$fmt t$th failed for \`$v\` (see $(basename "$log"))"
          [ -n "$wall" ] || { echo "| xlarge.$fmt | $th | $v | FAILED | | | |" >> "$SUMMARY"; continue; }
        fi
        if [ "$v" = "$first" ]; then
          base="$wall"
        elif [ -n "$base" ]; then
          delta=$(awk -v b="$base" -v w="$wall" 'BEGIN{printf "%+.1f%%", 100*(w-b)/b}')
        fi
        echo "| xlarge.$fmt | $th | $v | $wall | $delta | $reps | $rss |" >> "$SUMMARY"
      done
    done
  done
  echo >> "$SUMMARY"
}

# ---------------------------------------------------------------------------
# spb: SPB per variant, same bring-up as nightly.yml.
# ---------------------------------------------------------------------------
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
    fail "spb skipped: assets missing under \`$SPB_ASSETS\`"
    return 1
  fi
  cp crates/harness/scenarios/spb-nightly.properties "$SPB_ASSETS/spb-nightly.properties"

  local v log pid
  for v in "${ORDER[@]}"; do
    [ -n "${BUILT[$v]:-}" ] || continue
    log="$OUT/spb-$v.log"
    note "== spb variant $v"
    # The engine runs from the variant's own tree and target dir; start-engine.sh
    # rebuilds serve there, which is a no-op after the build step above.
    ( cd "${SRC[$v]}" && CARGO_TARGET_DIR="${TDIR[$v]}" RUSTFLAGS="$(rustflags_for "${CPU[$v]}")" \
        DATA_FILES="$dataset" RELEASE=1 BIND="$HORNDB_BIND" MEMORY_MAX="$MEMORY_MAX" \
        exec ./crates/harness/scripts/start-engine.sh ) > "$OUT/spb-engine-$v.log" 2>&1 &
    pid=$!
    # /readyz, not /query: serve answers /query while still loading.
    if ./crates/harness/scripts/wait-for-sparql.sh "http://$HORNDB_BIND/query" 600 \
       && wait_for_ready "http://$HORNDB_BIND/readyz" 2400; then
      # The driver runs from the shared harness build: run-spb-256.sh does
      # `cargo run`, which honours these two variables.
      CARGO_TARGET_DIR="$HARNESS_TDIR" RUSTFLAGS="" \
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

  local first="${ORDER[0]:-}"
  {
    echo "## spb — LDBC SPB, \`$(basename "$dataset")\`, nightly scenario (one run per variant; delta vs \`$first\`)"
    echo
    echo "| variant | editorial-ops/s | Δ | aggregation-qps | Δ |"
    echo "|---|---|---|---|---|"
  } >> "$SUMMARY"
  local bed="" bagg=""
  for v in "${ORDER[@]}"; do
    log="$OUT/spb-$v.log"
    [ -f "$log" ] || continue
    local ed agg ded="—" dagg="—"
    ed=$(grep -o 'editorial_ops_per_sec=[0-9.]*' "$log" | tail -1 | cut -d= -f2)
    agg=$(grep -o 'aggregation_queries_per_sec=[0-9.]*' "$log" | tail -1 | cut -d= -f2)
    # A zero editorial rate is not a measurement: the editorial agents found
    # nothing to do (the failure spb-256.nt produced in run #39).
    if [ -z "$ed" ] || [ -z "$agg" ] || awk -v e="$ed" 'BEGIN{exit !(e+0 == 0)}'; then
      fail "spb produced no usable numbers for \`$v\` (see spb-$v.log)"
      # Into the job log too: the artifact is not reachable from every session.
      note ">> tail of spb-$v.log:"; tail -60 "$log" >&2
      note ">> tail of spb-engine-$v.log:"; tail -30 "$OUT/spb-engine-$v.log" >&2
    fi
    if [ "$v" = "$first" ]; then
      bed="$ed"; bagg="$agg"
    else
      [ -n "$bed" ] && [ -n "$ed" ] && ded=$(awk -v b="$bed" -v w="$ed" 'BEGIN{if(b>0) printf "%+.1f%%", 100*(w-b)/b; else print "—"}')
      [ -n "$bagg" ] && [ -n "$agg" ] && dagg=$(awk -v b="$bagg" -v w="$agg" 'BEGIN{if(b>0) printf "%+.1f%%", 100*(w-b)/b; else print "—"}')
    fi
    echo "| $v | ${ed:-FAILED} | $ded | ${agg:-FAILED} | $dagg |" >> "$SUMMARY"
  done
  echo >> "$SUMMARY"
}

[ "${#ORDER[@]}" -gt 0 ] || fail "no runnable variants"
want load && { leg_load || true; }
want spb && { leg_spb || true; }

if [ "$FAILED" = 1 ]; then
  echo "**Result: INCOMPLETE — at least one variant or leg failed; see the lines above.**" >> "$SUMMARY"
fi
cat "$SUMMARY"
exit "$FAILED"
