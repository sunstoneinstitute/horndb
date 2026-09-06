#!/usr/bin/env bash
# Throwaway sizing probe for SPEC-25 S6: how fast can this host actually
# generate LUBM, and with which RDF/XML -> N-Triples converter? Extrapolating
# from a small N is the only honest way to decide whether LUBM-8000 fits the
# 240-minute bench.yml budget before spending it.
set -uo pipefail
OUT="${BENCH_OUT:-bench-out}"; mkdir -p "$OUT"
N="${SIZING_N:-20}"
DIR="$(mktemp -d /tmp/lubm-sizing.XXXXXX)"
{
  echo "## LUBM generation sizing (N=$N)"; echo; echo '```'
  echo "-- converters --"
  command -v riot   && riot --version 2>&1 | head -2 || echo "riot: absent"
  python3 -c 'import rdflib; print("rdflib:", rdflib.__version__)' 2>&1 || echo "rdflib: absent"
  echo
  t0=$(date +%s)
  bash scripts/bench/gen_lubm.sh --universities "$N" --out "$DIR" 2>&1 | tail -20
  rc=$?
  t1=$(date +%s)
  echo
  echo "gen exit:   $rc"
  echo "elapsed:    $((t1 - t0)) s"
  if [ -s "$DIR/abox.nt" ]; then
    tr=$(wc -l < "$DIR/abox.nt"); by=$(stat -c%s "$DIR/abox.nt")
    echo "triples:    $tr"
    echo "bytes:      $by"
    echo "rate:       $(( tr / ((t1 - t0) > 0 ? (t1 - t0) : 1) )) triples/s"
    echo "-- extrapolated to LUBM-8000 (x$((8000 / N))) --"
    echo "triples:    $(( tr * 8000 / N ))"
    echo "bytes:      $(( by * 8000 / N )) (~$(( by * 8000 / N / 1000000000 )) GB)"
    echo "gen time:   ~$(( (t1 - t0) * 8000 / N / 60 )) min"
  else
    echo "no abox.nt produced"
  fi
  rm -rf "$DIR"
  echo '```'
} 2>&1 | tee "$OUT/SUMMARY.md"
