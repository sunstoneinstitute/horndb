#!/usr/bin/env bash
#
# cold-tier.sh — SPEC-25 S5 cold-tier footprint and read amplification (HDB-181).
#
# Answers the two measured clauses of SPEC-25 acceptance #5:
#   * SPEC-02 NF1 — cold-resident bytes/triple <= 6, amortised. The 5.440
#     B/triple already in docs/benchmarks.md is the *snapshot* encoding; the
#     cold partition file is a different format (no dictionary, global TermId
#     bits, one subject-major block), so it must be measured separately.
#   * SPEC-02 NF4 — a cold scan costs at most 2x a contiguous encoded scan.
#
# Runs `cargo bench -p horndb-storage --bench cold_tier` and turns its `[cold]`
# stderr lines into bench-out/SUMMARY.md. An NF1/NF4 miss is a recorded result,
# not a script failure: the table says MISS and the script still exits 0. Only
# a bench that fails to run exits non-zero.
#
# Reuses the persistent LUBM corpus on the bench host (the Actions checkout is
# wiped between runs), same convention as footprint-split.sh. Run it via
# .github/workflows/bench.yml:
#   gh workflow run bench.yml --ref <branch> -f script=scripts/bench/cold-tier.sh
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

OUT="$REPO_ROOT/bench-out"
mkdir -p "$OUT"
LOG="$OUT/cold-tier.log"
SUMMARY="$OUT/SUMMARY.md"

PERSIST="${BENCH_PERSIST:-/home/bench/horndb-bench}"
mkdir -p "$PERSIST" 2>/dev/null || PERSIST="$REPO_ROOT/target/bench-persist"
LUBM_N="${LUBM_N:-10}"
WORK="${LUBM_DIR:-$PERSIST/lubm-$LUBM_N}"

# Same pin as the other footprint scripts: every recorded number was measured
# at one parse thread, and `auto` is host-dependent on top of that.
export HORNDB_LOAD_THREADS="${HORNDB_LOAD_THREADS:-1}"

# A real corpus if we can get one; the bench falls back to its own synthetic
# LUBM-shaped generator when LUBM_NT is unset or missing, and prints which one
# it used either way.
if [ -z "${LUBM_NT:-}" ]; then
  if "$REPO_ROOT/scripts/bench/get_lubm.sh" "$LUBM_N" "$WORK"; then
    export LUBM_NT="$WORK/abox.nt"
  else
    echo ">> no LUBM-$LUBM_N corpus available — bench falls back to synthetic" >&2
  fi
fi

echo ">> running cold_tier bench (LUBM_NT=${LUBM_NT:-unset})" >&2
# Straight redirect, not `2> >(tee ...)`: bash does not wait for a process
# substitution to finish, so the parser below could read a truncated log.
cargo bench -p horndb-storage --bench cold_tier 2>"$LOG" \
  || { cat "$LOG" >&2; echo "::error::cold_tier bench failed" >&2; exit 1; }
cat "$LOG" >&2

python3 - "$LOG" "$(git rev-parse --short HEAD)" "$(hostname)" "$(uname -sr)" \
    "$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$SUMMARY" <<'PY'
import re, sys

log, commit, host, kernel, date = sys.argv[1:6]

def kv(line):
    return dict(re.findall(r"(\w+)=(\S+)", line))

corpus, foot = None, None
ratios, byts, trips = [], [], []
for line in open(log, errors="replace"):
    if not line.startswith("[cold] "):
        continue
    kind = line.split()[1]
    d = kv(line)
    if kind == "corpus":
        corpus = d
    elif kind == "footprint":
        foot = d
    elif kind == "ratio":
        ratios.append(d)
    elif kind == "bytes":
        byts.append(d)
    elif kind == "roundtrip":
        trips.append(d)

if corpus is None or foot is None:
    sys.exit(f"no [cold] lines parsed from {log}")

NF1, NF4 = 6.0, 2.0
cold_bpt = float(foot["cold_bpt"])
worst = max((float(r["ratio"]) for r in ratios if r["graded"] == "1"), default=0.0)
verdict = lambda ok: "**PASS**" if ok else "**MISS**"

print("# cold-tier footprint and read amplification (HDB-181, SPEC-25 S5)\n")
print(f"- commit: `{commit}`  host: `{host}`  kernel: `{kernel}`  date: {date}")
print(f"- corpus: `{corpus['label']}` — {corpus['triples']} triples, "
      f"{corpus['demoted_partitions']} partitions demoted")
print("- `cargo bench -p horndb-storage --bench cold_tier`; two stores over the "
      "same corpus, one left warm, one fully demoted\n")

print("## SPEC-02 NF1 — cold-resident bytes/triple\n")
print("| tier | bytes | B/triple | budget | verdict |")
print("|---|---:|---:|---:|:--|")
print(f"| warm (in-memory columns) | {foot['warm_bytes']} | {float(foot['warm_bpt']):.3f} | — | — |")
print(f"| cold (mapped files) | {foot['cold_bytes']} | {cold_bpt:.3f} | "
      f"<= {NF1:.1f} | {verdict(cold_bpt <= NF1)} |")
print()

print("## SPEC-02 NF4 — cold scan read amplification\n")
print("Wall clock, fastest of 5 full scans per cell, top predicates by row "
      "count. Two graded comparisons: the cold subject-major scan against the "
      "same scan over the warm columns (the contiguous encoded scan NF4 names), "
      "and the cold object-major materialisation against the cold "
      "subject-major decode of the same file (the transient decode+sort). The "
      "cold/warm-cached row is context only — a warm partition materialises "
      "its object-major columns once and hands out `Arc` clones after that, so "
      "that ratio is a cache hit against a decode, not read amplification.\n")
print("| predicate | rows | comparison | baseline us | cold us | ratio | budget | verdict |")
print("|---|---:|---|---:|---:|---:|---:|:--|")
for r in ratios:
    x = float(r["ratio"])
    graded = r["graded"] == "1"
    print(f"| `{r['pred']}` | {r['rows']} | `{r['what']}` | "
          f"{float(r['base_ns'])/1e3:.1f} | {float(r['cold_ns'])/1e3:.1f} | "
          f"{x:.2f}x | " + (f"<= {NF4:.0f}x | {verdict(x <= NF4)} |" if graded
                            else "— | info |"))
print()
print("Structural amplification: bytes the cold scan touches (one forward pass "
      "over the mapped file) against a contiguous encoded scan of the same rows "
      "(two u64 columns).\n")
print("| predicate | rows | cold mapped B | contiguous encoded B | touched/encoded |")
print("|---|---:|---:|---:|---:|")
for b in byts:
    print(f"| `{b['pred']}` | {b['rows']} | {b['mapped']} | {b['encoded']} | "
          f"{float(b['amp']):.2f}x |")
print()

if trips:
    print("## Promote/demote roundtrip\n")
    print("| predicate | rows | promote ms | demote ms |")
    print("|---|---:|---:|---:|")
    for t in trips:
        print(f"| `{t['pred']}` | {t['rows']} | {float(t['promote_ms']):.2f} | "
              f"{float(t['demote_ms']):.2f} |")
    print()

print(f"**NF1 {'pass' if cold_bpt <= NF1 else 'MISS'}** "
      f"({cold_bpt:.3f} B/triple vs <= {NF1:.1f}); "
      f"**NF4 {'pass' if worst <= NF4 else 'MISS'}** "
      f"(worst graded ratio {worst:.2f}x vs <= {NF4:.0f}x).")
PY
rc=$?
if [ "$rc" -ne 0 ]; then
  echo "::error::could not summarise $LOG" >&2
  exit "$rc"
fi

echo ">> summary:" >&2
cat "$SUMMARY"
