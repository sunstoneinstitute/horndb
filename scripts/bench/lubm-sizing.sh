#!/usr/bin/env bash
# Sizing probe #2 for SPEC-25 S6. hornbench has a JDK but no RDF/XML -> NT
# converter, so gen_lubm.sh cannot run there. Test the fix: fetch Apache Jena
# (pure Java, runs on the installed JDK), generate a small LUBM in parallel,
# convert in parallel, and report the per-phase rates so LUBM-8000 can be
# extrapolated honestly against the 240-minute bench.yml budget.
set -uo pipefail
OUT="${BENCH_OUT:-bench-out}"; mkdir -p "$OUT"
BENCH_DIR="${BENCH_DIR:-/home/bench/horndb-bench}"
N="${SIZING_N:-40}"
GEN_P="${GEN_P:-8}"
CONV_P="${CONV_P:-16}"
JENA_VER="${JENA_VER:-5.3.0}"
WORK="$(mktemp -d /tmp/lubm-sizing.XXXXXX)"

log() { echo "$*" >&2; }

exec > >(tee "$OUT/SUMMARY.md") 2>"$OUT/stderr.log"
echo "## LUBM staging sizing (N=$N, gen_p=$GEN_P, conv_p=$CONV_P)"
echo
echo '```'

# --- Jena, cached in the persistent bench dir ---------------------------------
JENA_HOME="$BENCH_DIR/jena/apache-jena-$JENA_VER"
t0=$(date +%s)
if [ ! -x "$JENA_HOME/bin/riot" ]; then
  mkdir -p "$BENCH_DIR/jena"
  URL="https://archive.apache.org/dist/jena/binaries/apache-jena-$JENA_VER.tar.gz"
  echo "fetching $URL"
  curl -fsSL "$URL" | tar xz -C "$BENCH_DIR/jena" || { echo "jena fetch FAILED"; echo '```'; exit 0; }
fi
t1=$(date +%s)
echo "jena fetch/reuse:  $((t1-t0)) s   ($JENA_HOME)"
export PATH="$JENA_HOME/bin:$PATH"
riot --version 2>&1 | head -2

# --- UBA generator, cached too ------------------------------------------------
UBA="$BENCH_DIR/uba"
mkdir -p "$UBA"
[ -f "$UBA/uba1.7.zip" ] || curl -fsSL http://swat.cse.lehigh.edu/projects/lubm/uba1.7.zip -o "$UBA/uba1.7.zip" \
  || { echo "uba fetch FAILED"; echo '```'; exit 0; }
[ -d "$UBA/extracted" ] || { mkdir -p "$UBA/extracted"; unzip -oq "$UBA/uba1.7.zip" -d "$UBA/extracted"; }
GEN_CLASS="$(find "$UBA/extracted" -path '*edu/lehigh/swat/bench/uba/Generator.class' | head -1)"
[ -n "$GEN_CLASS" ] || { echo "Generator.class not found"; echo '```'; exit 0; }
CP="${GEN_CLASS%/edu/lehigh/swat/bench/uba/Generator.class}"
ONTO=http://swat.cse.lehigh.edu/onto/univ-bench.owl

# --- phase 1: parallel generation --------------------------------------------
# UBA's -index is the starting university index, so P processes each own a
# disjoint slice and write into their own directory.
t0=$(date +%s)
per=$(( (N + GEN_P - 1) / GEN_P ))
pids=()
for ((i=0; i<GEN_P; i++)); do
  start=$(( i * per ))
  [ "$start" -ge "$N" ] && break
  k=$(( N - start < per ? N - start : per ))
  d="$WORK/gen$i"; mkdir -p "$d"
  ( cd "$d" && java -cp "$CP" edu.lehigh.swat.bench.uba.Generator \
      -univ "$k" -index "$start" -seed 0 -onto "$ONTO" >gen.log 2>&1 ) &
  pids+=($!)
done
fail=0; for p in "${pids[@]}"; do wait "$p" || fail=1; done
t1=$(date +%s); GEN_S=$((t1-t0))
echo "generate ($GEN_P proc): $GEN_S s   (fail=$fail)"
mapfile -t OWL < <(find "$WORK" -name 'University*.owl' -o -name '*University*.owl' | sort)
echo "owl files:         ${#OWL[@]}"
if [ "${#OWL[@]}" -eq 0 ]; then
  echo "no OWL files produced; generator logs:"; cat "$WORK"/gen*/gen.log 2>/dev/null | head -20
  echo '```'; exit 0
fi
echo "owl bytes:         $(du -sb "$WORK" | cut -f1)"

# --- phase 2: parallel RDF/XML -> N-Triples ----------------------------------
t0=$(date +%s)
printf '%s\n' "${OWL[@]}" > "$WORK/list"
split -n "r/$CONV_P" -d "$WORK/list" "$WORK/chunk."
pids=()
for c in "$WORK"/chunk.*; do
  [ -s "$c" ] || continue
  ( xargs -a "$c" -r riot --syntax=RDFXML --output=NT > "$c.nt" 2>"$c.err" ) &
  pids+=($!)
done
fail=0; for p in "${pids[@]}"; do wait "$p" || fail=1; done
cat "$WORK"/chunk.*.nt > "$WORK/abox.nt"
t2=$(date +%s); CONV_S=$((t2-t0))
echo "convert ($CONV_P proc): $CONV_S s   (fail=$fail)"
head -3 "$WORK"/chunk.*.err 2>/dev/null | head -10

TR=$(wc -l < "$WORK/abox.nt"); BY=$(stat -c%s "$WORK/abox.nt")
echo
echo "triples:           $TR"
echo "bytes:             $BY"
echo
S=$(( 8000 / N ))
echo "-- extrapolated to LUBM-8000 (x$S, same parallelism) --"
echo "triples:           $(( TR * S ))"
echo "abox.nt bytes:     $(( BY * S ))  (~$(( BY * S / 1000000000 )) GB)"
echo "generate:          ~$(( GEN_S * S / 60 )) min"
echo "convert:           ~$(( CONV_S * S / 60 )) min"
echo "staging total:     ~$(( (GEN_S + CONV_S) * S / 60 )) min  (bench.yml cap is 240 min)"
rm -rf "$WORK"
echo '```'
