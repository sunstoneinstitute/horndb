#!/usr/bin/env bash
# HDB-37 capacity probe: can hornbench hold and serve the true SF=0.256
# (256M-triple) SPB dataset for BOTH engines?
#
# Read-only and fast (seconds). It builds nothing and generates nothing —
# it only reports what the host has, so the scale-up decision rests on
# measured capacity rather than a guess. Everything lands in
# bench-out/SUMMARY.md.
set -uo pipefail
mkdir -p bench-out
OUT=bench-out/SUMMARY.md

# Measured on trainmarks xlarge by HDB-146: HornDB's own serving structures
# (columnar partitions + dictionary + one query snapshot) cost 170 B/triple.
# The SPB corpus has a different term mix, so treat this as an order-of-
# magnitude estimate, not a prediction.
B_PER_TRIPLE=170
TARGET_TRIPLES=256000000

say() { echo "$@" >> "$OUT"; }
have() { command -v "$1" >/dev/null 2>&1; }

{
  echo "# HDB-37 — SPB SF=0.256 capacity probe"
  echo
  echo "Host: \`$(hostname)\`  ·  $(date -Is)"
  echo
} > "$OUT"

say '## Memory'
say '```'
free -h 2>&1 >> "$OUT"
say '```'
MEM_KB=$(awk '/MemTotal/{print $2}' /proc/meminfo 2>/dev/null || echo 0)
MEM_GB=$(( MEM_KB / 1024 / 1024 ))
NEED_GB=$(( TARGET_TRIPLES * B_PER_TRIPLE / 1024 / 1024 / 1024 ))
say
say "Total RAM: **${MEM_GB} GiB**."
say "HornDB serving estimate at ${B_PER_TRIPLE} B/triple (HDB-146, trainmarks): **~${NEED_GB} GiB** for ${TARGET_TRIPLES} triples — before the load-time peak, and before GraphDB's own copy."
say

say '## Disk'
say '```'
df -h / /home /tmp "${SPB_ASSETS:-/home/bench}" 2>&1 | sort -u >> "$OUT"
say '```'
say

say '## Existing SPB assets'
say '```'
ls -la "${SPB_ASSETS:-/home/bench/src/horndb/crates/harness/data/ldbc-spb/dist}" 2>&1 | head -30 >> "$OUT"
echo "--- sizes ---" >> "$OUT"
du -sh /home/bench/src/horndb/crates/harness/data/ldbc-spb 2>&1 >> "$OUT"
du -sh /home/bench/horndb-bench 2>&1 >> "$OUT"
say '```'
say

say '## Current corpus actually loaded by the nightly'
say '```'
find /home/bench -maxdepth 6 \( -name '*.nt' -o -name '*closure*' \) -size +10M \
  -printf '%10s  %p\n' 2>/dev/null | sort -rn | head -15 >> "$OUT"
say '```'
say

say '## GraphDB'
say '```'
ls -d /home/bench/graphdb* /opt/graphdb* 2>&1 | head >> "$OUT"
grep -rhoE '\-Xmx[0-9]+[gGmM]' /home/bench/graphdb*/conf/* /home/bench/graphdb*/bin/* 2>/dev/null | sort -u >> "$OUT"
say '```'
say

say '## Generator'
say '```'
if have ant; then ant -version 2>&1 >> "$OUT"; else echo "ant: NOT INSTALLED (SPB data generation needs it)" >> "$OUT"; fi
if have java; then java -version 2>&1 | head -1 >> "$OUT"; else echo "java: NOT INSTALLED" >> "$OUT"; fi
say '```'
