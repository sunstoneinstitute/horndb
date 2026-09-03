#!/usr/bin/env bash
#
# SPEC-15 fix #2 (HDB-40, #134): naïve vs semi-naïve compiled-rule firing,
# same binary, same corpora. Driven on hornbench by `.github/workflows/bench.yml`:
#
#   gh workflow run bench.yml --ref <branch> -f script=scripts/bench/seminaive-ab.sh
#
# Corpora: LUBM-N for each N in $LUBM_N (default "1 10"), obtained by
# `get_lubm.sh` (generated where a JDK exists, else downloaded from the
# `bench-corpora` release), plus the taxonomy d=12/40k corpus the audit-pass
# lubm leg uses. For each corpus and each closure backend, run `horndb-bench
# materialize --firing naive|semi-naive` three times and record the medians,
# including peak RSS of the process. Parity: dump the closure under both
# strategies and `cmp` the sorted files.
#
# Output: bench-out/seminaive.log and bench-out/SUMMARY.md (the workflow appends
# the summary to the job summary and uploads bench-out/).

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"
OUT="$REPO_ROOT/bench-out"
mkdir -p "$OUT"
SUMMARY="$OUT/SUMMARY.md"
LOG="$OUT/seminaive.log"
: > "$LOG"

PERSIST="${BENCH_PERSIST:-/home/bench/horndb-bench}"
mkdir -p "$PERSIST" 2>/dev/null || PERSIST="$REPO_ROOT/target/bench-persist"
mkdir -p "$PERSIST"

{
  echo "# Semi-naïve firing A/B (HDB-40)"
  echo
  echo "- commit: \`$(git rev-parse --short HEAD)\`"
  echo "- date (UTC): $(date -u +%Y-%m-%d)"
  echo "- host: \`$(hostname)\` — $(nproc) cores, $(uname -sr)"
  echo
  echo "| corpus | backend | firing | reason_ms | compiled_rules_ms | apply_ms | rounds | inferred | peak_rss_mib |"
  echo "|---|---|---|---|---|---|---|---|---|"
} > "$SUMMARY"

cargo build --release -p horndb-bench-rdfox --bin horndb-bench || exit 1
hb="$REPO_ROOT/target/release/horndb-bench"

# Ordered corpus list (name + the --data args it needs), so the summary rows
# come out in a stable order run to run.
NAMES=()
declare -A CORPUS
for n in ${LUBM_N:-1 10}; do
  dir="$PERSIST/lubm/$n"
  if scripts/bench/get_lubm.sh "$n" "$dir" 2>&1 | tee -a "$LOG"; then
    NAMES+=("lubm-$n")
    CORPUS[lubm-$n]="--data $dir/tbox.nt --data $dir/abox.nt"
  else
    echo ">> LUBM-$n unavailable (no JDK, and the corpus download failed); skipping" | tee -a "$LOG"
  fi
done
tax="$PERSIST/taxonomy/abox.nt"
mkdir -p "$PERSIST/taxonomy"
[ -f "$tax" ] || python3 scripts/bench/gen_workload.py taxonomy 12 40000 "$tax" || exit 1
NAMES+=(taxonomy-d12-40k)
CORPUS[taxonomy-d12-40k]="--data $tax"

median() { sort -g | awk '{a[NR]=$1} END{if(NR)print a[int((NR+1)/2)]}'; }
field() { grep -o "\"$1\":[0-9.]*" | cut -d: -f2; }

# Run the bench binary once, appending `"peak_rss_kib":<n>` to its JSON line.
# `/usr/bin/time` is not installed on every runner; python3 is (gen_workload.py
# needs it), and getrusage(RUSAGE_CHILDREN) gives the child's high-water mark.
run_once() {
  python3 -c 'import resource, subprocess, sys
p = subprocess.run(sys.argv[1:], capture_output=True, text=True)
sys.stderr.write(p.stderr)
if p.returncode:
    sys.exit(p.returncode)
rss = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
print(p.stdout.strip().rstrip("}") + ",\"peak_rss_kib\":%d}" % rss)' "$@"
}

for corpus in "${NAMES[@]}"; do
  # shellcheck disable=SC2206
  args=(${CORPUS[$corpus]})
  for backend in rulefiring graphblas; do
    for firing in naive semi-naive; do
      runs=""
      for i in 1 2 3; do
        line=$(run_once "$hb" materialize --backend "$backend" --firing "$firing" "${args[@]}") || exit 1
        echo "$corpus $backend $firing run$i: $line" >> "$LOG"
        runs+="$line"$'\n'
      done
      row=""
      for k in reason_ms compiled_rules_ms apply_ms rounds inferred; do
        row+="$(printf '%s' "$runs" | field "$k" | median) | "
      done
      rss_kib=$(printf '%s' "$runs" | field peak_rss_kib | median)
      row+="$(awk -v k="${rss_kib:-0}" 'BEGIN{printf "%.0f", k/1024}') | "
      echo "| $corpus | $backend | $firing | ${row% | } |" >> "$SUMMARY"
    done
  done
  # Parity: identical closure under both strategies (graphblas backend).
  "$hb" materialize --backend graphblas --firing naive --dump-nt "$OUT/$corpus-naive.nt" "${args[@]}" >/dev/null || exit 1
  "$hb" materialize --backend graphblas --firing semi-naive --dump-nt "$OUT/$corpus-semi.nt" "${args[@]}" >/dev/null || exit 1
  if cmp -s <(sort "$OUT/$corpus-naive.nt") <(sort "$OUT/$corpus-semi.nt"); then
    echo "| $corpus | graphblas | parity | identical closure ($(wc -l < "$OUT/$corpus-semi.nt") triples) | | | | | |" >> "$SUMMARY"
  else
    echo "| $corpus | graphblas | parity | **CLOSURES DIFFER** | | | | | |" >> "$SUMMARY"
  fi
  rm -f "$OUT/$corpus-naive.nt" "$OUT/$corpus-semi.nt"
done

cat "$SUMMARY"
