#!/usr/bin/env bash
#
# stage1-acceptance.sh — SPEC-25 S6 (HDB-61): run the three deferred SPEC-02
# Stage-1 acceptance measurements on a LUBM-scale corpus.
#
#   2. LUBM-8000 N-Triples import wall clock, target <=30 min
#   3. LUBM-8000 fully-warm footprint (Store::report_footprint()), target <=55 GB
#   4. rdf:type partition scan vs. STREAM Triad measured on this host, target >=80%
#
# Generating LUBM-8000 (~1.1 B triples) may not fit in one 240-minute CI job
# alongside the measurement itself, so staging and measuring are separate:
#
#   --stage-only   fetch/generate the corpus into the persistent bench dir
#                  and stop, reporting how long that took and how big it is.
#   (default)      reuse a corpus already staged there, then run the bench.
#
# Usage: stage1-acceptance.sh [--universities N] [--stage-only]
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

OUT="$REPO_ROOT/bench-out"
mkdir -p "$OUT"
SUMMARY="$OUT/SUMMARY.md"
RAW="$OUT/raw.txt"

UNIV=8000
STAGE_ONLY=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --universities) UNIV="$2"; shift 2 ;;
    --stage-only)   STAGE_ONLY=1; shift ;;
    -h|--help)      sed -n '2,20p' "$0"; exit 0 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

BENCH_DIR="${BENCH_DIR:-/home/bench/horndb-bench}"
CORPUS_DIR="$BENCH_DIR/lubm-$UNIV"

echo ">> staging LUBM-$UNIV into $CORPUS_DIR" >&2
stage_start=$(date +%s)
"$REPO_ROOT/scripts/bench/get_lubm.sh" "$UNIV" "$CORPUS_DIR" \
  || { echo "::error::could not stage LUBM-$UNIV" >&2; exit 1; }
stage_seconds=$(( $(date +%s) - stage_start ))
corpus_bytes=$(du -sb "$CORPUS_DIR" | cut -f1)

if [[ "$STAGE_ONLY" -eq 1 ]]; then
  {
    echo "# LUBM-$UNIV staging (SPEC-25 S6)"
    echo
    echo "- corpus dir: \`$CORPUS_DIR\`"
    echo "- staging wall clock: ${stage_seconds}s"
    echo "- corpus size: $(numfmt --to=iec-i --suffix=B "$corpus_bytes" 2>/dev/null || echo "$corpus_bytes bytes")"
    echo
    echo "Not measured — this was a \`--stage-only\` run. Dispatch again without"
    echo "\`--stage-only\` to run the bench against this corpus."
  } | tee "$SUMMARY"
  exit 0
fi

[[ -s "$CORPUS_DIR/abox.nt" ]] || { echo "::error::$CORPUS_DIR/abox.nt missing — stage it first with --stage-only" >&2; exit 1; }

RUNNER=()
if command -v numactl >/dev/null; then
  RUNNER=(numactl --cpunodebind=0 --membind=0)
fi

echo ">> cargo bench -p horndb-storage --bench stage1_acceptance against $CORPUS_DIR/abox.nt" >&2
export LUBM_NT="$CORPUS_DIR/abox.nt"
export LUBM_TBOX="$CORPUS_DIR/tbox.nt"
"${RUNNER[@]}" cargo bench -p horndb-storage --bench stage1_acceptance 2>&1 | tee "$OUT/stage1-acceptance.log"
status=${PIPESTATUS[0]}
[[ "$status" -eq 0 ]] || { echo "::error::stage1_acceptance exited $status" >&2; exit 1; }

grep '^\[s1\]' "$OUT/stage1-acceptance.log" > "$RAW"
[[ -s "$RAW" ]] || { echo "::error::no [s1] lines in the bench output" >&2; exit 1; }

python3 - "$RAW" "$UNIV" "$CORPUS_DIR" "$(git rev-parse --short HEAD)" "$(hostname)" > "$SUMMARY" <<'PY'
import sys

raw, univ, corpus_dir, commit, host = sys.argv[1:6]
kv = {}
for line in open(raw):
    line = line.strip()
    if not line.startswith("[s1] ") or "=" not in line:
        continue
    key, val = line[len("[s1] "):].split("=", 1)
    kv[key] = val

def f(key, default="?"):
    return kv.get(key, default)

import subprocess
kernel = subprocess.run(["uname", "-sr"], capture_output=True, text=True).stdout.strip()
cpu = ""
try:
    with open("/proc/cpuinfo") as fh:
        for line in fh:
            if line.startswith("model name"):
                cpu = line.split(":", 1)[1].strip()
                break
except OSError:
    pass
ram = ""
try:
    with open("/proc/meminfo") as fh:
        for line in fh:
            if line.startswith("MemTotal"):
                ram = line.split(":", 1)[1].strip()
                break
except OSError:
    pass

print(f"# SPEC-25 S6 — LUBM-{univ} Stage-1 acceptance (HDB-61)\n")
print(f"- host: `{host}`  kernel: `{kernel}`")
print(f"- cpu: `{cpu}`  ram: `{ram}`")
print(f"- commit: `{commit}`")
print(f"- corpus: `{corpus_dir}` — {f('triples')} triples imported, {f('input_bytes')} bytes of N-Triples\n")

print("| # | criterion | target | measured | verdict |")
print("|---|---|---|---|---|")
print(f"| 2 | LUBM-{univ} import wall clock | <= 30 min | "
      f"{float(f('import_seconds', 0)):.1f}s ({float(f('triples_per_sec', 0)):.0f} triples/s) | {f('verdict_acceptance2')} |")
print(f"| 3 | LUBM-{univ} fully-warm footprint | <= 55 GB | "
      f"{float(f('footprint_bytes', 0))/1e9:.2f} GB ({float(f('footprint_bytes_per_triple', 0)):.1f} B/triple); "
      f"peak RSS {float(f('peak_rss_bytes', 0))/1e9:.2f} GB | {f('verdict_acceptance3')} |")
print(f"| 4 | rdf:type scan vs. STREAM Triad | >= 80% | "
      f"{float(f('rdf_type_scan_gb_per_s', 0)):.2f} GB/s / {float(f('triad_nt_gb_per_s', 0)):.2f} GB/s "
      f"= {float(f('scan_over_triad_nt', 0))*100:.1f}% | {f('verdict_acceptance4')} |")
print()
print("Triad detail (same run, same host): "
      f"single-thread {float(f('triad_1t_gb_per_s', 0)):.2f} GB/s, "
      f"full-socket {float(f('triad_nt_gb_per_s', 0)):.2f} GB/s. "
      f"rdf:type scan is {float(f('scan_over_triad_1t', 0))*100:.1f}% of single-thread Triad, "
      f"{float(f('scan_over_triad_nt', 0))*100:.1f}% of full-socket Triad "
      f"({f('rdf_type_rows')} rows, {f('rdf_type_scan_bytes')} bytes).")
print()
print("A MISS is a measured result, not a script failure — see the honesty clause")
print("in `docs/specs/SPEC-25-storage-stage2.md` S6 before recording it.")
PY

echo ">> summary:" >&2
cat "$SUMMARY"
