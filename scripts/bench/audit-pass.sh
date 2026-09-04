#!/usr/bin/env bash
#
# One pass over the benches that landed with the audit PRs (#328/#329/#331/
# #332/#333/#334/#337) but were never run on `hornbench`. Meant to be driven by
# `.github/workflows/bench.yml`, which is the only route to that host for
# anyone without ssh.
#
# Everything lands in `bench-out/`: one `<leg>.log` per leg (full criterion /
# driver output, kept as the artifact) plus `SUMMARY.md`, the table appended to
# the workflow's job summary.
#
# Legs are independently selectable so one failure does not cost the others:
#
#   BENCHES="stats dict_gc" bash scripts/bench/audit-pass.sh
#
# Legs (default: all):
#   lubm            #328 HDB-117 — OWL 2 RL materialize wall-clock, HornDB leg
#   backend         #329 HDB-126 — rule-firing vs graphblas closure backend
#   stats           #331 HDB-123 — stats_incremental
#   insert_retract  #332 HDB-122 — write latency under a concurrent reader
#   dict_gc         #333 HDB-121 — dict_gc_churn
#   dict_persist    HDB-57 — persistent dictionary flush / reopen / base probes (SPEC-25 S2)
#   wal             HDB-58 — WAL append under each fsync policy, replay (SPEC-25 S3)
#   view_derivation #337 HDB-72  — SPEC-29 view derivation (PLAN-29-01 T7)
#   trainmarks      #334 HDB-120 — q1-q6 timings, direct-source A/B
#   footprint       #334 HDB-120 — serving footprint, isolated (--mem-only)
#   spb             #334 HDB-120 — SPB-256 aggregation-qps, direct-source A/B
#
# Deliberately NOT `set -e`: a leg that dies must leave the others running. Each
# leg is wrapped in `run_leg`, which records its exit status in the summary.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

OUT="$REPO_ROOT/bench-out"
mkdir -p "$OUT"
SUMMARY="$OUT/SUMMARY.md"

ALL_LEGS="lubm backend stats insert_retract dict_gc dict_persist wal view_derivation trainmarks footprint spb"
BENCHES="${BENCHES:-$ALL_LEGS}"

# Persistent scratch on the runner's own disk. The Actions checkout is wiped by
# `git clean -ffdx` between runs, so anything expensive to build (the ~1.7 GB
# trainmarks corpora, the LUBM generator output) has to live outside it.
PERSIST="${BENCH_PERSIST:-/home/bench/horndb-bench}"
mkdir -p "$PERSIST" 2>/dev/null || PERSIST="$REPO_ROOT/target/bench-persist"
mkdir -p "$PERSIST"

# SPB writes its results through the harness into a SQLite trend DB. Point that
# at a scratch file: `run-spb-256.sh` hardcodes `--label horndb`, so writing to
# the nightly's cumulative DB would inject two off-schedule points into the
# published trend series.
export HARNESS_DB="$OUT/audit-harness.sqlite"

SPB_ASSETS="${SPB_ASSETS:-/home/bench/src/horndb/crates/harness/data/ldbc-spb/dist}"
HORNDB_BIND="${HORNDB_BIND:-127.0.0.1:3841}"

COMMIT="$(git rev-parse --short HEAD)"
DATE="$(date -u +%Y-%m-%d)"

{
  echo "# Audit-PR bench pass"
  echo
  echo "- commit: \`$COMMIT\`"
  echo "- date (UTC): $DATE"
  echo "- host: \`$(hostname)\` — $(nproc) cores, $(uname -sr)"
  echo "- legs requested: \`$BENCHES\`"
  echo
  echo "| bench | metric | value | unit |"
  echo "|---|---|---|---|"
} > "$SUMMARY"

row() { printf '| %s | %s | %s | %s |\n' "$1" "$2" "$3" "$4" >> "$SUMMARY"; }
note() { echo "$*" >&2; }

want() {
  case " $BENCHES " in *" $1 "*) return 0 ;; esac
  return 1
}

# Run one leg if selected, logging to bench-out/<leg>.log and recording whether
# it succeeded. `|| true` on the call site is deliberate: see the header.
run_leg() {
  local leg="$1"; shift
  want "$leg" || { note "== skip $leg (not in BENCHES)"; return 0; }
  note "== leg: $leg"
  local t0=$SECONDS
  "$@" 2>&1 | tee "$OUT/$leg.log"
  local st=${PIPESTATUS[0]}
  if [ "$st" -eq 0 ]; then
    row "$leg" "status" "ok ($((SECONDS - t0))s)" ""
  else
    row "$leg" "status" "**FAILED** (exit $st, $((SECONDS - t0))s, see $leg.log)" ""
  fi
}

# Criterion prints a benchmark id on its own unindented line, then an indented
# `time: [low median high]`. Pull out (id, median) pairs and add a summary row
# each. One extractor for every criterion leg.
criterion_rows() {
  local leg="$1" log="$2"
  awk '
    /^[^[:space:]]/ && NF { id = $1 }
    /time:[[:space:]]*\[/ {
      # ... time:   [1.0 ms 1.1 ms 1.2 ms]  -> median is the middle pair
      n = split($0, f, /[][]/)
      split(f[2], v, " ")
      if (id != "") printf "%s\t%s %s\n", id, v[3], v[4]
      id = ""
    }
  ' "$log" | while IFS=$'\t' read -r id val; do
    row "$leg" "$id" "${val% *}" "${val##* }"
  done
}

# --------------------------------------------------------------------------
# #328 (HDB-117) — id-level closure hand-off: OWL 2 RL materialize wall-clock.
#
# The brief names `scripts/bench/compare-rdfox.sh --lubm 1`, but that script
# hard-requires RDFox (it exits before doing anything if the zip, binary or
# licence is missing) and has no HornDB-only path. So run the HornDB half of
# that comparison directly — the same `horndb-bench materialize` call the
# script makes — and say plainly that the RDFox column is absent.
#
# The LUBM-1 corpus comes from `get_lubm.sh`: generated locally where a JDK
# exists, otherwise downloaded from the `bench-corpora` release (hornbench has
# no JDK). Only if both fail do we fall back to the taxonomy workload from
# gen_workload.py (pure python), and the summary then says so.
# --------------------------------------------------------------------------
leg_lubm() {
  cargo build --release -p horndb-bench-rdfox --bin horndb-bench || return 1
  local hb="$REPO_ROOT/target/release/horndb-bench"
  local dir="$PERSIST/lubm/1"

  scripts/bench/get_lubm.sh 1 "$dir" || {
    echo ">> LUBM-1 unavailable (no JDK and no corpus download); falling back to taxonomy"
    dir="$PERSIST/taxonomy"
    mkdir -p "$dir"
    [ -f "$dir/abox.nt" ] || python3 scripts/bench/gen_workload.py taxonomy 12 40000 "$dir/abox.nt" || return 1
    : > "$dir/tbox.nt"
  }

  echo ">> RDFox column: NOT MEASURED — RDFox is not installed on this runner"
  echo ">> corpus: $dir"
  wc -l "$dir"/*.nt

  local args=(--data "$dir/abox.nt")
  [ -s "$dir/tbox.nt" ] && args=(--data "$dir/tbox.nt" --data "$dir/abox.nt")
  # Three runs; the summary takes the median.
  for i in 1 2 3; do echo "run$i: $("$hb" materialize "${args[@]}")"; done
}

summarize_lubm() {
  local log="$OUT/lubm.log"
  [ -f "$log" ] || return 0
  # Each run line is a flat JSON object; pull the phase timings out of it.
  # Label the corpus that actually ran. The leg falls back to the synthetic
  # taxonomy corpus if LUBM-1 could be neither generated nor downloaded, and
  # reporting that as "lubm-1" would be a false record.
  local corpus="lubm-1"
  grep -q "falling back to taxonomy" "$log" && corpus="taxonomy d12/40k (LUBM-1 unavailable)"
  for k in reason_ms apply_ms total_ms inferred; do
    local med
    med=$(grep -o "\"$k\":[0-9.]*" "$log" | cut -d: -f2 | sort -g | awk '{a[NR]=$1} END{if(NR)print a[int((NR+1)/2)]}')
    [ -n "$med" ] && row "$corpus (HornDB only)" "$k (median of 3)" "$med" "${k##*_}"
  done
  grep -q "NOT MEASURED" "$log" && row "$corpus (HornDB only)" "RDFox column" "not measured — RDFox absent on runner" ""
}

# --------------------------------------------------------------------------
# #329 (HDB-126) — closure backend selection: rule-firing vs graphblas, same
# corpus, same process shape. The brief also asked for a SKOS-heavy synthetic
# corpus; the repo has no generator for one (only small hand-written SKOS test
# fixtures), so that half is skipped rather than invented here.
# --------------------------------------------------------------------------
leg_backend() {
  cargo build --release -p horndb-bench-rdfox --bin horndb-bench || return 1
  local hb="$REPO_ROOT/target/release/horndb-bench"
  local data="$PERSIST/taxonomy/abox.nt"
  mkdir -p "$PERSIST/taxonomy"
  [ -f "$data" ] || python3 scripts/bench/gen_workload.py taxonomy 12 40000 "$data" || return 1

  echo ">> SKOS-heavy synthetic corpus: SKIPPED — no generator exists in-repo"
  for backend in rulefiring graphblas; do
    for i in 1 2 3; do
      echo "$backend run$i: $("$hb" materialize --backend "$backend" --data "$data")"
    done
  done
}

summarize_backend() {
  local log="$OUT/backend.log"
  [ -f "$log" ] || return 0
  for backend in rulefiring graphblas; do
    for k in closure_backend_ms reason_ms total_ms inferred; do
      local med
      med=$(grep "^$backend run" "$log" | grep -o "\"$k\":[0-9.]*" | cut -d: -f2 \
            | sort -g | awk '{a[NR]=$1} END{if(NR)print a[int((NR+1)/2)]}')
      [ -n "$med" ] && row "backend/$backend" "$k (median of 3)" "$med" "${k##*_}"
    done
  done
  row "backend" "SKOS-heavy corpus" "skipped — no in-repo generator" ""
}

# --------------------------------------------------------------------------
# #331 / #332 / #333 / #337 — plain criterion benches.
# --------------------------------------------------------------------------
leg_stats()          { cargo bench -p horndb-sparql  --bench stats_incremental; }
leg_dict_gc()        { cargo bench -p horndb-storage --bench dict_gc_churn; }
leg_dict_persist()   { cargo bench -p horndb-storage --bench dict_persist; }
leg_wal()            { cargo bench -p horndb-storage --bench wal_append; }
leg_view_derivation() { cargo bench -p horndb-sparql --bench view_derivation; }

# Only the concurrent-reader group is recordable: the file's own header marks
# `insert_10k` / `retract_then_scan_10k` as local smoke checks.
leg_insert_retract() {
  cargo bench -p horndb-storage --bench insert_retract -- write_under_concurrent_reader
}

# Criterion's own output is a confidence interval on the mean, which hides the
# shape of a bimodal benchmark (stats_incremental is explicitly bimodal, and
# insert_retract wants a tail percentile). Both need the raw per-sample data
# criterion writes to target/criterion/**/new/sample.json, so pull min/p99 out
# of every sample file a leg produced and keep the files in the artifact.
# $1 = leg name, $2.. = target/criterion subdirectories belonging to that leg
# (criterion accumulates across legs in one target dir, so an unscoped walk
# would re-emit every earlier leg's samples).
criterion_samples() {
  local leg="$1"; shift
  local f
  while IFS= read -r f; do
    [ -n "$f" ] || continue
    local id
    id=$(dirname "$(dirname "$f")"); id=${id#target/criterion/}
    python3 - "$f" "$leg" "$id" >> "$SUMMARY" <<'PY'
import json, sys
path, leg, bench_id = sys.argv[1], sys.argv[2], sys.argv[3]
s = json.load(open(path))
# `times` is total nanoseconds per sample, `iters` how many iterations it covered.
per = sorted(t / i for t, i in zip(s["times"], s["iters"]))
def q(p):
    return per[min(len(per) - 1, int(round(p * (len(per) - 1))))] / 1000.0
print(f"| {leg} | {bench_id} min (fastest sample) | {q(0):.2f} | µs |")
print(f"| {leg} | {bench_id} p99 | {q(0.99):.2f} | µs |")
PY
  done < <(find "$@" -path '*/new/sample.json' 2>/dev/null | sort)
  mkdir -p "$OUT/criterion-samples"
  find "$@" -path '*/new/sample.json' -exec cp --parents {} "$OUT/criterion-samples/" \; 2>/dev/null || true
}

# The one-shot lines the view_derivation bench prints before criterion starts.
summarize_view_derivation() {
  criterion_rows view_derivation "$OUT/view_derivation.log"
  grep -E '^\[view_derivation\] views=|^\[mem\]' "$OUT/view_derivation.log" 2>/dev/null \
    | while read -r line; do row "view_derivation" "one-shot" "$line" ""; done
}

# --------------------------------------------------------------------------
# #334 (HDB-120) — trainmarks xlarge, `HORNDB_DIRECT_SOURCE` unset vs =1.
# Gives both halves the PENDING benchmarks row asks for: the `[mem] serving
# footprint` line and the six query times.
# --------------------------------------------------------------------------
leg_trainmarks() {
  export TRAINMARKS_DIR="$PERSIST/trainmarks"
  local mode
  for mode in vec direct; do
    echo "===== MODE=$mode ====="
    if [ "$mode" = direct ]; then export HORNDB_DIRECT_SOURCE=1; else unset HORNDB_DIRECT_SOURCE; fi
    scripts/bench/trainmarks.sh --scales xlarge --timeout 1800 || return 1
    cp "$TRAINMARKS_DIR/results_horndb.json" "$OUT/trainmarks-$mode.json" 2>/dev/null || true
  done
}

summarize_trainmarks() {
  local mode
  for mode in vec direct; do
    [ -f "$OUT/trainmarks-$mode.json" ] || continue
    python3 - "$OUT/trainmarks-$mode.json" "$mode" >> "$SUMMARY" <<'PY'
import json, sys
# bench-trainmarks writes a flat array of {framework, scale, operation, seconds};
# `seconds` is a number, or a string starting "ERROR:" when the op failed.
rows = json.load(open(sys.argv[1]))
mode = sys.argv[2]
for r in rows:
    secs = r.get("seconds")
    val = f"{secs:.4f}" if isinstance(secs, (int, float)) else str(secs)
    unit = "s" if isinstance(secs, (int, float)) else ""
    print(f'| trainmarks/{mode} | {r.get("scale")} {r.get("operation")} | {val} | {unit} |')
PY
  done
}

# --------------------------------------------------------------------------
# #334 — serving footprint, isolated.
#
# Separate from the `trainmarks` leg on purpose. The full trainmarks run builds
# a second store for the read_ntriples timing and drops it, and snmalloc does
# not return freed arenas to the OS -- so an RSS sample taken after that run is
# a whole-process high-water mark, not the memory a served store occupies.
# `--mem-only` loads once, snapshots once, runs the read queries, then samples.
# --------------------------------------------------------------------------
leg_footprint() {
  cargo build --release -p horndb-bench-trainmarks || return 1
  local driver="$REPO_ROOT/target/release/bench-trainmarks"
  local work="$PERSIST/trainmarks"
  [ -d "$work/data" ] || { echo ">> no corpus at $work/data — run the trainmarks leg first"; return 1; }
  export HORNDB_LOAD_THREADS="${HORNDB_LOAD_THREADS:-1}"
  local mode
  for mode in vec direct; do
    echo "===== MODE=$mode ====="
    if [ "$mode" = direct ]; then export HORNDB_DIRECT_SOURCE=1; else unset HORNDB_DIRECT_SOURCE; fi
    "$driver" --data-dir "$work/data" --queries-dir "$work/queries" \
      --scale xlarge --out "$OUT/footprint-$mode.json" --timeout-secs 1800 --mem-only || return 1
  done
}

summarize_footprint() {
  awk '/^===== MODE=/ { mode = substr($2, 6) }
       /serving footprint/ { print mode "\t" $0 }' "$OUT/footprint.log" 2>/dev/null \
    | while IFS=$'\t' read -r mode line; do
        row "footprint/$mode" "serving footprint (isolated, --mem-only)" "${line#*: }" ""
      done
}

# Block until `serve` reports it has finished loading. See the call site.
wait_for_ready() {
  local url="$1" timeout="$2" i
  for ((i = 0; i < timeout; i++)); do
    if curl -fsS "$url" >/dev/null 2>&1; then
      echo ">> engine ready after ${i}s"
      return 0
    fi
    sleep 1
  done
  echo ">> engine never became ready at $url within ${timeout}s"
  return 1
}

# --------------------------------------------------------------------------
# #334 (HDB-120) — SPB-256 aggregation-qps, same A/B. Mirrors nightly.yml's
# bring-up so the number is comparable with the published trend.
# --------------------------------------------------------------------------
leg_spb() {
  local dataset="$SPB_ASSETS/spb-256.nt"
  local jar="$SPB_ASSETS/semantic_publishing_benchmark-basic-standard.jar"
  if [ ! -f "$dataset" ] || [ ! -f "$jar" ]; then
    echo ">> SPB assets missing under $SPB_ASSETS — skipping"
    return 1
  fi
  cargo build -p horndb-harness --bin harness --release --features real-engine || return 1
  cp crates/harness/scenarios/spb-nightly.properties "$SPB_ASSETS/spb-nightly.properties"

  local mode
  for mode in vec direct; do
    echo "===== MODE=$mode ====="
    if [ "$mode" = direct ]; then export HORNDB_DIRECT_SOURCE=1; else unset HORNDB_DIRECT_SOURCE; fi
    DATA_FILES="$dataset" RELEASE=1 BIND="$HORNDB_BIND" \
      ./crates/harness/scripts/start-engine.sh > "$OUT/spb-engine-$mode.log" 2>&1 &
    local pid=$!
    # Liveness, then readiness. `serve` loads the dataset on a background
    # thread and answers /query with 200 the whole time, so waiting only on
    # /query starts the driver against an empty store — which is exactly how
    # the first attempt at this leg produced "no Creative Works were found".
    # /readyz is the endpoint that reports 503 until the load finishes.
    if ./crates/harness/scripts/wait-for-sparql.sh "http://$HORNDB_BIND/query" 600 \
       && wait_for_ready "http://$HORNDB_BIND/readyz" 900; then
      SPB_DRIVER_JAR="$jar" \
      SPB_SCENARIO="$SPB_ASSETS/spb-nightly.properties" \
      HORNDB_ENDPOINT="http://$HORNDB_BIND/query" \
      HORNDB_UPDATE_ENDPOINT="http://$HORNDB_BIND/update" \
        ./crates/harness/scripts/run-spb-256.sh
    else
      echo ">> engine did not become ready in 600s ($mode)"
      tail -50 "$OUT/spb-engine-$mode.log"
    fi
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    sleep 5
  done
}

summarize_spb() {
  awk '/^===== MODE=/ { mode = substr($2, 6) }
       /aggregation_queries_per_sec=/ { print mode "\t" $0 }' "$OUT/spb.log" 2>/dev/null \
    | while IFS=$'\t' read -r mode line; do
        local agg ed
        agg=$(echo "$line" | grep -o 'aggregation_queries_per_sec=[0-9.]*' | cut -d= -f2)
        ed=$(echo "$line" | grep -o 'editorial_ops_per_sec=[0-9.]*' | cut -d= -f2)
        row "spb-256/$mode" "aggregation-qps" "$agg" "qps"
        row "spb-256/$mode" "editorial-ops-per-sec" "$ed" "ops/s"
      done
}

# --------------------------------------------------------------------------

run_leg lubm            leg_lubm            || true
run_leg backend         leg_backend         || true
run_leg stats           leg_stats           || true
run_leg insert_retract  leg_insert_retract  || true
run_leg dict_gc         leg_dict_gc         || true
run_leg dict_persist    leg_dict_persist    || true
run_leg wal             leg_wal             || true
run_leg view_derivation leg_view_derivation || true
run_leg trainmarks      leg_trainmarks      || true
run_leg footprint       leg_footprint       || true
run_leg spb             leg_spb             || true

echo "== summarizing" >&2
want lubm            && summarize_lubm
want backend         && summarize_backend
want stats           && { criterion_rows stats "$OUT/stats.log"; criterion_samples stats target/criterion/stats_incremental; }
want insert_retract  && { criterion_rows insert_retract "$OUT/insert_retract.log"; criterion_samples insert_retract target/criterion/write_under_concurrent_reader; }
want dict_gc         && { criterion_rows dict_gc "$OUT/dict_gc.log"; criterion_samples dict_gc target/criterion/churn_4x1k_no_gc target/criterion/churn_4x1k_compact_gc; }
want dict_persist    && { criterion_rows dict_persist "$OUT/dict_persist.log"; criterion_samples dict_persist target/criterion/dict_persist target/criterion/dict_persist_probe; }
want wal             && { criterion_rows wal "$OUT/wal.log"; criterion_samples wal target/criterion/append target/criterion/replay; }
want view_derivation && summarize_view_derivation
want trainmarks      && summarize_trainmarks
want footprint       && summarize_footprint
want spb             && summarize_spb

echo >> "$SUMMARY"
echo "Full per-leg output is in the \`bench-${GITHUB_RUN_ID:-local}\` artifact (\`<leg>.log\`)." >> "$SUMMARY"
cat "$SUMMARY"
exit 0
