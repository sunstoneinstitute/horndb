#!/usr/bin/env bash
#
# stage-lubm.sh N DIR [PREFIX_N] — build a large LUBM corpus, in parallel.
#
# `gen_lubm.sh` generates one university at a time and converts one file at a
# time. That is fine at LUBM-10 and hopeless at LUBM-8000 (~1.1 B triples).
# This does the same work with every core: several UBA processes over disjoint
# university ranges, then several Jena `riot` processes over the generated
# RDF/XML.
#
# It also carries its own converter. A host can have a JDK and still have no
# RDF/XML -> N-Triples converter — hornbench does — which is what stops
# `gen_lubm.sh` there. Apache Jena is pure Java, so it is fetched once into the
# persistent bench dir and reused.
#
# Writes DIR/{tbox.nt,abox.nt}: the layout `get_lubm.sh` looks for, so a later
# run reuses the corpus instead of rebuilding it.
#
# PREFIX_N (optional) also writes a smaller corpus from universities
# 0..PREFIX_N-1 into a sibling `lubm-<PREFIX_N>` directory. Those universities
# are a prefix of this generation, so the smaller corpus costs one extra
# conversion pass rather than a second generation. SPEC-25 S6's honesty clause
# asks for the largest scale that fits alongside the LUBM-8000 attempt; this is
# how that fallback is produced without paying for it twice.
#
# Env: STAGE_P (parallelism, default 16), JENA_VER, BENCH_DIR (cache location).
set -uo pipefail

N="${1:?usage: stage-lubm.sh N DIR [PREFIX_N]}"
DEST="${2:?usage: stage-lubm.sh N DIR [PREFIX_N]}"
SUB="${3:-}"
P="${STAGE_P:-16}"
JENA_VER="${JENA_VER:-5.3.0}"
BENCH_DIR="${BENCH_DIR:-$(dirname "$DEST")}"
ONTO=http://swat.cse.lehigh.edu/onto/univ-bench.owl

say() { echo ">> $*" >&2; }

WORK="$DEST/work"
mkdir -p "$WORK" || exit 1
say "staging LUBM-$N into $DEST (parallelism $P)"

# --- Apache Jena, cached ------------------------------------------------------
JENA="$BENCH_DIR/jena/apache-jena-$JENA_VER"
if [ ! -x "$JENA/bin/riot" ]; then
  mkdir -p "$BENCH_DIR/jena"
  say "fetching Apache Jena $JENA_VER"
  curl -fsSL "https://archive.apache.org/dist/jena/binaries/apache-jena-$JENA_VER.tar.gz" \
    | tar xz -C "$BENCH_DIR/jena" || { say "Jena fetch failed"; exit 1; }
fi
export PATH="$JENA/bin:$PATH"
export JVM_ARGS="${JVM_ARGS:--Xmx2g}"
command -v java >/dev/null || { say "no JDK on PATH"; exit 1; }

# --- UBA generator, cached ----------------------------------------------------
UBA="$BENCH_DIR/uba"; mkdir -p "$UBA"
[ -f "$UBA/uba1.7.zip" ] \
  || curl -fsSL http://swat.cse.lehigh.edu/projects/lubm/uba1.7.zip -o "$UBA/uba1.7.zip" \
  || { say "UBA fetch failed"; exit 1; }
[ -d "$UBA/extracted" ] || { mkdir -p "$UBA/extracted"; unzip -oq "$UBA/uba1.7.zip" -d "$UBA/extracted"; }
GEN_CLASS="$(find "$UBA/extracted" -path '*edu/lehigh/swat/bench/uba/Generator.class' | head -1)"
[ -n "$GEN_CLASS" ] || { say "Generator.class not found in the UBA archive"; exit 1; }
CP="${GEN_CLASS%/edu/lehigh/swat/bench/uba/Generator.class}"

# --- generate RDF/XML in parallel --------------------------------------------
# UBA's -index is the first university index, so each process owns a disjoint
# slice. UBA also prepends a literal "generated\" to every output name, so the
# files carry a backslash: every later step must keep it (xargs -d '\n', never
# bare xargs, which would read the backslash as an escape).
t0=$(date +%s)
per=$(( (N + P - 1) / P ))
pids=()
for ((i=0; i<P; i++)); do
  start=$(( i * per )); [ "$start" -ge "$N" ] && break
  k=$(( N - start < per ? N - start : per ))
  d="$WORK/g$i"; mkdir -p "$d"
  ( cd "$d" && java -cp "$CP" edu.lehigh.swat.bench.uba.Generator \
      -univ "$k" -index "$start" -seed 0 -onto "$ONTO" >gen.log 2>&1 ) &
  pids+=($!)
done
gfail=0; for p in "${pids[@]}"; do wait "$p" || gfail=1; done
say "generated in $(( $(date +%s) - t0 ))s (fail=$gfail)"

find "$WORK" -name '*University*.owl' > "$WORK/list"
NFILES=$(wc -l < "$WORK/list")
say "$NFILES RDF/XML files, $(du -sh "$WORK" | cut -f1)"
[ "$NFILES" -gt 0 ] || { say "generator produced nothing"; head -20 "$WORK"/g0/gen.log >&2; exit 1; }

# --- tbox --------------------------------------------------------------------
curl -fsSL "$ONTO" -o "$WORK/univ-bench.owl" \
  && riot --syntax=RDFXML --output=NT "$WORK/univ-bench.owl" > "$DEST/tbox.nt" \
  || { say "ontology conversion failed"; exit 1; }
say "tbox.nt: $(wc -l < "$DEST/tbox.nt") triples"

# --- convert to N-Triples in parallel ----------------------------------------
convert() {  # convert <listfile> <dest.nt>
  local list="$1" dest="$2" t c f=0
  t=$(date +%s)
  rm -f "$WORK"/chunk.*
  split -n "r/$P" -d -a 3 "$list" "$WORK/chunk."
  local pids=()
  for c in "$WORK"/chunk.[0-9]*; do
    case "$c" in *.nt) continue ;; esac
    [ -s "$c" ] || continue
    ( xargs -d '\n' -a "$c" -r riot --syntax=RDFXML --output=NT > "$c.nt" 2>/dev/null ) &
    pids+=($!)
  done
  local p; for p in "${pids[@]}"; do wait "$p" || f=1; done
  cat "$WORK"/chunk.*.nt > "$dest"
  rm -f "$WORK"/chunk.*
  say "$dest: $(wc -l < "$dest") triples in $(( $(date +%s) - t ))s (fail=$f)"
  [ "$f" -eq 0 ]
}

convert "$WORK/list" "$DEST/abox.nt" || exit 1

# --- optional smaller prefix corpus ------------------------------------------
if [ -n "$SUB" ]; then
  SUBDEST="$BENCH_DIR/lubm-$SUB"; mkdir -p "$SUBDEST"
  awk -v m="$SUB" -F'University' '{split($2,a,"_"); if (a[1]+0 < m) print}' "$WORK/list" > "$WORK/list.sub"
  cp "$DEST/tbox.nt" "$SUBDEST/tbox.nt"
  convert "$WORK/list.sub" "$SUBDEST/abox.nt" || exit 1
fi

rm -rf "$WORK"
[ -s "$DEST/tbox.nt" ] && [ -s "$DEST/abox.nt" ]
