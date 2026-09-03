#!/usr/bin/env bash
#
# exec-phases.sh — trainmarks xlarge exec-phase sweep (HDB-99 / HDB-109).
#
# Runs `bench-trainmarks` at xlarge with `HORNDB_EXEC_PHASES=1` and turns the
# driver's cumulative `[exec-phases after <label>]` stderr dumps into a
# per-query phase table. Reproduces the HDB-99 measurement pass, which was
# done by hand; HDB-109 needs it again after adding phases, so it is a script.
#
# A query's own share is the diff between an *adjacent pair of that query's
# own* dumps — `qN_pre` -> `qN_cold`, and `qN_warm_pre` -> `qN_warm`. The
# counters are process-wide and cumulative, so any other pairing folds in
# unrelated work (see `dump_exec_phases`'s doc comment in the driver).
#
# `exec` is not read from the wall clock: `phases::flush` derives `residual`
# as `exec_elapsed - sum(named)`, so the phase totals of one query already
# sum to that query's `exec`. Percentages are of that sum.
#
# Reuses the persistent trainmarks corpus on the bench host (the Actions
# checkout is wiped between runs), same convention as audit-pass.sh. Run it
# via .github/workflows/bench.yml:
#   gh workflow run bench.yml --ref <branch> \
#     -f script=scripts/bench/exec-phases.sh -f env='HORNDB_EXEC_PHASES=1'
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

OUT="$REPO_ROOT/bench-out"
mkdir -p "$OUT"
LOG="$OUT/exec-phases.log"
SUMMARY="$OUT/SUMMARY.md"

PERSIST="${BENCH_PERSIST:-/home/bench/horndb-bench}"
mkdir -p "$PERSIST" 2>/dev/null || PERSIST="$REPO_ROOT/target/bench-persist"
WORK="${TRAINMARKS_DIR:-$PERSIST/trainmarks}"

# Same pin as trainmarks.sh: every trainmarks number in docs/benchmarks.md was
# measured at one parse thread, and `auto` is host-dependent on top of that.
export HORNDB_LOAD_THREADS="${HORNDB_LOAD_THREADS:-1}"
export HORNDB_EXEC_PHASES="${HORNDB_EXEC_PHASES:-1}"

if [ ! -d "$WORK/data" ]; then
  echo ">> no corpus at $WORK/data — generating via trainmarks.sh conventions" >&2
  mkdir -p "$WORK/queries"
  cp "$REPO_ROOT/scripts/bench/trainmarks/generate_data.py" "$WORK/generate_data.py"
  cp "$REPO_ROOT/scripts/bench/trainmarks/queries/"*.rq "$WORK/queries/"
  ( cd "$WORK" && python3 generate_data.py ) || exit 1
fi

echo ">> building bench-trainmarks (release)" >&2
cargo build --release -p horndb-bench-trainmarks || exit 1

echo ">> running trainmarks xlarge with HORNDB_EXEC_PHASES=$HORNDB_EXEC_PHASES" >&2
"$REPO_ROOT/target/release/bench-trainmarks" \
  --data-dir "$WORK/data" \
  --queries-dir "$WORK/queries" \
  --scale xlarge \
  --out "$OUT/exec-phases-results.json" \
  --timeout-secs 1800 2>"$LOG"
# Straight redirect, not `2> >(tee ...)`: bash does not wait for a process
# substitution to finish, so the parser below could read a truncated log.
cat "$LOG" >&2

python3 - "$LOG" "$(git rev-parse --short HEAD)" > "$SUMMARY" <<'PY'
import re, sys, collections

log, commit = sys.argv[1], sys.argv[2]
# label -> {phase: ns}. One entry per `[exec-phases after <label>]` dump.
dumps = collections.OrderedDict()
label = None
pat = re.compile(r'horndb_sparql_exec_phase_nanoseconds_total\{[^}]*phase="([^"]+)"[^}]*\}\s+(\d+)')
for line in open(log, errors="replace"):
    m = re.search(r'\[exec-phases after (\S+)\]', line)
    if m:
        label = m.group(1)
        dumps.setdefault(label, {})
        continue
    m = pat.search(line)
    if m and label is not None:
        dumps[label][m.group(1)] = int(m.group(2))

def diff(pre, post):
    a, b = dumps.get(pre), dumps.get(post)
    if a is None or b is None:
        return None
    d = {k: b.get(k, 0) - a.get(k, 0) for k in set(a) | set(b)}
    return {k: v for k, v in d.items() if v > 0}

queries = sorted({m.group(1) for m in (re.match(r'(q\d+)_pre$', l) for l in dumps) if m})

print(f"# trainmarks xlarge exec-phase split (`HORNDB_EXEC_PHASES=1`)\n")
print(f"- commit: `{commit}`")
print(f"- pairs diffed: `qN_pre` -> `qN_cold` (cold), `qN_warm_pre` -> `qN_warm` (warm)")
print(f"- `exec` = sum of all phases incl. `residual` (that is how `flush` derives it)\n")

for kind, pre_s, post_s in (("cold", "{q}_pre", "{q}_cold"),
                            ("warm", "{q}_warm_pre", "{q}_warm")):
    cols = [(q, diff(pre_s.format(q=q), post_s.format(q=q))) for q in queries]
    cols = [(q, d) for q, d in cols if d]
    if not cols:
        continue
    phases = sorted({p for _, d in cols for p in d})
    print(f"## {kind}\n")
    print("| phase | " + " | ".join(q for q, _ in cols) + " |")
    print("|---" * (len(cols) + 1) + "|")
    totals = {q: sum(d.values()) for q, d in cols}
    for p in phases:
        cells = []
        for q, d in cols:
            ns, tot = d.get(p, 0), totals[q]
            cells.append("—" if ns == 0 else f"{ns/1e9:.3f}s ({100*ns/tot:.1f}%)")
        print(f"| `{p}` | " + " | ".join(cells) + " |")
    print("| **exec (sum)** | " + " | ".join(f"{totals[q]/1e9:.3f}s" for q, _ in cols) + " |")
    print()
PY

echo ">> summary:" >&2
cat "$SUMMARY"
