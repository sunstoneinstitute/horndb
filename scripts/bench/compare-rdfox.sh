#!/usr/bin/env bash
#
# compare-rdfox.sh — internal HornDB-vs-RDFox performance comparison.
#
# Runs the three operations we have published goals against RDFox for, on
# identical input files, through both engines, and prints a side-by-side
# table with the goal verdict:
#
#   import       bulk N-Triples load throughput   (SPEC-02 F8:  >= 1 M tps)
#   transitive   single-predicate closure         (SPEC-05 #1:  >= 10x RDFox)
#   materialize  OWL 2 RL forward materialization  (Stage-1:     within 3x RDFox)
#
# RDFox commercial licences forbid *publishing* comparative numbers (the
# "DeWitt clause"); this script is for INTERNAL use only. See BENCHMARKS.md.
#
# Usage:
#   scripts/bench/compare-rdfox.sh [--chain N] [--depth D] [--instances I] [--keep]
#
# Environment (all optional, sensible defaults):
#   RDFOX_HOME   unpacked RDFox dir containing the `RDFox` binary.
#   RDFOX_ZIP    RDFox zip to unpack if RDFOX_HOME is unset.
#                default: ~/Downloads/RDFox-macOS-arm64-7.5b.zip
#   RDFOX_LIC    path to the licence file.  default: ~/Downloads/RDFox.lic
#   OUTDIR       work/results dir.           default: target/bench-rdfox
set -euo pipefail

# --- locate the repo root (script lives in scripts/bench/) ---
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$REPO_ROOT"

# --- config ---
CHAIN_N=2500
TAX_DEPTH=30
TAX_INSTANCES=20000
KEEP=0
LUBM_N=0          # 0 = run the original three comparisons; >0 = LUBM mode
CAP_SECONDS=1800  # wall-clock cap for HornDB materialize (LUBM mode)
RDFOX_ZIP="${RDFOX_ZIP:-$HOME/Downloads/RDFox-macOS-arm64-7.5b.zip}"
RDFOX_LIC="${RDFOX_LIC:-$HOME/Downloads/RDFox.lic}"
OUTDIR="${OUTDIR:-$REPO_ROOT/target/bench-rdfox}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --chain)     CHAIN_N="$2"; shift 2 ;;
    --depth)     TAX_DEPTH="$2"; shift 2 ;;
    --instances) TAX_INSTANCES="$2"; shift 2 ;;
    --keep)      KEEP=1; shift ;;
    --lubm)         LUBM_N="$2"; shift 2 ;;
    --cap-seconds)  CAP_SECONDS="$2"; shift 2 ;;
    -h|--help)   sed -n '2,30p' "$0"; exit 0 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

WORK="$OUTDIR/work"
LOGS="$OUTDIR/logs"
mkdir -p "$WORK" "$LOGS"

# --- resolve RDFox ---
if [[ -z "${RDFOX_HOME:-}" ]]; then
  [[ -f "$RDFOX_ZIP" ]] || { echo "RDFOX_ZIP not found: $RDFOX_ZIP (set RDFOX_HOME or RDFOX_ZIP)" >&2; exit 1; }
  echo ">> unpacking RDFox from $RDFOX_ZIP" >&2
  unzip -o -q "$RDFOX_ZIP" -d "$OUTDIR/rdfox"
  RDFOX_HOME="$(dirname "$(find "$OUTDIR/rdfox" -name RDFox -type f | head -1)")"
fi
RDFOX_BIN="$RDFOX_HOME/RDFox"
[[ -x "$RDFOX_BIN" ]] || { echo "RDFox binary not executable: $RDFOX_BIN" >&2; exit 1; }
[[ -f "$RDFOX_LIC" ]] || { echo "licence not found: $RDFOX_LIC" >&2; exit 1; }
export RDFOX_LICENSE_CONTENT="$(cat "$RDFOX_LIC")"
echo ">> RDFox: $RDFOX_BIN" >&2

# --- build the HornDB micro-runner ---
echo ">> building horndb-bench (release)" >&2
cargo build --release -p horndb-bench-rdfox >/dev/null 2>"$LOGS/cargo-build.log" \
  || { echo "cargo build failed; see $LOGS/cargo-build.log" >&2; tail -20 "$LOGS/cargo-build.log" >&2; exit 1; }
HB="$REPO_ROOT/target/release/horndb-bench"

# ----------------------------------------------------------------------------
# helpers
# ----------------------------------------------------------------------------

# run_rdfox <root> <cmd...>  — run a sandbox instance, stdout+stderr returned.
run_rdfox() {
  local root="$1"; shift
  "$RDFOX_BIN" sandbox "$root" "$@" 2>&1
}

# cap_run <seconds> <outfile> <cmd...>
# Run a command writing stdout+stderr to <outfile>, killed after <seconds>.
# Returns 0 on success, 124 if it hit the cap (or otherwise failed). Portable:
# uses gtimeout/timeout if present, else a background-process + watchdog.
cap_run() {
  local cap="$1" out="$2"; shift 2
  if command -v timeout >/dev/null; then
    timeout "${cap}s" "$@" >"$out" 2>&1; return $?
  elif command -v gtimeout >/dev/null; then
    gtimeout "${cap}s" "$@" >"$out" 2>&1; return $?
  fi
  "$@" >"$out" 2>&1 &
  local pid=$!
  ( sleep "$cap"; kill -TERM "$pid" 2>/dev/null ) &
  local watcher=$!
  local rc=0
  wait "$pid" 2>/dev/null || rc=124
  # Reap the watchdog AND its lingering `sleep` child (else a slept timer
  # outlives a fast run — noticeable on macOS where the fallback is the live path).
  pkill -P "$watcher" 2>/dev/null || true
  kill "$watcher" 2>/dev/null || true
  wait "$watcher" 2>/dev/null || true
  return $rc
}

# last "Import operation took X s" -> seconds (the rule/last import isolates
# reasoning when data was imported first).
rdfox_import_secs() {  # <log> [first|last]
  local which="${2:-last}"
  if [[ "$which" == first ]]; then
    grep -oE 'Import operation took [0-9.]+' "$1" | head -1 | awk '{print $4}'
  else
    grep -oE 'Import operation took [0-9.]+' "$1" | tail -1 | awk '{print $4}'
  fi
}

# "Aggregate number of <label> facts : N" -> integer (commas stripped).
rdfox_facts() {  # <log> <label: all|explicit>
  grep "Aggregate number of $2 facts" "$1" | grep -oE '[0-9,]+' | head -1 | tr -d ','
}

# pull a numeric field out of horndb-bench's one-line JSON.
hb_field() { sed -E "s/.*\"$2\":([0-9.eE+-]+).*/\1/" <<<"$1"; }

# integer/float helpers via awk.
fdiv() { awk -v a="$1" -v b="$2" 'BEGIN{ if(b==0){print "inf"}else{printf "%.2f", a/b} }'; }
fmt()  { awk -v x="$1" 'BEGIN{ if(x>=1e6)printf "%.2fM",x/1e6; else if(x>=1e3)printf "%.1fk",x/1e3; else printf "%.0f",x }'; }

ROWS=()    # collected "label|metric|horndb|rdfox|ratio|goal|verdict"
DETAIL=()  # per-workload size / closure-count cross-check lines

# ----------------------------------------------------------------------------
# 1. bulk import throughput (SPEC-02 F8 >= 1 M tps; RDFox baseline)
# ----------------------------------------------------------------------------
import_compare() {
  local data
  # Prefer the real LUBM(1) sample RDFox ships; else a generated chain.
  if [[ -f "$RDFOX_HOME/examples/data/lubm1.ttl" ]]; then
    data="$WORK/import.nt"; cp "$RDFOX_HOME/examples/data/lubm1.ttl" "$data"
  else
    python3 "$SCRIPT_DIR/gen_workload.py" chain 200000 "$WORK/import.nt"; data="$WORK/import.nt"
  fi

  echo ">> [import] HornDB" >&2
  local hj; hj="$($HB import --data "$data")"; echo "$hj" >"$LOGS/import.horndb.json"
  local h_tps; h_tps="$(hb_field "$hj" tps)"
  local n; n="$(hb_field "$hj" input_triples)"

  echo ">> [import] RDFox" >&2
  cp "$data" "$WORK/import-rdfox.nt"
  run_rdfox "$WORK" 'dstore create default' 'import import-rdfox.nt' 'info' >"$LOGS/import.rdfox.log"
  local r_secs r_tps; r_secs="$(rdfox_import_secs "$LOGS/import.rdfox.log" first)"
  r_tps="$(awk -v n="$n" -v s="$r_secs" 'BEGIN{ if(s>0)printf "%.0f", n/s; else print 0 }')"

  local ratio; ratio="$(fdiv "$h_tps" "$r_tps")"
  local verdict="—"; awk -v t="$h_tps" 'BEGIN{exit !(t>=1e6)}' && verdict="PASS (>=1M)" || verdict="below 1M"
  ROWS+=("import|tps|$(fmt "$h_tps")/s|$(fmt "$r_tps")/s|${ratio}x|>=1M tps|$verdict")
  DETAIL+=("import     : $(fmt "$n") triples loaded (no reasoning)")
}

# ----------------------------------------------------------------------------
# 2. transitive closure (SPEC-05 #1 >= 10x RDFox on a chain)
# ----------------------------------------------------------------------------
transitive_compare() {
  local nt="$WORK/chain.nt" pred
  python3 "$SCRIPT_DIR/gen_workload.py" chain "$CHAIN_N" "$nt"
  pred="$(python3 "$SCRIPT_DIR/gen_workload.py" predicate)"

  echo ">> [transitive] HornDB" >&2
  local hj; hj="$($HB transitive --data "$nt" --predicate "$pred")"; echo "$hj" >"$LOGS/transitive.horndb.json"
  local h_build h_reason h_ms h_edges
  h_build="$(hb_field "$hj" build_ms)"; h_reason="$(hb_field "$hj" reason_ms)"
  h_ms="$(awk -v a="$h_build" -v b="$h_reason" 'BEGIN{printf "%.3f", a+b}')"
  h_edges="$(hb_field "$hj" closure_edges)"

  echo ">> [transitive] RDFox" >&2
  # transitivity rule for this predicate
  printf '[?x, <%s>, ?z] :- [?x, <%s>, ?y], [?y, <%s>, ?z] .\n' "$pred" "$pred" "$pred" >"$WORK/trans.dlog"
  # import data first (no rules), THEN the rule -> the rule-import time is the
  # materialization time only.
  run_rdfox "$WORK" 'dstore create default' 'import chain.nt' 'import trans.dlog' 'info' >"$LOGS/transitive.rdfox.log"
  local r_secs r_facts r_ms
  r_secs="$(rdfox_import_secs "$LOGS/transitive.rdfox.log" last)"   # rule import = reasoning
  r_ms="$(awk -v s="$r_secs" 'BEGIN{printf "%.3f", s*1000}')"
  r_facts="$(rdfox_facts "$LOGS/transitive.rdfox.log" all)"

  local ratio verdict
  ratio="$(fdiv "$r_ms" "$h_ms")"   # how many x faster HornDB is
  awk -v r="$ratio" 'BEGIN{exit !(r+0>=10)}' && verdict="PASS (>=10x)" || verdict="below 10x"
  ROWS+=("transitive|reason ms|${h_ms}|${r_ms}|${ratio}x|>=10x faster|$verdict")
  local match="EXACT"; [[ "$h_edges" != "$r_facts" ]] && match="differ"
  DETAIL+=("transitive : chain ${CHAIN_N} nodes; closure $(fmt "$h_edges") / $(fmt "$r_facts") facts (${match})")
}

# ----------------------------------------------------------------------------
# 3. OWL 2 RL materialization (Stage-1 gate: within 3x RDFox)
#    Indicative: each engine runs its own OWL 2 RL closure. HornDB also injects
#    a fixed XSD datatype-lattice base (~60 triples), so totals differ slightly;
#    we print both counts. At these sizes the offset is < 0.05%.
# ----------------------------------------------------------------------------
materialize_compare() {
  local nt="$WORK/tax.nt"
  python3 "$SCRIPT_DIR/gen_workload.py" taxonomy "$TAX_DEPTH" "$TAX_INSTANCES" "$nt"

  echo ">> [materialize] HornDB" >&2
  local hj; hj="$($HB materialize --data "$nt")"; echo "$hj" >"$LOGS/materialize.horndb.json"
  local h_ms h_total h_inf
  h_ms="$(hb_field "$hj" reason_ms)"; h_total="$(hb_field "$hj" total)"; h_inf="$(hb_field "$hj" inferred)"

  echo ">> [materialize] RDFox" >&2
  cp "$SCRIPT_DIR/rules/owl2rl-core.dlog" "$WORK/owl2rl-core.dlog"
  run_rdfox "$WORK" 'dstore create default' 'import tax.nt' 'import owl2rl-core.dlog' 'info' >"$LOGS/materialize.rdfox.log"
  local r_secs r_ms r_total
  r_secs="$(rdfox_import_secs "$LOGS/materialize.rdfox.log" last)"  # rule import = reasoning
  r_ms="$(awk -v s="$r_secs" 'BEGIN{printf "%.3f", s*1000}')"
  r_total="$(rdfox_facts "$LOGS/materialize.rdfox.log" all)"

  local ratio verdict
  ratio="$(fdiv "$h_ms" "$r_ms")"   # HornDB time / RDFox time
  awk -v r="$ratio" 'BEGIN{exit !(r+0<=3.0)}' && verdict="PASS (within 3x)" || verdict="over 3x"
  ROWS+=("materialize|reason ms|${h_ms}|${r_ms}|${ratio}x|within 3x|$verdict")
  local delta=$(( h_total - r_total ))
  DETAIL+=("materialize: taxonomy d=${TAX_DEPTH} inst=${TAX_INSTANCES}; closure $(fmt "$h_total") / $(fmt "$r_total") facts (HornDB +${delta} XSD axioms)")
}

# ----------------------------------------------------------------------------
# 4. LUBM-N OWL 2 RL materialization (Stage-1 gate: within 3x RDFox).
#    Same TBox + same ABox + SAME RULES (generated from rules.toml) through both
#    engines. Hard closure-count parity gate. HornDB capped at CAP_SECONDS.
# ----------------------------------------------------------------------------
lubm_compare() {
  local n="$1"
  local lubmdir="$OUTDIR/lubm/$n"
  if [[ ! -f "$lubmdir/abox.nt" || ! -f "$lubmdir/tbox.nt" ]]; then
    echo ">> [lubm] generating LUBM-$n data" >&2
    "$SCRIPT_DIR/gen_lubm.sh" --universities "$n" --out "$lubmdir" >&2
  fi

  # Matched ruleset, freshly generated from rules.toml each run (no drift).
  python3 "$SCRIPT_DIR/gen_ruleset.py" >"$WORK/owl2rl-horndb-subset.dlog"

  echo ">> [lubm] HornDB materialize (cap ${CAP_SECONDS}s)" >&2
  local capped=0 hj="" h_ms="—" h_total=""
  if cap_run "$CAP_SECONDS" "$LOGS/lubm.horndb.json" \
        "$HB" materialize --data "$lubmdir/tbox.nt" --data "$lubmdir/abox.nt"; then
    hj="$(cat "$LOGS/lubm.horndb.json")"
    h_ms="$(hb_field "$hj" reason_ms)"; h_total="$(hb_field "$hj" total)"
  else
    capped=1
    echo ">> [lubm] HornDB did not finish within ${CAP_SECONDS}s" >&2
  fi

  echo ">> [lubm] RDFox materialize (matched ruleset)" >&2
  cp "$lubmdir/tbox.nt" "$lubmdir/abox.nt" "$WORK/owl2rl-horndb-subset.dlog" "$WORK/"
  # Data first (tbox + abox), rules LAST -> last import time = reasoning only.
  run_rdfox "$WORK" 'dstore create default' 'import tbox.nt' 'import abox.nt' \
            'import owl2rl-horndb-subset.dlog' 'info' >"$LOGS/lubm.rdfox.log"
  local r_secs r_ms r_total
  r_secs="$(rdfox_import_secs "$LOGS/lubm.rdfox.log" last)"
  r_ms="$(awk -v s="$r_secs" 'BEGIN{printf "%.3f", s*1000}')"
  r_total="$(rdfox_facts "$LOGS/lubm.rdfox.log" all)"

  # --- closure-count parity gate ---
  # HornDB carries a fixed XSD datatype base, so HornDB total = RDFox + offset.
  # PASS iff 0 <= (h_total - r_total) <= XSD_OFFSET_MAX. HornDB having FEWER
  # facts means the ruleset translation dropped a rule -> hard FAIL.
  # HornDB's fixed XSD datatype base is ~60 axioms; 512 is generous headroom.
  # Task 5 (N=1) measures the exact offset and may tighten this.
  local XSD_OFFSET_MAX=512
  local parity ratio verdict
  if [[ "$capped" -eq 1 ]]; then
    parity="n/a (capped)"
    ratio="—"
    verdict="DID NOT COMPLETE within ${CAP_SECONDS}s"
  else
    local delta=$(( h_total - r_total ))
    if [[ "$delta" -ge 0 && "$delta" -le "$XSD_OFFSET_MAX" ]]; then
      parity="OK (HornDB +${delta})"
    else
      parity="MISMATCH (delta=${delta}) — ruleset translation suspect"
    fi
    ratio="$(fdiv "$h_ms" "$r_ms")"
    if [[ "$parity" == MISMATCH* ]]; then
      verdict="PARITY FAIL"
    else
      awk -v r="$ratio" 'BEGIN{exit !(r+0<=3.0)}' && verdict="PASS (within 3x)" || verdict="over 3x"
    fi
  fi

  ROWS+=("lubm-$n|reason ms|${h_ms}|${r_ms}|${ratio}x|within 3x|$verdict")
  DETAIL+=("lubm-$n     : LUBM($n); closure ${h_total:-—} / ${r_total} facts; parity ${parity}")
}

# ----------------------------------------------------------------------------
if [[ "$LUBM_N" -gt 0 ]]; then
  lubm_compare "$LUBM_N"
else
  import_compare
  transitive_compare
  materialize_compare
fi

# --- where results are STORED (gitignored — RDFox numbers must never be
#     committed to a public repo; that counts as "publishing"). ---
RESULTS_DIR="${RESULTS_DIR:-$REPO_ROOT/scripts/bench/results}"
RUN_DIR="$RESULTS_DIR/run-$(date '+%Y%m%d-%H%M%S')"
mkdir -p "$RUN_DIR"
# Safety net: refuse to leave RDFox numbers anywhere git would track them.
if git -C "$REPO_ROOT" check-ignore -q "$RUN_DIR"; then
  :
else
  echo "WARNING: results dir is NOT gitignored — RDFox numbers must not be committed:" >&2
  echo "         $RUN_DIR" >&2
  echo "         set RESULTS_DIR to a gitignored path, or do not 'git add' these files." >&2
fi

print_report() {
  echo "================================================================================"
  echo "  HornDB vs RDFox — internal comparison ($(date '+%Y-%m-%d %H:%M'))"
  echo "  RDFox $("$RDFOX_BIN" sandbox "$WORK" 'quit' 2>/dev/null | grep -oE 'licensed for [A-Za-z]+ use' | head -1 || true)"
  echo "  INTERNAL ONLY — do not publish these numbers (RDFox licence / DeWitt clause)."
  echo "================================================================================"
  printf '%-42s %-9s %-12s %-12s %-8s %-14s %s\n' "workload" "metric" "HornDB" "RDFox" "ratio" "goal" "verdict"
  printf '%-42s %-9s %-12s %-12s %-8s %-14s %s\n' "------------------------------------------" "---------" "------------" "------------" "--------" "--------------" "-------"
  for row in "${ROWS[@]}"; do
    IFS='|' read -r label metric h r ratio goal verdict <<<"$row"
    printf '%-42s %-9s %-12s %-12s %-8s %-14s %s\n' "$label" "$metric" "$h" "$r" "$ratio" "$goal" "$verdict"
  done
  echo
  echo "workloads & closure cross-check (HornDB / RDFox total facts):"
  for d in "${DETAIL[@]}"; do echo "  $d"; done
  echo
  echo "raw HornDB JSON:"
  cat "$LOGS"/*.json 2>/dev/null | sed 's/^/  /'
  echo
  echo "notes:"
  echo "  * transitive/materialize 'reason ms' isolates reasoning: data is imported"
  echo "    first, so the RDFox rule-import time is materialization-only."
  echo "  * HornDB transitive 'reason' = matrix build + GraphBLAS closure compute;"
  echo "    RDFox materializes triples into its store (broader work). See README."
  echo "  * materialize totals differ by HornDB's injected XSD datatype axioms;"
  echo "    the comparison is indicative, not a closure-equality check."
}

echo
print_report | tee "$RUN_DIR/summary.txt"
# Persist the full raw evidence alongside the summary (also gitignored).
cp "$LOGS"/*.json "$LOGS"/*.log "$RUN_DIR/" 2>/dev/null || true

echo
echo "results stored (gitignored): $RUN_DIR/"

if [[ "$KEEP" -eq 0 ]]; then
  rm -f "$WORK"/*.nt "$WORK"/*.dlog 2>/dev/null || true
fi
