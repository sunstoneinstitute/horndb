#!/usr/bin/env bash
#
# stage-lubm.sh — build a large LUBM corpus on the bench host, in parallel.
#
# scripts/bench/gen_lubm.sh generates one university at a time and converts one
# file at a time, which is fine at LUBM-10 and hopeless at LUBM-8000 (~1.1 B
# triples). This does the same work with every core: N parallel UBA processes
# over disjoint university ranges, then N parallel Jena `riot` processes over
# the generated RDF/XML.
#
# hornbench has a JDK but no RDF/XML -> N-Triples converter, so Jena is fetched
# into the persistent bench dir on first use and reused after that.
#
# Output: $BENCH_DIR/lubm-<N>/{tbox.nt,abox.nt} — the layout get_lubm.sh looks
# for, so a later measuring run reuses the corpus instead of rebuilding it.
set -uo pipefail

OUT="${BENCH_OUT:-bench-out}"; mkdir -p "$OUT"
BENCH_DIR="${BENCH_DIR:-/home/bench/horndb-bench}"
N="${1:-8000}"
SUB="${2:-}"                 # optional smaller prefix corpus, e.g. 1000
P="${STAGE_P:-16}"
JENA_VER="${JENA_VER:-5.3.0}"
ONTO=http://swat.cse.lehigh.edu/onto/univ-bench.owl

DEST="$BENCH_DIR/lubm-$N"
WORK="$DEST/work"
mkdir -p "$WORK" || exit 1

say() { echo "$*"; }

exec > >(tee "$OUT/SUMMARY.md") 2>"$OUT/stage.log"
say "## staging LUBM-$N (parallelism $P)"
say
say '```'
say "dest: $DEST"
df -h "$BENCH_DIR" | tail -1

# --- Jena (RDF/XML -> N-Triples), cached ------------------------------------
JENA="$BENCH_DIR/jena/apache-jena-$JENA_VER"
if [ ! -x "$JENA/bin/riot" ]; then
  mkdir -p "$BENCH_DIR/jena"
  curl -fsSL "https://archive.apache.org/dist/jena/binaries/apache-jena-$JENA_VER.tar.gz" \
    | tar xz -C "$BENCH_DIR/jena" || { say "jena fetch FAILED"; say '```'; exit 1; }
fi
export PATH="$JENA/bin:$PATH"
# riot logs one INFO line per input file; at 160k files that is the bulk of the
# output, so quiet it down rather than write a 20 MB log.
export JVM_ARGS="${JVM_ARGS:--Xmx2g}"

# --- UBA generator, cached ---------------------------------------------------
UBA="$BENCH_DIR/uba"; mkdir -p "$UBA"
[ -f "$UBA/uba1.7.zip" ] || curl -fsSL http://swat.cse.lehigh.edu/projects/lubm/uba1.7.zip -o "$UBA/uba1.7.zip" || exit 1
[ -d "$UBA/extracted" ] || { mkdir -p "$UBA/extracted"; unzip -oq "$UBA/uba1.7.zip" -d "$UBA/extracted"; }
GEN_CLASS="$(find "$UBA/extracted" -path '*edu/lehigh/swat/bench/uba/Generator.class' | head -1)"
[ -n "$GEN_CLASS" ] || { say "Generator.class not found"; say '```'; exit 1; }
CP="${GEN_CLASS%/edu/lehigh/swat/bench/uba/Generator.class}"

# --- phase 1: generate RDF/XML in parallel -----------------------------------
# UBA's -index is the first university index, so each process owns a disjoint
# slice. It also prepends a literal "generated\" to every output name, which is
# why every later step has to keep backslashes intact (xargs -d '\n', never
# bare xargs).
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
t1=$(date +%s)
say "generate:   $((t1-t0)) s (fail=$gfail)"

find "$WORK" -name '*University*.owl' > "$WORK/list"
NFILES=$(wc -l < "$WORK/list")
say "owl files:  $NFILES"
say "owl bytes:  $(du -sb "$WORK" | cut -f1)"
[ "$NFILES" -gt 0 ] || { say "nothing generated"; head -20 "$WORK"/g0/gen.log; say '```'; exit 1; }

# --- tbox --------------------------------------------------------------------
curl -fsSL "$ONTO" -o "$WORK/univ-bench.owl" && riot --syntax=RDFXML --output=NT "$WORK/univ-bench.owl" > "$DEST/tbox.nt"
say "tbox:       $(wc -l < "$DEST/tbox.nt") triples"

# --- phase 2: convert to N-Triples in parallel -------------------------------
convert() {  # convert <listfile> <dest.nt>
  local list="$1" dest="$2" t base
  t=$(date +%s)
  rm -f "$WORK"/chunk.*
  split -n "r/$P" -d -a 3 "$list" "$WORK/chunk."
  local pids=() c
  for c in "$WORK"/chunk.[0-9]*; do
    case "$c" in *.nt|*.err) continue;; esac
    [ -s "$c" ] || continue
    ( xargs -d '\n' -a "$c" -r riot --syntax=RDFXML --output=NT > "$c.nt" 2>/dev/null ) &
    pids+=($!)
  done
  local f=0 p; for p in "${pids[@]}"; do wait "$p" || f=1; done
  cat "$WORK"/chunk.*.nt > "$dest"
  rm -f "$WORK"/chunk.*
  say "convert:    $(( $(date +%s) - t )) s (fail=$f) -> $dest"
}

convert "$WORK/list" "$DEST/abox.nt"
say "triples:    $(wc -l < "$DEST/abox.nt")"
say "bytes:      $(stat -c%s "$DEST/abox.nt")"

# --- optional smaller prefix corpus ------------------------------------------
# Universities 0..SUB-1 are a prefix of this generation, so the fallback corpus
# the honesty clause may need costs one extra conversion pass, not a second
# generation.
if [ -n "$SUB" ]; then
  SUBDEST="$BENCH_DIR/lubm-$SUB"; mkdir -p "$SUBDEST"
  grep -E "University([0-9]+)_" "$WORK/list" \
    | awk -v m="$SUB" -F'University' '{split($2,a,"_"); if (a[1]+0 < m) print}' > "$WORK/list.sub"
  say "sub files:  $(wc -l < "$WORK/list.sub")"
  cp "$DEST/tbox.nt" "$SUBDEST/tbox.nt"
  convert "$WORK/list.sub" "$SUBDEST/abox.nt"
  say "sub triples:$(wc -l < "$SUBDEST/abox.nt")"
fi

rm -rf "$WORK"
say
df -h "$BENCH_DIR" | tail -1
say '```'
