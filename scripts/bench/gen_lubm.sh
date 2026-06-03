#!/usr/bin/env bash
#
# gen_lubm.sh — generate a LUBM-N workload as N-Triples for the RDFox comparison.
#
# Fetches the Lehigh UBA1.7 data generator (Java) and the canonical RDF/XML
# univ-bench.owl ontology, generates N universities, and converts everything to
# N-Triples via Apache Jena `riot`:
#
#   <out>/tbox.nt   the ontology (RDF/XML -> NT)
#   <out>/abox.nt   the generated instance data (RDF/XML -> NT, concatenated)
#
# All outputs live under the gitignored target/ tree.
#
# Usage: gen_lubm.sh --universities N [--out DIR] [--seed S]
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

UNIV=1
SEED=0
OUTDIR_BASE="${OUTDIR:-$REPO_ROOT/target/bench-rdfox}"
OUT=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --universities) UNIV="$2"; shift 2 ;;
    --seed)         SEED="$2"; shift 2 ;;
    --out)          OUT="$2"; shift 2 ;;
    -h|--help)      sed -n '2,17p' "$0"; exit 0 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done
[[ -n "$OUT" ]] || OUT="$OUTDIR_BASE/lubm/$UNIV"

command -v java >/dev/null || { echo "java not found (UBA generator needs a JDK)" >&2; exit 1; }
command -v riot >/dev/null || { echo "riot not found (brew install jena)" >&2; exit 1; }

UBA_DIR="$OUTDIR_BASE/uba"
UBA_ZIP="$UBA_DIR/uba1.7.zip"
UBA_URL="http://swat.cse.lehigh.edu/projects/lubm/uba1.7.zip"
ONTO_URL="http://swat.cse.lehigh.edu/onto/univ-bench.owl"

mkdir -p "$UBA_DIR"
if [[ ! -f "$UBA_ZIP" ]]; then
  echo ">> fetching UBA1.7 generator" >&2
  curl -fsSL "$UBA_URL" -o "$UBA_ZIP"
fi
# Unzip once; the archive contains compiled classes + a bundled univ-bench.owl.
if [[ ! -d "$UBA_DIR/extracted" ]]; then
  mkdir -p "$UBA_DIR/extracted"
  unzip -o -q "$UBA_ZIP" -d "$UBA_DIR/extracted"
fi

# Locate the classpath root (dir containing edu/lehigh/.../Generator.class) and
# the bundled RDF/XML ontology.
GEN_CLASS="$(find "$UBA_DIR/extracted" -path '*edu/lehigh/swat/bench/uba/Generator.class' | head -1)"
[[ -n "$GEN_CLASS" ]] || { echo "Generator.class not found in UBA archive" >&2; exit 1; }
CP_ROOT="${GEN_CLASS%/edu/lehigh/swat/bench/uba/Generator.class}"
ONTO_SRC="$(find "$UBA_DIR/extracted" -iname 'univ-bench.owl' | head -1)"
# The UBA1.7 archive does not bundle univ-bench.owl; fetch it separately.
if [[ -z "$ONTO_SRC" ]]; then
  ONTO_SRC="$UBA_DIR/univ-bench.owl"
  if [[ ! -f "$ONTO_SRC" ]]; then
    echo ">> fetching univ-bench.owl ontology" >&2
    curl -fsSL "$ONTO_URL" -o "$ONTO_SRC"
  fi
fi
[[ -n "$ONTO_SRC" ]] || { echo "univ-bench.owl not found and could not be fetched" >&2; exit 1; }

# Generate into a clean dir (UBA writes University*.owl into the CWD).
# NOTE: UBA1.7 (compiled 2004) uses File.separator in path construction; on
# macOS/Linux the Java path separator is '/' so files land directly in GENDIR.
# However the generator prepends "generated\" (backslash) to each output name,
# causing files to be written with a literal backslash in the filename to the
# *parent* dir ($OUT) instead of inside $GENDIR.  We detect both layouts.
GENDIR="$OUT/generated"
rm -rf "$GENDIR"; mkdir -p "$GENDIR"
# Also remove any stale backslash-named files from a previous run.
rm -f "$OUT/generated"\\University*.owl "$OUT/generated"\\log.txt 2>/dev/null || true
echo ">> generating LUBM-$UNIV (seed $SEED) into $GENDIR" >&2
# Note: the generator fetches the ontology from -onto <URL> at runtime (it does
# not use the locally cached copy); the local copy is only for riot -> tbox.nt.
( cd "$GENDIR" && java -cp "$CP_ROOT" edu.lehigh.swat.bench.uba.Generator \
    -univ "$UNIV" -index 0 -seed "$SEED" -onto "$ONTO_URL" >/dev/null ) \
  || { echo "UBA generator (java) failed — see stderr above" >&2; exit 1; }

# Locate the generated OWL files.  Two layouts seen in the wild:
#   1) Normal:   $GENDIR/University*.owl   (File.separator='/')
#   2) Backslash: $OUT/generated\University*.owl  (backslash literal in name)
OWL_FILES=( "$GENDIR"/University*.owl )
if [[ ! -e "${OWL_FILES[0]}" ]]; then
  # Try the backslash-named layout (UBA1.7 on macOS).
  OWL_FILES=( "$OUT/generated"\\University*.owl )
fi
[[ -e "${OWL_FILES[0]}" ]] || { echo "No University*.owl files found after generation" >&2; ls "$OUT/" >&2; exit 1; }

# Convert ontology + all generated files to N-Triples.
echo ">> converting ontology -> tbox.nt" >&2
riot --syntax=RDFXML --output=NT "$ONTO_SRC" > "$OUT/tbox.nt"

echo ">> converting ${#OWL_FILES[@]} ABox files -> abox.nt" >&2
: > "$OUT/abox.nt"
for f in "${OWL_FILES[@]}"; do
  riot --syntax=RDFXML --output=NT "$f" >> "$OUT/abox.nt" \
    || { echo "riot failed converting $f" >&2; exit 1; }
done

TBOX_N="$(wc -l < "$OUT/tbox.nt" | tr -d ' ')"
ABOX_N="$(wc -l < "$OUT/abox.nt" | tr -d ' ')"
echo ">> done: tbox.nt=$TBOX_N triples, abox.nt=$ABOX_N triples ($OUT)" >&2
# Sanity: the ontology must carry the structural axioms we reason over.
SCO="$(grep -c 'subClassOf' "$OUT/tbox.nt" || true)"
echo ">> tbox subClassOf triples: $SCO" >&2
[[ "$ABOX_N" -gt 0 ]] || { echo "abox.nt is empty — generation failed" >&2; exit 1; }
[[ "$SCO" -gt 0 ]] || { echo "tbox.nt has no subClassOf — wrong ontology variant?" >&2; exit 1; }
