#!/usr/bin/env bash
#
# host-capacity.sh — report what the bench host actually has.
#
# SPEC-25 S6 asks for LUBM-8000 (1.1 B triples) numbers. Whether that corpus
# can even exist on this host is a memory- and disk-capacity question, and the
# honesty clause says to answer it with measurements rather than assume. This
# script answers it: RAM, free disk on the bench dir, and whether a JDK is
# present (the UBA generator needs one; hornbench historically has none, so
# corpora arrive as pre-generated release tarballs).
set -uo pipefail

OUT="${BENCH_OUT:-bench-out}"
mkdir -p "$OUT"
BENCH_DIR="${BENCH_DIR:-/home/bench/horndb-bench}"

{
  echo "## host capacity"
  echo
  echo '```'
  echo "uname:      $(uname -sr)"
  echo "cpu:        $(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2- | sed 's/^ *//')"
  echo "cores:      $(nproc)"
  free -h
  echo
  echo "swap total: $(awk '/SwapTotal/{print $2" "$3}' /proc/meminfo)"
  echo
  echo "-- disk --"
  df -h "$BENCH_DIR" / /tmp 2>&1
  echo
  echo "-- java --"
  (command -v java && java -version 2>&1) || echo "no JDK on PATH"
  echo
  echo "-- existing corpora under $BENCH_DIR --"
  du -sh "$BENCH_DIR"/* 2>/dev/null || echo "(none / unreadable)"
  echo '```'
} | tee "$OUT/SUMMARY.md"
