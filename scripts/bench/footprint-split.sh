#!/usr/bin/env bash
#
# footprint-split.sh — trainmarks xlarge serving-footprint split (HDB-146).
#
# HDB-144 measured the isolated serving footprint at 625 B/triple while the
# bulk-import peak over the same corpus is 140 B/triple. Nothing said where the
# difference went. `bench-trainmarks --mem-only` now prints a per-component
# split next to RSS (partitions, dictionary keys/terms/index, query snapshots,
# planner stats) plus the unattributed residual; this script runs that and
# turns the `[mem] ...` stderr lines into a table.
#
# Reuses the persistent trainmarks corpus on the bench host (the Actions
# checkout is wiped between runs), same convention as exec-phases.sh. Run it
# via .github/workflows/bench.yml:
#   gh workflow run bench.yml --ref <branch> -f script=scripts/bench/footprint-split.sh
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

OUT="$REPO_ROOT/bench-out"
mkdir -p "$OUT"
LOG="$OUT/footprint-split.log"
SUMMARY="$OUT/SUMMARY.md"

PERSIST="${BENCH_PERSIST:-/home/bench/horndb-bench}"
mkdir -p "$PERSIST" 2>/dev/null || PERSIST="$REPO_ROOT/target/bench-persist"
WORK="${TRAINMARKS_DIR:-$PERSIST/trainmarks}"

# Same pin as trainmarks.sh: every trainmarks number in docs/benchmarks.md was
# measured at one parse thread, and `auto` is host-dependent on top of that.
export HORNDB_LOAD_THREADS="${HORNDB_LOAD_THREADS:-1}"

if [ ! -d "$WORK/data" ]; then
  echo ">> no corpus at $WORK/data — generating via trainmarks.sh conventions" >&2
  mkdir -p "$WORK/queries"
  cp "$REPO_ROOT/scripts/bench/trainmarks/generate_data.py" "$WORK/generate_data.py"
  cp "$REPO_ROOT/scripts/bench/trainmarks/queries/"*.rq "$WORK/queries/"
  ( cd "$WORK" && python3 generate_data.py ) || exit 1
fi

echo ">> building bench-trainmarks (release)" >&2
cargo build --release -p horndb-bench-trainmarks || exit 1

# `vec` is the default read path (six-ordering `VecTripleSource`); `direct`
# reads the partitions in place. HDB-144 measured both, so split both — the
# direct source itself is not attributed (its leaves can be `Arc`-clones of the
# partitions' columns, so counting them would double-count), which shows up as
# a larger residual in that mode.
: > "$LOG"
for mode in vec direct; do
  echo "===== MODE=$mode =====" | tee -a "$LOG" >&2
  if [ "$mode" = direct ]; then export HORNDB_DIRECT_SOURCE=1; else unset HORNDB_DIRECT_SOURCE; fi
  "$REPO_ROOT/target/release/bench-trainmarks" \
    --data-dir "$WORK/data" --queries-dir "$WORK/queries" \
    --scale xlarge --out "$OUT/footprint-split-$mode.json" \
    --timeout-secs 1800 --mem-only 2>>"$LOG" \
    || { cat "$LOG" >&2; echo "::error::bench-trainmarks ($mode) failed" >&2; exit 1; }
done
cat "$LOG" >&2

python3 - "$LOG" "$(git rev-parse --short HEAD)" "$(hostname)" > "$SUMMARY" <<'PY'
import re, sys, collections

log, commit, host = sys.argv[1], sys.argv[2], sys.argv[3]
mode = None
# mode -> [(label, mib, pct, bpt)], plus the footprint line per mode.
rows = collections.OrderedDict()
marks = collections.OrderedDict()
foot = {}
comp = re.compile(r"\[mem\] (.+?): ([\d.]+) MiB \(([\d.]+)% of RSS, ([\d.]+) B/triple\)")
timeline = re.compile(r"\[mem\] after (\S+): RSS ([\d.]+) MiB, peak ([\d.]+) MiB")
unattr = re.compile(r"\[mem\] attributed: ([\d.]+) MiB; unattributed[^:]*: ([\d.]+) MiB \(([\d.]+)% of RSS\)")
serving = re.compile(r"\[mem\] serving footprint[^:]*: RSS ([\d.]+) MiB over (\d+) triples = ([\d.]+) B/triple")
for line in open(log, errors="replace"):
    m = re.search(r"===== MODE=(\S+) =====", line)
    if m:
        mode = m.group(1)
        rows.setdefault(mode, [])
        continue
    if mode is None:
        continue
    if m := timeline.search(line):
        marks.setdefault(mode, []).append((m.group(1), float(m.group(2)), float(m.group(3))))
        continue
    if m := comp.search(line):
        rows[mode].append((m.group(1), float(m.group(2)), float(m.group(3)), float(m.group(4))))
        continue
    if m := unattr.search(line):
        rows[mode].append(("**unattributed**", float(m.group(2)), float(m.group(3)), None))
        continue
    if m := serving.search(line):
        foot[mode] = (float(m.group(1)), int(m.group(2)), float(m.group(3)))

if not rows or not foot:
    sys.exit(f"no [mem] component lines parsed from {log}")

print("# trainmarks xlarge serving-footprint split (HDB-146)\n")
print(f"- commit: `{commit}`  host: `{host}`")
print("- `bench-trainmarks --mem-only`, one loaded store + one warm query snapshot")
print("- residual = RSS minus the sum of the self-accounting components\n")
for mode, comps in rows.items():
    if mode not in foot:
        continue
    rss, triples, bpt = foot[mode]
    print(f"## {mode} source — RSS {rss:.0f} MiB over {triples} triples = {bpt:.1f} B/triple\n")
    print("| component | MiB | % of RSS | B/triple |")
    print("|---|---:|---:|---:|")
    for label, mib, pct, per in comps:
        print(f"| {label} | {mib:.0f} | {pct:.1f}% | " + ("—" if per is None else f"{per:.1f}") + " |")
    print()
    if marks.get(mode):
        print("RSS timeline (where the residual is acquired):\n")
        print("| after | RSS MiB | peak MiB |")
        print("|---|---:|---:|")
        for label, r, p in marks[mode]:
            print(f"| {label} | {r:.0f} | {p:.0f} |")
        print()
PY

echo ">> summary:" >&2
cat "$SUMMARY"
